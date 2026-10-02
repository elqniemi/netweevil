use std::collections::BinaryHeap;

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use anyhow::Result;
#[cfg(test)]
use anyhow::bail;
use netweevil_core::TopologyBundle;

use crate::*;

/// Lazily filled lower-triangle witnesses for every hierarchy arc of one
/// customized metric, shared by all threads querying it.
///
/// Each entry is 0 until first unpacked, [`UNPACK_BASE_ARC`] for an arc
/// without a lower triangle of equal weight, or `(left + 1) << 32 | right`.
/// The arrays are zero-allocated, so only pages of arcs that some route has
/// unpacked are ever committed.
pub(crate) struct UnpackTable {
    upward: Box<[AtomicU64]>,
    downward: Box<[AtomicU64]>,
}

const UNPACK_UNKNOWN: u64 = 0;
const UNPACK_BASE_ARC: u64 = u64::MAX;

impl UnpackTable {
    pub(crate) fn new(upward_len: usize, downward_len: usize) -> Self {
        Self {
            upward: bytemuck::zeroed_slice_box(upward_len),
            downward: bytemuck::zeroed_slice_box(downward_len),
        }
    }

    fn entry(&self, is_upward: bool, slot: usize) -> &AtomicU64 {
        if is_upward {
            &self.upward[slot]
        } else {
            &self.downward[slot]
        }
    }
}

#[derive(Default)]
pub(crate) struct BidirectionalAccelerationScratch {
    forward_dist: Vec<f64>,
    forward_parent: Vec<u32>,
    forward_touched: Vec<u32>,
    forward_heap: BinaryHeap<State>,
    backward_dist: Vec<f64>,
    backward_parent: Vec<u32>,
    backward_touched: Vec<u32>,
    backward_heap: BinaryHeap<State>,
}

impl BidirectionalAccelerationScratch {
    fn prepare(&mut self, edge_count: usize) {
        if self.forward_dist.len() < edge_count {
            self.forward_dist.resize(edge_count, f64::INFINITY);
            self.forward_parent.resize(edge_count, NO_PREVIOUS_ARC);
            self.backward_dist.resize(edge_count, f64::INFINITY);
            self.backward_parent.resize(edge_count, NO_PREVIOUS_ARC);
        }
        for &edge_index in &self.forward_touched {
            self.forward_dist[edge_index as usize] = f64::INFINITY;
            self.forward_parent[edge_index as usize] = NO_PREVIOUS_ARC;
        }
        for &edge_index in &self.backward_touched {
            self.backward_dist[edge_index as usize] = f64::INFINITY;
            self.backward_parent[edge_index as usize] = NO_PREVIOUS_ARC;
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
        self.forward_parent[edge_index] = previous_arc;
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
        self.backward_parent[edge_index] = next_arc;
        true
    }
}

/// One hierarchy arc: its bundle slot, direction and tail state. The ranked
/// hierarchy keeps no per-arc tail, so unpacking carries it along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HierarchyArc {
    pub(crate) is_upward: bool,
    pub(crate) tail: u32,
    pub(crate) slot: u32,
}

impl HierarchyArc {
    pub(crate) fn upward(tail: u32, slot: usize) -> Self {
        Self {
            is_upward: true,
            tail,
            slot: slot as u32,
        }
    }

    pub(crate) fn downward(tail: u32, slot: usize) -> Self {
        Self {
            is_upward: false,
            tail,
            slot: slot as u32,
        }
    }

    /// Head state: the later state on the unpacked path.
    pub(crate) fn head(self, acceleration: &AccelerationGraph) -> u32 {
        if self.is_upward {
            acceleration.source.upward_head[self.slot as usize]
        } else {
            acceleration.source.downward_head[self.slot as usize]
        }
    }

