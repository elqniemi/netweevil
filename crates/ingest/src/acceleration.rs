use netweevil_core::{
    AccelerationBuildSettings, AccelerationBundleStats, CacheBundleId, DatasetAccelerationBundle,
    TopologyBundle,
};
use std::collections::HashSet;

use crate::import::{DatasetImportProgress, DatasetImportStage, PercentReporter, emit_progress};

#[cfg(test)]
fn build_dataset_acceleration_bundle(
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
) -> DatasetAccelerationBundle {
    build_dataset_acceleration_bundle_with_progress(
        topology,
        source_topology_bundle_id,
        AccelerationBuildSettings::default(),
        &mut |_| {},
    )
}

#[cfg(test)]
fn build_dataset_acceleration_bundle_with_settings(
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
    settings: AccelerationBuildSettings,
) -> DatasetAccelerationBundle {
    build_dataset_acceleration_bundle_with_progress(
        topology,
        source_topology_bundle_id,
        settings,
        &mut |_| {},
    )
}

pub(crate) fn build_dataset_acceleration_bundle_with_progress(
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
    settings: AccelerationBuildSettings,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> DatasetAccelerationBundle {
    let settings = settings.normalized();

    let transition_topology = &topology.edge_based_topology;
    let edge_count = topology.edge_count();
    let mut in_degree = vec![0_u32; edge_count];
    let mut out_degree = vec![0_u32; edge_count];

    if transition_topology.edge_transition_first_out.len() == edge_count + 1 {
        for edge_index in 0..edge_count {
            let start = transition_topology.edge_transition_first_out[edge_index] as usize;
            let end = transition_topology.edge_transition_first_out[edge_index + 1] as usize;
            let degree = end.saturating_sub(start) as u32;
            out_degree[edge_index] = degree;
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
        format!(
            "Building acceleration 0% ({} edge states, profile {:?}, budget {}/{})",
            edge_count,
            settings.profile,
            settings.max_shortcut_budget_per_edge,
            settings.max_shortcuts_per_contracted_edge
        ),
    );

    let mut arcs = Vec::<ShortcutArc>::new();
    let mut seen_arc_pairs = HashSet::with_capacity(
        transition_topology
            .edge_transition_edges
            .len()
            .saturating_mul(2),
    );
    let mut active_out = vec![Vec::<u32>::new(); edge_count];
    let mut active_in = vec![Vec::<u32>::new(); edge_count];
    if transition_topology.edge_transition_first_out.len() == edge_count + 1 {
        for edge_index in 0..edge_count {
            let start = transition_topology.edge_transition_first_out[edge_index] as usize;
            let end = transition_topology.edge_transition_first_out[edge_index + 1] as usize;
            for &next_edge in &transition_topology.edge_transition_edges[start..end] {
                if !seen_arc_pairs.insert(shortcut_arc_key(edge_index as u32, next_edge)) {
                    continue;
                }
                let arc_id = arcs.len() as u32;
                arcs.push(ShortcutArc {
                    tail: edge_index as u32,
                    head: next_edge,
                    path_len: 1,
                    kind: ShortcutArcKind::Base,
                });
                active_out[edge_index].push(arc_id);
                active_in[next_edge as usize].push(arc_id);
            }
        }
    }
    let base_arc_count = arcs.len();
    let max_shortcut_count =
        edge_count.saturating_mul(settings.max_shortcut_budget_per_edge as usize);
    let mut added_shortcuts = 0_usize;

    let mut active_vertex = vec![true; edge_count];
    let mut reporter = PercentReporter::starting_at_zero();
    for (order_index, &contracted_edge) in edge_order.iter().enumerate() {
        let contracted_edge = contracted_edge as usize;
        let incoming = active_in[contracted_edge]
            .iter()
            .copied()
            .filter(|&arc_id| {
                let arc = &arcs[arc_id as usize];
                active_vertex[arc.tail as usize]
                    && active_vertex[arc.head as usize]
                    && arc.head as usize == contracted_edge
                    && arc.tail as usize != contracted_edge
            })
            .collect::<Vec<_>>();
        let outgoing = active_out[contracted_edge]
            .iter()
            .copied()
            .filter(|&arc_id| {
                let arc = &arcs[arc_id as usize];
                active_vertex[arc.tail as usize]
                    && active_vertex[arc.head as usize]
                    && arc.tail as usize == contracted_edge
                    && arc.head as usize != contracted_edge
            })
            .collect::<Vec<_>>();

        let remaining_shortcut_budget = max_shortcut_count.saturating_sub(added_shortcuts);
        let local_shortcut_budget =
            remaining_shortcut_budget.min(settings.max_shortcuts_per_contracted_edge as usize);
        let mut added_for_vertex = 0_usize;
        for incoming_arc in incoming {
            let tail = arcs[incoming_arc as usize].tail as usize;
            for &outgoing_arc in &outgoing {
                if added_for_vertex >= local_shortcut_budget {
                    break;
                }
                let head = arcs[outgoing_arc as usize].head as usize;
                if tail == head {
                    continue;
                }
                let path_len = arcs[incoming_arc as usize]
                    .path_len
                    .saturating_add(arcs[outgoing_arc as usize].path_len);
                if path_len > settings.max_shortcut_path_len {
                    continue;
                }
                if !seen_arc_pairs.insert(shortcut_arc_key(tail as u32, head as u32)) {
                    continue;
                }
                let arc_id = arcs.len() as u32;
                arcs.push(ShortcutArc {
                    tail: tail as u32,
                    head: head as u32,
                    path_len,
                    kind: ShortcutArcKind::Shortcut {
                        left: incoming_arc,
                        right: outgoing_arc,
                    },
                });
                active_out[tail].push(arc_id);
                active_in[head].push(arc_id);
                added_shortcuts += 1;
                added_for_vertex += 1;
            }
            if added_for_vertex >= local_shortcut_budget {
                break;
            }
        }
        active_vertex[contracted_edge] = false;
        reporter.emit_if_needed(
            (order_index + 1) as u64,
            edge_order.len() as u64,
            DatasetImportStage::BuildAcceleration,
            progress,
            |percent| {
                format!(
                    "Building acceleration {:.0}% ({}/{}) with {} arcs ({} base, {} shortcuts)",
                    percent,
                    order_index + 1,
                    edge_order.len(),
                    arcs.len(),
                    base_arc_count,
                    added_shortcuts
                )
            },
        );
    }
    emit_progress(
        progress,
        DatasetImportStage::BuildAcceleration,
        Some(100.0),
        format!(
            "Building acceleration 100% ({} arcs: {} base, {} shortcuts)",
            arcs.len(),
            base_arc_count,
            added_shortcuts
        ),
    );

    let mut upward_first_out = Vec::with_capacity(edge_count + 1);
    let mut downward_first_out = Vec::with_capacity(edge_count + 1);
    upward_first_out.push(0);
    downward_first_out.push(0);

    let mut upward_head = Vec::new();
    let mut downward_head = Vec::new();
    let mut upward_path_first_out = Vec::new();
    let mut downward_path_first_out = Vec::new();
    let mut upward_path_edges = Vec::new();
    let mut downward_path_edges = Vec::new();
    let mut path_stack = Vec::new();
    upward_path_first_out.push(0);
    downward_path_first_out.push(0);

    for tail in 0..edge_count {
        for &arc_id in &active_out[tail] {
            let arc = &arcs[arc_id as usize];
            if edge_rank[arc.tail as usize] < edge_rank[arc.head as usize] {
                upward_head.push(arc.head);
                append_shortcut_arc_path(&arcs, arc_id, &mut upward_path_edges, &mut path_stack);
                upward_path_first_out.push(upward_path_edges.len() as u32);
            } else {
                downward_head.push(arc.head);
                append_shortcut_arc_path(&arcs, arc_id, &mut downward_path_edges, &mut path_stack);
                downward_path_first_out.push(downward_path_edges.len() as u32);
            }
        }
        upward_first_out.push(upward_head.len() as u32);
        downward_first_out.push(downward_head.len() as u32);
    }

    DatasetAccelerationBundle {
        schema_version: 2,
        source_topology_bundle_id,
        algorithm: "edge_based_shortcut_ch_v1".to_string(),
        build_settings: settings,
        stats: AccelerationBundleStats {
            base_arc_count: base_arc_count as u64,
            shortcut_arc_count: added_shortcuts as u64,
            total_arc_count: arcs.len() as u64,
        },
        edge_order,
        edge_rank,
        upward_first_out,
        upward_head,
        upward_path_first_out,
        upward_path_edges,
        downward_first_out,
        downward_head,
        downward_path_first_out,
        downward_path_edges,
    }
}

#[derive(Debug, Clone, Copy)]
struct ShortcutArc {
    tail: u32,
    head: u32,
    path_len: u32,
    kind: ShortcutArcKind,
}

#[derive(Debug, Clone, Copy)]
enum ShortcutArcKind {
    Base,
    Shortcut { left: u32, right: u32 },
}

fn append_shortcut_arc_path(
    arcs: &[ShortcutArc],
    arc_id: u32,
    output: &mut Vec<u32>,
    stack: &mut Vec<u32>,
) {
    stack.clear();
    stack.push(arc_id);
    while let Some(current_arc_id) = stack.pop() {
        match arcs[current_arc_id as usize].kind {
            ShortcutArcKind::Base => output.push(arcs[current_arc_id as usize].head),
            ShortcutArcKind::Shortcut { left, right } => {
                stack.push(right);
                stack.push(left);
            }
        }
    }
}

fn shortcut_arc_key(tail: u32, head: u32) -> u64 {
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
    use super::{
        build_dataset_acceleration_bundle, build_dataset_acceleration_bundle_with_settings,
    };
    use crate::test_util::edge;
    use crate::topology::build_edge_based_topology;
    use netweevil_core::{
        AccelerationBuildProfile, AccelerationBuildSettings, CacheBundleId, NodeId, TopologyBundle,
        TopologyNode,
    };

    #[test]
    fn builds_ordered_edge_transition_acceleration_bundle() {
        let edges = vec![edge(0, 0, 1, 10), edge(1, 1, 2, 11), edge(2, 2, 3, 12)];
        let topology = TopologyBundle {
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
        };

        let bundle =
            build_dataset_acceleration_bundle(&topology, CacheBundleId::new("topology-test"));

        assert_eq!(bundle.algorithm, "edge_based_shortcut_ch_v1");
        assert_eq!(
            bundle.build_settings,
            AccelerationBuildSettings::for_profile(AccelerationBuildProfile::Balanced)
        );
        assert_eq!(bundle.stats.base_arc_count, 2);
        assert_eq!(bundle.stats.shortcut_arc_count, 0);
        assert_eq!(bundle.stats.total_arc_count, 2);
        assert_eq!(bundle.edge_order, vec![2, 0, 1]);
        assert_eq!(bundle.edge_rank, vec![1, 2, 0]);
        assert_eq!(bundle.upward_first_out, vec![0, 1, 1, 1]);
        assert_eq!(bundle.upward_head, vec![1]);
        assert_eq!(bundle.upward_path_first_out, vec![0, 1]);
        assert_eq!(bundle.upward_path_edges, vec![1]);
        assert_eq!(bundle.downward_first_out, vec![0, 0, 1, 1]);
        assert_eq!(bundle.downward_head, vec![2]);
        assert_eq!(bundle.downward_path_first_out, vec![0, 1]);
        assert_eq!(bundle.downward_path_edges, vec![2]);
    }

    #[test]
    fn compact_acceleration_profile_is_more_conservative() {
        let compact = AccelerationBuildSettings::for_profile(AccelerationBuildProfile::Compact);
        let balanced = AccelerationBuildSettings::for_profile(AccelerationBuildProfile::Balanced);
        let aggressive =
            AccelerationBuildSettings::for_profile(AccelerationBuildProfile::Aggressive);

        assert!(compact.max_shortcut_path_len < balanced.max_shortcut_path_len);
        assert!(
            compact.max_shortcuts_per_contracted_edge < balanced.max_shortcuts_per_contracted_edge
        );
        assert!(compact.max_shortcut_budget_per_edge < balanced.max_shortcut_budget_per_edge);

        assert!(aggressive.max_shortcut_path_len > balanced.max_shortcut_path_len);
        assert!(
            aggressive.max_shortcuts_per_contracted_edge
                > balanced.max_shortcuts_per_contracted_edge
        );
        assert!(aggressive.max_shortcut_budget_per_edge > balanced.max_shortcut_budget_per_edge);
    }

    #[test]
    fn persists_custom_acceleration_build_settings_in_bundle() {
        let edges = vec![edge(0, 0, 1, 10), edge(1, 1, 2, 11), edge(2, 2, 3, 12)];
        let topology = TopologyBundle {
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
        };
        let settings = AccelerationBuildSettings {
            profile: AccelerationBuildProfile::Compact,
            max_shortcut_path_len: 8,
            max_shortcuts_per_contracted_edge: 2,
            max_shortcut_budget_per_edge: 0,
        };

        let bundle = build_dataset_acceleration_bundle_with_settings(
            &topology,
            CacheBundleId::new("topology-test"),
            settings.clone(),
        );

        assert_eq!(bundle.build_settings, settings.normalized());
        assert_eq!(bundle.stats.shortcut_arc_count, 0);
        assert_eq!(bundle.stats.total_arc_count, bundle.stats.base_arc_count);
    }
}
