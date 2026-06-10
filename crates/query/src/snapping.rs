use std::collections::HashMap;

use anyhow::Result;
use netweevil_core::{TopologyBundle, TopologyNode};

use crate::*;

pub(crate) fn presnap_point_set(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    points: &[LabeledPoint],
    max_distance_m: f64,
    is_origin: bool,
) -> Vec<std::result::Result<Vec<SnappedPoint>, AnalysisFailure>> {
    let mut snap_cache = HashMap::new();
    points
        .iter()
        .map(|point| {
            cached_snap_candidates(
                &mut snap_cache,
                topology,
                routing_graph,
                point,
                max_distance_m,
                is_origin,
            )
        })
        .collect()
}

pub(crate) fn cached_snap_candidates(
    cache: &mut HashMap<
        (bool, u64, u64, u64),
        std::result::Result<Vec<SnappedPoint>, AnalysisFailure>,
    >,
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    max_distance_m: f64,
    is_origin: bool,
) -> std::result::Result<Vec<SnappedPoint>, AnalysisFailure> {
    let key = snap_point_cache_key(point, max_distance_m, is_origin);
    let value = cache.entry(key).or_insert_with(|| {
        snap_candidates(topology, routing_graph, point, max_distance_m, is_origin).map_err(
            |error| {
                analysis_failure(&error)
                    .cloned()
                    .unwrap_or_else(|| route_snap_failure(point, max_distance_m))
            },
        )
    });
    value.clone()
}

fn snap_point_cache_key(
    point: &LabeledPoint,
    max_distance_m: f64,
    is_origin: bool,
) -> (bool, u64, u64, u64) {
    (
        is_origin,
        point.lon.to_bits(),
        point.lat.to_bits(),
        max_distance_m.to_bits(),
    )
}

pub(crate) fn snap_cache_key(point: &SnappedPoint) -> (u32, u64, u64) {
    (
        point.snapped_edge_id.unwrap_or(u32::MAX),
        point.snapped_edge_fraction.unwrap_or_default().to_bits(),
        point.snapped_node_id.to_owned() as u64,
    )
}

