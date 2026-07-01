//! Concave isochrone polygons from reachable network segments.
//!
//! The reachable segments are rasterized onto a metric grid (each sample
//! marks its cell plus the 8 neighbours as a one-cell buffer), and the
//! boundary of the marked region is traced into rings. Outer rings come out
//! counter-clockwise and enclosed cavities clockwise, so unreachable pockets
//! — water, restricted areas, missing network — become polygon holes
//! instead of being swept over by a convex hull.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::geometry::simplify_polygon_ring;

/// One traced polygon: an outer ring plus the holes it encloses, all closed
/// and in lon/lat coordinates (outer counter-clockwise, holes clockwise).
pub(crate) struct TracedPolygon {
    pub(crate) exterior: Vec<[f64; 2]>,
    pub(crate) holes: Vec<Vec<[f64; 2]>>,
}

/// Approximate metres per degree of latitude on the shared spherical model.
const METERS_PER_DEGREE_LAT: f64 =
    netweevil_core::geo::EARTH_RADIUS_M * std::f64::consts::PI / 180.0;

/// Rasterizes segment polylines onto a grid of `cell_size_m` cells and
/// traces the marked region's boundary into concave polygons with holes.
/// `segments` supplies straight sub-segments as (start, end) lon/lat pairs.
pub(crate) fn trace_reachable_polygons(
    segments: &[([f64; 2], [f64; 2])],
    cell_size_m: f64,
    simplification_tolerance_m: f64,
) -> Vec<TracedPolygon> {
    if segments.is_empty() {
        return Vec::new();
    }
    let cell_size_m = cell_size_m.max(1.0);

    let reference_lat = segments
        .iter()
        .map(|(start, end)| (start[1] + end[1]) * 0.5)
        .sum::<f64>()
        / segments.len() as f64;
    let lat_step = cell_size_m / METERS_PER_DEGREE_LAT;
    let lon_step =
        cell_size_m / (METERS_PER_DEGREE_LAT * reference_lat.to_radians().cos().abs().max(0.01));
    let origin_lon = segments
        .iter()
        .flat_map(|(start, end)| [start[0], end[0]])
        .fold(f64::INFINITY, f64::min)
        - lon_step;
    let origin_lat = segments
        .iter()
        .flat_map(|(start, end)| [start[1], end[1]])
        .fold(f64::INFINITY, f64::min)
        - lat_step;

    let cell_of = |lon: f64, lat: f64| -> (i32, i32) {
        (
            ((lon - origin_lon) / lon_step).floor() as i32,
            ((lat - origin_lat) / lat_step).floor() as i32,
        )
    };

    // Rasterize: sample along each sub-segment at half-cell spacing and mark
    // a 3x3 neighbourhood per sample as a one-cell buffer around the road.
    let mut marked = FxHashSet::<(i32, i32)>::default();
    for (start, end) in segments {
        let dx_cells = (end[0] - start[0]) / lon_step;
        let dy_cells = (end[1] - start[1]) / lat_step;
        let steps = (dx_cells.abs().max(dy_cells.abs()) * 2.0).ceil().max(1.0) as usize;
        for step in 0..=steps {
            let fraction = step as f64 / steps as f64;
            let lon = start[0] + (end[0] - start[0]) * fraction;
            let lat = start[1] + (end[1] - start[1]) * fraction;
            let (col, row) = cell_of(lon, lat);
            for d_col in -1..=1 {
                for d_row in -1..=1 {
                    marked.insert((col + d_col, row + d_row));
                }
            }
        }
    }

    // Boundary edges directed with the marked region on the LEFT: outer
    // boundaries trace counter-clockwise, cavity boundaries clockwise.
    // Corners are addressed by (col, row) of the cell whose lower-left
    // corner they are.
    let mut edges = FxHashMap::<(i32, i32), Vec<(i32, i32)>>::default();
    let mut edge_count = 0_usize;
    let push_edge =
        |from: (i32, i32), to: (i32, i32), edges: &mut FxHashMap<(i32, i32), Vec<(i32, i32)>>| {
            edges.entry(from).or_default().push(to);
        };
    for &(col, row) in &marked {
        if !marked.contains(&(col, row - 1)) {
            push_edge((col, row), (col + 1, row), &mut edges);
            edge_count += 1;
        }
        if !marked.contains(&(col + 1, row)) {
            push_edge((col + 1, row), (col + 1, row + 1), &mut edges);
            edge_count += 1;
        }
        if !marked.contains(&(col, row + 1)) {
            push_edge((col + 1, row + 1), (col, row + 1), &mut edges);
            edge_count += 1;
        }
        if !marked.contains(&(col - 1, row)) {
            push_edge((col, row + 1), (col, row), &mut edges);
            edge_count += 1;
        }
    }

    // Chain directed edges into rings. At corners where two regions touch
    // diagonally a vertex has two outgoing edges; take the sharpest left
    // turn relative to the incoming direction so each ring keeps its own
    // region on the left and rings never cross.
    let mut rings_grid = Vec::<Vec<(i32, i32)>>::new();
    let mut walked = 0_usize;
    while walked < edge_count {
        // Prefer starting at a plain vertex (one outgoing edge): starting at
        // a diagonal-touch junction has no incoming direction to disambiguate
        // the turn and could stitch two touching rings into a figure eight.
        let start = edges
            .iter()
            .filter(|(_, targets)| !targets.is_empty())
            .min_by_key(|(_, targets)| targets.len())
            .map(|(&vertex, _)| vertex);
        let Some(start) = start else {
            break;
        };
        let mut ring = vec![start];
        let mut cursor = start;
        let mut incoming = (0_i32, 0_i32);
        loop {
            let targets = edges.get_mut(&cursor).expect("boundary edges form cycles");
            let next = if targets.len() == 1 {
                targets.remove(0)
            } else {
                // Left turn of incoming direction (d_col, d_row) is
                // (-d_row, d_col); prefer left, then straight, then right.
                let mut chosen = 0;
                let mut best_rank = i32::MAX;
                for (position, target) in targets.iter().enumerate() {
                    let direction = (target.0 - cursor.0, target.1 - cursor.1);
                    let rank = if direction == (-incoming.1, incoming.0) {
                        0
                    } else if direction == incoming {
                        1
                    } else if direction == (incoming.1, -incoming.0) {
                        2
                    } else {
                        3
                    };
                    if rank < best_rank {
                        best_rank = rank;
                        chosen = position;
                    }
                }
                targets.remove(chosen)
            };
            walked += 1;
            incoming = (next.0 - cursor.0, next.1 - cursor.1);
            cursor = next;
            if cursor == start {
                break;
            }
            ring.push(cursor);
        }
        if ring.len() >= 4 {
            rings_grid.push(ring);
        }
    }

    // Split into outer rings (positive area) and holes (negative area) and
    // convert to lon/lat, dropping collinear lattice points along straight
    // grid runs first.
    struct GridRing {
        ring: Vec<(i32, i32)>,
        area: f64,
    }
    let mut outers = Vec::<GridRing>::new();
    let mut holes = Vec::<GridRing>::new();
    for ring in rings_grid {
        let ring = drop_collinear(ring);
        let area = signed_area(&ring);
        if area > 0.0 {
            outers.push(GridRing { ring, area });
        } else if area < 0.0 {
            holes.push(GridRing { ring, area });
        }
    }

    let to_lonlat = |corner: (i32, i32)| -> [f64; 2] {
        [
            origin_lon + corner.0 as f64 * lon_step,
            origin_lat + corner.1 as f64 * lat_step,
        ]
    };
    let finish_ring = |ring: &[(i32, i32)]| -> Option<Vec<[f64; 2]>> {
        simplify_polygon_ring(
            ring.iter().map(|&corner| to_lonlat(corner)).collect(),
            simplification_tolerance_m,
        )
    };

    let mut polygons = outers
        .iter()
        .filter_map(|outer| {
            Some((
                outer,
                TracedPolygon {
                    exterior: finish_ring(&outer.ring)?,
                    holes: Vec::new(),
                },
            ))
        })
        .collect::<Vec<_>>();

    // Attach each hole to the smallest outer ring containing it. The hole
    // interior (unmarked cavity) lies to the right of the hole's first
    // edge, giving a test point strictly inside the cavity.
    for hole in &holes {
        let first = hole.ring[0];
        let second = hole.ring[1];
        let direction = (second.0 - first.0, second.1 - first.1);
        let right_normal = (direction.1, -direction.0);
        let test_point = (
            (first.0 + second.0) as f64 / 2.0 + right_normal.0 as f64 * 0.5,
            (first.1 + second.1) as f64 / 2.0 + right_normal.1 as f64 * 0.5,
        );
        let mut best: Option<usize> = None;
        for (position, (outer, _)) in polygons.iter().enumerate() {
            if grid_ring_contains(&outer.ring, test_point)
                && best.is_none_or(|known| outer.area < polygons[known].0.area)
            {
                best = Some(position);
            }
        }
        if let Some(position) = best
            && let Some(ring) = finish_ring(&hole.ring)
        {
            polygons[position].1.holes.push(ring);
        }
    }

    polygons.into_iter().map(|(_, polygon)| polygon).collect()
}

