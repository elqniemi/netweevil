use std::cell::RefCell;
use std::collections::{BinaryHeap, HashMap, HashSet};

use anyhow::Result;
use netweevil_core::{CompiledProfileBundle, TopologyBundle};

use crate::*;

thread_local! {
    pub(crate) static EDGE_SEARCH_SCRATCH: RefCell<BidirectionalEdgeSearchScratch> =
        RefCell::new(BidirectionalEdgeSearchScratch::default());
    pub(crate) static ACCELERATION_SEARCH_SCRATCH: RefCell<BidirectionalAccelerationScratch> =
        RefCell::new(BidirectionalAccelerationScratch::default());
    pub(crate) static RESTRICTED_SEARCH_SCRATCH: RefCell<RestrictedSearchScratch> =
        RefCell::new(RestrictedSearchScratch::default());
    pub(crate) static SINGLE_SOURCE_SEARCH_SCRATCH: RefCell<SingleSourceEdgeSearchScratch> =
        RefCell::new(SingleSourceEdgeSearchScratch::default());
}

#[derive(Default)]
pub(crate) struct BidirectionalEdgeSearchScratch {
    pub(crate) forward_dist: Vec<f64>,
    forward_previous: Vec<u32>,
    forward_touched: Vec<u32>,
    pub(crate) forward_heap: BinaryHeap<State>,
    pub(crate) backward_dist: Vec<f64>,
    backward_next: Vec<u32>,
    backward_touched: Vec<u32>,
    pub(crate) backward_heap: BinaryHeap<State>,
}

impl BidirectionalEdgeSearchScratch {
    pub(crate) fn prepare(&mut self, edge_count: usize) {
        if self.forward_dist.len() < edge_count {
            self.forward_dist.resize(edge_count, f64::INFINITY);
            self.forward_previous.resize(edge_count, NO_PREVIOUS_EDGE);
            self.backward_dist.resize(edge_count, f64::INFINITY);
            self.backward_next.resize(edge_count, NO_PREVIOUS_EDGE);
        }
        for &edge_index in &self.forward_touched {
            self.forward_dist[edge_index as usize] = f64::INFINITY;
            self.forward_previous[edge_index as usize] = NO_PREVIOUS_EDGE;
        }
        for &edge_index in &self.backward_touched {
            self.backward_dist[edge_index as usize] = f64::INFINITY;
            self.backward_next[edge_index as usize] = NO_PREVIOUS_EDGE;
        }
        self.forward_touched.clear();
        self.forward_heap.clear();
        self.backward_touched.clear();
        self.backward_heap.clear();
    }

    pub(crate) fn update_forward(
        &mut self,
        edge_index: usize,
        cost: f64,
        previous_edge: u32,
    ) -> bool {
        if !cost.is_finite() {
            return false;
        }
        if cost + f64::EPSILON >= self.forward_dist[edge_index] {
            return false;
        }
        if !self.forward_dist[edge_index].is_finite() {
            self.forward_touched.push(edge_index as u32);
        }
        self.forward_dist[edge_index] = cost;
        self.forward_previous[edge_index] = previous_edge;
        true
    }

    pub(crate) fn update_backward(&mut self, edge_index: usize, cost: f64, next_edge: u32) -> bool {
        if !cost.is_finite() {
            return false;
        }
        if cost + f64::EPSILON >= self.backward_dist[edge_index] {
            return false;
        }
        if !self.backward_dist[edge_index].is_finite() {
            self.backward_touched.push(edge_index as u32);
        }
        self.backward_dist[edge_index] = cost;
        self.backward_next[edge_index] = next_edge;
        true
    }
}

#[derive(Default)]
pub(crate) struct SingleSourceEdgeSearchScratch {
    pub(crate) dist: Vec<f64>,
    pub(crate) previous: Vec<u32>,
    touched: Vec<u32>,
    pub(crate) heap: BinaryHeap<State>,
}

impl SingleSourceEdgeSearchScratch {
    pub(crate) fn prepare(&mut self, edge_count: usize) {
        if self.dist.len() < edge_count {
            self.dist.resize(edge_count, f64::INFINITY);
            self.previous.resize(edge_count, NO_PREVIOUS_EDGE);
        }
        for &edge_index in &self.touched {
            self.dist[edge_index as usize] = f64::INFINITY;
            self.previous[edge_index as usize] = NO_PREVIOUS_EDGE;
        }
        self.touched.clear();
        self.heap.clear();
    }

