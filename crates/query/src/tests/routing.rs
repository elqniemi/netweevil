use crate::{
    AlternativeRouteOptions, AnalysisKind, EngineMode, OdPair, OdPairsDocument, PointSetDocument,
    PreparedRoutingEngine, RouteRequest, SnapOptions, build_routing_graph, execute_matrix,
    execute_od, execute_route, execute_route_with_edge_names, load_experiment, load_od_pairs,
    load_point_set,
};
use netweevil_core::{
    CacheBundleId, CompiledCostComponent, CompiledEdgeMetric, CompiledProfileBundle,
    CompiledTurnCostConfig, EdgeId, TravelMode,
};
use netweevil_profile::{BreakdownMetric, ReturnConfig, ReturnGeometry};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use super::fixtures::*;
use super::fixtures_disconnected::*;

#[test]
fn static_component_search_errors_instead_of_pruning_a_nondominated_label() {
    let (topology, metrics) = component_label_overflow_fixture();
    let request = component_label_overflow_request(None);
    let error = execute_route(&topology, &metrics, &request)
        .expect_err("an exact static frontier must not be truncated at the label guard");
    assert!(
        error
            .to_string()
            .contains("component search exceeded max_labels_per_state=1")
    );
}

#[test]
fn temporal_component_search_errors_instead_of_pruning_a_nondominated_label() {
    let (topology, metrics) = component_label_overflow_fixture();
    let request = component_label_overflow_request(Some("2026-07-10T10:00:00Z"));
    let error = execute_route(&topology, &metrics, &request)
        .expect_err("an exact temporal frontier must not be truncated at the label guard");
    assert!(
        error
            .to_string()
            .contains("temporal component search exceeded max_labels_per_state=1")
    );
}

#[test]
fn temporal_reachability_prepass_returns_unreachable_before_tradeoff_overflow() {
    let (mut topology, mut metrics) = component_label_overflow_fixture();
    topology.temporal_rule_sets = vec![netweevil_core::TemporalRuleSet {
        rules: vec![netweevil_core::TemporalRule {
            day_mask: netweevil_core::EVERY_DAY,
            intervals: Vec::new(),
            effect: netweevil_core::TemporalEffect::Closed,
        }],
    }];
    topology.edges[5].temporal_rule_id = Some(0);
    for (index, (travel_time_s, generalized_cost)) in [
        (10.0, 1.0),
        (10.0, 1.0),
        (1.0, 10.0),
        (1.0, 10.0),
        (1.0, 1.0),
        (1.0, 1.0),
    ]
    .into_iter()
    .enumerate()
    {
        metrics.edge_metrics[index].travel_time_s = Some(travel_time_s);
        metrics.edge_metrics[index].generalized_cost = Some(generalized_cost);
    }
    let mut request = component_label_overflow_request(Some("2026-07-10T10:00:00Z"));
    request.temporal.pareto = None;
    request.temporal.max_labels_per_state = 1;

    let error = execute_route(&topology, &metrics, &request)
        .expect_err("the permanently closed target edge is temporally unreachable");
    assert_eq!(
        crate::analysis_failure(&error).map(|failure| failure.outcome),
        Some(crate::AnalysisOutcome::Unreachable)
    );
    assert!(!error.to_string().contains("max_labels_per_state"));
}

#[test]
fn enforces_component_budget_and_returns_pareto_frontier() {
    let topology = test_topology();
    let mut metrics = test_metrics();
    metrics.components = vec![CompiledCostComponent {
        name: "uncovered_time".to_string(),
        weight: 0.0,
        edge_values: vec![100.0, 100.0, 0.0],
        scales_with_travel_time: false,
        overlay_name: None,
        invert_overlay: false,
    }];
    let base_request = RouteRequest {
        route_id: "covered-budget".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 20.0,
            ..Default::default()
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..Default::default()
        },
        alternatives: Default::default(),
        temporal: crate::TemporalRequestOptions {
            constraints: vec![crate::ComponentConstraint {
                component: "uncovered_time".to_string(),
                max_value: 0.0,
            }],
            ..Default::default()
        },
    };
    let covered = execute_route(&topology, &metrics, &base_request)
        .expect("covered component budget has a route");
    assert_eq!(covered.edge_path, vec![2]);
    assert_eq!(covered.summary.components["uncovered_time"], 0.0);

    let prepared =
        PreparedRoutingEngine::new(Arc::new(topology.clone()), Arc::new(metrics.clone()), None)
            .expect("prepared component routing engine");
    let origin_candidates = prepared
        .snap_route_candidates_with_options(&base_request.origin, &base_request.snap, true)
        .expect("origin candidates");
    let destination_candidates = prepared
        .snap_route_candidates_with_options(&base_request.destination, &base_request.snap, false)
        .expect("destination candidates");
    let covered_from_candidates = prepared
        .execute_route_between_candidates(
            &base_request,
            &origin_candidates,
            &destination_candidates,
        )
        .expect("candidate API preserves component constraints");
    assert_eq!(covered_from_candidates.edge_path, vec![2]);
    assert_eq!(
        prepared
            .effective_route_engine_description(&base_request, EngineMode::Auto)
            .route_engine,
        "component_nondominated_label_setting"
    );

    let pareto_request = RouteRequest {
        route_id: "covered-pareto".to_string(),
        temporal: crate::TemporalRequestOptions {
            pareto: Some(crate::ParetoRouteOptions {
                component: "uncovered_time".to_string(),
                max_routes: 4,
                max_labels_per_state: 16,
            }),
            ..Default::default()
        },
        ..base_request
    };
    let frontier =
        execute_route(&topology, &metrics, &pareto_request).expect("Pareto frontier has routes");
    assert_eq!(frontier.edge_path, vec![0, 1]);
    assert_eq!(frontier.alternatives.len(), 1);
    assert_eq!(frontier.alternatives[0].edge_path, vec![2]);
}

fn component_label_overflow_fixture() -> (
    netweevil_core::TopologyBundle,
    netweevil_core::CompiledProfileBundle,
) {
    let mut topology = test_topology();
    let node_template = topology.nodes[0].clone();
    let coordinates = [
        (6.0, 53.0),
        (6.001, 53.0),
        (6.0, 53.001),
        (6.001, 53.001),
        (6.002, 53.001),
        (6.003, 53.001),
    ];
    topology.nodes = coordinates
        .into_iter()
        .enumerate()
        .map(|(index, (lon, lat))| netweevil_core::TopologyNode {
            node_id: netweevil_core::NodeId(index as u32),
            lon,
            lat,
            ..node_template
        })
        .collect();
    let edge_template = topology.edges[0];
    let edge = |edge_id: u32, from: u32, to: u32| netweevil_core::DirectedEdge {
        edge_id: EdgeId(edge_id),
        from: netweevil_core::NodeId(from),
        to: netweevil_core::NodeId(to),
        source_way_id: i64::from(edge_id) + 100,
        length_m: 10,
        ..edge_template
    };
    topology.edges = vec![
        edge(0, 0, 1),
        edge(1, 1, 3),
        edge(2, 0, 2),
        edge(3, 2, 3),
        edge(4, 3, 4),
        edge(5, 4, 5),
    ];
    topology.edge_based_topology = crate::build_edge_based_topology_fallback(&topology);
    topology.spatial_index = None;
    topology.node_component_ids = vec![0; topology.nodes.len()];
    topology.edge_component_ids = vec![0; topology.edge_count()];

    let mut metrics = test_metrics();
    metrics.edge_metrics = [1.0, 1.0, 10.0, 1.0, 1.0, 1.0]
        .into_iter()
        .enumerate()
        .map(|(index, cost)| CompiledEdgeMetric {
            edge_id: EdgeId(index as u32),
            travel_time_s: Some(cost),
            generalized_cost: Some(cost),
        })
        .collect();
    metrics.components = vec![CompiledCostComponent {
        name: "exposure".to_string(),
        weight: 0.0,
        edge_values: vec![10.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        scales_with_travel_time: false,
        overlay_name: None,
        invert_overlay: false,
    }];
    (topology, metrics)
}

fn component_label_overflow_request(departure_time: Option<&str>) -> RouteRequest {
    RouteRequest {
        route_id: "component-label-overflow".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.003,
            lat: 53.001,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 20.0,
            ..Default::default()
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..Default::default()
        },
        alternatives: Default::default(),
        temporal: crate::TemporalRequestOptions {
            departure_time: departure_time.map(str::to_string),
            pareto: Some(crate::ParetoRouteOptions {
                component: "exposure".to_string(),
                max_routes: 4,
                max_labels_per_state: 1,
            }),
            ..Default::default()
        },
    }
}