pub(crate) fn snap_candidates(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    max_distance_m: f64,
    is_origin: bool,
) -> Result<Vec<SnappedPoint>> {
    const MAX_SNAP_CANDIDATES: usize = 8;

    let nearby_nodes = if let Some(spatial_index) = topology.spatial_index.as_ref() {
        spatial_snap_nodes(topology, spatial_index, point, max_distance_m)
    } else {
        topology
            .nodes
            .iter()
            .map(|node| {
                (
                    node.node_id.0,
                    haversine_meters(point.lon, point.lat, node.lon, node.lat),
                )
            })
            .filter(|(_, distance_m)| *distance_m <= max_distance_m)
            .collect()
    };

    let mut candidates = Vec::with_capacity(MAX_SNAP_CANDIDATES * 2);
    for &(node_id, distance_m) in &nearby_nodes {
        if !node_is_traversable_for_snap(routing_graph, node_id as usize, is_origin) {
            continue;
        }
        push_best_snap_candidate(
            &mut candidates,
            SnappedPoint {
                point_id: point.id.clone(),
                requested_lon: point.lon,
                requested_lat: point.lat,
                snapped_node_id: node_id,
                snapped_lon: topology.nodes[node_id as usize].lon,
                snapped_lat: topology.nodes[node_id as usize].lat,
                snap_distance_m: distance_m,
                snapped_edge_id: None,
                snapped_edge_fraction: None,
                snapped_from_node_id: None,
                snapped_to_node_id: None,
                component_id: traversable_node_component_id(
                    topology,
                    routing_graph,
                    node_id as usize,
                    is_origin,
                ),
            },
            MAX_SNAP_CANDIDATES,
        );
    }

    let mut candidate_edges = Vec::<u32>::new();
    for &(node_id, _) in &nearby_nodes {
        candidate_edges.extend_from_slice(routing_graph.outgoing_edges(node_id as usize));
        candidate_edges.extend_from_slice(routing_graph.incoming_edges(node_id as usize));
    }
    if candidate_edges.is_empty() {
        for edge_index in 0..topology.edge_count() {
            if routing_graph.edge_costs[edge_index].is_finite() {
                candidate_edges.push(edge_index as u32);
            }
        }
    } else {
        candidate_edges.sort_unstable();
        candidate_edges.dedup();
    }

    for edge_index in candidate_edges {
        let edge = topology.routing_edge(edge_index as usize);
        let from = &topology.nodes[edge.from.0 as usize];
        let to = &topology.nodes[edge.to.0 as usize];
        let projection = project_point_onto_segment(point.lon, point.lat, from, to);
        if projection.distance_m > max_distance_m {
            continue;
        }
        if projection.fraction <= 1.0e-6 || projection.fraction >= 1.0 - 1.0e-6 {
            continue;
        }
        let snapped_node_id = if projection.fraction <= 0.5 {
            edge.from.0
        } else {
            edge.to.0
        };
        push_best_snap_candidate(
            &mut candidates,
            SnappedPoint {
                point_id: point.id.clone(),
                requested_lon: point.lon,
                requested_lat: point.lat,
                snapped_node_id,
                snapped_lon: projection.lon,
                snapped_lat: projection.lat,
                snap_distance_m: projection.distance_m,
                snapped_edge_id: Some(edge_index),
                snapped_edge_fraction: Some(projection.fraction),
                snapped_from_node_id: Some(edge.from.0),
                snapped_to_node_id: Some(edge.to.0),
                component_id: topology.edge_component_id(edge_index),
            },
            MAX_SNAP_CANDIDATES,
        );
    }

    if candidates.is_empty() {
        return Err(route_snap_failure(point, max_distance_m).into());
    }

    candidates.sort_by(|left, right| left.snap_distance_m.total_cmp(&right.snap_distance_m));
    candidates.dedup_by(|left, right| snap_candidate_key(left) == snap_candidate_key(right));
    candidates.truncate(MAX_SNAP_CANDIDATES);
    Ok(candidates)
}

fn node_is_traversable_for_snap(
    routing_graph: &RoutingGraph,
    node_index: usize,
    is_origin: bool,
) -> bool {
    if is_origin {
        !routing_graph.outgoing_edges(node_index).is_empty()
    } else {
        !routing_graph.incoming_edges(node_index).is_empty()
    }
}

fn traversable_node_component_id(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    node_index: usize,
    is_origin: bool,
) -> Option<u32> {
    let edge_index = if is_origin {
        routing_graph.outgoing_edges(node_index).first().copied()
    } else {
        routing_graph.incoming_edges(node_index).first().copied()
    };
    edge_index
        .and_then(|edge_index| topology.edge_component_id(edge_index))
        .or_else(|| topology.node_component_id(node_index as u32))
}

struct SegmentProjection {
    fraction: f64,
    lon: f64,
    lat: f64,
    distance_m: f64,
}

fn project_point_onto_segment(
    lon: f64,
    lat: f64,
    from: &TopologyNode,
    to: &TopologyNode,
) -> SegmentProjection {
    let origin_x = 0.0;
    let origin_y = 0.0;
    let point_x = projected_delta_x(from.lon, from.lat, lon);
    let point_y = projected_delta_y(from.lat, lat);
    let segment_x = projected_delta_x(from.lon, from.lat, to.lon);
    let segment_y = projected_delta_y(from.lat, to.lat);
    let segment_len_sq = segment_x * segment_x + segment_y * segment_y;
    let fraction = if segment_len_sq <= f64::EPSILON {
        0.0
    } else {
        ((point_x * segment_x + point_y * segment_y) / segment_len_sq).clamp(0.0, 1.0)
    };
    let snapped_x = origin_x + segment_x * fraction;
    let snapped_y = origin_y + segment_y * fraction;
    let distance_m = ((point_x - snapped_x).powi(2) + (point_y - snapped_y).powi(2)).sqrt();
    SegmentProjection {
        fraction,
        lon: from.lon + (to.lon - from.lon) * fraction,
        lat: from.lat + (to.lat - from.lat) * fraction,
        distance_m,
    }
}