    pub(crate) fn update(&mut self, edge_index: usize, cost: f64, previous_edge: u32) -> bool {
        if !cost.is_finite() || cost + f64::EPSILON >= self.dist[edge_index] {
            return false;
        }
        if !self.dist[edge_index].is_finite() {
            self.touched.push(edge_index as u32);
        }
        self.dist[edge_index] = cost;
        self.previous[edge_index] = previous_edge;
        true
    }
}

#[derive(Clone)]
pub(crate) struct SingleSourceEdgeTree {
    pub(crate) dist: Vec<f64>,
    pub(crate) previous: Vec<u32>,
}

pub(crate) fn route_between_candidates(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    fallback: &FallbackPolicy,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
) -> Result<(SnappedPoint, SnappedPoint, RoutePath, HopSelectionInfo)> {
    let origin_components = candidate_component_ids(origin_candidates);
    let destination_components = candidate_component_ids(destination_candidates);
    if !origin_components.is_empty()
        && !destination_components.is_empty()
        && origin_components.is_disjoint(&destination_components)
        && matches!(connectivity.disconnected, DisconnectedNetworkMode::Strict)
    {
        return Err(no_route_failure(topology, origin_candidates, destination_candidates).into());
    }

    let mut path_cache = HashMap::<((u32, u64, u64), (u32, u64, u64)), Option<RoutePath>>::new();

    for origin in origin_candidates {
        for destination in destination_candidates {
            if same_edge_reverse_pair(origin, destination) {
                continue;
            }
            let Some(hop_info) =
                hop_info_for_pair(origin, destination, snap_max_distance_m, connectivity)
            else {
                continue;
            };
            let key = (snap_cache_key(origin), snap_cache_key(destination));
            let path = if let Some(cached) = path_cache.get(&key) {
                cached.clone()
            } else {
                let direct_path = direct_same_edge_path(routing_graph, origin, destination);
                let path = if routing_graph.has_restriction_sequences() {
                    let origin_seeds = origin_edge_seeds(routing_graph, origin);
                    let destination_seeds = destination_edge_seeds(routing_graph, destination);
                    if has_failure_modes(fallback) {
                        if routing_graph.acceleration.is_some()
                            && routing_graph.acceleration_includes_failure_penalties
                        {
                            // The customized weights already carry the
                            // pairwise failure penalties (customization is
                            // only attached for pairwise-only datasets), so
                            // hierarchy query minimizes the quantized metric
                            // without an automaton pass.
                            accelerated_route_query_seeded(
                                topology,
                                routing_graph,
                                &origin_seeds,
                                &destination_seeds,
                                direct_path.clone(),
                            )?
                        } else {
                            dijkstra_between_edge_seeds_with_failure_modes(
                                topology,
                                metrics,
                                routing_graph,
                                &origin_seeds,
                                &destination_seeds,
                                direct_path.clone(),
                                fallback,
                            )?
                        }
                    } else {
                        // Both searches ignore multi-edge restrictions. A
                        // legal candidate is optimal under the search metric,
                        // which is quantized when CCH is active. An illegal
                        // candidate requires the automaton search.
                        let pairwise_candidate = if routing_graph.acceleration.is_some() {
                            accelerated_route_query_seeded(
                                topology,
                                routing_graph,
                                &origin_seeds,
                                &destination_seeds,
                                direct_path.clone(),
                            )?
                        } else {
                            seeded_bidirectional_dijkstra_on_edge_transitions(
                                topology,
                                routing_graph,
                                &origin_seeds,
                                &destination_seeds,
                                direct_path.clone(),
                            )?
                        };
                        match pairwise_candidate {
                            Some(path)
                                if path_respects_restriction_sequences(
                                    routing_graph,
                                    &path.edge_indexes,
                                ) =>
                            {
                                Some(path)
                            }
                            Some(_) => dijkstra_between_edge_seeds_with_failure_modes(
                                topology,
                                metrics,
                                routing_graph,
                                &origin_seeds,
                                &destination_seeds,
                                direct_path.clone(),
                                fallback,
                            )?,
                            None => None,
                        }
                    }
                } else if routing_graph.acceleration.is_some() {
                    // The CCH query minimizes the quantized metric without
                    // a follow-up search on the base graph.
                    let origin_seeds = origin_edge_seeds(routing_graph, origin);
                    let destination_seeds = destination_edge_seeds(routing_graph, destination);
                    accelerated_route_query_seeded(
                        topology,
                        routing_graph,
                        &origin_seeds,
                        &destination_seeds,
                        direct_path.clone(),
                    )?
                } else {
                    let origin_seeds = origin_edge_seeds(routing_graph, origin);
                    let destination_seeds = destination_edge_seeds(routing_graph, destination);
                    if has_failure_modes(fallback) {
                        dijkstra_between_edge_seeds_with_failure_modes(
                            topology,
                            metrics,
                            routing_graph,
                            &origin_seeds,
                            &destination_seeds,
                            direct_path.clone(),
                            fallback,
                        )?
                    } else {
                        seeded_bidirectional_dijkstra_on_edge_transitions(
                            topology,
                            routing_graph,
                            &origin_seeds,
                            &destination_seeds,
                            direct_path.clone(),
                        )?
                    }
                };
                path_cache.insert(key, path.clone());
                path
            };
            if let Some(path) = path {
                let path =
                    finalize_route_path(topology, metrics, path.edge_indexes, origin, destination);
                return Ok((origin.clone(), destination.clone(), path, hop_info));
            }
        }
    }

    Err(no_route_failure(topology, origin_candidates, destination_candidates).into())
}