#[test]
fn enforces_temporal_exposure_budget_and_returns_dynamic_pareto_frontier() {
    let topology = test_topology();
    let mut metrics = test_metrics();
    metrics.components = vec![CompiledCostComponent {
        name: "exposure".to_string(),
        weight: 0.0,
        edge_values: vec![10.0, 20.0, 100.0],
        scales_with_travel_time: false,
        overlay_name: Some("exposure_factor".to_string()),
        invert_overlay: false,
    }];
    let overlay_path = write_temp_file(
        "temporal-exposure.csv",
        "feature_id,t_start,t_end,exposure_factor\n10,2026-07-10T00:00:00Z,2026-07-11T00:00:00Z,1\n11,2026-07-10T00:00:00Z,2026-07-11T00:00:00Z,0\n",
    );
    let scenario_path = write_temp_file("temporal-exposure-scenario.yml", "id: exposure_test\n");
    let holiday_path = write_temp_file("temporal-exposure-holidays.yml", "- 2026-07-10\n");
    let request = RouteRequest {
        route_id: "temporal-exposure-budget".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 20.0,
            ..Default::default()
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            segment_rows: true,
            ..Default::default()
        },
        alternatives: Default::default(),
        temporal: crate::TemporalRequestOptions {
            departure_time: Some("2026-07-10T10:00:00Z".to_string()),
            scenario: Some(scenario_path.clone()),
            holiday_calendar: Some(holiday_path.clone()),
            overlays: vec![overlay_path.clone()],
            constraints: vec![crate::ComponentConstraint {
                component: "exposure".to_string(),
                max_value: 0.0,
            }],
            ..Default::default()
        },
    };

    let constrained = execute_route(&topology, &metrics, &request)
        .expect("temporal exposure budget has an unexposed route");
    assert_eq!(constrained.edge_path, vec![2]);
    assert_eq!(constrained.summary.components["exposure"], 0.0);
    assert_eq!(
        constrained.summary.scenario_id.as_deref(),
        Some("exposure_test")
    );
    assert!(constrained.summary.departure_time.is_some());
    assert!(constrained.segments.as_ref().is_some_and(|segments| {
        segments
            .iter()
            .all(|segment| segment.entry_time.is_some() && segment.exit_time.is_some())
    }));

    let pareto_request = RouteRequest {
        route_id: "temporal-exposure-pareto".to_string(),
        temporal: crate::TemporalRequestOptions {
            departure_time: Some("2026-07-10T10:00:00Z".to_string()),
            scenario: Some(scenario_path.clone()),
            holiday_calendar: Some(holiday_path.clone()),
            overlays: vec![overlay_path.clone()],
            pareto: Some(crate::ParetoRouteOptions {
                component: "exposure".to_string(),
                max_routes: 4,
                max_labels_per_state: 16,
            }),
            ..Default::default()
        },
        ..request
    };
    let frontier = execute_route(&topology, &metrics, &pareto_request)
        .expect("temporal exposure Pareto frontier");
    assert_eq!(frontier.edge_path, vec![0, 1]);
    assert_eq!(frontier.summary.components["exposure"], 30.0);
    assert_eq!(frontier.alternatives.len(), 1);
    assert_eq!(frontier.alternatives[0].edge_path, vec![2]);
    assert_eq!(frontier.alternatives[0].summary.components["exposure"], 0.0);

    let _ = fs::remove_file(overlay_path);
    let _ = fs::remove_file(scenario_path);
    let _ = fs::remove_file(holiday_path);
}

#[test]
fn temporal_component_arrive_by_finds_narrow_later_window_without_waiting() {
    let mut topology = test_topology();
    topology.temporal_rule_sets = vec![netweevil_core::TemporalRuleSet {
        rules: vec![
            netweevil_core::TemporalRule {
                day_mask: netweevil_core::EVERY_DAY,
                intervals: vec![netweevil_core::MinuteInterval {
                    start_minute: 9 * 60,
                    end_minute: 10 * 60,
                }],
                effect: netweevil_core::TemporalEffect::OpenOnly,
            },
            netweevil_core::TemporalRule {
                day_mask: netweevil_core::EVERY_DAY,
                intervals: vec![netweevil_core::MinuteInterval {
                    start_minute: 11 * 60,
                    end_minute: 11 * 60 + 1,
                }],
                effect: netweevil_core::TemporalEffect::OpenOnly,
            },
        ],
    }];
    for edge in &mut topology.edges {
        edge.temporal_rule_id = Some(0);
    }
    let mut metrics = test_metrics();
    metrics.components = vec![CompiledCostComponent {
        name: "exposure".to_string(),
        weight: 0.0,
        edge_values: vec![0.0, 0.0, 10.0],
        scales_with_travel_time: false,
        overlay_name: None,
        invert_overlay: false,
    }];
    metrics.temporal.allow_wait = false;
    let request = RouteRequest {
        route_id: "late-opening-component-arrive-by".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 20.0,
            ..Default::default()
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..Default::default()
        },
        alternatives: Default::default(),
        temporal: crate::TemporalRequestOptions {
            arrive_by: Some("2026-07-10T12:00:00Z".to_string()),
            arrive_by_lookback_s: 4.0 * 3_600.0,
            constraints: vec![crate::ComponentConstraint {
                component: "exposure".to_string(),
                max_value: 0.0,
            }],
            pareto: Some(crate::ParetoRouteOptions {
                component: "exposure".to_string(),
                max_routes: 4,
                max_labels_per_state: 16,
            }),
            ..Default::default()
        },
    };

    let result = execute_route(&topology, &metrics, &request)
        .expect("late opening constrained route arrives by deadline");
    assert_eq!(result.edge_path, vec![0, 1]);
    assert_eq!(result.summary.components["exposure"], 0.0);
    assert!(
        result
            .summary
            .departure_time
            .as_deref()
            .is_some_and(|value| {
                ("2026-07-10T11:00:00Z".."2026-07-10T11:01:00Z").contains(&value)
            })
    );
    assert!(
        result
            .summary
            .arrival_time
            .as_deref()
            .is_some_and(|value| value <= "2026-07-10T12:00:00Z")
    );
}

#[test]
fn temporal_component_arrive_by_pulls_overlay_boundary_through_path_prefix() {
    let topology = test_topology();
    let mut metrics = test_metrics();
    metrics.edge_metrics[2].travel_time_s = None;
    metrics.edge_metrics[2].generalized_cost = None;
    metrics.components = vec![CompiledCostComponent {
        name: "exposure".to_string(),
        weight: 0.0,
        edge_values: vec![10.0, 10.0, 0.0],
        scales_with_travel_time: false,
        overlay_name: Some("exposure_factor".to_string()),
        invert_overlay: false,
    }];
    let overlay_path = write_temp_file(
        "arrive-by-overlay-boundary.csv",
        "feature_id,t_start,t_end,exposure_factor\n10,2026-07-10T08:00:00Z,2026-07-10T11:00:00Z,0\n10,2026-07-10T11:00:00Z,2026-07-10T13:00:00Z,1\n",
    );
    let request = RouteRequest {
        route_id: "overlay-boundary-arrive-by".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 20.0,
            ..Default::default()
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..Default::default()
        },
        alternatives: Default::default(),
        temporal: crate::TemporalRequestOptions {
            arrive_by: Some("2026-07-10T12:00:00Z".to_string()),
            arrive_by_lookback_s: 4.0 * 3_600.0,
            overlays: vec![overlay_path.clone()],
            constraints: vec![crate::ComponentConstraint {
                component: "exposure".to_string(),
                max_value: 0.0,
            }],
            pareto: Some(crate::ParetoRouteOptions {
                component: "exposure".to_string(),
                max_routes: 4,
                max_labels_per_state: 16,
            }),
            ..Default::default()
        },
    };

    let result = execute_route(&topology, &metrics, &request)
        .expect("the last pre-overlay departure remains discoverable");
    assert_eq!(result.edge_path, vec![0, 1]);
    assert_eq!(result.summary.components["exposure"], 0.0);
    let departure = crate::parse_datetime(result.summary.departure_time.as_deref().unwrap())
        .expect("departure timestamp parses");
    assert!(departure >= crate::parse_datetime("2026-07-10T10:59:49Z").unwrap());
    assert!(departure < crate::parse_datetime("2026-07-10T10:59:50Z").unwrap());

    let _ = fs::remove_file(overlay_path);
}

