use std::collections::BinaryHeap;

use anyhow::Result;
#[cfg(test)]
use anyhow::bail;
use netweevil_core::TopologyBundle;

use crate::*;

#[derive(Default)]
pub(crate) struct BidirectionalAccelerationScratch {
    forward_dist: Vec<f64>,
    forward_previous_arc: Vec<u32>,
    forward_touched: Vec<u32>,
    forward_heap: BinaryHeap<State>,
    backward_dist: Vec<f64>,
    backward_next_arc: Vec<u32>,
    backward_touched: Vec<u32>,
    backward_heap: BinaryHeap<State>,
    unpack_stack: Vec<(bool, u32)>,
}

impl BidirectionalAccelerationScratch {
    fn prepare(&mut self, edge_count: usize) {
        if self.forward_dist.len() < edge_count {
            self.forward_dist.resize(edge_count, f64::INFINITY);
            self.forward_previous_arc
                .resize(edge_count, NO_PREVIOUS_ARC);
            self.backward_dist.resize(edge_count, f64::INFINITY);
            self.backward_next_arc.resize(edge_count, NO_PREVIOUS_ARC);
        }
        for &edge_index in &self.forward_touched {
            self.forward_dist[edge_index as usize] = f64::INFINITY;
            self.forward_previous_arc[edge_index as usize] = NO_PREVIOUS_ARC;
        }
        for &edge_index in &self.backward_touched {
            self.backward_dist[edge_index as usize] = f64::INFINITY;
            self.backward_next_arc[edge_index as usize] = NO_PREVIOUS_ARC;
        }
        self.forward_touched.clear();
        self.forward_heap.clear();
        self.backward_touched.clear();
        self.backward_heap.clear();
    }

    fn update_forward(&mut self, edge_index: usize, cost: f64, previous_arc: u32) -> bool {
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
        self.forward_previous_arc[edge_index] = previous_arc;
        true
    }

    fn update_backward(&mut self, edge_index: usize, cost: f64, next_arc: u32) -> bool {
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
        self.backward_next_arc[edge_index] = next_arc;
        true
    }
}

/// Unpacks arcs by finding a lower triangle with the same integer weight.
/// Child endpoints have lower rank, so expansion terminates even with zero costs.
fn push_unpacked_arc(
    acceleration: &AccelerationGraph,
    stack: &mut Vec<(bool, u32)>,
    is_upward: bool,
    arc_slot: u32,
    edge_indexes: &mut Vec<usize>,
) {
    let compiled = acceleration
        .metrics
        .acceleration
        .as_ref()
        .expect("validated acceleration weights");
    debug_assert!(stack.is_empty());
    stack.push((is_upward, arc_slot));
    while let Some((is_upward, slot)) = stack.pop() {
        let slot_index = slot as usize;
        let (tail, head, weight) = if is_upward {
            (
                acceleration.upward_tail[slot_index] as usize,
                acceleration.source.upward_head[slot_index],
                compiled.upward_weight[slot_index],
            )
        } else {
            (
                acceleration.downward_tail[slot_index] as usize,
                acceleration.source.downward_head[slot_index],
                compiled.downward_weight[slot_index],
            )
        };
        assert!(
            weight < netweevil_core::CCH_WEIGHT_OVERFLOW,
            "only finite CCH arcs can be unpacked"
        );
        let source = &acceleration.source;
        let triangle = (source.downward_first_out[tail] as usize
            ..source.downward_first_out[tail + 1] as usize)
            .find_map(|left| {
                let middle = source.downward_head[left] as usize;
                if source.edge_rank[middle] >= source.edge_rank[head as usize] {
                    return None;
                }
                let right = acceleration.upward_arc_slot(middle, head)?;
                (netweevil_core::add_cch_weights(
                    compiled.downward_weight[left],
                    compiled.upward_weight[right],
                ) == weight)
                    .then_some((left, right))
            });
        let Some((left, right)) = triangle else {
            edge_indexes.push(head as usize);
            continue;
        };
        // Process left before right: LIFO stack, so push right first.
        stack.push((true, right as u32));
        stack.push((false, left as u32));
    }
}

