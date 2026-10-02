use super::{compile_profile_bundle, compile_profile_bundle_with_acceleration};
use crate::ProfileDocument;
use crate::test_support::{edge, profile, topology, with_admin_areas};
use netweevil_core::{
    AccessMask, CCH_WEIGHT_INFINITY, CCH_WEIGHT_OVERFLOW, CacheBundleId, CompiledProfileBundle,
    DatasetAccelerationBundle, DirectedEdge, EdgeBasedTopology, FeatureAttributeColumn,
    FeatureAttributeColumnData, FeatureAttributeDefinition, FeatureAttributeTable,
    FeatureAttributeType, NodeId, RoadClass, SmoothnessClass, SurfaceClass, TopologyBundle,
    TopologyNode,
};
use std::collections::BTreeMap;

fn compile(profile: &ProfileDocument, topology: &TopologyBundle) -> CompiledProfileBundle {
    compile_profile_bundle(profile, topology, CacheBundleId::new("topology-test"))
        .expect("compile succeeds")
}

fn nodes(coordinates: &[(f64, f64, f64)]) -> Vec<TopologyNode> {
    coordinates
        .iter()
        .enumerate()
        .map(|(index, &(lon, lat, z))| TopologyNode {
            node_id: NodeId(index as u32),
            lon,
            lat,
            z,
        })
        .collect()
}

/// Car profile whose ferry boarding costs 300 s and is weighted by
/// `use_ferry: 0.2`.
fn ferry_profile(infer_duration_when_missing: bool) -> ProfileDocument {
    profile(
        "car",
        &format!(
            "ferry: {{allow: true, use_ferry: 0.2, boarding_cost_s: 300, \
             infer_duration_when_missing: {infer_duration_when_missing}, default_speed_kph: 20}}"
        ),
    )
}

fn ferry_topology(duration_s: Option<f64>) -> TopologyBundle {
    topology(&[DirectedEdge {
        duration_s,
        surface: SurfaceClass::Unknown,
        ..edge(0, RoadClass::Ferry, AccessMask::CAR)
    }])
}

#[test]
fn sub_metre_edges_keep_physical_cost_without_penalizing_coincident_connectors() {
    let profile = profile("foot", "cost: {time_weight: 1.0, distance_weight: 2.0}");
    let mut topology = topology(&[DirectedEdge {
        length_m: 0,
        surface: SurfaceClass::Unknown,
        ..edge(0, RoadClass::Path, AccessMask::FOOT)
    }]);
    topology.nodes = nodes(&[(114.18, 22.28, 0.0), (114.180003, 22.28, 0.0)]);
    let physical_length =
        netweevil_core::geo::distance_3d_meters(114.18, 22.28, 0.0, 114.180003, 22.28, 0.0);
    assert!(physical_length > 0.0 && physical_length < 0.5);
    let metric = |topology: &TopologyBundle| compile(&profile, topology).edge_metrics[0].clone();

    let cost = metric(&topology);
    let expected_time = physical_length / (5.0 / 3.6);
    assert!((cost.travel_time_s.unwrap() - expected_time).abs() < 1e-9);
    assert!((cost.generalized_cost.unwrap() - expected_time - 2.0 * physical_length).abs() < 1e-9);
    assert!(netweevil_core::encode_cch_weight(cost.generalized_cost.unwrap()) > 0);

    topology.nodes[1].lon = topology.nodes[0].lon;
    topology.nodes[1].z = 0.25;
    topology.edge_layers.routing[0].ascent_m = 0.25;
    assert!(
        metric(&topology).travel_time_s.unwrap() > 0.0,
        "vertical sub-metre edges must cost time"
    );

    topology.nodes[1].z = 0.0;
    topology.edge_layers.routing[0].ascent_m = 0.0;
    let coincident = metric(&topology);
    assert_eq!(coincident.travel_time_s, Some(0.0));
    assert_eq!(coincident.generalized_cost, Some(0.0));
}

#[test]
fn compiles_edge_metrics_from_topology() {
    let profile = profile(
        "car",
        "speed_rules:\n  - {match: {highway: residential}, speed_kph: 40}\n\
         factors:\n  - {match: {smoothness: bad}, speed_factor: 0.5}",
    );
    let topology = topology(&[DirectedEdge {
        smoothness: SmoothnessClass::Bad,
        ..edge(0, RoadClass::Residential, AccessMask::CAR)
    }]);

    let compiled = compile(&profile, &topology);

    // 1 km at 40 km/h halved by the smoothness factor.
    assert_eq!(compiled.edge_metrics.len(), 1);
    let travel_time = compiled.edge_metrics[0].travel_time_s.unwrap();
    assert!((170.0..190.0).contains(&travel_time));
    assert_eq!(compiled.source_topology_bundle_id.0, "topology-test");
}