#[test]
fn temporal_component_arrive_by_maps_overlay_boundary_across_edge_waiting() {
    let mut topology = test_topology();
    topology.temporal_rule_sets = vec![netweevil_core::TemporalRuleSet {
        rules: vec![
            netweevil_core::TemporalRule {
                day_mask: netweevil_core::EVERY_DAY,
                intervals: vec![netweevil_core::MinuteInterval {
                    start_minute: 8 * 60,
                    end_minute: 10 * 60 + 55,
                }],
                effect: netweevil_core::TemporalEffect::OpenOnly,
            },
            netweevil_core::TemporalRule {
                day_mask: netweevil_core::EVERY_DAY,
                intervals: vec![netweevil_core::MinuteInterval {
                    start_minute: 11 * 60,
                    end_minute: 13 * 60,
                }],
                effect: netweevil_core::TemporalEffect::OpenOnly,
            },
        ],
    }];
    topology.edges[1].temporal_rule_id = Some(0);
    let mut metrics = test_metrics();
    metrics.edge_metrics[2].travel_time_s = None;
    metrics.edge_metrics[2].generalized_cost = None;
    metrics.temporal.allow_wait = true;
    metrics.temporal.max_wait_s = 10.0 * 60.0;
    metrics.components = vec![CompiledCostComponent {
        name: "exposure".to_string(),
        weight: 0.0,
        edge_values: vec![0.0, 10.0, 0.0],
        scales_with_travel_time: false,
        overlay_name: Some("exposure_factor".to_string()),
        invert_overlay: false,
    }];
    let overlay_path = write_temp_file(
        "arrive-by-overlay-waiting.csv",
        "feature_id,t_start,t_end,exposure_factor\n10,2026-07-10T08:00:00Z,2026-07-10T11:00:00Z,0\n10,2026-07-10T11:00:00Z,2026-07-10T13:00:00Z,1\n",
    );
    let request = RouteRequest {
        route_id: "overlay-waiting-arrive-by".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 20.0,
            ..Default::default()
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..Default::default()
        },
        alternatives: Default::default(),
        temporal: crate::TemporalRequestOptions {
            arrive_by: Some("2026-07-10T12:00:00Z".to_string()),
            arrive_by_lookback_s: 4.0 * 3_600.0,
            overlays: vec![overlay_path.clone()],
            constraints: vec![crate::ComponentConstraint {
                component: "exposure".to_string(),
                max_value: 0.0,
            }],
            ..Default::default()
        },
    };

    let result = execute_route(&topology, &metrics, &request)
        .expect("the last non-waiting pre-overlay departure remains discoverable");
    assert_eq!(result.edge_path, vec![0, 1]);
    assert_eq!(result.summary.components["exposure"], 0.0);
    assert_eq!(result.summary.waiting_time_s, 0.0);
    let departure = crate::parse_datetime(result.summary.departure_time.as_deref().unwrap())
        .expect("departure timestamp parses");
    assert!(departure >= crate::parse_datetime("2026-07-10T10:54:49Z").unwrap());
    assert!(departure < crate::parse_datetime("2026-07-10T10:54:50Z").unwrap());

    let _ = fs::remove_file(overlay_path);
}

#[test]
fn executes_scenario_batch_and_diffs_rerouting_burden() {
    let mut topology = test_topology();
    topology.edges[0].feature_row = 0;
    topology.edges[1].feature_row = 0;
    topology.edges[2].feature_row = 1;
    topology.feature_attributes = netweevil_core::FeatureAttributeTable {
        row_count: 2,
        strings: vec!["mall".to_string(), "street".to_string()],
        columns: vec![netweevil_core::FeatureAttributeColumn {
            definition: netweevil_core::FeatureAttributeDefinition {
                name: "AssetGroup".to_string(),
                semantic_role: Some("asset_group".to_string()),
                value_type: netweevil_core::FeatureAttributeType::String,
                domain: Default::default(),
            },
            data: netweevil_core::FeatureAttributeColumnData::String(vec![Some(0), Some(1)]),
        }],
    };
    let metrics = test_metrics();
    let engine = PreparedRoutingEngine::new(Arc::new(topology), Arc::new(metrics), None)
        .expect("prepared scenario routing engine");
    let scenario_path = write_temp_file(
        "scenario-batch-closure.yml",
        r#"id: close_short_path
features:
  - source_feature_id: 10
    force_closed: true
"#,
    );
    let ranking_path = write_temp_file(
        "scenario-batch-ranking.csv",
        "source_feature_id,score\n10,9\n11,1\n",
    );
    let origin = crate::LabeledPoint {
        id: "a".to_string(),
        lon: 6.0,
        lat: 53.0,
        z: None,
    };
    let destination = crate::LabeledPoint {
        id: "c".to_string(),
        lon: 6.002,
        lat: 53.0,
        z: None,
    };
    let snap = SnapOptions {
        max_distance_m: 20.0,
        ..Default::default()
    };
    let request = crate::ScenarioBatchRequest {
        batch_id: "reroute".to_string(),
        departure_time: Some("2026-07-10T10:00:00Z".to_string()),
        scenarios: vec![
            crate::ScenarioBatchCase {
                id: "closure".to_string(),
                selector: crate::ScenarioBatchSelector::Overlay {
                    overlay: scenario_path.clone(),
                },
            },
            crate::ScenarioBatchCase {
                id: "mall_group".to_string(),
                selector: crate::ScenarioBatchSelector::Group {
                    group: crate::ScenarioGroupSelector {
                        attribute: "asset_group".to_string(),
                        value: "mall".to_string(),
                    },
                },
            },
            crate::ScenarioBatchCase {
                id: "top_ranked".to_string(),
                selector: crate::ScenarioBatchSelector::TopK {
                    top_k: crate::ScenarioTopKSelector {
                        ranking: ranking_path.clone(),
                        count: 1,
                    },
                },
            },
        ],
        routes: vec![RouteRequest {
            route_id: "a_to_c".to_string(),
            origin: origin.clone(),
            destination: destination.clone(),
            snap: snap.clone(),
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..Default::default()
            },
            alternatives: Default::default(),
            temporal: Default::default(),
        }],
        service_areas: Vec::new(),
        accessibility: Vec::new(),
        od: vec![crate::ScenarioOdRequest {
            analysis_id: "od".to_string(),
            request: crate::OdPairsDocument {
                pairs: vec![crate::OdPair {
                    pair_id: "a_to_c".to_string(),
                    origin: origin.clone(),
                    destination: destination.clone(),
                }],
                snap: snap.clone(),
                connectivity: Default::default(),
                fallback: Default::default(),
                returns: Default::default(),
                alternatives: Default::default(),
                temporal: Default::default(),
            },
        }],
        matrices: vec![crate::ScenarioMatrixRequest {
            analysis_id: "matrix".to_string(),
            origins: crate::PointSetDocument {
                points: vec![origin.clone()],
                snap: snap.clone(),
                connectivity: Default::default(),
                fallback: Default::default(),
                returns: Default::default(),
                alternatives: Default::default(),
                temporal: Default::default(),
            },
            destinations: crate::PointSetDocument {
                points: vec![destination.clone()],
                snap: snap.clone(),
                connectivity: Default::default(),
                fallback: Default::default(),
                returns: Default::default(),
                alternatives: Default::default(),
                temporal: Default::default(),
            },
        }],
        betweenness: vec![crate::BetweennessRequest {
            analysis_id: "usage".to_string(),
            origins: vec![crate::WeightedPoint {
                id: origin.id.clone(),
                lon: origin.lon,
                lat: origin.lat,
                z: origin.z,
                weight: 2.0,
            }],
            destinations: vec![crate::WeightedPoint {
                id: destination.id.clone(),
                lon: destination.lon,
                lat: destination.lat,
                z: destination.z,
                weight: 3.0,
            }],
            snap,
            temporal: Default::default(),
            max_od_pairs: 10,
            include_zero: false,
        }],
    };

    let result = crate::execute_scenario_batch(&engine, &request).expect("scenario batch result");
    let baseline = result.baseline.routes[0]
        .result
        .as_ref()
        .expect("baseline route");
    let scenario = result.scenarios[0].analyses.routes[0]
        .result
        .as_ref()
        .expect("scenario route");
    assert_eq!(baseline.edge_path, vec![0, 1]);
    assert_eq!(scenario.edge_path, vec![2]);
    assert_eq!(
        scenario.summary.scenario_id.as_deref(),
        Some("close_short_path")
    );
    let scenario_od = result.scenarios[0].analyses.od[0]
        .result
        .as_ref()
        .expect("scenario OD");
    assert_eq!(
        scenario_od.departure_time.as_deref(),
        Some("2026-07-10T10:00:00Z")
    );
    assert_eq!(scenario_od.scenario_id.as_deref(), Some("close_short_path"));
    let scenario_matrix = result.scenarios[0].analyses.matrices[0]
        .result
        .as_ref()
        .expect("scenario matrix");
    assert_eq!(
        scenario_matrix.departure_time.as_deref(),
        Some("2026-07-10T10:00:00Z")
    );
    assert_eq!(
        scenario_matrix.scenario_id.as_deref(),
        Some("close_short_path")
    );
    assert_eq!(result.scenarios[0].diff.rerouting_burden_s, 70.0);
    assert_eq!(result.scenarios[0].diff.od_rerouting_burden_s, 70.0);
    assert_eq!(result.scenarios[0].diff.matrix_rerouting_burden_s, 70.0);
    assert_eq!(
        result.scenarios[0]
            .diff
            .betweenness_edge_score_absolute_change,
        18.0
    );
    assert_eq!(result.scenarios[0].diff.weighted_disconnected_demand, 0.0);
    assert_eq!(result.scenarios[1].closed_source_feature_ids, vec![10]);
    assert_eq!(result.scenarios[1].diff.rerouting_burden_s, 70.0);
    assert_eq!(result.scenarios[2].closed_source_feature_ids, vec![10]);
    assert_eq!(result.scenarios[2].diff.rerouting_burden_s, 70.0);
    assert_eq!(
        result.baseline.betweenness[0]
            .result
            .as_ref()
            .expect("baseline betweenness")
            .pairs
            .len(),
        1
    );
    let scenario_betweenness = result.scenarios[0].analyses.betweenness[0]
        .result
        .as_ref()
        .expect("scenario betweenness");
    assert_eq!(
        scenario_betweenness.departure_time.as_deref(),
        Some("2026-07-10T10:00:00Z")
    );
    assert_eq!(
        scenario_betweenness.scenario_id.as_deref(),
        Some("close_short_path")
    );

    let stale_ranking_path = write_temp_file(
        "scenario-batch-stale-ranking.csv",
        "source_feature_id,score\n999,9\n",
    );
    let mut stale_request = request.clone();
    stale_request.scenarios = vec![crate::ScenarioBatchCase {
        id: "stale_top_ranked".to_string(),
        selector: crate::ScenarioBatchSelector::TopK {
            top_k: crate::ScenarioTopKSelector {
                ranking: stale_ranking_path.clone(),
                count: 1,
            },
        },
    }];
    let error = crate::execute_scenario_batch(&engine, &stale_request)
        .expect_err("stale ranked feature ids must be rejected");
    assert!(
        error
            .to_string()
            .contains("not present in the active topology")
            && error.to_string().contains("999"),
        "unexpected error: {error}"
    );

    let stale_overlay_path = write_temp_file(
        "scenario-batch-stale-overlay.yml",
        r#"id: stale_overlay
features:
  - source_feature_id: 10
    force_closed: true
  - source_feature_id: 999
    force_closed: true
"#,
    );
    let mut stale_overlay_request = request.clone();
    stale_overlay_request.scenarios = vec![crate::ScenarioBatchCase {
        id: "stale_explicit_overlay".to_string(),
        selector: crate::ScenarioBatchSelector::Overlay {
            overlay: stale_overlay_path.clone(),
        },
    }];
    let error = crate::execute_scenario_batch(&engine, &stale_overlay_request)
        .expect_err("every explicit overlay feature id must exist in the active topology");
    assert!(
        error
            .to_string()
            .contains("not present in the active topology")
            && error.to_string().contains("999"),
        "unexpected error: {error}"
    );

    let missing_overlay_path = write_temp_file("scenario-batch-missing-overlay.yml", "");
    fs::remove_file(&missing_overlay_path).expect("remove missing-overlay placeholder");
    let mut missing_overlay_request = request.clone();
    missing_overlay_request.scenarios = vec![crate::ScenarioBatchCase {
        id: "missing_explicit_overlay".to_string(),
        selector: crate::ScenarioBatchSelector::Overlay {
            overlay: missing_overlay_path.clone(),
        },
    }];
    let error = crate::execute_scenario_batch(&engine, &missing_overlay_request)
        .expect_err("explicit overlays must load before baseline execution");
    assert!(
        error
            .to_string()
            .contains("validating scenario 'missing_explicit_overlay' overlay")
            && error
                .to_string()
                .contains("scenario-batch-missing-overlay.yml"),
        "unexpected error: {error}"
    );

    let _ = fs::remove_file(scenario_path);
    let _ = fs::remove_file(ranking_path);
    let _ = fs::remove_file(stale_ranking_path);
    let _ = fs::remove_file(stale_overlay_path);
}

