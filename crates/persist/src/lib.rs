//! Local state layout and IO for the `.netweevil/` directory: dataset,
//! profile, and run manifests plus binary bundle reading and writing.
//!
//! Large bundles use one sectioned fast-load format whose big primitive
//! arrays load with a single memcpy per array from the mapped file.

mod codec;
mod sectioned;

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use memmap2::Mmap;
use netweevil_core::{
    CompiledProfileBundle, DatasetAccelerationBundle, EdgeNameBundle, TopologyBundle,
};
use netweevil_manifest::{CompiledProfileManifest, DatasetManifest, RunManifest};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct WorkspacePaths {
    pub root: PathBuf,
    pub state_dir: PathBuf,
    pub bundles_dir: PathBuf,
    pub topology_bundles_dir: PathBuf,
    pub edge_name_bundles_dir: PathBuf,
    pub acceleration_bundles_dir: PathBuf,
    pub metric_bundles_dir: PathBuf,
    pub transit_bundles_dir: PathBuf,
    pub datasets_dir: PathBuf,
    pub transit_feeds_dir: PathBuf,
    pub compiled_profiles_dir: PathBuf,
    pub runs_dir: PathBuf,
    pub reports_dir: PathBuf,
    /// Files uploaded through the API setup flow (OSM/Overture extracts, GTFS zips).
    pub uploads_dir: PathBuf,
    /// Routing profiles created through the API setup flow.
    pub profiles_dir: PathBuf,
    /// GTFS editor scenario documents and their exported GTFS archives.
    pub gtfs_scenarios_dir: PathBuf,
    /// Active dataset/profile/feed selection used when `api serve` runs without arguments.
    pub workspace_config_path: PathBuf,
}

impl WorkspacePaths {
    pub fn discover(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let state_dir = root.join(".netweevil");
        let paths = Self {
            root,
            bundles_dir: state_dir.join("bundles"),
            topology_bundles_dir: state_dir.join("bundles").join("topology"),
            edge_name_bundles_dir: state_dir.join("bundles").join("names"),
            acceleration_bundles_dir: state_dir.join("bundles").join("acceleration"),
            metric_bundles_dir: state_dir.join("bundles").join("metrics"),
            transit_bundles_dir: state_dir.join("bundles").join("transit"),
            datasets_dir: state_dir.join("datasets"),
            transit_feeds_dir: state_dir.join("transit_feeds"),
            compiled_profiles_dir: state_dir.join("compiled_profiles"),
            runs_dir: state_dir.join("runs"),
            reports_dir: state_dir.join("reports"),
            uploads_dir: state_dir.join("uploads"),
            profiles_dir: state_dir.join("profiles"),
            gtfs_scenarios_dir: state_dir.join("gtfs_scenarios"),
            workspace_config_path: state_dir.join("workspace.json"),
            state_dir,
        };
        paths.ensure()?;
        Ok(paths)
    }

    pub fn ensure(&self) -> Result<()> {
        for dir in [
            &self.state_dir,
            &self.bundles_dir,
            &self.topology_bundles_dir,
            &self.edge_name_bundles_dir,
            &self.acceleration_bundles_dir,
            &self.metric_bundles_dir,
            &self.transit_bundles_dir,
            &self.datasets_dir,
            &self.transit_feeds_dir,
            &self.compiled_profiles_dir,
            &self.runs_dir,
            &self.reports_dir,
            &self.uploads_dir,
            &self.profiles_dir,
            &self.gtfs_scenarios_dir,
        ] {
            fs::create_dir_all(dir)
                .with_context(|| format!("creating workspace directory {}", dir.display()))?;
        }
        Ok(())
    }
}

/// One directory of the `.netweevil/` layout with a plain-language purpose,
/// shown to users so they know where their data ends up.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceLocation {
    pub key: &'static str,
    pub path: String,
    pub purpose: &'static str,
}

