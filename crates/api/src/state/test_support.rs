use super::*;

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

pub(crate) fn test_service(
    topology: TopologyBundle,
    document: ProfileDocument,
) -> Arc<ServiceRuntime> {
    Arc::new(test_runtime(topology, document))
}

/// [`test_service`] before it is shared, so tests can attach traffic.
pub(crate) fn test_runtime(topology: TopologyBundle, document: ProfileDocument) -> ServiceRuntime {
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
    ServiceRuntime {
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
        traffic: None,
    }
}
