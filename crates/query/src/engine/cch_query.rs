//! Point-to-point CCH query used by single routes and waypoint legs.
//!
//! The query runs on the hierarchy renumbered by contraction rank
//! ([`RankedCch`]). Both searches climb towards high ranks, and every query
//! shares the top of the hierarchy, so with rank-ordered rows and labels
//! most of the work hits the same few hot cache lines. One 24-byte label per
//! state holds both directions' costs and parents, and both queues are 4-ary
//! [`StateHeap`]s of 16-byte entries.

use std::sync::{Arc, OnceLock};

use anyhow::Result;
use netweevil_core::{DatasetAccelerationBundle, TopologyBundle, decode_cch_weight};

use crate::*;

static PAIRWISE_CCH_SCRATCH: ScratchPool<PairwiseCchScratch> = ScratchPool::new();

/// The metric-independent part of a [`RankedCch`]: the hierarchy's arcs with
/// states renumbered by rank, shared by every metric customized on it.
/// Upward rows keep the dataset bundle's row order, so slot `s` of edge `e`'s
/// row sits at offset `s - upward_first_out[e]` of `e`'s ranked row.
/// Downward arcs are grouped by head, tails ascending by edge id: the order
/// of the former reverse-downward index.
pub(crate) struct RankedTopology {
    /// Upward arc head ranks per tail rank.
    upward_first_out: Vec<u32>,
    upward_head: Vec<u32>,
    /// Downward arcs `w -> v` as tail ranks per head rank `v`, the order in
    /// which backward searches relax them.
    downward_first_out: Vec<u32>,
    downward_tail: Vec<u32>,
    /// Bundle slot of each ranked downward arc, built on first use.
    downward_slots: OnceLock<Vec<u32>>,
}

impl RankedTopology {
    fn build(source: &DatasetAccelerationBundle) -> Self {
        let state_count = source.edge_order.len();
        let mut upward_first_out = Vec::with_capacity(state_count + 1);
        let mut upward_head = Vec::with_capacity(source.upward_head.len());
        upward_first_out.push(0);
        for &edge in &source.edge_order {
            let edge = edge as usize;
            let row =
                source.upward_first_out[edge] as usize..source.upward_first_out[edge + 1] as usize;
            upward_head.extend(
                source.upward_head[row]
                    .iter()
                    .map(|&head| source.edge_rank[head as usize]),
            );
            upward_first_out.push(upward_head.len() as u32);
        }
        let (downward_first_out, downward_tail) =
            group_downward_by_head(source, |tail, _| source.edge_rank[tail]);
        Self {
            upward_first_out,
            upward_head,
            downward_first_out,
            downward_tail,
            downward_slots: OnceLock::new(),
        }
    }
}

/// `value(tail, slot)` of every downward arc, grouped by head rank with
/// tails ascending by edge id, and the per-rank first-out.
fn group_downward_by_head<T: Copy + Default>(
    source: &DatasetAccelerationBundle,
    value: impl Fn(usize, usize) -> T,
) -> (Vec<u32>, Vec<T>) {
    let state_count = source.edge_order.len();
    let mut first_out = vec![0_u32; state_count + 1];
    for &head in &source.downward_head {
        first_out[source.edge_rank[head as usize] as usize + 1] += 1;
    }
    for rank in 0..state_count {
        first_out[rank + 1] += first_out[rank];
    }
    let mut write = first_out[..state_count].to_vec();
    let mut values = vec![T::default(); source.downward_head.len()];
    for tail in 0..state_count {
        for slot in
            source.downward_first_out[tail] as usize..source.downward_first_out[tail + 1] as usize
        {
            let head_rank = source.edge_rank[source.downward_head[slot] as usize] as usize;
            values[write[head_rank] as usize] = value(tail, slot);
            write[head_rank] += 1;
        }
    }
    (first_out, values)
}

/// The customized hierarchy of one metric with states renumbered by rank:
/// the arcs every hierarchy search walks and the only copy of the arc
/// weights. Metrics customized on the same hierarchy (traffic buckets) share
/// the [`RankedTopology`] and carry only their weights, aligned with its
/// rows.
pub(crate) struct RankedCch {
    topology: Arc<RankedTopology>,
    upward_weight: Vec<u32>,
    downward_weight: Vec<u32>,
}

