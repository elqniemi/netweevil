//! Workspace setup through the API: where state lives, file uploads, dataset
//! and GTFS imports, profile files, and activating a dataset/profile/feed
//! selection without restarting the server. Every write stays under
//! `.netweevil/`; files referenced by path are read in place.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use axum::Json;
use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use futures_util::StreamExt;
use netweevil_core::SourceFormat;
use netweevil_ingest::{DatasetImportOptions, import_dataset};
use netweevil_manifest::now_rfc3339;
use netweevil_persist::{
    WorkspaceConfig, WorkspaceLocation, WorkspacePaths, list_json_files,
    read_compiled_profile_manifests, read_dataset_manifests, read_json, read_workspace_config,
    write_json, write_workspace_config,
};
use netweevil_profile::{ProfileDocument, load_profile};
use netweevil_transit::{
    TransitBundle, TransitFeedManifest, TransitImportOptions, import_gtfs, transit_import_summary,
    write_transit_bundle,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tracing::info;

use crate::error::ApiError;
use crate::jobs::{JobRecord, spawn_job};
use crate::state::{ApiState, Workspace, load_service_runtime};

// --- Overview -------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct WorkspaceInfo {
    pub(crate) root: String,
    pub(crate) state_dir: String,
    pub(crate) config_path: String,
    pub(crate) locations: Vec<WorkspaceLocation>,
    /// Whether a dataset is loaded and analyses can run.
    pub(crate) loaded: bool,
    /// The selection currently loaded in memory.
    pub(crate) active: Option<WorkspaceConfig>,
    /// The selection saved for the next plain `api serve` start.
    pub(crate) saved: Option<WorkspaceConfig>,
    pub(crate) datasets: Vec<DatasetSummary>,
    pub(crate) profiles: Vec<ProfileFileInfo>,
    pub(crate) transit_feeds: Vec<TransitFeedSummary>,
    pub(crate) uploads: Vec<UploadInfo>,
    pub(crate) jobs: Vec<JobRecord>,
    pub(crate) example_profiles_dir: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DatasetSummary {
    pub(crate) dataset_id: String,
    pub(crate) label: String,
    pub(crate) source_path: String,
    pub(crate) source_format: SourceFormat,
    pub(crate) source_size_bytes: u64,
    pub(crate) imported_at: String,
    pub(crate) node_count: Option<u64>,
    pub(crate) edge_count: Option<u64>,
    pub(crate) has_acceleration: bool,
    pub(crate) compiled_profile_ids: Vec<String>,
    pub(crate) active: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProfileFileInfo {
    pub(crate) path: String,
    pub(crate) file_name: String,
    /// `workspace` for `.netweevil/profiles`, `examples` for `examples/profiles`.
    pub(crate) source: &'static str,
    pub(crate) profile_id: Option<String>,
    pub(crate) label: Option<String>,
    pub(crate) mode: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) active: bool,
    pub(crate) is_default: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct TransitFeedSummary {
    pub(crate) feed_id: String,
    pub(crate) label: String,
    pub(crate) source_path: String,
    pub(crate) imported_at: String,
    pub(crate) service_start_date: String,
    pub(crate) service_days: u32,
    pub(crate) agency_timezone: String,
    pub(crate) stop_count: u64,
    pub(crate) route_count: u64,
    pub(crate) trip_count: u64,
    pub(crate) bundle_path: String,
    pub(crate) loaded: bool,
    /// Set when the feed was built by the GTFS editor from a scenario.
    pub(crate) scenario_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct UploadInfo {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) size_bytes: u64,
    pub(crate) kind: &'static str,
}

pub(crate) async fn workspace_info(
    State(state): State<ApiState>,
) -> Result<Json<WorkspaceInfo>, ApiError> {
    let workspace = Arc::clone(&state.workspace);
    let info = tokio::task::spawn_blocking(move || build_workspace_info(&workspace))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .map_err(|error| ApiError::internal(format!("{error:#}")))?;
    Ok(Json(info))
}

fn build_workspace_info(workspace: &Workspace) -> Result<WorkspaceInfo> {
    let paths = &workspace.paths;
    let runtime = workspace.runtime();
    let active = runtime.as_ref().map(|runtime| {
        let mut active = runtime.active.clone();
        active.transit_feeds = runtime.transit_feed_ids();
        active
    });
    let saved = read_workspace_config(paths)?;
    let compiled = read_compiled_profile_manifests(paths).unwrap_or_default();
    let datasets = read_dataset_manifests(paths)?
        .into_iter()
        .map(|manifest| DatasetSummary {
            active: active
                .as_ref()
                .is_some_and(|active| active.dataset_id == manifest.dataset_id.0),
            compiled_profile_ids: compiled
                .iter()
                .filter(|entry| entry.dataset_id.0 == manifest.dataset_id.0)
                .map(|entry| entry.profile_id.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect(),
            dataset_id: manifest.dataset_id.0,
            label: manifest.label,
            source_path: manifest.source_path,
            source_format: manifest.source_format,
            source_size_bytes: manifest.source_size_bytes,
            imported_at: manifest.imported_at,
            node_count: manifest.topology_meta.as_ref().map(|meta| meta.node_count),
            edge_count: manifest.topology_meta.as_ref().map(|meta| meta.edge_count),
            has_acceleration: manifest.acceleration_bundle.is_some(),
        })
        .collect();

    let active_profile_paths = active
        .as_ref()
        .map(|active| {
            let mut all = vec![active.default_profile.clone()];
            all.extend(active.profiles.iter().cloned());
            all.into_iter()
                .map(|path| canonical_string(Path::new(&path)))
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let default_profile = active
        .as_ref()
        .map(|active| canonical_string(Path::new(&active.default_profile)));
    let example_dir = paths.root.join("examples").join("profiles");
    let mut profiles = list_profile_files(&paths.profiles_dir, "workspace")?;
    if example_dir.is_dir() {
        profiles.extend(list_profile_files(&example_dir, "examples")?);
    }
    for profile in &mut profiles {
        let canonical = canonical_string(Path::new(&profile.path));
        profile.active = active_profile_paths.contains(&canonical);
        profile.is_default = default_profile.as_deref() == Some(canonical.as_str());
    }

    let loaded_feed_ids = runtime
        .as_ref()
        .map(|runtime| runtime.transit_feed_ids())
        .unwrap_or_default();
    let transit_feeds = list_json_files(&paths.transit_feeds_dir)?
        .into_iter()
        .filter_map(|path| read_json::<TransitFeedManifest>(&path).ok())
        .map(|manifest| TransitFeedSummary {
            loaded: loaded_feed_ids.contains(&manifest.feed_id),
            scenario_id: scenario_id_of_feed(paths, &manifest),
            feed_id: manifest.feed_id,
            label: manifest.label,
            source_path: manifest.source_path,
            imported_at: manifest.imported_at,
            service_start_date: manifest.service_start_date,
            service_days: manifest.service_days,
            agency_timezone: manifest.agency_timezone,
            stop_count: manifest.stop_count,
            route_count: manifest.route_count,
            trip_count: manifest.trip_count,
            bundle_path: manifest.bundle_path,
        })
        .collect();

    let mut uploads = Vec::new();
    for entry in fs::read_dir(&paths.uploads_dir)
        .with_context(|| format!("reading {}", paths.uploads_dir.display()))?
    {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        uploads.push(UploadInfo {
            kind: upload_kind(&name),
            path: entry.path().display().to_string(),
            size_bytes: metadata.len(),
            name,
        });
    }
    uploads.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(WorkspaceInfo {
        root: paths.root.display().to_string(),
        state_dir: paths.state_dir.display().to_string(),
        config_path: paths.workspace_config_path.display().to_string(),
        locations: paths.locations(),
        loaded: runtime.is_some(),
        active,
        saved,
        datasets,
        profiles,
        transit_feeds,
        uploads,
        jobs: workspace.jobs.list(),
        example_profiles_dir: example_dir
            .is_dir()
            .then(|| example_dir.display().to_string()),
    })
}

/// Feeds built by the GTFS editor record their scenario document as source.
pub(crate) fn scenario_id_of_feed(
    paths: &WorkspacePaths,
    manifest: &TransitFeedManifest,
) -> Option<String> {
    let source = Path::new(&manifest.source_path);
    (source.starts_with(&paths.gtfs_scenarios_dir)
        && source.extension().is_some_and(|ext| ext == "json"))
    .then(|| {
        source
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
    })
    .flatten()
}

fn canonical_string(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn upload_kind(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".osm.pbf") || lower.ends_with(".pbf") {
        "osm"
    } else if lower.ends_with(".parquet") || lower.ends_with(".geoparquet") {
        "overture"
    } else if lower.ends_with(".gpkg") {
        "gpkg"
    } else if lower.ends_with(".zip") {
        "gtfs"
    } else if lower.ends_with(".yml") || lower.ends_with(".yaml") || lower.ends_with(".toml") {
        "profile"
    } else {
        "other"
    }
}

fn list_profile_files(dir: &Path, source: &'static str) -> Result<Vec<ProfileFileInfo>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        if !matches!(extension, "yml" | "yaml" | "toml") {
            continue;
        }
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let parsed = load_profile(&path).and_then(|document| {
            document.validate()?;
            Ok(document)
        });
        files.push(match parsed {
            Ok(document) => ProfileFileInfo {
                path: path.display().to_string(),
                file_name,
                source,
                profile_id: Some(document.profile.id.clone()),
                label: Some(document.profile.label.clone()),
                mode: Some(mode_label(&document)),
                error: None,
                active: false,
                is_default: false,
            },
            Err(error) => ProfileFileInfo {
                path: path.display().to_string(),
                file_name,
                source,
                profile_id: None,
                label: None,
                mode: None,
                error: Some(format!("{error:#}")),
                active: false,
                is_default: false,
            },
        });
    }
    files.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    Ok(files)
}

fn mode_label(document: &ProfileDocument) -> String {
    serde_json::to_value(document.profile.mode)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

// --- Jobs -------------------------------------------------------------------

pub(crate) async fn list_jobs(State(state): State<ApiState>) -> Json<Vec<JobRecord>> {
    Json(state.workspace.jobs.list())
}

pub(crate) async fn get_job(
    State(state): State<ApiState>,
    AxumPath(job_id): AxumPath<String>,
) -> Result<Json<JobRecord>, ApiError> {
    state
        .workspace
        .jobs
        .get(&job_id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found(format!("unknown job '{job_id}'")))
}

#[derive(Debug, Serialize)]
pub(crate) struct JobStarted {
    pub(crate) job_id: String,
}

// --- Uploads ----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct UploadQuery {
    pub(crate) name: String,
    /// Replace an existing upload of the same name.
    #[serde(default)]
    pub(crate) overwrite: bool,
}

/// Streams the request body into `.netweevil/uploads/<name>` so multi-gigabyte
/// extracts never sit in memory. Send the file as the raw body.
pub(crate) async fn upload_file(
    State(state): State<ApiState>,
    Query(query): Query<UploadQuery>,
    body: Body,
) -> Result<Json<UploadInfo>, ApiError> {
    let name = sanitize_file_name(&query.name)?;
    let path = state.workspace.paths.uploads_dir.join(&name);
    if path.exists() && !query.overwrite {
        return Err(ApiError::conflict(format!(
            "an upload named '{name}' already exists; pass overwrite=true to replace it"
        )));
    }
    let partial = state
        .workspace
        .paths
        .uploads_dir
        .join(format!("{name}.uploading"));
    let mut file = tokio::fs::File::create(&partial)
        .await
        .map_err(|error| ApiError::internal(format!("creating {}: {error}", partial.display())))?;
    let mut stream = body.into_data_stream();
    let mut size = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|error| ApiError::bad_request(format!("upload interrupted: {error}")))?;
        size += chunk.len() as u64;
        if let Err(error) = file.write_all(&chunk).await {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(ApiError::internal(format!("writing upload: {error}")));
        }
    }
    file.flush()
        .await
        .map_err(|error| ApiError::internal(format!("flushing upload: {error}")))?;
    drop(file);
    tokio::fs::rename(&partial, &path)
        .await
        .map_err(|error| ApiError::internal(format!("finishing upload: {error}")))?;
    info!(name = %name, size_bytes = size, "upload stored");
    Ok(Json(UploadInfo {
        kind: upload_kind(&name),
        path: path.display().to_string(),
        size_bytes: size,
        name,
    }))
}

pub(crate) async fn delete_upload(
    State(state): State<ApiState>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    let name = sanitize_file_name(&name)?;
    let path = state.workspace.paths.uploads_dir.join(&name);
    if !path.is_file() {
        return Err(ApiError::not_found(format!("no upload named '{name}'")));
    }
    fs::remove_file(&path).map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(Json(json!({ "deleted": name })))
}

fn sanitize_file_name(raw: &str) -> Result<String, ApiError> {
    let base = Path::new(raw)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let cleaned = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    if cleaned.is_empty() || cleaned.starts_with('.') {
        return Err(ApiError::bad_request(format!("invalid file name '{raw}'")));
    }
    Ok(cleaned)
}

/// Resolves a source given as an upload name, a path relative to the
/// workspace root, or an absolute path.
fn resolve_source(paths: &WorkspacePaths, raw: &str) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("source path is empty");
    }
    let candidates = [
        paths.uploads_dir.join(raw),
        PathBuf::from(raw),
        paths.root.join(raw),
    ];
    candidates
        .into_iter()
        .find(|candidate| candidate.exists())
        .ok_or_else(|| {
            anyhow!("source '{raw}' was not found as an upload or a path on this machine")
        })
}