pub(crate) fn route_between_candidates_with_banned_edges(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    fallback: &FallbackPolicy,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    banned_edges: &HashSet<usize>,
    max_generalized_cost: Option<f64>,
) -> Result<Option<(SnappedPoint, SnappedPoint, RoutePath, HopSelectionInfo)>> {
    let mut best: Option<(SnappedPoint, SnappedPoint, RoutePath, HopSelectionInfo)> = None;
    for origin in origin_candidates {
        for destination in destination_candidates {
            if same_edge_reverse_pair(origin, destination) {
                continue;
            }
            let Some(hop_info) =
                hop_info_for_pair(origin, destination, snap_max_distance_m, connectivity)
            else {
                continue;
            };
            let direct_path =
                direct_same_edge_path(routing_graph, origin, destination).filter(|path| {
                    !path
                        .edge_indexes
                        .iter()
                        .any(|edge| banned_edges.contains(edge))
                });
            let origin_seeds = origin_edge_seeds(routing_graph, origin)
                .into_iter()
                .filter(|(edge, _)| !banned_edges.contains(edge))
                .collect::<Vec<_>>();
            let destination_seeds = destination_edge_seeds(routing_graph, destination)
                .into_iter()
                .filter(|(edge, _)| !banned_edges.contains(edge))
                .collect::<Vec<_>>();
            let Some(path) = seeded_route_with_banned_edges(
                topology,
                metrics,
                routing_graph,
                &origin_seeds,
                &destination_seeds,
                direct_path,
                fallback,
                banned_edges,
                max_generalized_cost,
            )?
            else {
                continue;
            };
            let path =
                finalize_route_path(topology, metrics, path.edge_indexes, origin, destination);
            let replace = best.as_ref().is_none_or(|(_, _, best_path, _)| {
                path.total_generalized_cost < best_path.total_generalized_cost
            });
            if replace {
                best = Some((origin.clone(), destination.clone(), path, hop_info));
            }
        }
    }
    Ok(best)
}

fn seeded_route_with_banned_edges(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origin_seeds: &[(usize, f64)],
    destination_seeds: &[(usize, f64)],
    initial_upper_bound: Option<RoutePath>,
    fallback: &FallbackPolicy,
    banned_edges: &HashSet<usize>,
    max_generalized_cost: Option<f64>,
) -> Result<Option<RoutePath>> {
    if !has_failure_modes(fallback) {
        let candidate = seeded_bidirectional_dijkstra_with_banned_edges(
            topology,
            routing_graph,
            origin_seeds,
            destination_seeds,
            initial_upper_bound.clone(),
            banned_edges,
            max_generalized_cost,
        )?;
        if !routing_graph.has_restriction_sequences()
            || candidate.as_ref().is_some_and(|path| {
                path_respects_restriction_sequences(routing_graph, &path.edge_indexes)
            })
        {
            return Ok(candidate);
        }
    }

    seeded_forward_dijkstra_with_banned_edges(
        topology,
        metrics,
        routing_graph,
        origin_seeds,
        destination_seeds,
        initial_upper_bound,
        fallback,
        banned_edges,
        max_generalized_cost,
    )
}

