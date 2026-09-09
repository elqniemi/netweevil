use netweevil_core::{
    CCH_WEIGHT_INFINITY, CCH_WEIGHT_OVERFLOW, add_cch_weights, encode_cch_weight,
};
use rustc_hash::FxHashMap;
use std::collections::{BinaryHeap, HashMap};

use anyhow::{Context, Result};
use netweevil_core::{CompiledProfileBundle, TopologyBundle};

use crate::*;

#[derive(Default)]
pub(crate) struct RestrictedSearchScratch {
    dist: FxHashMap<SearchStateKey, f64>,
    previous: FxHashMap<SearchStateKey, Option<SearchStateKey>>,
    settled_edges: FxHashMap<usize, SearchStateKey>,
    heap: BinaryHeap<State>,
}

impl RestrictedSearchScratch {
    pub(crate) fn prepare(
        &mut self,
        metrics: &CompiledProfileBundle,
        routing_graph: &RoutingGraph,
        origin_seeds: &[(usize, f64)],
        fallback: &FallbackPolicy,
    ) {
        self.dist.clear();
        self.previous.clear();
        self.settled_edges.clear();
        self.heap.clear();
        for &(edge_index, cost) in origin_seeds {
            let (_, edge_penalty_cost) =
                edge_failure_mode_penalty(metrics, routing_graph, fallback, edge_index);
            let cost = cost + edge_penalty_cost;
            let automaton_state = routing_graph.automaton.transition(0, edge_index);
            let key = SearchStateKey {
                edge_index,
                automaton_state,
            };
            if !cost.is_finite() || self.dist.get(&key).is_some_and(|&known| known <= cost) {
                continue;
            }
            self.dist.insert(key, cost);
            self.previous.insert(key, None);
            self.heap.push(State {
                edge_index,
                automaton_state,
                cost,
                score: cost,
            });
        }
    }

    /// Continue one origin's Dijkstra frontier for another destination. Settled
    /// states and their predecessors remain valid across destination queries.
    pub(crate) fn route_to(
        &mut self,
        topology: &TopologyBundle,
        metrics: &CompiledProfileBundle,
        routing_graph: &RoutingGraph,
        destination_seeds: &[(usize, f64)],
        initial_upper_bound: Option<RoutePath>,
        fallback: &FallbackPolicy,
    ) -> Result<Option<RoutePath>> {
        let mut best_path = initial_upper_bound;
        let mut best_cost = best_path
            .as_ref()
            .map(|path| path.total_generalized_cost)
            .unwrap_or(f64::INFINITY);
        let mut best_state = None;
        let mut destination_adjustments = HashMap::<usize, f64>::new();
        for &(edge, adjustment) in destination_seeds {
            destination_adjustments
                .entry(edge)
                .and_modify(|known| *known = known.min(adjustment))
                .or_insert(adjustment);
        }
        let minimum_adjustment = destination_adjustments
            .values()
            .copied()
            .fold(0.0, f64::min);
        for (&edge, &adjustment) in &destination_adjustments {
            if let Some(&key) = self.settled_edges.get(&edge) {
                let cost = self.dist[&key] + adjustment;
                if cost < best_cost {
                    best_cost = cost;
                    best_state = Some(key);
                }
            }
        }
        // Preserve the unexpanded frontier for later destinations. Destination
        // offsets may be negative, so queue cost alone cannot bound a route.
        while self
            .heap
            .peek()
            .is_some_and(|state| state.cost + minimum_adjustment < best_cost)
        {
            let State {
                edge_index,
                automaton_state,
                cost,
                ..
            } = self.heap.pop().unwrap();
            let key = SearchStateKey {
                edge_index,
                automaton_state,
            };
            if cost > self.dist[&key] {
                continue;
            }
            self.settled_edges.entry(edge_index).or_insert(key);
            if let Some(&adjustment) = destination_adjustments.get(&edge_index) {
                let candidate_cost = cost + adjustment;
                if candidate_cost < best_cost {
                    best_cost = candidate_cost;
                    best_state = Some(key);
                }
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
                if next_cost + f64::EPSILON < *self.dist.get(&next_key).unwrap_or(&f64::INFINITY) {
                    self.dist.insert(next_key, next_cost);
                    self.previous.insert(next_key, Some(key));
                    self.heap.push(State {
                        edge_index: next_edge,
                        automaton_state: next_automaton_state,
                        cost: next_cost,
                        score: next_cost,
                    });
                }
            }
        }
        if let Some(mut cursor) = best_state {
            let mut edge_indexes = Vec::new();
            loop {
                edge_indexes.push(cursor.edge_index);
                let previous = self
                    .previous
                    .get(&cursor)
                    .context("failed to reconstruct restricted route path")?;
                let Some(previous) = previous else {
                    break;
                };
                cursor = *previous;
            }
            edge_indexes.reverse();
            best_path = Some(RoutePath {
                edge_indexes,
                total_generalized_cost: best_cost,
            });
        }
        Ok(best_path)
    }
}