// --- Datasets ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct DatasetImportRequest {
    /// Upload names or paths; several Overture parquet files may be combined.
    pub(crate) sources: Vec<String>,
    pub(crate) name: String,
    /// `osm_pbf`, `overture_parquet` or `geo_package`; detected when absent.
    #[serde(default)]
    pub(crate) format: Option<SourceFormat>,
    #[serde(default)]
    pub(crate) mapping: Option<String>,
}

pub(crate) async fn import_dataset_handler(
    State(state): State<ApiState>,
    Json(payload): Json<DatasetImportRequest>,
) -> Result<Json<JobStarted>, ApiError> {
    let paths = state.workspace.paths.clone();
    validate_identifier(&payload.name)?;
    if paths
        .datasets_dir
        .join(format!("{}.json", payload.name))
        .exists()
    {
        return Err(ApiError::conflict(format!(
            "dataset '{}' already exists; choose another name or delete it first",
            payload.name
        )));
    }
    let sources = payload
        .sources
        .iter()
        .map(|source| resolve_source(&paths, source))
        .collect::<Result<Vec<_>>>()
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    if sources.is_empty() {
        return Err(ApiError::bad_request("at least one source is required"));
    }
    if state.workspace.jobs.is_running("dataset-import") {
        return Err(ApiError::conflict(
            "another dataset import is still running",
        ));
    }
    let mapping = payload
        .mapping
        .as_deref()
        .map(|mapping| resolve_source(&paths, mapping))
        .transpose()
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let name = payload.name.clone();
    let label = format!("Import dataset '{name}'");
    let source_label = sources
        .iter()
        .map(|source| source.display().to_string())
        .collect::<Vec<_>>()
        .join(";");
    let job_id = spawn_job(
        Arc::clone(&state.workspace),
        "dataset-import",
        &label,
        move |job| {
            let manifest = import_dataset(
                &paths,
                &sources,
                DatasetImportOptions {
                    name,
                    source: source_label,
                    format: payload.format,
                    mapping,
                },
                |event| job.progress(event.stage.label(), &event.message, event.stage_percent),
            )?;
            Ok(json!({
                "dataset_id": manifest.dataset_id.0,
                "node_count": manifest.topology_meta.as_ref().map(|meta| meta.node_count),
                "edge_count": manifest.topology_meta.as_ref().map(|meta| meta.edge_count),
                "manifest_path": paths.datasets_dir.join(format!("{}.json", manifest.dataset_id.0)).display().to_string(),
            }))
        },
    );
    Ok(Json(JobStarted { job_id }))
}

