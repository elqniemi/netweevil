//! GTFS scenario editor endpoints: scenario documents under
//! `.netweevil/gtfs_scenarios`, building them into routable feeds (loaded on
//! the fly), reverting, exporting as GTFS zips, and browsing the stops and
//! route patterns of a loaded feed to draw new lines against.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use netweevil_manifest::now_rfc3339;
use netweevil_persist::{WorkspacePaths, list_json_files, read_json, write_json};
use netweevil_transit::{
    GtfsScenario, ScenarioBuildOptions, ScenarioSummary, TransitBundle, TransitFeedManifest,
    TransitRoutePattern, TransitStop, build_scenario_bundle, read_transit_bundle, route_patterns,
    write_gtfs_zip,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::info;

use crate::error::ApiError;
use crate::jobs::spawn_job;
use crate::state::{ApiState, Workspace};
use crate::workspace::{
    JobStarted, register_transit_feed, remove_transit_feed, write_transit_feed,
};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BuiltFeedInfo {
    pub(crate) feed_id: String,
    pub(crate) bundle_path: String,
    pub(crate) built_at: String,
    pub(crate) loaded: bool,
    pub(crate) stop_count: u64,
    pub(crate) route_count: u64,
    pub(crate) trip_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ScenarioInfo {
    pub(crate) scenario_id: String,
    pub(crate) label: String,
    pub(crate) base_feed_id: Option<String>,
    pub(crate) output_feed_id: String,
    pub(crate) path: String,
    pub(crate) export_path: String,
    pub(crate) stop_count: usize,
    pub(crate) line_count: usize,
    pub(crate) updated_at: String,
    /// Whether the base feed is loaded, which the editor needs for its stops.
    pub(crate) base_loaded: bool,
    pub(crate) built: Option<BuiltFeedInfo>,
    pub(crate) summary: Option<ScenarioSummary>,
    pub(crate) validation_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ScenarioResponse {
    pub(crate) info: ScenarioInfo,
    pub(crate) scenario: GtfsScenario,
}

fn scenario_path(paths: &WorkspacePaths, scenario_id: &str) -> PathBuf {
    paths.gtfs_scenarios_dir.join(format!("{scenario_id}.json"))
}

fn export_path(paths: &WorkspacePaths, scenario_id: &str) -> PathBuf {
    paths
        .gtfs_scenarios_dir
        .join(format!("{scenario_id}.gtfs.zip"))
}

fn validate_scenario_id(scenario_id: &str) -> Result<(), ApiError> {
    if scenario_id.is_empty()
        || !scenario_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(ApiError::bad_request(format!(
            "'{scenario_id}' is not a valid scenario id; use letters, digits, '_' and '-'"
        )));
    }
    Ok(())
}

fn read_scenario(paths: &WorkspacePaths, scenario_id: &str) -> Result<GtfsScenario, ApiError> {
    let path = scenario_path(paths, scenario_id);
    if !path.is_file() {
        return Err(ApiError::not_found(format!(
            "unknown scenario '{scenario_id}'"
        )));
    }
    read_json(&path).map_err(|error| ApiError::internal(format!("{error:#}")))
}

/// Stops of the base feed keyed by id, from the loaded router or the bundle on disk.
fn base_stops(
    workspace: &Workspace,
    base_feed_id: Option<&str>,
) -> Result<Option<HashMap<String, TransitStop>>> {
    let Some(feed_id) = base_feed_id else {
        return Ok(None);
    };
    if let Some(runtime) = workspace.runtime()
        && let Some(feed) = runtime.transit_feed(feed_id)
    {
        return Ok(Some(
            feed.router
                .bundle()
                .stops
                .iter()
                .map(|stop| (stop.stop_id.clone(), stop.clone()))
                .collect(),
        ));
    }
    let bundle = base_bundle(&workspace.paths, feed_id)?;
    Ok(Some(
        bundle
            .stops
            .into_iter()
            .map(|stop| (stop.stop_id.clone(), stop))
            .collect(),
    ))
}

fn base_bundle(paths: &WorkspacePaths, feed_id: &str) -> Result<TransitBundle> {
    let manifest_path = paths.transit_feeds_dir.join(format!("{feed_id}.json"));
    let manifest: TransitFeedManifest = read_json(&manifest_path)
        .with_context(|| format!("base feed '{feed_id}' is not imported"))?;
    read_transit_bundle(&manifest.bundle_path)
        .with_context(|| format!("reading base transit bundle {}", manifest.bundle_path))
}

fn scenario_info(workspace: &Workspace, scenario: &GtfsScenario) -> ScenarioInfo {
    let paths = &workspace.paths;
    let runtime = workspace.runtime();
    let base_loaded = scenario.base_feed_id.as_deref().is_none_or(|feed_id| {
        runtime
            .as_ref()
            .is_some_and(|runtime| runtime.transit_feed(feed_id).is_some())
    });
    let output_feed_id = scenario.output_feed_id().to_string();
    let manifest_path = paths
        .transit_feeds_dir
        .join(format!("{output_feed_id}.json"));
    let scenario_file = scenario_path(paths, &scenario.scenario_id);
    let built = read_json::<TransitFeedManifest>(&manifest_path)
        .ok()
        .filter(|manifest| manifest.source_path == scenario_file.display().to_string())
        .map(|manifest| BuiltFeedInfo {
            loaded: runtime
                .as_ref()
                .is_some_and(|runtime| runtime.transit_feed(&manifest.feed_id).is_some()),
            feed_id: manifest.feed_id,
            bundle_path: manifest.bundle_path,
            built_at: manifest.imported_at,
            stop_count: manifest.stop_count,
            route_count: manifest.route_count,
            trip_count: manifest.trip_count,
        });
    let (summary, validation_error) = match base_stops(workspace, scenario.base_feed_id.as_deref())
    {
        Ok(stops) => match scenario.summary(stops.as_ref()) {
            Ok(summary) => (Some(summary), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        },
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    ScenarioInfo {
        scenario_id: scenario.scenario_id.clone(),
        label: scenario.label.clone(),
        base_feed_id: scenario.base_feed_id.clone(),
        output_feed_id,
        path: scenario_file.display().to_string(),
        export_path: export_path(paths, &scenario.scenario_id)
            .display()
            .to_string(),
        stop_count: scenario.stops.len(),
        line_count: scenario.lines.len(),
        updated_at: scenario.updated_at.clone(),
        base_loaded,
        built,
        summary,
        validation_error,
    }
}

pub(crate) async fn list_scenarios(
    State(state): State<ApiState>,
) -> Result<Json<Vec<ScenarioInfo>>, ApiError> {
    let workspace = Arc::clone(&state.workspace);
    let infos = tokio::task::spawn_blocking(move || -> Result<Vec<ScenarioInfo>> {
        let mut infos = Vec::new();
        for path in list_json_files(&workspace.paths.gtfs_scenarios_dir)? {
            if let Ok(scenario) = read_json::<GtfsScenario>(&path) {
                infos.push(scenario_info(&workspace, &scenario));
            }
        }
        Ok(infos)
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(|error| ApiError::internal(format!("{error:#}")))?;
    Ok(Json(infos))
}

pub(crate) async fn create_scenario(
    State(state): State<ApiState>,
    Json(mut scenario): Json<GtfsScenario>,
) -> Result<Json<ScenarioResponse>, ApiError> {
    validate_scenario_id(&scenario.scenario_id)?;
    let paths = &state.workspace.paths;
    let path = scenario_path(paths, &scenario.scenario_id);
    if path.exists() {
        return Err(ApiError::conflict(format!(
            "scenario '{}' already exists",
            scenario.scenario_id
        )));
    }
    if scenario.label.trim().is_empty() {
        scenario.label = scenario.scenario_id.clone();
    }
    if let Some(base_feed_id) = scenario.base_feed_id.as_deref() {
        let manifest_path = paths.transit_feeds_dir.join(format!("{base_feed_id}.json"));
        let manifest: TransitFeedManifest = read_json(&manifest_path).map_err(|_| {
            ApiError::bad_request(format!("base feed '{base_feed_id}' is not imported"))
        })?;
        // Overlays inherit the base window; keep it on the document for display.
        scenario.agency.timezone = manifest.agency_timezone;
        scenario.service_start_date = Some(manifest.service_start_date);
        scenario.service_days = Some(manifest.service_days);
    }
    let now = now_rfc3339().unwrap_or_default();
    scenario.created_at = now.clone();
    scenario.updated_at = now;
    save_scenario(&state.workspace, scenario).await
}

async fn save_scenario(
    workspace: &Arc<Workspace>,
    scenario: GtfsScenario,
) -> Result<Json<ScenarioResponse>, ApiError> {
    let workspace = Arc::clone(workspace);
    let response = tokio::task::spawn_blocking(move || -> Result<ScenarioResponse, ApiError> {
        // Structural validation against the base stops; schedules may still be
        // incomplete while editing, so only hard errors are rejected here.
        let stops = base_stops(&workspace, scenario.base_feed_id.as_deref())
            .map_err(|error| ApiError::bad_request(format!("{error:#}")))?;
        if let Err(error) = structural_check(&scenario, stops.as_ref()) {
            return Err(ApiError::bad_request(format!("{error:#}")));
        }
        let path = scenario_path(&workspace.paths, &scenario.scenario_id);
        write_json(&path, &scenario).map_err(|error| ApiError::internal(format!("{error:#}")))?;
        let info = scenario_info(&workspace, &scenario);
        Ok(ScenarioResponse { info, scenario })
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))??;
    Ok(Json(response))
}

/// Errors that make a document unusable rather than merely unfinished.
fn structural_check(
    scenario: &GtfsScenario,
    base: Option<&HashMap<String, TransitStop>>,
) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for stop in &scenario.stops {
        if stop.stop_id.trim().is_empty() {
            return Err(anyhow!("a scenario stop has an empty id"));
        }
        if !seen.insert(stop.stop_id.as_str()) {
            return Err(anyhow!("duplicate scenario stop id '{}'", stop.stop_id));
        }
        if let Some(base) = base
            && base.contains_key(&stop.stop_id)
        {
            return Err(anyhow!(
                "scenario stop id '{}' collides with a stop of the base feed",
                stop.stop_id
            ));
        }
    }
    let mut lines = std::collections::HashSet::new();
    for line in &scenario.lines {
        if line.line_id.trim().is_empty() {
            return Err(anyhow!("a line has an empty id"));
        }
        if !lines.insert(line.line_id.as_str()) {
            return Err(anyhow!("duplicate line id '{}'", line.line_id));
        }
        for stop in &line.stops {
            let known = seen.contains(stop.stop_id.as_str())
                || base.is_some_and(|base| base.contains_key(&stop.stop_id));
            if !known {
                return Err(anyhow!(
                    "line '{}' references unknown stop '{}'",
                    line.line_id,
                    stop.stop_id
                ));
            }
        }
    }
    Ok(())
}

pub(crate) async fn get_scenario(
    State(state): State<ApiState>,
    AxumPath(scenario_id): AxumPath<String>,
) -> Result<Json<ScenarioResponse>, ApiError> {
    let workspace = Arc::clone(&state.workspace);
    let response = tokio::task::spawn_blocking(move || -> Result<ScenarioResponse, ApiError> {
        let scenario = read_scenario(&workspace.paths, &scenario_id)?;
        let info = scenario_info(&workspace, &scenario);
        Ok(ScenarioResponse { info, scenario })
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))??;
    Ok(Json(response))
}

pub(crate) async fn update_scenario(
    State(state): State<ApiState>,
    AxumPath(scenario_id): AxumPath<String>,
    Json(mut scenario): Json<GtfsScenario>,
) -> Result<Json<ScenarioResponse>, ApiError> {
    let existing = read_scenario(&state.workspace.paths, &scenario_id)?;
    if scenario.scenario_id != scenario_id {
        return Err(ApiError::bad_request(
            "scenario_id cannot be changed; create a new scenario instead",
        ));
    }
    if scenario.base_feed_id != existing.base_feed_id {
        return Err(ApiError::bad_request(
            "base_feed_id cannot be changed; create a new scenario instead",
        ));
    }
    scenario.created_at = existing.created_at;
    scenario.updated_at = now_rfc3339().unwrap_or_default();
    if existing.base_feed_id.is_some() {
        scenario.agency.timezone = existing.agency.timezone;
        scenario.service_start_date = existing.service_start_date;
        scenario.service_days = existing.service_days;
    }
    save_scenario(&state.workspace, scenario).await
}

pub(crate) async fn delete_scenario(
    State(state): State<ApiState>,
    AxumPath(scenario_id): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    let workspace = Arc::clone(&state.workspace);
    let id = scenario_id.clone();
    tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
        let scenario = read_scenario(&workspace.paths, &id)?;
        revert_built_feed(&workspace, &scenario)
            .map_err(|error| ApiError::bad_request(format!("{error:#}")))?;
        let _ = fs::remove_file(export_path(&workspace.paths, &id));
        fs::remove_file(scenario_path(&workspace.paths, &id))
            .map_err(|error| ApiError::internal(error.to_string()))?;
        Ok(())
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))??;
    Ok(Json(json!({ "deleted": scenario_id })))
}

/// Removes the feed a scenario built, if any. The scenario document and the
/// base feed are untouched.
fn revert_built_feed(workspace: &Workspace, scenario: &GtfsScenario) -> Result<bool> {
    let info = scenario_info(workspace, scenario);
    match info.built {
        Some(built) => {
            remove_transit_feed(workspace, &built.feed_id)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct BuildRequest {
    /// Load the built feed into the running service (default true).
    #[serde(default = "default_load")]
    pub(crate) load: bool,
}

fn default_load() -> bool {
    true
}

pub(crate) async fn build_scenario(
    State(state): State<ApiState>,
    AxumPath(scenario_id): AxumPath<String>,
    payload: Option<Json<BuildRequest>>,
) -> Result<Json<JobStarted>, ApiError> {
    let load = payload.map(|Json(request)| request.load).unwrap_or(true);
    let scenario = read_scenario(&state.workspace.paths, &scenario_id)?;
    if state.workspace.jobs.is_running("scenario-build") {
        return Err(ApiError::conflict(
            "another scenario build is still running",
        ));
    }
    let workspace = Arc::clone(&state.workspace);
    let label = format!("Build GTFS scenario '{}'", scenario.label);
    let job_id = spawn_job(
        Arc::clone(&state.workspace),
        "scenario-build",
        &label,
        move |job| {
            let paths = workspace.paths.clone();
            job.progress("Validate", "Checking the scenario", None);
            let base = match scenario.base_feed_id.as_deref() {
                Some(feed_id) => {
                    job.progress("Base", &format!("Reading base feed '{feed_id}'"), None);
                    Some(
                        match workspace
                            .runtime()
                            .and_then(|runtime| runtime.transit_feed(feed_id))
                        {
                            Some(feed) => feed.router.bundle().clone(),
                            None => base_bundle(&paths, feed_id)?,
                        },
                    )
                }
                None => None,
            };
            let feed_id = scenario.output_feed_id().to_string();
            if let Some(base) = base.as_ref()
                && base.feed_id == feed_id
            {
                anyhow::bail!("the output feed id must differ from the base feed id");
            }
            job.progress(
                "Build",
                "Expanding timetable and merging with the base feed",
                None,
            );
            let bundle = build_scenario_bundle(
                &scenario,
                base.as_ref(),
                &ScenarioBuildOptions {
                    feed_id: feed_id.clone(),
                    source_label: scenario_path(&paths, &scenario.scenario_id)
                        .display()
                        .to_string(),
                    service_start_date: scenario.service_start_date.clone().unwrap_or_default(),
                    service_days: scenario.service_days.unwrap_or(7),
                },
            )?;
            // Replace a previous build of the same scenario.
            let previous_manifest = paths.transit_feeds_dir.join(format!("{feed_id}.json"));
            if previous_manifest.exists() {
                let previous: TransitFeedManifest = read_json(&previous_manifest)?;
                if previous.source_path
                    != scenario_path(&paths, &scenario.scenario_id)
                        .display()
                        .to_string()
                {
                    anyhow::bail!(
                        "feed '{feed_id}' exists and was not built from this scenario; choose another output feed id"
                    );
                }
                remove_transit_feed(&workspace, &feed_id)?;
            }
            job.progress("Write", "Writing the scenario feed", None);
            let label = match scenario.base_feed_id.as_deref() {
                Some(base) => format!("GTFS scenario '{}' on feed {base}", scenario.label),
                None => format!("GTFS scenario '{}'", scenario.label),
            };
            let manifest = write_transit_feed(
                &paths,
                bundle,
                &label,
                &scenario_path(&paths, &scenario.scenario_id)
                    .display()
                    .to_string(),
            )?;
            let mut loaded = false;
            if load {
                job.progress(
                    "Load",
                    "Loading the scenario feed into the running service",
                    None,
                );
                loaded = register_transit_feed(&workspace, &feed_id)?;
            }
            info!(scenario_id = %scenario.scenario_id, feed_id = %feed_id, loaded, "scenario feed built");
            Ok(json!({
                "feed_id": manifest.feed_id,
                "bundle_path": manifest.bundle_path,
                "stop_count": manifest.stop_count,
                "route_count": manifest.route_count,
                "trip_count": manifest.trip_count,
                "connection_count": manifest.connection_count,
                "loaded": loaded,
            }))
        },
    );
    Ok(Json(JobStarted { job_id }))
}

pub(crate) async fn revert_scenario(
    State(state): State<ApiState>,
    AxumPath(scenario_id): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    let workspace = Arc::clone(&state.workspace);
    let reverted = tokio::task::spawn_blocking(move || -> Result<bool, ApiError> {
        let scenario = read_scenario(&workspace.paths, &scenario_id)?;
        revert_built_feed(&workspace, &scenario)
            .map_err(|error| ApiError::bad_request(format!("{error:#}")))
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))??;
    Ok(Json(json!({ "reverted": reverted })))
}

/// Writes the scenario as a GTFS zip next to the document and returns it.
pub(crate) async fn export_scenario(
    State(state): State<ApiState>,
    AxumPath(scenario_id): AxumPath<String>,
) -> Result<Response, ApiError> {
    let workspace = Arc::clone(&state.workspace);
    let (path, bytes) =
        tokio::task::spawn_blocking(move || -> Result<(PathBuf, Vec<u8>), ApiError> {
            let scenario = read_scenario(&workspace.paths, &scenario_id)?;
            let stops = base_stops(&workspace, scenario.base_feed_id.as_deref())
                .map_err(|error| ApiError::bad_request(format!("{error:#}")))?;
            let files = scenario
                .gtfs_files(stops.as_ref())
                .map_err(|error| ApiError::bad_request(format!("{error:#}")))?;
            let path = export_path(&workspace.paths, &scenario_id);
            write_gtfs_zip(&path, &files)
                .map_err(|error| ApiError::internal(format!("{error:#}")))?;
            let bytes = fs::read(&path).map_err(|error| ApiError::internal(error.to_string()))?;
            Ok((path, bytes))
        })
        .await
        .map_err(|error| ApiError::internal(error.to_string()))??;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "scenario.gtfs.zip".to_string());
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file_name}\""),
            ),
            (
                "x-netweevil-export-path".parse().unwrap(),
                path.display().to_string(),
            ),
        ],
        bytes,
    )
        .into_response())
}