fn reconstruct_accelerated_route_path(
    acceleration: &AccelerationGraph,
    scratch: &mut BidirectionalAccelerationScratch,
    meeting_edge: usize,
    total_generalized_cost: f64,
) -> RoutePath {
    let mut edge_indexes = Vec::new();
    let mut forward_arc_ids = Vec::new();
    let mut cursor = meeting_edge;
    loop {
        let previous_arc = scratch.forward_previous_arc[cursor];
        if previous_arc == NO_PREVIOUS_ARC {
            break;
        }
        forward_arc_ids.push(previous_arc);
        cursor = acceleration.upward_tail[previous_arc as usize] as usize;
    }
    edge_indexes.push(cursor);
    let mut unpack_stack = std::mem::take(&mut scratch.unpack_stack);
    for arc_slot in forward_arc_ids.into_iter().rev() {
        push_unpacked_arc(
            acceleration,
            &mut unpack_stack,
            true,
            arc_slot,
            &mut edge_indexes,
        );
    }

    let mut cursor = meeting_edge;
    loop {
        let next_arc = scratch.backward_next_arc[cursor];
        if next_arc == NO_PREVIOUS_ARC {
            break;
        }
        push_unpacked_arc(
            acceleration,
            &mut unpack_stack,
            false,
            next_arc,
            &mut edge_indexes,
        );
        cursor = acceleration.source.downward_head[next_arc as usize] as usize;
    }
    scratch.unpack_stack = unpack_stack;

    RoutePath {
        edge_indexes,
        total_generalized_cost,
    }
}

#[cfg(test)]
pub(crate) fn accelerated_route_query(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    source: usize,
    target: usize,
) -> Result<Option<RoutePath>> {
    if source >= topology.nodes.len() || target >= topology.nodes.len() {
        bail!("source or target node is out of bounds for the topology bundle");
    }
    let origin_seeds = routing_graph
        .outgoing_edges(source)
        .iter()
        .filter_map(|&edge_index| {
            let edge_cost = routing_graph.edge_costs[edge_index as usize];
            edge_cost
                .is_finite()
                .then_some((edge_index as usize, edge_cost))
        })
        .collect::<Vec<_>>();
    let destination_seeds = routing_graph
        .incoming_edges(target)
        .iter()
        .map(|&edge_index| (edge_index as usize, 0.0))
        .collect::<Vec<_>>();
    let initial_path = (source == target).then_some(RoutePath {
        edge_indexes: Vec::new(),
        total_generalized_cost: 0.0,
    });
    accelerated_route_query_seeded(
        topology,
        routing_graph,
        &origin_seeds,
        &destination_seeds,
        initial_path,
    )
}