pub(crate) async fn delete_dataset(
    State(state): State<ApiState>,
    AxumPath(dataset_id): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    if let Some(runtime) = state.workspace.runtime()
        && runtime.active.dataset_id == dataset_id
    {
        return Err(ApiError::conflict(
            "the dataset is loaded; activate another one before deleting it",
        ));
    }
    let paths = state.workspace.paths.clone();
    tokio::task::spawn_blocking(move || remove_dataset(&paths, &dataset_id))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .map_err(|error| ApiError::bad_request(format!("{error:#}")))?;
    Ok(Json(json!({ "deleted": true })))
}

fn remove_dataset(paths: &WorkspacePaths, dataset_id: &str) -> Result<()> {
    let manifest_path = paths.datasets_dir.join(format!("{dataset_id}.json"));
    let manifest: netweevil_manifest::DatasetManifest = read_json(&manifest_path)
        .with_context(|| format!("dataset '{dataset_id}' was not found"))?;
    for bundle in [
        manifest.topology_bundle.as_ref(),
        manifest.edge_name_bundle.as_ref(),
        manifest.acceleration_bundle.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        remove_workspace_file(paths, Path::new(&bundle.path));
    }
    for compiled in read_compiled_profile_manifests(paths).unwrap_or_default() {
        if compiled.dataset_id.0 == dataset_id {
            remove_workspace_file(paths, Path::new(&compiled.bundle.path));
            let _ = fs::remove_file(
                paths
                    .compiled_profiles_dir
                    .join(format!("{}.json", compiled.compile_id)),
            );
        }
    }
    fs::remove_file(&manifest_path)
        .with_context(|| format!("removing {}", manifest_path.display()))?;
    if let Some(mut saved) = read_workspace_config(paths)?
        && saved.dataset_id == dataset_id
    {
        saved.dataset_id.clear();
        let _ = fs::remove_file(&paths.workspace_config_path);
        let _ = saved;
    }
    Ok(())
}

