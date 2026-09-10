use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use anyhow::{Context, Result, bail};
use netweevil_core::{CacheBundleId, DatasetAccelerationBundle, DatasetId, TopologyBundle};
use netweevil_manifest::{BundleRef, CompiledProfileManifest, DatasetManifest, now_rfc3339};
use netweevil_persist::{
    WorkspaceConfig, WorkspacePaths, read_acceleration_bundle, read_compiled_profile_bundle,
    read_compiled_profile_manifests, read_dataset_manifest, read_edge_name_bundle, read_json,
    read_topology_bundle, write_compiled_profile_bundle, write_compiled_profile_manifest,
};
use netweevil_profile::{ProfileDocument, compile_profile_bundle_with_acceleration, load_profile};
use netweevil_query::PreparedRoutingEngine;
use netweevil_transit::{
    PreparedTransitRouter, TransitFeedManifest, read_transit_bundle, read_transit_transfer_table,
};
use serde::Serialize;
use tokio::sync::Semaphore;
use tracing::info;

use crate::dynamic_profiles::DynamicProfileCache;
use crate::error::ApiError;
use crate::jobs::JobRegistry;
use crate::simulation::SimulationRegistry;

#[derive(Debug, Clone)]
pub struct ApiServeOptions {
    pub bind: SocketAddr,
    /// Dataset to load at start. When absent, the saved workspace selection
    /// is used; when that is absent too, the API starts in setup mode and
    /// waits for the console (or `POST /v1/workspace/activate`).
    pub dataset_id: Option<String>,
    pub default_profile: Option<PathBuf>,
    pub profiles: Vec<PathBuf>,
    pub transit_feeds: Vec<String>,
    /// Directory of the built web console to serve at `/`.
    pub console_dir: Option<PathBuf>,
}

impl ApiServeOptions {
    /// The explicit selection from the command line, if one was given.
    pub fn explicit_selection(&self) -> Option<WorkspaceConfig> {
        let dataset_id = self.dataset_id.clone()?;
        let default_profile = self.default_profile.as_ref()?.display().to_string();
        Some(WorkspaceConfig {
            dataset_id,
            default_profile,
            profiles: self
                .profiles
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            transit_feeds: self.transit_feeds.clone(),
            updated_at: None,
        })
    }
}

#[derive(Clone)]
pub(crate) struct ApiState {
    pub(crate) workspace: Arc<Workspace>,
    pub(crate) simulations: Arc<SimulationRegistry>,
}

impl ApiState {
    /// The loaded network, or a 503 telling the caller to finish setup.
    pub(crate) fn runtime(&self) -> Result<Arc<ServiceRuntime>, ApiError> {
        self.workspace.runtime().ok_or_else(|| {
            ApiError::unavailable(
                "no dataset is loaded; open Setup in the console or POST /v1/workspace/activate",
            )
        })
    }
}

/// The `.netweevil/` workspace behind the API plus the currently loaded
/// runtime, which the setup flow can replace while the server runs.
pub(crate) struct Workspace {
    pub(crate) paths: WorkspacePaths,
    runtime: RwLock<Option<Arc<ServiceRuntime>>>,
    pub(crate) jobs: JobRegistry,
}

impl Workspace {
    pub(crate) fn new(paths: WorkspacePaths, runtime: Option<ServiceRuntime>) -> Self {
        Self {
            paths,
            runtime: RwLock::new(runtime.map(Arc::new)),
            jobs: JobRegistry::default(),
        }
    }

