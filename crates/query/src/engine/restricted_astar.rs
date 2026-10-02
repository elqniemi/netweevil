//! Restriction-aware A* for candidates whose hierarchy path violates a
//! multi-edge restriction.
//!
//! The search runs on the same state space and floating-point transition
//! costs as the restriction-aware Dijkstra fallback, so it returns the same
//! optimal cost. Its potential is a CCH distance to the destination seeds
//! under a lower-bound customization: every transition starts at
//! `floor(1024 * cost)` units, so each hierarchy distance is a lower bound
//! on the original cost of any path, restrictions only remove paths, and the
//! potential is consistent. Potentials are evaluated lazily over upward arcs
//! (`pi(x) = min(B(x), min over x -> y of w(x -> y) + pi(y))`, where `B` is
//! the complete backward search space of the destination seeds), as in
//! CH-potentials.

use std::collections::BinaryHeap;

use anyhow::{Context, Result};
use netweevil_core::{
    CCH_WEIGHT_INFINITY, CCH_WEIGHT_OVERFLOW, CCH_WEIGHT_SCALE, CompiledProfileBundle,
    TopologyBundle, decode_cch_weight,
};
use rustc_hash::FxHashMap;

use crate::*;

/// Hierarchy weights whose base arcs round transition costs down.
pub(crate) struct LowerBoundWeights {
    upward: Vec<u32>,
    downward: Vec<u32>,
}

/// Lower-bound weights for `routing_graph`, customized on first use. `None`
/// when the graph has no hierarchy or a transition has no hierarchy arc, in
/// which case callers keep the Dijkstra fallback.
pub(crate) fn lower_bound_weights(routing_graph: &RoutingGraph) -> Option<&LowerBoundWeights> {
    let acceleration = routing_graph.acceleration.as_ref()?;
    acceleration
        .lower_bounds
        .get_or_init(|| build_lower_bound_weights(routing_graph, acceleration))
        .as_ref()
}

/// Customizes the lower-bound weights of a graph with multi-edge
/// restrictions on a background thread, so the first restricted fallback
/// does not pay for them. A search that needs them sooner waits for the
/// same customization rather than starting its own.
pub(crate) fn prepare_lower_bounds_in_background(routing_graph: &RoutingGraph) {
    if routing_graph.acceleration.is_none() || !routing_graph.has_restriction_sequences() {
        return;
    }
    let graph = routing_graph.clone();
    let spawned = std::thread::Builder::new()
        .name("netweevil-lower-bounds".into())
        .spawn(move || {
            lower_bound_weights(&graph);
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "lower-bound customization runs on first use instead");
    }
}

fn build_lower_bound_weights(
    routing_graph: &RoutingGraph,
    acceleration: &AccelerationGraph,
) -> Option<LowerBoundWeights> {
    let source = acceleration.source.as_ref();
    let mut upward = vec![CCH_WEIGHT_INFINITY; source.upward_head.len()];
    let mut downward = vec![CCH_WEIGHT_INFINITY; source.downward_head.len()];
    for edge in 0..routing_graph.edge_costs.len() {
        for transition in routing_graph.transition_range(edge) {
            let next = routing_graph.transition_edges[transition] as usize;
            let cost = routing_graph.transition_costs[transition];
            if next == edge || !cost.is_finite() {
                continue;
            }
            let scaled = (cost.max(0.0) * CCH_WEIGHT_SCALE).floor();
            if scaled >= f64::from(CCH_WEIGHT_OVERFLOW) {
                return None;
            }
            let weight = if source.edge_rank[edge] < source.edge_rank[next] {
                &mut upward[acceleration.upward_arc_slot(edge, next as u32)?]
            } else {
                &mut downward[acceleration.downward_arc_slot(edge, next as u32)?]
            };
            *weight = (*weight).min(scaled as u32);
        }
    }
    relax_lower_triangles(acceleration, &mut upward, &mut downward);
    if upward.contains(&CCH_WEIGHT_OVERFLOW) || downward.contains(&CCH_WEIGHT_OVERFLOW) {
        return None;
    }
    Some(LowerBoundWeights { upward, downward })
}

