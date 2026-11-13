//! Point-to-point CCH query used by single routes and waypoint legs.
//!
//! The query runs on the hierarchy renumbered by contraction rank
//! ([`RankedCch`]). Both searches climb towards high ranks, and every query
//! shares the top of the hierarchy, so with rank-ordered rows and labels
//! most of the work hits the same few hot cache lines. One 24-byte label per
//! state holds both directions' costs and parents in a zero-allocated
//! array, so a query commits memory only for the states it labels. Both
//! queues are 4-ary [`StateHeap`]s of 16-byte entries.

use std::sync::Arc;

use anyhow::Result;
use netweevil_core::{Column, DatasetAccelerationBundle, TopologyBundle, decode_cch_weight};

use crate::*;

static PAIRWISE_CCH_SCRATCH: ScratchPool<PairwiseCchScratch> = ScratchPool::new();

/// The customized hierarchy of one metric: the dataset hierarchy, whose
/// rank-ordered rows every hierarchy search walks, and the arc weights
/// aligned with its arc positions. Metrics customized on the same hierarchy
/// (traffic buckets) share it and carry only their weights. Weights loaded
/// from a compiled profile bundle stay in its memory map.
pub(crate) struct RankedCch {
    hierarchy: Arc<DatasetAccelerationBundle>,
    upward_weight: Column<u32>,
    downward_weight: Column<u32>,
}

impl RankedCch {
    /// `hierarchy` with weights aligned with its upward and downward arc
    /// positions.
    pub(crate) fn new(
        hierarchy: Arc<DatasetAccelerationBundle>,
        upward_weight: Column<u32>,
        downward_weight: Column<u32>,
    ) -> Self {
        debug_assert_eq!(upward_weight.len(), hierarchy.upward_head.len());
        debug_assert_eq!(downward_weight.len(), hierarchy.downward_tail.len());
        Self {
            hierarchy,
            upward_weight,
            downward_weight,
        }
    }

    /// Upward arcs `(head rank, weight)` leaving `rank`.
    #[inline]
    pub(crate) fn upward_row(&self, rank: usize) -> impl Iterator<Item = (u32, u32)> + '_ {
        let range = self.hierarchy.upward_range(rank);
        self.hierarchy.upward_head[range.clone()]
            .iter()
            .copied()
            .zip(self.upward_weight[range].iter().copied())
    }

    /// Positions of the downward arcs entering `rank`.
    #[inline]
    pub(crate) fn downward_range(&self, rank: usize) -> std::ops::Range<usize> {
        self.hierarchy.downward_range(rank)
    }

    /// Downward arcs `(tail rank, weight)` entering `rank`.
    #[inline]
    pub(crate) fn downward_row(&self, rank: usize) -> impl Iterator<Item = (u32, u32)> + '_ {
        let range = self.hierarchy.downward_range(rank);
        self.hierarchy.downward_tail[range.clone()]
            .iter()
            .copied()
            .zip(self.downward_weight[range].iter().copied())
    }

    /// The downward arc `(tail rank, weight)` at a position of a
    /// [`Self::downward_range`].
    pub(crate) fn downward_arc(&self, position: usize) -> (u32, u32) {
        (
            self.hierarchy.downward_tail[position],
            self.downward_weight[position],
        )
    }

    /// Weight of the arc at `position` of the upward or downward arcs.
    #[inline]
    pub(crate) fn weight(&self, is_upward: bool, position: usize) -> u32 {
        if is_upward {
            self.upward_weight[position]
        } else {
            self.downward_weight[position]
        }
    }

    /// The `(upward, downward)` weight arrays, aligned with the arc
    /// positions.
    pub(crate) fn weights(&self) -> (&[u32], &[u32]) {
        (&self.upward_weight, &self.downward_weight)
    }

    /// This hierarchy with other weights.
    pub(crate) fn with_weights(&self, upward: Column<u32>, downward: Column<u32>) -> Self {
        Self::new(self.hierarchy.clone(), upward, downward)
    }

    /// Whether `other` shares this hierarchy.
    #[cfg(test)]
    pub(crate) fn shares_topology(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.hierarchy, &other.hierarchy)
    }

    /// The `(upward, downward)` weights as vectors.
    #[cfg(test)]
    pub(crate) fn weight_vectors(&self) -> (Vec<u32>, Vec<u32>) {
        (self.upward_weight.to_vec(), self.downward_weight.to_vec())
    }
}

