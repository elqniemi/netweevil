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
    position: u64,
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
    let base_arcs = CostBaseArcs::new(topology, metrics);
    let (upward, downward) = base.ranked.weights();
    let (mut upward, mut downward) = (upward.to_vec(), downward.to_vec());

    // The arc `tail -> head` (ranks) as a ranked position: upward when the
    // head ranks higher.
    let arc = |tail: u32, head: u32| -> Option<RankedArc> {
        let arc = HierarchyArc::between(source, tail, head)?;
        Some(RankedArc {
            is_downward: !arc.is_upward(),
            position: arc.position as u64,
        })
    };

    let mut queue = BinaryHeap::<Reverse<(u32, RankedArc, u32, u32)>>::new();
    let mut queued = [
        vec![0_u64; upward.len().div_ceil(64)],
        vec![0_u64; downward.len().div_ceil(64)],
    ];
    let mut enqueue = |queue: &mut BinaryHeap<_>, tail: u32, head: u32| {
        if tail == head {
            return;
        }
        let Some(target) = arc(tail, head) else {
            return;
        };
        let bits = &mut queued[usize::from(target.is_downward)];
        let (word, bit) = ((target.position / 64) as usize, target.position % 64);
        if bits[word] & (1 << bit) == 0 {
            bits[word] |= 1 << bit;
            queue.push(Reverse((tail.min(head), target, tail, head)));
        }
    };

    // Base arcs of the transitions into and out of every changed edge. Edges
    // the base graph cannot route on keep infinite base arcs, so only its
    // incoming edges can lead into a changed edge with a new weight.
    let rank = |edge: u32| source.edge_rank[edge as usize];
    let transitions = &topology.edge_based_topology;
    for &edge in changed_edges {
        for &previous in
            base_graph.incoming_edges(topology.routing_edge(edge as usize).from.0 as usize)
        {
            enqueue(&mut queue, rank(previous), rank(edge));
        }
        for &next in &transitions.edge_transition_edges[transitions.edge_transition_first_out
            [edge as usize] as usize
            ..transitions.edge_transition_first_out[edge as usize + 1] as usize]
        {
            enqueue(&mut queue, rank(edge), rank(next));
        }
    }

    while let Some(Reverse((_, target, tail, head))) = queue.pop() {
        let mut value = base_arcs.base_weight(
            source.edge_order[tail as usize] as usize,
            source.edge_order[head as usize] as usize,
        );
        // Lower triangles `tail -> middle -> head` with middle below both.
        for &middle in &source.downward_head[source.downward_out_range(tail as usize)] {
            if middle >= head {
                continue;
            }
            let Some(right) = source.upward_position(middle, head) else {
                continue;
            };
            let left = source
                .downward_position(tail, middle)
                .expect("downward arc is in its head's row");
            let (left_weight, right_weight) = (downward[left], upward[right]);
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
            for &y in &source.upward_head[source.upward_range(head as usize)] {
                enqueue(&mut queue, tail, y);
            }
        } else {
            // `tail -> head` is the right child of `x -> head` for every
            // downward arc `x -> tail`.
            for &x in &source.downward_tail[source.downward_range(tail as usize)] {
                enqueue(&mut queue, x, head);
            }
        }
    }
    Some(base.ranked.with_weights(upward.into(), downward.into()))
}

#[cfg(test)]
mod tests;
