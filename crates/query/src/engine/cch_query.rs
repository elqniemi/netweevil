//! Point-to-point CCH query used by single routes and waypoint legs.
//!
//! The query runs on a copy of the hierarchy renumbered by contraction rank
//! ([`RankedCch`]). Both searches climb towards high ranks, and every query
//! shares the top of the hierarchy, so with rank-ordered rows and labels
//! most of the work hits the same few hot cache lines. One 24-byte label per
//! state holds both directions' costs and parents, and both queues are 4-ary
//! [`StateHeap`]s of 16-byte entries.

use std::cell::RefCell;
use std::sync::OnceLock;

use anyhow::Result;
use netweevil_core::{TopologyBundle, decode_cch_weight};

use crate::*;

thread_local! {
    static PAIRWISE_CCH_SCRATCH: RefCell<PairwiseCchScratch> =
        RefCell::new(PairwiseCchScratch::default());
}

/// The hierarchy arcs used by the pairwise query, with states renumbered by
/// rank. Rows keep the original row order, so relaxations happen in the
/// same order as on the original arrays and results are unchanged.
pub(crate) struct RankedCch {
    /// Upward arcs `(head rank, weight)` per tail rank.
    upward_first_out: Vec<u32>,
    upward_arcs: Vec<(u32, u32)>,
    /// Downward arcs `w -> v` as `(tail rank, weight)` per head rank `v`,
    /// the order in which the backward search relaxes them.
    downward_first_out: Vec<u32>,
    downward_arcs: Vec<(u32, u32)>,
}

impl RankedCch {
    fn build(acceleration: &AccelerationGraph) -> Self {
        let source = acceleration.source.as_ref();
        let compiled = acceleration
            .metrics
            .acceleration
            .as_ref()
            .expect("validated acceleration weights");
        let state_count = source.edge_order.len();
        let mut upward_first_out = Vec::with_capacity(state_count + 1);
        let mut upward_arcs = Vec::with_capacity(source.upward_head.len());
        let mut downward_first_out = Vec::with_capacity(state_count + 1);
        let mut downward_arcs = Vec::with_capacity(source.downward_head.len());
        upward_first_out.push(0);
        downward_first_out.push(0);
        for &edge in &source.edge_order {
            let edge = edge as usize;
            for slot in
                source.upward_first_out[edge] as usize..source.upward_first_out[edge + 1] as usize
            {
                upward_arcs.push((
                    source.edge_rank[source.upward_head[slot] as usize],
                    compiled.upward_weight[slot],
                ));
            }
            upward_first_out.push(upward_arcs.len() as u32);
            for slot in acceleration.reverse_downward_first_out[edge] as usize
                ..acceleration.reverse_downward_first_out[edge + 1] as usize
            {
                downward_arcs.push((
                    source.edge_rank[acceleration.reverse_downward_edge[slot] as usize],
                    compiled.downward_weight[acceleration.reverse_downward_arc[slot] as usize],
                ));
            }
            downward_first_out.push(downward_arcs.len() as u32);
        }
        Self {
            upward_first_out,
            upward_arcs,
            downward_first_out,
            downward_arcs,
        }
    }
}

/// Lazily built [`RankedCch`] of one acceleration graph.
#[derive(Default)]
pub(crate) struct RankedCchCell(OnceLock<RankedCch>);

impl RankedCchCell {
    pub(crate) fn get(&self, acceleration: &AccelerationGraph) -> &RankedCch {
        self.0.get_or_init(|| RankedCch::build(acceleration))
    }
}

/// Search state of one rank. Parents are the neighbouring rank on the path;
/// the hierarchy has at most one upward and one downward arc per ordered
/// pair, so the arc is recovered from the pair when unpacking.
#[derive(Clone, Copy)]
struct CchLabel {
    forward: f64,
    backward: f64,
    forward_parent: u32,
    backward_parent: u32,
}

const NO_PARENT: u32 = u32::MAX;