/// Deletes derived files only when they live inside `.netweevil/`.
pub(crate) fn remove_workspace_file(paths: &WorkspacePaths, path: &Path) {
    if path.starts_with(&paths.state_dir) {
        let _ = fs::remove_file(path);
    }
}

fn validate_identifier(name: &str) -> Result<(), ApiError> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(ApiError::bad_request(format!(
            "'{name}' is not a valid name; use letters, digits, '_' and '-'"
        )));
    }
    Ok(())
}

// --- Profiles ---------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProfileTemplate {
    pub(crate) template_id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) mode: &'static str,
    pub(crate) description: &'static str,
    pub(crate) yaml: &'static str,
}

pub(crate) const PROFILE_TEMPLATES: &[ProfileTemplate] = &[
    ProfileTemplate {
        template_id: "car",
        label: "Car (default)",
        mode: "car",
        description: "Fastest-route car profile with posted speed limits capping the class speeds, turn penalties and ferries allowed.",
        yaml: include_str!("../templates/car_default.yml"),
    },
    ProfileTemplate {
        template_id: "bicycle",
        label: "Bicycle (default)",
        mode: "bicycle",
        description: "Bicycle profile preferring cycle paths and quiet streets, avoiding motorways and rough surfaces.",
        yaml: include_str!("../templates/bicycle_default.yml"),
    },
    ProfileTemplate {
        template_id: "pedestrian",
        label: "Pedestrian (default)",
        mode: "foot",
        description: "Walking profile at 5 km/h ignoring one-way streets, excluding motorways; also used for transit access and transfers.",
        yaml: include_str!("../templates/pedestrian_default.yml"),
    },
];

