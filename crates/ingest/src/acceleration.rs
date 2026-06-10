use std::collections::HashSet;

use netweevil_core::{
    ACCELERATION_BUNDLE_SCHEMA_VERSION, AccelerationBundleStats, CCH_ALGORITHM, CacheBundleId,
    DatasetAccelerationBundle, TopologyBundle,
};

use crate::import::{DatasetImportProgress, DatasetImportStage, PercentReporter, emit_progress};

#[cfg(test)]
pub(crate) fn build_dataset_acceleration_bundle(
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
) -> DatasetAccelerationBundle {
    build_dataset_acceleration_bundle_with_progress(
        topology,
        source_topology_bundle_id,
        &mut |_| {},
    )
}

/// Builds the metric-independent CCH topology for a dataset.
///
/// Edge states are ordered by recursive geometric bisection (separators
/// last), then contracted in that order via the elimination game: contracting
/// state `v` inserts a shortcut `x -> y` for every pair of arcs `x -> v` and
/// `v -> y` whose other endpoints outrank `v`. The insertion is complete (no
/// budgets), which is what makes hierarchy queries exact without any
/// follow-up search on the base graph.
pub(crate) fn build_dataset_acceleration_bundle_with_progress(
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> DatasetAccelerationBundle {
    let transition_topology = &topology.edge_based_topology;
    let edge_count = topology.edge_count();
    let mut in_degree = vec![0_u32; edge_count];
    let mut out_degree = vec![0_u32; edge_count];

    let has_transitions = transition_topology.edge_transition_first_out.len() == edge_count + 1;
    if has_transitions {
        for (edge_index, degree) in out_degree.iter_mut().enumerate() {
            let start = transition_topology.edge_transition_first_out[edge_index] as usize;
            let end = transition_topology.edge_transition_first_out[edge_index + 1] as usize;
            *degree = end.saturating_sub(start) as u32;
            for &next_edge in &transition_topology.edge_transition_edges[start..end] {
                if let Some(entry) = in_degree.get_mut(next_edge as usize) {
                    *entry += 1;
                }
            }
        }
    }

    let edge_order = build_recursive_spatial_edge_order(topology, &in_degree, &out_degree);

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

    // Arc store: (tail, head). Pending lists hold the arcs each still-active
    // state will consume when it is contracted; an arc is owned by its
    // lower-ranked endpoint, so each list is final by contraction time.
    let mut arcs = Vec::<(u32, u32)>::new();
    let mut seen_arc_pairs = HashSet::<u64>::new();
    let mut out_to_higher = vec![Vec::<u32>::new(); edge_count];
    let mut in_from_higher = vec![Vec::<u32>::new(); edge_count];

    let push_arc = |tail: u32,
                    head: u32,
                    arcs: &mut Vec<(u32, u32)>,
                    out_to_higher: &mut [Vec<u32>],
                    in_from_higher: &mut [Vec<u32>]| {
        let arc_id = arcs.len() as u32;
        arcs.push((tail, head));
        if edge_rank[tail as usize] < edge_rank[head as usize] {
            out_to_higher[tail as usize].push(arc_id);
        } else {
            in_from_higher[head as usize].push(arc_id);
        }
    };

    if has_transitions {
        for edge_index in 0..edge_count {
            let start = transition_topology.edge_transition_first_out[edge_index] as usize;
            let end = transition_topology.edge_transition_first_out[edge_index + 1] as usize;
            for &next_edge in &transition_topology.edge_transition_edges[start..end] {
                if next_edge as usize == edge_index || next_edge as usize >= edge_count {
                    continue;
                }
                if !seen_arc_pairs.insert(arc_pair_key(edge_index as u32, next_edge)) {
                    continue;
                }
                push_arc(
                    edge_index as u32,
                    next_edge,
                    &mut arcs,
                    &mut out_to_higher,
                    &mut in_from_higher,
                );
            }
        }
    }
    let base_arc_count = arcs.len();

    let mut reporter = PercentReporter::starting_at_zero();
    for (order_index, &contracted_edge) in edge_order.iter().enumerate() {
        let contracted_edge = contracted_edge as usize;
        let incoming = std::mem::take(&mut in_from_higher[contracted_edge]);
        let outgoing = std::mem::take(&mut out_to_higher[contracted_edge]);
        for &incoming_arc in &incoming {
            let tail = arcs[incoming_arc as usize].0;
            for &outgoing_arc in &outgoing {
                let head = arcs[outgoing_arc as usize].1;
                if tail == head {
                    continue;
                }
                if !seen_arc_pairs.insert(arc_pair_key(tail, head)) {
                    continue;
                }
                push_arc(
                    tail,
                    head,
                    &mut arcs,
                    &mut out_to_higher,
                    &mut in_from_higher,
                );
            }
        }
        reporter.emit_if_needed(
            (order_index + 1) as u64,
            edge_order.len() as u64,
            DatasetImportStage::BuildAcceleration,
            progress,
            |percent| {
                format!(
                    "Building CCH {percent:.0}% ({}/{}) with {} arcs ({} base, {} shortcuts)",
                    order_index + 1,
                    edge_order.len(),
                    arcs.len(),
                    base_arc_count,
                    arcs.len() - base_arc_count
                )
            },
        );
    }
    drop(seen_arc_pairs);
    drop(out_to_higher);
    drop(in_from_higher);

    emit_progress(
        progress,
        DatasetImportStage::BuildAcceleration,
        Some(100.0),
        format!(
            "Building CCH 100% ({} arcs: {} base, {} shortcuts)",
            arcs.len(),
            base_arc_count,
            arcs.len() - base_arc_count
        ),
    );

    let stats = AccelerationBundleStats {
        base_arc_count: base_arc_count as u64,
        shortcut_arc_count: (arcs.len() - base_arc_count) as u64,
        total_arc_count: arcs.len() as u64,
    };

    let mut upward = Vec::new();
    let mut downward = Vec::new();
    for &(tail, head) in &arcs {
        if edge_rank[tail as usize] < edge_rank[head as usize] {
            upward.push((tail, head));
        } else {
            downward.push((tail, head));
        }
    }
    drop(arcs);
    // Rows sorted by head id enable binary-search arc lookup during
    // customization and unpacking.
    upward.sort_unstable();
    downward.sort_unstable();

    let (upward_first_out, upward_head) = arcs_to_csr(&upward, edge_count);
    let (downward_first_out, downward_head) = arcs_to_csr(&downward, edge_count);

    DatasetAccelerationBundle {
        schema_version: ACCELERATION_BUNDLE_SCHEMA_VERSION,
        source_topology_bundle_id,
        algorithm: CCH_ALGORITHM.to_string(),
        stats,
        edge_order,
        edge_rank,
        upward_first_out,
        upward_head,
        downward_first_out,
        downward_head,
    }
}

fn arcs_to_csr(sorted_arcs: &[(u32, u32)], edge_count: usize) -> (Vec<u32>, Vec<u32>) {
    let mut first_out = vec![0_u32; edge_count + 1];
    let mut heads = vec![0_u32; sorted_arcs.len()];
    for (slot, &(tail, head)) in sorted_arcs.iter().enumerate() {
        first_out[tail as usize + 1] += 1;
        heads[slot] = head;
    }
    for edge_index in 0..edge_count {
        first_out[edge_index + 1] += first_out[edge_index];
    }
    (first_out, heads)
}

fn arc_pair_key(tail: u32, head: u32) -> u64 {
    ((tail as u64) << 32) | head as u64
}

fn build_recursive_spatial_edge_order(
    topology: &TopologyBundle,
    in_degree: &[u32],
    out_degree: &[u32],
) -> Vec<u32> {
    const LEAF_SIZE: usize = 1_024;

    fn sort_small_block(
        edges: &mut [u32],
        topology: &TopologyBundle,
        in_degree: &[u32],
        out_degree: &[u32],
    ) {
        edges.sort_unstable_by_key(|&edge_index| {
            let edge = topology.routing_edge(edge_index as usize);
            (
                out_degree[edge_index as usize] + in_degree[edge_index as usize],
                out_degree[edge_index as usize],
                in_degree[edge_index as usize],
                edge.from.0,
                edge.to.0,
                edge_index,
            )
        });
    }

    fn recurse(
        topology: &TopologyBundle,
        in_degree: &[u32],
        out_degree: &[u32],
        edges: &mut [u32],
        output: &mut Vec<u32>,
    ) {
        if edges.len() <= LEAF_SIZE {
            sort_small_block(edges, topology, in_degree, out_degree);
            output.extend_from_slice(edges);
            return;
        }

        let mut min_lon = f64::INFINITY;
        let mut max_lon = f64::NEG_INFINITY;
        let mut min_lat = f64::INFINITY;
        let mut max_lat = f64::NEG_INFINITY;
        let mut coords = Vec::with_capacity(edges.len());
        for &edge_index in edges.iter() {
            let edge = topology.routing_edge(edge_index as usize);
            let from = &topology.nodes[edge.from.0 as usize];
            let to = &topology.nodes[edge.to.0 as usize];
            let lon = (from.lon + to.lon) * 0.5;
            let lat = (from.lat + to.lat) * 0.5;
            min_lon = min_lon.min(lon);
            max_lon = max_lon.max(lon);
            min_lat = min_lat.min(lat);
            max_lat = max_lat.max(lat);
            coords.push((edge_index, lon, lat));
        }

        let split_lon = (max_lon - min_lon) >= (max_lat - min_lat);
        let mut axis_values = coords
            .iter()
            .map(|(_, lon, lat)| if split_lon { *lon } else { *lat })
            .collect::<Vec<_>>();
        axis_values
            .sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
        let pivot = axis_values[axis_values.len() / 2];

        let mut left = Vec::new();
        let mut right = Vec::new();
        let mut separator = Vec::new();
        for (edge_index, _, _) in coords {
            let edge = topology.routing_edge(edge_index as usize);
            let from = &topology.nodes[edge.from.0 as usize];
            let to = &topology.nodes[edge.to.0 as usize];
            let from_axis = if split_lon { from.lon } else { from.lat };
            let to_axis = if split_lon { to.lon } else { to.lat };
            if from_axis <= pivot && to_axis <= pivot {
                left.push(edge_index);
            } else if from_axis > pivot && to_axis > pivot {
                right.push(edge_index);
            } else {
                separator.push(edge_index);
            }
        }

        if left.is_empty() || right.is_empty() || separator.len() == edges.len() {
            sort_small_block(edges, topology, in_degree, out_degree);
            output.extend_from_slice(edges);
            return;
        }

        recurse(topology, in_degree, out_degree, &mut left, output);
        recurse(topology, in_degree, out_degree, &mut right, output);
        sort_small_block(&mut separator, topology, in_degree, out_degree);
        output.extend(separator);
    }

    let mut edge_order = (0..topology.edge_count() as u32).collect::<Vec<_>>();
    let mut ordered = Vec::with_capacity(edge_order.len());
    recurse(
        topology,
        in_degree,
        out_degree,
        &mut edge_order,
        &mut ordered,
    );
    ordered
}

#[cfg(test)]
mod tests {
    use super::build_dataset_acceleration_bundle;
    use crate::test_util::edge;
    use crate::topology::build_edge_based_topology;
    use netweevil_core::{
        ACCELERATION_BUNDLE_SCHEMA_VERSION, CCH_ALGORITHM, CacheBundleId, NodeId, TopologyBundle,
        TopologyNode,
    };

    fn line_topology() -> TopologyBundle {
        let edges = vec![edge(0, 0, 1, 10), edge(1, 1, 2, 11), edge(2, 2, 3, 12)];
        TopologyBundle {
            schema_version: 1,
            source_path: "test".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 100,
                    lon: 0.0,
                    lat: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 101,
                    lon: 1.0,
                    lat: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 102,
                    lon: 2.0,
                    lat: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(3),
                    osm_node_id: 103,
                    lon: 3.0,
                    lat: 0.0,
                },
            ],
            edge_layers: Default::default(),
            edges: edges.clone(),
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: build_edge_based_topology(4, &edges),
            spatial_index: None,
            node_component_ids: vec![0, 0, 0, 0],
            edge_component_ids: vec![0, 0, 0, 0],
        }
    }

    #[test]
    fn builds_complete_cch_bundle_for_line_topology() {
        let topology = line_topology();
        let bundle =
            build_dataset_acceleration_bundle(&topology, CacheBundleId::new("topology-test"));

        assert_eq!(bundle.schema_version, ACCELERATION_BUNDLE_SCHEMA_VERSION);
        assert_eq!(bundle.algorithm, CCH_ALGORITHM);
        assert_eq!(bundle.stats.base_arc_count, 2);
        // Contracting edge state 2 first (lowest degree) joins 1 -> 2 -> none;
        // contracting 0 next joins nothing new; the line produces exactly one
        // elimination shortcut candidate only when a middle state has both an
        // incoming and an outgoing arc to higher-ranked states.
        assert_eq!(
            bundle.stats.total_arc_count,
            bundle.stats.base_arc_count + bundle.stats.shortcut_arc_count
        );
        assert_eq!(bundle.edge_order.len(), 3);
        assert_eq!(bundle.edge_rank.len(), 3);
        assert_eq!(bundle.upward_first_out.len(), 4);
        assert_eq!(bundle.downward_first_out.len(), 4);
        assert_eq!(
            bundle.upward_head.len() + bundle.downward_head.len(),
            bundle.stats.total_arc_count as usize
        );
        // Every rank pairing must be strictly monotone per direction.
        for edge_index in 0..3_usize {
            for slot in bundle.upward_first_out[edge_index] as usize
                ..bundle.upward_first_out[edge_index + 1] as usize
            {
                let head = bundle.upward_head[slot] as usize;
                assert!(bundle.edge_rank[edge_index] < bundle.edge_rank[head]);
            }
            for slot in bundle.downward_first_out[edge_index] as usize
                ..bundle.downward_first_out[edge_index + 1] as usize
            {
                let head = bundle.downward_head[slot] as usize;
                assert!(bundle.edge_rank[edge_index] > bundle.edge_rank[head]);
            }
        }
    }

    #[test]
    fn cch_rows_are_sorted_by_head_for_binary_search() {
        let topology = line_topology();
        let bundle =
            build_dataset_acceleration_bundle(&topology, CacheBundleId::new("topology-test"));
        for edge_index in 0..3_usize {
            let row = &bundle.upward_head[bundle.upward_first_out[edge_index] as usize
                ..bundle.upward_first_out[edge_index + 1] as usize];
            assert!(row.windows(2).all(|pair| pair[0] < pair[1]));
            let row = &bundle.downward_head[bundle.downward_first_out[edge_index] as usize
                ..bundle.downward_first_out[edge_index + 1] as usize];
            assert!(row.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }
}
