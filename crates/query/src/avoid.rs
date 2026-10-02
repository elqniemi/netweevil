//! Request-level avoid sets: locations, areas and source features whose
//! edges a request must not use (hard exclusion) or should use only at a
//! higher cost (penalty factor).
//!
//! An avoid set resolves to per-edge factors. Excluded edges leave the graph
//! entirely (no traversal, no snapping); penalized edges multiply their
//! generalized cost. Both are applied by re-deriving the metric, so every
//! engine (CCH, exact Dijkstra, temporal labels, service areas) honours them
//! without special cases.

use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};

use anyhow::{Result, bail, ensure};
use netweevil_core::geo::{EARTH_RADIUS_M, projected_delta_x, projected_delta_y};
use netweevil_core::{CompiledProfileBundle, TopologyBundle, TopologyNode};
use serde::{Deserialize, Serialize};

use crate::edge_spatial_index::EdgeSpatialIndex;

const METERS_PER_DEGREE: f64 = EARTH_RADIUS_M * std::f64::consts::PI / 180.0;
const MAX_AVOID_POINTS: usize = 10_000;
const MAX_AVOID_POLYGON_VERTICES: usize = 100_000;
const MAX_AVOID_RADIUS_M: f64 = 10_000.0;
const MAX_PENALTY_FACTOR: f64 = 1_000.0;

/// Locations, areas and source features a request avoids.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvoidOptions {
    /// Edges within `radius_m` of a point.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<AvoidPoint>,
    /// Edges that touch a polygon (boundary included).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub polygons: Vec<AvoidPolygon>,
    /// Every edge of these source features (OSM way ids); always excluded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_feature_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvoidPoint {
    pub lon: f64,
    pub lat: f64,
    #[serde(default = "default_avoid_radius_m")]
    pub radius_m: f64,
    /// `None` excludes the matched edges; a factor in `[1, 1000]` multiplies
    /// their generalized cost instead.
    #[serde(default)]
    pub penalty_factor: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvoidPolygon {
    /// GeoJSON polygon coordinates: an outer `[lon, lat]` ring followed by
    /// optional holes. Rings may be open or closed.
    pub rings: Vec<Vec<[f64; 2]>>,
    /// `None` excludes the matched edges; a factor in `[1, 1000]` multiplies
    /// their generalized cost instead.
    #[serde(default)]
    pub penalty_factor: Option<f64>,
}

fn default_avoid_radius_m() -> f64 {
    10.0
}

impl AvoidOptions {
    pub fn is_empty(&self) -> bool {
        self.points.is_empty() && self.polygons.is_empty() && self.source_feature_ids.is_empty()
    }

    /// Concatenation of several avoid sets (for example both matrix sides).
    pub(crate) fn union<'a>(sets: impl IntoIterator<Item = &'a AvoidOptions>) -> AvoidOptions {
        let mut union = AvoidOptions::default();
        for set in sets {
            union.points.extend(set.points.iter().cloned());
            union.polygons.extend(set.polygons.iter().cloned());
            union
                .source_feature_ids
                .extend(set.source_feature_ids.iter().copied());
        }
        union
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.points.len() <= MAX_AVOID_POINTS,
            "avoid supports at most {MAX_AVOID_POINTS} points"
        );
        for point in &self.points {
            validate_coordinate(point.lon, point.lat)?;
            ensure!(
                point.radius_m.is_finite()
                    && point.radius_m > 0.0
                    && point.radius_m <= MAX_AVOID_RADIUS_M,
                "avoid point radius_m must be in (0, {MAX_AVOID_RADIUS_M}]"
            );
            validate_penalty(point.penalty_factor)?;
        }
        let vertices = self
            .polygons
            .iter()
            .flat_map(|polygon| &polygon.rings)
            .map(Vec::len)
            .sum::<usize>();
        ensure!(
            vertices <= MAX_AVOID_POLYGON_VERTICES,
            "avoid polygons support at most {MAX_AVOID_POLYGON_VERTICES} vertices in total"
        );
        for polygon in &self.polygons {
            ensure!(
                !polygon.rings.is_empty(),
                "avoid polygon needs an outer ring"
            );
            for ring in &polygon.rings {
                for &[lon, lat] in ring {
                    validate_coordinate(lon, lat)?;
                }
                let distinct = ring
                    .iter()
                    .map(|[lon, lat]| (lon.to_bits(), lat.to_bits()))
                    .collect::<HashSet<_>>();
                ensure!(
                    distinct.len() >= 3,
                    "avoid polygon rings need at least three distinct vertices"
                );
            }
            validate_penalty(polygon.penalty_factor)?;
        }
        Ok(())
    }
}

fn validate_coordinate(lon: f64, lat: f64) -> Result<()> {
    ensure!(
        lon.is_finite() && (-180.0..=180.0).contains(&lon),
        "avoid longitude must be finite and in [-180, 180]"
    );
    ensure!(
        lat.is_finite() && (-90.0..=90.0).contains(&lat),
        "avoid latitude must be finite and in [-90, 90]"
    );
    Ok(())
}