    /// The two child arcs of a lower-triangle witness `(left, right)`:
    /// `tail -> middle` downward, then `middle -> head` upward.
    pub(crate) fn children(
        self,
        acceleration: &AccelerationGraph,
        (left, right): (u32, u32),
    ) -> (Self, Self) {
        let middle = acceleration.source.downward_head[left as usize];
        (
            Self::downward(self.tail, left as usize),
            Self::upward(middle, right as usize),
        )
    }
}

/// Tail of a stacked arc. A right child's tail is the head of its left
/// sibling; it is only looked up when the arc's witness is not memoized.
#[derive(Debug, Clone, Copy)]
enum PendingTail {
    State(u32),
    HeadOfDownward(u32),
}

#[derive(Debug, Clone, Copy)]
struct PendingArc {
    is_upward: bool,
    slot: u32,
    tail: PendingTail,
}

/// Reusable stack of [`push_unpacked_arc`].
#[derive(Debug, Default)]
pub(crate) struct UnpackStack(Vec<PendingArc>);

/// Unpacks arcs by finding a lower triangle with the same integer weight.
/// Child endpoints have lower rank, so expansion terminates even with zero costs.
/// Witnesses are memoized in the graph's [`UnpackTable`]; a witness depends
/// only on the arc topology and customized weights that the graph owns.
pub(crate) fn push_unpacked_arc(
    acceleration: &AccelerationGraph,
    stack: &mut UnpackStack,
    arc: HierarchyArc,
    edge_indexes: &mut Vec<usize>,
) {
    let stack = &mut stack.0;
    debug_assert!(stack.is_empty());
    stack.push(PendingArc {
        is_upward: arc.is_upward,
        slot: arc.slot,
        tail: PendingTail::State(arc.tail),
    });
    while let Some(pending) = stack.pop() {
        let witness =
            memoized_witness(
                acceleration,
                pending.is_upward,
                pending.slot,
                || match pending.tail {
                    PendingTail::State(tail) => tail,
                    PendingTail::HeadOfDownward(left) => {
                        acceleration.source.downward_head[left as usize]
                    }
                },
            );
        let Some((left, right)) = witness else {
            let head = if pending.is_upward {
                acceleration.source.upward_head[pending.slot as usize]
            } else {
                acceleration.source.downward_head[pending.slot as usize]
            };
            edge_indexes.push(head as usize);
            continue;
        };
        // Process left before right: LIFO stack, so push right first.
        stack.push(PendingArc {
            is_upward: true,
            slot: right,
            tail: PendingTail::HeadOfDownward(left),
        });
        stack.push(PendingArc {
            is_upward: false,
            slot: left,
            tail: pending.tail,
        });
    }
}

/// The lower triangle `tail -> middle -> head` that unpacks an arc: the first
/// downward arc out of `tail`, in CSR order, whose middle state ranks below
/// `head` and whose child weights sum exactly to the arc weight, as bundle
/// slots `(left, right)`. `None` means the arc is a base transition. Every
/// unpacking path reads the same memoized witness from the graph's
/// [`UnpackTable`], so summaries and explicit paths agree on the chosen
/// decomposition.
pub(crate) fn unpack_witness(
    acceleration: &AccelerationGraph,
    arc: HierarchyArc,
) -> Option<(u32, u32)> {
    memoized_witness(acceleration, arc.is_upward, arc.slot, || arc.tail)
}

fn memoized_witness(
    acceleration: &AccelerationGraph,
    is_upward: bool,
    slot: u32,
    tail: impl FnOnce() -> u32,
) -> Option<(u32, u32)> {
    let entry = acceleration.unpack.entry(is_upward, slot as usize);
    let mut witness = entry.load(AtomicOrdering::Relaxed);
    if witness == UNPACK_UNKNOWN {
        let arc = HierarchyArc {
            is_upward,
            tail: tail(),
            slot,
        };
        witness = find_unpack_witness(acceleration, arc);
        entry.store(witness, AtomicOrdering::Relaxed);
    }
    (witness != UNPACK_BASE_ARC).then(|| ((witness >> 32) as u32 - 1, witness as u32))
}