fn drop_collinear(ring: Vec<(i32, i32)>) -> Vec<(i32, i32)> {
    let length = ring.len();
    if length < 4 {
        return ring;
    }
    let mut result = Vec::with_capacity(length);
    for index in 0..length {
        let previous = ring[(index + length - 1) % length];
        let current = ring[index];
        let next = ring[(index + 1) % length];
        let cross = (current.0 - previous.0) * (next.1 - current.1)
            - (current.1 - previous.1) * (next.0 - current.0);
        if cross != 0 {
            result.push(current);
        }
    }
    result
}

fn signed_area(ring: &[(i32, i32)]) -> f64 {
    let mut doubled = 0_i64;
    for index in 0..ring.len() {
        let current = ring[index];
        let next = ring[(index + 1) % ring.len()];
        doubled += current.0 as i64 * next.1 as i64 - next.0 as i64 * current.1 as i64;
    }
    doubled as f64 / 2.0
}

/// Even-odd containment of a fractional grid point in an integer-lattice
/// ring. The test point sits at cell centres (offset by 0.5), so it never
/// lies exactly on a lattice edge.
fn grid_ring_contains(ring: &[(i32, i32)], point: (f64, f64)) -> bool {
    let mut inside = false;
    for index in 0..ring.len() {
        let a = ring[index];
        let b = ring[(index + 1) % ring.len()];
        let (ax, ay) = (a.0 as f64, a.1 as f64);
        let (bx, by) = (b.0 as f64, b.1 as f64);
        if (ay > point.1) != (by > point.1) {
            let intersect_x = ax + (point.1 - ay) / (by - ay) * (bx - ax);
            if point.0 < intersect_x {
                inside = !inside;
            }
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meters_to_lon(meters: f64, lat: f64) -> f64 {
        meters / (METERS_PER_DEGREE_LAT * lat.to_radians().cos())
    }

    fn meters_to_lat(meters: f64) -> f64 {
        meters / METERS_PER_DEGREE_LAT
    }

    #[test]
    fn traces_a_single_segment_into_one_polygon() {
        let start = [6.0, 53.0];
        let end = [6.0 + meters_to_lon(500.0, 53.0), 53.0];
        let polygons = trace_reachable_polygons(&[(start, end)], 25.0, 0.0);
        assert_eq!(polygons.len(), 1);
        let polygon = &polygons[0];
        assert!(polygon.holes.is_empty());
        assert!(polygon.exterior.len() >= 5);
        assert_eq!(polygon.exterior.first(), polygon.exterior.last());
        // The polygon must hug the segment: no vertex farther than a few
        // cells from the segment's latitude.
        for vertex in &polygon.exterior {
            assert!(
                (vertex[1] - 53.0).abs() <= meters_to_lat(100.0),
                "vertex {vertex:?} strays from the segment"
            );
        }
    }

    #[test]
    fn a_ring_road_produces_a_hole_over_the_unreachable_middle() {
        // A square ring of roads, 1 km per side: the enclosed block is not
        // reachable network and must become a polygon hole, not be covered.
        let lat = 53.0;
        let side_m = 1_000.0;
        let lon = |meters: f64| 6.0 + meters_to_lon(meters, lat);
        let lat_at = |meters: f64| lat + meters_to_lat(meters);
        let corners = [
            [lon(0.0), lat_at(0.0)],
            [lon(side_m), lat_at(0.0)],
            [lon(side_m), lat_at(side_m)],
            [lon(0.0), lat_at(side_m)],
        ];
        let segments = (0..4)
            .map(|side| (corners[side], corners[(side + 1) % 4]))
            .collect::<Vec<_>>();
        let polygons = trace_reachable_polygons(&segments, 25.0, 0.0);
        assert_eq!(polygons.len(), 1);
        assert_eq!(
            polygons[0].holes.len(),
            1,
            "the unreachable middle must be a hole"
        );

        // The hole must cover most of the enclosed block: the centre of the
        // ring lies inside it.
        let hole = &polygons[0].holes[0];
        let centre = (lon(side_m / 2.0), lat_at(side_m / 2.0));
        let mut inside = false;
        for index in 0..hole.len() - 1 {
            let a = hole[index];
            let b = hole[index + 1];
            if (a[1] > centre.1) != (b[1] > centre.1) {
                let intersect = a[0] + (centre.1 - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                if centre.0 < intersect {
                    inside = !inside;
                }
            }
        }
        assert!(inside, "ring centre must lie inside the traced hole");
    }

    #[test]
    fn disconnected_segments_produce_separate_polygons() {
        let lat = 53.0;
        let near = ([6.0, lat], [6.0 + meters_to_lon(200.0, lat), lat]);
        let far_lon = 6.0 + meters_to_lon(5_000.0, lat);
        let far = ([far_lon, lat], [far_lon + meters_to_lon(200.0, lat), lat]);
        let polygons = trace_reachable_polygons(&[near, far], 25.0, 0.0);
        assert_eq!(polygons.len(), 2);
    }
}
