//! H3 hexagonal cell layers.
//!
//! Cells carry values such as population, POI counts or traffic patterns.
//! A layer may mix resolutions: a lookup resolves a coordinate to the
//! finest cell of the layer that covers it, so a coarse base layer can be
//! refined locally. Everything here runs at import or setup time; no
//! routing query touches a cell.

mod aggregate;
mod cover;
mod io;
mod layer;
mod network;
mod surge;

use anyhow::{Context, Result, bail};
use h3o::LatLng;
pub use h3o::{CellIndex, Resolution};
use rustc_hash::FxHashMap;

pub use aggregate::{CellStat, aggregate_points, resample_cells};
pub use cover::{compact_cells, cover_bbox, cover_polygon, uncompact_cells};
pub use io::{WeightedCell, cells_geojson, read_cells_csv};
pub use layer::{
    Aggregation, CellLayer, CellLayerSummary, ColumnSpec, ColumnSummary, LayerBuilder, PaintOp,
    default_column_specs, validate_layer_id,
};
pub use network::{NetworkCellStat, edge_midpoint, summarize_network};
pub use surge::{RebalanceMove, SurgeCell, SurgeConfig, SurgeIndex, compute_surge, plan_rebalance};

/// Parses an H3 cell id in its hexadecimal form (`882830828dfffff`).
pub fn parse_cell(text: &str) -> Result<CellIndex> {
    let trimmed = text.trim();
    trimmed
        .parse::<CellIndex>()
        .map_err(|error| anyhow::anyhow!("invalid H3 cell '{trimmed}': {error}"))
}

/// Checks a resolution number (0 through 15).
pub fn parse_resolution(resolution: u8) -> Result<Resolution> {
    Resolution::try_from(resolution)
        .ok()
        .with_context(|| format!("H3 resolution must be 0 through 15, got {resolution}"))
}

/// Checks a list of resolutions; duplicates are dropped and the result is
/// sorted from coarse to fine.
pub fn parse_resolutions(resolutions: &[u8]) -> Result<Vec<Resolution>> {
    if resolutions.is_empty() {
        bail!("at least one H3 resolution is required");
    }
    let mut parsed = resolutions
        .iter()
        .map(|resolution| parse_resolution(*resolution))
        .collect::<Result<Vec<_>>>()?;
    parsed.sort_unstable();
    parsed.dedup();
    Ok(parsed)
}

/// The cell that contains a coordinate; `None` for a non-finite coordinate.
pub fn cell_at(lon: f64, lat: f64, resolution: Resolution) -> Option<CellIndex> {
    LatLng::new(lat, lon)
        .ok()
        .map(|point| point.to_cell(resolution))
}

/// Centre of a cell as `[lon, lat]`.
pub fn cell_center(cell: CellIndex) -> [f64; 2] {
    let center = LatLng::from(cell);
    [center.lng(), center.lat()]
}

/// Closed outer ring of a cell as `[lon, lat]` pairs. A cell that crosses
/// the antimeridian keeps contiguous longitudes above 180 degrees.
pub fn cell_ring(cell: CellIndex) -> Vec<[f64; 2]> {
    let boundary = cell.boundary();
    let mut ring = boundary
        .iter()
        .map(|vertex| [vertex.lng(), vertex.lat()])
        .collect::<Vec<_>>();
    let (min, max) = ring
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), point| {
            (min.min(point[0]), max.max(point[0]))
        });
    if max - min > 180.0 {
        for point in &mut ring {
            if point[0] < 0.0 {
                point[0] += 360.0;
            }
        }
    }
    if let Some(first) = ring.first().copied() {
        ring.push(first);
    }
    ring
}

/// Values keyed by cells of one or more resolutions.
#[derive(Debug, Clone)]
pub struct CellMap<T> {
    values: FxHashMap<CellIndex, T>,
    /// Resolutions in use, finest first.
    resolutions: Vec<Resolution>,
}

impl<T> Default for CellMap<T> {
    fn default() -> Self {
        Self {
            values: FxHashMap::default(),
            resolutions: Vec::new(),
        }
    }
}

impl<T> CellMap<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Resolutions in use, finest first.
    pub fn resolutions(&self) -> &[Resolution] {
        &self.resolutions
    }

    pub fn insert(&mut self, cell: CellIndex, value: T) -> Option<T> {
        self.note_resolution(cell.resolution());
        self.values.insert(cell, value)
    }

    /// The value of `cell`, inserted from `default` when absent.
    pub fn entry_or_insert_with(&mut self, cell: CellIndex, default: impl FnOnce() -> T) -> &mut T {
        self.note_resolution(cell.resolution());
        self.values.entry(cell).or_insert_with(default)
    }

    fn note_resolution(&mut self, resolution: Resolution) {
        if let Err(position) = self
            .resolutions
            .binary_search_by(|probe| resolution.cmp(probe))
        {
            self.resolutions.insert(position, resolution);
        }
    }

    pub fn get(&self, cell: CellIndex) -> Option<&T> {
        self.values.get(&cell)
    }

    pub fn iter(&self) -> impl Iterator<Item = (CellIndex, &T)> {
        self.values.iter().map(|(cell, value)| (*cell, value))
    }

    /// Cells that cover a coordinate, finest first.
    pub fn covering(&self, lon: f64, lat: f64) -> impl Iterator<Item = (CellIndex, &T)> {
        let finest = self
            .resolutions
            .first()
            .and_then(|resolution| cell_at(lon, lat, *resolution));
        self.resolutions.iter().filter_map(move |resolution| {
            let cell = finest?.parent(*resolution)?;
            self.values.get(&cell).map(|value| (cell, value))
        })
    }

    /// The finest cell that covers a coordinate.
    pub fn lookup(&self, lon: f64, lat: f64) -> Option<(CellIndex, &T)> {
        self.covering(lon, lat).next()
    }
}

/// A set of cells of one or more resolutions.
pub type CellSet = CellMap<()>;

impl CellSet {
    /// Parses a list of cell ids into a set.
    pub fn parse<S: AsRef<str>>(cells: &[S]) -> Result<Self> {
        let mut set = Self::new();
        for cell in cells {
            set.insert(parse_cell(cell.as_ref())?, ());
        }
        Ok(set)
    }

    pub fn contains(&self, lon: f64, lat: f64) -> bool {
        self.lookup(lon, lat).is_some()
    }
}

#[cfg(test)]
mod tests;
