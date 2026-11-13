//! Cells that cover a bounding box or polygon, and compaction.

use std::collections::VecDeque;

use anyhow::{Result, bail};
use h3o::{CellIndex, Resolution};
use rustc_hash::FxHashSet;

use crate::{cell_at, cell_center};

/// Cells of `resolution` whose centre lies in `[min_lon, min_lat, max_lon,
/// max_lat]`, ordered by cell. A box smaller than one cell yields the cell
/// at its centre. Fails when more than `limit` cells would be visited.
pub fn cover_bbox(bbox: [f64; 4], resolution: Resolution, limit: usize) -> Result<Vec<CellIndex>> {
    let [min_lon, min_lat, max_lon, max_lat] = bbox;
    if !(bbox.iter().all(|value| value.is_finite()) && min_lon <= max_lon && min_lat <= max_lat) {
        bail!("bbox must be finite [min_lon, min_lat, max_lon, max_lat], got {bbox:?}");
    }
    let Some(seed) = cell_at(
        (min_lon + max_lon) / 2.0,
        (min_lat + max_lat) / 2.0,
        resolution,
    ) else {
        bail!("bbox {bbox:?} is outside the valid coordinate range");
    };
    // The flood walks through one extra ring of cells around the box, so
    // cells whose centres are inside stay connected along the box edges.
    let pad_lat = (2.0 * resolution.edge_length_m() / 111_320.0).min(90.0);
    let widest_lat = min_lat.abs().max(max_lat.abs()).min(89.0);
    let pad_lon = (pad_lat / widest_lat.to_radians().cos()).min(180.0);
    let in_padded = |point: [f64; 2]| {
        point[0] >= min_lon - pad_lon
            && point[0] <= max_lon + pad_lon
            && point[1] >= min_lat - pad_lat
            && point[1] <= max_lat + pad_lat
    };
    let inside = |point: [f64; 2]| {
        point[0] >= min_lon && point[0] <= max_lon && point[1] >= min_lat && point[1] <= max_lat
    };
    let mut seen = FxHashSet::default();
    let mut queue = VecDeque::from([seed]);
    let mut cells = Vec::new();
    seen.insert(seed);
    while let Some(cell) = queue.pop_front() {
        if seen.len() > limit.saturating_mul(2).max(64) {
            bail!(
                "covering the area at resolution {resolution} needs more than {limit} cells; use a coarser resolution"
            );
        }
        if inside(cell_center(cell)) {
            cells.push(cell);
        }
        for neighbor in cell.grid_disk::<Vec<_>>(1) {
            if in_padded(cell_center(neighbor)) && seen.insert(neighbor) {
                queue.push_back(neighbor);
            }
        }
    }
    if cells.len() > limit {
        bail!(
            "covering the area at resolution {resolution} needs {} cells; the limit is {limit}",
            cells.len()
        );
    }
    if cells.is_empty() {
        cells.push(seed);
    }
    cells.sort_unstable();
    Ok(cells)
}

/// Cells of `resolution` whose centre lies inside a polygon ring of
/// `[lon, lat]` pairs (even-odd rule), ordered by cell.
pub fn cover_polygon(
    ring: &[[f64; 2]],
    resolution: Resolution,
    limit: usize,
) -> Result<Vec<CellIndex>> {
    if ring.len() < 3 {
        bail!("a polygon needs at least 3 vertices");
    }
    let mut bbox = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for point in ring {
        bbox[0] = bbox[0].min(point[0]);
        bbox[1] = bbox[1].min(point[1]);
        bbox[2] = bbox[2].max(point[0]);
        bbox[3] = bbox[3].max(point[1]);
    }
    let mut cells = cover_bbox(bbox, resolution, limit)?;
    cells.retain(|cell| ring_contains(ring, cell_center(*cell)));
    Ok(cells)
}

fn ring_contains(ring: &[[f64; 2]], point: [f64; 2]) -> bool {
    let mut inside = false;
    let mut previous = ring[ring.len() - 1];
    for &current in ring {
        if (current[1] > point[1]) != (previous[1] > point[1])
            && point[0]
                < (previous[0] - current[0]) * (point[1] - current[1]) / (previous[1] - current[1])
                    + current[0]
        {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

/// Replaces complete sets of sibling cells by their parent. The input must
/// have one resolution and no duplicates.
pub fn compact_cells(mut cells: Vec<CellIndex>) -> Result<Vec<CellIndex>> {
    CellIndex::compact(&mut cells)
        .map_err(|error| anyhow::anyhow!("cells cannot be compacted: {error}"))?;
    cells.sort_unstable();
    Ok(cells)
}

/// Expands cells to their descendants at `resolution`, ordered by cell.
/// Fails when a cell is finer than `resolution`.
pub fn uncompact_cells(cells: &[CellIndex], resolution: Resolution) -> Result<Vec<CellIndex>> {
    if let Some(finer) = cells.iter().find(|cell| cell.resolution() > resolution) {
        bail!(
            "cell {finer} has resolution {}, finer than the target {resolution}",
            finer.resolution()
        );
    }
    let mut out = CellIndex::uncompact(cells.iter().copied(), resolution).collect::<Vec<_>>();
    out.sort_unstable();
    out.dedup();
    Ok(out)
}
