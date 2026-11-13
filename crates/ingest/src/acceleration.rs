use anyhow::Result;
use netweevil_core::{
    ACCELERATION_BUNDLE_SCHEMA_VERSION, AccelerationBundleStats, CCH_ALGORITHM, CacheBundleId,
    DatasetAccelerationBundle, TopologyBundle,
};
use rayon::prelude::*;

use crate::import::{DatasetImportProgress, DatasetImportStage, emit_progress};
use crate::topology::checked_u32;

use dissection::{LEAF_NODES, build_edge_cell, dissect};

/// Builds the metric-independent CCH topology for a dataset.
///
/// Edge states are ordered by nested dissection (separators last), then
/// contracted in that order via the elimination game: contracting state `v`
/// inserts a shortcut `x -> y` for every pair of arcs `x -> v` and `v -> y`
/// whose other endpoints outrank `v`. The insertion is complete (no budgets),
/// which is what makes hierarchy queries exact without any follow-up search
/// on the base graph.
pub(crate) fn build_dataset_acceleration_bundle_with_progress(
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<DatasetAccelerationBundle> {
    let edge_count = topology.edge_count();

    emit_progress(
        progress,
        DatasetImportStage::BuildAcceleration,
        Some(0.0),
        format!("Ordering {edge_count} edge states by nested dissection"),
    );

    let edge_order = nested_dissection_order(topology)?;

    build_with_order(topology, source_topology_bundle_id, edge_order, progress)
}

/// Full nested-dissection elimination order of the topology's edge states.
pub(crate) fn nested_dissection_order(topology: &TopologyBundle) -> Result<Vec<u32>> {
    dissect(build_edge_cell(topology)?, LEAF_NODES)
}

/// An elimination order carried over from a previous build of the dataset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PatchedOrder {
    pub(crate) order: Vec<u32>,
    /// Edge states that kept their previous relative rank.
    pub(crate) reused: usize,
    /// Edge states without a previous rank.
    pub(crate) inserted: usize,
}

