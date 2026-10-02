use std::collections::HashMap;

use anyhow::{Result, bail};
use netweevil_core::{TopologyBundle, TopologyNode};

use crate::*;

#[derive(PartialEq, Eq, Hash)]
pub(crate) struct SnapPointCacheKey {
    is_origin: bool,
    lon: u64,
    lat: u64,
    z: Option<u64>,
    max_distance: u64,
    z_window: Option<u64>,
    attribute_filters: Vec<(String, String)>,
    constraint: Option<SnapConstraintCacheKey>,
}

#[derive(PartialEq, Eq, Hash)]
struct SnapConstraintCacheKey {
    bearing: Option<(u64, u64)>,
    approach: SnapApproach,
    driving_side: Option<DrivingSide>,
    street_side_tolerance: u64,
}

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
    match value {
        Ok(candidates) => Ok(candidates
            .iter()
            .cloned()
            .map(|mut candidate| {
                candidate.point_id.clone_from(&point.id);
                candidate
            })
            .collect()),
        Err(_) => Err(route_snap_failure(point, options.max_distance_m)),
    }
}

fn snap_point_cache_key(
    point: &LabeledPoint,
    options: &SnapOptions,
    is_origin: bool,
) -> SnapPointCacheKey {
    SnapPointCacheKey {
        is_origin,
        lon: point.lon.to_bits(),
        lat: point.lat.to_bits(),
        z: point.z.map(f64::to_bits),
        max_distance: options.max_distance_m.to_bits(),
        z_window: options.z_window_m.map(f64::to_bits),
        attribute_filters: options
            .attribute_filters
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
        constraint: options.point_constraints.get(&point.id).map(|constraint| {
            SnapConstraintCacheKey {
                bearing: constraint.bearing.map(|bearing| {
                    (
                        bearing.degrees.to_bits(),
                        bearing.tolerance_degrees.to_bits(),
                    )
                }),
                approach: constraint.approach,
                driving_side: constraint.driving_side,
                street_side_tolerance: constraint.street_side_tolerance_m.to_bits(),
            }
        }),
    }
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

const MAX_SNAP_CANDIDATES: usize = 8;
const INITIAL_SNAP_RADIUS_M: f64 = 50.0;
const SNAP_RADIUS_GROWTH: f64 = 4.0;

pub(crate) fn snap_candidates_with_options(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    options: &SnapOptions,
    is_origin: bool,
) -> Result<Vec<SnappedPoint>> {
    if !point.lon.is_finite()
        || !(-180.0..=180.0).contains(&point.lon)
        || !point.lat.is_finite()
        || !(-90.0..=90.0).contains(&point.lat)
    {
        bail!("snap point longitude and latitude must be finite and within geographic bounds");
    }
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
    let constraint = options.point_constraints.get(&point.id);
    if let Some(constraint) = constraint {
        validate_point_constraint(constraint)?;
    }
    let directed = constraint.is_some_and(|constraint| {
        constraint.bearing.is_some() || constraint.approach != SnapApproach::Unrestricted
    });
    // Search a small radius first and widen it only while fewer than
    // `MAX_SNAP_CANDIDATES` candidates qualify. Every candidate within the
    // radius is found, so the nearest `MAX_SNAP_CANDIDATES` (ties in scan
    // order, which is window-independent) equal those of a full-radius scan.
    let mut radius_m = options.max_distance_m.min(INITIAL_SNAP_RADIUS_M);
    let mut candidates = loop {
        let candidates = collect_snap_candidates(
            topology,
            routing_graph,
            point,
            options,
            directed,
            is_origin,
            radius_m,
        )?;
        if candidates.len() >= MAX_SNAP_CANDIDATES || radius_m >= options.max_distance_m {
            break candidates;
        }
        radius_m = options.max_distance_m.min(radius_m * SNAP_RADIUS_GROWTH);
    };

    if candidates.is_empty() {
        return Err(route_snap_failure(point, options.max_distance_m).into());
    }

    candidates.sort_by(|left, right| left.snap_distance_m.total_cmp(&right.snap_distance_m));
    candidates.dedup_by(|left, right| snap_candidate_key(left) == snap_candidate_key(right));
    candidates.truncate(MAX_SNAP_CANDIDATES);
    Ok(candidates)
}

/// Reference for tests: one scan at the full radius, without widening.
#[cfg(test)]
pub(crate) fn snap_candidates_single_scan(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    options: &SnapOptions,
    is_origin: bool,
) -> Result<Vec<SnappedPoint>> {
    let mut candidates = collect_snap_candidates(
        topology,
        routing_graph,
        point,
        options,
        false,
        is_origin,
        options.max_distance_m,
    )?;
    if candidates.is_empty() {
        return Err(route_snap_failure(point, options.max_distance_m).into());
    }
    candidates.sort_by(|left, right| left.snap_distance_m.total_cmp(&right.snap_distance_m));
    candidates.dedup_by(|left, right| snap_candidate_key(left) == snap_candidate_key(right));
    candidates.truncate(MAX_SNAP_CANDIDATES);
    Ok(candidates)
}

/// Snap candidates within `radius_m`, keeping the `2 * MAX_SNAP_CANDIDATES`
/// nearest in push order (nodes first, then edges by id).
fn collect_snap_candidates(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    options: &SnapOptions,
    directed: bool,
    is_origin: bool,
    radius_m: f64,
) -> Result<Vec<SnappedPoint>> {
    let constraint = options.point_constraints.get(&point.id);
    // Node scans without an index are only acceptable on small graphs.
    const MAX_FULL_SCAN_ELEMENTS: usize = 250_000;

    let nearby_nodes = if directed {
        // Generic node seeds allow every incident direction and would bypass
        // bearing/approach restrictions. Directed endpoints are added below.
        Vec::new()
    } else if let Some(spatial_index) = topology.spatial_index.as_ref() {
        spatial_snap_nodes(topology, spatial_index, point, radius_m)
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
            .filter(|(_, distance_m)| *distance_m <= radius_m)
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

    let candidate_edges = routing_graph
        .edge_spatial_index
        .candidates(point.lon, point.lat, radius_m);

    for edge_index in candidate_edges {
        let edge = topology.routing_edge(edge_index as usize);
        let from = &topology.nodes[edge.from.0 as usize];
        let to = &topology.nodes[edge.to.0 as usize];
        let projection = project_point_onto_segment(point.lon, point.lat, from, to);
        let candidate_z = if projection.fraction == 0.0 {
            from.elevation_m()
        } else if projection.fraction == 1.0 {
            to.elevation_m()
        } else {
            (from.z.is_finite() && to.z.is_finite()).then_some(projection.z)
        };
        if projection.distance_m > radius_m
            || !edge_matches_filters(topology, edge_index as usize, &options.attribute_filters)
            || !elevation_is_eligible(point, candidate_z, options.z_window_m)
            || constraint.is_some_and(|constraint| {
                let driving_side = constraint
                    .driving_side
                    .or_else(|| topology.edge_driving_side(edge_index as usize))
                    .unwrap_or_default();
                !edge_matches_point_constraint(point, from, to, constraint, driving_side)
            })
        {
            continue;
        }
        if !directed && (projection.fraction <= 1.0e-6 || projection.fraction >= 1.0 - 1.0e-6) {
            continue;
        }
        // An origin at an edge's end has not departed along that edge; likewise
        // a destination at its start has not arrived along it. Zero-length
        // seeds at those endpoints would satisfy a heading without traveling it.
        if directed
            && ((is_origin && projection.fraction == 1.0)
                || (!is_origin && projection.fraction == 0.0))
        {
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

    Ok(candidates)
}

fn validate_point_constraint(constraint: &PointSnapConstraint) -> Result<()> {
    if let Some(bearing) = constraint.bearing {
        if !bearing.degrees.is_finite() || !(0.0..=360.0).contains(&bearing.degrees) {
            bail!("snap bearing degrees must be finite and in [0, 360]");
        }
        if !bearing.tolerance_degrees.is_finite()
            || !(0.0..=180.0).contains(&bearing.tolerance_degrees)
        {
            bail!("snap bearing tolerance_degrees must be finite and in [0, 180]");
        }
    }
    if !constraint.street_side_tolerance_m.is_finite() || constraint.street_side_tolerance_m < 0.0 {
        bail!("snap street_side_tolerance_m must be finite and non-negative");
    }
    Ok(())
}

fn edge_matches_point_constraint(
    point: &LabeledPoint,
    from: &TopologyNode,
    to: &TopologyNode,
    constraint: &PointSnapConstraint,
    driving_side: DrivingSide,
) -> bool {
    // Use the same local road tangent as projection, including its latitude
    // scale. Cross-product sign is positive on the left of travel.
    let dx = projected_delta_x(from.lon, from.lat, to.lon);
    let dy = projected_delta_y(from.lat, to.lat);
    let length = dx.hypot(dy);
    if length <= f64::EPSILON {
        return constraint.bearing.is_none() && constraint.approach == SnapApproach::Unrestricted;
    }
    if let Some(bearing) = constraint.bearing {
        let heading = dx.atan2(dy).to_degrees().rem_euclid(360.0);
        let difference = ((heading - bearing.degrees + 180.0).rem_euclid(360.0) - 180.0).abs();
        if difference > bearing.tolerance_degrees + 1.0e-10 {
            return false;
        }
    }
    if constraint.approach == SnapApproach::Unrestricted {
        return true;
    }
    let px = projected_delta_x(from.lon, from.lat, point.lon);
    let py = projected_delta_y(from.lat, point.lat);
    let side_distance = (dx * py - dy * px) / length;
    if side_distance.abs() <= constraint.street_side_tolerance_m {
        return true;
    }
    let on_driving_side = (side_distance < 0.0) == (driving_side == DrivingSide::Right);
    on_driving_side == (constraint.approach == SnapApproach::Curb)
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
        z: if fraction == 0.0 {
            finite_elevation(from.z)
        } else if fraction == 1.0 {
            finite_elevation(to.z)
        } else if from.z.is_finite() && to.z.is_finite() {
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
    if spatial_index.columns == 0 || spatial_index.rows == 0 {
        return Vec::new();
    }
    let window = SnapWindow::new(point.lon, point.lat, max_distance_m);
    // Cells overlapping the window's bounding box, clamped to the grid. All
    // indexed nodes lie inside the grid bounds, so clamping loses nothing.
    let cell_range = |low: f64, high: f64, origin: f64, span: f64, count: u32| {
        let last = i64::from(count) - 1;
        let first = (((low - origin) / span).floor() as i64).clamp(0, last);
        let end = (((high - origin) / span).floor() as i64).clamp(0, last);
        first..=end
    };
    let rows = cell_range(
        window.min_lat,
        window.max_lat,
        spatial_index.bounds.min_lat,
        spatial_index.cell_height_deg,
        spatial_index.rows,
    );
    let cols = if window.full_longitude {
        0..=i64::from(spatial_index.columns) - 1
    } else {
        cell_range(
            point.lon - window.lon_delta,
            point.lon + window.lon_delta,
            spatial_index.bounds.min_lon,
            spatial_index.cell_width_deg,
            spatial_index.columns,
        )
    };
    let mut candidates = Vec::with_capacity(8);

    for row in rows {
        for col in cols.clone() {
            let cell =
                &spatial_index.cells[row as usize * spatial_index.columns as usize + col as usize];
            let start = cell.node_start as usize;
            let end = start + cell.node_len as usize;
            for &node_id in &spatial_index.node_ids[start..end] {
                let node = &topology.nodes[node_id as usize];
                if !window.may_contain(node.lon, node.lat) {
                    continue;
                }
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

/// A conservative longitude/latitude box around a point: every location
/// within `radius_m` great-circle metres lies inside it. It only prefilters;
/// the haversine distance still decides membership.
struct SnapWindow {
    lon: f64,
    min_lat: f64,
    max_lat: f64,
    lon_delta: f64,
    full_longitude: bool,
}

impl SnapWindow {
    fn new(lon: f64, lat: f64, radius_m: f64) -> Self {
        // Slack covers floating-point rounding in the bound below.
        const SLACK: f64 = 1.0 + 1.0e-6;
        let angle = (radius_m / netweevil_core::geo::EARTH_RADIUS_M) * SLACK;
        // hav(d) >= hav(dlat), so |dlat| <= d / R.
        let lat_delta = angle.to_degrees() + 1.0e-9;
        let min_lat = lat - lat_delta;
        let max_lat = lat + lat_delta;
        // hav(d) >= cos(lat1) cos(lat2) hav(dlon), with cos(lat2) bounded
        // below over the latitude band.
        let extreme_lat = min_lat.abs().max(max_lat.abs());
        let cos_product = if extreme_lat >= 90.0 {
            0.0
        } else {
            lat.to_radians().cos() * extreme_lat.to_radians().cos()
        };
        let half_angle = (angle / 2.0).min(std::f64::consts::FRAC_PI_2);
        let sine = if cos_product > 0.0 {
            half_angle.sin() / cos_product.sqrt()
        } else {
            f64::INFINITY
        };
        let (lon_delta, full_longitude) = if sine >= 1.0 || !sine.is_finite() {
            (180.0, true)
        } else {
            let delta = (2.0 * sine.asin()).to_degrees() * SLACK + 1.0e-9;
            (delta, delta >= 180.0)
        };
        Self {
            lon,
            min_lat,
            max_lat,
            lon_delta,
            full_longitude,
        }
    }

    fn may_contain(&self, lon: f64, lat: f64) -> bool {
        if lat < self.min_lat || lat > self.max_lat {
            return false;
        }
        if self.full_longitude {
            return true;
        }
        let mut delta = (lon - self.lon).abs();
        if delta > 180.0 {
            delta = 360.0 - delta;
        }
        delta <= self.lon_delta
    }
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

#[cfg(test)]
mod tests;
