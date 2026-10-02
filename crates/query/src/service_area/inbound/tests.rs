use std::collections::BinaryHeap;
use std::sync::Arc;

use super::*;
use crate::tests::fixtures::*;

/// Cost from the start of `start_edge` to the anchor by an independent
/// forward Dijkstra over (edge, forward-automaton state).
fn forward_cost_to_anchor(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    graph: &RoutingGraph,
    start_edge: usize,
    anchor: &[InboundSeed],
    metric_kind: ServiceAreaMetricKind,
) -> f64 {
    let anchor_fraction = |edge: usize| {
        anchor
            .iter()
            .filter(|seed| seed.edge_index == edge)
            .map(|seed| seed.limit_fraction)
            .fold(None, |best: Option<f64>, fraction| {
                Some(best.map_or(fraction, |known| known.min(fraction)))
            })
    };
    let cost_of = |edge| service_area_edge_cost(topology, metrics, edge, metric_kind).unwrap();
    let mut best = anchor_fraction(start_edge)
        .map(|fraction| fraction * cost_of(start_edge))
        .unwrap_or(f64::INFINITY);
    let mut dist = HashMap::<(usize, usize), f64>::new();
    let mut heap = BinaryHeap::new();
    let state = graph.automaton.transition(0, start_edge);
    dist.insert((start_edge, state), cost_of(start_edge));
    heap.push(State {
        edge_index: start_edge,
        automaton_state: state,
        cost: cost_of(start_edge),
        score: cost_of(start_edge),
    });
    while let Some(State {
        edge_index,
        automaton_state,
        cost,
        ..
    }) = heap.pop()
    {
        if cost > dist[&(edge_index, automaton_state)] {
            continue;
        }
        for transition in graph.transition_range(edge_index) {
            let next = graph.transition_edges[transition] as usize;
            if !graph.automaton.is_transition_allowed(automaton_state, next) {
                continue;
            }
            let before =
                cost + service_area_turn_cost(topology, metrics, edge_index, next, metric_kind);
            if let Some(fraction) = anchor_fraction(next) {
                best = best.min(before + fraction * cost_of(next));
            }
            let next_state = graph.automaton.transition(automaton_state, next);
            let next_cost = before + cost_of(next);
            if next_cost
                < dist
                    .get(&(next, next_state))
                    .copied()
                    .unwrap_or(f64::INFINITY)
            {
                dist.insert((next, next_state), next_cost);
                heap.push(State {
                    edge_index: next,
                    automaton_state: next_state,
                    cost: next_cost,
                    score: next_cost,
                });
            }
        }
    }
    best
}

fn accelerated(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
) -> (RoutingGraph, Arc<CompiledProfileBundle>) {
    let (bundle, metrics) = build_test_cch(topology, metrics);
    let metrics = Arc::new(metrics);
    let graph = crate::build_routing_graph_from_shared(topology, metrics.clone(), Some(bundle))
        .expect("accelerated graph builds");
    (graph, metrics)
}

/// Every inbound label equals the forward route cost from the start of the
/// edge to the anchor, with turn costs and multi-edge restrictions, on both
/// the exact backward Dijkstra and the backward PHAST sweep.
#[test]
fn inbound_labels_match_forward_routes_to_the_anchor() {
    let cases = [
        (test_topology(), test_metrics(), (6.002, 53.0)),
        (
            turn_penalty_topology(),
            turn_penalty_metrics(),
            (6.0015, 53.001),
        ),
        (restricted_topology(), restricted_metrics(), (6.003, 53.0)),
        (
            multi_edge_restricted_topology(),
            restricted_metrics(),
            (6.003, 53.0),
        ),
    ];
    for (topology, base_metrics, (lon, lat)) in cases {
        let (graph, metrics) = accelerated(&topology, &base_metrics);
        let anchor = point("anchor", lon, lat);
        let candidates = crate::snapping::snap_candidates(&topology, &graph, &anchor, 500.0, false)
            .expect("anchor snaps");
        for metric_kind in [
            ServiceAreaMetricKind::TravelTimeS,
            ServiceAreaMetricKind::DistanceM,
        ] {
            let resolution =
                resolve_service_area_origin(&anchor, &candidates, 500.0, &Default::default())
                    .expect("anchor resolves");
            let seeds = resolution
                .seed_candidates
                .iter()
                .flat_map(|candidate| {
                    inbound_seed_specs(&graph, &topology, &metrics, candidate, metric_kind)
                })
                .collect::<Vec<_>>();
            for settle_limit in [usize::MAX, 0] {
                let expansion = build_inbound_service_area_expansion_with_settle_limit(
                    &topology,
                    &metrics,
                    &graph,
                    &anchor,
                    &candidates,
                    500.0,
                    &Default::default(),
                    metric_kind,
                    1.0e9,
                    settle_limit,
                )
                .expect("inbound expansion succeeds");
                for edge in 0..topology.edge_count() {
                    let expected = forward_cost_to_anchor(
                        &topology,
                        &metrics,
                        &graph,
                        edge,
                        &seeds,
                        metric_kind,
                    );
                    let actual = expansion.edge_end_costs[edge];
                    assert!(
                        (expected - actual).abs() <= 1e-6
                            || (expected.is_infinite() && actual.is_infinite()),
                        "{} edge {edge} {metric_kind:?} settle {settle_limit}: forward {expected} vs inbound {actual}",
                        topology.source_path
                    );
                }
            }
        }
    }
}

/// On a one-way line only upstream positions reach the end node; the cut
/// span and ring bands follow the decreasing cost toward the anchor.
#[test]
fn inbound_service_area_cuts_upstream_spans_and_rings() {
    let topology = one_way_dead_end_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let request: ServiceAreaRequest = serde_json::from_value(serde_json::json!({
        "analysis_id": "inbound",
        "origins": [{"id": "anchor", "lon": 6.002, "lat": 53.0}],
        "thresholds": [
            {"id": "near", "limit": 5.0, "metric": "travel_time_s"},
            {"id": "far", "limit": 15.0, "metric": "travel_time_s"}
        ],
        "output_mode": "network",
        "band_mode": "ring",
        "boundary_mode": "cut_at_boundary",
        "multi_origin_mode": "overlap",
        "direction": "inbound",
        "returns": {"segments": true}
    }))
    .expect("request parses");
    let result = crate::execute_service_area(&topology, &metrics, &request)
        .expect("inbound service area succeeds");
    assert_eq!(result.direction, ServiceAreaDirection::Inbound);
    let spans = result
        .segments
        .iter()
        .map(|segment| {
            (
                segment.threshold_id.clone().unwrap(),
                segment.edge_index,
                (segment.start_fraction * 100.0).round() / 100.0,
                (segment.end_fraction * 100.0).round() / 100.0,
                (segment.start_cost * 100.0).round() / 100.0,
                (segment.end_cost * 100.0).round() / 100.0,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        spans,
        vec![
            ("near".to_string(), 1, 0.5, 1.0, 5.0, 0.0),
            ("far".to_string(), 0, 0.5, 1.0, 15.0, 10.0),
            ("far".to_string(), 1, 0.0, 0.5, 10.0, 5.0),
        ]
    );

    let mut outbound = request;
    outbound.direction = ServiceAreaDirection::Outbound;
    let outbound = crate::execute_service_area(&topology, &metrics, &outbound)
        .expect("outbound service area succeeds");
    // Outbound snapping cannot start at the dead end and never covers the
    // upstream edge 0.
    assert!(
        outbound
            .segments
            .iter()
            .all(|segment| segment.edge_index != 0)
    );
}