// --- Browsing a loaded feed -------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct StopsQuery {
    /// `west,south,east,north`
    #[serde(default)]
    pub(crate) bbox: Option<String>,
    #[serde(default)]
    pub(crate) q: Option<String>,
    #[serde(default = "default_stop_limit")]
    pub(crate) limit: usize,
}

fn default_stop_limit() -> usize {
    4000
}

#[derive(Debug, Serialize)]
pub(crate) struct FeedStop {
    pub(crate) stop_id: String,
    pub(crate) name: String,
    pub(crate) lon: f64,
    pub(crate) lat: f64,
}

#[derive(Debug, Serialize)]
pub(crate) struct FeedStopsResponse {
    pub(crate) feed_id: String,
    pub(crate) total: usize,
    pub(crate) truncated: bool,
    pub(crate) stops: Vec<FeedStop>,
}

fn parse_bbox(raw: &str) -> Result<[f64; 4], ApiError> {
    let parts = raw
        .split(',')
        .map(|part| part.trim().parse::<f64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ApiError::bad_request("bbox must be west,south,east,north"))?;
    if parts.len() != 4 {
        return Err(ApiError::bad_request("bbox must be west,south,east,north"));
    }
    Ok([parts[0], parts[1], parts[2], parts[3]])
}

pub(crate) async fn feed_stops(
    State(state): State<ApiState>,
    AxumPath(feed_id): AxumPath<String>,
    Query(query): Query<StopsQuery>,
) -> Result<Json<FeedStopsResponse>, ApiError> {
    let runtime = state.runtime()?;
    let feed = runtime
        .transit_feed(&feed_id)
        .ok_or_else(|| ApiError::not_found(format!("transit feed '{feed_id}' is not loaded")))?;
    let bbox = query.bbox.as_deref().map(parse_bbox).transpose()?;
    let needle = query
        .q
        .as_deref()
        .map(|q| q.trim().to_lowercase())
        .filter(|q| !q.is_empty());
    let bundle = feed.router.bundle();
    let mut total = 0;
    let mut stops = Vec::new();
    for stop in &bundle.stops {
        if let Some([west, south, east, north]) = bbox
            && (stop.lon < west || stop.lon > east || stop.lat < south || stop.lat > north)
        {
            continue;
        }
        if let Some(needle) = needle.as_deref()
            && !stop.name.to_lowercase().contains(needle)
            && !stop.stop_id.to_lowercase().contains(needle)
        {
            continue;
        }
        total += 1;
        if stops.len() < query.limit.max(1) {
            stops.push(FeedStop {
                stop_id: stop.stop_id.clone(),
                name: stop.name.clone(),
                lon: stop.lon,
                lat: stop.lat,
            });
        }
    }
    Ok(Json(FeedStopsResponse {
        feed_id,
        truncated: total > stops.len(),
        total,
        stops,
    }))
}