fn snap_candidate_key(candidate: &SnappedPoint) -> (u32, u64, u64) {
    (
        candidate.snapped_edge_id.unwrap_or(u32::MAX),
        candidate
            .snapped_edge_fraction
            .unwrap_or_default()
            .to_bits(),
        candidate.snapped_node_id as u64,
    )
}

fn spatial_snap_nodes(
    topology: &TopologyBundle,
    spatial_index: &netweevil_core::NodeSpatialIndex,
    point: &LabeledPoint,
    max_distance_m: f64,
) -> Vec<(u32, f64)> {
    let Some((center_col, center_row)) =
        spatial_index_cell_for_point(spatial_index, point.lon, point.lat)
    else {
        return Vec::new();
    };
    let cell_height_m = haversine_meters(
        point.lon,
        point.lat,
        point.lon,
        point.lat + spatial_index.cell_height_deg,
    )
    .max(1.0);
    let cell_width_m = haversine_meters(
        point.lon,
        point.lat,
        point.lon + spatial_index.cell_width_deg,
        point.lat,
    )
    .max(1.0);
    let ring_limit = ((max_distance_m / cell_height_m.min(cell_width_m)).ceil() as i32).max(1) + 1;
    let mut candidates = Vec::with_capacity(8);

    for row in center_row - ring_limit..=center_row + ring_limit {
        if !(0..spatial_index.rows as i32).contains(&row) {
            continue;
        }
        for col in center_col - ring_limit..=center_col + ring_limit {
            if !(0..spatial_index.columns as i32).contains(&col) {
                continue;
            }
            let cell =
                &spatial_index.cells[row as usize * spatial_index.columns as usize + col as usize];
            let start = cell.node_start as usize;
            let end = start + cell.node_len as usize;
            for &node_id in &spatial_index.node_ids[start..end] {
                let node = &topology.nodes[node_id as usize];
                let distance_m = haversine_meters(point.lon, point.lat, node.lon, node.lat);
                if distance_m <= max_distance_m {
                    let insert_index =
                        candidates.partition_point(|(_, existing)| *existing <= distance_m);
                    candidates.insert(insert_index, (node_id, distance_m));
                    if candidates.len() > 32 {
                        candidates.pop();
                    }
                }
            }
        }
    }

    candidates
}

fn push_best_snap_candidate(
    candidates: &mut Vec<SnappedPoint>,
    candidate: SnappedPoint,
    max_candidates: usize,
) {
    let insert_index = candidates
        .partition_point(|existing| existing.snap_distance_m <= candidate.snap_distance_m);
    if insert_index >= max_candidates * 2 {
        return;
    }
    candidates.insert(insert_index, candidate);
    if candidates.len() > max_candidates * 2 {
        candidates.pop();
    }
}

fn spatial_index_cell_for_point(
    spatial_index: &netweevil_core::NodeSpatialIndex,
    lon: f64,
    lat: f64,
) -> Option<(i32, i32)> {
    if spatial_index.columns == 0 || spatial_index.rows == 0 {
        return None;
    }
    let col = (((lon - spatial_index.bounds.min_lon) / spatial_index.cell_width_deg).floor()
        as i32)
        .clamp(0, spatial_index.columns as i32 - 1);
    let row = (((lat - spatial_index.bounds.min_lat) / spatial_index.cell_height_deg).floor()
        as i32)
        .clamp(0, spatial_index.rows as i32 - 1);
    Some((col, row))
}
