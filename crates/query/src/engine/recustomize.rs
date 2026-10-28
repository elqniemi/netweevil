//! Exact re-customization of a hierarchy after a few edges change.
//!
//! Customization gives every arc `a = x -> y` the value
//! `min(base(a), min over lower triangles x -> v -> y of w(x -> v) + w(v -> y))`,
//! where every triangle middle `v` ranks below both endpoints. An arc's
//! value can therefore change only when its base weight changes or the value
//! of one of its triangle children changes. When some edges get a new cost
//! (or close), only base arcs of transitions into or out of those edges
//! change. Starting from the old customization, this module recomputes
//! exactly those arcs and then, in increasing order of their lower
//! endpoint's rank, every arc a changed value can reach. Each recomputed
//! arc uses the full formula over children that are already final, so every
//! weight equals what a complete customization of the new metric produces.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use netweevil_core::{CCH_WEIGHT_INFINITY, CompiledProfileBundle, TopologyBundle, add_cch_weights};
use netweevil_profile::CostBaseArcs;

use crate::*;

/// A hierarchy arc by its position in the ranked weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RankedArc {
    is_downward: bool,
    position: u32,
}

/// The ranked hierarchy of `metrics`, which differs from the metric
/// `base_graph` was built for only on `changed_edges`, whose generalized
/// cost may rise or whose routability may be lost. `topology` and the
/// dataset hierarchy must be the ones `base_graph` was built from, and the
/// turn costs must be unchanged.
pub(crate) fn recustomize_changed_edges(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    base_graph: &RoutingGraph,
    changed_edges: &[u32],
) -> Option<RankedCch> {
    let base = base_graph.acceleration.as_deref()?;
    let source = base.source.as_ref();
    let rank = |edge: usize| source.edge_rank[edge] as usize;
    let base_arcs = CostBaseArcs::new(topology, metrics);
    let (upward, downward) = base.ranked.ranked_weights();
    let (mut upward, mut downward) = (upward.to_vec(), downward.to_vec());

    // The arc `tail -> head` as a ranked position: upward when the head
    // ranks higher.
    let arc = |tail: usize, head: usize| -> Option<RankedArc> {
        if rank(tail) < rank(head) {
            let slot = base.upward_arc_slot(tail, head as u32)?;
            Some(RankedArc {
                is_downward: false,
                position: base.ranked.upward_position(source, tail, slot) as u32,
            })
        } else {
            Some(RankedArc {
                is_downward: true,
                position: base.ranked.downward_position(source, tail, head)? as u32,
            })
        }
    };

    let mut queue = BinaryHeap::<Reverse<(u32, RankedArc, u32, u32)>>::new();
    let mut queued = [
        vec![0_u64; upward.len().div_ceil(64)],
        vec![0_u64; downward.len().div_ceil(64)],
    ];
    let mut enqueue = |queue: &mut BinaryHeap<_>, tail: usize, head: usize| {
        if tail == head {
            return;
        }
        let Some(target) = arc(tail, head) else {
            return;
        };
        let bits = &mut queued[usize::from(target.is_downward)];
        let (word, bit) = (target.position as usize / 64, target.position % 64);
        if bits[word] & (1 << bit) == 0 {
            bits[word] |= 1 << bit;
            let lower = rank(tail).min(rank(head)) as u32;
            queue.push(Reverse((lower, target, tail as u32, head as u32)));
        }
    };

    // Base arcs of the transitions into and out of every changed edge. Edges
    // the base graph cannot route on keep infinite base arcs, so only its
    // incoming edges can lead into a changed edge with a new weight.
    let transitions = &topology.edge_based_topology;
    for &edge in changed_edges {
        let edge = edge as usize;
        for &previous in base_graph.incoming_edges(topology.routing_edge(edge).from.0 as usize) {
            enqueue(&mut queue, previous as usize, edge);
        }
        for &next in &transitions.edge_transition_edges[transitions.edge_transition_first_out[edge]
            as usize
            ..transitions.edge_transition_first_out[edge + 1] as usize]
        {
            enqueue(&mut queue, edge, next as usize);
        }
    }

    while let Some(Reverse((_, target, tail, head))) = queue.pop() {
        let (tail, head) = (tail as usize, head as usize);
        let mut value = base_arcs.base_weight(tail, head);
        // Lower triangles `tail -> middle -> head` with middle below both.
        for left in
            source.downward_first_out[tail] as usize..source.downward_first_out[tail + 1] as usize
        {
            let middle = source.downward_head[left] as usize;
            if rank(middle) >= rank(head) {
                continue;
            }
            let Some(right) = base.upward_arc_slot(middle, head as u32) else {
                continue;
            };
            let left_weight = downward[base
                .ranked
                .downward_position(source, tail, middle)
                .expect("downward arc is in its head's ranked row")];
            let right_weight = upward[base.ranked.upward_position(source, middle, right)];
            if left_weight == CCH_WEIGHT_INFINITY || right_weight == CCH_WEIGHT_INFINITY {
                continue;
            }
            value = value.min(add_cch_weights(left_weight, right_weight));
        }
        let weights = if target.is_downward {
            &mut downward
        } else {
            &mut upward
        };
        if weights[target.position as usize] == value {
            continue;
        }
        weights[target.position as usize] = value;
        if target.is_downward {
            // `tail -> head` is the left child of `tail -> y` for every
            // upward arc `head -> y`.
            for slot in
                source.upward_first_out[head] as usize..source.upward_first_out[head + 1] as usize
            {
                enqueue(&mut queue, tail, source.upward_head[slot] as usize);
            }
        } else {
            // `tail -> head` is the right child of `x -> head` for every
            // downward arc `x -> tail`.
            let row = base.ranked.downward_range(rank(tail));
            for position in row {
                let (x_rank, _) = base.ranked.downward_arc(position);
                enqueue(
                    &mut queue,
                    source.edge_order[x_rank as usize] as usize,
                    head,
                );
            }
        }
    }
    Some(base.ranked.with_ranked_weights(upward, downward))
}

#[cfg(test)]
mod tests;
