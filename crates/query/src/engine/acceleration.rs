use std::collections::BinaryHeap;

use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

use anyhow::Result;
#[cfg(test)]
use anyhow::bail;
use netweevil_core::{DatasetAccelerationBundle, TopologyBundle};

use crate::*;

/// Lazily filled lower-triangle witnesses for every hierarchy arc of one
/// customized metric, shared by all threads querying it.
///
/// Each entry is [`UNPACK_UNKNOWN`] until first unpacked, [`UNPACK_BASE_ARC`]
/// for an arc without a lower triangle of equal weight, or the triangle's
/// middle rank plus one. Ranks stay below `u32::MAX`, so the encoding holds
/// for any arc count. The arrays are zero-allocated, so only pages of arcs
/// that some route has unpacked are ever committed.
pub(crate) struct UnpackTable {
    upward: Box<[AtomicU32]>,
    downward: Box<[AtomicU32]>,
}

const UNPACK_UNKNOWN: u32 = 0;
const UNPACK_BASE_ARC: u32 = u32::MAX;

impl UnpackTable {
    pub(crate) fn new(upward_len: usize, downward_len: usize) -> Self {
        Self {
            upward: bytemuck::zeroed_slice_box(upward_len),
            downward: bytemuck::zeroed_slice_box(downward_len),
        }
    }

    fn entry(&self, arc: HierarchyArc) -> &AtomicU32 {
        if arc.is_upward() {
            &self.upward[arc.position]
        } else {
            &self.downward[arc.position]
        }
    }
}

#[derive(Default)]
pub(crate) struct BidirectionalAccelerationScratch {
    /// Forward costs and previous arcs.
    forward: SearchLabels,
    forward_heap: BinaryHeap<State>,
    /// Backward costs and next arcs.
    backward: SearchLabels,
    backward_heap: BinaryHeap<State>,
}

impl BidirectionalAccelerationScratch {
    fn prepare(&mut self, edge_count: usize) {
        self.forward.prepare(edge_count);
        self.backward.prepare(edge_count);
        clear_heap(&mut self.forward_heap);
        clear_heap(&mut self.backward_heap);
    }

    fn update_forward(&mut self, edge_index: usize, cost: f64, previous_arc: u32) -> bool {
        self.forward.update(edge_index, cost, previous_arc)
    }

    fn update_backward(&mut self, edge_index: usize, cost: f64, next_arc: u32) -> bool {
        self.backward.update(edge_index, cost, next_arc)
    }
}

/// One hierarchy arc: its tail and head ranks and its position among the
/// upward arcs (tail below head) or the downward arcs (tail above head).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HierarchyArc {
    pub(crate) tail: u32,
    pub(crate) head: u32,
    pub(crate) position: usize,
}

impl HierarchyArc {
    /// The arc `tail -> head` (ranks), if the hierarchy has it.
    pub(crate) fn between(
        source: &DatasetAccelerationBundle,
        tail: u32,
        head: u32,
    ) -> Option<Self> {
        let position = if tail < head {
            source.upward_position(tail, head)?
        } else {
            source.downward_position(tail, head)?
        };
        Some(Self {
            tail,
            head,
            position,
        })
    }

    pub(crate) fn is_upward(self) -> bool {
        self.tail < self.head
    }

    /// Edge state of the tail.
    pub(crate) fn tail_edge(self, source: &DatasetAccelerationBundle) -> u32 {
        source.edge_order[self.tail as usize]
    }

    /// Edge state of the head: the later state on the unpacked path.
    pub(crate) fn head_edge(self, source: &DatasetAccelerationBundle) -> u32 {
        source.edge_order[self.head as usize]
    }

    /// The two child arcs of a lower triangle through `middle`:
    /// `tail -> middle` downward, then `middle -> head` upward.
    pub(crate) fn children(self, source: &DatasetAccelerationBundle, middle: u32) -> (Self, Self) {
        let child = |tail, head| {
            Self::between(source, tail, head).expect("triangle children are hierarchy arcs")
        };
        (child(self.tail, middle), child(middle, self.head))
    }
}

/// Reusable stack of [`push_unpacked_arc`].
#[derive(Debug, Default)]
pub(crate) struct UnpackStack(Vec<HierarchyArc>);

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
    let source = acceleration.source.as_ref();
    let stack = &mut stack.0;
    debug_assert!(stack.is_empty());
    stack.push(arc);
    while let Some(arc) = stack.pop() {
        let Some(middle) = unpack_witness(acceleration, arc) else {
            edge_indexes.push(arc.head_edge(source) as usize);
            continue;
        };
        let (left, right) = arc.children(source, middle);
        // Process left before right: LIFO stack, so push right first.
        stack.push(right);
        stack.push(left);
    }
}