#[test]
fn temporal_accessibility_prices_a_mid_edge_destination_at_edge_entry_time() {
    let topology = test_topology();
    let metrics = test_metrics();
    let scenario_path = write_temp_file(
        "accessibility-mid-edge-speed.yml",
        r#"id: speed_up
features:
  - source_feature_id: 10
    speed_factor: 2.0
"#,
    );
    let snap = SnapOptions {
        max_distance_m: 5.0,
        ..Default::default()
    };
    let mut request = crate::AccessibilityRequest {
        origins: PointSetDocument {
            points: vec![crate::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0,
                lat: 53.0,
                z: None,
            }],
            snap: snap.clone(),
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: Default::default(),
            alternatives: Default::default(),
            temporal: Default::default(),
        },
        categories: vec![crate::AccessibilityCategoryRequest {
            category_id: "mid_edge".to_string(),
            destinations: PointSetDocument {
                points: vec![crate::LabeledPoint {
                    id: "destination".to_string(),
                    lon: 6.0005,
                    lat: 53.0,
                    z: None,
                }],
                snap,
                connectivity: Default::default(),
                fallback: Default::default(),
                returns: Default::default(),
                alternatives: Default::default(),
                temporal: Default::default(),
            },
        }],
        thresholds_s: vec![3.0],
        max_travel_time_s: 10.0,
    };

    let static_result = crate::execute_accessibility(&topology, &metrics, &request)
        .expect("static accessibility succeeds");
    assert!((static_result.rows[0].nearest_travel_time_s.unwrap() - 5.0).abs() < 1.0e-6);
    assert_eq!(static_result.rows[0].counts_within_threshold_s["3"], 0);

    request.origins.temporal = crate::TemporalRequestOptions {
        departure_time: Some("2026-07-10T10:00:00Z".to_string()),
        scenario: Some(scenario_path.clone()),
        ..Default::default()
    };
    let temporal_result = crate::execute_accessibility(&topology, &metrics, &request)
        .expect("temporal accessibility succeeds");
    assert!((temporal_result.rows[0].nearest_travel_time_s.unwrap() - 2.5).abs() < 1.0e-6);
    assert_eq!(temporal_result.rows[0].counts_within_threshold_s["3"], 1);

    let _ = fs::remove_file(scenario_path);
}