fn validate_penalty(factor: Option<f64>) -> Result<()> {
    if let Some(factor) = factor
        && !(factor.is_finite() && (1.0..=MAX_PENALTY_FACTOR).contains(&factor))
    {
        bail!("avoid penalty_factor must be in [1, {MAX_PENALTY_FACTOR}]");
    }
    Ok(())
}

/// One-shot entry points build a plain graph per call; avoid sets need the
/// cached, re-customized engines of [`crate::PreparedRoutingEngine`].
pub(crate) fn ensure_no_avoid<'a>(sets: impl IntoIterator<Item = &'a AvoidOptions>) -> Result<()> {
    ensure!(
        sets.into_iter().all(AvoidOptions::is_empty),
        "avoid requires a PreparedRoutingEngine"
    );
    Ok(())
}

/// The edges an avoid set touches, sorted by edge index, each with its cost
/// factor; `f64::INFINITY` marks an excluded edge. When several entries
/// match one edge, exclusion wins over penalties and the largest penalty
/// wins among penalties.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct ResolvedAvoid {
    pub(crate) edges: Vec<(u32, f64)>,
}

impl ResolvedAvoid {
    pub(crate) fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    fn factor(&self, edge_index: usize) -> Option<f64> {
        let edge_index = u32::try_from(edge_index).ok()?;
        self.edges
            .binary_search_by_key(&edge_index, |&(edge, _)| edge)
            .ok()
            .map(|position| self.edges[position].1)
    }

    pub(crate) fn excludes(&self, edge_index: usize) -> bool {
        self.factor(edge_index).is_some_and(f64::is_infinite)
    }

    /// Whether the edge is excluded or penalized.
    pub(crate) fn affects(&self, edge_index: usize) -> bool {
        self.factor(edge_index).is_some()
    }

    /// This set with `edges` excluded as well.
    pub(crate) fn excluding(self, edges: &[u32]) -> Self {
        let mut factors = self.edges.into_iter().collect::<BTreeMap<_, _>>();
        for &edge in edges {
            factors.insert(edge, f64::INFINITY);
        }
        Self {
            edges: factors.into_iter().collect(),
        }
    }

    pub(crate) fn fingerprint(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for &(edge, factor) in &self.edges {
            edge.hash(&mut hasher);
            factor.to_bits().hash(&mut hasher);
        }
        hasher.finish()
    }

    /// `metrics` with excluded edges made untraversable and penalized edges
    /// costlier. Acceleration weights are dropped so the engine re-customizes
    /// them for the new metric.
    pub(crate) fn apply(&self, metrics: &CompiledProfileBundle) -> CompiledProfileBundle {
        let mut metrics = metrics.clone();
        for &(edge, factor) in &self.edges {
            let metric = &mut metrics.edge_metrics[edge as usize];
            if factor.is_infinite() {
                metric.generalized_cost = None;
                metric.travel_time_s = None;
            } else if let Some(cost) = metric.generalized_cost.as_mut() {
                *cost *= factor;
            }
        }
        metrics.profile_hash =
            format!("{}+avoid-{:016x}", metrics.profile_hash, self.fingerprint());
        if let Some(acceleration) = metrics.acceleration.as_mut() {
            for weights in [
                &mut acceleration.upward_weight,
                &mut acceleration.downward_weight,
                &mut acceleration.time_upward_weight,
                &mut acceleration.time_downward_weight,
                &mut acceleration.distance_upward_weight,
                &mut acceleration.distance_downward_weight,
            ] {
                *weights = Vec::new();
            }
        }
        metrics
    }
}