/// Point-to-point query that minimizes the quantized CCH metric.
///
/// Forward search relaxes upward arcs from the origin seeds; backward search
/// relaxes downward arcs toward the destination seeds. Each direction runs
/// until its queue minimum plus the opposite seed lower bound reaches the
/// best known up-down cost. The sum rule
/// used by same-graph bidirectional Dijkstra is NOT sound here because the
/// two directions explore disjoint arc sets, so a meeting state is only ever
/// labeled by the side that reaches it.
///
/// Complete contraction preserves the optimum of the quantized metric.
/// No follow-up search optimizes the original floating-point metric.
pub(crate) fn accelerated_route_query_seeded(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    origin_seeds: &[(usize, f64)],
    destination_seeds: &[(usize, f64)],
    initial_path: Option<RoutePath>,
) -> Result<Option<RoutePath>> {
    let Some(acceleration) = routing_graph.acceleration.as_ref() else {
        return Ok(None);
    };

    // Destination phantoms subtract the unused part of the final edge. A
    // queue minimum alone is therefore not a lower bound on a complete path.
    let minimum_origin_cost = minimum_seed_cost(origin_seeds);
    let minimum_destination_cost = minimum_seed_cost(destination_seeds);

    ACCELERATION_SEARCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.prepare(topology.edge_count());

        for &(edge_index, cost) in origin_seeds {
            if !scratch.update_forward(edge_index, cost, NO_PREVIOUS_ARC) {
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
            if !scratch.update_backward(edge_index, cost, NO_PREVIOUS_ARC) {
                continue;
            }
            scratch.backward_heap.push(State {
                edge_index,
                automaton_state: 0,
                cost,
                score: cost,
            });
        }

        let mut best_path = initial_path;
        let mut best_cost = best_path
            .as_ref()
            .map(|path| path.total_generalized_cost)
            .unwrap_or(f64::INFINITY);
        let mut best_edge = None;

        let compiled = acceleration
            .metrics
            .acceleration
            .as_ref()
            .expect("validated acceleration weights");

        loop {
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
            let forward_active = next_forward_cost + minimum_destination_cost < best_cost;
            let backward_active = next_backward_cost + minimum_origin_cost < best_cost;
            if !forward_active && !backward_active {
                break;
            }

            if forward_active && (next_forward_cost <= next_backward_cost || !backward_active) {
                let Some(State {
                    edge_index, cost, ..
                }) = scratch.forward_heap.pop()
                else {
                    break;
                };
                if cost > scratch.forward_dist[edge_index] {
                    continue;
                }
                if scratch.backward_dist[edge_index].is_finite() {
                    let candidate_cost = cost + scratch.backward_dist[edge_index];
                    if candidate_cost < best_cost {
                        best_cost = candidate_cost;
                        best_edge = Some(edge_index);
                    }
                }

                for slot in acceleration.source.upward_first_out[edge_index] as usize
                    ..acceleration.source.upward_first_out[edge_index + 1] as usize
                {
                    let next_edge = acceleration.source.upward_head[slot] as usize;
                    let next_cost =
                        cost + netweevil_core::decode_cch_weight(compiled.upward_weight[slot]);
                    if !scratch.update_forward(next_edge, next_cost, slot as u32) {
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
                    edge_index, cost, ..
                }) = scratch.backward_heap.pop()
                else {
                    break;
                };
                if cost > scratch.backward_dist[edge_index] {
                    continue;
                }
                if scratch.forward_dist[edge_index].is_finite() {
                    let candidate_cost = scratch.forward_dist[edge_index] + cost;
                    if candidate_cost < best_cost {
                        best_cost = candidate_cost;
                        best_edge = Some(edge_index);
                    }
                }

                for slot in acceleration.reverse_downward_first_out[edge_index] as usize
                    ..acceleration.reverse_downward_first_out[edge_index + 1] as usize
                {
                    let previous_edge = acceleration.reverse_downward_edge[slot] as usize;
                    let next_cost = cost
                        + netweevil_core::decode_cch_weight(
                            compiled.downward_weight
                                [acceleration.reverse_downward_arc[slot] as usize],
                        );
                    if !scratch.update_backward(
                        previous_edge,
                        next_cost,
                        acceleration.reverse_downward_arc[slot],
                    ) {
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

        let Some(meeting_edge) = best_edge else {
            return Ok(best_path);
        };

        best_path = Some(reconstruct_accelerated_route_path(
            acceleration,
            &mut scratch,
            meeting_edge,
            best_cost,
        ));

        Ok(best_path)
    })
}

fn minimum_seed_cost(seeds: &[(usize, f64)]) -> f64 {
    seeds
        .iter()
        .map(|&(_, cost)| cost)
        .filter(|cost| cost.is_finite())
        .fold(0.0, f64::min)
}

/// One settled hierarchy state of a full CCH search space.
#[derive(Debug, Clone, Copy)]
struct CchSpaceEntry {
    edge_index: u32,
    cost: f64,
    arc: u32,
}

/// Complete (unpruned) upward search space from a set of seeds, sorted by
/// edge index. Forward spaces store the predecessor upward arc per state;
/// backward spaces store the successor downward arc. Because both spaces are
/// complete, `min over shared states of (forward + backward)` equals the
/// shortest-path cost under the quantized metric, as in the pairwise CCH
/// query. The spaces are shared across origin/destination combinations.
#[derive(Debug)]
pub(crate) struct CchSearchSpace {
    entries: Vec<CchSpaceEntry>,
}

impl CchSearchSpace {
    fn lookup(&self, edge_index: u32) -> Option<(f64, u32)> {
        self.entries
            .binary_search_by_key(&edge_index, |entry| entry.edge_index)
            .ok()
            .map(|position| (self.entries[position].cost, self.entries[position].arc))
    }
}

pub(crate) fn build_forward_cch_space(
    routing_graph: &RoutingGraph,
    seeds: &[(usize, f64)],
) -> CchSearchSpace {
    let acceleration = routing_graph
        .acceleration
        .as_ref()
        .expect("many-to-many CCH space requires acceleration");
    let compiled = acceleration
        .metrics
        .acceleration
        .as_ref()
        .expect("validated acceleration weights");

    ACCELERATION_SEARCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.prepare(routing_graph.edge_costs.len());

        for &(edge_index, cost) in seeds {
            if !scratch.update_forward(edge_index, cost, NO_PREVIOUS_ARC) {
                continue;
            }
            scratch.forward_heap.push(State {
                edge_index,
                automaton_state: 0,
                cost,
                score: cost,
            });
        }

        while let Some(State {
            edge_index, cost, ..
        }) = scratch.forward_heap.pop()
        {
            if cost > scratch.forward_dist[edge_index] {
                continue;
            }
            for slot in acceleration.source.upward_first_out[edge_index] as usize
                ..acceleration.source.upward_first_out[edge_index + 1] as usize
            {
                let next_edge = acceleration.source.upward_head[slot] as usize;
                let next_cost =
                    cost + netweevil_core::decode_cch_weight(compiled.upward_weight[slot]);
                if !scratch.update_forward(next_edge, next_cost, slot as u32) {
                    continue;
                }
                scratch.forward_heap.push(State {
                    edge_index: next_edge,
                    automaton_state: 0,
                    cost: next_cost,
                    score: next_cost,
                });
            }
        }

        let mut entries = scratch
            .forward_touched
            .iter()
            .map(|&edge_index| CchSpaceEntry {
                edge_index,
                cost: scratch.forward_dist[edge_index as usize],
                arc: scratch.forward_previous_arc[edge_index as usize],
            })
            .collect::<Vec<_>>();
        entries.sort_unstable_by_key(|entry| entry.edge_index);
        CchSearchSpace { entries }
    })
}

pub(crate) fn build_backward_cch_space(
    routing_graph: &RoutingGraph,
    seeds: &[(usize, f64)],
) -> CchSearchSpace {
    let acceleration = routing_graph
        .acceleration
        .as_ref()
        .expect("many-to-many CCH space requires acceleration");
    let compiled = acceleration
        .metrics
        .acceleration
        .as_ref()
        .expect("validated acceleration weights");

    ACCELERATION_SEARCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.prepare(routing_graph.edge_costs.len());

        for &(edge_index, cost) in seeds {
            if !scratch.update_backward(edge_index, cost, NO_PREVIOUS_ARC) {
                continue;
            }
            scratch.backward_heap.push(State {
                edge_index,
                automaton_state: 0,
                cost,
                score: cost,
            });
        }

        while let Some(State {
            edge_index, cost, ..
        }) = scratch.backward_heap.pop()
        {
            if cost > scratch.backward_dist[edge_index] {
                continue;
            }
            for slot in acceleration.reverse_downward_first_out[edge_index] as usize
                ..acceleration.reverse_downward_first_out[edge_index + 1] as usize
            {
                let previous_edge = acceleration.reverse_downward_edge[slot] as usize;
                let next_cost = cost
                    + netweevil_core::decode_cch_weight(
                        compiled.downward_weight[acceleration.reverse_downward_arc[slot] as usize],
                    );
                if !scratch.update_backward(
                    previous_edge,
                    next_cost,
                    acceleration.reverse_downward_arc[slot],
                ) {
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

        let mut entries = scratch
            .backward_touched
            .iter()
            .map(|&edge_index| CchSpaceEntry {
                edge_index,
                cost: scratch.backward_dist[edge_index as usize],
                arc: scratch.backward_next_arc[edge_index as usize],
            })
            .collect::<Vec<_>>();
        entries.sort_unstable_by_key(|entry| entry.edge_index);
        CchSearchSpace { entries }
    })
}

/// Exact minimum up-down cost over the shared states of two complete search
/// spaces, via a linear merge of the edge-sorted entry lists.
pub(crate) fn join_cch_spaces(
    forward: &CchSearchSpace,
    backward: &CchSearchSpace,
) -> Option<(f64, u32)> {
    let mut best: Option<(f64, u32)> = None;
    let (mut i, mut j) = (0_usize, 0_usize);
    while i < forward.entries.len() && j < backward.entries.len() {
        let left = &forward.entries[i];
        let right = &backward.entries[j];
        match left.edge_index.cmp(&right.edge_index) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                let cost = left.cost + right.cost;
                if best.is_none_or(|(known, _)| cost < known) {
                    best = Some((cost, left.edge_index));
                }
                i += 1;
                j += 1;
            }
        }
    }
    best
}