impl WorkspacePaths {
    /// Every location in the state directory, in the order a newcomer meets them.
    pub fn locations(&self) -> Vec<WorkspaceLocation> {
        let entry = |key, path: &Path, purpose| WorkspaceLocation {
            key,
            path: path.display().to_string(),
            purpose,
        };
        vec![
            entry(
                "state",
                &self.state_dir,
                "Everything NetWeevil writes lives under this directory. Back it up or delete it as one unit.",
            ),
            entry(
                "workspace_config",
                &self.workspace_config_path,
                "The dataset, profiles and transit feeds the API loads when started without arguments.",
            ),
            entry(
                "uploads",
                &self.uploads_dir,
                "Copies of files uploaded through the console (OSM/Overture extracts, GTFS zips). Files chosen by path are read in place and never modified.",
            ),
            entry(
                "datasets",
                &self.datasets_dir,
                "One manifest per imported street network, pointing at its bundles.",
            ),
            entry(
                "profiles",
                &self.profiles_dir,
                "Routing profiles created in the console, as editable YAML files.",
            ),
            entry(
                "compiled_profiles",
                &self.compiled_profiles_dir,
                "Manifests of profiles compiled against a dataset (rebuilt automatically when a profile changes).",
            ),
            entry(
                "transit_feeds",
                &self.transit_feeds_dir,
                "One manifest per imported or edited GTFS feed.",
            ),
            entry(
                "gtfs_scenarios",
                &self.gtfs_scenarios_dir,
                "GTFS editor scenarios (new lines, express variants) and their exported GTFS archives. The original feed is never changed.",
            ),
            entry(
                "bundles",
                &self.bundles_dir,
                "Binary bundles derived from imports: topology, names, acceleration, compiled metrics and transit timetables. Safe to delete and re-import.",
            ),
            entry(
                "runs",
                &self.runs_dir,
                "Analysis outputs written by the CLI (routes, matrices, service areas, simulations).",
            ),
            entry("reports", &self.reports_dir, "Rendered reports."),
        ]
    }
}

/// The selection `api serve` loads when started without explicit arguments.
/// Written by the console setup flow and by `api serve` when it is given
/// explicit arguments, so the next plain start reproduces the last session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub dataset_id: String,
    pub default_profile: String,
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(default)]
    pub transit_feeds: Vec<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

pub fn read_workspace_config(paths: &WorkspacePaths) -> Result<Option<WorkspaceConfig>> {
    if !paths.workspace_config_path.exists() {
        return Ok(None);
    }
    read_json(&paths.workspace_config_path).map(Some)
}

pub fn write_workspace_config(paths: &WorkspacePaths, config: &WorkspaceConfig) -> Result<()> {
    write_json(&paths.workspace_config_path, config)
}

pub fn write_json<T: Serialize>(path: impl AsRef<Path>, value: &T) -> Result<()> {
    write_json_inner(path, value, true)
}

pub fn write_compact_json<T: Serialize>(path: impl AsRef<Path>, value: &T) -> Result<()> {
    write_json_inner(path, value, false)
}

fn write_json_inner<T: Serialize>(path: impl AsRef<Path>, value: &T, pretty: bool) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let writer = BufWriter::new(file);
    if pretty {
        serde_json::to_writer_pretty(writer, value).context("serializing JSON")?;
    } else {
        serde_json::to_writer(writer, value).context("serializing JSON")?;
    }
    Ok(())
}

pub fn read_json<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

pub fn write_topology_bundle(path: impl AsRef<Path>, bundle: &TopologyBundle) -> Result<()> {
    sectioned::write_topology_sectioned(path.as_ref(), bundle)
}

pub fn write_edge_name_bundle(path: impl AsRef<Path>, bundle: &EdgeNameBundle) -> Result<()> {
    write_binary(path, bundle)
}

pub fn write_acceleration_bundle(
    path: impl AsRef<Path>,
    bundle: &DatasetAccelerationBundle,
) -> Result<()> {
    sectioned::write_acceleration_sectioned(path.as_ref(), bundle)
}

pub fn read_topology_bundle(path: impl AsRef<Path>) -> Result<TopologyBundle> {
    let path = path.as_ref();
    let mmap = sectioned::map_file(path)?;
    sectioned::read_topology_sectioned(&mmap, path)
        .with_context(|| format!("parsing topology bundle {}", path.display()))
}