/// Derives an elimination order for `topology` from the ranks its edge
/// states had in a previous build (`previous_rank[edge]`, `None` for new
/// states).
///
/// Reused states keep their relative order, so the nested-dissection cells
/// and separators survive. A new state is placed directly after the
/// highest-ranked neighbour that already has a place: a state adjacent to
/// both sides of a separator lands in that separator, and one inside a cell
/// stays in that cell. States reachable only through other new states are
/// placed in breadth-first layers from the reused ones. New components with
/// no reused state are ordered by nested dissection and go first.
///
/// Every permutation yields a complete hierarchy, so the order only affects
/// shortcut count, never query results.
pub(crate) fn patch_edge_order(
    topology: &TopologyBundle,
    previous_rank: &[Option<u32>],
) -> Result<PatchedOrder> {
    const UNPLACED: u32 = u32::MAX;
    let edge_count = topology.edge_count();
    // Edge ids stay below the `UNPLACED` marker.
    checked_u32(edge_count, "edge states")?;
    assert_eq!(
        previous_rank.len(),
        edge_count,
        "one previous rank per edge"
    );
    let transitions = &topology.edge_based_topology;
    let node_count = topology.nodes.len();

    // Outgoing edges of every node come from the topology; incoming ones
    // are gathered here, so an edge state's neighbours are the edges leaving
    // its head and the edges entering its tail.
    let mut incoming_first = vec![0_u32; node_count + 1];
    for edge in &topology.edge_layers.routing {
        incoming_first[edge.to.0 as usize + 1] += 1;
    }
    for node in 0..node_count {
        incoming_first[node + 1] += incoming_first[node];
    }
    let mut cursor = incoming_first[..node_count].to_vec();
    let mut incoming_edges = vec![0_u32; edge_count];
    for (index, edge) in topology.edge_layers.routing.iter().enumerate() {
        let slot = &mut cursor[edge.to.0 as usize];
        incoming_edges[*slot as usize] = index as u32;
        *slot += 1;
    }
    let neighbours = |edge: usize| {
        let routing = &topology.edge_layers.routing[edge];
        let head = routing.to.0 as usize;
        let tail = routing.from.0 as usize;
        transitions.node_edge_order[transitions.node_first_out[head] as usize
            ..transitions.node_first_out[head + 1] as usize]
            .iter()
            .chain(
                &incoming_edges[incoming_first[tail] as usize..incoming_first[tail + 1] as usize],
            )
            .copied()
            .filter(move |&neighbour| neighbour as usize != edge)
    };

    let mut anchor = previous_rank
        .iter()
        .map(|rank| rank.unwrap_or(UNPLACED))
        .collect::<Vec<_>>();
    let mut sequence = vec![0_u32; edge_count];
    let mut next_sequence = 0_u32;
    let mut queued = vec![false; edge_count];
    let mut layer = (0..edge_count)
        .filter(|&edge| {
            previous_rank[edge].is_none()
                && neighbours(edge).any(|neighbour| previous_rank[neighbour as usize].is_some())
        })
        .collect::<Vec<_>>();
    for &edge in &layer {
        queued[edge] = true;
    }
    let mut layer_anchors = Vec::new();
    while !layer.is_empty() {
        // Anchors come from states placed before this layer only, so the
        // result does not depend on the order within a layer.
        layer_anchors.clear();
        layer_anchors.extend(layer.iter().map(|&edge| {
            neighbours(edge)
                .map(|neighbour| anchor[neighbour as usize])
                .filter(|&rank| rank != UNPLACED)
                .max()
                .expect("a layer state has a placed neighbour")
        }));
        for (&edge, &edge_anchor) in layer.iter().zip(&layer_anchors) {
            anchor[edge] = edge_anchor;
            sequence[edge] = next_sequence;
            next_sequence += 1;
        }
        let mut next_layer = Vec::new();
        for &edge in &layer {
            for neighbour in neighbours(edge) {
                let neighbour = neighbour as usize;
                if anchor[neighbour] == UNPLACED && !queued[neighbour] {
                    queued[neighbour] = true;
                    next_layer.push(neighbour);
                }
            }
        }
        layer = next_layer;
    }

    let isolated = (0..edge_count as u32)
        .filter(|&edge| anchor[edge as usize] == UNPLACED)
        .collect::<Vec<_>>();
    let mut order = if isolated.is_empty() {
        Vec::with_capacity(edge_count)
    } else {
        dissect(build_edge_cell(topology)?.induced(&isolated), LEAF_NODES)?
    };
    let mut placed = (0..edge_count as u32)
        .filter(|&edge| anchor[edge as usize] != UNPLACED)
        .collect::<Vec<_>>();
    placed.par_sort_unstable_by_key(|&edge| {
        let edge = edge as usize;
        (anchor[edge], previous_rank[edge].is_none(), sequence[edge])
    });
    order.extend(placed);
    let reused = previous_rank.iter().flatten().count();
    Ok(PatchedOrder {
        order,
        reused,
        inserted: edge_count - reused,
    })
}

/// Builds the CCH bundle for `topology` contracted in `edge_order`.
pub(crate) fn build_with_order(
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
    edge_order: Vec<u32>,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<DatasetAccelerationBundle> {
    let edge_count = topology.edge_count();
    checked_u32(edge_count, "edge states")?;
    let mut edge_rank = vec![0_u32; edge_count];
    for (rank, &edge_index) in edge_order.iter().enumerate() {
        edge_rank[edge_index as usize] = rank as u32;
    }

    emit_progress(
        progress,
        DatasetImportStage::BuildAcceleration,
        Some(0.0),
        format!("Building CCH 0% ({edge_count} edge states)"),
    );
    let contraction = contraction::contract(topology, &edge_order, &edge_rank, progress)?;
    let stats = AccelerationBundleStats {
        base_arc_count: contraction.base_arc_count,
        shortcut_arc_count: contraction.shortcut_arc_count,
        total_arc_count: contraction.base_arc_count + contraction.shortcut_arc_count,
    };
    emit_progress(
        progress,
        DatasetImportStage::BuildAcceleration,
        Some(100.0),
        format!(
            "Building CCH 100% ({} arcs: {} base, {} shortcuts)",
            stats.total_arc_count, stats.base_arc_count, stats.shortcut_arc_count
        ),
    );

    Ok(DatasetAccelerationBundle {
        schema_version: ACCELERATION_BUNDLE_SCHEMA_VERSION,
        source_topology_bundle_id,
        algorithm: CCH_ALGORITHM.to_string(),
        stats,
        edge_order,
        edge_rank,
        upward_first_out: contraction.upward_first_out,
        upward_head: contraction.upward_head,
        downward_first_out: contraction.downward_first_out,
        downward_head: contraction.downward_head,
    })
}

mod contraction;
mod dissection;
mod flow;
#[cfg(test)]
mod tests;