    pub(crate) fn runtime(&self) -> Option<Arc<ServiceRuntime>> {
        self.runtime
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn set_runtime(&self, runtime: Option<Arc<ServiceRuntime>>) {
        *self.runtime.write().unwrap_or_else(|e| e.into_inner()) = runtime;
    }
}

pub(crate) struct ServiceRuntime {
    pub(crate) workspace_root: PathBuf,
    /// The selection this runtime was loaded from (what `workspace.json` records).
    pub(crate) active: WorkspaceConfig,
    pub(crate) dataset_manifest: DatasetManifest,
    pub(crate) topology: Arc<TopologyBundle>,
    pub(crate) edge_names: OnceLock<Arc<[String]>>,
    pub(crate) routing_workers: Arc<Semaphore>,
    pub(crate) default_profile_id: String,
    pub(crate) profiles: BTreeMap<String, LoadedProfile>,
    pub(crate) dynamic_profiles: DynamicProfileCache,
    /// Feeds can be added and removed while running (GTFS editor builds).
    transit_feeds: RwLock<BTreeMap<String, Arc<LoadedTransitFeed>>>,
    pub(crate) capabilities: ServiceCapabilities,
    pub(crate) engine: EngineDescription,
}

impl ServiceRuntime {
    pub(crate) fn transit_feed(&self, feed_id: &str) -> Option<Arc<LoadedTransitFeed>> {
        self.transit_feeds
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(feed_id)
            .cloned()
    }