fn seeded_bidirectional_dijkstra_with_banned_edges(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    origin_seeds: &[(usize, f64)],
    destination_seeds: &[(usize, f64)],
    initial_upper_bound: Option<RoutePath>,
    banned_edges: &HashSet<usize>,
    max_generalized_cost: Option<f64>,
) -> Result<Option<RoutePath>> {
    EDGE_SEARCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.prepare(topology.edge_count());

        for &(edge_index, cost) in origin_seeds {
            if banned_edges.contains(&edge_index) {
                continue;
            }
            if !scratch.update_forward(edge_index, cost, NO_PREVIOUS_EDGE) {
                continue;
            }
            scratch.forward_heap.push(State {
                edge_index,
                automaton_state: 0,
                cost,
                score: cost,
            });
        }
        for &(edge_index, cost) in destination_seeds {
            if banned_edges.contains(&edge_index) {
                continue;
            }
            if !scratch.update_backward(edge_index, cost, NO_PREVIOUS_EDGE) {
                continue;
            }
            scratch.backward_heap.push(State {
                edge_index,
                automaton_state: 0,
                cost,
                score: cost,
            });
        }

        let mut best_path = initial_upper_bound.filter(|path| {
            max_generalized_cost.is_none_or(|limit| path.total_generalized_cost <= limit)
        });
        let mut best_cost = best_path
            .as_ref()
            .map(|path| path.total_generalized_cost)
            .unwrap_or_else(|| max_generalized_cost.unwrap_or(f64::INFINITY));
        let mut best_edge = None;

        while !(scratch.forward_heap.is_empty() || scratch.backward_heap.is_empty()) {
            let next_forward_cost = scratch
                .forward_heap
                .peek()
                .map(|state| state.cost)
                .unwrap_or(f64::INFINITY);
            let next_backward_cost = scratch
                .backward_heap
                .peek()
                .map(|state| state.cost)
                .unwrap_or(f64::INFINITY);
            if next_forward_cost + next_backward_cost >= best_cost {
                break;
            }

            if next_forward_cost <= next_backward_cost {
                let Some(State {
                    edge_index,
                    automaton_state: _,
                    cost,
                    score: _,
                }) = scratch.forward_heap.pop()
                else {
                    break;
                };
                if banned_edges.contains(&edge_index) || cost > scratch.forward_dist[edge_index] {
                    continue;
                }
                if scratch.backward_dist[edge_index].is_finite() {
                    let candidate_cost = cost + scratch.backward_dist[edge_index];
                    if candidate_cost < best_cost {
                        best_cost = candidate_cost;
                        best_edge = Some(edge_index);
                    }
                }
                if cost > best_cost {
                    continue;
                }

                for transition_index in routing_graph.transition_range(edge_index) {
                    let next_edge = routing_graph.transition_edges[transition_index] as usize;
                    if banned_edges.contains(&next_edge) {
                        continue;
                    }
                    let next_cost = cost + routing_graph.transition_costs[transition_index];
                    if next_cost >= best_cost
                        || !scratch.update_forward(next_edge, next_cost, edge_index as u32)
                    {
                        continue;
                    }
                    scratch.forward_heap.push(State {
                        edge_index: next_edge,
                        automaton_state: 0,
                        cost: next_cost,
                        score: next_cost,
                    });
                }
            } else {
                let Some(State {
                    edge_index,
                    automaton_state: _,
                    cost,
                    score: _,
                }) = scratch.backward_heap.pop()
                else {
                    break;
                };
                if banned_edges.contains(&edge_index) || cost > scratch.backward_dist[edge_index] {
                    continue;
                }
                if scratch.forward_dist[edge_index].is_finite() {
                    let candidate_cost = scratch.forward_dist[edge_index] + cost;
                    if candidate_cost < best_cost {
                        best_cost = candidate_cost;
                        best_edge = Some(edge_index);
                    }
                }
                if cost > best_cost {
                    continue;
                }

                for transition_index in routing_graph.reverse_transition_range(edge_index) {
                    let previous_edge =
                        routing_graph.reverse_transition_edges[transition_index] as usize;
                    if banned_edges.contains(&previous_edge) {
                        continue;
                    }
                    let next_cost = cost + routing_graph.reverse_transition_costs[transition_index];
                    if next_cost >= best_cost
                        || !scratch.update_backward(previous_edge, next_cost, edge_index as u32)
                    {
                        continue;
                    }
                    scratch.backward_heap.push(State {
                        edge_index: previous_edge,
                        automaton_state: 0,
                        cost: next_cost,
                        score: next_cost,
                    });
                }
            }
        }

        if let Some(meeting_edge) = best_edge {
            best_path = Some(reconstruct_bidirectional_route_path(
                &scratch,
                meeting_edge,
                best_cost,
            ));
        }

        Ok(best_path)
    })
}

