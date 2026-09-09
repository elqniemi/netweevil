use std::sync::Arc;

use super::fixtures::{test_metrics, test_topology, with_edge_based_topology};
use crate::{PreparedRoutingEngine, TraceGapReason, TraceMatchRequest};
use netweevil_core::{
    AccessMask, CompiledEdgeMetric, EdgeId, NodeId, TopologyEdgeLayers, TopologyNode,
    TurnRestriction, TurnRestrictionKind,
};

fn engine(
    nodes: &[(f64, f64)],
    connections: &[(u32, u32)],
    restriction: Option<&[u32]>,
) -> PreparedRoutingEngine {
    let mut topology = test_topology();
    let template = topology.edge(0);
    topology.nodes = nodes
        .iter()
        .enumerate()
        .map(|(index, &(lon, lat))| TopologyNode {
            node_id: NodeId(index as u32),
            lon,
            lat,
            z: 0.0,
        })
        .collect();
    let edges = connections
        .iter()
        .enumerate()
        .map(|(index, &(from, to))| {
            let mut edge = template;
            edge.edge_id = EdgeId(index as u32);
            edge.from = NodeId(from);
            edge.to = NodeId(to);
            let (a, b) = (nodes[from as usize], nodes[to as usize]);
            edge.length_m =
                netweevil_core::geo::haversine_meters(a.0, a.1, b.0, b.1).round() as u32;
            edge
        })
        .collect::<Vec<_>>();
    topology.edge_layers = TopologyEdgeLayers::from_directed_edges(&edges);
    topology.spatial_index = None;
    if let Some(restriction) = restriction {
        topology.turn_restrictions = vec![TurnRestriction {
            relation_id: 1,
            kind: TurnRestrictionKind::NoTurn,
            edge_path: restriction.iter().copied().map(EdgeId).collect(),
            mode_mask: AccessMask::new(AccessMask::CAR),
        }];
    }
    topology = with_edge_based_topology(topology);
    let mut metrics = test_metrics();
    metrics.edge_metrics = edges
        .iter()
        .map(|edge| CompiledEdgeMetric {
            edge_id: edge.edge_id,
            travel_time_s: Some(f64::from(edge.length_m) / 10.0),
            generalized_cost: Some(f64::from(edge.length_m)),
        })
        .collect();
    PreparedRoutingEngine::new(Arc::new(topology), Arc::new(metrics), None).unwrap()
}

fn request(points: &[(f64, f64)]) -> TraceMatchRequest {
    serde_json::from_value(serde_json::json!({"trace_id":"test", "snap":{"max_distance_m":40},
        "max_transition_distance_m":2000,
        "observations":points.iter().enumerate().map(|(index, &(lon, lat))| serde_json::json!({"point":{"id":index.to_string(),"lon":lon,"lat":lat}})).collect::<Vec<_>>() })).unwrap()
}

#[test]
fn viterbi_prefers_a_connected_parallel_road_over_independent_nearest_snaps() {
    let engine = engine(
        &[
            (6.0, 53.0),
            (6.01, 53.0),
            (6.02, 53.0),
            (6.0, 53.0002),
            (6.01, 53.0002),
            (6.02, 53.0002),
        ],
        &[(0, 1), (1, 2), (3, 4), (4, 5)],
        None,
    );
    let request = request(&[(6.002, 53.00001), (6.009, 53.00019), (6.017, 53.00001)]);
    let nearest_middle = engine
        .snap_route_candidates(&request.observations[1].point, 40.0, true)
        .unwrap();
    assert_eq!(nearest_middle[0].snapped_edge_id, Some(2));
    let result = engine.execute_trace_match(&request).unwrap();
    assert_eq!(result.matchings.len(), 1);
    assert!(result.tracepoints.iter().all(Option::is_some));
    assert_eq!(
        result.tracepoints[1]
            .as_ref()
            .unwrap()
            .snapped
            .snapped_edge_id,
        Some(0)
    );
    assert_eq!(result.matchings[0].edge_path, vec![0, 1]);
    assert!(result.matchings[0].confidence > 0.0 && result.matchings[0].confidence < 1.0);
    assert_eq!(
        result.confidence_method,
        "exp_negative_mean_model_penalty_not_probability"
    );
}

#[test]
fn one_way_reverse_motion_splits_and_isolated_points_stay_unmatched() {
    let engine = engine(&[(6.0, 53.0), (6.01, 53.0)], &[(0, 1)], None);
    let result = engine
        .execute_trace_match(&request(&[(6.008, 53.0), (6.002, 53.0)]))
        .unwrap();
    assert!(result.matchings.is_empty());
    assert!(result.tracepoints.iter().all(Option::is_none));
    assert_eq!(result.gaps[0].reason, TraceGapReason::NoLegalTransition);
}

#[test]
fn sparse_same_edge_return_can_follow_a_legal_network_loop() {
    let engine = engine(
        &[(6.0, 53.0), (6.001, 53.0), (6.001, 53.001)],
        &[(0, 1), (1, 2), (2, 0)],
        None,
    );
    let mut request = request(&[(6.0008, 53.0), (6.0002, 53.0)]);
    request.snap.max_distance_m = 1.0;
    let result = engine.execute_trace_match(&request).unwrap();
    assert_eq!(result.matchings.len(), 1);
    assert_eq!(result.matchings[0].edge_path, vec![0, 1, 2, 0]);
    assert!(result.matchings[0].total_distance_m > 200.0);
    assert_eq!(result.matchings[0].geometry.last().unwrap()[0], 6.0002);
}

