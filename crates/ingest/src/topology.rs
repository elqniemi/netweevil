use std::path::Path;

use anyhow::Result;
use netweevil_core::{
    ConnectedComponentKind, ConnectedComponentsMeta, DirectedEdge, EDGE_FLAG_ROUNDABOUT,
    EDGE_FLAG_TARGET_TRAFFIC_SIGNAL, EdgeBasedTopology, EdgeId, EdgeNameBundle, NodeId,
    SpatialIndexCell, TopologyBounds, TopologyBundle, TopologyBundleMeta, TopologyNode,
};
use std::collections::BTreeMap;

use rustc_hash::FxHashMap;

use crate::import::{DatasetImportProgress, DatasetImportStage, PercentReporter, emit_progress};
use crate::restrictions::build_turn_restrictions;
use crate::scan::{PendingWay, load_node_coords, scan_routable_objects};

pub(crate) fn build_topology_bundle(
    source_path: &Path,
    source_size_bytes: u64,
    source_sha256: &str,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<(TopologyBundle, EdgeNameBundle, TopologyBundleMeta)> {
    let (pending_ways, restriction_candidates, needed_nodes, traffic_signal_nodes, counts) =
        scan_routable_objects(source_path, source_size_bytes, progress)?;
    let node_coords = load_node_coords(source_path, source_size_bytes, &needed_nodes, progress)?;
    let (nodes, node_lookup) = build_nodes(&node_coords);
    let name_lookup = build_name_lookup(&pending_ways);

    emit_progress(
        progress,
        DatasetImportStage::BuildTopology,
        Some(0.0),
        format!(
            "Building topology from {} routable ways and {} node coordinates",
            pending_ways.len(),
            node_coords.len()
        ),
    );
    let spatial_index = build_spatial_index(&nodes);

    let mut edges = Vec::new();
    let mut edge_highways = Vec::new();
    let mut skipped_way_count = 0_u64;
    let mut build_reporter = PercentReporter::starting_at_zero();
    for (way_index, way) in pending_ways.iter().enumerate() {
        let name_index = way
            .name
            .as_ref()
            .and_then(|name| name_lookup.get(name))
            .copied();
        let mut segments = Vec::new();

        for pair in way.node_ids.windows(2) {
            let from = pair[0];
            let to = pair[1];
            if from == to {
                continue;
            }

            let Some(&(from_id, from_lon, from_lat)) = node_lookup.get(&from) else {
                continue;
            };
            let Some(&(to_id, to_lon, to_lat)) = node_lookup.get(&to) else {
                continue;
            };

            let length_m = haversine_meters(from_lon, from_lat, to_lon, to_lat).round() as u32;
            segments.push((from_id, to_id, from, to, length_m));
        }

        if segments.is_empty() {
            skipped_way_count += 1;
            continue;
        }

        let total_length_m = segments
            .iter()
            .map(|(_, _, _, _, length_m)| *length_m as u64)
            .sum();
        let segment_count = segments.len() as u32;
        for (from_id, to_id, from_osm_id, to_osm_id, length_m) in segments {
            let duration_s =
                apportioned_duration_s(way.duration_s, length_m, total_length_m, segment_count);
            let mut forward_flags = 0_u32;
            let mut reverse_flags = 0_u32;
            if way.is_roundabout {
                forward_flags |= EDGE_FLAG_ROUNDABOUT;
                reverse_flags |= EDGE_FLAG_ROUNDABOUT;
            }
            if traffic_signal_nodes.contains(&to_osm_id) {
                forward_flags |= EDGE_FLAG_TARGET_TRAFFIC_SIGNAL;
            }
            if traffic_signal_nodes.contains(&from_osm_id) {
                reverse_flags |= EDGE_FLAG_TARGET_TRAFFIC_SIGNAL;
            }

            if way.forward_access_mask.0 != 0 {
                edges.push(DirectedEdge {
                    edge_id: EdgeId(edges.len() as u32),
                    from: from_id,
                    to: to_id,
                    source_way_id: way.osm_way_id,
                    length_m,
                    duration_s,
                    road_class: way.road_class,
                    surface: way.surface,
                    smoothness: way.smoothness,
                    access_mask: way.forward_access_mask,
                    is_toll: way.is_toll,
                    name_index,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: forward_flags | way.forward_extra_flags,
                });
                edge_highways.push(way.highway);
            }

            if way.reverse_access_mask.0 != 0 {
                edges.push(DirectedEdge {
                    edge_id: EdgeId(edges.len() as u32),
                    from: to_id,
                    to: from_id,
                    source_way_id: way.osm_way_id,
                    length_m,
                    duration_s,
                    road_class: way.road_class,
                    surface: way.surface,
                    smoothness: way.smoothness,
                    access_mask: way.reverse_access_mask,
                    is_toll: way.is_toll,
                    name_index,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: reverse_flags | way.reverse_extra_flags,
                });
                edge_highways.push(way.highway);
            }
        }

        build_reporter.emit_if_needed(
            (way_index + 1) as u64,
            pending_ways.len() as u64,
            DatasetImportStage::BuildTopology,
            progress,
            |percent| {
                format!(
                    "Building topology {:.0}% ({}/{})",
                    percent,
                    way_index + 1,
                    pending_ways.len()
                )
            },
        );
    }

    emit_progress(
        progress,
        DatasetImportStage::BuildTopology,
        Some(100.0),
        format!(
            "Resolving {} turn restrictions",
            restriction_candidates.len()
        ),
    );
    let turn_restrictions =
        build_turn_restrictions(&restriction_candidates, &pending_ways, &node_lookup, &edges);

    let mut names = vec![String::new(); name_lookup.len()];
    for (name, index) in name_lookup {
        names[index as usize] = name;
    }

    let edge_name_bundle = EdgeNameBundle { names };

    emit_progress(
        progress,
        DatasetImportStage::BuildTopology,
        Some(100.0),
        format!(
            "Building edge-transition topology from {} directed edges",
            edges.len()
        ),
    );
    let edge_based_topology = build_edge_based_topology(nodes.len(), &edges);
    let components = label_weak_components(nodes.len(), &edges);

    let mut edge_layers = netweevil_core::TopologyEdgeLayers::from_directed_edges(&edges);
    for (profile, highway) in edge_layers.profile.iter_mut().zip(edge_highways) {
        profile.highway = highway;
    }
    let bundle = TopologyBundle {
        schema_version: 9,
        source_path: source_path.display().to_string(),
        source_sha256: source_sha256.to_string(),
        nodes,
        edge_layers,
        edges: Vec::new(),
        turn_restrictions,
        names: Vec::new(),
        edge_based_topology,
        spatial_index,
        node_component_ids: components.node_component_ids.clone(),
        edge_component_ids: components.edge_component_ids.clone(),
    };
    let meta = TopologyBundleMeta {
        node_count: bundle.nodes.len() as u64,
        edge_count: bundle.edge_count() as u64,
        geometry_bytes: 0,
        turn_count: bundle.turn_restrictions.len() as u64,
        connected_components: Some(ConnectedComponentsMeta {
            kind: ConnectedComponentKind::Weak,
            component_count: components.component_count,
            largest_component_node_count: components.largest_component_node_count,
            largest_component_edge_count: components.largest_component_edge_count,
        }),
        source_node_count: counts.nodes,
        source_way_count: counts.ways,
        source_relation_count: counts.relations,
        routable_way_count: pending_ways.len() as u64,
        skipped_way_count,
    };
    Ok((bundle, edge_name_bundle, meta))
}

pub(crate) fn build_edge_based_topology(
    node_count: usize,
    edges: &[DirectedEdge],
) -> EdgeBasedTopology {
    let mut out_degree = vec![0_u32; node_count];
    let mut head = vec![0_u32; edges.len()];
    for (edge_index, edge) in edges.iter().enumerate() {
        out_degree[edge.from.0 as usize] += 1;
        head[edge_index] = edge.to.0;
    }

    let mut node_first_out = vec![0_u32; node_count + 1];
    for (node_index, degree) in out_degree.iter().enumerate() {
        node_first_out[node_index + 1] = node_first_out[node_index] + degree;
    }

    let mut node_edge_order = vec![0_u32; edges.len()];
    let mut write_positions = node_first_out[..node_count].to_vec();
    for (edge_index, edge) in edges.iter().enumerate() {
        let write_index = &mut write_positions[edge.from.0 as usize];
        node_edge_order[*write_index as usize] = edge_index as u32;
        *write_index += 1;
    }

    let mut edge_transition_first_out = vec![0_u32; edges.len() + 1];
    for edge_index in 0..edges.len() {
        let head_node = head[edge_index] as usize;
        edge_transition_first_out[edge_index + 1] = edge_transition_first_out[edge_index]
            + (node_first_out[head_node + 1] - node_first_out[head_node]);
    }

    let mut edge_transition_edges = vec![0_u32; edge_transition_first_out[edges.len()] as usize];
    let mut transition_write_positions = edge_transition_first_out[..edges.len()].to_vec();
    for edge_index in 0..edges.len() {
        let head_node = head[edge_index] as usize;
        for &next_edge in &node_edge_order
            [node_first_out[head_node] as usize..node_first_out[head_node + 1] as usize]
        {
            let write_index = &mut transition_write_positions[edge_index];
            edge_transition_edges[*write_index as usize] = next_edge;
            *write_index += 1;
        }
    }

    EdgeBasedTopology {
        node_first_out,
        node_edge_order,
        edge_transition_first_out,
        edge_transition_edges,
    }
}

#[derive(Debug, Clone)]
struct WeakComponentLabels {
    node_component_ids: Vec<u32>,
    edge_component_ids: Vec<u32>,
    component_count: u32,
    largest_component_node_count: u64,
    largest_component_edge_count: u64,
}

fn label_weak_components(node_count: usize, edges: &[DirectedEdge]) -> WeakComponentLabels {
    let mut parent = (0..node_count as u32).collect::<Vec<_>>();
    let mut rank = vec![0_u8; node_count];

    for edge in edges {
        union_components(
            &mut parent,
            &mut rank,
            edge.from.0 as usize,
            edge.to.0 as usize,
        );
    }

    let mut root_to_component = BTreeMap::<u32, u32>::new();
    let mut node_counts = Vec::<u64>::new();
    let mut node_component_ids = vec![0_u32; node_count];

    for (node_index, component_slot) in node_component_ids.iter_mut().enumerate() {
        let root = find_component_root(&mut parent, node_index);
        let component_id = if let Some(&component_id) = root_to_component.get(&root) {
            component_id
        } else {
            let component_id = root_to_component.len() as u32;
            root_to_component.insert(root, component_id);
            node_counts.push(0);
            component_id
        };
        *component_slot = component_id;
        node_counts[component_id as usize] += 1;
    }

    let mut edge_counts = vec![0_u64; node_counts.len()];
    let edge_component_ids = edges
        .iter()
        .map(|edge| {
            let component_id = node_component_ids[edge.from.0 as usize];
            edge_counts[component_id as usize] += 1;
            component_id
        })
        .collect::<Vec<_>>();

    WeakComponentLabels {
        node_component_ids,
        edge_component_ids,
        component_count: node_counts.len() as u32,
        largest_component_node_count: node_counts.into_iter().max().unwrap_or_default(),
        largest_component_edge_count: edge_counts.into_iter().max().unwrap_or_default(),
    }
}

fn find_component_root(parent: &mut [u32], index: usize) -> u32 {
    // Iterative two-pass find with full path compression: long parent
    // chains on continent graphs would overflow the stack recursively.
    let mut root = index as u32;
    while parent[root as usize] != root {
        root = parent[root as usize];
    }
    let mut cursor = index as u32;
    while parent[cursor as usize] != root {
        let next = parent[cursor as usize];
        parent[cursor as usize] = root;
        cursor = next;
    }
    root
}

fn union_components(parent: &mut [u32], rank: &mut [u8], left: usize, right: usize) {
    let left_root = find_component_root(parent, left);
    let right_root = find_component_root(parent, right);
    if left_root == right_root {
        return;
    }

    let left_rank = rank[left_root as usize];
    let right_rank = rank[right_root as usize];
    if left_rank < right_rank {
        parent[left_root as usize] = right_root;
    } else if left_rank > right_rank {
        parent[right_root as usize] = left_root;
    } else if left_root <= right_root {
        parent[right_root as usize] = left_root;
        rank[left_root as usize] += 1;
    } else {
        parent[left_root as usize] = right_root;
        rank[right_root as usize] += 1;
    }
}

fn build_nodes(
    node_coords: &FxHashMap<i64, (f64, f64)>,
) -> (Vec<TopologyNode>, FxHashMap<i64, (NodeId, f64, f64)>) {
    let mut used_nodes = node_coords.keys().copied().collect::<Vec<_>>();
    used_nodes.sort_unstable();

    let mut nodes = Vec::with_capacity(used_nodes.len());
    let mut lookup = FxHashMap::with_capacity_and_hasher(used_nodes.len(), Default::default());
    for osm_node_id in used_nodes {
        let Some(&(lon, lat)) = node_coords.get(&osm_node_id) else {
            continue;
        };
        let node_id = NodeId(nodes.len() as u32);
        nodes.push(TopologyNode {
            node_id,
            osm_node_id,
            lon,
            lat,
        });
        lookup.insert(osm_node_id, (node_id, lon, lat));
    }

    (nodes, lookup)
}

fn build_name_lookup(ways: &[PendingWay]) -> BTreeMap<String, u32> {
    let mut distinct_names = ways
        .iter()
        .filter_map(|way| way.name.clone())
        .collect::<Vec<_>>();
    distinct_names.sort_unstable();
    distinct_names.dedup();

    distinct_names
        .into_iter()
        .enumerate()
        .map(|(index, name)| (name, index as u32))
        .collect()
}

fn build_spatial_index(nodes: &[TopologyNode]) -> Option<netweevil_core::NodeSpatialIndex> {
    const TARGET_CELL_SPAN_M: f64 = 750.0;
    const METERS_PER_DEGREE_LAT: f64 = 111_320.0;

    let first = nodes.first()?;
    let mut bounds = TopologyBounds {
        min_lon: first.lon,
        min_lat: first.lat,
        max_lon: first.lon,
        max_lat: first.lat,
    };
    for node in nodes.iter().skip(1) {
        bounds.min_lon = bounds.min_lon.min(node.lon);
        bounds.min_lat = bounds.min_lat.min(node.lat);
        bounds.max_lon = bounds.max_lon.max(node.lon);
        bounds.max_lat = bounds.max_lat.max(node.lat);
    }

    let mid_lat = ((bounds.min_lat + bounds.max_lat) / 2.0).to_radians();
    let cos_lat = mid_lat.cos().abs().max(0.2);
    let cell_height_deg = (TARGET_CELL_SPAN_M / METERS_PER_DEGREE_LAT).max(f64::EPSILON);
    let cell_width_deg = (TARGET_CELL_SPAN_M / (METERS_PER_DEGREE_LAT * cos_lat)).max(f64::EPSILON);
    let columns =
        (((bounds.max_lon - bounds.min_lon) / cell_width_deg).floor() as u32).saturating_add(1);
    let rows =
        (((bounds.max_lat - bounds.min_lat) / cell_height_deg).floor() as u32).saturating_add(1);
    let cell_count = columns as usize * rows as usize;
    let mut counts = vec![0_u32; cell_count];

    for node in nodes {
        counts[spatial_cell_index(
            &bounds,
            columns,
            rows,
            cell_width_deg,
            cell_height_deg,
            node.lon,
            node.lat,
        )] += 1;
    }

    let mut cells = vec![SpatialIndexCell::default(); cell_count];
    let mut next_offset = 0_u32;
    for (cell, count) in cells.iter_mut().zip(&counts) {
        cell.node_start = next_offset;
        cell.node_len = *count;
        next_offset += *count;
    }

    let mut write_positions = cells
        .iter()
        .map(|cell| cell.node_start as usize)
        .collect::<Vec<_>>();
    let mut node_ids = vec![0_u32; nodes.len()];
    for node in nodes {
        let cell_index = spatial_cell_index(
            &bounds,
            columns,
            rows,
            cell_width_deg,
            cell_height_deg,
            node.lon,
            node.lat,
        );
        let write_index = write_positions[cell_index];
        node_ids[write_index] = node.node_id.0;
        write_positions[cell_index] += 1;
    }

    Some(netweevil_core::NodeSpatialIndex {
        bounds,
        columns,
        rows,
        cell_width_deg,
        cell_height_deg,
        cells,
        node_ids,
    })
}

fn spatial_cell_index(
    bounds: &TopologyBounds,
    columns: u32,
    rows: u32,
    cell_width_deg: f64,
    cell_height_deg: f64,
    lon: f64,
    lat: f64,
) -> usize {
    let column = (((lon - bounds.min_lon) / cell_width_deg).floor() as i64)
        .clamp(0, columns.saturating_sub(1) as i64) as usize;
    let row = (((lat - bounds.min_lat) / cell_height_deg).floor() as i64)
        .clamp(0, rows.saturating_sub(1) as i64) as usize;
    row * columns as usize + column
}

fn apportioned_duration_s(
    total_duration_s: Option<f64>,
    segment_length_m: u32,
    total_length_m: u64,
    segment_count: u32,
) -> Option<f64> {
    let total_duration_s = total_duration_s?;
    if total_length_m > 0 {
        return Some(total_duration_s * segment_length_m as f64 / total_length_m as f64);
    }
    if segment_count > 0 {
        return Some(total_duration_s / segment_count as f64);
    }
    None
}

fn haversine_meters(from_lon: f64, from_lat: f64, to_lon: f64, to_lat: f64) -> f64 {
    let earth_radius_m = 6_371_000.0_f64;
    let d_lat = (to_lat - from_lat).to_radians();
    let d_lon = (to_lon - from_lon).to_radians();
    let from_lat = from_lat.to_radians();
    let to_lat = to_lat.to_radians();
    let a =
        (d_lat / 2.0).sin().powi(2) + from_lat.cos() * to_lat.cos() * (d_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    earth_radius_m * c
}

#[cfg(test)]
mod tests {
    use super::{apportioned_duration_s, haversine_meters, label_weak_components};
    use crate::test_util::edge;

    #[test]
    fn computes_reasonable_segment_length() {
        let meters = haversine_meters(6.5665, 53.2194, 6.5675, 53.2204);
        assert!(meters > 100.0);
        assert!(meters < 200.0);
    }

    #[test]
    fn apportions_way_duration_by_segment_length() {
        let duration = apportioned_duration_s(Some(600.0), 250, 1_000, 2).expect("duration");
        assert!((duration - 150.0).abs() < f64::EPSILON);
    }

    #[test]
    fn falls_back_to_even_duration_apportion_when_length_is_zero() {
        let duration = apportioned_duration_s(Some(600.0), 0, 0, 3).expect("duration");
        assert!((duration - 200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn labels_weak_components_for_disconnected_subnetworks() {
        let labels = label_weak_components(
            4,
            &[edge(0, 0, 1, 10), edge(1, 1, 0, 10), edge(2, 2, 3, 20)],
        );

        assert_eq!(labels.component_count, 2);
        assert_eq!(labels.node_component_ids, vec![0, 0, 1, 1]);
        assert_eq!(labels.edge_component_ids, vec![0, 0, 1]);
        assert_eq!(labels.largest_component_node_count, 2);
        assert_eq!(labels.largest_component_edge_count, 2);
    }
}