pub(crate) fn edge_failure_mode_penalty(
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    fallback: &FallbackPolicy,
    edge_index: usize,
) -> (f64, f64) {
    if routing_graph
        .virtual_reverse_of
        .get(edge_index)
        .and_then(|value| *value)
        .is_some()
    {
        let penalty_s = fallback
            .penalties
            .reverse_oneway_penalty_s
            .unwrap_or_default();
        return (penalty_s, penalty_s * metrics.turn_costs.cost_time_weight);
    }
    (0.0, 0.0)
}

pub(crate) fn transition_failure_mode_penalty(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    automaton_state: usize,
    previous_edge_index: usize,
    next_edge_index: usize,
    fallback: &FallbackPolicy,
) -> Option<(f64, f64)> {
    let mut penalty_s = 0.0;
    if let Some(sequence_len) = routing_graph
        .automaton
        .prohibited_sequence_len(automaton_state, next_edge_index)
    {
        if fallback.ignore_turn_restrictions {
            penalty_s += fallback
                .penalties
                .ignored_turn_restriction_penalty_s
                .unwrap_or_default();
        } else if sequence_len == 2
            && classify_turn(topology, previous_edge_index, next_edge_index) == TurnDirection::Uturn
            && fallback.allow_uturn_where_normally_forbidden
        {
            penalty_s += fallback
                .penalties
                .forbidden_uturn_penalty_s
                .or(fallback.penalties.illegal_turn_penalty_s)
                .unwrap_or_default();
        } else if sequence_len == 2 && fallback.allow_illegal_turn {
            penalty_s += fallback
                .penalties
                .illegal_turn_penalty_s
                .unwrap_or_default();
        } else {
            return None;
        }
    }
    let (_, edge_penalty_cost) =
        edge_failure_mode_penalty(metrics, routing_graph, fallback, next_edge_index);
    Some((
        penalty_s,
        penalty_s * metrics.turn_costs.cost_time_weight + edge_penalty_cost,
    ))
}

