//! Inbound service areas: every network position that can reach the anchor
//! point within the budget.
//!
//! The search runs backward over the edge-based graph. A label `L(e)` is the
//! cost from the start of edge `e` to the anchor, so the position at fraction
//! `f` of `e` costs `L(e) - f * cost(e)` and a predecessor `p` of `e` gets
//! `L(p) = cost(p) + turn(p, e) + L(e)`. Turn costs are those of the forward
//! movement `p -> e`, and multi-edge restrictions are matched with the
//! reverse automaton (the reversed sequences read in backward order), so the
//! labels equal forward route costs exactly.
//!
//! Labels share the outbound expansion record with mirrored meaning:
//! `before` is the cost at the anchor-side end of the reachable span,
//! `end` the cost at fraction 0, and `start_fraction` that anchor-side end
//! (1 for whole edges, the snapped fraction on the anchor edge).

use super::*;

#[cfg(test)]
mod tests;

pub(crate) fn build_inbound_service_area_expansion(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    candidates: &[SnappedPoint],
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    metric_kind: ServiceAreaMetricKind,
    max_cost: f64,
) -> Result<ServiceAreaOriginExpansion> {
    build_inbound_service_area_expansion_with_settle_limit(
        topology,
        metrics,
        routing_graph,
        point,
        candidates,
        snap_max_distance_m,
        connectivity,
        metric_kind,
        max_cost,
        default_phast_settle_limit(topology.edge_count()),
    )
}

pub(crate) fn build_inbound_service_area_expansion_with_settle_limit(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    candidates: &[SnappedPoint],
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    metric_kind: ServiceAreaMetricKind,
    max_cost: f64,
    phast_settle_limit: usize,
) -> Result<ServiceAreaOriginExpansion> {
    let resolution =
        resolve_service_area_origin(point, candidates, snap_max_distance_m, connectivity)?;
    let mut labels = EdgeLabelBuilder::new(topology.edge_count());
    let seeds = resolution
        .seed_candidates
        .iter()
        .flat_map(|candidate| {
            inbound_seed_specs(routing_graph, topology, metrics, candidate, metric_kind)
        })
        .collect::<Vec<_>>();
    // Multi-edge restrictions need the product search; the hierarchy only
    // encodes pairwise bans.
    let phast_ready = !routing_graph.has_restriction_sequences()
        && phast_ready_for_metric(routing_graph, metric_kind);
    let settle_limit = if phast_ready {
        phast_settle_limit
    } else {
        usize::MAX
    };
    let aborted = backward_dijkstra(
        topology,
        metrics,
        routing_graph,
        &seeds,
        metric_kind,
        max_cost,
        settle_limit,
        &mut labels,
    );
    if aborted {
        labels.clear();
        backward_phast(
            topology,
            metrics,
            routing_graph,
            &seeds,
            metric_kind,
            max_cost,
            &mut labels,
        );
    }
    let (reached_edges, edge_before_costs, edge_end_costs, edge_start_fractions, _) =
        labels.finish();
    Ok(ServiceAreaOriginExpansion {
        origin_id: point.id.clone(),
        representative_origin: resolution.representative_origin,
        fallback_used: resolution.fallback_used,
        origin_hop_distance_m: resolution.origin_hop_distance_m,
        inbound: true,
        edge_before_costs,
        edge_end_costs,
        edge_start_fractions,
        edge_component_ranges: HashMap::new(),
        reached_edges,
        diagnostics: resolution.diagnostics,
        warnings: resolution.warnings,
    })
}

/// An anchor-side edge: `cost` reaches the anchor from fraction 0 and the
/// edge is usable up to `limit_fraction`.
#[derive(Debug, Clone, Copy)]
struct InboundSeed {
    edge_index: usize,
    cost: f64,
    limit_fraction: f64,
}

fn inbound_seed_specs(
    routing_graph: &RoutingGraph,
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    candidate: &SnappedPoint,
    metric_kind: ServiceAreaMetricKind,
) -> Vec<InboundSeed> {
    if let (Some(edge_id), Some(fraction)) =
        (candidate.snapped_edge_id, candidate.snapped_edge_fraction)
    {
        let edge_index = edge_id as usize;
        return service_area_edge_cost(topology, metrics, edge_index, metric_kind)
            .filter(|_| routing_graph.edge_costs[edge_index].is_finite())
            .map(|full_cost| InboundSeed {
                edge_index,
                cost: full_cost * fraction,
                limit_fraction: fraction,
            })
            .into_iter()
            .collect();
    }
    routing_graph
        .incoming_edges(candidate.snapped_node_id as usize)
        .iter()
        .filter_map(|&edge_index| {
            let edge_index = edge_index as usize;
            service_area_edge_cost(topology, metrics, edge_index, metric_kind).map(|cost| {
                InboundSeed {
                    edge_index,
                    cost,
                    limit_fraction: 1.0,
                }
            })
        })
        .collect()
}