fn find_unpack_witness(acceleration: &AccelerationGraph, arc: HierarchyArc) -> u64 {
    let source = acceleration.source.as_ref();
    let ranked = &acceleration.ranked;
    let tail = arc.tail as usize;
    let head = arc.head(acceleration) as usize;
    let weight = if arc.is_upward {
        ranked.upward_weight(source, tail, arc.slot as usize)
    } else {
        ranked
            .downward_weight(source, tail, head)
            .expect("downward arc is in its head's ranked row")
    };
    assert!(
        weight < netweevil_core::CCH_WEIGHT_OVERFLOW,
        "only finite CCH arcs can be unpacked"
    );
    let head_rank = source.edge_rank[head];
    (source.downward_first_out[tail] as usize..source.downward_first_out[tail + 1] as usize)
        .find_map(|left| {
            let middle = source.downward_head[left] as usize;
            if source.edge_rank[middle] >= head_rank {
                return None;
            }
            let right = acceleration.upward_arc_slot(middle, head as u32)?;
            let left_weight = ranked
                .downward_weight(source, tail, middle)
                .expect("downward arc is in its head's ranked row");
            (netweevil_core::add_cch_weights(
                left_weight,
                ranked.upward_weight(source, middle, right),
            ) == weight)
                .then(|| ((left as u64 + 1) << 32) | right as u64)
        })
        .unwrap_or(UNPACK_BASE_ARC)
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

pub(crate) fn minimum_seed_cost(seeds: &[(usize, f64)]) -> f64 {
    seeds
        .iter()
        .map(|&(_, cost)| cost)
        .filter(|cost| cost.is_finite())
        .fold(0.0, f64::min)
}

/// One settled hierarchy state of a full CCH search space.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CchSpaceEntry {
    pub(crate) edge_index: u32,
    pub(crate) cost: f64,
    /// Bundle slot of the predecessor upward arc (forward) or successor
    /// downward arc (backward); `NO_PREVIOUS_ARC` at seeds.
    pub(crate) arc: u32,
    /// Entry position of the arc's other endpoint; `NO_PREVIOUS_ARC` at seeds.
    pub(crate) parent: u32,
}

/// Complete (unpruned) upward search space from a set of seeds, sorted by
/// edge index. Forward spaces store the predecessor upward arc per state;
/// backward spaces store the successor downward arc. Because both spaces are
/// complete, `min over shared states of (forward + backward)` equals the
/// shortest-path cost under the quantized metric, as in the pairwise CCH
/// query. The spaces are shared across origin/destination combinations.
#[derive(Debug)]
pub(crate) struct CchSearchSpace {
    pub(crate) entries: Vec<CchSpaceEntry>,
}

impl CchSearchSpace {
    /// Sorts the settled states and resolves each state's neighbour edge,
    /// held in `parent` until now, to its entry position and the arc slot
    /// `arc_slot(state, neighbour)`.
    fn from_unsorted(
        mut entries: Vec<CchSpaceEntry>,
        arc_slot: impl Fn(u32, u32) -> Option<usize>,
    ) -> Self {
        entries.sort_unstable_by_key(|entry| entry.edge_index);
        for index in 0..entries.len() {
            let neighbour = entries[index].parent;
            if neighbour == NO_PREVIOUS_ARC {
                continue;
            }
            entries[index].arc = arc_slot(entries[index].edge_index, neighbour)
                .expect("search-space parent is joined by a hierarchy arc")
                as u32;
            entries[index].parent = entries
                .binary_search_by_key(&neighbour, |entry| entry.edge_index)
                .expect("search-space predecessor is settled")
                as u32;
        }
        Self { entries }
    }

    pub(crate) fn position(&self, edge_index: u32) -> Option<usize> {
        self.entries
            .binary_search_by_key(&edge_index, |entry| entry.edge_index)
            .ok()
    }