/// The middle rank of the lower triangle `tail -> middle -> head` that
/// unpacks an arc: the first lower neighbour of `tail`, in the order of its
/// downward arcs (by the middle's edge index), that ranks below `head` and
/// whose child weights sum exactly to the arc weight. `None` means the arc
/// is a base transition. Every unpacking path reads the same memoized
/// witness from the graph's [`UnpackTable`], so summaries and explicit
/// paths agree on the chosen decomposition.
pub(crate) fn unpack_witness(acceleration: &AccelerationGraph, arc: HierarchyArc) -> Option<u32> {
    let entry = acceleration.unpack.entry(arc);
    let mut witness = entry.load(AtomicOrdering::Relaxed);
    if witness == UNPACK_UNKNOWN {
        witness =
            find_unpack_witness(acceleration, arc).map_or(UNPACK_BASE_ARC, |middle| middle + 1);
        entry.store(witness, AtomicOrdering::Relaxed);
    }
    (witness != UNPACK_BASE_ARC).then(|| witness - 1)
}

fn find_unpack_witness(acceleration: &AccelerationGraph, arc: HierarchyArc) -> Option<u32> {
    let source = acceleration.source.as_ref();
    let ranked = &acceleration.ranked;
    let weight = ranked.weight(arc.is_upward(), arc.position);
    assert!(
        weight < netweevil_core::CCH_WEIGHT_OVERFLOW,
        "only finite CCH arcs can be unpacked"
    );
    source.downward_head[source.downward_out_range(arc.tail as usize)]
        .iter()
        .copied()
        .find(|&middle| {
            if middle >= arc.head {
                return false;
            }
            let Some(right) = source.upward_position(middle, arc.head) else {
                return false;
            };
            let left = source
                .downward_position(arc.tail, middle)
                .expect("downward arc is in its head's row");
            netweevil_core::add_cch_weights(ranked.weight(false, left), ranked.weight(true, right))
                == weight
        })
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
    /// Position of the predecessor upward arc (forward) or successor
    /// downward arc (backward); [`NO_SPACE_ARC`] at seeds.
    pub(crate) arc: u64,
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
    /// held in `parent` until now, to its entry position and the arc
    /// position `arc_position(state, neighbour)`.
    fn from_unsorted(
        mut entries: Vec<CchSpaceEntry>,
        arc_position: impl Fn(u32, u32) -> Option<usize>,
    ) -> Self {
        entries.sort_unstable_by_key(|entry| entry.edge_index);
        for index in 0..entries.len() {
            let neighbour = entries[index].parent;
            if neighbour == NO_PREVIOUS_ARC {
                continue;
            }
            entries[index].arc = arc_position(entries[index].edge_index, neighbour)
                .expect("search-space parent is joined by a hierarchy arc")
                as u64;
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
    pub(crate) fn arc(
        &self,
        source: &DatasetAccelerationBundle,
        position: usize,
        forward: bool,
    ) -> Option<HierarchyArc> {
        let entry = self.entries[position];
        if entry.arc == NO_SPACE_ARC {
            return None;
        }
        let state = source.edge_rank[entry.edge_index as usize];
        let neighbour = source.edge_rank[self.entries[entry.parent as usize].edge_index as usize];
        let (tail, head) = if forward {
            (neighbour, state)
        } else {
            (state, neighbour)
        };
        Some(HierarchyArc {
            tail,
            head,
            position: entry.arc as usize,
        })
    }
}

fn space_entries(labels: &SearchLabels) -> Vec<CchSpaceEntry> {
    labels
        .touched
        .iter()
        .map(|&edge_index| CchSpaceEntry {
            edge_index,
            cost: labels.cost.get(edge_index as usize),
            arc: NO_SPACE_ARC,
            parent: labels.link.get(edge_index as usize),
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
            if cost > scratch.forward.cost.get(edge_index) {
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

        CchSearchSpace::from_unsorted(space_entries(&scratch.forward), |edge, previous| {
            source.upward_position(
                source.edge_rank[previous as usize],
                source.edge_rank[edge as usize],
            )
        })
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
            if cost > scratch.backward.cost.get(edge_index) {
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

        CchSearchSpace::from_unsorted(space_entries(&scratch.backward), |edge, next| {
            source.downward_position(
                source.edge_rank[edge as usize],
                source.edge_rank[next as usize],
            )
        })
    })
}

/// [`CchSpaceEntry::arc`] of a seed.
pub(crate) const NO_SPACE_ARC: u64 = u64::MAX;

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
    let source = acceleration.source.as_ref();
    let mut position = forward
        .position(meeting_edge)
        .expect("meeting edge settled in forward space");
    while let Some(arc) = forward.arc(source, position, true) {
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
    while let Some(arc) = backward.arc(source, position, false) {
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
                if cost > scratch.forward.cost.get(edge_index) {
                    continue;
                }
                if scratch.backward.cost.get(edge_index).is_finite() {
                    let candidate_cost = cost + scratch.backward.cost.get(edge_index);
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
                if cost > scratch.backward.cost.get(edge_index) {
                    continue;
                }
                if scratch.forward.cost.get(edge_index).is_finite() {
                    let candidate_cost = scratch.forward.cost.get(edge_index) + cost;
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
                scratch,
                meeting_edge,
                best_cost,
            ));
        }

        Ok(best_path)
    })
}

#[cfg(test)]
mod tests;
