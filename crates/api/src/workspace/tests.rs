use super::*;
use netweevil_core::{
    AccessMask, BuildStage, CacheBundleId, DatasetId, DirectedEdge, EdgeBasedTopology, EdgeId,
    NO_FEATURE_ROW, NodeId, NodeSpatialIndex, RoadClass, SurfaceClass,
    TOPOLOGY_BUNDLE_SCHEMA_VERSION, TopologyBundle, TopologyEdgeLayers, TopologyNode,
};
use netweevil_manifest::{BundleRef, DatasetManifest};
use netweevil_persist::{write_dataset_manifest, write_topology_bundle};

/// Two-way residential street over `node_count` nodes 100 m apart.
fn street(node_count: u32) -> TopologyBundle {
    let mut edges = Vec::new();
    for node in 0..node_count - 1 {
        for (from, to) in [(node, node + 1), (node + 1, node)] {
            edges.push(DirectedEdge {
                edge_id: EdgeId(edges.len() as u32),
                from: NodeId(from),
                to: NodeId(to),
                source_way_id: 1,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: NO_FEATURE_ROW,
                source_direction: if from < to { 1 } else { -1 },
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: Default::default(),
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            });
        }
    }
    let nodes = (0..node_count)
        .map(|node| TopologyNode {
            node_id: NodeId(node),
            lon: 6.5 + f64::from(node) * 0.0015,
            lat: 53.2,
            z: 0.0,
        })
        .collect::<Vec<_>>();
    TopologyBundle {
        schema_version: TOPOLOGY_BUNDLE_SCHEMA_VERSION,
        source_path: "street".into(),
        source_sha256: "street".into(),
        edge_based_topology: EdgeBasedTopology::build(nodes.len(), edges.len(), |index| {
            (edges[index].from.0, edges[index].to.0)
        }),
        spatial_index: NodeSpatialIndex::build(&nodes),
        node_component_ids: vec![0; nodes.len()],
        edge_component_ids: vec![0; edges.len()],
        nodes,
        edge_layers: TopologyEdgeLayers::from_directed_edges(&edges),
        turn_restrictions: Vec::new(),
        names: Vec::new(),
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
    }
}

/// Writes `topology` as the current build of dataset `city`.
fn publish(paths: &WorkspacePaths, topology: &TopologyBundle, build: &str) {
    let bundle_id = CacheBundleId::new(format!("topology-city-{build}"));
    let path = paths
        .topology_bundles_dir
        .join(format!("{}.bin", bundle_id.0));
    write_topology_bundle(&path, topology).expect("topology writes");
    let manifest: DatasetManifest = serde_json::from_value(json!({
        "dataset_id": "city",
        "label": "city",
        "source_path": "city.osm.pbf",
        "source_sha256": build,
        "source_size_bytes": 0,
        "imported_at": "2026-10-01T00:00:00Z",
        "build_stage": BuildStage::TopologyReady,
    }))
    .expect("manifest parses");
    write_dataset_manifest(
        paths,
        &DatasetManifest {
            dataset_id: DatasetId::new("city"),
            topology_bundle: Some(BundleRef {
                bundle_id,
                path: path.display().to_string(),
            }),
            ..manifest
        },
    )
    .expect("manifest writes");
}

#[test]
fn reload_swaps_in_an_updated_build_and_recompiles_stale_profiles() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("netweevil-api-reload-{unique}"));
    let paths = WorkspacePaths::discover(&root).expect("workspace");
    let selection = WorkspaceConfig {
        dataset_id: "city".to_string(),
        default_profile: concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/profiles/car_research_v1.yml"
        )
        .to_string(),
        ..Default::default()
    };
    publish(&paths, &street(3), "first");
    let workspace = Workspace::new(paths.clone(), None);
    reload_runtime(&workspace, &selection, |_| {}).expect("first build loads");
    let serving = workspace.runtime().expect("runtime loaded");

    publish(&paths, &street(5), "second");
    let summary = reload_runtime(&workspace, &selection, |_| {}).expect("second build loads");

    assert_eq!(summary["topology_bundle_id"], "topology-city-second");
    let reloaded = workspace.runtime().expect("runtime swapped");
    assert_eq!(reloaded.topology.nodes.len(), 5);
    // A request that held the previous runtime keeps a consistent view.
    assert_eq!(serving.topology.nodes.len(), 3);
    let profile = &reloaded.profiles[&reloaded.default_profile_id];
    assert_eq!(
        profile.manifest.topology_bundle_id,
        Some(CacheBundleId::new("topology-city-second"))
    );
    assert_eq!(profile.manifest.edge_count, Some(8));
    let compiled = read_compiled_profile_manifests(&paths).expect("compiled manifests read");
    assert_eq!(compiled.len(), 1);
    assert_eq!(
        compiled[0].topology_bundle_id,
        Some(CacheBundleId::new("topology-city-second"))
    );
    std::fs::remove_dir_all(root).expect("temporary workspace removed");
}