/// Turn cost of the forward movement `previous -> next`, read from the
/// per-transition table when the graph has one.
fn forward_turn_cost(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    turn_seconds: Option<&TransitionTurnSeconds>,
    previous: usize,
    next: usize,
    metric_kind: ServiceAreaMetricKind,
) -> f64 {
    if matches!(metric_kind, ServiceAreaMetricKind::DistanceM) {
        return 0.0;
    }
    if let Some(table) = turn_seconds
        && let Some(transition_index) = routing_graph
            .transition_range(previous)
            .find(|&index| routing_graph.transition_edges[index] as usize == next)
    {
        return table.get(transition_index);
    }
    service_area_turn_cost(topology, metrics, previous, next, metric_kind)
}

/// Exact backward Dijkstra over (edge, reverse-automaton state). Returns
/// `true` when it settled more than `settle_limit` states and gave up.
fn backward_dijkstra(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    seeds: &[InboundSeed],
    metric_kind: ServiceAreaMetricKind,
    max_cost: f64,
    settle_limit: usize,
    labels: &mut EdgeLabelBuilder,
) -> bool {
    let automaton = &routing_graph.reverse_automaton;
    let turn_seconds = match metric_kind {
        ServiceAreaMetricKind::TravelTimeS => {
            routing_graph.transition_turn_seconds(topology, metrics)
        }
        ServiceAreaMetricKind::DistanceM => None,
    };
    SINGLE_SOURCE_SEARCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.prepare(topology.edge_count());
        let mut heap = take_radix_heap();
        let mut restricted_dist = FxHashMap::<(u32, u32), f64>::default();
        let known = |scratch: &SingleSourceEdgeSearchScratch,
                     restricted_dist: &FxHashMap<(u32, u32), f64>,
                     edge_index: usize,
                     state: usize| {
            if state == 0 {
                scratch.dist[edge_index]
            } else {
                restricted_dist
                    .get(&(edge_index as u32, state as u32))
                    .copied()
                    .unwrap_or(f64::INFINITY)
            }
        };
        let improve = |scratch: &mut SingleSourceEdgeSearchScratch,
                       restricted_dist: &mut FxHashMap<(u32, u32), f64>,
                       edge_index: usize,
                       state: usize,
                       cost: f64| {
            if state == 0 {
                scratch.update(edge_index, cost, NO_PREVIOUS_EDGE)
            } else {
                let slot = restricted_dist
                    .entry((edge_index as u32, state as u32))
                    .or_insert(f64::INFINITY);
                if cost + f64::EPSILON >= *slot {
                    return false;
                }
                *slot = cost;
                true
            }
        };

        // Positions on an anchor edge just before the anchor are always
        // inside the budget, so every seed gets a label.
        for seed in seeds {
            let state = automaton.transition(0, seed.edge_index);
            labels.update(
                seed.edge_index,
                0.0,
                seed.cost,
                seed.limit_fraction,
                Vec::new(),
                Vec::new(),
            );
            if improve(
                &mut scratch,
                &mut restricted_dist,
                seed.edge_index,
                state,
                seed.cost,
            ) && seed.cost <= max_cost
            {
                heap.push(seed.cost, heap_payload(seed.edge_index, state));
            }
        }

        let mut settled = 0_usize;
        while let Some((cost, payload)) = heap.pop() {
            let (edge_index, state) = heap_state(payload);
            if cost > known(&scratch, &restricted_dist, edge_index, state) || cost > max_cost {
                continue;
            }
            settled += 1;
            if settled > settle_limit {
                return_radix_heap(heap);
                return true;
            }
            for slot in routing_graph.reverse_transition_range(edge_index) {
                let previous = routing_graph.reverse_transition_edges[slot] as usize;
                if !automaton.is_transition_allowed(state, previous) {
                    continue;
                }
                let Some(edge_cost) =
                    service_area_edge_cost(topology, metrics, previous, metric_kind)
                else {
                    continue;
                };
                let near = cost
                    + forward_turn_cost(
                        topology,
                        metrics,
                        routing_graph,
                        turn_seconds,
                        previous,
                        edge_index,
                        metric_kind,
                    );
                if near > max_cost {
                    continue;
                }
                let far = near + edge_cost;
                let next_state = automaton.transition(state, previous);
                if !improve(
                    &mut scratch,
                    &mut restricted_dist,
                    previous,
                    next_state,
                    far,
                ) {
                    continue;
                }
                labels.update(previous, near, far, 1.0, Vec::new(), Vec::new());
                if far <= max_cost {
                    heap.push(far, heap_payload(previous, next_state));
                }
            }
        }
        return_radix_heap(heap);
        false
    })
}