    pub(crate) fn transit_feeds(&self) -> Vec<Arc<LoadedTransitFeed>> {
        self.transit_feeds
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub(crate) fn transit_feed_ids(&self) -> Vec<String> {
        self.transit_feeds
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    pub(crate) fn insert_transit_feed(&self, feed: LoadedTransitFeed) {
        self.transit_feeds
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(feed.manifest.feed_id.clone(), Arc::new(feed));
    }

    pub(crate) fn remove_transit_feed(&self, feed_id: &str) -> bool {
        self.transit_feeds
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(feed_id)
            .is_some()
    }
}

pub(crate) struct LoadedProfile {
    pub(crate) source_path: PathBuf,
    pub(crate) document: ProfileDocument,
    pub(crate) manifest: CompiledProfileManifest,
    pub(crate) engine: Arc<PreparedRoutingEngine>,
    pub(crate) dataset_acceleration: Option<Arc<DatasetAccelerationBundle>>,
}

pub(crate) struct LoadedTransitFeed {
    pub(crate) manifest: TransitFeedManifest,
    pub(crate) router: Arc<PreparedTransitRouter>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct EngineDescription {
    route_engine: &'static str,
    batch_engine: &'static str,
    acceleration: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ServiceCapabilities {
    analyses: Vec<&'static str>,
    geometry: Vec<&'static str>,
    breakdown_metrics: Vec<&'static str>,
    connectivity_policies: Vec<&'static str>,
    failure_modes: Vec<&'static str>,
}

fn service_capabilities() -> ServiceCapabilities {
    ServiceCapabilities {
        analyses: vec![
            "route",
            "locate",
            "directions",
            "match",
            "waypoints",
            "od",
            "matrix",
            "accessibility",
            "service_area",
            "service_area_sequence",
            "betweenness",
            "scenario_batch",
            "transit_route",
            "transit_service_area",
            "simulation",
        ],
        geometry: vec!["none", "full", "segments"],
        breakdown_metrics: vec!["time_s", "distance_m"],
        connectivity_policies: vec![
            "strict",
            "ignore_unreachable",
            "hop_origin_to_nearest_reachable_component",
            "hop_destination_to_nearest_reachable_component",
            "hop_either_end",
        ],
        failure_modes: vec![
            "auto_relax_unreachable",
            "allow_reverse_oneway",
            "allow_illegal_turn",
            "ignore_turn_restrictions",
            "allow_uturn_where_normally_forbidden",
        ],
    }
}

/// Loads a dataset, its profiles and transit feeds into memory. `progress`
/// receives one line per step so long loads can be followed from the console.
pub(crate) fn load_service_runtime(
    paths: &WorkspacePaths,
    selection: &WorkspaceConfig,
    mut progress: impl FnMut(&str),
) -> Result<ServiceRuntime> {
    info!(dataset_id = %selection.dataset_id, "loading dataset manifest");
    progress(&format!("Reading dataset '{}'", selection.dataset_id));
    let dataset_manifest = read_dataset_manifest(paths, &selection.dataset_id)
        .with_context(|| format!("reading dataset manifest for '{}'", selection.dataset_id))?;
    let topology_ref = dataset_manifest
        .topology_bundle
        .clone()
        .context("dataset is missing a topology bundle; run `netweevil dataset import` first")?;

    info!(
        bundle = %topology_ref.path,
        "loading topology bundle"
    );
    let topology = Arc::new(
        read_topology_bundle(&topology_ref.path)
            .with_context(|| format!("reading topology bundle {}", topology_ref.path))?,
    );
    let topology_meta = dataset_manifest.topology_meta.as_ref();
    info!(
        nodes = topology_meta.map(|m| m.node_count).unwrap_or(0),
        edges = topology_meta.map(|m| m.edge_count).unwrap_or(0),
        turns = topology_meta.map(|m| m.turn_count).unwrap_or(0),
        "topology loaded"
    );
    let routing_workers = Arc::new(Semaphore::new(
        std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1)
            .max(1),
    ));

    let mut loaded_profiles: BTreeMap<String, LoadedProfile> = BTreeMap::new();
    let default_profile_path = PathBuf::from(&selection.default_profile);
    let mut requested_paths = Vec::with_capacity(selection.profiles.len() + 1);
    requested_paths.push(default_profile_path.clone());
    requested_paths.extend(selection.profiles.iter().map(PathBuf::from));

    let compiled_manifests = read_compiled_profile_manifests(paths).unwrap_or_default();
    let mut default_profile_id = None;

    for profile_path in requested_paths {
        info!(path = %profile_path.display(), "loading profile");
        progress(&format!("Preparing profile {}", profile_path.display()));
        let loaded = load_or_compile_profile(
            paths,
            &dataset_manifest,
            topology_ref.bundle_id.clone(),
            topology.clone(),
            &compiled_manifests,
            &profile_path,
        )?;
        let profile_id = loaded.document.profile.id.clone();
        info!(
            profile_id = %profile_id,
            hash = %loaded.manifest.profile_hash,
            edge_count = loaded.manifest.edge_count.unwrap_or(0),
            "profile ready"
        );
        if profile_path == default_profile_path {
            default_profile_id = Some(profile_id.clone());
        }
        if let Some(existing) = loaded_profiles.get(&profile_id) {
            if existing.manifest.profile_hash != loaded.manifest.profile_hash {
                bail!(
                    "profile id '{}' was loaded from multiple files with different hashes",
                    profile_id
                );
            }
            continue;
        }
        loaded_profiles.insert(profile_id, loaded);
    }

    let default_profile_id =
        default_profile_id.context("default profile could not be loaded into the API runtime")?;
    let effective = loaded_profiles[&default_profile_id]
        .engine
        .effective_engine_description(netweevil_query::EngineMode::Auto);
    let engine = EngineDescription {
        route_engine: effective.route_engine,
        batch_engine: effective.batch_engine,
        acceleration: effective.acceleration,
    };

    let mut loaded_transit_feeds = BTreeMap::new();
    for feed_id in &selection.transit_feeds {
        progress(&format!("Loading transit feed '{feed_id}'"));
        let feed = load_transit_feed(paths, &selection.dataset_id, &loaded_profiles, feed_id)?;
        loaded_transit_feeds.insert(feed_id.clone(), Arc::new(feed));
    }

    info!(
        profile_count = loaded_profiles.len(),
        transit_feed_count = loaded_transit_feeds.len(),
        default_profile = %default_profile_id,
        route_engine = %engine.route_engine,
        "network ready"
    );

    Ok(ServiceRuntime {
        workspace_root: paths.root.clone(),
        active: selection.clone(),
        dataset_manifest,
        topology,
        edge_names: OnceLock::new(),
        routing_workers,
        default_profile_id,
        profiles: loaded_profiles,
        dynamic_profiles: DynamicProfileCache::from_env()?,
        transit_feeds: RwLock::new(loaded_transit_feeds),
        capabilities: service_capabilities(),
        engine,
    })
}

/// Reads one imported feed and prepares its router, attaching the transfer
/// tables built for the loaded dataset and profiles.
pub(crate) fn load_transit_feed(
    paths: &WorkspacePaths,
    dataset_id: &str,
    loaded_profiles: &BTreeMap<String, LoadedProfile>,
    feed_id: &str,
) -> Result<LoadedTransitFeed> {
    let manifest_path = paths.transit_feeds_dir.join(format!("{feed_id}.json"));
    let manifest: TransitFeedManifest = read_json(&manifest_path)
        .with_context(|| format!("reading transit feed manifest {}", manifest_path.display()))?;
    let bundle = Arc::new(
        read_transit_bundle(&manifest.bundle_path)
            .with_context(|| format!("reading transit bundle {}", manifest.bundle_path))?,
    );
    let transfer_tables = manifest
        .transfer_tables
        .iter()
        .filter(|entry| entry.dataset_id == dataset_id)
        .map(|entry| {
            if let Some(profile) = loaded_profiles.get(&entry.profile_id)
                && profile.manifest.profile_hash != entry.profile_hash
            {
                bail!(
                    "transit transfer table '{}' uses stale profile hash {}; loaded profile hash is {}",
                    entry.path,
                    entry.profile_hash,
                    profile.manifest.profile_hash
                );
            }
            let table = read_transit_transfer_table(&entry.path)
                .with_context(|| format!("reading transit transfer table {}", entry.path))?;
            if table.dataset_id != dataset_id
                || table.profile_hash != entry.profile_hash
                || table.profile_id != entry.profile_id
            {
                bail!(
                    "transit transfer table '{}' metadata does not match its feed manifest registration",
                    entry.path
                );
            }
            Ok(table)
        })
        .collect::<Result<Vec<_>>>()?;
    let router = Arc::new(if transfer_tables.is_empty() {
        PreparedTransitRouter::new(bundle)
    } else {
        PreparedTransitRouter::new_with_transfer_tables(bundle, transfer_tables)?
    });
    info!(
        feed_id = %manifest.feed_id,
        stop_count = manifest.stop_count,
        connection_count = manifest.connection_count,
        bound_stop_count = manifest.bound_stop_count,
        transfer_profiles = ?router.transfer_profile_ids(),
        "transit feed ready"
    );
    Ok(LoadedTransitFeed { manifest, router })
}

fn load_or_compile_profile(
    paths: &WorkspacePaths,
    dataset_manifest: &DatasetManifest,
    topology_bundle_id: CacheBundleId,
    topology: Arc<TopologyBundle>,
    compiled_manifests: &[CompiledProfileManifest],
    profile_path: &Path,
) -> Result<LoadedProfile> {
    let document = load_profile(profile_path)
        .with_context(|| format!("loading {}", profile_path.display()))?;
    document.validate()?;
    let profile_hash = document.fingerprint()?;
    let dataset_acceleration: Option<Arc<DatasetAccelerationBundle>> = dataset_manifest
        .acceleration_bundle
        .as_ref()
        .map(|bundle_ref| {
            read_acceleration_bundle(&bundle_ref.path)
                .with_context(|| format!("reading acceleration bundle {}", bundle_ref.path))
                .map(Arc::new)
        })
        .transpose()?;

    // Reuse a cached compiled bundle only when it still reads cleanly and its
    // acceleration matches the dataset's current CCH bundle; otherwise fall
    // through to a fresh compile (covers format migrations and re-imports).
    let cached = compiled_manifests
        .iter()
        .find(|manifest| {
            manifest.dataset_id.0 == dataset_manifest.dataset_id.0
                && manifest.profile_hash == profile_hash
        })
        .and_then(|manifest| {
            let bundle = read_compiled_profile_bundle(&manifest.bundle.path).ok()?;
            let acceleration_current = match (dataset_acceleration.as_ref(), &bundle.acceleration) {
                (Some(_), Some(acceleration)) => {
                    acceleration.algorithm == netweevil_core::CCH_ALGORITHM
                        && acceleration.schema_version == 3
                }
                (Some(_), None) => false,
                (None, _) => true,
            };
            acceleration_current.then(|| (manifest.clone(), bundle))
        });

    let (manifest, compiled_bundle) = if let Some(cached) = cached {
        cached
    } else {
        let compiled_bundle = compile_profile_bundle_with_acceleration(
            &document,
            topology.as_ref(),
            topology_bundle_id,
            dataset_manifest
                .acceleration_bundle
                .as_ref()
                .zip(dataset_acceleration.as_ref())
                .map(|(bundle_ref, bundle)| (bundle.as_ref(), bundle_ref.bundle_id.clone())),
        )
        .with_context(|| {
            format!(
                "compiling profile '{}' for dataset '{}'",
                document.profile.id, dataset_manifest.dataset_id.0
            )
        })?;
        let compile_id = format!("{}-{}", dataset_manifest.dataset_id.0, &profile_hash[..12]);
        let bundle_path = paths
            .metric_bundles_dir
            .join(format!("metric-{compile_id}.bin"));
        write_compiled_profile_bundle(&bundle_path, &compiled_bundle)?;
        let manifest = CompiledProfileManifest {
            compile_id: compile_id.clone(),
            dataset_id: DatasetId::new(dataset_manifest.dataset_id.0.clone()),
            profile_id: document.profile.id.clone(),
            profile_hash: profile_hash.clone(),
            defaults_pack: document.profile.defaults_pack.clone(),
            mode: document.profile.mode,
            created_at: now_rfc3339()?,
            topology_bundle_id: Some(compiled_bundle.source_topology_bundle_id.clone()),
            edge_count: Some(compiled_bundle.edge_metrics.len() as u64),
            bundle: BundleRef {
                bundle_id: CacheBundleId::new(format!("metric-{compile_id}")),
                path: bundle_path.display().to_string(),
            },
        };
        write_compiled_profile_manifest(paths, &manifest)?;
        (manifest, compiled_bundle)
    };

    let engine = Arc::new(
        PreparedRoutingEngine::new(
            topology,
            Arc::new(compiled_bundle),
            dataset_acceleration.clone(),
        )
        .with_context(|| {
            format!(
                "preparing in-memory routing engine for profile '{}'",
                document.profile.id
            )
        })?,
    );

    Ok(LoadedProfile {
        source_path: profile_path.to_path_buf(),
        document,
        manifest,
        engine,
        dataset_acceleration,
    })
}

pub(crate) fn resolve_profile<'a>(
    service: &'a ServiceRuntime,
    requested_profile_id: Option<&str>,
) -> Result<&'a LoadedProfile, ApiError> {
    let profile_id = requested_profile_id.unwrap_or(&service.default_profile_id);
    service
        .profiles
        .get(profile_id)
        .ok_or_else(|| ApiError::not_found(format!("unknown profile_id '{}'", profile_id)))
}

pub(crate) async fn execute_on_routing_worker<T, F>(
    service: &ServiceRuntime,
    job: F,
) -> Result<T, anyhow::Error>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let permit = service
        .routing_workers
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| anyhow::anyhow!("routing worker pool closed: {error}"))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        job()
    })
    .await
    .map_err(|error| anyhow::anyhow!("routing worker panicked: {error}"))?
}