fn seeded_forward_dijkstra_with_banned_edges(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origin_seeds: &[(usize, f64)],
    destination_seeds: &[(usize, f64)],
    initial_upper_bound: Option<RoutePath>,
    fallback: &FallbackPolicy,
    banned_edges: &HashSet<usize>,
    max_generalized_cost: Option<f64>,
) -> Result<Option<RoutePath>> {
    let mut heap = BinaryHeap::new();
    let mut dist = HashMap::<SearchStateKey, f64>::new();
    let mut previous = HashMap::<SearchStateKey, Option<SearchStateKey>>::new();
    let mut destination_adjustments = HashMap::<usize, f64>::new();
    for &(edge_index, adjustment) in destination_seeds {
        destination_adjustments
            .entry(edge_index)
            .and_modify(|existing| *existing = existing.min(adjustment))
            .or_insert(adjustment);
    }

    let mut best_path = initial_upper_bound.filter(|path| {
        max_generalized_cost.is_none_or(|limit| path.total_generalized_cost <= limit)
    });
    let mut best_cost = best_path
        .as_ref()
        .map(|path| path.total_generalized_cost)
        .unwrap_or_else(|| max_generalized_cost.unwrap_or(f64::INFINITY));
    let mut best_state = None;

    for &(edge_index, cost) in origin_seeds {
        if banned_edges.contains(&edge_index) {
            continue;
        }
        let (_, edge_penalty_cost) =
            edge_failure_mode_penalty(metrics, routing_graph, fallback, edge_index);
        let seeded_cost = cost + edge_penalty_cost;
        if !seeded_cost.is_finite() {
            continue;
        }
        let automaton_state = routing_graph.automaton.transition(0, edge_index);
        let key = SearchStateKey {
            edge_index,
            automaton_state,
        };
        dist.insert(key, seeded_cost);
        previous.insert(key, None);
        heap.push(State {
            edge_index,
            automaton_state,
            cost: seeded_cost,
            score: seeded_cost,
        });
    }

    while let Some(State {
        edge_index,
        automaton_state,
        cost,
        score: _,
    }) = heap.pop()
    {
        let key = SearchStateKey {
            edge_index,
            automaton_state,
        };
        if cost > *dist.get(&key).unwrap_or(&f64::INFINITY) {
            continue;
        }
        if let Some(&adjustment) = destination_adjustments.get(&edge_index) {
            let candidate_cost = cost + adjustment;
            if candidate_cost < best_cost {
                best_cost = candidate_cost;
                best_state = Some(key);
            }
        }
        if cost >= best_cost {
            continue;
        }
        for transition_index in routing_graph.transition_range(edge_index) {
            let next_edge = routing_graph.transition_edges[transition_index] as usize;
            if banned_edges.contains(&next_edge) {
                continue;
            }
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
            let next_cost = cost + routing_graph.transition_costs[transition_index] + penalty_cost;
            let next_automaton_state = routing_graph
                .automaton
                .transition(automaton_state, next_edge);
            let next_key = SearchStateKey {
                edge_index: next_edge,
                automaton_state: next_automaton_state,
            };
            if next_cost + f64::EPSILON < *dist.get(&next_key).unwrap_or(&f64::INFINITY) {
                dist.insert(next_key, next_cost);
                previous.insert(next_key, Some(key));
                heap.push(State {
                    edge_index: next_edge,
                    automaton_state: next_automaton_state,
                    cost: next_cost,
                    score: next_cost,
                });
            }
        }
    }

    let Some(mut cursor) = best_state else {
        return Ok(best_path);
    };
    let mut edge_indexes = Vec::new();
    loop {
        edge_indexes.push(cursor.edge_index);
        let previous_state = previous.get(&cursor).copied().flatten();
        let Some(previous_state) = previous_state else {
            break;
        };
        cursor = previous_state;
    }
    edge_indexes.reverse();
    best_path = Some(RoutePath {
        edge_indexes,
        total_generalized_cost: best_cost,
    });
    Ok(best_path)
}