#[test]
fn executes_exact_route_and_breakdowns() {
    let topology = test_topology();
    let metrics = CompiledProfileBundle {
        schema_version: 2,
        profile_id: "test".to_string(),
        profile_hash: "abc".to_string(),
        mode: TravelMode::Car,
        turn_costs: CompiledTurnCostConfig::default(),
        components: Vec::new(),
        temporal: Default::default(),
        source_topology_bundle_id: CacheBundleId::new("topology-test"),
        acceleration: None,
        edge_metrics: vec![
            CompiledEdgeMetric {
                edge_id: EdgeId(0),
                travel_time_s: Some(10.0),
                generalized_cost: Some(10.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(1),
                travel_time_s: Some(20.0),
                generalized_cost: Some(20.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(2),
                travel_time_s: Some(100.0),
                generalized_cost: Some(100.0),
            },
        ],
    };
    let request = RouteRequest {
        route_id: "route".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            segment_rows: true,
            road_type_breakdown: vec![BreakdownMetric::DistanceM, BreakdownMetric::TimeS],
            surface_breakdown: vec![BreakdownMetric::DistanceM],
            penalty_breakdown: false,
            explain_cost_derivation: false,
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
    assert_eq!(result.edge_path, vec![0, 1]);
    assert_eq!(result.summary.segment_count, 2);
    assert_eq!(result.summary.total_distance_m, 300);
    assert_eq!(result.summary.total_travel_time_s, 30.0);
    assert!(result.geometry.is_some());
    assert!(result.segments.is_some());
    assert_eq!(
        result
            .breakdowns
            .as_ref()
            .and_then(|value| value.road_class.get("residential"))
            .and_then(|value| value.distance_m),
        Some(300)
    );
}

#[test]
fn returns_opt_in_alternative_routes() {
    let topology = test_topology();
    let metrics = test_metrics();
    let request = RouteRequest {
        route_id: "route_alternatives".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: AlternativeRouteOptions {
            max_routes: 2,
            max_cost_ratio: 4.0,
            ..AlternativeRouteOptions::default()
        },
        temporal: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
    assert_eq!(result.edge_path, vec![0, 1]);
    assert_eq!(result.alternatives.len(), 1);
    assert_eq!(result.alternatives[0].rank, 1);
    assert_eq!(result.alternatives[0].edge_path, vec![2]);
    assert_eq!(result.alternatives[0].summary.total_generalized_cost, 100.0);
}

#[test]
fn uses_external_edge_name_bundle_for_segment_rows() {
    let mut topology = test_topology();
    topology.names.clear();
    topology.set_edge_name_index(0, Some(0));
    topology.set_edge_name_index(1, Some(1));
    let request = RouteRequest {
        route_id: "route".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::None,
            segment_rows: true,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_route_with_edge_names(
        &topology,
        &test_metrics(),
        &request,
        &["alpha".to_string(), "beta".to_string()],
    )
    .expect("route succeeds");

    let segments = result.segments.expect("segments requested");
    assert_eq!(segments[0].name.as_deref(), Some("alpha"));
    assert_eq!(segments[1].name.as_deref(), Some("beta"));
}

#[test]
fn prepared_engine_reuses_prebuilt_graph() {
    let engine =
        PreparedRoutingEngine::new(Arc::new(test_topology()), Arc::new(test_metrics()), None)
            .expect("prepared engine builds");
    let request = RouteRequest {
        route_id: "prepared-route".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let route = engine.execute_route(&request).expect("route succeeds");
    assert_eq!(route.edge_path, vec![0, 1]);
    assert_eq!(route.summary.total_distance_m, 300);
}

#[test]
fn accelerated_query_returns_an_unpacked_path() {
    let topology = test_topology();
    let (bundle, metrics) = build_test_cch(&topology, &test_metrics());
    let graph = crate::build_routing_graph_from_shared(&topology, Arc::new(metrics), Some(bundle))
        .expect("graph builds");
    assert!(graph.acceleration.is_some(), "test CCH must be active");

    let path = crate::accelerated_route_query(&topology, &graph, 0, 2)
        .expect("accelerated query succeeds")
        .expect("accelerated path exists");

    assert_eq!(path.edge_indexes, vec![0, 1]);
    assert_eq!(path.total_generalized_cost, 30.0);
}

#[test]
fn pure_summary_routes_omit_path_payloads() {
    let request = RouteRequest {
        route_id: "summary-route".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let route = execute_route(&test_topology(), &test_metrics(), &request).expect("route succeeds");
    assert!(route.node_path.is_empty());
    assert!(route.edge_path.is_empty());
    assert!(route.geometry.is_none());
    assert!(route.segments.is_none());
}

#[test]
fn routes_between_phantom_edge_snaps_with_partial_edge_costs() {
    let mut topology = test_topology();
    topology.nodes[0].z = 10.0;
    topology.nodes[1].z = 20.0;
    topology.nodes[2].z = 30.0;
    let request = RouteRequest {
        route_id: "phantom-route".to_string(),
        origin: crate::LabeledPoint {
            id: "a_mid".to_string(),
            lon: 6.0005,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "b_mid".to_string(),
            lon: 6.0015,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            segment_rows: true,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let route = execute_route(&topology, &test_metrics(), &request).expect("route succeeds");

    assert_eq!(route.edge_path, vec![0, 1]);
    assert_eq!(route.summary.total_distance_m, 150);
    assert!((route.summary.total_travel_time_s - 15.0).abs() < 1.0e-6);
    assert_eq!(route.origin.snapped_edge_id, Some(0));
    assert_eq!(route.destination.snapped_edge_id, Some(1));
    let geometry = route.geometry.expect("geometry requested");
    assert!((route.origin.snapped_z - 15.0).abs() < 1.0e-9);
    assert!((route.destination.snapped_z - 25.0).abs() < 1.0e-9);
    let first = geometry.first().copied().expect("origin geometry");
    let last = geometry.last().copied().expect("destination geometry");
    assert_eq!([first[0], first[1]], [6.0005, 53.0]);
    assert_eq!([last[0], last[1]], [6.0015, 53.0]);
    assert!((first[2] - 15.0).abs() < 1.0e-9);
    assert!((last[2] - 25.0).abs() < 1.0e-9);
    let segments = route.segments.expect("segments requested");
    assert_eq!(segments[0].length_m, 50);
    assert!((segments[0].travel_time_s - 5.0).abs() < 1.0e-6);
    assert_eq!(segments[1].length_m, 100);
    assert!((segments[1].travel_time_s - 10.0).abs() < 1.0e-6);
}

#[test]
fn rejects_snap_beyond_threshold() {
    let request = RouteRequest {
        route_id: "route".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 0.0,
            lat: 0.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 10.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let error =
        execute_route(&test_topology(), &test_metrics(), &request).expect_err("snap should fail");
    assert!(
        error
            .to_string()
            .contains("no traversable candidate node or edge within")
    );
}

#[test]
fn snap_respects_elevation_window_and_feature_attribute_filters() {
    let mut topology = test_topology();
    topology.nodes[1].lon = topology.nodes[0].lon;
    topology.nodes[1].lat = topology.nodes[0].lat;
    topology.nodes[0].z = 0.0;
    topology.nodes[1].z = 20.0;
    topology.edges[0].feature_row = 0;
    topology.edges[1].feature_row = 1;
    topology.edges[2].feature_row = 0;
    topology.feature_attributes = netweevil_core::FeatureAttributeTable {
        row_count: 2,
        strings: vec!["outdoor".to_string(), "indoor".to_string()],
        columns: vec![netweevil_core::FeatureAttributeColumn {
            definition: netweevil_core::FeatureAttributeDefinition {
                name: "Location".to_string(),
                semantic_role: Some("indoor_location".to_string()),
                value_type: netweevil_core::FeatureAttributeType::String,
                domain: Default::default(),
            },
            data: netweevil_core::FeatureAttributeColumnData::String(vec![Some(0), Some(1)]),
        }],
    };
    let metrics = test_metrics();
    let graph = build_routing_graph(&topology, &metrics).expect("graph builds");
    let point = crate::LabeledPoint {
        id: "platform".to_string(),
        lon: 6.0,
        lat: 53.0,
        z: Some(20.0),
    };
    let candidates = crate::snapping::snap_candidates_with_options(
        &topology,
        &graph,
        &point,
        &SnapOptions {
            max_distance_m: 50.0,
            z_window_m: Some(2.0),
            attribute_filters: std::collections::BTreeMap::from([(
                "indoor_location".to_string(),
                "indoor".to_string(),
            )]),
        },
        true,
    )
    .expect("filtered snap succeeds");

    assert!(!candidates.is_empty());
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.snapped_node_id == 1)
    );
    assert!(
        candidates
            .iter()
            .all(|candidate| (candidate.snapped_z - 20.0).abs() < f64::EPSILON)
    );
}

#[test]
fn accumulates_demand_weighted_edge_betweenness() {
    let engine = PreparedRoutingEngine::new(
        std::sync::Arc::new(test_topology()),
        std::sync::Arc::new(test_metrics()),
        None,
    )
    .expect("engine builds");
    let result = engine
        .execute_betweenness(&crate::BetweennessRequest {
            analysis_id: "usage".to_string(),
            origins: vec![crate::WeightedPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
                z: None,
                weight: 2.0,
            }],
            destinations: vec![crate::WeightedPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
                z: None,
                weight: 3.0,
            }],
            snap: SnapOptions::default(),
            temporal: Default::default(),
            max_od_pairs: 10,
            include_zero: false,
        })
        .expect("betweenness succeeds");

    assert_eq!(result.routed_pair_count, 1);
    assert_eq!(result.routed_demand, 6.0);
    assert!(result.pairs.is_empty());
    assert_eq!(result.edges.len(), 2);
    assert!(
        result
            .edges
            .iter()
            .all(|edge| edge.score == 6.0 && edge.normalized_score == 1.0)
    );
}

#[test]
fn betweenness_honors_static_component_constraints() {
    let topology = test_topology();
    let mut metrics = test_metrics();
    metrics.components = vec![CompiledCostComponent {
        name: "uncovered_time".to_string(),
        weight: 0.0,
        edge_values: vec![100.0, 100.0, 0.0],
        scales_with_travel_time: false,
        overlay_name: None,
        invert_overlay: false,
    }];
    let engine = PreparedRoutingEngine::new(Arc::new(topology), Arc::new(metrics), None)
        .expect("engine builds");
    let result = engine
        .execute_betweenness(&crate::BetweennessRequest {
            analysis_id: "covered_usage".to_string(),
            origins: vec![crate::WeightedPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
                z: None,
                weight: 2.0,
            }],
            destinations: vec![crate::WeightedPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
                z: None,
                weight: 3.0,
            }],
            snap: SnapOptions {
                max_distance_m: 20.0,
                ..Default::default()
            },
            temporal: crate::TemporalRequestOptions {
                constraints: vec![crate::ComponentConstraint {
                    component: "uncovered_time".to_string(),
                    max_value: 0.0,
                }],
                ..Default::default()
            },
            max_od_pairs: 10,
            include_zero: false,
        })
        .expect("constrained betweenness succeeds");

    assert_eq!(result.routed_pair_count, 1);
    assert_eq!(result.routed_demand, 6.0);
    assert!(result.pairs.is_empty());
    assert_eq!(result.edges.len(), 1);
    assert_eq!(result.edges[0].edge_id, 2);
    assert_eq!(result.edges[0].score, 6.0);
}

#[test]
fn snap_candidates_keep_only_the_nearest_eight() {
    let topology = snap_test_topology();
    let metrics = uniform_metrics(topology.edge_count(), 1.0);
    let graph = build_routing_graph(&topology, &metrics).expect("graph builds");
    let point = crate::LabeledPoint {
        id: "snap".to_string(),
        lon: 6.0,
        lat: 53.0,
        z: None,
    };

    let candidates =
        crate::snap_candidates(&topology, &graph, &point, 500.0, true).expect("snap works");

    assert_eq!(candidates.len(), 8);
    assert!(
        candidates
            .windows(2)
            .all(|window| window[0].snap_distance_m <= window[1].snap_distance_m)
    );
    assert_eq!(candidates[0].snapped_node_id, 0);
    assert_eq!(candidates[7].snapped_node_id, 7);
}

#[test]
fn executes_od_batch_with_failures() {
    let document = OdPairsDocument {
        pairs: vec![
            OdPair {
                pair_id: "ok".to_string(),
                origin: crate::LabeledPoint {
                    id: "a".to_string(),
                    lon: 6.0,
                    lat: 53.0,
                    z: None,
                },
                destination: crate::LabeledPoint {
                    id: "c".to_string(),
                    lon: 6.002,
                    lat: 53.0,
                    z: None,
                },
            },
            OdPair {
                pair_id: "bad".to_string(),
                origin: crate::LabeledPoint {
                    id: "far".to_string(),
                    lon: 0.0,
                    lat: 0.0,
                    z: None,
                },
                destination: crate::LabeledPoint {
                    id: "c".to_string(),
                    lon: 6.002,
                    lat: 53.0,
                    z: None,
                },
            },
        ],
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_od(&test_topology(), &test_metrics(), &document).expect("OD succeeds");
    assert_eq!(result.succeeded_count, 1);
    assert_eq!(result.failed_count, 1);
    assert_eq!(result.pairs[0].total_distance_m, Some(300));
    assert!(
        result.pairs[0]
            .geometry
            .as_ref()
            .is_some_and(|coords| coords.len() >= 2)
    );
    assert_eq!(result.pairs[1].status, crate::BatchItemStatus::Failed);
}

#[test]
fn executes_matrix_batch() {
    let origins = PointSetDocument {
        points: vec![
            crate::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
                z: None,
            },
            crate::LabeledPoint {
                id: "b".to_string(),
                lon: 6.001,
                lat: 53.0,
                z: None,
            },
        ],
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };
    let destinations = PointSetDocument {
        points: vec![crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        }],
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_matrix(&test_topology(), &test_metrics(), &origins, &destinations)
        .expect("matrix succeeds");
    assert_eq!(result.cell_count, 2);
    assert_eq!(result.succeeded_count, 2);
    assert_eq!(result.cells[0].total_distance_m, Some(300));
    assert_eq!(result.cells[1].total_distance_m, Some(200));
    assert!(
        result.cells[0]
            .geometry
            .as_ref()
            .is_some_and(|coords| coords.len() >= 2)
    );
}

#[test]
fn executes_matrix_batch_with_presnapped_failures() {
    let origins = PointSetDocument {
        points: vec![
            crate::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
                z: None,
            },
            crate::LabeledPoint {
                id: "far".to_string(),
                lon: 0.0,
                lat: 0.0,
                z: None,
            },
        ],
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };
    let destinations = PointSetDocument {
        points: vec![
            crate::LabeledPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
                z: None,
            },
            crate::LabeledPoint {
                id: "b".to_string(),
                lon: 6.001,
                lat: 53.0,
                z: None,
            },
        ],
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_matrix(&test_topology(), &test_metrics(), &origins, &destinations)
        .expect("matrix succeeds");

    assert_eq!(result.cell_count, 4);
    assert_eq!(result.succeeded_count, 2);
    assert_eq!(result.failed_count, 2);
    assert_eq!(result.cells[0].status, crate::BatchItemStatus::Succeeded);
    assert_eq!(result.cells[1].status, crate::BatchItemStatus::Succeeded);
    assert_eq!(result.cells[2].status, crate::BatchItemStatus::Failed);
    assert_eq!(result.cells[3].status, crate::BatchItemStatus::Failed);
    assert!(
        result.cells[2]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("no traversable candidate node or edge within"))
    );
}

#[test]
fn matrix_matches_repeated_exact_route_execution() {
    let topology = test_topology();
    let metrics = test_metrics();
    let origins = PointSetDocument {
        points: vec![
            crate::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
                z: None,
            },
            crate::LabeledPoint {
                id: "b".to_string(),
                lon: 6.001,
                lat: 53.0,
                z: None,
            },
        ],
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };
    let destinations = PointSetDocument {
        points: vec![
            crate::LabeledPoint {
                id: "b".to_string(),
                lon: 6.001,
                lat: 53.0,
                z: None,
            },
            crate::LabeledPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
                z: None,
            },
        ],
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let matrix =
        execute_matrix(&topology, &metrics, &origins, &destinations).expect("matrix succeeds");

    for (cell, (origin, destination)) in
        matrix
            .cells
            .iter()
            .zip(origins.points.iter().flat_map(|origin| {
                destinations
                    .points
                    .iter()
                    .map(move |destination| (origin, destination))
            }))
    {
        let route = execute_route(
            &topology,
            &metrics,
            &RouteRequest {
                route_id: format!("{}__{}", origin.id, destination.id),
                origin: origin.clone(),
                destination: destination.clone(),
                snap: origins.snap.clone(),
                connectivity: origins.connectivity.clone(),
                fallback: origins.fallback.clone(),
                returns: origins.returns.clone(),
                alternatives: origins.alternatives.clone(),
                temporal: Default::default(),
            },
        )
        .expect("route succeeds");
        assert_eq!(cell.total_distance_m, Some(route.summary.total_distance_m));
        assert_eq!(
            cell.total_travel_time_s,
            Some(route.summary.total_travel_time_s)
        );
        assert_eq!(
            cell.total_generalized_cost,
            Some(route.summary.total_generalized_cost)
        );
        assert_eq!(cell.geometry, route.geometry);
    }
}

#[test]
fn accelerated_engine_matches_exact_engine_on_small_topology() {
    let topology = test_topology();
    let (bundle, accelerated_metrics) = build_test_cch(&topology, &test_metrics());
    let exact_engine =
        PreparedRoutingEngine::new(Arc::new(topology.clone()), Arc::new(test_metrics()), None)
            .expect("exact engine builds");
    let accelerated_engine = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(accelerated_metrics),
        Some(bundle),
    )
    .expect("accelerated engine builds");

    let requests = [
        RouteRequest {
            route_id: "a_to_c".to_string(),
            origin: crate::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
                z: None,
            },
            destination: crate::LabeledPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
                z: None,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
                z_window_m: None,
                attribute_filters: Default::default(),
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
            alternatives: Default::default(),
            temporal: Default::default(),
        },
        RouteRequest {
            route_id: "a_to_b".to_string(),
            origin: crate::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
                z: None,
            },
            destination: crate::LabeledPoint {
                id: "b".to_string(),
                lon: 6.001,
                lat: 53.0,
                z: None,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
                z_window_m: None,
                attribute_filters: Default::default(),
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
            alternatives: Default::default(),
            temporal: Default::default(),
        },
    ];

    for request in requests {
        let exact = exact_engine
            .execute_route(&request)
            .expect("exact route succeeds");
        let accelerated = accelerated_engine
            .execute_route(&request)
            .expect("accelerated route succeeds");
        assert_eq!(
            accelerated.summary.total_distance_m,
            exact.summary.total_distance_m
        );
        assert_eq!(
            accelerated.summary.total_travel_time_s,
            exact.summary.total_travel_time_s
        );
        assert_eq!(
            accelerated.summary.total_generalized_cost,
            exact.summary.total_generalized_cost
        );
        assert_eq!(accelerated.geometry, exact.geometry);
        assert_eq!(accelerated.edge_path, exact.edge_path);
    }
}