pub(crate) async fn profile_templates() -> Json<&'static [ProfileTemplate]> {
    Json(PROFILE_TEMPLATES)
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateProfileRequest {
    pub(crate) yaml: String,
    /// File name under `.netweevil/profiles`; defaults to `<profile id>.yml`.
    #[serde(default)]
    pub(crate) file_name: Option<String>,
    #[serde(default)]
    pub(crate) overwrite: bool,
}

pub(crate) async fn create_profile(
    State(state): State<ApiState>,
    Json(payload): Json<CreateProfileRequest>,
) -> Result<Json<ProfileFileInfo>, ApiError> {
    let document: ProfileDocument = serde_yaml::from_str(&payload.yaml)
        .map_err(|error| ApiError::bad_request(format!("profile YAML does not parse: {error}")))?;
    document
        .validate()
        .map_err(|error| ApiError::bad_request(format!("profile is invalid: {error:#}")))?;
    let file_name = match payload.file_name {
        Some(name) => sanitize_file_name(&name)?,
        None => format!("{}.yml", document.profile.id),
    };
    if !file_name.ends_with(".yml") && !file_name.ends_with(".yaml") {
        return Err(ApiError::bad_request(
            "profile file name must end with .yml",
        ));
    }
    let path = state.workspace.paths.profiles_dir.join(&file_name);
    if path.exists() && !payload.overwrite {
        return Err(ApiError::conflict(format!(
            "profile file '{file_name}' already exists; pass overwrite=true to replace it"
        )));
    }
    fs::write(&path, payload.yaml.as_bytes())
        .map_err(|error| ApiError::internal(format!("writing {}: {error}", path.display())))?;
    info!(path = %path.display(), profile_id = %document.profile.id, "profile saved");
    Ok(Json(ProfileFileInfo {
        path: path.display().to_string(),
        file_name,
        source: "workspace",
        profile_id: Some(document.profile.id.clone()),
        label: Some(document.profile.label.clone()),
        mode: Some(mode_label(&document)),
        error: None,
        active: false,
        is_default: false,
    }))
}

