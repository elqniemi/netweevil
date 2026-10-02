//! Point-to-point CCH query used by single routes and waypoint legs.
//!
//! The search state is laid out for cache locality: one 24-byte label per
//! edge state holds both directions' tentative costs and parent arcs, so a
//! relaxation or meeting check touches a single cache line, and both queues
//! are 4-ary [`StateHeap`]s of 16-byte entries.

use std::cell::RefCell;

use anyhow::Result;
use netweevil_core::{TopologyBundle, decode_cch_weight};

use crate::*;

thread_local! {
    static PAIRWISE_CCH_SCRATCH: RefCell<PairwiseCchScratch> =
        RefCell::new(PairwiseCchScratch::default());
}

#[derive(Clone, Copy)]
struct CchLabel {
    forward: f64,
    backward: f64,
    forward_arc: u32,
    backward_arc: u32,
}

impl CchLabel {
    const EMPTY: Self = Self {
        forward: f64::INFINITY,
        backward: f64::INFINITY,
        forward_arc: NO_PREVIOUS_ARC,
        backward_arc: NO_PREVIOUS_ARC,
    };
}

#[derive(Default)]
struct PairwiseCchScratch {
    labels: Vec<CchLabel>,
    touched: Vec<u32>,
    forward_heap: StateHeap,
    backward_heap: StateHeap,
    forward_arcs: Vec<u32>,
    unpack_stack: Vec<(bool, u32)>,
}

impl PairwiseCchScratch {
    fn prepare(&mut self, edge_count: usize) {
        if self.labels.len() < edge_count {
            self.labels.resize(edge_count, CchLabel::EMPTY);
        }
        for &edge_index in &self.touched {
            self.labels[edge_index as usize] = CchLabel::EMPTY;
        }
        self.touched.clear();
        self.forward_heap.clear();
        self.backward_heap.clear();
    }

    fn update_forward(&mut self, edge_index: usize, cost: f64, previous_arc: u32) -> bool {
        if !cost.is_finite() {
            return false;
        }
        let label = &mut self.labels[edge_index];
        if cost + f64::EPSILON >= label.forward {
            return false;
        }
        if label.forward == f64::INFINITY && label.backward == f64::INFINITY {
            self.touched.push(edge_index as u32);
        }
        label.forward = cost;
        label.forward_arc = previous_arc;
        true
    }

    fn update_backward(&mut self, edge_index: usize, cost: f64, next_arc: u32) -> bool {
        if !cost.is_finite() {
            return false;
        }
        let label = &mut self.labels[edge_index];
        if cost + f64::EPSILON >= label.backward {
            return false;
        }
        if label.forward == f64::INFINITY && label.backward == f64::INFINITY {
            self.touched.push(edge_index as u32);
        }
        label.backward = cost;
        label.backward_arc = next_arc;
        true
    }
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
    let compiled = acceleration
        .metrics
        .acceleration
        .as_ref()
        .expect("validated acceleration weights");
    let source = acceleration.source.as_ref();

    // Destination phantoms subtract the unused part of the final edge. A
    // queue minimum alone is therefore not a lower bound on a complete path.
    let minimum_origin_cost = minimum_seed_cost(origin_seeds);
    let minimum_destination_cost = minimum_seed_cost(destination_seeds);

    PAIRWISE_CCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        let scratch = &mut *scratch;
        scratch.prepare(topology.edge_count());

        for &(edge_index, cost) in origin_seeds {
            if scratch.update_forward(edge_index, cost, NO_PREVIOUS_ARC) {
                scratch.forward_heap.push(edge_index, 0, cost);
            }
        }
        for &(edge_index, cost) in destination_seeds {
            if scratch.update_backward(edge_index, cost, NO_PREVIOUS_ARC) {
                scratch.backward_heap.push(edge_index, 0, cost);
            }
        }

        let mut best_cost = initial_path
            .as_ref()
            .map(|path| path.total_generalized_cost)
            .unwrap_or(f64::INFINITY);
        let mut best_edge = None;

        loop {
            let next_forward_cost = scratch.forward_heap.peek_cost();
            let next_backward_cost = scratch.backward_heap.peek_cost();
            let forward_active = next_forward_cost + minimum_destination_cost < best_cost;
            let backward_active = next_backward_cost + minimum_origin_cost < best_cost;
            if !forward_active && !backward_active {
                break;
            }

            if forward_active && (next_forward_cost <= next_backward_cost || !backward_active) {
                let Some((edge_index, _, cost)) = scratch.forward_heap.pop() else {
                    break;
                };
                let label = scratch.labels[edge_index];
                if cost > label.forward {
                    continue;
                }
                if label.backward.is_finite() {
                    let candidate_cost = cost + label.backward;
                    if candidate_cost < best_cost {
                        best_cost = candidate_cost;
                        best_edge = Some(edge_index);
                    }
                }

                for slot in source.upward_first_out[edge_index] as usize
                    ..source.upward_first_out[edge_index + 1] as usize
                {
                    let next_edge = source.upward_head[slot] as usize;
                    let next_cost = cost + decode_cch_weight(compiled.upward_weight[slot]);
                    if scratch.update_forward(next_edge, next_cost, slot as u32) {
                        scratch.forward_heap.push(next_edge, 0, next_cost);
                    }
                }
            } else {
                let Some((edge_index, _, cost)) = scratch.backward_heap.pop() else {
                    break;
                };
                let label = scratch.labels[edge_index];
                if cost > label.backward {
                    continue;
                }
                if label.forward.is_finite() {
                    let candidate_cost = label.forward + cost;
                    if candidate_cost < best_cost {
                        best_cost = candidate_cost;
                        best_edge = Some(edge_index);
                    }
                }

                for slot in acceleration.reverse_downward_first_out[edge_index] as usize
                    ..acceleration.reverse_downward_first_out[edge_index + 1] as usize
                {
                    let previous_edge = acceleration.reverse_downward_edge[slot] as usize;
                    let arc = acceleration.reverse_downward_arc[slot];
                    let next_cost =
                        cost + decode_cch_weight(compiled.downward_weight[arc as usize]);
                    if scratch.update_backward(previous_edge, next_cost, arc) {
                        scratch.backward_heap.push(previous_edge, 0, next_cost);
                    }
                }
            }
        }

        let Some(meeting_edge) = best_edge else {
            return Ok(initial_path);
        };
        Ok(Some(reconstruct_pairwise_path(
            acceleration,
            scratch,
            meeting_edge,
            best_cost,
        )))
    })
}

fn reconstruct_pairwise_path(
    acceleration: &AccelerationGraph,
    scratch: &mut PairwiseCchScratch,
    meeting_edge: usize,
    total_generalized_cost: f64,
) -> RoutePath {
    let mut forward_arcs = std::mem::take(&mut scratch.forward_arcs);
    forward_arcs.clear();
    let mut cursor = meeting_edge;
    loop {
        let previous_arc = scratch.labels[cursor].forward_arc;
        if previous_arc == NO_PREVIOUS_ARC {
            break;
        }
        forward_arcs.push(previous_arc);
        cursor = acceleration.upward_tail[previous_arc as usize] as usize;
    }
    let mut edge_indexes = vec![cursor];
    let mut unpack_stack = std::mem::take(&mut scratch.unpack_stack);
    for &arc_slot in forward_arcs.iter().rev() {
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
        let next_arc = scratch.labels[cursor].backward_arc;
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
    scratch.forward_arcs = forward_arcs;

    RoutePath {
        edge_indexes,
        total_generalized_cost,
    }
}
