use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use netweevil_core::{
    AccessMask, CacheBundleId, CompiledCostComponent, CompiledProfileBundle, DirectedEdge, EdgeId,
    FeatureAttributeColumn, FeatureAttributeColumnData, FeatureAttributeDefinition,
    FeatureAttributeTable, FeatureAttributeType, MinuteInterval, NodeId, RoadClass, TemporalEffect,
    TemporalRule, TemporalRuleSet, TopologyBundle, TopologyEdgeLayers,
};
use netweevil_profile::{BreakdownMetric, ReturnConfig};

use super::fixtures::*;
use crate::service_area::{ServiceAreaMetricKind, build_service_area_expansion_with_settle_limit};
use crate::{
    AlternativeRouteOptions, AnalysisKind, ComponentConstraint, EngineMode, LabeledPoint,
    ParetoRouteOptions, PointSetDocument, PreparedRoutingEngine, RouteRequest, RoutingGraph,
    ScenarioBatchCase, ScenarioBatchSelector, TemporalRequestOptions, WeightedPoint,
    build_routing_graph, execute_matrix, execute_od, execute_route, execute_route_with_edge_names,
    load_experiment, load_od_pairs, load_point_set,
};

/// a -> c across `test_topology`; the cheapest path is edges 0, 1.
fn a_to_c(route_id: &str) -> RouteRequest {
    route_request(route_id, point("a", 6.0, 53.0), point("c", 6.002, 53.0))
}

/// Full-geometry route from node 0 to node 3 of `restricted_topology`.
fn restricted_a_to_d(route_id: &str) -> RouteRequest {
    RouteRequest {
        returns: full_geometry(),
        ..route_request(route_id, point("a", 6.0, 53.0), point("d", 6.003, 53.0))
    }
}

/// Full-geometry route from node 0 to node 3 of the turn penalty fixtures.
fn penalty_route(route_id: &str) -> RouteRequest {
    RouteRequest {
        returns: full_geometry(),
        ..route_request(route_id, point("a", 6.0, 53.0), point("d", 6.002, 53.001))
    }
}

fn zero_budget(component: &str) -> ComponentConstraint {
    ComponentConstraint {
        component: component.to_string(),
        max_value: 0.0,
    }
}

fn pareto(component: &str, max_labels_per_state: usize) -> ParetoRouteOptions {
    ParetoRouteOptions {
        component: component.to_string(),
        max_routes: 4,
        max_labels_per_state,
    }
}

fn open_only(start_minute: u16, end_minute: u16) -> TemporalRule {
    TemporalRule {
        day_mask: netweevil_core::EVERY_DAY,
        intervals: vec![MinuteInterval {
            start_minute,
            end_minute,
        }],
        effect: TemporalEffect::OpenOnly,
    }
}

fn weighted_point(id: &str, lon: f64, lat: f64, weight: f64) -> WeightedPoint {
    WeightedPoint {
        id: id.to_string(),
        lon,
        lat,
        z: None,
        weight,
    }
}

/// One string attribute column whose row `i` holds `values[i]`.
fn string_attribute_table(
    name: &str,
    semantic_role: &str,
    values: &[&str],
) -> FeatureAttributeTable {
    FeatureAttributeTable {
        row_count: values.len() as u32,
        strings: values.iter().map(|value| value.to_string()).collect(),
        columns: vec![FeatureAttributeColumn {
            definition: FeatureAttributeDefinition {
                name: name.to_string(),
                semantic_role: Some(semantic_role.to_string()),
                value_type: FeatureAttributeType::String,
                domain: Default::default(),
            },
            data: FeatureAttributeColumnData::String((0..values.len() as u32).map(Some).collect()),
        }],
    }
}

/// A routing graph accelerated by the independent test CCH, plus the metrics
/// carrying its customized weights.
fn cch_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
) -> (RoutingGraph, Arc<CompiledProfileBundle>) {
    let (bundle, metrics) = build_test_cch(topology, metrics);
    let metrics = Arc::new(metrics);
    let graph = crate::build_routing_graph_from_shared(topology, metrics.clone(), Some(bundle))
        .expect("accelerated graph builds");
    (graph, metrics)
}

/// Exposure is free on edge 1 before 11:00 and charged from 11:00.
const EXPOSED_FROM_ELEVEN: &str = "feature_id,t_start,t_end,exposure_factor\n10,2026-07-10T08:00:00Z,2026-07-10T11:00:00Z,0\n10,2026-07-10T11:00:00Z,2026-07-10T13:00:00Z,1\n";

/// Diamond 0 -> {1, 2} -> 3 with a tail 3 -> 4 -> 5. The fast upper branch
/// carries exposure, so the exact frontier needs two labels at node 3.
fn component_label_overflow_fixture() -> (TopologyBundle, CompiledProfileBundle) {
    let edges = [(0, 1), (1, 3), (0, 2), (2, 3), (3, 4), (4, 5)]
        .into_iter()
        .enumerate()
        .map(|(index, (from, to))| edge(index as u32, from, to, index as i64 + 100, 10))
        .collect::<Vec<_>>();
    let mut topology = topology(
        "test",
        &[
            (6.0, 53.0),
            (6.001, 53.0),
            (6.0, 53.001),
            (6.001, 53.001),
            (6.002, 53.001),
            (6.003, 53.001),
        ],
        &edges,
    );
    topology.spatial_index = None;
    let mut metrics = metrics(&[1.0, 1.0, 10.0, 1.0, 1.0, 1.0]);
    metrics.components = vec![cost_component("exposure", &[10.0, 0.0, 0.0, 0.0, 0.0, 0.0])];
    (topology, metrics)
}

fn component_label_overflow_request(departure_time: Option<&str>) -> RouteRequest {
    let mut request = route_request(
        "component-label-overflow",
        point("origin", 6.0, 53.0),
        point("destination", 6.003, 53.001),
    );
    request.snap = snap_within(20.0);
    request.returns = full_geometry();
    request.temporal.departure_time = departure_time.map(str::to_string);
    request.temporal.pareto = Some(pareto("exposure", 1));
    request
}

