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

    let marked = rasterize_segments(segments, cell_of, lon_step, lat_step);

    // Boundary edges directed with the marked region on the LEFT: outer
    // boundaries trace counter-clockwise, cavity boundaries clockwise.
    // Corners are addressed by (col, row) of the cell whose lower-left
    // corner they are. A corner has at most two outgoing boundary edges
    // (two outgoing only where regions touch diagonally).
    let mut edges = FxHashMap::<(i32, i32), ([(i32, i32); 2], u8)>::default();
    let mut corners = Vec::<(i32, i32)>::new();
    let mut edge_count = 0_usize;
    let mut push_edge = |from: (i32, i32), to: (i32, i32)| {
        let entry = edges.entry(from).or_insert_with(|| {
            corners.push(from);
            ([(0, 0); 2], 0)
        });
        entry.0[entry.1 as usize] = to;
        entry.1 += 1;
        edge_count += 1;
    };
    for (col, row) in marked.cells() {
        if !marked.contains((col, row - 1)) {
            push_edge((col, row), (col + 1, row));
        }
        if !marked.contains((col + 1, row)) {
            push_edge((col + 1, row), (col + 1, row + 1));
        }
        if !marked.contains((col, row + 1)) {
            push_edge((col + 1, row + 1), (col, row + 1));
        }
        if !marked.contains((col - 1, row)) {
            push_edge((col, row + 1), (col, row));
        }
    }

    // Chain directed edges into rings. At corners where two regions touch
    // diagonally a vertex has two outgoing edges; take the sharpest left
    // turn relative to the incoming direction so each ring keeps its own
    // region on the left and rings never cross. Rings start at plain corners
    // (one outgoing edge) in scan order: starting at a diagonal-touch
    // junction has no incoming direction to disambiguate the turn and could
    // stitch two touching rings into a figure eight.
    let starts = corners
        .iter()
        .copied()
        .filter(|corner| edges[corner].1 == 1)
        .chain(corners.iter().copied().filter(|corner| edges[corner].1 > 1))
        .collect::<Vec<_>>();
    let mut rings_grid = Vec::<Vec<(i32, i32)>>::new();
    let mut walked = 0_usize;
    for start in starts {
        if walked >= edge_count {
            break;
        }
        while edges[&start].1 > 0 {
            let mut ring = vec![start];
            let mut cursor = start;
            let mut incoming = (0_i32, 0_i32);
            loop {
                let (targets, count) = edges.get_mut(&cursor).expect("boundary edges form cycles");
                let chosen = if *count == 1 {
                    0
                } else {
                    // Left turn of incoming direction (d_col, d_row) is
                    // (-d_row, d_col); prefer left, then straight, then right.
                    let mut chosen = 0;
                    let mut best_rank = i32::MAX;
                    for (position, target) in targets[..*count as usize].iter().enumerate() {
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
                    chosen
                };
                let next = targets[chosen];
                targets[chosen] = targets[*count as usize - 1];
                *count -= 1;
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
    // edge: the centre of the cavity cell beside its first unit step is a
    // test point strictly inside the cavity and never on a lattice line,
    // whatever the edge length after collinear points were dropped.
    for hole in &holes {
        let first = hole.ring[0];
        let second = hole.ring[1];
        let direction = ((second.0 - first.0).signum(), (second.1 - first.1).signum());
        let right_normal = (direction.1, -direction.0);
        let test_point = (
            first.0 as f64 + direction.0 as f64 * 0.5 + right_normal.0 as f64 * 0.5,
            first.1 as f64 + direction.1 as f64 * 0.5 + right_normal.1 as f64 * 0.5,
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

/// Dense grids above this many cells fall back to sparse cell sets.
const MAX_DENSE_GRID_CELLS: u64 = 1 << 28;

/// Marked cells of the raster: a row-major bitmap over the bounding box of
/// the samples, or a hash set when the box is too large for a bitmap.
enum MarkedCells {
    Dense {
        min_col: i32,
        min_row: i32,
        width: usize,
        height: usize,
        words: usize,
        bits: Vec<u64>,
    },
    Sparse(FxHashSet<(i32, i32)>),
}

impl MarkedCells {
    fn contains(&self, (col, row): (i32, i32)) -> bool {
        match self {
            Self::Dense {
                min_col,
                min_row,
                width,
                height,
                words,
                bits,
            } => {
                let (x, y) = (col - min_col, row - min_row);
                if x < 0 || y < 0 || x as usize >= *width || y as usize >= *height {
                    return false;
                }
                let (x, y) = (x as usize, y as usize);
                bits[y * words + x / 64] & (1 << (x % 64)) != 0
            }
            Self::Sparse(cells) => cells.contains(&(col, row)),
        }
    }

    /// Marked cells in row-major order (dense) or arbitrary order (sparse).
    fn cells(&self) -> Vec<(i32, i32)> {
        match self {
            Self::Dense {
                min_col,
                min_row,
                height,
                words,
                bits,
                ..
            } => {
                let mut cells = Vec::new();
                for y in 0..*height {
                    for word_index in 0..*words {
                        let mut word = bits[y * words + word_index];
                        while word != 0 {
                            let bit = word.trailing_zeros() as usize;
                            word &= word - 1;
                            cells.push((
                                min_col + (word_index * 64 + bit) as i32,
                                min_row + y as i32,
                            ));
                        }
                    }
                }
                cells
            }
            Self::Sparse(cells) => {
                let mut cells = cells.iter().copied().collect::<Vec<_>>();
                cells.sort_unstable_by_key(|&(col, row)| (row, col));
                cells
            }
        }
    }
}

/// Samples each sub-segment at half-cell spacing and marks the 3x3
/// neighbourhood of every sample cell, as a one-cell buffer around roads.
fn rasterize_segments(
    segments: &[([f64; 2], [f64; 2])],
    cell_of: impl Fn(f64, f64) -> (i32, i32),
    lon_step: f64,
    lat_step: f64,
) -> MarkedCells {
    let mut samples = Vec::new();
    for (start, end) in segments {
        let dx_cells = (end[0] - start[0]) / lon_step;
        let dy_cells = (end[1] - start[1]) / lat_step;
        let steps = (dx_cells.abs().max(dy_cells.abs()) * 2.0).ceil().max(1.0) as usize;
        let mut previous = None;
        for step in 0..=steps {
            let fraction = step as f64 / steps as f64;
            let lon = start[0] + (end[0] - start[0]) * fraction;
            let lat = start[1] + (end[1] - start[1]) * fraction;
            let cell = cell_of(lon, lat);
            if previous != Some(cell) {
                samples.push(cell);
                previous = Some(cell);
            }
        }
    }
    let (mut min_col, mut min_row) = (i32::MAX, i32::MAX);
    let (mut max_col, mut max_row) = (i32::MIN, i32::MIN);
    for &(col, row) in &samples {
        min_col = min_col.min(col);
        min_row = min_row.min(row);
        max_col = max_col.max(col);
        max_row = max_row.max(row);
    }
    // One cell of dilation on every side.
    let (min_col, min_row) = (min_col - 1, min_row - 1);
    let width = (i64::from(max_col) + 2 - i64::from(min_col)) as u64;
    let height = (i64::from(max_row) + 2 - i64::from(min_row)) as u64;
    if width.saturating_mul(height) > MAX_DENSE_GRID_CELLS {
        let mut marked = FxHashSet::default();
        for (col, row) in samples {
            for d_col in -1..=1 {
                for d_row in -1..=1 {
                    marked.insert((col + d_col, row + d_row));
                }
            }
        }
        return MarkedCells::Sparse(marked);
    }
    let (width, height) = (width as usize, height as usize);
    let words = width.div_ceil(64);
    let mut core = vec![0_u64; words * height];
    for (col, row) in samples {
        let (x, y) = ((col - min_col) as usize, (row - min_row) as usize);
        core[y * words + x / 64] |= 1 << (x % 64);
    }
    // Horizontal then vertical dilation by one cell.
    let mut horizontal = vec![0_u64; words * height];
    for y in 0..height {
        let row = &core[y * words..(y + 1) * words];
        for word_index in 0..words {
            let word = row[word_index];
            let mut dilated = word | (word << 1) | (word >> 1);
            if word_index > 0 {
                dilated |= row[word_index - 1] >> 63;
            }
            if word_index + 1 < words {
                dilated |= row[word_index + 1] << 63;
            }
            horizontal[y * words + word_index] = dilated;
        }
    }
    let mut bits = vec![0_u64; words * height];
    for y in 0..height {
        for word_index in 0..words {
            let mut dilated = horizontal[y * words + word_index];
            if y > 0 {
                dilated |= horizontal[(y - 1) * words + word_index];
            }
            if y + 1 < height {
                dilated |= horizontal[(y + 1) * words + word_index];
            }
            bits[y * words + word_index] = dilated;
        }
    }
    MarkedCells::Dense {
        min_col,
        min_row,
        width,
        height,
        words,
        bits,
    }
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
mod tests;