pub(crate) fn same_edge_reverse_pair(origin: &SnappedPoint, destination: &SnappedPoint) -> bool {
    matches!(
        (
            origin.snapped_edge_id,
            origin.snapped_edge_fraction,
            destination.snapped_edge_id,
            destination.snapped_edge_fraction
        ),
        (Some(origin_edge), Some(origin_fraction), Some(destination_edge), Some(destination_fraction))
            if origin_edge == destination_edge && origin_fraction > destination_fraction
    )
}

pub(crate) fn direct_same_edge_path(
    routing_graph: &RoutingGraph,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> Option<RoutePath> {
    let (Some(edge_id), Some(origin_fraction), Some(destination_fraction)) = (
        origin.snapped_edge_id,
        origin.snapped_edge_fraction,
        destination.snapped_edge_fraction,
    ) else {
        if origin.snapped_edge_id.is_none()
            && destination.snapped_edge_id.is_none()
            && origin.snapped_node_id == destination.snapped_node_id
        {
            return Some(RoutePath {
                edge_indexes: Vec::new(),
                total_generalized_cost: 0.0,
            });
        }
        return None;
    };
    if destination.snapped_edge_id != Some(edge_id) || origin_fraction > destination_fraction {
        return None;
    }
    let edge_cost = routing_graph.edge_costs[edge_id as usize];
    if !edge_cost.is_finite() {
        return None;
    }
    Some(RoutePath {
        edge_indexes: vec![edge_id as usize],
        total_generalized_cost: edge_cost * (destination_fraction - origin_fraction),
    })
}

pub(crate) fn path_respects_restriction_sequences(
    routing_graph: &RoutingGraph,
    edge_indexes: &[usize],
) -> bool {
    if !routing_graph.has_restriction_sequences() {
        return true;
    }

    let mut automaton_state = 0_usize;
    for &edge_index in edge_indexes {
        if routing_graph
            .automaton
            .prohibited_sequence_len(automaton_state, edge_index)
            .is_some()
        {
            return false;
        }
        automaton_state = routing_graph
            .automaton
            .transition(automaton_state, edge_index);
    }

    true
}

pub(crate) fn origin_edge_seeds(
    routing_graph: &RoutingGraph,
    origin: &SnappedPoint,
) -> Vec<(usize, f64)> {
    if let (Some(edge_id), Some(fraction)) = (origin.snapped_edge_id, origin.snapped_edge_fraction)
    {
        let edge_cost = routing_graph.edge_costs[edge_id as usize];
        if edge_cost.is_finite() {
            return vec![(edge_id as usize, edge_cost * (1.0 - fraction))];
        }
        return Vec::new();
    }
    routing_graph
        .outgoing_edges(origin.snapped_node_id as usize)
        .iter()
        .filter_map(|&edge_index| {
            let edge_cost = routing_graph.edge_costs[edge_index as usize];
            edge_cost
                .is_finite()
                .then_some((edge_index as usize, edge_cost))
        })
        .collect()
}

pub(crate) fn destination_edge_seeds(
    routing_graph: &RoutingGraph,
    destination: &SnappedPoint,
) -> Vec<(usize, f64)> {
    if let (Some(edge_id), Some(fraction)) = (
        destination.snapped_edge_id,
        destination.snapped_edge_fraction,
    ) {
        let edge_cost = routing_graph.edge_costs[edge_id as usize];
        if edge_cost.is_finite() {
            return vec![(edge_id as usize, edge_cost * fraction - edge_cost)];
        }
        return Vec::new();
    }
    routing_graph
        .incoming_edges(destination.snapped_node_id as usize)
        .iter()
        .map(|&edge_index| (edge_index as usize, 0.0))
        .collect()
}

pub(crate) fn reconstruct_bidirectional_route_path(
    scratch: &BidirectionalEdgeSearchScratch,
    meeting_edge: usize,
    total_generalized_cost: f64,
) -> RoutePath {
    let mut cursor = meeting_edge;
    let mut edge_indexes = Vec::new();
    loop {
        edge_indexes.push(cursor);
        let previous_edge = scratch.forward_previous[cursor];
        if previous_edge == NO_PREVIOUS_EDGE {
            break;
        }
        cursor = previous_edge as usize;
    }
    edge_indexes.reverse();

    let mut cursor = meeting_edge;
    loop {
        let next_edge = scratch.backward_next[cursor];
        if next_edge == NO_PREVIOUS_EDGE {
            break;
        }
        edge_indexes.push(next_edge as usize);
        cursor = next_edge as usize;
    }

    RoutePath {
        edge_indexes,
        total_generalized_cost,
    }
}