/// Search state of one rank. Parents are the neighbouring rank on the path;
/// the hierarchy has at most one upward and one downward arc per ordered
/// pair, so the arc is recovered from the pair when unpacking.
///
/// Costs are stored with [`encode_cost`] and parents with [`encode_link`],
/// so the empty label is all zero bytes and the label array can come from a
/// zeroed allocation. Seed costs are arbitrary `f64` phantom offsets, so the
/// costs stay `f64` rather than fixed-point sums of hierarchy weights.
#[derive(Clone, Copy)]
#[repr(C)]
struct CchLabel {
    forward: u64,
    backward: u64,
    forward_parent: u32,
    backward_parent: u32,
}

// SAFETY: every field is an integer, for which all-zero bytes are valid.
unsafe impl bytemuck::Zeroable for CchLabel {}
// SAFETY: `repr(C)` integer fields of 8, 8, 4 and 4 bytes leave no padding.
unsafe impl bytemuck::Pod for CchLabel {}

const NO_PARENT: u32 = u32::MAX;

impl CchLabel {
    const EMPTY: Self = Self {
        forward: 0,
        backward: 0,
        forward_parent: 0,
        backward_parent: 0,
    };

    #[inline]
    fn forward(&self) -> f64 {
        decode_cost(self.forward)
    }

    #[inline]
    fn backward(&self) -> f64 {
        decode_cost(self.backward)
    }

    #[inline]
    fn forward_parent(&self) -> u32 {
        decode_link(self.forward_parent)
    }

    #[inline]
    fn backward_parent(&self) -> u32 {
        decode_link(self.backward_parent)
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.forward == 0 && self.backward == 0
    }

    /// `(cost, parent)` of one direction.
    #[inline]
    fn side(&self, backward: bool) -> (f64, u32) {
        if backward {
            (self.backward(), self.backward_parent())
        } else {
            (self.forward(), self.forward_parent())
        }
    }

    #[inline]
    fn set_side(&mut self, backward: bool, cost: f64, parent: u32) {
        if backward {
            self.backward = encode_cost(cost);
            self.backward_parent = encode_link(parent);
        } else {
            self.forward = encode_cost(cost);
            self.forward_parent = encode_link(parent);
        }
    }
}

#[derive(Default)]
struct PairwiseCchScratch {
    labels: ZeroedArray<CchLabel>,
    touched: Vec<u32>,
    forward_heap: StateHeap,
    backward_heap: StateHeap,
    forward_arcs: Vec<HierarchyArc>,
    unpack_stack: UnpackStack,
}

impl PairwiseCchScratch {
    fn prepare(&mut self, state_count: usize) {
        let release = release_after(self.labels.len(), self.touched.len());
        self.labels.reset(state_count, &self.touched, release);
        reset_touched(&mut self.touched, release);
        self.forward_heap.clear();
        self.backward_heap.clear();
    }

    #[inline]
    fn update(&mut self, backward: bool, rank: u32, cost: f64, parent: u32) -> bool {
        if !cost.is_finite() {
            return false;
        }
        let label = self.labels.get_mut(rank as usize);
        if cost + f64::EPSILON >= label.side(backward).0 {
            return false;
        }
        if label.is_empty() {
            self.touched.push(rank);
        }
        label.set_side(backward, cost, parent);
        true
    }

    fn update_forward(&mut self, rank: u32, cost: f64, parent: u32) -> bool {
        self.update(false, rank, cost, parent)
    }

