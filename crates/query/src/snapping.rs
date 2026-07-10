use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use anyhow::{Result, bail};
use netweevil_core::{TopologyBundle, TopologyNode};

use crate::*;

type SnapPointCacheKey = (bool, u64, u64, u64, u64, u64);

pub(crate) fn presnap_point_set(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    points: &[LabeledPoint],
    options: &SnapOptions,
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
                options,
                is_origin,
            )
        })
        .collect()
}

pub(crate) fn cached_snap_candidates(
    cache: &mut HashMap<SnapPointCacheKey, std::result::Result<Vec<SnappedPoint>, AnalysisFailure>>,
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    options: &SnapOptions,
    is_origin: bool,
) -> std::result::Result<Vec<SnappedPoint>, AnalysisFailure> {
    let key = snap_point_cache_key(point, options, is_origin);
    let value = cache.entry(key).or_insert_with(|| {
        snap_candidates_with_options(topology, routing_graph, point, options, is_origin).map_err(
            |error| {
                analysis_failure(&error)
                    .cloned()
                    .unwrap_or_else(|| route_snap_failure(point, options.max_distance_m))
            },
        )
    });
    value.clone()
}

fn snap_point_cache_key(
    point: &LabeledPoint,
    options: &SnapOptions,
    is_origin: bool,
) -> SnapPointCacheKey {
    let mut filter_hasher = std::collections::hash_map::DefaultHasher::new();
    for (name, value) in &options.attribute_filters {
        name.hash(&mut filter_hasher);
        value.hash(&mut filter_hasher);
    }
    (
        is_origin,
        point.lon.to_bits(),
        point.lat.to_bits(),
        point.z.map(f64::to_bits).unwrap_or(u64::MAX),
        options.max_distance_m.to_bits() ^ options.z_window_m.map(f64::to_bits).unwrap_or_default(),
        filter_hasher.finish(),
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
    snap_candidates_with_options(
        topology,
        routing_graph,
        point,
        &SnapOptions {
            max_distance_m,
            ..SnapOptions::default()
        },
        is_origin,
    )
}

pub(crate) fn snap_candidates_with_options(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    options: &SnapOptions,
    is_origin: bool,
) -> Result<Vec<SnappedPoint>> {
    if options.max_distance_m < 0.0 || !options.max_distance_m.is_finite() {
        bail!("snap max_distance_m must be a finite non-negative value");
    }
    if options
        .z_window_m
        .is_some_and(|window| window < 0.0 || !window.is_finite())
    {
        bail!("snap z_window_m must be a finite non-negative value");
    }
    if point.z.is_some_and(|z| !z.is_finite()) {
        bail!("snap point elevation must be finite when provided");
    }
    const MAX_SNAP_CANDIDATES: usize = 8;
    /// Linear-scan snapping fallbacks (no spatial index, or no nearby node)
    /// are only acceptable on small graphs; on large datasets a full scan
    /// per snapped point is a per-request O(network) cost.
    const MAX_FULL_SCAN_ELEMENTS: usize = 250_000;

    let nearby_nodes = if let Some(spatial_index) = topology.spatial_index.as_ref() {
        spatial_snap_nodes(topology, spatial_index, point, options.max_distance_m)
    } else {
        if topology.nodes.len() > MAX_FULL_SCAN_ELEMENTS {
            bail!(
                "topology bundle has no spatial index and is too large to snap by linear scan; re-import the dataset to build one"
            );
        }
        topology
            .nodes
            .iter()
            .map(|node| {
                (
                    node.node_id.0,
                    haversine_meters(point.lon, point.lat, node.lon, node.lat),
                )
            })
            .filter(|(_, distance_m)| *distance_m <= options.max_distance_m)
            .collect()
    };

    let mut candidates = Vec::with_capacity(MAX_SNAP_CANDIDATES * 2);
    for &(node_id, distance_m) in &nearby_nodes {
        let node = &topology.nodes[node_id as usize];
        if !elevation_is_eligible(point, node.elevation_m(), options.z_window_m)
            || !node_is_traversable_for_snap(
                topology,
                routing_graph,
                node_id as usize,
                is_origin,
                &options.attribute_filters,
            )
        {
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
                snapped_z: finite_elevation(topology.nodes[node_id as usize].z),
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
        // No node landed within range: the point may still project onto the
        // middle of a long edge whose endpoints are far away. A full edge
        // scan finds it, but is only tolerable on small graphs.
        if topology.edge_count() <= MAX_FULL_SCAN_ELEMENTS {
            for edge_index in 0..topology.edge_count() {
                if routing_graph.edge_costs[edge_index].is_finite() {
                    candidate_edges.push(edge_index as u32);
                }
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
        if projection.distance_m > options.max_distance_m
            || !edge_matches_filters(topology, edge_index as usize, &options.attribute_filters)
            || !elevation_is_eligible(
                point,
                (from.z.is_finite() && to.z.is_finite()).then_some(projection.z),
                options.z_window_m,
            )
        {
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
                snapped_z: projection.z,
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
        return Err(route_snap_failure(point, options.max_distance_m).into());
    }

    candidates.sort_by(|left, right| left.snap_distance_m.total_cmp(&right.snap_distance_m));
    candidates.dedup_by(|left, right| snap_candidate_key(left) == snap_candidate_key(right));
    candidates.truncate(MAX_SNAP_CANDIDATES);
    Ok(candidates)
}

fn node_is_traversable_for_snap(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    node_index: usize,
    is_origin: bool,
    filters: &std::collections::BTreeMap<String, String>,
) -> bool {
    let edges = if is_origin {
        routing_graph.outgoing_edges(node_index)
    } else {
        routing_graph.incoming_edges(node_index)
    };
    edges
        .iter()
        .any(|edge| edge_matches_filters(topology, *edge as usize, filters))
}

fn edge_matches_filters(
    topology: &TopologyBundle,
    edge_index: usize,
    filters: &std::collections::BTreeMap<String, String>,
) -> bool {
    filters
        .iter()
        .all(|(name, expected)| topology.edge_attribute_matches(edge_index, name, expected))
}

fn elevation_is_eligible(
    point: &LabeledPoint,
    candidate_z: Option<f64>,
    z_window_m: Option<f64>,
) -> bool {
    match (point.z, candidate_z, z_window_m) {
        (_, _, None) | (None, _, Some(_)) => true,
        (Some(point_z), Some(candidate_z), Some(window)) => {
            point_z.is_finite()
                && candidate_z.is_finite()
                && window >= 0.0
                && (point_z - candidate_z).abs() <= window
        }
        (Some(_), None, Some(_)) => false,
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
    z: f64,
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
        z: if from.z.is_finite() && to.z.is_finite() {
            from.z + (to.z - from.z) * fraction
        } else {
            0.0
        },
        distance_m,
    }
}

fn finite_elevation(z: f64) -> f64 {
    if z.is_finite() { z } else { 0.0 }
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