impl CchLabel {
    const EMPTY: Self = Self {
        forward: f64::INFINITY,
        backward: f64::INFINITY,
        forward_parent: NO_PARENT,
        backward_parent: NO_PARENT,
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
    fn prepare(&mut self, state_count: usize) {
        if self.labels.len() < state_count {
            self.labels.resize(state_count, CchLabel::EMPTY);
        }
        for &rank in &self.touched {
            self.labels[rank as usize] = CchLabel::EMPTY;
        }
        self.touched.clear();
        self.forward_heap.clear();
        self.backward_heap.clear();
    }

    fn update_forward(&mut self, rank: u32, cost: f64, parent: u32) -> bool {
        if !cost.is_finite() {
            return false;
        }
        let label = &mut self.labels[rank as usize];
        if cost + f64::EPSILON >= label.forward {
            return false;
        }
        if label.forward == f64::INFINITY && label.backward == f64::INFINITY {
            self.touched.push(rank);
        }
        label.forward = cost;
        label.forward_parent = parent;
        true
    }

    fn update_backward(&mut self, rank: u32, cost: f64, parent: u32) -> bool {
        if !cost.is_finite() {
            return false;
        }
        let label = &mut self.labels[rank as usize];
        if cost + f64::EPSILON >= label.backward {
            return false;
        }
        if label.forward == f64::INFINITY && label.backward == f64::INFINITY {
            self.touched.push(rank);
        }
        label.backward = cost;
        label.backward_parent = parent;
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
    let source = acceleration.source.as_ref();
    let ranked = acceleration.ranked.get(acceleration);
    let edge_rank = source.edge_rank.as_slice();
    let edge_order = source.edge_order.as_slice();

    // Destination phantoms subtract the unused part of the final edge. A
    // queue minimum alone is therefore not a lower bound on a complete path.
    let minimum_origin_cost = minimum_seed_cost(origin_seeds);
    let minimum_destination_cost = minimum_seed_cost(destination_seeds);

    PAIRWISE_CCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        let scratch = &mut *scratch;
        scratch.prepare(topology.edge_count());

        // Queue entries carry the edge index for tie-breaking (lowest cost,
        // then highest edge, as on the original arrays) and the rank as the
        // payload.
        for &(edge_index, cost) in origin_seeds {
            let rank = edge_rank[edge_index];
            if scratch.update_forward(rank, cost, NO_PARENT) {
                scratch.forward_heap.push(edge_index, rank as usize, cost);
            }
        }
        for &(edge_index, cost) in destination_seeds {
            let rank = edge_rank[edge_index];
            if scratch.update_backward(rank, cost, NO_PARENT) {
                scratch.backward_heap.push(edge_index, rank as usize, cost);
            }
        }

        let mut best_cost = initial_path
            .as_ref()
            .map(|path| path.total_generalized_cost)
            .unwrap_or(f64::INFINITY);
        let mut best_rank = None;

        loop {
            let next_forward_cost = scratch.forward_heap.peek_cost();
            let next_backward_cost = scratch.backward_heap.peek_cost();
            let forward_active = next_forward_cost + minimum_destination_cost < best_cost;
            let backward_active = next_backward_cost + minimum_origin_cost < best_cost;
            if !forward_active && !backward_active {
                break;
            }

            if forward_active && (next_forward_cost <= next_backward_cost || !backward_active) {
                let Some((_, rank, cost)) = scratch.forward_heap.pop() else {
                    break;
                };
                let label = scratch.labels[rank];
                if cost > label.forward {
                    continue;
                }
                if label.backward.is_finite() {
                    let candidate_cost = cost + label.backward;
                    if candidate_cost < best_cost {
                        best_cost = candidate_cost;
                        best_rank = Some(rank);
                    }
                }

                let arcs = &ranked.upward_arcs[ranked.upward_first_out[rank] as usize
                    ..ranked.upward_first_out[rank + 1] as usize];
                for &(head, weight) in arcs {
                    let next_cost = cost + decode_cch_weight(weight);
                    if scratch.update_forward(head, next_cost, rank as u32) {
                        scratch.forward_heap.push(
                            edge_order[head as usize] as usize,
                            head as usize,
                            next_cost,
                        );
                    }
                }
            } else {
                let Some((_, rank, cost)) = scratch.backward_heap.pop() else {
                    break;
                };
                let label = scratch.labels[rank];
                if cost > label.backward {
                    continue;
                }
                if label.forward.is_finite() {
                    let candidate_cost = label.forward + cost;
                    if candidate_cost < best_cost {
                        best_cost = candidate_cost;
                        best_rank = Some(rank);
                    }
                }

                let arcs = &ranked.downward_arcs[ranked.downward_first_out[rank] as usize
                    ..ranked.downward_first_out[rank + 1] as usize];
                for &(tail, weight) in arcs {
                    let next_cost = cost + decode_cch_weight(weight);
                    if scratch.update_backward(tail, next_cost, rank as u32) {
                        scratch.backward_heap.push(
                            edge_order[tail as usize] as usize,
                            tail as usize,
                            next_cost,
                        );
                    }
                }
            }
        }

        let Some(meeting_rank) = best_rank else {
            return Ok(initial_path);
        };
        Ok(Some(reconstruct_pairwise_path(
            acceleration,
            scratch,
            meeting_rank,
            best_cost,
        )))
    })
}

fn reconstruct_pairwise_path(
    acceleration: &AccelerationGraph,
    scratch: &mut PairwiseCchScratch,
    meeting_rank: usize,
    total_generalized_cost: f64,
) -> RoutePath {
    let edge_order = acceleration.source.edge_order.as_slice();
    let mut forward_arcs = std::mem::take(&mut scratch.forward_arcs);
    forward_arcs.clear();
    let mut cursor = meeting_rank;
    loop {
        let parent = scratch.labels[cursor].forward_parent;
        if parent == NO_PARENT {
            break;
        }
        let slot = acceleration
            .upward_arc_slot(edge_order[parent as usize] as usize, edge_order[cursor])
            .expect("forward parent is joined by an upward arc");
        forward_arcs.push(slot as u32);
        cursor = parent as usize;
    }
    let mut edge_indexes = vec![edge_order[cursor] as usize];
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

    let mut cursor = meeting_rank;
    loop {
        let parent = scratch.labels[cursor].backward_parent;
        if parent == NO_PARENT {
            break;
        }
        let slot = acceleration
            .downward_arc_slot(edge_order[cursor] as usize, edge_order[parent as usize])
            .expect("backward parent is joined by a downward arc");
        push_unpacked_arc(
            acceleration,
            &mut unpack_stack,
            false,
            slot as u32,
            &mut edge_indexes,
        );
        cursor = parent as usize;
    }
    scratch.unpack_stack = unpack_stack;
    scratch.forward_arcs = forward_arcs;

    RoutePath {
        edge_indexes,
        total_generalized_cost,
    }
}