/// Unpacks the up-down path through `meeting_edge` from two stored search
/// spaces, mirroring `reconstruct_accelerated_route_path` on the pairwise
/// scratch.
pub(crate) fn reconstruct_cch_space_path(
    routing_graph: &RoutingGraph,
    forward: &CchSearchSpace,
    backward: &CchSearchSpace,
    meeting_edge: u32,
    total_generalized_cost: f64,
) -> RoutePath {
    let acceleration = routing_graph
        .acceleration
        .as_ref()
        .expect("many-to-many CCH space requires acceleration");

    let mut forward_arc_ids = Vec::new();
    let mut cursor = meeting_edge;
    loop {
        let (_, arc) = forward
            .lookup(cursor)
            .expect("meeting edge chain settled in forward space");
        if arc == NO_PREVIOUS_ARC {
            break;
        }
        forward_arc_ids.push(arc);
        cursor = acceleration.upward_tail[arc as usize];
    }

    let mut edge_indexes = vec![cursor as usize];
    let mut unpack_stack = Vec::new();
    for arc_slot in forward_arc_ids.into_iter().rev() {
        push_unpacked_arc(
            acceleration,
            &mut unpack_stack,
            true,
            arc_slot,
            &mut edge_indexes,
        );
    }

    let mut cursor = meeting_edge;
    loop {
        let (_, arc) = backward
            .lookup(cursor)
            .expect("meeting edge chain settled in backward space");
        if arc == NO_PREVIOUS_ARC {
            break;
        }
        push_unpacked_arc(
            acceleration,
            &mut unpack_stack,
            false,
            arc,
            &mut edge_indexes,
        );
        cursor = acceleration.source.downward_head[arc as usize];
    }

    RoutePath {
        edge_indexes,
        total_generalized_cost,
    }
}

