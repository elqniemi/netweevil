use std::sync::Arc;

use netweevil_core::CacheBundleId;

use super::*;
use crate::tests::fixtures::{build_test_cch, edge, set_edge_cost, topology, with_no_turn};

const SIDE: u32 = 5;

/// A 5x5 two-way grid with varied costs, turn penalties and one forbidden
/// turn, so base arcs carry turn costs and some transitions are missing.
fn grid() -> (TopologyBundle, CompiledProfileBundle) {
    let nodes = (0..SIDE * SIDE)
        .map(|id| {
            (
                6.0 + f64::from(id % SIDE) * 0.001,
                53.0 + f64::from(id / SIDE) * 0.001,
            )
        })
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    let mut costs = Vec::new();
    for id in 0..SIDE * SIDE {
        for neighbour in [
            (id % SIDE + 1 < SIDE).then_some(id + 1),
            (id / SIDE + 1 < SIDE).then_some(id + SIDE),
        ]
        .into_iter()
        .flatten()
        {
            for (from, to) in [(id, neighbour), (neighbour, id)] {
                edges.push(edge(edges.len() as u32, from, to, i64::from(id), 80));
                costs.push(f64::from(4 + (from * 7 + to * 3) % 9));
            }
        }
    }
    let topology = with_no_turn(topology("recustomize-grid", &nodes, &edges), 900, &[0, 2]);
    let mut metrics = crate::tests::fixtures::turn_weighted_metrics(&costs);
    metrics.turn_costs.left_penalty_s = 3.0;
    metrics.turn_costs.right_penalty_s = 1.5;
    metrics.turn_costs.uturn_penalty_s = 20.0;
    (topology, metrics)
}

/// Slot-ordered weights a full customization of `metrics` produces.
fn customized(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    source: &netweevil_core::DatasetAccelerationBundle,
) -> (Vec<u32>, Vec<u32>) {
    let weights = netweevil_profile::customize_acceleration(
        topology,
        metrics,
        source,
        CacheBundleId::new("recustomize-test"),
    );
    (weights.upward_weight, weights.downward_weight)
}

#[test]
fn recustomized_weights_equal_a_full_customization() {
    let (topology, metrics) = grid();
    let (bundle, compiled) = build_test_cch(&topology, &metrics);
    let topology = Arc::new(topology);
    let engine = PreparedRoutingEngine::new(topology.clone(), Arc::new(compiled), Some(bundle))
        .expect("engine prepares");
    let (graph, _) = engine.routing_graph_for_mode(EngineMode::Auto);
    let base = graph.acceleration.as_ref().expect("accelerated graph");
    assert_eq!(
        base.ranked.slot_weights(&base.source),
        customized(&topology, &metrics, &base.source)
    );

    let edge_count = topology.edge_count();
    let changes: Vec<Vec<(usize, Option<f64>)>> = vec![
        vec![(7, None)],
        vec![(3, Some(40.0)), (4, Some(55.5))],
        (0..edge_count)
            .step_by(5)
            .map(|edge| (edge, None))
            .collect(),
        (10..30).map(|edge| (edge, Some(13.25))).collect(),
        vec![(1, None), (2, Some(1.0e7)), (edge_count - 1, None)],
    ];
    for change in changes {
        let mut changed_metrics = metrics.clone();
        for &(edge, cost) in &change {
            set_edge_cost(&mut changed_metrics, edge, cost);
        }
        let changed = change
            .iter()
            .map(|&(edge, _)| edge as u32)
            .collect::<Vec<_>>();
        let ranked = recustomize_changed_edges(&topology, &changed_metrics, graph, &changed)
            .expect("accelerated base");
        assert_eq!(
            ranked.slot_weights(&base.source),
            customized(&topology, &changed_metrics, &base.source),
            "change {change:?}"
        );
    }
}