pub(crate) fn load_edge_names(service: &ServiceRuntime) -> Result<Arc<[String]>, ApiError> {
    if let Some(edge_names) = service.edge_names.get() {
        return Ok(edge_names.clone());
    }

    let Some(bundle_ref) = service.dataset_manifest.edge_name_bundle.as_ref() else {
        return Err(ApiError::internal(format!(
            "dataset '{}' is missing the edge-name bundle required by the current format; remove the old cached dataset and re-import it",
            service.dataset_manifest.dataset_id.0
        )));
    };
    let loaded = {
        info!(bundle = %bundle_ref.path, "loading edge-name bundle");
        let bundle = read_edge_name_bundle(&bundle_ref.path).map_err(|error| {
            ApiError::internal(format!(
                "reading edge-name bundle {}: {error}",
                bundle_ref.path
            ))
        })?;
        Arc::<[String]>::from(bundle.names)
    };

    let _ = service.edge_names.set(loaded.clone());
    Ok(loaded)
}

#[cfg(test)]
impl ApiState {
    /// State over a throwaway workspace directory with `service` loaded.
    pub(crate) fn for_test(service: Arc<ServiceRuntime>) -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("netweevil-api-test-{unique}"));
        let paths = WorkspacePaths::discover(root).expect("temp workspace");
        let workspace = Workspace::new(paths, None);
        workspace.set_runtime(Some(service));
        Self {
            workspace: Arc::new(workspace),
            simulations: Arc::new(SimulationRegistry::default()),
        }
    }
}