pub fn read_edge_name_bundle(path: impl AsRef<Path>) -> Result<EdgeNameBundle> {
    read_binary_mmap(path)
}

pub fn read_acceleration_bundle(path: impl AsRef<Path>) -> Result<DatasetAccelerationBundle> {
    let path = path.as_ref();
    let mmap = sectioned::map_file(path)?;
    sectioned::read_acceleration_sectioned(&mmap, path)
        .with_context(|| format!("parsing acceleration bundle {}", path.display()))
}

pub fn write_compiled_profile_bundle(
    path: impl AsRef<Path>,
    bundle: &CompiledProfileBundle,
) -> Result<()> {
    sectioned::write_compiled_profile_sectioned(path.as_ref(), bundle)
}

pub fn read_compiled_profile_bundle(path: impl AsRef<Path>) -> Result<CompiledProfileBundle> {
    let path = path.as_ref();
    let mmap = sectioned::map_file(path)?;
    sectioned::read_compiled_profile_sectioned(&mmap, path)
        .with_context(|| format!("parsing compiled profile bundle {}", path.display()))
}

fn write_binary<T: Serialize>(path: impl AsRef<Path>, value: &T) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    bincode::serialize_into(&mut writer, value)
        .with_context(|| format!("serializing binary {}", path.display()))?;
    writer
        .flush()
        .with_context(|| format!("flushing binary {}", path.display()))
}

fn read_binary_mmap<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T> {
    let path = path.as_ref();
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mmap = unsafe { Mmap::map(&file) }
        .with_context(|| format!("memory-mapping {}", path.display()))?;
    bincode::deserialize(&mmap).with_context(|| format!("parsing binary bundle {}", path.display()))
}

pub fn write_dataset_manifest(
    paths: &WorkspacePaths,
    manifest: &DatasetManifest,
) -> Result<PathBuf> {
    let path = paths
        .datasets_dir
        .join(format!("{}.json", manifest.dataset_id.0));
    write_json(&path, manifest)?;
    Ok(path)
}

pub fn write_compiled_profile_manifest(
    paths: &WorkspacePaths,
    manifest: &CompiledProfileManifest,
) -> Result<PathBuf> {
    let path = paths
        .compiled_profiles_dir
        .join(format!("{}.json", manifest.compile_id));
    write_json(&path, manifest)?;
    Ok(path)
}

pub fn write_run_manifest(paths: &WorkspacePaths, manifest: &RunManifest) -> Result<PathBuf> {
    let path = paths.runs_dir.join(format!("{}.json", manifest.run_id));
    write_json(&path, manifest)?;
    Ok(path)
}

pub fn list_json_files(dir: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir.as_ref())
        .with_context(|| format!("reading directory {}", dir.as_ref().display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension() == Some(OsStr::new("json")) {
            entries.push(path);
        }
    }
    entries.sort();
    Ok(entries)
}

pub fn read_dataset_manifests(paths: &WorkspacePaths) -> Result<Vec<DatasetManifest>> {
    list_json_files(&paths.datasets_dir)?
        .into_iter()
        .map(read_json)
        .collect()
}

pub fn read_dataset_manifest(paths: &WorkspacePaths, dataset_id: &str) -> Result<DatasetManifest> {
    read_json(paths.datasets_dir.join(format!("{dataset_id}.json")))
}

pub fn read_compiled_profile_manifests(
    paths: &WorkspacePaths,
) -> Result<Vec<CompiledProfileManifest>> {
    list_json_files(&paths.compiled_profiles_dir)?
        .into_iter()
        .map(read_json)
        .collect()
}

pub fn read_run_manifest(path: impl AsRef<Path>) -> Result<RunManifest> {
    read_json(path)
}

/// Whether `path` starts with the current sectioned bundle magic. Bundles
/// written by older releases fail this check and must be rebuilt.
pub fn is_current_bundle_format(path: impl AsRef<Path>) -> bool {
    sectioned::has_current_magic(path.as_ref())
}