#[test]
fn compiles_open_attributes_facilities_slope_and_named_components() {
    let profile = profile(
        "foot",
        r#"
slope_model:
  kind: tobler
facilities:
  attribute: feature_type
  classes:
    staircase:
      kind: stairs
      vertical_speed_mps: 0.25
      burden_per_vertical_m: 2.0
components:
  uncovered_time:
    expression: travel_time * (covered == false)
    weight: 0.5
  ascent:
    expression: ascent
    weight: 1.0
temporal:
  allow_wait: true
  max_wait_s: 900
"#,
    );
    let mut topology = topology(&[DirectedEdge {
        length_m: 100,
        ascent_m: 10.0,
        feature_row: 0,
        source_direction: 1,
        surface: SurfaceClass::Paved,
        smoothness: SmoothnessClass::Good,
        ..edge(0, RoadClass::Path, AccessMask::FOOT)
    }]);
    let column = |name: &str, semantic_role: &str, value_type, data| FeatureAttributeColumn {
        definition: FeatureAttributeDefinition {
            name: name.to_string(),
            semantic_role: Some(semantic_role.to_string()),
            value_type,
            domain: Default::default(),
        },
        data,
    };
    topology.feature_attributes = FeatureAttributeTable {
        row_count: 1,
        strings: vec!["staircase".to_string()],
        columns: vec![
            column(
                "FacilityType",
                "feature_type",
                FeatureAttributeType::String,
                FeatureAttributeColumnData::String(vec![Some(0)]),
            ),
            column(
                "Covered",
                "covered",
                FeatureAttributeType::Boolean,
                FeatureAttributeColumnData::Boolean(vec![Some(false)]),
            ),
        ],
    };

    let compiled = compile(&profile, &topology);
    let travel_time = compiled.edge_metrics[0].travel_time_s.unwrap();
    assert!(travel_time >= 40.0, "stairs must respect vertical speed");
    let components = compiled
        .components
        .iter()
        .map(|component| (component.name.as_str(), component.edge_values[0]))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(components["ascent"], 10.0);
    assert_eq!(components["stair_burden"], 20.0);
    assert!((f64::from(components["uncovered_time"]) - travel_time).abs() < 1e-4);
    assert!(compiled.temporal.allow_wait);
    assert_eq!(compiled.temporal.max_wait_s, 900.0);
}

#[test]
fn rejects_unsupported_match_keys() {
    let profile = profile(
        "car",
        "speed_rules:\n  - {match: {lanes: \"2\"}, speed_kph: 50}",
    );

    let error = compile_profile_bundle(
        &profile,
        &topology(&[]),
        CacheBundleId::new("topology-test"),
    )
    .expect_err("unsupported matcher should fail");
    assert!(error.to_string().contains("'lanes'"), "{error}");
}

/// Three 1 km car edges through NL (Groningen), NL (Drenthe) and DE nodes:
/// edge 0 stays in NL, edge 1 crosses into Germany, edge 2 runs from
/// Germany to a node outside every area.
fn border_topology() -> TopologyBundle {
    with_admin_areas(
        topology(&[
            edge(0, RoadClass::Primary, AccessMask::CAR),
            edge(1, RoadClass::Primary, AccessMask::CAR),
            edge(2, RoadClass::Primary, AccessMask::CAR),
        ]),
        &[
            ("NL", Some("NL-GR")),
            ("NL", Some("NL-DR")),
            ("DE", None),
            ("", None),
        ],
    )
}

#[test]
fn country_and_state_keys_select_regional_speeds_and_exclusions() {
    let topology = border_topology();
    let profile = profile(
        "car",
        "speed_rules:\n  - {match: {country: de, highway: primary}, speed_kph: 100}\n  - {match: {state: NL-DR}, speed_kph: 60}\nexclude_rules:\n  - match: {state: NL-GR}",
    );
    let compiled = compile(&profile, &topology);
    let time = |compiled: &CompiledProfileBundle, edge: usize| {
        compiled.edge_metrics[edge]
            .travel_time_s
            .map(|time| (time * 1e6).round() / 1e6)
    };
    // Edge 0 starts in Groningen (excluded); edge 1 in Drenthe (60 km/h);
    // edge 2 in Germany (100 km/h).
    assert_eq!(time(&compiled, 0), None);
    assert_eq!(time(&compiled, 1), Some(60.0));
    assert_eq!(time(&compiled, 2), Some(36.0));

    // Without admin areas the keys match nothing rather than failing.
    let compiled = compile(
        &profile,
        &crate::test_support::topology(&[edge(0, RoadClass::Primary, AccessMask::CAR)]),
    );
    assert_eq!(
        compiled.edge_metrics[0].travel_time_s,
        Some(1_000.0 / (70.0 / 3.6))
    );
}