    fn update_backward(&mut self, rank: u32, cost: f64, parent: u32) -> bool {
        self.update(true, rank, cost, parent)
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
    PAIRWISE_CCH_SCRATCH.with(|scratch| {
        scratch.prepare(topology.edge_count());
        Ok(pairwise_query(
            scratch,
            acceleration,
            origin_seeds,
            destination_seeds,
            initial_path,
        ))
    })
}

/// The search of [`accelerated_route_query_seeded`] on a prepared scratch.
fn pairwise_query(
    scratch: &mut PairwiseCchScratch,
    acceleration: &AccelerationGraph,
    origin_seeds: &[(usize, f64)],
    destination_seeds: &[(usize, f64)],
    initial_path: Option<RoutePath>,
) -> Option<RoutePath> {
    let source = acceleration.source.as_ref();
    let ranked = &acceleration.ranked;
    let edge_rank = source.edge_rank.as_slice();
    let edge_order = source.edge_order.as_slice();

    // Destination phantoms subtract the unused part of the final edge. A
    // queue minimum alone is therefore not a lower bound on a complete path.
    let minimum_origin_cost = minimum_seed_cost(origin_seeds);
    let minimum_destination_cost = minimum_seed_cost(destination_seeds);

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
            let label = scratch.labels.get(rank);
            if cost > label.forward() {
                continue;
            }
            if label.backward().is_finite() {
                let candidate_cost = cost + label.backward();
                if candidate_cost < best_cost {
                    best_cost = candidate_cost;
                    best_rank = Some(rank);
                }
            }

            for (head, weight) in ranked.upward_row(rank) {
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
            let label = scratch.labels.get(rank);
            if cost > label.backward() {
                continue;
            }
            if label.forward().is_finite() {
                let candidate_cost = label.forward() + cost;
                if candidate_cost < best_cost {
                    best_cost = candidate_cost;
                    best_rank = Some(rank);
                }
            }

            for (tail, weight) in ranked.downward_row(rank) {
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
        return initial_path;
    };
    Some(reconstruct_pairwise_path(
        acceleration,
        scratch,
        meeting_rank,
        best_cost,
    ))
}

fn reconstruct_pairwise_path(
    acceleration: &AccelerationGraph,
    scratch: &mut PairwiseCchScratch,
    meeting_rank: usize,
    total_generalized_cost: f64,
) -> RoutePath {
    let source = acceleration.source.as_ref();
    let mut forward_arcs = std::mem::take(&mut scratch.forward_arcs);
    forward_arcs.clear();
    let mut cursor = meeting_rank as u32;
    loop {
        let parent = scratch.labels.get(cursor as usize).forward_parent();
        if parent == NO_PARENT {
            break;
        }
        forward_arcs.push(
            HierarchyArc::between(source, parent, cursor)
                .expect("forward parent is joined by an upward arc"),
        );
        cursor = parent;
    }
    let mut edge_indexes = vec![source.edge_order[cursor as usize] as usize];
    let mut unpack_stack = std::mem::take(&mut scratch.unpack_stack);
    for &arc in forward_arcs.iter().rev() {
        push_unpacked_arc(acceleration, &mut unpack_stack, arc, &mut edge_indexes);
    }

    let mut cursor = meeting_rank as u32;
    loop {
        let parent = scratch.labels.get(cursor as usize).backward_parent();
        if parent == NO_PARENT {
            break;
        }
        push_unpacked_arc(
            acceleration,
            &mut unpack_stack,
            HierarchyArc::between(source, cursor, parent)
                .expect("backward parent is joined by a downward arc"),
            &mut edge_indexes,
        );
        cursor = parent;
    }
    scratch.unpack_stack = unpack_stack;
    scratch.forward_arcs = forward_arcs;

    RoutePath {
        edge_indexes,
        total_generalized_cost,
    }
}

/// One state settled by [`recorded_hierarchy_search`].
pub(crate) struct HierarchySettle {
    pub(crate) edge_index: u32,
    pub(crate) cost: f64,
    /// Settled by the backward search toward the destination.
    pub(crate) backward: bool,
    /// Edge of the state this label was relaxed from over a hierarchy arc;
    /// `None` for seeds.
    pub(crate) previous_edge_index: Option<u32>,
}

/// The search of [`accelerated_route_query_seeded`], recording each state
/// as either direction settles it. Returns the settles in order, whether
/// `max_states` stopped the search, and the best up-down cost. `None`
/// without a hierarchy.
pub(crate) fn recorded_hierarchy_search(
    routing_graph: &RoutingGraph,
    origin_seeds: &[(usize, f64)],
    destination_seeds: &[(usize, f64)],
    max_states: usize,
) -> Option<(Vec<HierarchySettle>, bool, Option<f64>)> {
    let acceleration = routing_graph.acceleration.as_ref()?;
    let source = acceleration.source.as_ref();
    let ranked = &acceleration.ranked;
    let edge_rank = source.edge_rank.as_slice();
    let edge_order = source.edge_order.as_slice();
    let minimum_origin_cost = minimum_seed_cost(origin_seeds);
    let minimum_destination_cost = minimum_seed_cost(destination_seeds);

    let mut labels = rustc_hash::FxHashMap::<u32, CchLabel>::default();
    let mut forward_heap = StateHeap::default();
    let mut backward_heap = StateHeap::default();
    for &(edge_index, cost) in origin_seeds {
        let rank = edge_rank[edge_index];
        let label = labels.entry(rank).or_insert(CchLabel::EMPTY);
        if cost < label.forward() {
            label.set_side(false, cost, NO_PARENT);
            forward_heap.push(edge_index, rank as usize, cost);
        }
    }
    for &(edge_index, cost) in destination_seeds {
        let rank = edge_rank[edge_index];
        let label = labels.entry(rank).or_insert(CchLabel::EMPTY);
        if cost < label.backward() {
            label.set_side(true, cost, NO_PARENT);
            backward_heap.push(edge_index, rank as usize, cost);
        }
    }

    let mut settles = Vec::new();
    let mut best_cost = f64::INFINITY;
    let mut truncated = false;
    loop {
        let next_forward_cost = forward_heap.peek_cost();
        let next_backward_cost = backward_heap.peek_cost();
        let forward_active = next_forward_cost + minimum_destination_cost < best_cost;
        let backward_active = next_backward_cost + minimum_origin_cost < best_cost;
        if !forward_active && !backward_active {
            break;
        }
        let backward =
            !(forward_active && (next_forward_cost <= next_backward_cost || !backward_active));
        let popped = if backward {
            backward_heap.pop()
        } else {
            forward_heap.pop()
        };
        let Some((_, rank, cost)) = popped else {
            break;
        };
        let label = labels[&(rank as u32)];
        let (own, parent) = label.side(backward);
        let (other, _) = label.side(!backward);
        if cost > own {
            continue;
        }
        if settles.len() >= max_states {
            truncated = true;
            break;
        }
        settles.push(HierarchySettle {
            edge_index: edge_order[rank],
            cost,
            backward,
            previous_edge_index: (parent != NO_PARENT).then(|| edge_order[parent as usize]),
        });
        if other.is_finite() {
            best_cost = best_cost.min(cost + other);
        }
        let mut relax = |next: u32, weight: u32| {
            let next_cost = cost + decode_cch_weight(weight);
            let label = labels.entry(next).or_insert(CchLabel::EMPTY);
            if next_cost + f64::EPSILON >= label.side(backward).0 {
                return;
            }
            label.set_side(backward, next_cost, rank as u32);
            if backward {
                backward_heap.push(edge_order[next as usize] as usize, next as usize, next_cost);
            } else {
                forward_heap.push(edge_order[next as usize] as usize, next as usize, next_cost);
            }
        };
        if backward {
            for (tail, weight) in ranked.downward_row(rank) {
                relax(tail, weight);
            }
        } else {
            for (head, weight) in ranked.upward_row(rank) {
                relax(head, weight);
            }
        }
    }
    Some((
        settles,
        truncated,
        best_cost.is_finite().then_some(best_cost),
    ))
}

#[cfg(test)]
mod tests;