pub(crate) fn seeded_bidirectional_dijkstra_on_edge_transitions(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    origin_seeds: &[(usize, f64)],
    destination_seeds: &[(usize, f64)],
    initial_upper_bound: Option<RoutePath>,
) -> Result<Option<RoutePath>> {
    let minimum_origin_cost = minimum_seed_cost(origin_seeds);
    let minimum_destination_cost = minimum_seed_cost(destination_seeds);
    EDGE_SEARCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.prepare(topology.edge_count());

        for &(edge_index, cost) in origin_seeds {
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

        let mut best_path = initial_upper_bound;
        let mut best_cost = best_path
            .as_ref()
            .map(|path| path.total_generalized_cost)
            .unwrap_or(f64::INFINITY);
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
                if cost > scratch.forward_dist[edge_index] {
                    continue;
                }
                if scratch.backward_dist[edge_index].is_finite() {
                    let candidate_cost = cost + scratch.backward_dist[edge_index];
                    if candidate_cost < best_cost {
                        best_cost = candidate_cost;
                        best_edge = Some(edge_index);
                    }
                }
                if cost + minimum_destination_cost > best_cost {
                    continue;
                }

                for transition_index in routing_graph.transition_range(edge_index) {
                    let next_edge = routing_graph.transition_edges[transition_index] as usize;
                    let next_cost = cost + routing_graph.transition_costs[transition_index];
                    if !scratch.update_forward(next_edge, next_cost, edge_index as u32) {
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
                if cost > scratch.backward_dist[edge_index] {
                    continue;
                }
                if scratch.forward_dist[edge_index].is_finite() {
                    let candidate_cost = scratch.forward_dist[edge_index] + cost;
                    if candidate_cost < best_cost {
                        best_cost = candidate_cost;
                        best_edge = Some(edge_index);
                    }
                }
                if cost + minimum_origin_cost > best_cost {
                    continue;
                }

                for transition_index in routing_graph.reverse_transition_range(edge_index) {
                    let previous_edge =
                        routing_graph.reverse_transition_edges[transition_index] as usize;
                    let next_cost = cost + routing_graph.reverse_transition_costs[transition_index];
                    if !scratch.update_backward(previous_edge, next_cost, edge_index as u32) {
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