#[test]
fn accelerated_engine_excludes_profile_pruned_edges() {
    let topology = test_topology();
    let mut sparse_metrics = test_metrics();
    sparse_metrics.edge_metrics[2].travel_time_s = None;
    sparse_metrics.edge_metrics[2].generalized_cost = None;
    let (bundle, accelerated_metrics) = build_test_cch(&topology, &sparse_metrics);
    let engine = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(accelerated_metrics),
        Some(bundle),
    )
    .expect("accelerated engine builds");
    let request = RouteRequest {
        route_id: "a_to_c_sparse_acceleration".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let route = engine
        .execute_route(&request)
        .expect("accelerated route succeeds");
    assert_eq!(route.summary.total_distance_m, 300);
    assert_eq!(route.summary.total_travel_time_s, 30.0);
}

#[test]
fn accelerated_engine_matches_exact_engine_under_pairwise_restrictions() {
    let topology = restricted_topology();
    let metrics = restricted_metrics();
    let (bundle, accelerated_metrics) = build_test_cch(&topology, &metrics);
    let exact_engine =
        PreparedRoutingEngine::new(Arc::new(topology.clone()), Arc::new(metrics), None)
            .expect("exact engine builds");
    let accelerated_engine = PreparedRoutingEngine::new(
        Arc::new(topology.clone()),
        Arc::new(accelerated_metrics),
        Some(bundle),
    )
    .expect("accelerated engine builds");

    // All-pairs differential against the exact engine, including the pair
    // affected by the pairwise turn restriction.
    for origin in &topology.nodes {
        for destination in &topology.nodes {
            if origin.node_id == destination.node_id {
                continue;
            }
            let request = RouteRequest {
                route_id: format!("diff_{}_{}", origin.node_id.0, destination.node_id.0),
                origin: crate::LabeledPoint {
                    id: format!("o{}", origin.node_id.0),
                    lon: origin.lon,
                    lat: origin.lat,
                    z: None,
                },
                destination: crate::LabeledPoint {
                    id: format!("d{}", destination.node_id.0),
                    lon: destination.lon,
                    lat: destination.lat,
                    z: None,
                },
                snap: SnapOptions {
                    max_distance_m: 500.0,
                    z_window_m: None,
                    attribute_filters: Default::default(),
                },
                connectivity: Default::default(),
                fallback: Default::default(),
                returns: ReturnConfig {
                    geometry: ReturnGeometry::Full,
                    ..ReturnConfig::default()
                },
                alternatives: Default::default(),
                temporal: Default::default(),
            };
            let exact = exact_engine.execute_route(&request);
            let accelerated = accelerated_engine.execute_route(&request);
            match (exact, accelerated) {
                (Ok(exact), Ok(accelerated)) => {
                    assert_eq!(
                        accelerated.summary.total_generalized_cost,
                        exact.summary.total_generalized_cost,
                        "cost mismatch for {}",
                        request.route_id
                    );
                    assert_eq!(
                        accelerated.edge_path, exact.edge_path,
                        "path mismatch for {}",
                        request.route_id
                    );
                }
                (Err(_), Err(_)) => {}
                (exact, accelerated) => panic!(
                    "engines disagree on feasibility for {}: exact={:?} accelerated={:?}",
                    request.route_id,
                    exact.map(|route| route.edge_path),
                    accelerated.map(|route| route.edge_path)
                ),
            }
        }
    }
}