#[derive(Default)]
struct AStarScratch {
    backward: FxHashMap<u32, f64>,
    backward_heap: BinaryHeap<State>,
    potential: FxHashMap<u32, f64>,
    potential_stack: Vec<u32>,
    dist: FxHashMap<SearchStateKey, f64>,
    previous: FxHashMap<SearchStateKey, Option<SearchStateKey>>,
    heap: BinaryHeap<State>,
}

static ASTAR_SCRATCH: ScratchPool<AStarScratch> = ScratchPool::new();

impl AStarScratch {
    fn clear(&mut self) {
        self.backward.clear();
        self.backward_heap.clear();
        self.potential.clear();
        self.potential_stack.clear();
        self.dist.clear();
        self.previous.clear();
        self.heap.clear();
    }

    /// Complete backward search space of the destination seeds under the
    /// lower-bound weights.
    fn search_backward(
        &mut self,
        acceleration: &AccelerationGraph,
        weights: &LowerBoundWeights,
        destination_seeds: &[(usize, f64)],
    ) {
        for &(edge, adjustment) in destination_seeds {
            let known = self.backward.entry(edge as u32).or_insert(f64::INFINITY);
            if adjustment < *known {
                *known = adjustment;
                self.backward_heap.push(State {
                    edge_index: edge,
                    automaton_state: 0,
                    cost: adjustment,
                    score: adjustment,
                });
            }
        }
        while let Some(State {
            edge_index, cost, ..
        }) = self.backward_heap.pop()
        {
            if cost > self.backward[&(edge_index as u32)] {
                continue;
            }
            let source = acceleration.source.as_ref();
            let downward_slots = acceleration.downward_slots();
            let rank = source.edge_rank[edge_index] as usize;
            for position in acceleration.ranked.downward_range(rank) {
                let weight = weights.downward[downward_slots[position] as usize];
                if weight == CCH_WEIGHT_INFINITY {
                    continue;
                }
                let (tail_rank, _) = acceleration.ranked.downward_arc(position);
                let previous = source.edge_order[tail_rank as usize];
                let next_cost = cost + decode_cch_weight(weight);
                let known = self.backward.entry(previous).or_insert(f64::INFINITY);
                if next_cost < *known {
                    *known = next_cost;
                    self.backward_heap.push(State {
                        edge_index: previous as usize,
                        automaton_state: 0,
                        cost: next_cost,
                        score: next_cost,
                    });
                }
            }
        }
    }

    /// Lower bound on the remaining cost from `edge` to any destination seed.
    fn potential(
        &mut self,
        acceleration: &AccelerationGraph,
        weights: &LowerBoundWeights,
        edge: u32,
    ) -> f64 {
        if let Some(&known) = self.potential.get(&edge) {
            return known;
        }
        let source = acceleration.source.as_ref();
        self.potential_stack.push(edge);
        while let Some(&state) = self.potential_stack.last() {
            if self.potential.contains_key(&state) {
                self.potential_stack.pop();
                continue;
            }
            let arcs = source.upward_first_out[state as usize] as usize
                ..source.upward_first_out[state as usize + 1] as usize;
            let mut pending = false;
            for slot in arcs.clone() {
                let head = source.upward_head[slot];
                if weights.upward[slot] != CCH_WEIGHT_INFINITY
                    && !self.potential.contains_key(&head)
                {
                    self.potential_stack.push(head);
                    pending = true;
                }
            }
            if pending {
                continue;
            }
            let mut best = self.backward.get(&state).copied().unwrap_or(f64::INFINITY);
            for slot in arcs {
                let weight = weights.upward[slot];
                if weight == CCH_WEIGHT_INFINITY {
                    continue;
                }
                best =
                    best.min(decode_cch_weight(weight) + self.potential[&source.upward_head[slot]]);
            }
            self.potential.insert(state, best);
            self.potential_stack.pop();
        }
        self.potential[&edge]
    }
}