/// Resolves an avoid set against the edges `index` holds (the routable edges
/// of the profile). Point distances use the snapper's local metric, so an
/// edge is avoided exactly when a snap within `radius_m` could reach it.
pub(crate) fn resolve_avoid(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    index: &EdgeSpatialIndex,
    avoid: &AvoidOptions,
) -> Result<ResolvedAvoid> {
    avoid.validate()?;
    let mut factors = BTreeMap::<u32, f64>::new();
    let mut mark = |edge: u32, factor: f64| {
        let known = factors.entry(edge).or_insert(factor);
        *known = known.max(factor);
    };
    let segment = |edge: u32| {
        let edge = topology.routing_edge(edge as usize);
        (
            &topology.nodes[edge.from.0 as usize],
            &topology.nodes[edge.to.0 as usize],
        )
    };
    for point in &avoid.points {
        let factor = point.penalty_factor.unwrap_or(f64::INFINITY);
        for edge in index.candidates(point.lon, point.lat, point.radius_m) {
            let (from, to) = segment(edge);
            if point_segment_distance_m(point.lon, point.lat, from, to) <= point.radius_m {
                mark(edge, factor);
            }
        }
    }
    for polygon in &avoid.polygons {
        let factor = polygon.penalty_factor.unwrap_or(f64::INFINITY);
        let outer = &polygon.rings[0];
        let (mut min_lon, mut min_lat) = (f64::INFINITY, f64::INFINITY);
        let (mut max_lon, mut max_lat) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &[lon, lat] in outer {
            min_lon = min_lon.min(lon);
            min_lat = min_lat.min(lat);
            max_lon = max_lon.max(lon);
            max_lat = max_lat.max(lat);
        }
        // Every edge touching the polygon has a point in its bounding box.
        // Scaling longitude by a full degree over-estimates the index's own
        // metric, so this circle query returns a superset of those edges.
        let radius_m = ((max_lon - min_lon) / 2.0 * METERS_PER_DEGREE)
            .hypot((max_lat - min_lat) / 2.0 * METERS_PER_DEGREE)
            + 1.0;
        let center = ((min_lon + max_lon) / 2.0, (min_lat + max_lat) / 2.0);
        for edge in index.candidates(center.0, center.1, radius_m) {
            let (from, to) = segment(edge);
            if segment_touches_polygon([from.lon, from.lat], [to.lon, to.lat], &polygon.rings) {
                mark(edge, factor);
            }
        }
    }
    if !avoid.source_feature_ids.is_empty() {
        let features = avoid
            .source_feature_ids
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        for edge in 0..topology.edge_count() {
            if metrics.edge_metrics[edge].generalized_cost.is_some()
                && features.contains(&topology.routing_edge(edge).source_way_id)
            {
                mark(edge as u32, f64::INFINITY);
            }
        }
    }
    Ok(ResolvedAvoid {
        edges: factors.into_iter().collect(),
    })
}

/// Distance from a point to an edge segment in the snapper's local metric
/// (equirectangular around the edge's start node).
pub(crate) fn point_segment_distance_m(
    lon: f64,
    lat: f64,
    from: &TopologyNode,
    to: &TopologyNode,
) -> f64 {
    let px = projected_delta_x(from.lon, from.lat, lon);
    let py = projected_delta_y(from.lat, lat);
    let sx = projected_delta_x(from.lon, from.lat, to.lon);
    let sy = projected_delta_y(from.lat, to.lat);
    let length_sq = sx * sx + sy * sy;
    let fraction = if length_sq <= f64::EPSILON {
        0.0
    } else {
        ((px * sx + py * sy) / length_sq).clamp(0.0, 1.0)
    };
    (px - sx * fraction).hypot(py - sy * fraction)
}

/// Whether segment `a`-`b` shares any point with the closed polygon (outer
/// ring minus holes), in planar lon/lat coordinates.
fn segment_touches_polygon(a: [f64; 2], b: [f64; 2], rings: &[Vec<[f64; 2]>]) -> bool {
    let crosses_boundary = rings.iter().any(|ring| {
        (0..ring.len()).any(|index| {
            let c = ring[index];
            let d = ring[(index + 1) % ring.len()];
            segments_intersect(a, b, c, d)
        })
    });
    // Without a boundary crossing the segment lies wholly inside or outside
    // the polygon, so one endpoint decides.
    crosses_boundary
        || (point_in_ring(a, &rings[0]) && !rings[1..].iter().any(|hole| point_in_ring(a, hole)))
}

fn point_in_ring(point: [f64; 2], ring: &[[f64; 2]]) -> bool {
    let mut inside = false;
    let mut previous = ring[ring.len() - 1];
    for &current in ring {
        if (current[1] > point[1]) != (previous[1] > point[1]) {
            let crossing_lon = (previous[0] - current[0]) * (point[1] - current[1])
                / (previous[1] - current[1])
                + current[0];
            if point[0] < crossing_lon {
                inside = !inside;
            }
        }
        previous = current;
    }
    inside
}

fn segments_intersect(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    let orientation = |p: [f64; 2], q: [f64; 2], r: [f64; 2]| {
        let value = (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0]);
        if value > 0.0 {
            1
        } else if value < 0.0 {
            -1
        } else {
            0
        }
    };
    let on_segment = |p: [f64; 2], q: [f64; 2], r: [f64; 2]| {
        q[0] >= p[0].min(r[0])
            && q[0] <= p[0].max(r[0])
            && q[1] >= p[1].min(r[1])
            && q[1] <= p[1].max(r[1])
    };
    let (o1, o2, o3, o4) = (
        orientation(a, b, c),
        orientation(a, b, d),
        orientation(c, d, a),
        orientation(c, d, b),
    );
    (o1 != o2 && o3 != o4)
        || (o1 == 0 && on_segment(a, c, b))
        || (o2 == 0 && on_segment(a, d, b))
        || (o3 == 0 && on_segment(c, a, d))
        || (o4 == 0 && on_segment(c, b, d))
}