#[test]
fn respects_turn_restrictions() {
    let topology = restricted_topology();
    let metrics = restricted_metrics();
    let request = RouteRequest {
        route_id: "restricted".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "d".to_string(),
            lon: 6.003,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
    assert_eq!(result.edge_path, vec![3, 2]);
    assert_eq!(result.summary.total_distance_m, 300);
}

#[test]
fn builds_pairwise_turn_table_without_automaton_for_two_edge_restrictions() {
    let graph =
        build_routing_graph(&restricted_topology(), &restricted_metrics()).expect("graph builds");

    assert!(!graph.has_restriction_sequences());
    assert!(!graph.has_edge_transition(1, 2));
    assert!(graph.has_edge_transition(0, 1));
}

#[test]
fn builds_routing_graph_from_persisted_edge_based_topology() {
    let mut topology = restricted_topology();
    topology.edge_based_topology = crate::build_edge_based_topology_fallback(&topology);

    let graph =
        build_routing_graph(&topology, &restricted_metrics()).expect("graph builds from bundle");

    assert!(!graph.has_restriction_sequences());
    assert!(!graph.has_edge_transition(1, 2));
    assert!(graph.has_edge_transition(0, 1));
}

#[test]
fn applies_turn_penalties_when_selecting_routes() {
    let topology = turn_penalty_topology();
    let mut metrics = turn_penalty_metrics();
    let request = RouteRequest {
        route_id: "turn-penalty".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "d".to_string(),
            lon: 6.002,
            lat: 53.001,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let without_penalty =
        execute_route(&topology, &metrics, &request).expect("route without turn penalty");
    assert_eq!(without_penalty.edge_path, vec![0, 1, 2]);
    assert_eq!(without_penalty.summary.total_travel_time_s, 15.0);

    metrics.turn_costs.left_penalty_s = 10.0;
    metrics.turn_costs.right_penalty_s = 5.0;

    let with_penalty =
        execute_route(&topology, &metrics, &request).expect("route with turn penalty");
    assert_eq!(with_penalty.edge_path, vec![3, 4]);
    assert_eq!(with_penalty.summary.total_travel_time_s, 25.0);
}

#[test]
fn applies_traffic_signal_penalties() {
    let topology = traffic_signal_penalty_topology();
    let mut metrics = turn_penalty_metrics();
    let request = RouteRequest {
        route_id: "traffic-signal-penalty".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "d".to_string(),
            lon: 6.002,
            lat: 53.001,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let without_penalty =
        execute_route(&topology, &metrics, &request).expect("route without signal penalty");
    assert_eq!(without_penalty.edge_path, vec![0, 1, 2]);
    assert_eq!(without_penalty.summary.total_travel_time_s, 15.0);

    metrics.turn_costs.traffic_signal_penalty_s = 10.0;

    let with_penalty =
        execute_route(&topology, &metrics, &request).expect("route with signal penalty");
    assert_eq!(with_penalty.edge_path, vec![3, 4]);
    assert_eq!(with_penalty.summary.total_travel_time_s, 20.0);
}

#[test]
fn applies_roundabout_entry_penalties() {
    let topology = roundabout_entry_penalty_topology();
    let mut metrics = roundabout_entry_penalty_metrics();
    let request = RouteRequest {
        route_id: "roundabout-entry-penalty".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "d".to_string(),
            lon: 6.002,
            lat: 53.001,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let without_penalty =
        execute_route(&topology, &metrics, &request).expect("route without roundabout penalty");
    assert_eq!(without_penalty.edge_path, vec![0, 1]);
    assert_eq!(without_penalty.summary.total_travel_time_s, 10.0);

    metrics.turn_costs.roundabout_entry_penalty_s = 11.0;

    let with_penalty =
        execute_route(&topology, &metrics, &request).expect("route with roundabout penalty");
    assert_eq!(with_penalty.edge_path, vec![2, 3]);
    assert_eq!(with_penalty.summary.total_travel_time_s, 12.0);
}

#[test]
fn uses_automaton_for_multi_edge_restriction_sequences() {
    let graph = build_routing_graph(&multi_edge_restricted_topology(), &restricted_metrics())
        .expect("graph builds");

    assert!(graph.has_restriction_sequences());
}

#[test]
fn respects_multi_edge_restriction_sequences() {
    let topology = multi_edge_restricted_topology();
    let metrics = restricted_metrics();
    let request = RouteRequest {
        route_id: "multi-edge-restricted".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "d".to_string(),
            lon: 6.003,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
    assert_eq!(result.edge_path, vec![3, 2]);
    assert_eq!(result.summary.total_distance_m, 300);
}

#[test]
fn alternative_routes_fall_back_to_restricted_search_when_needed() {
    let topology = multi_edge_restricted_topology();
    let metrics = restricted_metrics();
    let request = RouteRequest {
        route_id: "multi-edge-restricted-alternative".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "d".to_string(),
            lon: 6.003,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: AlternativeRouteOptions {
            max_routes: 2,
            max_cost_ratio: 2.0,
            ..AlternativeRouteOptions::default()
        },
        temporal: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");

    assert_eq!(result.edge_path, vec![3, 2]);
    assert_eq!(result.alternatives.len(), 1);
    assert_eq!(result.alternatives[0].edge_path, vec![0, 1, 4]);
}

#[test]
fn detects_when_a_path_violates_multi_edge_restriction_sequences() {
    let graph = build_routing_graph(&multi_edge_restricted_topology(), &restricted_metrics())
        .expect("graph builds");

    assert!(!crate::path_respects_restriction_sequences(
        &graph,
        &[0, 1, 2]
    ));
    assert!(crate::path_respects_restriction_sequences(&graph, &[3, 2]));
}

#[test]
fn can_ignore_multi_edge_restriction_sequences_via_engine_mode() {
    let topology = multi_edge_restricted_topology();
    let mut metrics = restricted_metrics();
    metrics.edge_metrics[3].travel_time_s = Some(25.0);
    metrics.edge_metrics[3].generalized_cost = Some(25.0);
    let engine = PreparedRoutingEngine::new(Arc::new(topology), Arc::new(metrics), None)
        .expect("prepared engine builds");
    let request = RouteRequest {
        route_id: "multi-edge-override".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "d".to_string(),
            lon: 6.003,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let exact = engine
        .execute_route_with_mode(&request, EngineMode::Auto)
        .expect("exact route succeeds");
    let override_route = engine
        .execute_route_with_mode(&request, EngineMode::IgnoreMultiEdgeRestrictions)
        .expect("override route succeeds");
    let override_engine =
        engine.effective_engine_description(EngineMode::IgnoreMultiEdgeRestrictions);

    assert_eq!(exact.edge_path, vec![3, 2]);
    assert_eq!(override_route.edge_path, vec![0, 1, 2]);
    assert_eq!(
        override_engine.route_engine,
        "bidirectional_exact_pairwise_turns"
    );
}

#[test]
fn loads_od_pairs_from_csv() {
    let path = write_temp_file(
        "od_pairs.csv",
        "id,source_x,source_y,target_x,target_y\npair_1,6.1,53.1,6.2,53.2\n",
    );

    let document = load_od_pairs(&path).expect("CSV loads");

    assert_eq!(document.pairs.len(), 1);
    assert_eq!(document.pairs[0].pair_id, "pair_1");
    assert_eq!(document.pairs[0].origin.id, "pair_1:source");
    assert_eq!(document.pairs[0].destination.id, "pair_1:target");
    assert_eq!(document.pairs[0].origin.lon, 6.1);
    assert_eq!(document.pairs[0].destination.lat, 53.2);
    fs::remove_file(path).ok();
}

#[test]
fn loads_point_set_from_csv() {
    let path = write_temp_file("points.csv", "id,x,y\na,6.1,53.1\nb,6.2,53.2\n");

    let document = load_point_set(&path).expect("CSV loads");

    assert_eq!(document.points.len(), 2);
    assert_eq!(document.points[0].id, "a");
    assert_eq!(document.points[1].lon, 6.2);
    assert_eq!(document.points[1].lat, 53.2);
    fs::remove_file(path).ok();
}

#[test]
fn loads_experiment_with_route_and_matrix_scenarios() {
    let path = write_temp_file(
        "experiment.yml",
        r#"
experiment:
  id: baseline_sweep
  label: Baseline Sweep
  dataset: groningen_2026_03
scenarios:
  - id: baseline_route
    profile: ../profiles/car_research_v1.yml
    analysis: route
    request: ../requests/route.json
  - id: baseline_matrix
    profile: ../profiles/car_research_v1.yml
    analysis: matrix
    origins: ../requests/matrix_origins.csv
    destinations: ../requests/matrix_destinations.csv
    out: exports/matrix.csv
"#,
    );

    let document = load_experiment(&path).expect("experiment loads");

    assert_eq!(document.experiment.id, "baseline_sweep");
    assert_eq!(document.scenarios.len(), 2);
    assert_eq!(document.scenarios[0].analysis, AnalysisKind::Route);
    assert_eq!(
        document.scenarios[1].origins.as_deref(),
        Some(PathBuf::from("../requests/matrix_origins.csv").as_path())
    );
    assert_eq!(
        document.scenarios[1].destinations.as_deref(),
        Some(PathBuf::from("../requests/matrix_destinations.csv").as_path())
    );
    fs::remove_file(path).ok();
}

#[test]
fn accelerated_many_to_many_matrix_matches_pairwise_routes() {
    let topology = test_topology();
    let (bundle, accelerated_metrics) = build_test_cch(&topology, &test_metrics());
    let engine = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(accelerated_metrics),
        Some(bundle),
    )
    .expect("accelerated engine builds");

    let point = |id: &str, lon: f64| crate::LabeledPoint {
        id: id.to_string(),
        lon,
        lat: 53.0,
        z: None,
    };
    // Five distinct snapped destinations per origin push the batch strategy
    // onto the shared CCH search-space path; the pairwise CCH query answers
    // the same pairs one by one for comparison.
    let origin_points = vec![point("a", 6.0), point("ab_mid", 6.0004)];
    let destination_points = vec![
        point("b", 6.001),
        point("c", 6.002),
        point("bc_low", 6.0013),
        point("bc_mid", 6.0015),
        point("bc_high", 6.0017),
    ];
    let point_set = |points: Vec<crate::LabeledPoint>| PointSetDocument {
        points,
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };
    let origins = point_set(origin_points.clone());
    let destinations = point_set(destination_points.clone());

    let matrix = engine
        .execute_matrix(&origins, &destinations)
        .expect("matrix succeeds");
    assert_eq!(
        matrix.cell_count,
        origin_points.len() * destination_points.len()
    );

    for cell in &matrix.cells {
        let origin = origin_points
            .iter()
            .find(|point| point.id == cell.origin_id)
            .expect("cell origin known");
        let destination = destination_points
            .iter()
            .find(|point| point.id == cell.destination_id)
            .expect("cell destination known");
        let request = RouteRequest {
            route_id: format!("{}-{}", cell.origin_id, cell.destination_id),
            origin: origin.clone(),
            destination: destination.clone(),
            snap: SnapOptions {
                max_distance_m: 500.0,
                z_window_m: None,
                attribute_filters: Default::default(),
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
            alternatives: Default::default(),
            temporal: Default::default(),
        };
        let pairwise = engine.execute_route(&request);
        match (&cell.status, pairwise) {
            (crate::BatchItemStatus::Succeeded, Ok(route)) => {
                let matrix_cost = cell.total_generalized_cost.expect("matrix cost");
                assert!(
                    (matrix_cost - route.summary.total_generalized_cost).abs() <= 1e-6,
                    "cell {}->{} cost {} != pairwise {}",
                    cell.origin_id,
                    cell.destination_id,
                    matrix_cost,
                    route.summary.total_generalized_cost
                );
                let matrix_time = cell.total_travel_time_s.expect("matrix time");
                assert!(
                    (matrix_time - route.summary.total_travel_time_s).abs() <= 1e-6,
                    "cell {}->{} time {} != pairwise {}",
                    cell.origin_id,
                    cell.destination_id,
                    matrix_time,
                    route.summary.total_travel_time_s
                );
            }
            (crate::BatchItemStatus::Succeeded, Err(error)) => {
                panic!(
                    "matrix cell {}->{} succeeded but pairwise route failed: {error:#}",
                    cell.origin_id, cell.destination_id
                );
            }
            (_, Ok(route)) => panic!(
                "matrix cell {}->{} did not succeed but pairwise route did (cost {})",
                cell.origin_id, cell.destination_id, route.summary.total_generalized_cost
            ),
            (_, Err(_)) => {}
        }
    }
}

#[test]
fn phast_expansion_matches_bounded_dijkstra_expansion() {
    use crate::service_area::{
        ServiceAreaMetricKind, build_service_area_expansion_with_settle_limit,
    };

    let topology = test_topology();
    let (bundle, accelerated_metrics) = build_test_cch(&topology, &test_metrics());
    let metrics = std::sync::Arc::new(accelerated_metrics);
    let routing_graph =
        crate::build_routing_graph_from_shared(&topology, metrics.clone(), Some(bundle))
            .expect("accelerated routing graph builds");
    assert!(
        routing_graph.acceleration.is_some(),
        "fixture must produce an accelerated graph"
    );

    let origin = crate::LabeledPoint {
        id: "origin".to_string(),
        lon: 6.0,
        lat: 53.0,
        z: None,
    };
    let candidates =
        crate::snapping::snap_candidates(&topology, &routing_graph, &origin, 500.0, true)
            .expect("origin snaps");

    for metric_kind in [
        ServiceAreaMetricKind::TravelTimeS,
        ServiceAreaMetricKind::DistanceM,
    ] {
        for max_cost in [40.0_f64, 150.0, 1.0e9] {
            let run = |settle_limit: usize| {
                build_service_area_expansion_with_settle_limit(
                    &topology,
                    metrics.as_ref(),
                    &routing_graph,
                    &origin,
                    &candidates,
                    500.0,
                    &Default::default(),
                    metric_kind,
                    max_cost,
                    settle_limit,
                )
                .expect("expansion succeeds")
            };
            let dijkstra = run(usize::MAX);
            let phast = run(0);

            let mut dijkstra_reached = dijkstra.reached_edges.clone();
            let mut phast_reached = phast.reached_edges.clone();
            dijkstra_reached.sort_unstable();
            phast_reached.sort_unstable();
            assert_eq!(
                dijkstra_reached, phast_reached,
                "reached sets differ for {metric_kind:?} max_cost={max_cost}"
            );
            for &edge in &dijkstra_reached {
                let edge = edge as usize;
                assert!(
                    (dijkstra.edge_end_costs[edge] - phast.edge_end_costs[edge]).abs() <= 1e-9,
                    "end cost differs on edge {edge} for {metric_kind:?} max_cost={max_cost}: {} vs {}",
                    dijkstra.edge_end_costs[edge],
                    phast.edge_end_costs[edge]
                );
                assert!(
                    (dijkstra.edge_before_costs[edge] - phast.edge_before_costs[edge]).abs()
                        <= 1e-9,
                    "before cost differs on edge {edge} for {metric_kind:?} max_cost={max_cost}: {} vs {}",
                    dijkstra.edge_before_costs[edge],
                    phast.edge_before_costs[edge]
                );
                assert!(
                    (dijkstra.edge_start_fractions[edge] - phast.edge_start_fractions[edge]).abs()
                        <= 1e-12,
                    "start fraction differs on edge {edge} for {metric_kind:?}"
                );
            }
        }
    }
}

#[test]
fn component_service_area_labels_bypass_scalar_phast() {
    use crate::service_area::{
        ServiceAreaMetricKind, build_service_area_expansion_with_settle_limit,
    };

    let topology = test_topology();
    let mut base_metrics = test_metrics();
    base_metrics.components = vec![CompiledCostComponent {
        name: "exposure".to_string(),
        weight: 0.0,
        edge_values: vec![1.0, 2.0, 3.0],
        scales_with_travel_time: false,
        overlay_name: None,
        invert_overlay: false,
    }];
    let (bundle, accelerated_metrics) = build_test_cch(&topology, &base_metrics);
    let metrics = Arc::new(accelerated_metrics);
    let routing_graph =
        crate::build_routing_graph_from_shared(&topology, metrics.clone(), Some(bundle))
            .expect("accelerated routing graph builds");
    let origin = crate::LabeledPoint {
        id: "origin".to_string(),
        lon: 6.0,
        lat: 53.0,
        z: None,
    };
    let candidates =
        crate::snapping::snap_candidates(&topology, &routing_graph, &origin, 500.0, true)
            .expect("origin snaps");

    let expansion = build_service_area_expansion_with_settle_limit(
        &topology,
        metrics.as_ref(),
        &routing_graph,
        &origin,
        &candidates,
        500.0,
        &Default::default(),
        ServiceAreaMetricKind::TravelTimeS,
        1.0e9,
        0,
    )
    .expect("component expansion succeeds");

    assert!(expansion.warnings.iter().any(|warning| {
        warning.contains("PHAST was intentionally bypassed")
            && warning.contains("component vectors")
    }));
    assert!(!expansion.reached_edges.is_empty());
}
