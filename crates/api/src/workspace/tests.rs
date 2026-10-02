use super::*;
use netweevil_core::{
    AccessMask, BuildStage, CacheBundleId, DatasetId, DirectedEdge, EdgeBasedTopology, EdgeId,
    NO_FEATURE_ROW, NodeId, NodeSpatialIndex, RoadClass, SurfaceClass,
    TOPOLOGY_BUNDLE_SCHEMA_VERSION, TopologyBundle, TopologyEdgeLayers, TopologyNode,
};
use netweevil_manifest::{BundleRef, DatasetManifest};
use netweevil_persist::{write_dataset_manifest, write_topology_bundle};

fn temp_workspace() -> WorkspacePaths {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    WorkspacePaths::discover(std::env::temp_dir().join(format!("netweevil-ws-test-{unique}")))
        .unwrap()
}

/// Request fields that name files resolve only inside the state directory
/// (uploads, saved profiles) or the example profiles; server paths, `..`
/// traversal and path-shaped ids are rejected before any file is opened.
#[test]
fn workspace_requests_cannot_reach_files_outside_the_state_directory() {
    let paths = temp_workspace();
    let outside = paths.root.join("outside.yml");
    fs::write(&outside, "profile: {}\n").unwrap();
    fs::write(paths.uploads_dir.join("city.osm.pbf"), b"pbf").unwrap();
    fs::write(paths.profiles_dir.join("car.yml"), "profile: {}\n").unwrap();

    assert_eq!(
        resolve_upload(&paths, "city.osm.pbf").unwrap(),
        paths.uploads_dir.join("city.osm.pbf")
    );
    let outside_text = outside.display().to_string();
    for source in [
        outside_text.as_str(),
        "../outside.yml",
        "uploads/../../outside.yml",
        "/etc/hosts",
    ] {
        assert!(resolve_upload(&paths, source).is_err(), "{source}");
    }

    let saved = paths.profiles_dir.join("car.yml").canonicalize().unwrap();
    assert_eq!(resolve_profile_source(&paths, "car.yml").unwrap(), saved);
    assert_eq!(
        resolve_profile_source(&paths, &saved.display().to_string()).unwrap(),
        saved
    );
    let traversal = paths.profiles_dir.join("../../outside.yml");
    for profile in [
        outside_text.as_str(),
        traversal.to_str().unwrap(),
        "../outside.yml",
        "/etc/hosts",
    ] {
        assert!(
            resolve_profile_source(&paths, profile).is_err(),
            "{profile}"
        );
    }

    for id in ["../uploads/evil", "/etc/passwd", "a/b", ""] {
        assert!(!is_identifier(id), "{id}");
    }
    assert!(remove_dataset(&paths, "../../outside").is_err());
    remove_workspace_file(&paths, &paths.state_dir.join("../outside.yml"));
    assert!(
        outside.exists(),
        "files outside .netweevil are never deleted"
    );
    let _ = fs::remove_dir_all(&paths.root);
}

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
        elevation: Default::default(),
        regions: Default::default(),
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

/// Every profile template, and the copies of the vehicle templates under
/// `examples/profiles`, parses, declares the mode it is listed under, and
/// compiles to a profile that can use a street open to every mode.
#[test]
fn profile_templates_and_their_examples_compile() {
    let mut topology = street(3);
    for profile in &mut topology.edge_layers.profile {
        profile.access_mask = AccessMask::new(AccessMask::ALL);
    }
    let example_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/profiles");
    let examples = [
        ("truck", "truck_hgv_v1.yml"),
        ("bus", "bus_city_v1.yml"),
        ("taxi", "taxi_v1.yml"),
        ("motorcycle", "motorcycle_v1.yml"),
        ("moped", "moped_v1.yml"),
    ];
    let templates = PROFILE_TEMPLATES.iter().map(|template| {
        (
            template.template_id,
            template.mode,
            template.yaml.to_string(),
        )
    });
    let example_profiles = examples.iter().map(|&(template_id, file)| {
        let template = PROFILE_TEMPLATES
            .iter()
            .find(|template| template.template_id == template_id)
            .expect("example has a template");
        let yaml = fs::read_to_string(example_dir.join(file)).expect("example profile reads");
        (file, template.mode, yaml)
    });
    for (name, mode, yaml) in templates.chain(example_profiles) {
        let profile: ProfileDocument =
            serde_yaml::from_str(&yaml).unwrap_or_else(|error| panic!("{name} parses: {error}"));
        profile
            .validate()
            .unwrap_or_else(|error| panic!("{name} validates: {error}"));
        assert_eq!(
            serde_json::to_value(profile.profile.mode).unwrap(),
            json!(mode),
            "{name}"
        );
        let compiled = netweevil_profile::compile_profile_bundle(
            &profile,
            &topology,
            CacheBundleId::new("street"),
        )
        .unwrap_or_else(|error| panic!("{name} compiles: {error}"));
        assert!(
            compiled
                .edge_metrics
                .iter()
                .all(|metric| metric.generalized_cost.is_some()),
            "{name} cannot use the street"
        );
    }
}