#[derive(Debug, Deserialize)]
pub(crate) struct RoutesQuery {
    #[serde(default)]
    pub(crate) q: Option<String>,
    #[serde(default = "default_route_limit")]
    pub(crate) limit: usize,
}

fn default_route_limit() -> usize {
    500
}

#[derive(Debug, Serialize)]
pub(crate) struct FeedRoute {
    pub(crate) route_id: String,
    pub(crate) short_name: String,
    pub(crate) long_name: String,
    pub(crate) mode: netweevil_transit::TransitMode,
    pub(crate) trip_count: usize,
}

pub(crate) async fn feed_routes(
    State(state): State<ApiState>,
    AxumPath(feed_id): AxumPath<String>,
    Query(query): Query<RoutesQuery>,
) -> Result<Json<Vec<FeedRoute>>, ApiError> {
    let runtime = state.runtime()?;
    let feed = runtime
        .transit_feed(&feed_id)
        .ok_or_else(|| ApiError::not_found(format!("transit feed '{feed_id}' is not loaded")))?;
    let bundle = feed.router.bundle();
    let mut trip_counts = vec![0_usize; bundle.routes.len()];
    for trip in &bundle.trips {
        if let Some(count) = trip_counts.get_mut(trip.route_index as usize) {
            *count += 1;
        }
    }
    let needle = query
        .q
        .as_deref()
        .map(|q| q.trim().to_lowercase())
        .filter(|q| !q.is_empty());
    let mut routes = bundle
        .routes
        .iter()
        .enumerate()
        .filter(|(_, route)| {
            needle.as_deref().is_none_or(|needle| {
                route.short_name.to_lowercase().contains(needle)
                    || route.long_name.to_lowercase().contains(needle)
                    || route.route_id.to_lowercase().contains(needle)
            })
        })
        .map(|(index, route)| FeedRoute {
            route_id: route.route_id.clone(),
            short_name: route.short_name.clone(),
            long_name: route.long_name.clone(),
            mode: route.mode,
            trip_count: trip_counts[index],
        })
        .collect::<Vec<_>>();
    routes.sort_by(|a, b| {
        b.trip_count
            .cmp(&a.trip_count)
            .then_with(|| a.short_name.cmp(&b.short_name))
    });
    routes.truncate(query.limit.max(1));
    Ok(Json(routes))
}