#[derive(Debug, Serialize)]
pub(crate) struct ProfileFileContent {
    pub(crate) path: String,
    pub(crate) file_name: String,
    pub(crate) yaml: String,
}

pub(crate) async fn get_profile_file(
    State(state): State<ApiState>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<ProfileFileContent>, ApiError> {
    let path = profile_file_path(&state.workspace.paths, &name)?;
    let yaml = fs::read_to_string(&path)
        .map_err(|_| ApiError::not_found(format!("no profile file named '{name}'")))?;
    Ok(Json(ProfileFileContent {
        path: path.display().to_string(),
        file_name: name,
        yaml,
    }))
}

pub(crate) async fn delete_profile_file(
    State(state): State<ApiState>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    let path = profile_file_path(&state.workspace.paths, &name)?;
    if let Some(runtime) = state.workspace.runtime() {
        let canonical = canonical_string(&path);
        let in_use = std::iter::once(&runtime.active.default_profile)
            .chain(runtime.active.profiles.iter())
            .any(|used| canonical_string(Path::new(used)) == canonical);
        if in_use {
            return Err(ApiError::conflict(
                "the profile is loaded; activate a selection without it before deleting",
            ));
        }
    }
    if !path.is_file() {
        return Err(ApiError::not_found(format!(
            "no profile file named '{name}'"
        )));
    }
    fs::remove_file(&path).map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(Json(json!({ "deleted": name })))
}

/// Profile files are addressed by name inside `.netweevil/profiles`, or by
/// `examples/<file>` for the repository examples (read-only in practice).
fn profile_file_path(paths: &WorkspacePaths, name: &str) -> Result<PathBuf, ApiError> {
    if let Some(example) = name.strip_prefix("examples/") {
        let file = sanitize_file_name(example)?;
        return Ok(paths.root.join("examples").join("profiles").join(file));
    }
    let file = sanitize_file_name(name)?;
    Ok(paths.profiles_dir.join(file))
}

// --- Transit ----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct TransitImportRequest {
    /// Upload name or path of a GTFS zip (or an unzipped GTFS directory).
    pub(crate) source: String,
    pub(crate) name: String,
    /// First service date, `YYYY-MM-DD`.
    pub(crate) service_start: String,
    #[serde(default = "default_service_days")]
    pub(crate) service_days: u32,
}

fn default_service_days() -> u32 {
    7
}

