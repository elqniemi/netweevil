//! DEM sampling for sources without surveyed heights (OSM, Overture).
//!
//! Node elevations come from bilinear DEM samples. Nodes that only belong to
//! bridges or tunnels are not read from the terrain (which lies below a
//! bridge or above a tunnel): their height is interpolated along the
//! structure between the ground nodes it connects, weighted by the inverse
//! path distance to each, which is linear along a simple bridge. Ground
//! segments longer than the sample spacing are densified so ascent, descent
//! and the steepest grade see the terrain between nodes; structure segments
//! use their end heights only.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use netweevil_core::elevation::{grade_to_permille, quantize_decimetres, round_height_decimetres};
use netweevil_core::geo::haversine_meters;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::dem::DemRaster;
use crate::scan::PendingWay;

/// Smallest spacing between elevation samples along an edge, in metres.
pub(crate) const MIN_SAMPLE_SPACING_M: f64 = 10.0;

/// A DEM prepared for one import.
pub(crate) struct DemSampling<'a> {
    pub(crate) dem: &'a DemRaster,
    /// Maximum distance between samples along a ground segment.
    pub(crate) spacing_m: f64,
}

impl<'a> DemSampling<'a> {
    /// Spacing of one cell or [`MIN_SAMPLE_SPACING_M`], whichever is larger:
    /// finer samples only add interpolation noise to grades.
    pub(crate) fn new(dem: &'a DemRaster, reference_lat: f64) -> Self {
        Self {
            dem,
            spacing_m: dem.cell_size_m(reference_lat).max(MIN_SAMPLE_SPACING_M),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct NodeElevationStats {
    pub(crate) sampled: u64,
    pub(crate) missing: u64,
    pub(crate) structure: u64,
}

/// Replaces the elevation of every node in `node_coords` with its DEM
/// height (whole decimetres; `NaN` outside coverage).
pub(crate) fn assign_node_elevations(
    sampling: &DemSampling<'_>,
    node_coords: &mut FxHashMap<i64, (f64, f64, f64)>,
    ways: &[PendingWay],
    structure_ways: &FxHashSet<i64>,
) -> NodeElevationStats {
    let ground_nodes = ways
        .iter()
        .filter(|way| !structure_ways.contains(&way.osm_way_id))
        .flat_map(|way| way.node_ids.iter().copied())
        .collect::<FxHashSet<_>>();
    let structure_nodes = ways
        .iter()
        .filter(|way| structure_ways.contains(&way.osm_way_id))
        .flat_map(|way| way.node_ids.iter().copied())
        .filter(|node| !ground_nodes.contains(node) && node_coords.contains_key(node))
        .collect::<FxHashSet<_>>();

    let mut ids = node_coords.keys().copied().collect::<Vec<_>>();
    ids.sort_unstable();
    let heights = ids
        .par_iter()
        .map(|id| {
            let (lon, lat, _) = node_coords[id];
            sampling
                .dem
                .sample(lon, lat)
                .map_or(f64::NAN, round_height_decimetres)
        })
        .collect::<Vec<_>>();
    for (id, height) in ids.iter().zip(heights) {
        node_coords.get_mut(id).expect("id came from the map").2 = height;
    }

    let interpolated = interpolate_structures(node_coords, ways, structure_ways, &structure_nodes);
    for (id, height) in &interpolated {
        node_coords.get_mut(id).expect("structure node exists").2 = *height;
    }

    let missing = node_coords
        .values()
        .filter(|(_, _, z)| !z.is_finite())
        .count() as u64;
    NodeElevationStats {
        sampled: node_coords.len() as u64 - missing,
        missing,
        structure: interpolated.len() as u64,
    }
}

/// Heights of structure-only nodes from the ground nodes ("anchors") their
/// structure connects to: `sum(z_a / d_a) / sum(1 / d_a)` over anchors at
/// path distance `d_a`. Components without an anchor keep their DEM sample.
fn interpolate_structures(
    node_coords: &FxHashMap<i64, (f64, f64, f64)>,
    ways: &[PendingWay],
    structure_ways: &FxHashSet<i64>,
    structure_nodes: &FxHashSet<i64>,
) -> Vec<(i64, f64)> {
    let mut adjacency: FxHashMap<i64, Vec<(i64, f64)>> = FxHashMap::default();
    for way in ways
        .iter()
        .filter(|way| structure_ways.contains(&way.osm_way_id))
    {
        for pair in way.node_ids.windows(2) {
            let (Some(&(from_lon, from_lat, _)), Some(&(to_lon, to_lat, _))) =
                (node_coords.get(&pair[0]), node_coords.get(&pair[1]))
            else {
                continue;
            };
            if pair[0] == pair[1]
                || !(structure_nodes.contains(&pair[0]) || structure_nodes.contains(&pair[1]))
            {
                continue;
            }
            let length = haversine_meters(from_lon, from_lat, to_lon, to_lat).max(0.01);
            adjacency
                .entry(pair[0])
                .or_default()
                .push((pair[1], length));
            adjacency
                .entry(pair[1])
                .or_default()
                .push((pair[0], length));
        }
    }

    let mut sorted_nodes = structure_nodes.iter().copied().collect::<Vec<_>>();
    sorted_nodes.sort_unstable();
    let mut visited = FxHashSet::default();
    let mut result = Vec::new();
    for start in sorted_nodes {
        if !visited.insert(start) {
            continue;
        }
        let mut component = vec![start];
        let mut anchors = Vec::new();
        let mut cursor = 0;
        while cursor < component.len() {
            let node = component[cursor];
            cursor += 1;
            for &(neighbour, _) in adjacency.get(&node).into_iter().flatten() {
                if structure_nodes.contains(&neighbour) {
                    if visited.insert(neighbour) {
                        component.push(neighbour);
                    }
                } else if node_coords[&neighbour].2.is_finite() && !anchors.contains(&neighbour) {
                    anchors.push(neighbour);
                }
            }
        }
        if anchors.is_empty() {
            continue;
        }
        let members = component.iter().copied().collect::<FxHashSet<_>>();
        let mut weighted = FxHashMap::<i64, (f64, f64)>::default();
        for &anchor in &anchors {
            let anchor_z = node_coords[&anchor].2;
            for (node, distance) in path_distances(anchor, &adjacency, &members) {
                let entry = weighted.entry(node).or_default();
                entry.0 += anchor_z / distance;
                entry.1 += 1.0 / distance;
            }
        }
        for node in component {
            if let Some((sum, weight)) = weighted.get(&node) {
                result.push((node, round_height_decimetres(sum / weight)));
            }
        }
    }
    result
}

/// Shortest path distances from `anchor` to the nodes of `members`, moving
/// only through members.
fn path_distances(
    anchor: i64,
    adjacency: &FxHashMap<i64, Vec<(i64, f64)>>,
    members: &FxHashSet<i64>,
) -> Vec<(i64, f64)> {
    let mut best = FxHashMap::<i64, f64>::default();
    let mut heap = BinaryHeap::new();
    for &(neighbour, length) in adjacency.get(&anchor).into_iter().flatten() {
        if members.contains(&neighbour) && best.get(&neighbour).is_none_or(|old| length < *old) {
            best.insert(neighbour, length);
            heap.push(Reverse((OrderedDistance(length), neighbour)));
        }
    }
    while let Some(Reverse((OrderedDistance(distance), node))) = heap.pop() {
        if best.get(&node).is_some_and(|known| distance > *known) {
            continue;
        }
        for &(neighbour, length) in adjacency.get(&node).into_iter().flatten() {
            let candidate = distance + length;
            if members.contains(&neighbour)
                && best.get(&neighbour).is_none_or(|old| candidate < *old)
            {
                best.insert(neighbour, candidate);
                heap.push(Reverse((OrderedDistance(candidate), neighbour)));
            }
        }
    }
    best.into_iter().collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct OrderedDistance(f64);

impl Eq for OrderedDistance {}

impl PartialOrd for OrderedDistance {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedDistance {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// Vertical profile of one segment in its source direction.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct SegmentElevation {
    pub(crate) ascent_m: f32,
    pub(crate) descent_m: f32,
    pub(crate) max_uphill_permille: u16,
    pub(crate) max_downhill_permille: u16,
}

impl SegmentElevation {
    /// The same segment traversed backwards.
    pub(crate) fn reversed(self) -> Self {
        Self {
            ascent_m: self.descent_m,
            descent_m: self.ascent_m,
            max_uphill_permille: self.max_downhill_permille,
            max_downhill_permille: self.max_uphill_permille,
        }
    }
}

/// Profile between two nodes with known heights; interior DEM samples are
/// taken every `spacing_m` on ground segments. Unknown end heights give an
/// empty profile.
pub(crate) fn segment_elevation(
    sampling: &DemSampling<'_>,
    from: (f64, f64, f64),
    to: (f64, f64, f64),
    structure: bool,
) -> SegmentElevation {
    if !(from.2.is_finite() && to.2.is_finite()) {
        return SegmentElevation::default();
    }
    let horizontal_m = haversine_meters(from.0, from.1, to.0, to.1);
    let pieces = if structure || horizontal_m <= sampling.spacing_m {
        1
    } else {
        (horizontal_m / sampling.spacing_m).ceil() as usize
    };
    let mut heights = Vec::with_capacity(pieces + 1);
    heights.push(from.2);
    for piece in 1..pieces {
        let t = piece as f64 / pieces as f64;
        let lon = from.0 + (to.0 - from.0) * t;
        let lat = from.1 + (to.1 - from.1) * t;
        heights.push(
            sampling
                .dem
                .sample(lon, lat)
                .unwrap_or(from.2 + (to.2 - from.2) * t),
        );
    }
    heights.push(to.2);
    let piece_m = horizontal_m / pieces as f64;
    let (mut ascent, mut descent, mut max_up, mut max_down) = (0.0, 0.0, 0.0_f64, 0.0_f64);
    for pair in heights.windows(2) {
        let rise = pair[1] - pair[0];
        if rise > 0.0 {
            ascent += rise;
        } else {
            descent -= rise;
        }
        if piece_m > f64::EPSILON {
            max_up = max_up.max(rise / piece_m);
            max_down = max_down.max(-rise / piece_m);
        }
    }
    SegmentElevation {
        ascent_m: quantize_decimetres(ascent),
        descent_m: quantize_decimetres(descent),
        max_uphill_permille: grade_to_permille(max_up),
        max_downhill_permille: grade_to_permille(max_down),
    }
}

#[cfg(test)]
mod tests;