#[test]
fn component_search_errors_instead_of_pruning_a_nondominated_label() {
    let (topology, metrics) = component_label_overflow_fixture();
    for (departure_time, expected) in [
        (None, "component search exceeded max_labels_per_state=1"),
        (
            Some("2026-07-10T10:00:00Z"),
            "temporal component search exceeded max_labels_per_state=1",
        ),
    ] {
        let request = component_label_overflow_request(departure_time);
        let error = execute_route(&topology, &metrics, &request)
            .expect_err("an exact frontier must not be truncated at the label guard");
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn temporal_reachability_prepass_returns_unreachable_before_tradeoff_overflow() {
    let (mut topology, mut metrics) = component_label_overflow_fixture();
    topology.temporal_rule_sets = vec![TemporalRuleSet {
        rules: vec![TemporalRule {
            day_mask: netweevil_core::EVERY_DAY,
            intervals: Vec::new(),
            effect: TemporalEffect::Closed,
        }],
    }];
    topology.edge_layers.routing[5].temporal_rule_id = Some(0);
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
    metrics.components = vec![cost_component("uncovered_time", &[100.0, 100.0, 0.0])];
    let mut request = a_to_c("covered-budget");
    request.snap = snap_within(20.0);
    request.returns = full_geometry();
    request.temporal.constraints = vec![zero_budget("uncovered_time")];

    let covered =
        execute_route(&topology, &metrics, &request).expect("covered component budget has a route");
    assert_eq!(covered.edge_path, vec![2]);
    assert_eq!(covered.summary.components["uncovered_time"], 0.0);

    let prepared = exact_engine(topology.clone(), metrics.clone());
    let origin_candidates = prepared
        .snap_route_candidates_with_options(&request.origin, &request.snap, true)
        .expect("origin candidates");
    let destination_candidates = prepared
        .snap_route_candidates_with_options(&request.destination, &request.snap, false)
        .expect("destination candidates");
    let covered_from_candidates = prepared
        .execute_route_between_candidates(&request, &origin_candidates, &destination_candidates)
        .expect("candidate API preserves component constraints");
    assert_eq!(covered_from_candidates.edge_path, vec![2]);
    assert_eq!(
        prepared
            .effective_route_engine_description(&request, EngineMode::Auto)
            .route_engine,
        "component_nondominated_label_setting"
    );

    request.route_id = "covered-pareto".to_string();
    request.temporal = TemporalRequestOptions {
        pareto: Some(pareto("uncovered_time", 16)),
        ..Default::default()
    };
    let frontier =
        execute_route(&topology, &metrics, &request).expect("Pareto frontier has routes");
    assert_eq!(frontier.edge_path, vec![0, 1]);
    assert_eq!(frontier.alternatives.len(), 1);
    assert_eq!(frontier.alternatives[0].edge_path, vec![2]);
}

#[test]
fn enforces_temporal_exposure_budget_and_returns_dynamic_pareto_frontier() {
    let topology = test_topology();
    let mut metrics = test_metrics();
    metrics.components = vec![CompiledCostComponent {
        overlay_name: Some("exposure_factor".to_string()),
        ..cost_component("exposure", &[10.0, 20.0, 100.0])
    }];
    let overlay_path = write_temp_file(
        "temporal-exposure.csv",
        "feature_id,t_start,t_end,exposure_factor\n10,2026-07-10T00:00:00Z,2026-07-11T00:00:00Z,1\n11,2026-07-10T00:00:00Z,2026-07-11T00:00:00Z,0\n",
    );
    let scenario_path = write_temp_file("temporal-exposure-scenario.yml", "id: exposure_test\n");
    let holiday_path = write_temp_file("temporal-exposure-holidays.yml", "- 2026-07-10\n");
    let mut request = a_to_c("temporal-exposure-budget");
    request.snap = snap_within(20.0);
    request.returns = ReturnConfig {
        segment_rows: true,
        ..full_geometry()
    };
    request.temporal = TemporalRequestOptions {
        departure_time: Some("2026-07-10T10:00:00Z".to_string()),
        scenario: Some(scenario_path.clone()),
        holiday_calendar: Some(holiday_path.clone()),
        overlays: vec![overlay_path.clone()],
        constraints: vec![zero_budget("exposure")],
        ..Default::default()
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

    request.route_id = "temporal-exposure-pareto".to_string();
    request.temporal.constraints.clear();
    request.temporal.pareto = Some(pareto("exposure", 16));
    let frontier =
        execute_route(&topology, &metrics, &request).expect("temporal exposure Pareto frontier");
    assert_eq!(frontier.edge_path, vec![0, 1]);
    assert_eq!(frontier.summary.components["exposure"], 30.0);
    assert_eq!(frontier.alternatives.len(), 1);
    assert_eq!(frontier.alternatives[0].edge_path, vec![2]);
    assert_eq!(frontier.alternatives[0].summary.components["exposure"], 0.0);

    let _ = fs::remove_file(overlay_path);
    let _ = fs::remove_file(scenario_path);
    let _ = fs::remove_file(holiday_path);
}

/// Arrive at c by 12:00 (4 h lookback) without any exposure.
fn exposure_free_arrive_by_noon(route_id: &str) -> RouteRequest {
    let mut request = a_to_c(route_id);
    request.snap = snap_within(20.0);
    request.returns = full_geometry();
    request.temporal = TemporalRequestOptions {
        arrive_by: Some("2026-07-10T12:00:00Z".to_string()),
        arrive_by_lookback_s: 4.0 * 3_600.0,
        constraints: vec![zero_budget("exposure")],
        ..Default::default()
    };
    request
}

#[test]
fn temporal_component_arrive_by_finds_narrow_later_window_without_waiting() {
    let mut topology = test_topology();
    topology.temporal_rule_sets = vec![TemporalRuleSet {
        rules: vec![open_only(9 * 60, 10 * 60), open_only(11 * 60, 11 * 60 + 1)],
    }];
    for edge in &mut topology.edge_layers.routing {
        edge.temporal_rule_id = Some(0);
    }
    let mut metrics = test_metrics();
    metrics.components = vec![cost_component("exposure", &[0.0, 0.0, 10.0])];
    metrics.temporal.allow_wait = false;
    let mut request = exposure_free_arrive_by_noon("late-opening-component-arrive-by");
    request.temporal.pareto = Some(pareto("exposure", 16));

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
    set_edge_cost(&mut metrics, 2, None);
    metrics.components = vec![CompiledCostComponent {
        overlay_name: Some("exposure_factor".to_string()),
        ..cost_component("exposure", &[10.0, 10.0, 0.0])
    }];
    let overlay_path = write_temp_file("arrive-by-overlay-boundary.csv", EXPOSED_FROM_ELEVEN);
    let mut request = exposure_free_arrive_by_noon("overlay-boundary-arrive-by");
    request.temporal.overlays = vec![overlay_path.clone()];
    request.temporal.pareto = Some(pareto("exposure", 16));

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
    topology.temporal_rule_sets = vec![TemporalRuleSet {
        rules: vec![open_only(8 * 60, 10 * 60 + 55), open_only(11 * 60, 13 * 60)],
    }];
    topology.edge_layers.routing[1].temporal_rule_id = Some(0);
    let mut metrics = test_metrics();
    set_edge_cost(&mut metrics, 2, None);
    metrics.temporal.allow_wait = true;
    metrics.temporal.max_wait_s = 10.0 * 60.0;
    metrics.components = vec![CompiledCostComponent {
        overlay_name: Some("exposure_factor".to_string()),
        ..cost_component("exposure", &[0.0, 10.0, 0.0])
    }];
    let overlay_path = write_temp_file("arrive-by-overlay-waiting.csv", EXPOSED_FROM_ELEVEN);
    let mut request = exposure_free_arrive_by_noon("overlay-waiting-arrive-by");
    request.temporal.overlays = vec![overlay_path.clone()];

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
    topology.edge_layers.routing[0].feature_row = 0;
    topology.edge_layers.routing[1].feature_row = 0;
    topology.edge_layers.routing[2].feature_row = 1;
    topology.feature_attributes =
        string_attribute_table("AssetGroup", "asset_group", &["mall", "street"]);
    let engine = exact_engine(topology, test_metrics());
    let scenario_path = write_temp_file(
        "scenario-batch-closure.yml",
        "id: close_short_path\nfeatures:\n  - source_feature_id: 10\n    force_closed: true\n",
    );
    let ranking_path = write_temp_file(
        "scenario-batch-ranking.csv",
        "source_feature_id,score\n10,9\n11,1\n",
    );
    let origin = point("a", 6.0, 53.0);
    let destination = point("c", 6.002, 53.0);
    let snap = snap_within(20.0);
    let case = |id: &str, selector| ScenarioBatchCase {
        id: id.to_string(),
        selector,
    };
    let request = crate::ScenarioBatchRequest {
        batch_id: "reroute".to_string(),
        departure_time: Some("2026-07-10T10:00:00Z".to_string()),
        scenarios: vec![
            case(
                "closure",
                ScenarioBatchSelector::Overlay {
                    overlay: scenario_path.clone(),
                },
            ),
            case(
                "mall_group",
                ScenarioBatchSelector::Group {
                    group: crate::ScenarioGroupSelector {
                        attribute: "asset_group".to_string(),
                        value: "mall".to_string(),
                    },
                },
            ),
            case(
                "top_ranked",
                ScenarioBatchSelector::TopK {
                    top_k: crate::ScenarioTopKSelector {
                        ranking: ranking_path.clone(),
                        count: 1,
                    },
                },
            ),
        ],
        routes: vec![RouteRequest {
            snap: snap.clone(),
            returns: full_geometry(),
            ..a_to_c("a_to_c")
        }],
        service_areas: Vec::new(),
        accessibility: Vec::new(),
        od: vec![crate::ScenarioOdRequest {
            analysis_id: "od".to_string(),
            request: crate::OdPairsDocument {
                snap: snap.clone(),
                ..od_pairs(vec![od_pair("a_to_c", origin.clone(), destination.clone())])
            },
        }],
        matrices: vec![crate::ScenarioMatrixRequest {
            analysis_id: "matrix".to_string(),
            origins: PointSetDocument {
                snap: snap.clone(),
                ..point_set(vec![origin])
            },
            destinations: PointSetDocument {
                snap: snap.clone(),
                ..point_set(vec![destination])
            },
        }],
        betweenness: vec![crate::BetweennessRequest {
            analysis_id: "usage".to_string(),
            origins: vec![weighted_point("a", 6.0, 53.0, 2.0)],
            destinations: vec![weighted_point("c", 6.002, 53.0, 3.0)],
            snap,
            temporal: Default::default(),
            max_od_pairs: 10,
            include_zero: false,
        }],
    };

    let result = crate::execute_scenario_batch(&engine, &request).expect("scenario batch result");
    let closure = &result.scenarios[0];
    let baseline = result.baseline.routes[0]
        .result
        .as_ref()
        .expect("baseline route");
    let scenario = closure.analyses.routes[0]
        .result
        .as_ref()
        .expect("scenario route");
    assert_eq!(baseline.edge_path, vec![0, 1]);
    assert_eq!(scenario.edge_path, vec![2]);
    assert_eq!(
        scenario.summary.scenario_id.as_deref(),
        Some("close_short_path")
    );
    let scenario_od = closure.analyses.od[0].result.as_ref().expect("scenario OD");
    assert_eq!(
        scenario_od.departure_time.as_deref(),
        Some("2026-07-10T10:00:00Z")
    );
    assert_eq!(scenario_od.scenario_id.as_deref(), Some("close_short_path"));
    let scenario_matrix = closure.analyses.matrices[0]
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
    assert_eq!(closure.diff.rerouting_burden_s, 70.0);
    assert_eq!(closure.diff.od_rerouting_burden_s, 70.0);
    assert_eq!(closure.diff.matrix_rerouting_burden_s, 70.0);
    assert_eq!(closure.diff.betweenness_edge_score_absolute_change, 18.0);
    assert_eq!(closure.diff.weighted_disconnected_demand, 0.0);
    assert_eq!(result.scenarios[1].closed_source_feature_ids, vec![10]);
    assert_eq!(result.scenarios[1].diff.rerouting_burden_s, 70.0);
    assert_eq!(result.scenarios[2].closed_source_feature_ids, vec![10]);
    assert_eq!(result.scenarios[2].diff.rerouting_burden_s, 70.0);
    let baseline_betweenness = result.baseline.betweenness[0]
        .result
        .as_ref()
        .expect("baseline betweenness");
    assert_eq!(baseline_betweenness.pairs.len(), 1);
    let scenario_betweenness = closure.analyses.betweenness[0]
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

    // Feature ids must exist in the active topology, and explicit overlays
    // must load before any baseline work starts.
    let stale_ranking_path = write_temp_file(
        "scenario-batch-stale-ranking.csv",
        "source_feature_id,score\n999,9\n",
    );
    let stale_overlay_path = write_temp_file(
        "scenario-batch-stale-overlay.yml",
        "id: stale_overlay\nfeatures:\n  - source_feature_id: 10\n    force_closed: true\n  - source_feature_id: 999\n    force_closed: true\n",
    );
    let missing_overlay_path = write_temp_file("scenario-batch-missing-overlay.yml", "");
    fs::remove_file(&missing_overlay_path).expect("remove missing-overlay placeholder");
    for (scenario, expected) in [
        (
            case(
                "stale_top_ranked",
                ScenarioBatchSelector::TopK {
                    top_k: crate::ScenarioTopKSelector {
                        ranking: stale_ranking_path.clone(),
                        count: 1,
                    },
                },
            ),
            ["not present in the active topology", "999"],
        ),
        (
            case(
                "stale_explicit_overlay",
                ScenarioBatchSelector::Overlay {
                    overlay: stale_overlay_path.clone(),
                },
            ),
            ["not present in the active topology", "999"],
        ),
        (
            case(
                "missing_explicit_overlay",
                ScenarioBatchSelector::Overlay {
                    overlay: missing_overlay_path,
                },
            ),
            [
                "validating scenario 'missing_explicit_overlay' overlay",
                "scenario-batch-missing-overlay.yml",
            ],
        ),
    ] {
        let mut invalid = request.clone();
        invalid.scenarios = vec![scenario];
        let error = crate::execute_scenario_batch(&engine, &invalid)
            .expect_err("invalid scenario must be rejected");
        assert!(
            expected.iter().all(|text| error.to_string().contains(text)),
            "unexpected error: {error}"
        );
    }

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
        "id: speed_up\nfeatures:\n  - source_feature_id: 10\n    speed_factor: 2.0\n",
    );
    let mut request = crate::AccessibilityRequest {
        origins: PointSetDocument {
            snap: snap_within(5.0),
            ..point_set(vec![point("origin", 6.0, 53.0)])
        },
        categories: vec![crate::AccessibilityCategoryRequest {
            category_id: "mid_edge".to_string(),
            destinations: PointSetDocument {
                snap: snap_within(5.0),
                ..point_set(vec![point("destination", 6.0005, 53.0)])
            },
        }],
        thresholds_s: vec![3.0],
        max_travel_time_s: 10.0,
    };

    let static_result = crate::execute_accessibility(&topology, &metrics, &request)
        .expect("static accessibility succeeds");
    assert!((static_result.rows[0].nearest_travel_time_s.unwrap() - 5.0).abs() < 1.0e-6);
    assert_eq!(static_result.rows[0].counts_within_threshold_s["3"], 0);

    request.origins.temporal = TemporalRequestOptions {
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
    let mut request = a_to_c("route");
    request.returns = ReturnConfig {
        segment_rows: true,
        road_type_breakdown: vec![BreakdownMetric::DistanceM, BreakdownMetric::TimeS],
        surface_breakdown: vec![BreakdownMetric::DistanceM],
        ..full_geometry()
    };

    let result =
        execute_route(&test_topology(), &test_metrics(), &request).expect("route succeeds");
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
    let mut request = a_to_c("route_alternatives");
    request.returns = full_geometry();
    request.alternatives = AlternativeRouteOptions {
        max_routes: 2,
        max_cost_ratio: 4.0,
        ..Default::default()
    };

    let result =
        execute_route(&test_topology(), &test_metrics(), &request).expect("route succeeds");
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
    let mut request = a_to_c("route");
    request.returns.segment_rows = true;

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
fn accelerated_query_returns_an_unpacked_path() {
    let topology = test_topology();
    let (graph, _) = cch_graph(&topology, &test_metrics());
    assert!(graph.acceleration.is_some(), "test CCH must be active");

    let path = crate::accelerated_route_query(&topology, &graph, 0, 2)
        .expect("accelerated query succeeds")
        .expect("accelerated path exists");

    assert_eq!(path.edge_indexes, vec![0, 1]);
    assert_eq!(path.total_generalized_cost, 30.0);
}

/// Stored profile bundles carry no arc weights; engine preparation
/// customizes them and must match the independent fixture customization.
#[test]
fn engine_customizes_stored_metrics_like_the_reference_cch() {
    let topology = test_topology();
    let (bundle, reference) = build_test_cch(&topology, &test_metrics());
    let mut stored = reference.clone();
    let header = stored.acceleration.as_mut().expect("fixture header");
    for weights in [
        &mut header.upward_weight,
        &mut header.downward_weight,
        &mut header.time_upward_weight,
        &mut header.time_downward_weight,
        &mut header.distance_upward_weight,
        &mut header.distance_downward_weight,
    ] {
        weights.clear();
    }
    let engine = PreparedRoutingEngine::new(Arc::new(topology), Arc::new(stored), Some(bundle))
        .expect("engine prepares");
    let customized = engine.metrics().acceleration.as_ref().expect("customized");
    let reference = reference.acceleration.expect("reference weights");
    assert_eq!(customized.upward_weight, reference.upward_weight);
    assert_eq!(customized.downward_weight, reference.downward_weight);
    // Service-area weights are customized on first use.
    assert!(customized.time_upward_weight.is_empty());
    let (graph, _) = engine.routing_graph_for_mode(EngineMode::Auto);
    let acceleration = graph.acceleration.as_ref().expect("accelerated graph");
    let time = ServiceAreaMetricKind::TravelTimeS;
    let distance = ServiceAreaMetricKind::DistanceM;
    assert!(acceleration.has_metric_weights(time));
    let (upward, downward) = acceleration
        .metric_weights(time, engine.topology())
        .expect("time weights");
    assert_eq!(upward, reference.time_upward_weight.as_slice());
    assert_eq!(downward, reference.time_downward_weight.as_slice());
    let (upward, downward) = acceleration
        .metric_weights(distance, engine.topology())
        .expect("distance weights");
    assert_eq!(upward, reference.distance_upward_weight.as_slice());
    assert_eq!(downward, reference.distance_downward_weight.as_slice());
}

#[test]
fn pure_summary_routes_omit_path_payloads() {
    let route = execute_route(&test_topology(), &test_metrics(), &a_to_c("summary-route"))
        .expect("route succeeds");
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
    let mut request = route_request(
        "phantom-route",
        point("a_mid", 6.0005, 53.0),
        point("b_mid", 6.0015, 53.0),
    );
    request.returns = ReturnConfig {
        segment_rows: true,
        ..full_geometry()
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
    let mut request = route_request("route", point("a", 0.0, 0.0), point("c", 6.002, 53.0));
    request.snap = snap_within(10.0);

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
    topology.edge_layers.routing[0].feature_row = 0;
    topology.edge_layers.routing[1].feature_row = 1;
    topology.edge_layers.routing[2].feature_row = 0;
    topology.feature_attributes =
        string_attribute_table("Location", "indoor_location", &["outdoor", "indoor"]);
    let graph = build_routing_graph(&topology, &test_metrics()).expect("graph builds");
    let point = LabeledPoint {
        z: Some(20.0),
        ..point("platform", 6.0, 53.0)
    };
    let snap = crate::SnapOptions {
        max_distance_m: 50.0,
        z_window_m: Some(2.0),
        attribute_filters: [("indoor_location".to_string(), "indoor".to_string())].into(),
        point_constraints: Default::default(),
    };
    let candidates =
        crate::snapping::snap_candidates_with_options(&topology, &graph, &point, &snap, true)
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
    let engine = exact_engine(test_topology(), test_metrics());
    let result = engine
        .execute_betweenness(&crate::BetweennessRequest {
            analysis_id: "usage".to_string(),
            origins: vec![weighted_point("a", 6.0, 53.0, 2.0)],
            destinations: vec![weighted_point("c", 6.002, 53.0, 3.0)],
            snap: Default::default(),
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
    let mut metrics = test_metrics();
    metrics.components = vec![cost_component("uncovered_time", &[100.0, 100.0, 0.0])];
    let engine = exact_engine(test_topology(), metrics);
    let result = engine
        .execute_betweenness(&crate::BetweennessRequest {
            analysis_id: "covered_usage".to_string(),
            origins: vec![weighted_point("a", 6.0, 53.0, 2.0)],
            destinations: vec![weighted_point("c", 6.002, 53.0, 3.0)],
            snap: snap_within(20.0),
            temporal: TemporalRequestOptions {
                constraints: vec![zero_budget("uncovered_time")],
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

    let candidates =
        crate::snap_candidates(&topology, &graph, &point("snap", 6.0, 53.0), 500.0, true)
            .expect("snap works");

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
    let document = crate::OdPairsDocument {
        returns: full_geometry(),
        ..od_pairs(vec![
            od_pair("ok", point("a", 6.0, 53.0), point("c", 6.002, 53.0)),
            od_pair("bad", point("far", 0.0, 0.0), point("c", 6.002, 53.0)),
        ])
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
fn executes_matrix_batch_with_presnapped_failures() {
    let origins = PointSetDocument {
        returns: full_geometry(),
        ..point_set(vec![point("a", 6.0, 53.0), point("far", 0.0, 0.0)])
    };
    let destinations = PointSetDocument {
        returns: full_geometry(),
        ..point_set(vec![point("c", 6.002, 53.0), point("b", 6.001, 53.0)])
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
        returns: full_geometry(),
        ..point_set(vec![point("a", 6.0, 53.0), point("b", 6.001, 53.0)])
    };
    let destinations = PointSetDocument {
        returns: full_geometry(),
        ..point_set(vec![point("b", 6.001, 53.0), point("c", 6.002, 53.0)])
    };

    let matrix =
        execute_matrix(&topology, &metrics, &origins, &destinations).expect("matrix succeeds");
    assert_eq!(matrix.cell_count, 4);
    assert_eq!(matrix.succeeded_count, 4);
    assert_eq!(matrix.cells[1].total_distance_m, Some(300));
    assert_eq!(matrix.cells[3].total_distance_m, Some(200));
    assert!(
        matrix.cells[1]
            .geometry
            .as_ref()
            .is_some_and(|coords| coords.len() >= 2)
    );

    let pairs = origins.points.iter().flat_map(|origin| {
        destinations
            .points
            .iter()
            .map(move |destination| (origin, destination))
    });
    for (cell, (origin, destination)) in matrix.cells.iter().zip(pairs) {
        let request = RouteRequest {
            returns: origins.returns.clone(),
            ..route_request(
                &format!("{}__{}", origin.id, destination.id),
                origin.clone(),
                destination.clone(),
            )
        };
        let route = execute_route(&topology, &metrics, &request).expect("route succeeds");
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
    let exact = exact_engine(test_topology(), test_metrics());
    let accelerated = cch_engine(test_topology(), &test_metrics());
    let requests = [
        RouteRequest {
            returns: full_geometry(),
            ..a_to_c("a_to_c")
        },
        route_request("a_to_b", point("a", 6.0, 53.0), point("b", 6.001, 53.0)),
    ];

    for request in requests {
        let exact = exact.execute_route(&request).expect("exact route succeeds");
        let accelerated = accelerated
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
    let mut sparse_metrics = test_metrics();
    set_edge_cost(&mut sparse_metrics, 2, None);
    let engine = cch_engine(test_topology(), &sparse_metrics);

    let route = engine
        .execute_route(&a_to_c("a_to_c_sparse_acceleration"))
        .expect("accelerated route succeeds");
    assert_eq!(route.summary.total_distance_m, 300);
    assert_eq!(route.summary.total_travel_time_s, 30.0);
}

#[test]
fn accelerated_engine_matches_exact_engine_under_pairwise_restrictions() {
    let topology = restricted_topology();
    let exact_engine = exact_engine(topology.clone(), restricted_metrics());
    let accelerated_engine = cch_engine(topology.clone(), &restricted_metrics());

    // All-pairs differential against the exact engine, including the pair
    // affected by the pairwise turn restriction.
    for origin in &topology.nodes {
        for destination in &topology.nodes {
            if origin.node_id == destination.node_id {
                continue;
            }
            let (from, to) = (origin.node_id.0, destination.node_id.0);
            let request = RouteRequest {
                returns: full_geometry(),
                ..route_request(
                    &format!("diff_{from}_{to}"),
                    point(&format!("o{from}"), origin.lon, origin.lat),
                    point(&format!("d{to}"), destination.lon, destination.lat),
                )
            };
            match (
                exact_engine.execute_route(&request),
                accelerated_engine.execute_route(&request),
            ) {
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
fn respects_pairwise_and_multi_edge_turn_restrictions() {
    for topology in [restricted_topology(), multi_edge_restricted_topology()] {
        let result = execute_route(
            &topology,
            &restricted_metrics(),
            &restricted_a_to_d("restricted"),
        )
        .expect("route succeeds");
        assert_eq!(result.edge_path, vec![3, 2]);
        assert_eq!(result.summary.total_distance_m, 300);
    }
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
fn multi_edge_restrictions_use_an_automaton_that_rejects_violating_paths() {
    let graph = build_routing_graph(&multi_edge_restricted_topology(), &restricted_metrics())
        .expect("graph builds");

    assert!(graph.has_restriction_sequences());
    assert!(!crate::path_respects_restriction_sequences(
        &graph,
        &[0, 1, 2]
    ));
    assert!(crate::path_respects_restriction_sequences(&graph, &[3, 2]));
}

#[test]
fn applies_turn_penalties_when_selecting_routes() {
    let topology = turn_penalty_topology();
    let mut metrics = turn_penalty_metrics();
    let request = penalty_route("turn-penalty");

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
    let request = penalty_route("traffic-signal-penalty");

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
    let request = penalty_route("roundabout-entry-penalty");

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
fn alternative_routes_fall_back_to_restricted_search_when_needed() {
    let mut request = restricted_a_to_d("multi-edge-restricted-alternative");
    request.alternatives = AlternativeRouteOptions {
        max_routes: 2,
        max_cost_ratio: 2.0,
        ..Default::default()
    };

    let result = execute_route(
        &multi_edge_restricted_topology(),
        &restricted_metrics(),
        &request,
    )
    .expect("route succeeds");

    assert_eq!(result.edge_path, vec![3, 2]);
    assert_eq!(result.alternatives.len(), 1);
    assert_eq!(result.alternatives[0].edge_path, vec![0, 1, 4]);
}

#[test]
fn can_ignore_multi_edge_restriction_sequences_via_engine_mode() {
    let mut metrics = restricted_metrics();
    set_edge_cost(&mut metrics, 3, Some(25.0));
    let engine = exact_engine(multi_edge_restricted_topology(), metrics);
    let request = restricted_a_to_d("multi-edge-override");

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
        "id,source_lon,source_lat,target_lon,target_lat,source_z,target_z\npair_1,6.1,53.1,6.2,53.2,12,24\n",
    );

    let document = load_od_pairs(&path).expect("CSV loads");

    assert_eq!(document.pairs.len(), 1);
    assert_eq!(document.pairs[0].pair_id, "pair_1");
    assert_eq!(document.pairs[0].origin.id, "pair_1:source");
    assert_eq!(document.pairs[0].destination.id, "pair_1:target");
    assert_eq!(document.pairs[0].origin.lon, 6.1);
    assert_eq!(document.pairs[0].destination.lat, 53.2);
    assert_eq!(document.pairs[0].origin.z, Some(12.0));
    assert_eq!(document.pairs[0].destination.z, Some(24.0));
    fs::remove_file(path).ok();
}

#[test]
fn loads_point_set_from_csv() {
    let path = write_temp_file("points.csv", "id,lon,lat,z\na,6.1,53.1,12\nb,6.2,53.2,\n");

    let document = load_point_set(&path).expect("CSV loads");

    assert_eq!(document.points.len(), 2);
    assert_eq!(document.points[0].id, "a");
    assert_eq!(document.points[1].lon, 6.2);
    assert_eq!(document.points[1].lat, 53.2);
    assert_eq!(document.points[0].z, Some(12.0));
    assert_eq!(document.points[1].z, None);
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
    let mut topology = test_topology();
    let mut reverse = topology.edge(1);
    reverse.edge_id = EdgeId(3);
    std::mem::swap(&mut reverse.from, &mut reverse.to);
    topology.push_edge(reverse);
    let engine = cch_engine(
        with_edge_based_topology(topology),
        &metrics(&[10.0, 20.0, 100.0, 20.0]),
    );

    // Five distinct snapped destinations per origin push the batch strategy
    // onto the shared CCH search-space path; the pairwise CCH query answers
    // the same pairs one by one for comparison.
    let origin_points = vec![point("a", 6.0, 53.0), point("ab_mid", 6.0004, 53.0)];
    let destination_points = vec![
        point("b", 6.001, 53.0),
        point("c", 6.002, 53.0),
        point("bc_low", 6.0013, 53.0),
        point("bc_mid", 6.0015, 53.0),
        point("bc_high", 6.0017, 53.0),
    ];
    let matrix = engine
        .execute_matrix(
            &point_set(origin_points.clone()),
            &point_set(destination_points.clone()),
        )
        .expect("matrix succeeds");
    assert_eq!(
        matrix.cell_count,
        origin_points.len() * destination_points.len()
    );

    for cell in &matrix.cells {
        let find = |points: &[LabeledPoint], id: &str| {
            points
                .iter()
                .find(|point| point.id == id)
                .expect("cell endpoint known")
                .clone()
        };
        let request = route_request(
            &format!("{}-{}", cell.origin_id, cell.destination_id),
            find(&origin_points, &cell.origin_id),
            find(&destination_points, &cell.destination_id),
        );
        match (&cell.status, engine.execute_route(&request)) {
            (crate::BatchItemStatus::Succeeded, Ok(route)) => {
                let matrix_cost = cell.total_generalized_cost.expect("matrix cost");
                assert!(
                    (matrix_cost - route.summary.total_generalized_cost).abs() <= 1e-6,
                    "cell {} cost {matrix_cost} != pairwise {}",
                    request.route_id,
                    route.summary.total_generalized_cost
                );
                let matrix_time = cell.total_travel_time_s.expect("matrix time");
                assert!(
                    (matrix_time - route.summary.total_travel_time_s).abs() <= 1e-6,
                    "cell {} time {matrix_time} != pairwise {}",
                    request.route_id,
                    route.summary.total_travel_time_s
                );
            }
            (crate::BatchItemStatus::Succeeded, Err(error)) => panic!(
                "matrix cell {} succeeded but pairwise route failed: {error:#}",
                request.route_id
            ),
            (_, Ok(route)) => panic!(
                "matrix cell {} did not succeed but pairwise route did (cost {})",
                request.route_id, route.summary.total_generalized_cost
            ),
            (_, Err(_)) => {}
        }
    }
}

#[test]
fn phast_expansion_matches_bounded_dijkstra_expansion() {
    let topology = test_topology();
    let (routing_graph, metrics) = cch_graph(&topology, &test_metrics());
    assert!(
        routing_graph.acceleration.is_some(),
        "fixture must produce an accelerated graph"
    );
    let origin = point("origin", 6.0, 53.0);
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
    let topology = test_topology();
    let mut base_metrics = test_metrics();
    base_metrics.components = vec![cost_component("exposure", &[1.0, 2.0, 3.0])];
    let (routing_graph, metrics) = cch_graph(&topology, &base_metrics);
    let origin = point("origin", 6.0, 53.0);
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

#[test]
fn fixed_point_near_tie_keeps_reconstruction_legal_and_error_bounded() {
    let topology = test_topology();
    let mut metrics = test_metrics();
    metrics.edge_metrics[0].generalized_cost = Some(10.0);
    metrics.edge_metrics[1].generalized_cost = Some(20.0001);
    metrics.edge_metrics[2].generalized_cost = Some(30.00005);
    let (graph, _) = cch_graph(&topology, &metrics);
    let path = crate::accelerated_route_query(&topology, &graph, 0, 2)
        .expect("query succeeds")
        .expect("path exists");
    for pair in path.edge_indexes.windows(2) {
        assert_eq!(
            topology.routing_edge(pair[0]).to,
            topology.routing_edge(pair[1]).from
        );
    }
    let actual: f64 = path
        .edge_indexes
        .iter()
        .map(|&edge| metrics.edge_metrics[edge].generalized_cost.unwrap())
        .sum();
    let exact = 30.00005;
    assert!(actual >= exact);
    assert!(actual - exact <= path.edge_indexes.len() as f64 / 2048.0);
    assert!(
        (actual - path.total_generalized_cost).abs() <= path.edge_indexes.len() as f64 / 2048.0
    );
}

#[test]
fn finite_weight_overflow_disables_acceleration_without_losing_route() {
    let topology = test_topology();
    let mut metrics = test_metrics();
    metrics.edge_metrics[1].generalized_cost = Some(1e12);
    let (graph, _) = cch_graph(&topology, &metrics);
    assert!(graph.acceleration.is_none());
    assert_eq!(graph.edge_costs[1], 1e12);
}

#[test]
fn phast_fractional_time_labels_obey_fixed_point_error_bound() {
    let topology = test_topology();
    let mut base_metrics = test_metrics();
    base_metrics.edge_metrics[1].travel_time_s = Some(20.0001);
    let (graph, metrics) = cch_graph(&topology, &base_metrics);
    let origin = point("origin", 6.0, 53.0);
    let candidates = crate::snapping::snap_candidates(&topology, &graph, &origin, 1.0, true)
        .expect("origin snaps");
    // The budget lies between the rounded and original end time of edge 1.
    let run = |settle_limit| {
        build_service_area_expansion_with_settle_limit(
            &topology,
            &metrics,
            &graph,
            &origin,
            &candidates,
            1.0,
            &Default::default(),
            ServiceAreaMetricKind::TravelTimeS,
            30.00005,
            settle_limit,
        )
        .expect("expansion succeeds")
    };
    let exact = run(usize::MAX);
    let rounded = run(0);
    let error = (exact.edge_end_costs[1] - rounded.edge_end_costs[1]).abs();
    assert!(error > 0.0 && error <= 1.0 / 2048.0);
    assert!(exact.edge_end_costs[1] > 30.00005);
    assert!(rounded.edge_end_costs[1] < 30.00005);
}

#[test]
fn compiled_sub_metre_spur_does_not_add_a_free_backtrack() {
    let footpath = |id, from, to, length_m| DirectedEdge {
        access_mask: AccessMask::new(AccessMask::FOOT),
        road_class: RoadClass::Path,
        ..edge(id, from, to, 10, length_m)
    };
    let topology = topology(
        "test",
        &[
            (114.179, 22.28),
            (114.18, 22.28),
            (114.180003, 22.28),
            (114.181, 22.28),
        ],
        &[
            footpath(0, 0, 1, 100),
            footpath(1, 1, 2, 0),
            footpath(2, 2, 1, 0),
            footpath(3, 1, 3, 100),
        ],
    );
    let profile = serde_yaml::from_str(
        "profile: {id: sub_metre, label: Sub-metre regression, mode: foot, defaults_pack: eu_foot_2026_01}",
    )
    .unwrap();
    let metrics = netweevil_profile::compile_profile_bundle(
        &profile,
        &topology,
        CacheBundleId::new("sub-metre-spur"),
    )
    .unwrap();
    assert!(metrics.edge_metrics[1].generalized_cost.unwrap() > 0.0);
    assert!(metrics.edge_metrics[2].generalized_cost.unwrap() > 0.0);
    let (graph, _) = cch_graph(&topology, &metrics);
    let path = crate::accelerated_route_query(&topology, &graph, 0, 3)
        .unwrap()
        .unwrap();
    assert_eq!(path.edge_indexes, vec![0, 3]);
}

#[test]
fn fixed_point_shortcut_unpacks_lower_triangle_including_zero_costs() {
    let mut topology = test_topology();
    topology.nodes.push(node(3, 6.003, 53.0));
    for (edge, (from, to)) in topology
        .edge_layers
        .routing
        .iter_mut()
        .zip([(1, 2), (0, 1), (2, 3)])
    {
        edge.from = NodeId(from);
        edge.to = NodeId(to);
    }
    let topology = with_edge_based_topology(topology);
    for middle_cost in [20.0001, 0.0] {
        let mut metrics = test_metrics();
        metrics.edge_metrics[0].generalized_cost = Some(middle_cost);
        let (bundle, compiled) = build_test_cch(&topology, &metrics);
        let shortcut_row = bundle.upward_first_out[1] as usize..bundle.upward_first_out[2] as usize;
        assert!(
            bundle.upward_head[shortcut_row].contains(&2),
            "contracting state 0 must add 1 -> 2"
        );
        let graph =
            crate::build_routing_graph_from_shared(&topology, Arc::new(compiled), Some(bundle))
                .expect("graph builds");
        let path = crate::accelerated_route_query(&topology, &graph, 0, 3)
            .expect("query succeeds")
            .expect("path exists");
        assert_eq!(path.edge_indexes, vec![1, 0, 2]);
        let original: f64 = path
            .edge_indexes
            .iter()
            .map(|&edge| metrics.edge_metrics[edge].generalized_cost.unwrap())
            .sum();
        assert!((original - path.total_generalized_cost).abs() <= 2.0 / 2048.0);
    }
}

#[test]
fn cch_termination_accounts_for_negative_destination_phantom_cost() {
    let topology = test_topology();
    let (graph, _) = cch_graph(&topology, &test_metrics());
    // The first meeting costs 18, but the upward route 0 -> 1 costs 30 - 15.
    // Stopping the forward queue at 18 discards the optimum.
    let origins = [(0, 10.0), (2, 18.0)];
    let destinations = [(1, -15.0), (2, 0.0)];
    let path =
        crate::accelerated_route_query_seeded(&topology, &graph, &origins, &destinations, None)
            .unwrap()
            .unwrap();
    assert_eq!(path.total_generalized_cost, 15.0);
    assert_eq!(path.edge_indexes, vec![0, 1]);
}

#[test]
fn cch_pruning_matches_complete_spaces_on_directed_graphs() {
    // Vary edge ordering, one-way connectivity, zero costs,
    // multiple endpoint seeds, and destination offsets. Complete unpruned
    // spaces provide an independent oracle for the pruned point query.
    let mut random = 19_u64;
    let mut next = || {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        random >> 32
    };
    let nodes = (0..8)
        .map(|index| (6.0 + f64::from(index) * 0.001, 53.0))
        .collect::<Vec<_>>();
    for _ in 0..12 {
        let mut edges = Vec::new();
        let mut costs = Vec::new();
        for edge_index in 0..24 {
            let from = (next() % 8) as u32;
            let to = (from + 1 + (next() % 7) as u32) % 8;
            edges.push(edge(edge_index, from, to, 10, 100));
            costs.push((next() % 30) as f64);
        }
        let topology = topology("test", &nodes, &edges);
        let (graph, _) = cch_graph(&topology, &metrics(&costs));
        for _ in 0..100 {
            let origins = [
                (next() as usize % 24, (next() % 30) as f64),
                (next() as usize % 24, (next() % 30) as f64),
            ];
            let destinations = [
                (next() as usize % 24, -(next() as f64 % 30.0)),
                (next() as usize % 24, -(next() as f64 % 30.0)),
            ];
            let forward = crate::build_forward_cch_space(&graph, &origins);
            let backward = crate::build_backward_cch_space(&graph, &destinations);
            let expected = crate::join_cch_spaces(&forward, &backward).map(|(cost, _)| cost);
            let actual = crate::accelerated_route_query_seeded(
                &topology,
                &graph,
                &origins,
                &destinations,
                None,
            )
            .unwrap();
            assert_eq!(actual.map(|path| path.total_generalized_cost), expected);
            let exact = crate::seeded_bidirectional_dijkstra_on_edge_transitions(
                &topology,
                &graph,
                &origins,
                &destinations,
                None,
            )
            .unwrap();
            assert_eq!(exact.map(|path| path.total_generalized_cost), expected);
        }
    }
}

#[test]
fn snap_cache_preserves_each_point_id_and_separates_radius_and_elevation_window() {
    let topology = test_topology();
    let graph = build_routing_graph(&topology, &test_metrics()).unwrap();
    let mut cache = std::collections::HashMap::new();
    let mut point = LabeledPoint {
        z: Some(5.0),
        ..point("first", 6.0, 53.0)
    };
    let wide = crate::SnapOptions {
        z_window_m: Some(10.0),
        ..snap_within(1.0)
    };
    let narrow = crate::SnapOptions {
        z_window_m: Some(1.0),
        ..snap_within(10.0)
    };
    let mut snap = |point: &LabeledPoint, options| {
        crate::snapping::cached_snap_candidates(&mut cache, &topology, &graph, point, options, true)
    };
    let first = snap(&point, &wide).unwrap();
    assert!(first.iter().all(|candidate| candidate.point_id == "first"));
    point.id = "second".into();
    let second = snap(&point, &wide).unwrap();
    assert!(
        second
            .iter()
            .all(|candidate| candidate.point_id == "second")
    );
    assert!(snap(&point, &narrow).is_err());
    point.id = "third".into();
    let error = snap(&point, &narrow).unwrap_err();
    assert!(error.to_string().contains("third"));
    assert_eq!(error.diagnostics[0].point_ids, vec!["third"]);
}

#[test]
fn snaps_long_edge_midpoints_even_when_a_nearer_node_search_finds_other_edges() {
    let mut topology = test_topology();
    topology.nodes[0].lon = 6.0;
    topology.nodes[1].lon = 6.1;
    topology.nodes[2].lon = 6.05;
    topology.nodes[2].lat = 53.00005;
    topology = with_edge_based_topology(topology);
    let graph = build_routing_graph(&topology, &test_metrics()).unwrap();
    let candidates = crate::snapping::snap_candidates(
        &topology,
        &graph,
        &point("middle", 6.05, 53.0),
        10.0,
        true,
    )
    .unwrap();
    assert_eq!(candidates[0].snapped_edge_id, Some(0));
    assert!((candidates[0].snapped_edge_fraction.unwrap() - 0.5).abs() < 1e-10);
    assert!(candidates[0].snap_distance_m < 1e-8);
}

#[test]
fn edge_spatial_index_keeps_all_projectable_edges_and_prunes_distant_leaves() {
    let mut topology = test_topology();
    topology.nodes = (0..512_u32)
        .flat_map(|index| {
            let lon = 4.0 + f64::from(index) * 0.01;
            [
                node(index * 2, lon, 53.0),
                node(index * 2 + 1, lon + 0.009, 53.0),
            ]
        })
        .collect();
    let edges = (0..512)
        .map(|index| edge(index, index * 2, index * 2 + 1, 10, 100))
        .collect::<Vec<_>>();
    topology.edge_layers = TopologyEdgeLayers::from_directed_edges(&edges);
    let mut costs = vec![1.0; edges.len()];
    costs[100] = f64::INFINITY;
    let index = crate::edge_spatial_index::EdgeSpatialIndex::build(&topology, &costs);
    for edge in 0..512 {
        let candidates = index.candidates(4.0 + edge as f64 * 0.01 + 0.0045, 53.0, 1.0);
        assert_eq!(candidates.contains(&(edge as u32)), edge != 100);
        assert!(
            candidates.len() <= 32,
            "localized query must avoid scanning the whole network"
        );
    }
}

#[test]
fn restricted_frontier_resumes_without_losing_destination_offsets_or_turn_history() {
    let topology = multi_edge_restricted_topology();
    let metrics = restricted_metrics();
    let graph = build_routing_graph(&topology, &metrics).unwrap();
    let fallback = crate::FallbackPolicy::default();
    let origins = [(0, graph.edge_costs[0]), (3, graph.edge_costs[3])];
    let mut search = crate::RestrictedSearchScratch::default();
    search.prepare(&metrics, &graph, &origins, &fallback);
    // Query a nearby target, resume through the forbidden sequence for a far
    // target, then revisit an already settled target and a fractional edge.
    for (edge, adjustment, expected) in [
        (1, 0.0, vec![0, 1]),
        (2, 0.0, vec![3, 2]),
        (0, -5.0, vec![0]),
        (2, -5.0, vec![3, 2]),
        (1, -5.0, vec![0, 1]),
    ] {
        let path = search
            .route_to(
                &topology,
                &metrics,
                &graph,
                &[(edge, adjustment)],
                None,
                &fallback,
            )
            .unwrap()
            .unwrap();
        assert!(crate::path_respects_restriction_sequences(
            &graph,
            &path.edge_indexes
        ));
        assert_eq!(path.edge_indexes, expected);
        let expected_cost = expected
            .iter()
            .map(|&edge| graph.edge_costs[edge])
            .sum::<f64>()
            + adjustment;
        assert_eq!(path.total_generalized_cost, expected_cost);
    }
}

#[test]
fn restricted_many_to_many_matrix_matches_independent_routes() {
    let engine = cch_engine(multi_edge_restricted_topology(), &restricted_metrics());
    let lat53 = |lon: f64| point(&lon.to_string(), lon, 53.0);
    let point_set = |points| PointSetDocument {
        snap: snap_within(10.0),
        returns: full_geometry(),
        ..point_set(points)
    };
    let origins = point_set(vec![lat53(6.0), lat53(6.0005)]);
    // Five distinct destinations activate shared CCH spaces even with turn
    // sequences. The cheapest unrestricted route violates [0, 1, 2].
    let destinations = point_set(vec![
        lat53(6.0022),
        lat53(6.0024),
        lat53(6.0026),
        lat53(6.0028),
        lat53(6.003),
    ]);
    let matrix = engine.execute_matrix(&origins, &destinations).unwrap();
    for (origin_index, origin) in origins.points.iter().enumerate() {
        for (destination_index, destination) in destinations.points.iter().enumerate() {
            let route = engine
                .execute_route(&RouteRequest {
                    snap: origins.snap.clone(),
                    returns: origins.returns.clone(),
                    ..route_request("independent", origin.clone(), destination.clone())
                })
                .unwrap();
            let cell = &matrix.cells[origin_index * destinations.points.len() + destination_index];
            assert_eq!(
                cell.total_generalized_cost,
                Some(route.summary.total_generalized_cost)
            );
            assert_eq!(
                cell.total_travel_time_s,
                Some(route.summary.total_travel_time_s)
            );
            assert_eq!(cell.geometry, route.geometry);
        }
    }
}

#[test]
fn coincident_opposite_snap_directions_choose_legal_lower_cost_despite_nanometre_noise() {
    let mut topology = test_topology();
    topology.edge_layers.routing[2].from = NodeId(2);
    topology.edge_layers.routing[2].to = NodeId(1);
    topology.edge_layers.routing[2].length_m = 200;
    let topology = with_edge_based_topology(topology);
    let mut metrics = test_metrics();
    set_edge_cost(&mut metrics, 2, Some(20.0));
    for acceleration in [false, true] {
        let graph = if acceleration {
            cch_graph(&topology, &metrics).0
        } else {
            build_routing_graph(&topology, &metrics).unwrap()
        };
        let origins = crate::snapping::snap_candidates(
            &topology,
            &graph,
            &point("origin", 6.0, 53.0),
            0.1,
            true,
        )
        .unwrap();
        let destinations = crate::snapping::snap_candidates(
            &topology,
            &graph,
            &point("target", 6.0015, 53.0),
            0.1,
            false,
        )
        .unwrap();
        let candidate_on = |edge| {
            destinations
                .iter()
                .find(|point| point.snapped_edge_id == Some(edge))
                .unwrap()
                .clone()
        };
        let mut reverse = candidate_on(2);
        let mut forward = candidate_on(1);
        reverse.snap_distance_m = 0.2;
        forward.snap_distance_m = 0.200000006;
        forward.snapped_lon += 6e-14;
        let route = |destinations: &[crate::SnappedPoint]| {
            crate::route_between_candidates(
                &topology,
                &metrics,
                &graph,
                1.0,
                &Default::default(),
                &Default::default(),
                &origins,
                destinations,
            )
            .unwrap()
        };
        let (_, destination, path, _) = route(&[reverse.clone(), forward.clone()]);
        assert_eq!(destination.snapped_edge_id, Some(1));
        assert_eq!(path.edge_indexes, vec![0, 1]);
        assert!((path.total_generalized_cost - 20.0).abs() < 1e-6);
        // A different floor or a truly different location keeps nearest-snap
        // preference, even when it happens to have a cheaper route.
        forward.snapped_z += 1.0;
        assert_eq!(
            route(&[reverse.clone(), forward.clone()]).1.snapped_edge_id,
            Some(2)
        );
        forward.snapped_z -= 1.0;
        forward.snapped_lon += 0.00001;
        assert_eq!(
            route(&[reverse.clone(), forward]).1.snapped_edge_id,
            Some(2)
        );
    }
}