/// What [`prune_bundle_cache`] removed (or would remove on a dry run).
#[derive(Debug, Default)]
pub struct CachePruneReport {
    /// Compiled-profile manifests with the reason each one is stale.
    pub compiled_manifests: Vec<(PathBuf, &'static str)>,
    /// Bundle files no remaining manifest references, with their sizes.
    pub bundle_files: Vec<(PathBuf, u64)>,
}

impl CachePruneReport {
    pub fn reclaimed_bytes(&self) -> u64 {
        self.bundle_files.iter().map(|(_, bytes)| bytes).sum()
    }
}

/// Removes compiled profiles that can no longer be used and bundle files
/// that nothing references.
///
/// A compiled-profile manifest is stale when its dataset is gone, the
/// dataset was re-imported (different topology bundle), its bundle is
/// missing or uses an old format, or a newer compile of the same profile id
/// exists for the dataset (each profile edit gets a new hash and file).
/// Stale profiles are recompiled on demand. Afterwards, files under the
/// topology, name, acceleration and metric bundle directories that no
/// dataset or compiled-profile manifest references are removed. Transit
/// bundles are left alone. With `dry_run`, nothing is deleted.
pub fn prune_bundle_cache(paths: &WorkspacePaths, dry_run: bool) -> Result<CachePruneReport> {
    use std::collections::{BTreeMap, BTreeSet};

    let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let datasets = read_dataset_manifests(paths)?
        .into_iter()
        .map(|manifest| (manifest.dataset_id.0.clone(), manifest))
        .collect::<BTreeMap<_, _>>();

    let mut compiled = Vec::new();
    for path in list_json_files(&paths.compiled_profiles_dir)? {
        // Unreadable manifests are left for the user to inspect.
        if let Ok(manifest) = read_json::<CompiledProfileManifest>(&path) {
            compiled.push((path, manifest));
        }
    }
    let mut newest = BTreeMap::<(String, String), String>::new();
    for (_, manifest) in &compiled {
        let key = (manifest.dataset_id.0.clone(), manifest.profile_id.clone());
        let entry = newest.entry(key).or_default();
        if manifest.created_at > *entry {
            entry.clone_from(&manifest.created_at);
        }
    }

    let mut report = CachePruneReport::default();
    let mut referenced = BTreeSet::new();
    for (path, manifest) in &compiled {
        let dataset = datasets.get(&manifest.dataset_id.0);
        let bundle_path = Path::new(&manifest.bundle.path);
        let reason = if dataset.is_none() {
            Some("dataset no longer exists")
        } else if manifest.topology_bundle_id.as_ref()
            != dataset
                .and_then(|dataset| dataset.topology_bundle.as_ref())
                .map(|bundle| &bundle.bundle_id)
        {
            Some("dataset was re-imported")
        } else if !bundle_path.exists() {
            Some("bundle file is missing")
        } else if !is_current_bundle_format(bundle_path) {
            Some("bundle uses an old format")
        } else if newest
            .get(&(manifest.dataset_id.0.clone(), manifest.profile_id.clone()))
            .is_some_and(|created_at| *created_at != manifest.created_at)
        {
            Some("superseded by a newer compile of the same profile")
        } else {
            None
        };
        match reason {
            Some(reason) => report.compiled_manifests.push((path.clone(), reason)),
            None => {
                referenced.insert(canonical(bundle_path));
            }
        }
    }
    for dataset in datasets.values() {
        for bundle in [
            &dataset.topology_bundle,
            &dataset.edge_name_bundle,
            &dataset.acceleration_bundle,
        ]
        .into_iter()
        .flatten()
        {
            referenced.insert(canonical(Path::new(&bundle.path)));
        }
    }

    for dir in [
        &paths.topology_bundles_dir,
        &paths.edge_name_bundles_dir,
        &paths.acceleration_bundles_dir,
        &paths.metric_bundles_dir,
    ] {
        for entry in
            fs::read_dir(dir).with_context(|| format!("reading directory {}", dir.display()))?
        {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_file() && !referenced.contains(&canonical(&entry.path())) {
                report.bundle_files.push((entry.path(), metadata.len()));
            }
        }
    }
    report.bundle_files.sort();

    if !dry_run {
        for (path, _) in &report.compiled_manifests {
            fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
        }
        for (path, _) in &report.bundle_files {
            fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