/// Customizes CCH weights for a degraded (failure-mode) graph so requests
/// with failure modes route accelerated instead of via graph-wide Dijkstra.
///
/// Only sound when every restriction is pairwise: the failure penalties are
/// then a function of (previous edge, next edge) alone and are included
/// before quantizing base weights. Multi-edge restriction sequences return
/// `None` (their penalties are state-dependent) and keep the Dijkstra path.
/// Reverse-oneway variants change the edge set and never reach here.
pub(crate) fn customize_failure_mode_acceleration(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    degraded_graph: &RoutingGraph,
    base_acceleration: &AccelerationGraph,
    fallback: &FallbackPolicy,
) -> Option<netweevil_core::CompiledAcceleration> {
    let mode_bit = metrics.mode.access_bit();
    if topology.turn_restrictions.iter().any(|restriction| {
        restriction.mode_mask.contains(mode_bit) && restriction.edge_path.len() > 2
    }) {
        return None;
    }
    let source = base_acceleration.source.as_ref();
    let edge_count = degraded_graph.edge_costs.len();
    if source.upward_first_out.len() != edge_count + 1
        || source.downward_first_out.len() != edge_count + 1
    {
        return None;
    }
    let base_compiled = base_acceleration.metrics.acceleration.as_ref()?;

    let upward_len = source.upward_head.len();
    let downward_len = source.downward_head.len();
    let mut upward_weight = vec![CCH_WEIGHT_INFINITY; upward_len];
    let mut downward_weight = vec![CCH_WEIGHT_INFINITY; downward_len];

    // Phase 1: degraded transition costs plus the pairwise failure
    // penalties, exactly as the Dijkstra search would pay them.
    for edge_index in 0..edge_count {
        let automaton_state = degraded_graph.automaton.transition(0, edge_index);
        for transition_index in degraded_graph.transition_range(edge_index) {
            let next_edge = degraded_graph.transition_edges[transition_index] as usize;
            if next_edge == edge_index {
                continue;
            }
            let Some((_, penalty_cost)) = transition_failure_mode_penalty(
                topology,
                metrics,
                degraded_graph,
                automaton_state,
                edge_index,
                next_edge,
                fallback,
            ) else {
                continue;
            };
            let weight = degraded_graph.transition_costs[transition_index] + penalty_cost;
            if !weight.is_finite() {
                continue;
            }
            if source.edge_rank[edge_index] < source.edge_rank[next_edge] {
                if let Some(slot) = base_acceleration.upward_arc_slot(edge_index, next_edge as u32)
                {
                    upward_weight[slot] = upward_weight[slot].min(encode_cch_weight(weight));
                }
            } else if let Some(slot) =
                base_acceleration.downward_arc_slot(edge_index, next_edge as u32)
            {
                downward_weight[slot] = downward_weight[slot].min(encode_cch_weight(weight));
            }
        }
    }

    // Phase 2: triangle relaxation in elimination order, reusing the base
    // graph's reverse-downward index over the identical arc topology.
    for &middle in source.edge_order.iter() {
        let middle_index = middle as usize;
        let incoming_range = base_acceleration.reverse_downward_first_out[middle_index] as usize
            ..base_acceleration.reverse_downward_first_out[middle_index + 1] as usize;
        let outgoing_range = source.upward_first_out[middle_index] as usize
            ..source.upward_first_out[middle_index + 1] as usize;
        for reverse_slot in incoming_range {
            let incoming_arc = base_acceleration.reverse_downward_arc[reverse_slot] as usize;
            let incoming_weight = downward_weight[incoming_arc];
            if incoming_weight == CCH_WEIGHT_INFINITY {
                continue;
            }
            let tail = base_acceleration.downward_tail[incoming_arc] as usize;
            for outgoing_arc in outgoing_range.clone() {
                let head = source.upward_head[outgoing_arc];
                if head as usize == tail {
                    continue;
                }
                let outgoing_weight = upward_weight[outgoing_arc];
                if outgoing_weight == CCH_WEIGHT_INFINITY {
                    continue;
                }
                let candidate = add_cch_weights(incoming_weight, outgoing_weight);
                if source.edge_rank[tail] < source.edge_rank[head as usize] {
                    if let Some(slot) = base_acceleration.upward_arc_slot(tail, head)
                        && candidate < upward_weight[slot]
                    {
                        upward_weight[slot] = candidate;
                    }
                } else if let Some(slot) = base_acceleration.downward_arc_slot(tail, head)
                    && candidate < downward_weight[slot]
                {
                    downward_weight[slot] = candidate;
                }
            }
        }
    }

    if upward_weight.contains(&CCH_WEIGHT_OVERFLOW)
        || downward_weight.contains(&CCH_WEIGHT_OVERFLOW)
    {
        return None;
    }
    Some(netweevil_core::CompiledAcceleration {
        schema_version: netweevil_core::COMPILED_ACCELERATION_SCHEMA_VERSION,
        source_acceleration_bundle_id: base_compiled.source_acceleration_bundle_id.clone(),
        algorithm: source.algorithm.clone(),
        upward_weight,
        downward_weight,
        time_upward_weight: Vec::new(),
        time_downward_weight: Vec::new(),
        distance_upward_weight: Vec::new(),
        distance_downward_weight: Vec::new(),
    })
}

pub(crate) fn dijkstra_between_edge_seeds_with_failure_modes(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origin_seeds: &[(usize, f64)],
    destination_seeds: &[(usize, f64)],
    initial_upper_bound: Option<RoutePath>,
    fallback: &FallbackPolicy,
) -> Result<Option<RoutePath>> {
    RESTRICTED_SEARCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.prepare(metrics, routing_graph, origin_seeds, fallback);
        scratch.route_to(
            topology,
            metrics,
            routing_graph,
            destination_seeds,
            initial_upper_bound,
            fallback,
        )
    })
}
