//! Optional per-route context: the administrative areas, border crossings
//! and timezones a route passes through (`returns.admin_areas`) and its
//! elevation profile (`returns.elevation`).

use netweevil_core::geo::haversine_meters;
use netweevil_core::{DrivingSide, NO_ADMIN_AREA, TopologyBundle};
use netweevil_profile::ReturnConfig;
use serde::{Deserialize, Serialize};

use crate::{RouteResult, SnappedPoint, edge_traversal_factor};

/// Areas along a route, in travel order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteRegions {
    /// Consecutive stretches within one country/state combination.
    pub spans: Vec<RouteAdminSpan>,
    /// Edges that run from one country into another.
    pub border_crossing_count: usize,
    /// Distinct IANA zones in the order the route enters them.
    pub timezones: Vec<String>,
}

/// A stretch of a route inside one administrative area; all area fields are
/// null outside every imported boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteAdminSpan {
    pub country_code: Option<String>,
    pub country_name: Option<String>,
    pub state_code: Option<String>,
    pub state_name: Option<String>,
    pub driving_side: Option<DrivingSide>,
    pub start_distance_m: f64,
    pub distance_m: f64,
}

/// Heights along the route's vertices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteElevationProfile {
    /// `[distance_m, elevation_m]` per route vertex; the elevation is null
    /// where it is unknown. Distances are horizontal.
    pub points: Vec<(f64, Option<f64>)>,
    /// Ascent and descent along the traversed edges, including terrain
    /// between vertices where the dataset sampled a DEM.
    pub ascent_m: f64,
    pub descent_m: f64,
    pub min_elevation_m: Option<f64>,
    pub max_elevation_m: Option<f64>,
    /// Steepest uphill and downhill grade (rise/run) along the route.
    pub max_uphill_grade: f64,
    pub max_downhill_grade: f64,
    /// Vertical datum of DEM heights, when the dataset recorded one.
    pub vertical_datum: Option<String>,
}

pub(crate) fn wants_annotations(returns: &ReturnConfig) -> bool {
    returns.admin_areas || returns.elevation
}

/// Adds the requested annotations to a route and its alternatives, each
/// from its own edge path. Waypoint legs are routes and are annotated one
/// leg at a time.
pub(crate) fn annotate_route(
    topology: &TopologyBundle,
    returns: &ReturnConfig,
    route: &mut RouteResult,
) {
    if !wants_annotations(returns) {
        return;
    }
    (route.regions, route.elevation) = path_annotations(
        topology,
        returns,
        &route.edge_path,
        &route.origin,
        &route.destination,
    );
    for alternative in &mut route.alternatives {
        (alternative.regions, alternative.elevation) = path_annotations(
            topology,
            returns,
            &alternative.edge_path,
            &route.origin,
            &route.destination,
        );
    }
}

