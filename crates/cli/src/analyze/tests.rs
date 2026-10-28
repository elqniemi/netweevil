use clap::Parser;

use super::*;

fn parse_analyze(args: &[&str]) -> AnalyzeCommand {
    let cli = crate::Cli::try_parse_from(["netweevil", "analyze"].iter().chain(args))
        .unwrap_or_else(|error| panic!("{args:?} should parse: {error}"));
    let crate::Command::Analyze { command } = cli.command else {
        panic!("expected analyze command");
    };
    command
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn parses_repeatable_temporal_analyze_overrides() {
    let AnalyzeCommand::Route(args) = parse_analyze(&[
        "route",
        "--dataset",
        "hk-pedestrian",
        "--profile",
        "pedestrian.yml",
        "--request",
        "route.json",
        "--departure-time",
        "2026-07-10T09:55:00+08:00",
        "--scenario",
        "central.yml",
        "--holiday-calendar",
        "holidays.yml",
        "--overlay",
        "shade.csv",
        "--overlay",
        "heat.csv",
    ]) else {
        panic!("expected analyze route command");
    };
    let temporal = args.temporal;
    assert_eq!(
        temporal.departure_time.as_deref(),
        Some("2026-07-10T09:55:00+08:00")
    );
    assert_eq!(temporal.scenario.as_deref(), Some(Path::new("central.yml")));
    assert_eq!(
        temporal.holiday_calendar.as_deref(),
        Some(Path::new("holidays.yml"))
    );
    assert_eq!(
        temporal.overlays,
        [PathBuf::from("shade.csv"), PathBuf::from("heat.csv")]
    );
}

#[test]
fn temporal_overrides_inline_local_files_and_preserve_unmentioned_fields() {
    let directory = std::env::temp_dir().join(format!(
        "netweevil-cli-temporal-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let scenario_path = directory.join("cli-scenario.yml");
    std::fs::write(&scenario_path, "id: cli_scenario\nfeatures: []\n").unwrap();
    let overlay_path = directory.join("cli-overlay.csv");
    std::fs::write(
        &overlay_path,
        "feature_id,t_start,t_end,shade\n7,2026-07-10T08:00:00Z,2026-07-10T09:00:00Z,0.5\n",
    )
    .unwrap();
    let request_overlay = netweevil_query::TemporalOverlayDocument {
        id: Some("request".into()),
        rows: Vec::new(),
    };
    let mut temporal = TemporalRequestOptions {
        departure_time: Some("2026-07-10T08:00:00+08:00".to_string()),
        holidays: vec!["2026-12-25".to_string()],
        overlays: vec![request_overlay.clone()],
        ..TemporalRequestOptions::default()
    };
    let applied = TemporalOverrideArgs {
        scenario: Some(scenario_path),
        overlays: vec![overlay_path],
        ..TemporalOverrideArgs::default()
    }
    .apply_to(&mut temporal);
    let _ = std::fs::remove_dir_all(&directory);
    applied.expect("local files load");

    assert_eq!(
        temporal.departure_time.as_deref(),
        Some("2026-07-10T08:00:00+08:00")
    );
    assert_eq!(
        temporal
            .scenario
            .and_then(|scenario| scenario.id)
            .as_deref(),
        Some("cli_scenario")
    );
    assert_eq!(temporal.holidays, ["2026-12-25"]);
    assert_eq!(temporal.overlays.len(), 2);
    assert_eq!(temporal.overlays[0], request_overlay);
    let row = &temporal.overlays[1].rows[0];
    assert_eq!(row.feature_id, 7);
    assert_eq!(row.values.get("shade"), Some(&0.5));
}

#[test]
fn parses_transit_network_street_inputs() {
    for (subcommand, request_flag) in [
        ("transit-route", "--request"),
        ("transit-batch", "--requests"),
    ] {
        let (dataset, profile) = match parse_analyze(&[
            subcommand,
            "--feed",
            "hk-mtr",
            request_flag,
            "transit.json",
            "--street-dataset",
            "hk-pedestrian",
            "--street-profile",
            "step-free.yml",
        ]) {
            AnalyzeCommand::TransitRoute(args) => (args.street_dataset, args.street_profile),
            AnalyzeCommand::TransitBatch(args) => (args.street_dataset, args.street_profile),
            _ => panic!("expected {subcommand} command"),
        };
        assert_eq!(dataset.as_deref(), Some("hk-pedestrian"), "{subcommand}");
        assert_eq!(
            profile.as_deref(),
            Some(Path::new("step-free.yml")),
            "{subcommand}"
        );
    }
}

#[test]
fn cli_network_street_estimator_rejects_non_walk_modes() {
    let mut request =
        load_transit_request(repository().join("examples/requests/transit_openov_groningen.json"))
            .expect("transit request loads");
    request.modes.street_access = TransitStreetAccessModel::Network;
    assert!(
        validate_network_street_access_modes(&request).expect("walk-only network modes are valid")
    );

    request.modes.access = vec![AccessMode::Bicycle];
    request.modes.egress = vec![AccessMode::Bicycle];
    assert!(validate_network_street_access_modes(&request).is_err());
}

#[test]
fn hong_kong_escalator_fixture_uses_materialized_source_feature() {
    let mapping = netweevil_ingest::load_gpkg_mapping(
        repository().join("examples/ingest/hong_kong_pedestrian_mapping.yml"),
    )
    .expect("Hong Kong mapping loads");
    assert_eq!(mapping.materialize_both_directions_for.len(), 33);
    assert!(
        mapping
            .materialize_both_directions_for
            .contains(&250_009_423)
    );

    let scenario = netweevil_query::load_scenario_overlay(
        repository().join("examples/scenarios/hong_kong_central_escalator_example.yml"),
    )
    .expect("Central-Mid-Levels scenario loads");
    assert_eq!(scenario.features.len(), 1);
    assert_eq!(scenario.features[0].source_feature_id, 250_009_423);
    assert!(scenario.features[0].replace_rules);
}

/// Example request documents name local files that the loaders inline, so
/// the executed requests carry content only.
#[test]
fn example_requests_inline_their_referenced_files() {
    let requests = repository().join("examples/requests");
    let route =
        netweevil_query::load_route_request(requests.join("hong_kong_example_temporal_route.json"))
            .expect("temporal route example loads");
    assert!(route.temporal.scenario.is_some());
    assert!(!route.temporal.holidays.is_empty());
    assert!(!route.temporal.overlays[0].rows.is_empty());
    netweevil_query::load_betweenness_request(requests.join("hong_kong_example_betweenness.json"))
        .expect("betweenness example loads");
    netweevil_query::load_service_area_sequence_request(
        requests.join("hong_kong_example_service_area_sequence.json"),
    )
    .expect("service-area sequence example loads");
    let batch = netweevil_query::load_scenario_batch_request(
        requests.join("hong_kong_example_scenario_batch.yml"),
    )
    .expect("scenario batch example loads");
    assert_eq!(batch.scenarios.len(), 2);
}

#[test]
fn directions_flags_override_the_api_shaped_request_file() {
    let AnalyzeCommand::Directions(args) = parse_analyze(&[
        "directions",
        "--dataset",
        "north-nl",
        "--profile",
        "car.yml",
        "--request",
        "directions.json",
        "--units",
        "imperial",
        "--driving-side",
        "left",
        "--facility-attribute",
        "indoor_kind",
    ]) else {
        panic!("expected analyze directions command");
    };
    let mut document =
        load_directions_request(&repository().join("examples/api/north_nl_directions.json"))
            .expect("API directions payload loads");
    assert_eq!(document.request.route_id, "groningen_directions");
    args.apply_to(&mut document.directions_options);

    // The file's language stays; the flags replace units, side and attribute.
    let options = document.directions_options;
    assert_eq!(options.language.as_deref(), Some("nl"));
    assert_eq!(options.units, DistanceUnits::Imperial);
    assert_eq!(options.driving_side, Some(DrivingSide::Left));
    assert_eq!(options.facility_attribute.as_deref(), Some("indoor_kind"));

    // A bare route request reads as a directions request with default options.
    let bare = load_directions_request(&repository().join("examples/requests/route.json"))
        .expect("bare route request loads");
    assert_eq!(bare.directions_options, DirectionsOptions::default());
}

#[test]
fn directions_run_writes_the_api_result_and_a_run_manifest() {
    let root = std::env::temp_dir().join(format!(
        "netweevil-cli-directions-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let paths = WorkspacePaths::discover(&root).expect("workspace");
    publish_street_dataset(&paths, "street");
    let profile_path = repository().join("examples/profiles/car_research_v1.yml");
    crate::profile::profile_compile(&paths, "street", &profile_path).expect("profile compiles");
    let request_path = root.join("directions.json");
    std::fs::write(
        &request_path,
        serde_json::json!({
            "request": {
                "route_id": "cli-directions",
                "origin": {"id": "a", "lon": 6.5, "lat": 53.2},
                "destination": {"id": "b", "lon": 6.503, "lat": 53.2},
                "snap": {"max_distance_m": 50.0}
            },
            "directions_options": {"language": "fi", "units": "imperial"}
        })
        .to_string(),
    )
    .unwrap();
    let AnalyzeCommand::Directions(args) = parse_analyze(&[
        "directions",
        "--dataset",
        "street",
        "--profile",
        profile_path.to_str().unwrap(),
        "--request",
        request_path.to_str().unwrap(),
        "--language",
        "nl",
        "--driving-side",
        "left",
    ]) else {
        panic!("expected analyze directions command");
    };
    let profile = load_profile(&profile_path).unwrap();
    let read_json = |path: &Path| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let outcome = run_directions_analysis(&paths, &profile, &args).map(|(_, stored)| {
        (
            read_json(&stored.result_path),
            read_json(&stored.manifest_path),
        )
    });
    let _ = std::fs::remove_dir_all(&root);
    let (result, manifest) = outcome.expect("directions run succeeds");

    // The result has the `/v1/directions` result shape.
    assert_eq!(result["language"], "nl");
    assert_eq!(result["units"], "imperial");
    assert_eq!(result["driving_side"], "left");
    assert_eq!(result["driving_side_origin"], "request");
    assert_eq!(result["route"]["route_id"], "cli-directions");
    let maneuvers = result["maneuvers"].as_array().unwrap();
    assert_eq!(maneuvers.first().unwrap()["kind"], "depart");
    assert_eq!(maneuvers.last().unwrap()["kind"], "arrive");

    assert_eq!(manifest["run_kind"], "directions");
    assert_eq!(manifest["dataset_id"], "street");
    assert_eq!(
        manifest["effective_request"]["directions_options"]["language"],
        "nl"
    );
}

/// Writes a two-way, 300 m "Hoofdstraat" over four nodes as dataset `id`.
fn publish_street_dataset(paths: &WorkspacePaths, id: &str) {
    use netweevil_core::{
        AccessMask, CacheBundleId, DirectedEdge, EdgeBasedTopology, EdgeId, NO_FEATURE_ROW, NodeId,
        NodeSpatialIndex, RoadClass, SurfaceClass, TOPOLOGY_BUNDLE_SCHEMA_VERSION,
        TopologyEdgeLayers, TopologyNode,
    };
    use netweevil_persist::manifest::{BundleRef, DatasetManifest};

    let nodes = (0..4)
        .map(|node| TopologyNode {
            node_id: NodeId(node),
            lon: 6.5 + f64::from(node) * 0.0015,
            lat: 53.2,
            z: 0.0,
        })
        .collect::<Vec<_>>();
    let edges = (0..3)
        .flat_map(|node| [(node, node + 1), (node + 1, node)])
        .enumerate()
        .map(|(index, (from, to))| DirectedEdge {
            edge_id: EdgeId(index as u32),
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
            name_index: Some(0),
            geometry_offset: 0,
            geometry_len: 0,
            flags: 0,
        })
        .collect::<Vec<_>>();
    let topology = TopologyBundle {
        schema_version: TOPOLOGY_BUNDLE_SCHEMA_VERSION,
        source_path: "street".into(),
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
    };
    let bundle_ref = |dir: &Path, name: String| BundleRef {
        path: dir.join(format!("{name}.bin")).display().to_string(),
        bundle_id: CacheBundleId::new(name),
    };
    let topology_ref = bundle_ref(&paths.topology_bundles_dir, format!("topology-{id}"));
    netweevil_persist::write_topology_bundle(&topology_ref.path, &topology).unwrap();
    let names_ref = bundle_ref(&paths.edge_name_bundles_dir, format!("names-{id}"));
    netweevil_persist::write_edge_name_bundle(
        &names_ref.path,
        &EdgeNameBundle::from_names(vec!["Hoofdstraat".into()]),
    )
    .unwrap();
    let mut manifest: DatasetManifest = serde_json::from_value(serde_json::json!({
        "dataset_id": id,
        "label": id,
        "source_path": "street.osm.pbf",
        "source_size_bytes": 0,
        "imported_at": "2026-10-01T00:00:00Z",
        "build_stage": "topology_ready",
    }))
    .unwrap();
    manifest.topology_bundle = Some(topology_ref);
    manifest.edge_name_bundle = Some(names_ref);
    netweevil_persist::write_dataset_manifest(paths, &manifest).unwrap();
}