impl RankedCch {
    /// Ranked rows of `source` carrying the slot-ordered arc weights. Every
    /// metric customized on the same live `source` shares one topology.
    pub(crate) fn build(
        source: &Arc<DatasetAccelerationBundle>,
        upward_weight: &[u32],
        downward_weight: &[u32],
    ) -> Self {
        static TOPOLOGIES: DerivedCache<DatasetAccelerationBundle, RankedTopology> =
            DerivedCache::new();
        Self::with_topology(
            TOPOLOGIES.get_or_build(source, || RankedTopology::build(source)),
            source,
            upward_weight,
            downward_weight,
        )
    }

    /// Another metric's slot-ordered weights on this hierarchy.
    pub(crate) fn with_weights(
        &self,
        source: &DatasetAccelerationBundle,
        upward_weight: &[u32],
        downward_weight: &[u32],
    ) -> Self {
        Self::with_topology(
            self.topology.clone(),
            source,
            upward_weight,
            downward_weight,
        )
    }

    fn with_topology(
        topology: Arc<RankedTopology>,
        source: &DatasetAccelerationBundle,
        upward_weight: &[u32],
        downward_weight: &[u32],
    ) -> Self {
        let upward_weight = source
            .edge_order
            .iter()
            .flat_map(|&edge| {
                let edge = edge as usize;
                &upward_weight[source.upward_first_out[edge] as usize
                    ..source.upward_first_out[edge + 1] as usize]
            })
            .copied()
            .collect();
        let (_, downward_weight) = group_downward_by_head(source, |_, slot| downward_weight[slot]);
        Self {
            topology,
            upward_weight,
            downward_weight,
        }
    }

    /// Bundle slot of every downward arc, in the order of the ranked
    /// downward rows; built once per hierarchy.
    pub(crate) fn downward_slots(&self, source: &DatasetAccelerationBundle) -> &[u32] {
        self.topology
            .downward_slots
            .get_or_init(|| group_downward_by_head(source, |_, slot| slot as u32).1)
    }

    fn upward_range(&self, rank: usize) -> std::ops::Range<usize> {
        self.topology.upward_first_out[rank] as usize
            ..self.topology.upward_first_out[rank + 1] as usize
    }