#[test]
fn border_crossings_add_time_and_penalty_or_are_avoided() {
    let topology = border_topology();
    let base = compile(&profile("car", "preferences: {}"), &topology);
    let crossing = |profile: &ProfileDocument| compile(profile, &topology).edge_metrics;
    let penalized = crossing(&profile(
        "car",
        "preferences: {border_crossing_time_s: 600, border_crossing_penalty_s: 900}",
    ));
    for edge in [0, 2] {
        assert_eq!(
            penalized[edge].generalized_cost,
            base.edge_metrics[edge].generalized_cost
        );
    }
    let base_time = base.edge_metrics[1].travel_time_s.unwrap();
    assert_eq!(penalized[1].travel_time_s, Some(base_time + 600.0));
    let base_cost = base.edge_metrics[1].generalized_cost.unwrap();
    let cost = penalized[1].generalized_cost.unwrap();
    assert!(
        cost > base_cost + 600.0 + 900.0 - 1e-6,
        "{cost} vs {base_cost}"
    );

    let avoided = crossing(&profile(
        "car",
        "preferences: {avoid_border_crossings: true}",
    ));
    assert_eq!(avoided[1].travel_time_s, None);
    assert!(avoided[0].travel_time_s.is_some() && avoided[2].travel_time_s.is_some());

    let error = crate::test_support::profile("car", "preferences: {border_crossing_time_s: -1}")
        .validate()
        .expect_err("negative border time is rejected");
    assert!(
        error.to_string().contains("border_crossing_time_s"),
        "{error}"
    );
}

#[test]
fn excludes_matching_edges_from_compiled_metrics() {
    let profile = profile("foot", "exclude_rules:\n  - match: {highway: primary}");
    let topology = topology(&[
        DirectedEdge {
            length_m: 100,
            ..edge(0, RoadClass::Primary, AccessMask::FOOT)
        },
        DirectedEdge {
            length_m: 100,
            ..edge(1, RoadClass::Residential, AccessMask::FOOT)
        },
    ]);

    let compiled = compile(&profile, &topology);

    assert_eq!(compiled.edge_metrics[0].travel_time_s, None);
    assert_eq!(compiled.edge_metrics[0].generalized_cost, None);
    assert!(compiled.edge_metrics[1].travel_time_s.is_some());
}

#[test]
fn uses_tagged_ferry_duration_when_available() {
    let compiled = compile(&ferry_profile(true), &ferry_topology(Some(900.0)));

    // 900 s tagged crossing + 300 s boarding.
    assert_eq!(compiled.edge_metrics.len(), 1);
    assert_eq!(compiled.edge_metrics[0].travel_time_s, Some(1_200.0));
    assert_eq!(compiled.edge_metrics[0].generalized_cost, Some(1_560.0));
}

#[test]
fn rejects_ferry_without_duration_when_inference_is_disabled() {
    let compiled = compile(&ferry_profile(false), &ferry_topology(None));

    assert_eq!(compiled.edge_metrics.len(), 1);
    assert_eq!(compiled.edge_metrics[0].travel_time_s, None);
    assert_eq!(compiled.edge_metrics[0].generalized_cost, None);
}

#[test]
fn stores_turn_costs_in_compiled_profile_bundle() {
    let profile = profile(
        "car",
        "cost: {time_weight: 1.5}\n\
         turns: {left_penalty_s: 7, right_penalty_s: 3, uturn_penalty_s: 25, \
         traffic_signal_penalty_s: 4, roundabout_entry_penalty_s: 2}",
    );

    let turn_costs = compile(&profile, &ferry_topology(Some(900.0))).turn_costs;

    assert_eq!(turn_costs.left_penalty_s, 7.0);
    assert_eq!(turn_costs.right_penalty_s, 3.0);
    assert_eq!(turn_costs.uturn_penalty_s, 25.0);
    assert_eq!(turn_costs.traffic_signal_penalty_s, 4.0);
    assert_eq!(turn_costs.roundabout_entry_penalty_s, 2.0);
    assert_eq!(turn_costs.cost_time_weight, 1.5);
}