pub(crate) async fn import_transit_handler(
    State(state): State<ApiState>,
    Json(payload): Json<TransitImportRequest>,
) -> Result<Json<JobStarted>, ApiError> {
    let paths = state.workspace.paths.clone();
    validate_identifier(&payload.name)?;
    if paths
        .transit_feeds_dir
        .join(format!("{}.json", payload.name))
        .exists()
    {
        return Err(ApiError::conflict(format!(
            "transit feed '{}' already exists; choose another name or delete it first",
            payload.name
        )));
    }
    let source = resolve_source(&paths, &payload.source)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    if state.workspace.jobs.is_running("transit-import") {
        return Err(ApiError::conflict(
            "another transit import is still running",
        ));
    }
    let label = format!("Import GTFS feed '{}'", payload.name);
    let job_id = spawn_job(
        Arc::clone(&state.workspace),
        "transit-import",
        &label,
        move |job| {
            job.progress("Import", &format!("Reading {}", source.display()), None);
            let bundle = import_gtfs(
                &source,
                TransitImportOptions {
                    name: payload.name.clone(),
                    source_label: source.display().to_string(),
                    service_start_date: payload.service_start.clone(),
                    service_days: payload.service_days,
                },
            )
            .with_context(|| format!("importing GTFS feed {}", source.display()))?;
            job.progress("Write", "Writing transit bundle", None);
            let manifest = write_transit_feed(
                &paths,
                bundle,
                &format!("GTFS transit feed {}", payload.name),
                &source.display().to_string(),
            )?;
            Ok(json!({
                "feed_id": manifest.feed_id,
                "stop_count": manifest.stop_count,
                "route_count": manifest.route_count,
                "trip_count": manifest.trip_count,
                "connection_count": manifest.connection_count,
                "bundle_path": manifest.bundle_path,
            }))
        },
    );
    Ok(Json(JobStarted { job_id }))
}

/// Writes a transit bundle and its manifest under `.netweevil/` the way the
/// CLI import does, so both paths produce interchangeable feeds.
pub(crate) fn write_transit_feed(
    paths: &WorkspacePaths,
    bundle: TransitBundle,
    label: &str,
    source_path: &str,
) -> Result<TransitFeedManifest> {
    let summary = transit_import_summary(&bundle);
    let bundle_path = paths.transit_bundles_dir.join(format!(
        "transit-{}-{}.bin",
        bundle.feed_id,
        &summary.source_sha256[..12.min(summary.source_sha256.len())]
    ));
    write_transit_bundle(&bundle_path, &bundle)?;
    let manifest = TransitFeedManifest {
        feed_id: bundle.feed_id.clone(),
        label: label.to_string(),
        source_path: source_path.to_string(),
        source_sha256: summary.source_sha256.clone(),
        imported_at: now_rfc3339()?,
        service_start_date: summary.service_start_date.clone(),
        service_days: summary.service_days,
        agency_timezone: summary.agency_timezone.clone(),
        time_origin_unix_s: summary.time_origin_unix_s,
        stop_count: summary.stop_count as u64,
        route_count: summary.route_count as u64,
        trip_count: summary.trip_count as u64,
        connection_count: summary.connection_count as u64,
        bundle_path: bundle_path.display().to_string(),
        stop_binding_source_path: None,
        stop_binding_sha256: bundle.stop_binding_sha256.clone(),
        bound_stop_count: bundle
            .stops
            .iter()
            .filter(|stop| stop.binding.is_some())
            .count() as u64,
        transfer_tables: Vec::new(),
    };
    write_json(
        paths
            .transit_feeds_dir
            .join(format!("{}.json", manifest.feed_id)),
        &manifest,
    )?;
    Ok(manifest)
}

pub(crate) async fn delete_transit_feed(
    State(state): State<ApiState>,
    AxumPath(feed_id): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    let workspace = Arc::clone(&state.workspace);
    tokio::task::spawn_blocking(move || remove_transit_feed(&workspace, &feed_id))
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .map_err(|error| ApiError::bad_request(format!("{error:#}")))?;
    Ok(Json(json!({ "deleted": true })))
}

/// Unloads a feed if it is loaded, forgets it in the saved selection, and
/// deletes its manifest and bundle. The source GTFS file is left alone.
pub(crate) fn remove_transit_feed(workspace: &Workspace, feed_id: &str) -> Result<()> {
    let paths = &workspace.paths;
    let manifest_path = paths.transit_feeds_dir.join(format!("{feed_id}.json"));
    let manifest: TransitFeedManifest = read_json(&manifest_path)
        .with_context(|| format!("transit feed '{feed_id}' was not found"))?;
    if let Some(runtime) = workspace.runtime() {
        runtime.remove_transit_feed(feed_id);
    }
    if let Some(mut saved) = read_workspace_config(paths)? {
        let before = saved.transit_feeds.len();
        saved.transit_feeds.retain(|id| id != feed_id);
        if saved.transit_feeds.len() != before {
            saved.updated_at = now_rfc3339().ok();
            write_workspace_config(paths, &saved)?;
        }
    }
    remove_workspace_file(paths, Path::new(&manifest.bundle_path));
    for table in &manifest.transfer_tables {
        remove_workspace_file(paths, Path::new(&table.path));
    }
    fs::remove_file(&manifest_path)
        .with_context(|| format!("removing {}", manifest_path.display()))?;
    info!(feed_id, "transit feed removed");
    Ok(())
}