#[cfg(test)]
pub(crate) fn test_service(
    topology: TopologyBundle,
    document: ProfileDocument,
) -> Arc<ServiceRuntime> {
    let topology = Arc::new(topology);
    let metrics = netweevil_profile::compile_profile_bundle(
        &document,
        &topology,
        CacheBundleId::new("test-topology"),
    )
    .unwrap();
    let profile_id = document.profile.id.clone();
    let profile_hash = document.fingerprint().unwrap();
    let engine = Arc::new(
        PreparedRoutingEngine::new(Arc::clone(&topology), Arc::new(metrics), None).unwrap(),
    );
    Arc::new(ServiceRuntime {
        workspace_root: PathBuf::new(),
        active: WorkspaceConfig::default(),
        dataset_manifest: serde_json::from_value(serde_json::json!({
            "dataset_id": "test", "label": "test", "source_path": "test",
            "source_sha256": "test", "source_size_bytes": 0,
            "imported_at": "test", "build_stage": "topology_ready",
            "topology_bundle": {"bundle_id": "test-topology", "path": "unused"}
        }))
        .unwrap(),
        engine: {
            let effective = engine.effective_engine_description(netweevil_query::EngineMode::Auto);
            EngineDescription {
                route_engine: effective.route_engine,
                batch_engine: effective.batch_engine,
                acceleration: effective.acceleration,
            }
        },
        topology,
        edge_names: OnceLock::new(),
        routing_workers: Arc::new(Semaphore::new(1)),
        default_profile_id: profile_id.clone(),
        profiles: BTreeMap::from([(
            profile_id.clone(),
            LoadedProfile {
                source_path: PathBuf::new(),
                document,
                manifest: serde_json::from_value(serde_json::json!({
                    "compile_id": "test", "dataset_id": "test", "profile_id": profile_id,
                    "profile_hash": profile_hash, "defaults_pack": "test", "mode": "car",
                    "created_at": "test", "bundle": {"bundle_id": "test", "path": "unused"}
                }))
                .unwrap(),
                engine,
                dataset_acceleration: None,
            },
        )]),
        dynamic_profiles: DynamicProfileCache::from_env().unwrap(),
        transit_feeds: RwLock::new(BTreeMap::new()),
        capabilities: service_capabilities(),
    })
}

#[cfg(test)]
mod tests {
    use super::service_capabilities;

    #[test]
    fn advertises_accessibility_analysis() {
        assert!(service_capabilities().analyses.contains(&"accessibility"));
    }
}