    /// The arc that settled the entry at `position`, `None` at seeds. In a
    /// forward space it is the predecessor's upward arc into the state; in a
    /// backward space the state's downward arc to its successor.
    pub(crate) fn arc(&self, position: usize, forward: bool) -> Option<HierarchyArc> {
        let entry = self.entries[position];
        if entry.arc == NO_PREVIOUS_ARC {
            return None;
        }
        Some(if forward {
            HierarchyArc::upward(
                self.entries[entry.parent as usize].edge_index,
                entry.arc as usize,
            )
        } else {
            HierarchyArc::downward(entry.edge_index, entry.arc as usize)
        })
    }
}

fn space_entries(dist: &[f64], parent: &[u32], touched: &[u32]) -> Vec<CchSpaceEntry> {
    touched
        .iter()
        .map(|&edge_index| CchSpaceEntry {
            edge_index,
            cost: dist[edge_index as usize],
            arc: NO_PREVIOUS_ARC,
            parent: parent[edge_index as usize],
        })
        .collect()
}

pub(crate) fn build_forward_cch_space(
    routing_graph: &RoutingGraph,
    seeds: &[(usize, f64)],
) -> CchSearchSpace {
    let acceleration = routing_graph
        .acceleration
        .as_ref()
        .expect("many-to-many CCH space requires acceleration");
    let source = acceleration.source.as_ref();

    ACCELERATION_SEARCH_SCRATCH.with(|scratch| {
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
            let rank = source.edge_rank[edge_index] as usize;
            for (head_rank, weight) in acceleration.ranked.upward_row(rank) {
                let next_edge = source.edge_order[head_rank as usize] as usize;
                let next_cost = cost + netweevil_core::decode_cch_weight(weight);
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
        }

        CchSearchSpace::from_unsorted(
            space_entries(
                &scratch.forward_dist,
                &scratch.forward_parent,
                &scratch.forward_touched,
            ),
            |edge, previous| acceleration.upward_arc_slot(previous as usize, edge),
        )
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
    let source = acceleration.source.as_ref();

    ACCELERATION_SEARCH_SCRATCH.with(|scratch| {
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
            let rank = source.edge_rank[edge_index] as usize;
            for (tail_rank, weight) in acceleration.ranked.downward_row(rank) {
                let previous_edge = source.edge_order[tail_rank as usize] as usize;
                let next_cost = cost + netweevil_core::decode_cch_weight(weight);
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

        CchSearchSpace::from_unsorted(
            space_entries(
                &scratch.backward_dist,
                &scratch.backward_parent,
                &scratch.backward_touched,
            ),
            |edge, next| acceleration.downward_arc_slot(edge as usize, next),
        )
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
/// spaces, mirroring `reconstruct_pairwise_path` on the pairwise scratch.
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

    let mut forward_arcs = Vec::new();
    let mut position = forward
        .position(meeting_edge)
        .expect("meeting edge settled in forward space");
    while let Some(arc) = forward.arc(position, true) {
        forward_arcs.push(arc);
        position = forward.entries[position].parent as usize;
    }

    let mut edge_indexes = vec![forward.entries[position].edge_index as usize];
    let mut unpack_stack = UnpackStack::default();
    for arc in forward_arcs.into_iter().rev() {
        push_unpacked_arc(acceleration, &mut unpack_stack, arc, &mut edge_indexes);
    }

    let mut position = backward
        .position(meeting_edge)
        .expect("meeting edge settled in backward space");
    while let Some(arc) = backward.arc(position, false) {
        push_unpacked_arc(acceleration, &mut unpack_stack, arc, &mut edge_indexes);
        position = backward.entries[position].parent as usize;
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
                    let next_cost = cost + routing_graph.reverse_transition_cost(transition_index);
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

#[cfg(test)]
mod tests;