/// The requested regions and elevation profile of an edge path between two
/// snapped points; nothing for an empty path.
fn path_annotations(
    topology: &TopologyBundle,
    returns: &ReturnConfig,
    edge_path: &[u32],
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> (Option<RouteRegions>, Option<RouteElevationProfile>) {
    if edge_path.is_empty() {
        return (None, None);
    }
    let edges = edge_path
        .iter()
        .map(|edge| *edge as usize)
        .filter(|edge| *edge < topology.edge_count())
        .collect::<Vec<_>>();
    let factors = edges
        .iter()
        .enumerate()
        .map(|(position, &edge)| {
            edge_traversal_factor(
                edge,
                position == 0,
                position + 1 == edges.len(),
                origin,
                destination,
            )
        })
        .collect::<Vec<_>>();
    let regions = returns
        .admin_areas
        .then(|| route_regions(topology, &edges, &factors));
    let elevation = returns
        .elevation
        .then(|| elevation_profile(topology, &edges, &factors, origin, destination));
    (regions, elevation)
}

fn route_regions(topology: &TopologyBundle, edges: &[usize], factors: &[f64]) -> RouteRegions {
    let regions = &topology.regions;
    let mut spans: Vec<(u32, RouteAdminSpan)> = Vec::new();
    let mut travelled = 0.0;
    let mut timezones = Vec::<String>::new();
    for (&edge, &factor) in edges.iter().zip(factors) {
        let routing = topology.routing_edge(edge);
        let distance = f64::from(routing.length_m) * factor;
        let area_index = regions
            .node_admin_area
            .get(routing.from.0 as usize)
            .copied()
            .unwrap_or(NO_ADMIN_AREA);
        match spans.last_mut() {
            Some((index, span)) if *index == area_index => span.distance_m += distance,
            _ => {
                let area = regions.admin_areas.get(area_index as usize);
                spans.push((
                    area_index,
                    RouteAdminSpan {
                        country_code: area.and_then(|area| area.country()).map(str::to_string),
                        country_name: area.and_then(|area| area.country_name.clone()),
                        state_code: area.and_then(|area| area.state_code.clone()),
                        state_name: area.and_then(|area| area.state_name.clone()),
                        driving_side: area.and_then(|area| area.driving_side()),
                        start_distance_m: travelled,
                        distance_m: distance,
                    },
                ));
            }
        }
        travelled += distance;
        if let Some(zone) = topology.edge_timezone(edge)
            && !timezones.iter().any(|known| known == zone)
        {
            timezones.push(zone.to_string());
        }
    }
    RouteRegions {
        spans: if regions.has_admin_areas() {
            spans.into_iter().map(|(_, span)| span).collect()
        } else {
            Vec::new()
        },
        border_crossing_count: edges
            .iter()
            .filter(|edge| topology.edge_crosses_border(**edge))
            .count(),
        timezones,
    }
}

/// Whether node heights are measurements: a DEM was sampled, or the source
/// carried heights. OSM and Overture imports without a DEM store zero for
/// every node, which is not an elevation.
fn has_heights(topology: &TopologyBundle) -> bool {
    topology.elevation.dem.is_some()
        || topology
            .nodes
            .iter()
            .any(|node| node.z.is_finite() && node.z != 0.0)
}

/// Height at a snapped position: its node's, or interpolated along its edge.
fn snapped_height(topology: &TopologyBundle, point: &SnappedPoint) -> Option<f64> {
    let node_height = |node: u32| topology.nodes.get(node as usize)?.elevation_m();
    match (point.snapped_edge_id, point.snapped_edge_fraction) {
        (Some(edge), Some(fraction)) => {
            let routing = topology.edge_layers.routing.get(edge as usize)?;
            let from = node_height(routing.from.0)?;
            let to = node_height(routing.to.0)?;
            Some(from + (to - from) * fraction)
        }
        _ => node_height(point.snapped_node_id),
    }
}

fn elevation_profile(
    topology: &TopologyBundle,
    edges: &[usize],
    factors: &[f64],
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> RouteElevationProfile {
    let known = has_heights(topology);
    let height_of = |height: Option<f64>| height.filter(|_| known);
    let mut vertices = vec![(
        origin.snapped_lon,
        origin.snapped_lat,
        height_of(snapped_height(topology, origin)),
    )];
    for (position, &edge) in edges.iter().enumerate() {
        if position + 1 == edges.len() && destination.snapped_edge_id == Some(edge as u32) {
            break;
        }
        let node = &topology.nodes[topology.routing_edge(edge).to.0 as usize];
        vertices.push((node.lon, node.lat, height_of(node.elevation_m())));
    }
    let end = (
        destination.snapped_lon,
        destination.snapped_lat,
        height_of(snapped_height(topology, destination)),
    );
    if vertices
        .last()
        .is_none_or(|last| (last.0, last.1) != (end.0, end.1))
    {
        vertices.push(end);
    }

    let mut points = Vec::with_capacity(vertices.len());
    let mut distance = 0.0;
    let (mut max_up, mut max_down) = (0.0_f64, 0.0_f64);
    for (index, &(lon, lat, height)) in vertices.iter().enumerate() {
        if index > 0 {
            let (previous_lon, previous_lat, previous_height) = vertices[index - 1];
            let run = haversine_meters(previous_lon, previous_lat, lon, lat);
            distance += run;
            if let (Some(from), Some(to)) = (previous_height, height)
                && run > 1.0
            {
                max_up = max_up.max((to - from) / run);
                max_down = max_down.max((from - to) / run);
            }
        }
        points.push((distance, height));
    }
    let elevation = &topology.elevation;
    let mut ascent = 0.0;
    let mut descent = 0.0;
    for (&edge, &factor) in edges.iter().zip(factors) {
        let routing = topology.routing_edge(edge);
        ascent += f64::from(routing.ascent_m) * factor;
        descent += f64::from(routing.descent_m) * factor;
        if let Some((up, down)) = elevation.edge_max_grades(edge)
            && factor > 0.0
        {
            max_up = max_up.max(up);
            max_down = max_down.max(down);
        }
    }
    let heights = points.iter().filter_map(|(_, height)| *height);
    RouteElevationProfile {
        min_elevation_m: heights.clone().reduce(f64::min),
        max_elevation_m: heights.reduce(f64::max),
        points,
        ascent_m: ascent,
        descent_m: descent,
        max_uphill_grade: max_up,
        max_downhill_grade: max_down,
        vertical_datum: elevation
            .dem
            .as_ref()
            .and_then(|dem| dem.source.vertical_datum.clone()),
    }
}

#[cfg(test)]
mod tests;