#[test]
fn compiles_acceleration_weights_from_dataset_bundle() {
    let profile = ferry_profile(true);
    // Two chained car edges with the single transition 0 -> 1, ranked so
    // that it becomes the only (upward) CCH arc.
    let mut topology = topology(&[
        DirectedEdge {
            smoothness: SmoothnessClass::Good,
            ..edge(0, RoadClass::Residential, AccessMask::CAR)
        },
        DirectedEdge {
            smoothness: SmoothnessClass::Good,
            ..edge(1, RoadClass::Residential, AccessMask::CAR)
        },
    ]);
    topology.nodes = nodes(&[(0.0, 0.0, 0.0), (1.0, 0.0, 0.0), (2.0, 0.0, 0.0)]);
    topology.edge_based_topology = EdgeBasedTopology {
        node_first_out: vec![0, 1, 2, 2],
        node_edge_order: vec![0, 1],
        edge_transition_first_out: vec![0, 1, 1],
        edge_transition_edges: vec![1],
    };
    let acceleration = DatasetAccelerationBundle {
        schema_version: netweevil_core::ACCELERATION_BUNDLE_SCHEMA_VERSION,
        source_topology_bundle_id: CacheBundleId::new("topology-test"),
        algorithm: netweevil_core::CCH_ALGORITHM.to_string(),
        stats: Default::default(),
        edge_order: vec![0, 1],
        edge_rank: vec![0, 1],
        upward_first_out: vec![0, 1, 1],
        upward_head: vec![1],
        downward_first_out: vec![0, 0, 0],
        downward_head: vec![],
    };
    let compile_with_acceleration = |topology: &TopologyBundle| {
        compile_profile_bundle_with_acceleration(
            &profile,
            topology,
            CacheBundleId::new("topology-test"),
            Some((&acceleration, CacheBundleId::new("accel-test"))),
        )
        .expect("compile succeeds")
    };

    let compiled = compile_with_acceleration(&topology)
        .acceleration
        .expect("acceleration compiled");
    assert_eq!(compiled.source_acceleration_bundle_id.0, "accel-test");
    // Every metric's weight set aligns with the same single upward arc.
    for upward in [
        &compiled.upward_weight,
        &compiled.time_upward_weight,
        &compiled.distance_upward_weight,
    ] {
        assert_eq!(upward.len(), 1);
        assert!(upward[0] < CCH_WEIGHT_OVERFLOW);
    }
    assert!(compiled.downward_weight.is_empty());
    assert!(compiled.time_downward_weight.is_empty());
    assert!(compiled.distance_downward_weight.is_empty());

    // Storage compiles skip customization; load-time customization of the
    // stored metrics reproduces the compile-time weights exactly.
    let stored = super::compile_profile_metrics_with_progress(
        &profile,
        &topology,
        CacheBundleId::new("topology-test"),
        Some((&acceleration, CacheBundleId::new("accel-test"))),
        |_| {},
    )
    .expect("metric compile succeeds");
    let header = stored.acceleration.as_ref().expect("acceleration header");
    assert!(header.upward_weight.is_empty() && header.time_upward_weight.is_empty());
    let customized = super::customize_acceleration(
        &topology,
        &stored,
        &acceleration,
        header.source_acceleration_bundle_id.clone(),
    );
    assert_eq!(customized.upward_weight, compiled.upward_weight);
    // Service-area metrics are customized on demand, not at load.
    assert!(customized.time_upward_weight.is_empty());
    for (metric, expected) in [
        (
            super::ServiceAreaMetric::TravelTime,
            &compiled.time_upward_weight,
        ),
        (
            super::ServiceAreaMetric::Distance,
            &compiled.distance_upward_weight,
        ),
    ] {
        let (upward, downward) =
            super::customize_service_area_metric(&topology, &stored, &acceleration, metric)
                .expect("metric customizes");
        assert_eq!(&upward, expected);
        assert!(downward.is_empty());
    }

    // A temporal-only direction keeps exact metrics but is closed to the
    // static hierarchy.
    topology.edge_layers.routing[1].flags =
        netweevil_core::EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION;
    let compiled = compile_with_acceleration(&topology);
    assert!(compiled.edge_metrics[1].generalized_cost.is_some());
    let compiled = compiled.acceleration.expect("acceleration compiled");
    assert_eq!(compiled.upward_weight[0], CCH_WEIGHT_INFINITY);
    assert_eq!(compiled.time_upward_weight[0], CCH_WEIGHT_INFINITY);
    assert_eq!(compiled.distance_upward_weight[0], CCH_WEIGHT_INFINITY);
}