#[test]
fn turn_history_survives_intermediate_gps_observations() {
    let engine = engine(
        &[(6.0, 53.0), (6.001, 53.0), (6.001, 53.001), (6.002, 53.001)],
        &[(0, 1), (1, 2), (2, 3)],
        Some(&[0, 1, 2]),
    );
    let mut request = request(&[(6.0005, 53.0), (6.001, 53.0005), (6.0015, 53.001)]);
    request.snap.max_distance_m = 1.0;
    let result = engine.execute_trace_match(&request).unwrap();
    assert_eq!(result.matchings.len(), 1);
    assert_eq!(result.matchings[0].observation_indices, vec![0, 1]);
    assert!(result.tracepoints[2].is_none());
    assert_eq!(result.gaps[0].before_observation, 2);
    assert_eq!(result.gaps[0].reason, TraceGapReason::NoLegalTransition);
}

#[test]
fn disconnected_and_timestamp_gaps_produce_separate_matchings() {
    let engine = engine(
        &[(6.0, 53.0), (6.01, 53.0), (6.1, 53.0), (6.11, 53.0)],
        &[(0, 1), (2, 3)],
        None,
    );
    let mut request = request(&[(6.002, 53.0), (6.0021, 53.0), (6.102, 53.0), (6.1021, 53.0)]);
    let result = engine.execute_trace_match(&request).unwrap();
    assert_eq!(result.matchings.len(), 2);
    assert_eq!(result.gaps[0].reason, TraceGapReason::NoLegalTransition);
    for (observation, time) in request
        .observations
        .iter_mut()
        .zip([0.0, 1.0, 100.0, 101.0])
    {
        observation.timestamp_s = Some(time);
    }
    let result = engine.execute_trace_match(&request).unwrap();
    assert_eq!(result.matchings.len(), 2);
    assert_eq!(result.gaps[0].reason, TraceGapReason::TimeGap);
    request.observations[2].timestamp_s = Some(1.0);
    assert!(
        engine
            .execute_trace_match(&request)
            .unwrap_err()
            .to_string()
            .contains("strictly increasing")
    );
}

#[test]
fn state_budget_errors_and_timestamp_speed_bounds_are_explicit() {
    let engine = engine(
        &[(6.0, 53.0), (6.001, 53.0), (6.002, 53.0)],
        &[(0, 1), (1, 2)],
        None,
    );
    let mut request = request(&[(6.0005, 53.0), (6.0015, 53.0)]);
    request.max_transition_states = 1;
    assert!(
        engine
            .execute_trace_match(&request)
            .unwrap_err()
            .to_string()
            .contains("max_transition_states=1")
    );
    request.max_transition_states = 1000;
    request.snap.max_distance_m = 1.0;
    request.observations[0].timestamp_s = Some(0.0);
    request.observations[1].timestamp_s = Some(0.1);
    let result = engine.execute_trace_match(&request).unwrap();
    assert!(result.matchings.is_empty());
    request.temporal.departure_time = Some("2026-09-09T12:00:00Z".into());
    assert!(
        engine
            .execute_trace_match(&request)
            .unwrap_err()
            .to_string()
            .contains("static road costs only")
    );
}

#[test]
fn unmatched_observations_split_without_dropping_input_positions() {
    let engine = engine(&[(6.0, 53.0), (6.01, 53.0)], &[(0, 1)], None);
    let request = request(&[
        (6.002, 53.0),
        (6.004, 53.0),
        (40.0, 0.0),
        (6.006, 53.0),
        (6.008, 53.0),
    ]);
    let result = engine.execute_trace_match(&request).unwrap();
    assert_eq!(result.tracepoints.len(), 5);
    assert!(result.tracepoints[2].is_none());
    assert_eq!(result.matchings.len(), 2);
    assert_eq!(result.matchings[0].observation_indices, vec![0, 1]);
    assert_eq!(result.matchings[1].observation_indices, vec![3, 4]);
    assert_eq!(result.gaps[0].reason, TraceGapReason::NoCandidates);
}

#[test]
fn finite_extreme_parameters_cannot_produce_nonfinite_match_scores() {
    let engine = engine(
        &[(6.0, 53.0), (6.001, 53.0), (6.002, 53.0)],
        &[(0, 1), (1, 2)],
        None,
    );
    let mut request = request(&[(6.0005, 53.00001), (6.0015, 53.00001)]);
    request.gps_accuracy_m = f64::MIN_POSITIVE;
    assert!(
        engine
            .execute_trace_match(&request)
            .unwrap_err()
            .to_string()
            .contains("emission score overflowed")
    );
    request.gps_accuracy_m = 10.0;
    request.transition_beta_m = f64::from_bits(1);
    assert!(
        engine
            .execute_trace_match(&request)
            .unwrap_err()
            .to_string()
            .contains("model score overflowed")
    );
}

#[test]
fn total_search_budget_counts_work_across_separate_transitions() {
    let engine = engine(&[(6.0, 53.0), (6.01, 53.0)], &[(0, 1)], None);
    let mut request = request(&[(6.002, 53.0), (6.004, 53.0), (6.006, 53.0)]);
    request.snap.max_distance_m = 1.0;
    request.max_candidates = 1;
    request.max_transition_states = 1;
    request.max_search_states = 2;
    let matched = engine.execute_trace_match(&request).unwrap();
    assert_eq!(matched.matchings[0].observation_indices, vec![0, 1, 2]);
    request.max_search_states = 1;
    assert!(
        engine
            .execute_trace_match(&request)
            .unwrap_err()
            .to_string()
            .contains("max_search_states=1")
    );
}
