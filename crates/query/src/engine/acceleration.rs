use std::collections::BinaryHeap;

use anyhow::Result;
#[cfg(test)]
use anyhow::bail;
use netweevil_core::{NO_MIDDLE, TopologyBundle};

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

/// Expands one hierarchy arc to the base edge states it covers, excluding the
/// arc's tail and including its head, recursing through customized middle
/// pointers. Both child arcs of a middle `m` exist by contraction
/// completeness: `tail -> m` is a downward arc of `tail`, `m -> head` an
/// upward arc of `m`.
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
        let (tail, head, middle) = if is_upward {
            (
                acceleration.upward_tail[slot_index] as usize,
                acceleration.source.upward_head[slot_index],
                compiled.upward_middle[slot_index],
            )
        } else {
            (
                acceleration.downward_tail[slot_index] as usize,
                acceleration.source.downward_head[slot_index],
                compiled.downward_middle[slot_index],
            )
        };
        if middle == NO_MIDDLE {
            edge_indexes.push(head as usize);
            continue;
        }
        let left = acceleration
            .downward_arc_slot(tail, middle)
            .expect("customized middle implies a downward arc tail -> middle");
        let right = acceleration
            .upward_arc_slot(middle as usize, head)
            .expect("customized middle implies an upward arc middle -> head");
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

/// Exact point-to-point query over the customized CCH.
///
/// Forward search relaxes upward arcs from the origin seeds; backward search
/// relaxes downward arcs toward the destination seeds. Each direction runs
/// until its queue minimum reaches the best known up-down cost — the sum rule
/// used by same-graph bidirectional Dijkstra is NOT sound here because the
/// two directions explore disjoint arc sets, so a meeting state is only ever
/// labeled by the side that reaches it.
///
/// With the complete contraction this result is authoritative: no follow-up
/// search on the base graph is required.
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
            let forward_active = next_forward_cost < best_cost;
            let backward_active = next_backward_cost < best_cost;
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
                    let next_cost = cost + compiled.upward_weight[slot];
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
                        + compiled.downward_weight
                            [acceleration.reverse_downward_arc[slot] as usize];
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

pub(crate) fn seeded_bidirectional_dijkstra_on_edge_transitions(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    origin_seeds: &[(usize, f64)],
    destination_seeds: &[(usize, f64)],
    initial_upper_bound: Option<RoutePath>,
) -> Result<Option<RoutePath>> {
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
            if best_path.is_some() && next_forward_cost + next_backward_cost >= best_cost {
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
                if cost > best_cost {
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
                if cost > best_cost {
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