#[derive(Debug, Deserialize)]
pub(crate) struct PatternsQuery {
    pub(crate) route_id: String,
    #[serde(default = "default_pattern_limit")]
    pub(crate) limit: usize,
}

fn default_pattern_limit() -> usize {
    12
}

pub(crate) async fn feed_route_patterns(
    State(state): State<ApiState>,
    AxumPath(feed_id): AxumPath<String>,
    Query(query): Query<PatternsQuery>,
) -> Result<Json<Vec<TransitRoutePattern>>, ApiError> {
    let runtime = state.runtime()?;
    let feed = runtime
        .transit_feed(&feed_id)
        .ok_or_else(|| ApiError::not_found(format!("transit feed '{feed_id}' is not loaded")))?;
    let router = Arc::clone(&feed.router);
    let route_id = query.route_id.clone();
    let limit = query.limit.max(1);
    let patterns =
        tokio::task::spawn_blocking(move || route_patterns(router.bundle(), &route_id, limit))
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?;
    if patterns.is_empty()
        && !feed
            .router
            .bundle()
            .routes
            .iter()
            .any(|route| route.route_id == query.route_id)
    {
        return Err(ApiError::not_found(format!(
            "route '{}' is not in feed '{feed_id}'",
            query.route_id
        )));
    }
    Ok(Json(patterns))
}