/// Adds a freshly written feed to the running service and to the saved
/// selection so it survives a restart.
pub(crate) fn register_transit_feed(workspace: &Workspace, feed_id: &str) -> Result<bool> {
    let paths = &workspace.paths;
    let mut loaded = false;
    if let Some(runtime) = workspace.runtime() {
        let feed = crate::state::load_transit_feed(
            paths,
            &runtime.active.dataset_id,
            &runtime.profiles,
            feed_id,
        )?;
        runtime.insert_transit_feed(feed);
        loaded = true;
    }
    if let Some(mut saved) = read_workspace_config(paths)?
        && !saved.transit_feeds.iter().any(|id| id == feed_id)
    {
        saved.transit_feeds.push(feed_id.to_string());
        saved.updated_at = now_rfc3339().ok();
        write_workspace_config(paths, &saved)?;
    }
    Ok(loaded)
}

// --- Activation -------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct ActivateRequest {
    pub(crate) dataset_id: String,
    /// Profile file path (workspace, examples, or absolute).
    pub(crate) default_profile: String,
    #[serde(default)]
    pub(crate) profiles: Vec<String>,
    #[serde(default)]
    pub(crate) transit_feeds: Vec<String>,
}

/// Loads the selection in the background (compiling profiles that were not
/// compiled for the dataset yet), swaps it in, and saves it as the default
/// for the next start.
pub(crate) async fn activate(
    State(state): State<ApiState>,
    Json(payload): Json<ActivateRequest>,
) -> Result<Json<JobStarted>, ApiError> {
    let paths = state.workspace.paths.clone();
    if !paths
        .datasets_dir
        .join(format!("{}.json", payload.dataset_id))
        .exists()
    {
        return Err(ApiError::bad_request(format!(
            "dataset '{}' does not exist; import it first",
            payload.dataset_id
        )));
    }
    let resolve_profile = |raw: &str| -> Result<String, ApiError> {
        let path = resolve_source(&paths, raw)
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
        Ok(path.display().to_string())
    };
    let default_profile = resolve_profile(&payload.default_profile)?;
    let profiles = payload
        .profiles
        .iter()
        .map(|raw| resolve_profile(raw))
        .collect::<Result<Vec<_>, _>>()?;
    for feed_id in &payload.transit_feeds {
        if !paths
            .transit_feeds_dir
            .join(format!("{feed_id}.json"))
            .exists()
        {
            return Err(ApiError::bad_request(format!(
                "transit feed '{feed_id}' does not exist; import it first"
            )));
        }
    }
    if state.workspace.jobs.is_running("activate") {
        return Err(ApiError::conflict("another activation is still running"));
    }
    let selection = WorkspaceConfig {
        dataset_id: payload.dataset_id.clone(),
        default_profile,
        profiles,
        transit_feeds: payload.transit_feeds.clone(),
        updated_at: None,
    };
    let workspace = Arc::clone(&state.workspace);
    let label = format!("Load dataset '{}'", payload.dataset_id);
    let job_id = spawn_job(
        Arc::clone(&state.workspace),
        "activate",
        &label,
        move |job| {
            let runtime = load_service_runtime(&paths, &selection, |message| {
                job.progress("Load", message, None)
            })?;
            let mut saved = selection.clone();
            saved.updated_at = now_rfc3339().ok();
            write_workspace_config(&paths, &saved)?;
            let summary = json!({
                "dataset_id": runtime.dataset_manifest.dataset_id.0,
                "default_profile_id": runtime.default_profile_id,
                "profile_ids": runtime.profiles.keys().cloned().collect::<Vec<_>>(),
                "transit_feed_ids": runtime.transit_feed_ids(),
                "config_path": paths.workspace_config_path.display().to_string(),
            });
            workspace.set_runtime(Some(Arc::new(runtime)));
            info!("workspace activated");
            Ok(summary)
        },
    );
    Ok(Json(JobStarted { job_id }))
}