/// Backward PHAST over the quantized hierarchy metric. The hierarchy's arc
/// weight `w(e -> f)` includes the cost of `f`, so the sweep works on
/// `D(e) = L(e) - cost(e)`, the cost from the end of `e`: anchor seeds
/// start at `-(1 - fraction) * cost`, the upward phase follows downward
/// arcs in reverse from the anchor, and a rank-descending pull over upward
/// arcs finalizes every lower-ranked edge. Rounding can move labels across
/// the budget, as in the outbound sweep.
fn backward_phast(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    seeds: &[InboundSeed],
    metric_kind: ServiceAreaMetricKind,
    max_cost: f64,
    labels: &mut EdgeLabelBuilder,
) {
    let acceleration = routing_graph
        .acceleration
        .as_ref()
        .expect("backward PHAST requires acceleration");
    let (upward_weight, downward_weight) = acceleration
        .metric_weights(metric_kind, topology)
        .expect("backward PHAST requires per-metric weights");
    let source = acceleration.source.as_ref();
    let edge_count = routing_graph.edge_costs.len();
    let full_cost = |edge_index: usize| {
        service_area_edge_cost(topology, metrics, edge_index, metric_kind).unwrap_or(f64::INFINITY)
    };

    let mut dist = vec![f64::INFINITY; edge_count];
    let mut heap = BinaryHeap::new();
    for seed in seeds {
        labels.update(
            seed.edge_index,
            0.0,
            seed.cost,
            seed.limit_fraction,
            Vec::new(),
            Vec::new(),
        );
        let from_end = seed.cost - full_cost(seed.edge_index);
        if from_end < dist[seed.edge_index] {
            dist[seed.edge_index] = from_end;
            heap.push(State {
                edge_index: seed.edge_index,
                automaton_state: 0,
                cost: from_end,
                score: from_end,
            });
        }
    }

    // Upward phase of the reverse graph: downward arcs x -> y relaxed from
    // y to the higher-ranked x.
    while let Some(State {
        edge_index, cost, ..
    }) = heap.pop()
    {
        if cost > dist[edge_index] || cost > max_cost {
            continue;
        }
        for slot in acceleration.reverse_downward_first_out[edge_index] as usize
            ..acceleration.reverse_downward_first_out[edge_index + 1] as usize
        {
            let tail = acceleration.reverse_downward_edge[slot] as usize;
            let arc = acceleration.reverse_downward_arc[slot] as usize;
            let next_cost = cost + netweevil_core::decode_cch_weight(downward_weight[arc]);
            if next_cost < dist[tail] {
                dist[tail] = next_cost;
                heap.push(State {
                    edge_index: tail,
                    automaton_state: 0,
                    cost: next_cost,
                    score: next_cost,
                });
            }
        }
    }

    // Pull sweep in descending rank: every upward arc u -> v leads to a
    // higher-ranked v whose distance is already final.
    for &edge in source.edge_order.iter().rev() {
        let edge = edge as usize;
        let mut best = dist[edge];
        #[allow(clippy::needless_range_loop)]
        for slot in
            source.upward_first_out[edge] as usize..source.upward_first_out[edge + 1] as usize
        {
            let head = source.upward_head[slot] as usize;
            let head_cost = dist[head];
            if !head_cost.is_finite() || head_cost > max_cost {
                continue;
            }
            let candidate = head_cost + netweevil_core::decode_cch_weight(upward_weight[slot]);
            if candidate < best {
                best = candidate;
            }
        }
        dist[edge] = best;
    }

    for (edge_index, &from_end) in dist.iter().enumerate() {
        if !from_end.is_finite() || from_end > max_cost {
            continue;
        }
        let far = from_end + full_cost(edge_index);
        if !far.is_finite() {
            continue;
        }
        labels.update(
            edge_index,
            from_end.max(0.0),
            far,
            1.0,
            Vec::new(),
            Vec::new(),
        );
    }
}