    /// Upward arcs `(head rank, weight)` leaving `rank`, in bundle row order.
    pub(crate) fn upward_row(&self, rank: usize) -> impl Iterator<Item = (u32, u32)> + '_ {
        let range = self.upward_range(rank);
        self.topology.upward_head[range.clone()]
            .iter()
            .copied()
            .zip(self.upward_weight[range].iter().copied())
    }

    /// Range of the downward arcs entering `rank` in the ranked downward
    /// rows (the positions [`Self::downward_slots`] maps to bundle slots).
    pub(crate) fn downward_range(&self, rank: usize) -> std::ops::Range<usize> {
        self.topology.downward_first_out[rank] as usize
            ..self.topology.downward_first_out[rank + 1] as usize
    }

    /// Downward arcs `(tail rank, weight)` entering `rank`.
    pub(crate) fn downward_row(&self, rank: usize) -> impl Iterator<Item = (u32, u32)> + '_ {
        let range = self.downward_range(rank);
        self.topology.downward_tail[range.clone()]
            .iter()
            .copied()
            .zip(self.downward_weight[range].iter().copied())
    }

    /// The downward arc `(tail rank, weight)` at a position of a
    /// [`Self::downward_range`].
    pub(crate) fn downward_arc(&self, position: usize) -> (u32, u32) {
        (
            self.topology.downward_tail[position],
            self.downward_weight[position],
        )
    }

    /// Position in the ranked upward weights of the upward arc in bundle
    /// slot `slot` of `tail`'s row.
    pub(crate) fn upward_position(
        &self,
        source: &DatasetAccelerationBundle,
        tail: usize,
        slot: usize,
    ) -> usize {
        let offset = slot - source.upward_first_out[tail] as usize;
        self.upward_range(source.edge_rank[tail] as usize).start + offset
    }

    /// Position in the ranked downward weights of the downward arc
    /// `tail -> head`, if the hierarchy has it.
    pub(crate) fn downward_position(
        &self,
        source: &DatasetAccelerationBundle,
        tail: usize,
        head: usize,
    ) -> Option<usize> {
        let range = self.downward_range(source.edge_rank[head] as usize);
        self.topology.downward_tail[range.clone()]
            .binary_search_by(|&tail_rank| {
                (source.edge_order[tail_rank as usize] as usize).cmp(&tail)
            })
            .ok()
            .map(|offset| range.start + offset)
    }

    /// Weight of the upward arc in bundle slot `slot` of `tail`'s row.
    pub(crate) fn upward_weight(
        &self,
        source: &DatasetAccelerationBundle,
        tail: usize,
        slot: usize,
    ) -> u32 {
        self.upward_weight[self.upward_position(source, tail, slot)]
    }

    /// Weight of the downward arc `tail -> head`, if the hierarchy has it.
    pub(crate) fn downward_weight(
        &self,
        source: &DatasetAccelerationBundle,
        tail: usize,
        head: usize,
    ) -> Option<u32> {
        self.downward_position(source, tail, head)
            .map(|position| self.downward_weight[position])
    }

    /// The ranked `(upward, downward)` weight arrays.
    pub(crate) fn ranked_weights(&self) -> (&[u32], &[u32]) {
        (&self.upward_weight, &self.downward_weight)
    }

    /// This hierarchy with other weights in ranked order.
    pub(crate) fn with_ranked_weights(&self, upward: Vec<u32>, downward: Vec<u32>) -> Self {
        debug_assert_eq!(upward.len(), self.upward_weight.len());
        debug_assert_eq!(downward.len(), self.downward_weight.len());
        Self {
            topology: self.topology.clone(),
            upward_weight: upward,
            downward_weight: downward,
        }
    }

    /// Whether `other` shares this hierarchy's ranked topology.
    #[cfg(test)]
    pub(crate) fn shares_topology(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.topology, &other.topology)
    }

    /// Slot-ordered `(upward, downward)` weights, as customization emits them.
    #[cfg(test)]
    pub(crate) fn slot_weights(&self, source: &DatasetAccelerationBundle) -> (Vec<u32>, Vec<u32>) {
        let upward = (0..source.edge_order.len())
            .flat_map(|tail| {
                (source.upward_first_out[tail] as usize..source.upward_first_out[tail + 1] as usize)
                    .map(move |slot| self.upward_weight(source, tail, slot))
            })
            .collect();
        let mut downward = vec![0; source.downward_head.len()];
        for (position, &slot) in self.downward_slots(source).iter().enumerate() {
            downward[slot as usize] = self.downward_weight[position];
        }
        (upward, downward)
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
    forward_arcs: Vec<HierarchyArc>,
    unpack_stack: UnpackStack,
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
    let ranked = &acceleration.ranked;
    let edge_rank = source.edge_rank.as_slice();
    let edge_order = source.edge_order.as_slice();

    // Destination phantoms subtract the unused part of the final edge. A
    // queue minimum alone is therefore not a lower bound on a complete path.
    let minimum_origin_cost = minimum_seed_cost(origin_seeds);
    let minimum_destination_cost = minimum_seed_cost(destination_seeds);

    PAIRWISE_CCH_SCRATCH.with(|scratch| {
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
        let tail = edge_order[parent as usize];
        let slot = acceleration
            .upward_arc_slot(tail as usize, edge_order[cursor])
            .expect("forward parent is joined by an upward arc");
        forward_arcs.push(HierarchyArc::upward(tail, slot));
        cursor = parent as usize;
    }
    let mut edge_indexes = vec![edge_order[cursor] as usize];
    let mut unpack_stack = std::mem::take(&mut scratch.unpack_stack);
    for &arc in forward_arcs.iter().rev() {
        push_unpacked_arc(acceleration, &mut unpack_stack, arc, &mut edge_indexes);
    }

    let mut cursor = meeting_rank;
    loop {
        let parent = scratch.labels[cursor].backward_parent;
        if parent == NO_PARENT {
            break;
        }
        let tail = edge_order[cursor];
        let slot = acceleration
            .downward_arc_slot(tail as usize, edge_order[parent as usize])
            .expect("backward parent is joined by a downward arc");
        push_unpacked_arc(
            acceleration,
            &mut unpack_stack,
            HierarchyArc::downward(tail, slot),
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
