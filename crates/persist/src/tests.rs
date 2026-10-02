use super::{
    read_acceleration_bundle, read_compiled_profile_bundle, read_edge_name_bundle,
    read_topology_bundle, write_acceleration_bundle, write_compiled_profile_bundle,
    write_edge_name_bundle, write_topology_bundle,
};
use netweevil_core::{
    ACCELERATION_BUNDLE_SCHEMA_VERSION, AccessMask, CCH_ALGORITHM, CCH_WEIGHT_INFINITY,
    CCH_WEIGHT_OVERFLOW, COMPILED_ACCELERATION_SCHEMA_VERSION,
    COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION, CacheBundleId, CompiledAcceleration,
    CompiledEdgeMetric, CompiledProfileBundle, CompiledTurnCostConfig, DatasetAccelerationBundle,
    DirectedEdge, EdgeId, EdgeNameBundle, FeatureAttributeColumn, FeatureAttributeColumnData,
    FeatureAttributeDefinition, FeatureAttributeTable, FeatureAttributeType, MinuteInterval,
    NodeId, RoadClass, SmoothnessClass, SurfaceClass, TOPOLOGY_BUNDLE_SCHEMA_VERSION,
    TemporalEffect, TemporalRule, TemporalRuleSet, TopologyBundle, TopologyEdgeLayers,
    TopologyNode, TravelMode, TurnRestriction, TurnRestrictionKind,
};
use netweevil_core::{
    AdrTunnelCategory, CalendarDate, CompiledTemporalProfile, CompiledTemporalRules, DateRange,
    TurnRestrictionCondition, VehicleRestriction, VehicleRestrictionKind, VehicleRestrictionTable,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Temp file path that is removed on drop.
struct TempFile(PathBuf);

impl TempFile {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self(std::env::temp_dir().join(format!(
            "netweevil-persist-{tag}-{}-{}.bin",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn round_trips_topology_bundle_binary() {
    let forward = DirectedEdge {
        edge_id: EdgeId(0),
        from: NodeId(0),
        to: NodeId(1),
        source_way_id: 200,
        length_m: 123,
        ascent_m: 5.5,
        descent_m: 0.0,
        feature_row: 0,
        source_direction: 1,
        temporal_rule_id: Some(0),
        duration_s: None,
        road_class: RoadClass::Path,
        surface: SurfaceClass::Gravel,
        smoothness: SmoothnessClass::Bad,
        access_mask: AccessMask::new(AccessMask::FOOT),
        is_toll: false,
        max_speed_kph: None,
        lanes: None,
        name_index: Some(0),
        geometry_offset: 0,
        geometry_len: 0,
        flags: 0,
    };
    let reverse = DirectedEdge {
        edge_id: EdgeId(1),
        from: NodeId(1),
        to: NodeId(0),
        source_way_id: 201,
        length_m: 456,
        ascent_m: 0.0,
        descent_m: 5.5,
        feature_row: 1,
        source_direction: -1,
        temporal_rule_id: None,
        duration_s: Some(45.0),
        road_class: RoadClass::Residential,
        surface: SurfaceClass::Paved,
        smoothness: SmoothnessClass::Good,
        access_mask: AccessMask::new(AccessMask::CAR | AccessMask::FOOT),
        is_toll: true,
        name_index: None,
        flags: 1,
        ..forward
    };
    let column = |name: &str, semantic_role: &str, value_type, data| FeatureAttributeColumn {
        definition: FeatureAttributeDefinition {
            name: name.to_string(),
            semantic_role: Some(semantic_role.to_string()),
            value_type,
            domain: Default::default(),
        },
        data,
    };
    let bundle = TopologyBundle {
        schema_version: TOPOLOGY_BUNDLE_SCHEMA_VERSION,
        source_path: "dataset.osm.pbf".to_string(),
        source_sha256: "abc123".to_string(),
        nodes: vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 6.5,
                lat: 53.2,
                z: 12.5,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 6.6,
                lat: 53.3,
                z: 18.0,
            },
        ],
        edge_layers: TopologyEdgeLayers {
            vehicle_restrictions: VehicleRestrictionTable {
                restrictions: vec![
                    VehicleRestriction {
                        modes: AccessMask::new(AccessMask::VEHICLES),
                        kind: VehicleRestrictionKind::MaxHeightCm(380),
                    },
                    VehicleRestriction {
                        modes: AccessMask::new(AccessMask::HGV),
                        kind: VehicleRestrictionKind::AdrTunnel(AdrTunnelCategory::D),
                    },
                ],
                edges: vec![0, 0, 1],
                edge_restrictions: vec![0, 1, 0],
            },
            ..TopologyEdgeLayers::from_directed_edges(&[forward, reverse])
        },
        turn_restrictions: vec![TurnRestriction {
            relation_id: 300,
            kind: TurnRestrictionKind::NoTurn,
            edge_path: vec![EdgeId(0)],
            mode_mask: AccessMask::new(AccessMask::FOOT),
            condition: Some(TurnRestrictionCondition {
                rule_set_id: 0,
                applies_statically: true,
            }),
        }],
        names: vec!["path name".to_string()],
        edge_based_topology: Default::default(),
        spatial_index: None,
        node_component_ids: vec![0, 0],
        edge_component_ids: vec![0, 0],
        feature_attributes: FeatureAttributeTable {
            row_count: 2,
            strings: vec!["outdoor".to_string(), "paid".to_string()],
            columns: vec![
                column(
                    "feature_id",
                    "feature_id",
                    FeatureAttributeType::Integer,
                    FeatureAttributeColumnData::Integer(vec![Some(200), Some(201)]),
                ),
                column(
                    "Location",
                    "indoor_location",
                    FeatureAttributeType::String,
                    FeatureAttributeColumnData::String(vec![Some(0), Some(1)]),
                ),
            ],
        },
        temporal_rule_sets: vec![TemporalRuleSet {
            rules: vec![TemporalRule {
                modes: AccessMask::HGV,
                dates: Some(DateRange {
                    start: CalendarDate {
                        year: None,
                        month: 11,
                        day: 1,
                    },
                    end: CalendarDate {
                        year: None,
                        month: 4,
                        day: 1,
                    },
                }),
                weekdays_include_holidays: true,
                ..TemporalRule::new(
                    netweevil_core::EVERY_DAY,
                    vec![MinuteInterval {
                        start_minute: 9 * 60,
                        end_minute: 18 * 60,
                    }],
                    TemporalEffect::MaxSpeed(80.0),
                )
            }],
        }],
    };

    let file = TempFile::new("topology");
    write_topology_bundle(file.path(), &bundle).expect("bundle should serialize");
    let read = read_topology_bundle(file.path()).expect("bundle should deserialize");

    assert_eq!(read.schema_version, bundle.schema_version);
    assert_eq!(read.source_path, bundle.source_path);
    assert_eq!(read.source_sha256, bundle.source_sha256);
    assert_eq!(read.nodes.len(), 2);
    assert_eq!(read.nodes[0].z, 12.5);
    assert_eq!(read.edge_count(), 2);
    let (forward, reverse) = (read.routing_edge(0), read.routing_edge(1));
    assert_eq!(forward.ascent_m, 5.5);
    assert_eq!(reverse.descent_m, 5.5);
    assert_eq!(forward.feature_row, 0);
    assert_eq!(forward.source_direction, 1);
    assert_eq!(forward.temporal_rule_id, Some(0));
    assert_eq!(read.edge_profile(0).duration_s, None);
    assert_eq!(read.edge_profile(1).duration_s, Some(45.0));
    assert!(read.edge_profile(1).is_toll);
    assert!(
        read.feature_attributes
            .value_matches(1, "indoor_location", "paid")
    );
    assert_eq!(read.temporal_rule_sets, bundle.temporal_rule_sets);
    assert_eq!(
        read.edge_layers.vehicle_restrictions,
        bundle.edge_layers.vehicle_restrictions
    );
    assert_eq!(read.turn_restrictions.len(), 1);
    assert_eq!(
        read.turn_restrictions[0].condition,
        bundle.turn_restrictions[0].condition
    );
    assert_eq!(read.names, bundle.names);
}

#[test]
fn round_trips_edge_name_bundle_binary() {
    let bundle = EdgeNameBundle {
        names: vec!["main street".to_string(), "harbor road".to_string()],
    };
    let file = TempFile::new("edge-names");
    write_edge_name_bundle(file.path(), &bundle).expect("bundle should serialize");
    let read = read_edge_name_bundle(file.path()).expect("edge-name bundle should deserialize");
    assert_eq!(read.names, bundle.names);
}

#[test]
fn round_trips_acceleration_bundle_binary() {
    let bundle = DatasetAccelerationBundle {
        schema_version: ACCELERATION_BUNDLE_SCHEMA_VERSION,
        source_topology_bundle_id: CacheBundleId::new("topology-test"),
        algorithm: CCH_ALGORITHM.to_string(),
        stats: Default::default(),
        edge_order: vec![0, 2, 1],
        edge_rank: vec![0, 2, 1],
        upward_first_out: vec![0, 1, 1, 1],
        upward_head: vec![2],
        downward_first_out: vec![0, 0, 1, 1],
        downward_head: vec![0],
    };
    let file = TempFile::new("acceleration");
    write_acceleration_bundle(file.path(), &bundle).expect("bundle should serialize");
    let read =
        read_acceleration_bundle(file.path()).expect("acceleration bundle should deserialize");

    assert_eq!(read.schema_version, bundle.schema_version);
    assert_eq!(read.algorithm, bundle.algorithm);
    assert_eq!(read.stats, bundle.stats);
    assert_eq!(read.edge_order, bundle.edge_order);
    assert_eq!(read.edge_rank, bundle.edge_rank);
    assert_eq!(read.upward_first_out, bundle.upward_first_out);
    assert_eq!(read.upward_head, bundle.upward_head);
    assert_eq!(read.downward_first_out, bundle.downward_first_out);
    assert_eq!(read.downward_head, bundle.downward_head);
}

#[test]
fn round_trips_compiled_profile_bundle_with_turn_costs() {
    let mut bundle = CompiledProfileBundle {
        schema_version: COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION,
        profile_id: "test".to_string(),
        profile_hash: "abc".to_string(),
        mode: TravelMode::Car,
        turn_costs: CompiledTurnCostConfig {
            left_penalty_s: 7.0,
            right_penalty_s: 3.0,
            uturn_penalty_s: 20.0,
            traffic_signal_penalty_s: 4.0,
            roundabout_entry_penalty_s: 2.0,
            cost_time_weight: 1.5,
            destination_only_penalty_s: 600.0,
        },
        components: Vec::new(),
        temporal: CompiledTemporalProfile {
            rules: CompiledTemporalRules {
                rule_sets: vec![TemporalRuleSet {
                    rules: vec![TemporalRule::new(
                        netweevil_core::EVERY_DAY,
                        Vec::new(),
                        TemporalEffect::SpeedFactor(1.3),
                    )],
                }],
                edges: vec![0],
                edge_rule_sets: vec![0],
            },
            ..Default::default()
        },
        source_topology_bundle_id: CacheBundleId::new("topology-test"),
        acceleration: None,
        edge_metrics: vec![CompiledEdgeMetric {
            edge_id: EdgeId(0),
            travel_time_s: Some(12.0),
            generalized_cost: Some(18.0),
        }],
    };
    let file = TempFile::new("compiled-profile");

    write_compiled_profile_bundle(file.path(), &bundle).expect("bundle should serialize");
    let read = read_compiled_profile_bundle(file.path()).expect("bundle should deserialize");
    assert_eq!(read.turn_costs.left_penalty_s, 7.0);
    assert_eq!(read.turn_costs.cost_time_weight, 1.5);
    assert_eq!(read.turn_costs.destination_only_penalty_s, 600.0);
    assert_eq!(read.temporal.rules, bundle.temporal.rules);
    assert_eq!(read.edge_metrics.len(), 1);

    // Fixed-point weights, including both sentinels, are dropped from every
    // per-metric weight set while the acceleration header is kept.
    let weights = vec![0, 12345, CCH_WEIGHT_OVERFLOW, CCH_WEIGHT_INFINITY];
    bundle.acceleration = Some(CompiledAcceleration {
        schema_version: COMPILED_ACCELERATION_SCHEMA_VERSION,
        source_acceleration_bundle_id: CacheBundleId::new("acceleration-test"),
        algorithm: CCH_ALGORITHM.into(),
        upward_weight: weights.clone(),
        downward_weight: weights.clone(),
        time_upward_weight: weights.clone(),
        time_downward_weight: weights.clone(),
        distance_upward_weight: weights.clone(),
        distance_downward_weight: weights.clone(),
    });
    write_compiled_profile_bundle(file.path(), &bundle).expect("fixed-point bundle serializes");
    let read = read_compiled_profile_bundle(file.path()).expect("fixed-point bundle reads");
    // Customized weights are rebuilt at engine load, never persisted.
    let acceleration = read.acceleration.expect("acceleration retained");
    assert_eq!(
        acceleration.source_acceleration_bundle_id.0,
        "acceleration-test"
    );
    for actual in [
        &acceleration.upward_weight,
        &acceleration.downward_weight,
        &acceleration.time_upward_weight,
        &acceleration.time_downward_weight,
        &acceleration.distance_upward_weight,
        &acceleration.distance_downward_weight,
    ] {
        assert!(actual.is_empty());
    }
    assert_eq!(read.edge_metrics[0].travel_time_s, Some(12.0));
    assert_eq!(read.edge_metrics[0].generalized_cost, Some(18.0));
}

#[test]
fn prune_keeps_the_newest_compile_and_referenced_bundles() {
    use netweevil_manifest::{BundleRef, CompiledProfileManifest, DatasetManifest};

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("netweevil-prune-{unique}"));
    let paths = super::WorkspacePaths::discover(&root).expect("workspace");
    let bundle = |dir: &Path, name: &str| {
        let path = dir.join(name);
        std::fs::write(&path, b"NWSECB06").expect("bundle written");
        BundleRef {
            bundle_id: CacheBundleId::new(name),
            path: path.display().to_string(),
        }
    };
    let topology = bundle(&paths.topology_bundles_dir, "topology-a.bin");
    super::write_dataset_manifest(
        &paths,
        &DatasetManifest {
            dataset_id: netweevil_core::DatasetId::new("city"),
            label: "city".to_string(),
            source_path: "city.osm.pbf".to_string(),
            source_paths: Vec::new(),
            source_sha256: "abc".to_string(),
            source_size_bytes: 1,
            source_format: Default::default(),
            imported_at: "2026-01-01T00:00:00Z".to_string(),
            build_stage: netweevil_core::BuildStage::TopologyReady,
            topology_bundle: Some(topology.clone()),
            edge_name_bundle: None,
            acceleration_bundle: None,
            topology_meta: None,
            acceleration_stats: None,
        },
    )
    .expect("dataset manifest");
    for (hash, created_at) in [
        ("old", "2026-01-01T00:00:00Z"),
        ("new", "2026-02-01T00:00:00Z"),
    ] {
        super::write_compiled_profile_manifest(
            &paths,
            &CompiledProfileManifest {
                compile_id: format!("city-{hash}"),
                dataset_id: netweevil_core::DatasetId::new("city"),
                profile_id: "car".to_string(),
                profile_hash: hash.to_string(),
                defaults_pack: "test".to_string(),
                mode: TravelMode::Car,
                created_at: created_at.to_string(),
                topology_bundle_id: Some(topology.bundle_id.clone()),
                edge_count: None,
                bundle: bundle(&paths.metric_bundles_dir, &format!("metric-{hash}.bin")),
            },
        )
        .expect("compiled manifest");
    }
    bundle(&paths.acceleration_bundles_dir, "orphan.bin");

    let preview = super::prune_bundle_cache(&paths, true).expect("dry run");
    assert_eq!(preview.compiled_manifests.len(), 1);
    assert_eq!(preview.bundle_files.len(), 2);
    assert!(paths.metric_bundles_dir.join("metric-old.bin").exists());

    let report = super::prune_bundle_cache(&paths, false).expect("prune");
    assert_eq!(report.reclaimed_bytes(), 16);
    assert!(!paths.metric_bundles_dir.join("metric-old.bin").exists());
    assert!(!paths.acceleration_bundles_dir.join("orphan.bin").exists());
    assert!(paths.metric_bundles_dir.join("metric-new.bin").exists());
    assert!(paths.topology_bundles_dir.join("topology-a.bin").exists());
    assert_eq!(
        super::read_compiled_profile_manifests(&paths)
            .expect("manifests")
            .len(),
        1
    );
    std::fs::remove_dir_all(root).expect("temporary workspace removed");
}
