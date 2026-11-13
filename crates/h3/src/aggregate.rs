//! Point aggregation into cells and resampling between resolutions.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use h3o::{CellIndex, Resolution};
use rustc_hash::FxHashMap;
use serde::Serialize;

use crate::cell_at;
use crate::io::{WeightedCell, serialize_cell};

/// Points counted in one cell.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CellStat {
    #[serde(serialize_with = "serialize_cell")]
    pub cell: CellIndex,
    pub resolution: u8,
    pub count: u64,
    /// Sum of the point weights.
    pub weight: f64,
}

/// Aggregates `(lon, lat, weight)` points into cells of every requested
/// resolution. Rows are ordered by resolution (coarse to fine), then cell.
/// Points with a non-finite coordinate are skipped; the second value is
/// their count.
pub fn aggregate_points(
    points: impl IntoIterator<Item = (f64, f64, f64)>,
    resolutions: &[Resolution],
) -> (Vec<CellStat>, usize) {
    let Some(finest) = resolutions.iter().copied().max() else {
        return (Vec::new(), 0);
    };
    let mut skipped = 0;
    let mut finest_cells = FxHashMap::<CellIndex, (u64, f64)>::default();
    for (lon, lat, weight) in points {
        let Some(cell) = cell_at(lon, lat, finest) else {
            skipped += 1;
            continue;
        };
        let entry = finest_cells.entry(cell).or_default();
        entry.0 += 1;
        entry.1 += weight;
    }
    let mut levels = BTreeMap::<(Resolution, CellIndex), (u64, f64)>::new();
    for (cell, (count, weight)) in finest_cells {
        for resolution in resolutions {
            let Some(parent) = cell.parent(*resolution) else {
                continue;
            };
            let entry = levels.entry((*resolution, parent)).or_default();
            entry.0 += count;
            entry.1 += weight;
        }
    }
    let stats = levels
        .into_iter()
        .map(|((resolution, cell), (count, weight))| CellStat {
            cell,
            resolution: u8::from(resolution),
            count,
            weight,
        })
        .collect();
    (stats, skipped)
}

/// Largest number of cells one resample may produce.
const RESAMPLE_CELL_LIMIT: u64 = 5_000_000;

/// Moves weighted cells to one resolution. A finer cell adds its weight to
/// its parent; a coarser cell spreads its weight evenly over its children.
/// The total weight is preserved. Rows are ordered by cell.
pub fn resample_cells(cells: &[WeightedCell], resolution: Resolution) -> Result<Vec<WeightedCell>> {
    let produced = cells
        .iter()
        .map(|row| row.cell.children_count(resolution).max(1))
        .sum::<u64>();
    if produced > RESAMPLE_CELL_LIMIT {
        bail!(
            "resampling to resolution {resolution} would produce {produced} cells; the limit is {RESAMPLE_CELL_LIMIT}"
        );
    }
    let mut out = BTreeMap::<CellIndex, f64>::new();
    for row in cells {
        if let Some(parent) = row.cell.parent(resolution) {
            *out.entry(parent).or_default() += row.weight;
        } else {
            let share = row.weight / row.cell.children_count(resolution) as f64;
            for child in row.cell.children(resolution) {
                *out.entry(child).or_default() += share;
            }
        }
    }
    Ok(out
        .into_iter()
        .map(|(cell, weight)| WeightedCell { cell, weight })
        .collect())
}