/// Exact restriction-aware shortest path between seeded endpoints. Returns
/// `Ok(None)` when no path beats `initial_upper_bound`, mirroring
/// `RestrictedSearchScratch::route_to`; returns `Err` only on internal
/// reconstruction failure. Callers must check `lower_bound_weights` first.
pub(crate) fn restricted_astar_route(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    weights: &LowerBoundWeights,
    origin_seeds: &[(usize, f64)],
    destination_seeds: &[(usize, f64)],
    initial_upper_bound: Option<RoutePath>,
    fallback: &FallbackPolicy,
) -> Result<Option<RoutePath>> {
    let acceleration = routing_graph
        .acceleration
        .as_ref()
        .expect("lower-bound weights require acceleration");
    ASTAR_SCRATCH.with(|scratch| {
        scratch.clear();
        scratch.search_backward(acceleration, weights, destination_seeds);

        let mut destination_adjustments = FxHashMap::<usize, f64>::default();
        for &(edge, adjustment) in destination_seeds {
            destination_adjustments
                .entry(edge)
                .and_modify(|known| *known = known.min(adjustment))
                .or_insert(adjustment);
        }

        for &(edge_index, cost) in origin_seeds {
            let (_, edge_penalty_cost) =
                edge_failure_mode_penalty(metrics, routing_graph, fallback, edge_index);
            let cost = cost + edge_penalty_cost;
            let potential = scratch.potential(acceleration, weights, edge_index as u32);
            if !cost.is_finite() || !potential.is_finite() {
                continue;
            }
            let automaton_state = routing_graph.automaton.transition(0, edge_index);
            let key = SearchStateKey {
                edge_index,
                automaton_state,
            };
            if scratch.dist.get(&key).is_some_and(|&known| known <= cost) {
                continue;
            }
            scratch.dist.insert(key, cost);
            scratch.previous.insert(key, None);
            scratch.heap.push(State {
                edge_index,
                automaton_state,
                cost,
                score: cost + potential,
            });
        }

        let mut best_cost = initial_upper_bound
            .as_ref()
            .map(|path| path.total_generalized_cost)
            .unwrap_or(f64::INFINITY);
        let mut best_state = None;
        // The potential never overestimates the remaining cost, so the
        // minimum key bounds every unexplored completion.
        while scratch
            .heap
            .peek()
            .is_some_and(|state| state.score < best_cost)
        {
            let State {
                edge_index,
                automaton_state,
                cost,
                ..
            } = scratch.heap.pop().expect("peeked state");
            let key = SearchStateKey {
                edge_index,
                automaton_state,
            };
            if cost > scratch.dist[&key] {
                continue;
            }
            if let Some(&adjustment) = destination_adjustments.get(&edge_index)
                && cost + adjustment < best_cost
            {
                best_cost = cost + adjustment;
                best_state = Some(key);
            }
            for transition_index in routing_graph.transition_range(edge_index) {
                let next_edge = routing_graph.transition_edges[transition_index] as usize;
                let Some((_, penalty_cost)) = transition_failure_mode_penalty(
                    topology,
                    metrics,
                    routing_graph,
                    automaton_state,
                    edge_index,
                    next_edge,
                    fallback,
                ) else {
                    continue;
                };
                let next_cost =
                    cost + routing_graph.transition_costs[transition_index] + penalty_cost;
                let next_automaton_state = routing_graph
                    .automaton
                    .transition(automaton_state, next_edge);
                let next_key = SearchStateKey {
                    edge_index: next_edge,
                    automaton_state: next_automaton_state,
                };
                if next_cost + f64::EPSILON
                    >= scratch
                        .dist
                        .get(&next_key)
                        .copied()
                        .unwrap_or(f64::INFINITY)
                {
                    continue;
                }
                let potential = scratch.potential(acceleration, weights, next_edge as u32);
                if !potential.is_finite() {
                    continue;
                }
                scratch.dist.insert(next_key, next_cost);
                scratch.previous.insert(next_key, Some(key));
                scratch.heap.push(State {
                    edge_index: next_edge,
                    automaton_state: next_automaton_state,
                    cost: next_cost,
                    score: next_cost + potential,
                });
            }
        }

        let Some(mut cursor) = best_state else {
            return Ok(initial_upper_bound);
        };
        let mut edge_indexes = Vec::new();
        loop {
            edge_indexes.push(cursor.edge_index);
            let previous = scratch
                .previous
                .get(&cursor)
                .context("failed to reconstruct restricted route path")?;
            let Some(previous) = previous else {
                break;
            };
            cursor = *previous;
        }
        edge_indexes.reverse();
        Ok(Some(RoutePath {
            edge_indexes,
            total_generalized_cost: best_cost,
        }))
    })
}
