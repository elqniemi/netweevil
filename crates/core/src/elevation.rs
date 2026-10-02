//! Digital elevation model (DEM) metadata and the per-edge grade layer
//! derived from it at import.
//!
//! A DEM raster stays in its own horizontal CRS; callers transform a
//! longitude/latitude into that CRS and then use [`RasterGrid`] to find the
//! fractional cell position. [`bilinear`] interpolates between the four
//! surrounding cell centres.

use serde::{Deserialize, Serialize};

/// Regular north-up raster grid in its CRS units. Row 0 is the top
/// (northernmost) row; column 0 is the leftmost column. Cell values describe
/// the cell centre.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RasterGrid {
    pub columns: u32,
    pub rows: u32,
    /// CRS x of the left edge of column 0.
    pub min_x: f64,
    /// CRS y of the top edge of row 0.
    pub max_y: f64,
    pub cell_width: f64,
    /// Positive cell height; y decreases with the row index.
    pub cell_height: f64,
}

impl RasterGrid {
    pub fn max_x(&self) -> f64 {
        self.min_x + self.cell_width * f64::from(self.columns)
    }

    pub fn min_y(&self) -> f64 {
        self.max_y - self.cell_height * f64::from(self.rows)
    }

    /// Fractional cell position of a CRS coordinate: integer values are
    /// cell centres. Returns `None` outside the raster extent.
    pub fn cell_position(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        if !(x.is_finite() && y.is_finite())
            || x < self.min_x
            || x > self.max_x()
            || y > self.max_y
            || y < self.min_y()
        {
            return None;
        }
        Some((
            (x - self.min_x) / self.cell_width - 0.5,
            (self.max_y - y) / self.cell_height - 0.5,
        ))
    }

    /// CRS coordinate of a cell centre.
    pub fn cell_center(&self, column: u32, row: u32) -> (f64, f64) {
        (
            self.min_x + (f64::from(column) + 0.5) * self.cell_width,
            self.max_y - (f64::from(row) + 0.5) * self.cell_height,
        )
    }
}

/// Bilinear interpolation at a fractional cell position (see
/// [`RasterGrid::cell_position`]). Positions in the outer half cell use the
/// edge cells. Missing neighbours (nodata) are dropped and the remaining
/// weights renormalized, but a position whose own cell is nodata has no
/// value: coverage never extends beyond the raster's valid cells.
pub fn bilinear(
    grid: &RasterGrid,
    column: f64,
    row: f64,
    mut value: impl FnMut(u32, u32) -> Option<f32>,
) -> Option<f64> {
    let max_column = f64::from(grid.columns.checked_sub(1)?);
    let max_row = f64::from(grid.rows.checked_sub(1)?);
    let own_column = (column + 0.5).floor().clamp(0.0, max_column) as u32;
    let own_row = (row + 0.5).floor().clamp(0.0, max_row) as u32;
    value(own_column, own_row)?;

    let column = column.clamp(0.0, max_column);
    let row = row.clamp(0.0, max_row);
    let left = column.floor();
    let top = row.floor();
    let fx = column - left;
    let fy = row - top;
    let right = (left + 1.0).min(max_column);
    let bottom = (top + 1.0).min(max_row);
    let mut weighted = 0.0;
    let mut weight_sum = 0.0;
    for (x, y, weight) in [
        (left, top, (1.0 - fx) * (1.0 - fy)),
        (right, top, fx * (1.0 - fy)),
        (left, bottom, (1.0 - fx) * fy),
        (right, bottom, fx * fy),
    ] {
        if weight <= 0.0 {
            continue;
        }
        if let Some(sample) = value(x as u32, y as u32) {
            weighted += weight * f64::from(sample);
            weight_sum += weight;
        }
    }
    (weight_sum > 0.0).then(|| weighted / weight_sum)
}

/// Horizontal coordinate reference system of a raster. Externally tagged so
/// bundles can store it with bincode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RasterCrs {
    /// Longitude/latitude degrees on WGS84 or a datum treated as identical
    /// at DEM resolution (for example ETRS89).
    Geographic,
    /// A projected CRS given as a PROJ.4 definition. `label` records the
    /// EPSG code or name the user supplied.
    Proj4 {
        definition: String,
        #[serde(default)]
        label: Option<String>,
    },
}

/// Source file format of a DEM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemFormat {
    /// ESRI ASCII grid (`.asc`).
    AsciiGrid,
    /// GeoTIFF (`.tif`, `.tiff`).
    GeoTiff,
}

/// Provenance and georeferencing of a DEM sampled at import.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DemSourceInfo {
    pub path: String,
    pub sha256: String,
    pub format: DemFormat,
    pub crs: RasterCrs,
    /// Free-text vertical datum supplied by the user (for example `HKPD`
    /// or `EGM2008`). DEM heights are stored as read, without conversion.
    #[serde(default)]
    pub vertical_datum: Option<String>,
    /// The source raster's grid (before any cropping for storage).
    pub grid: RasterGrid,
}

/// Schema version of the stored elevation raster bundle.
pub const ELEVATION_RASTER_SCHEMA_VERSION: u32 = 1;

/// Header of the DEM raster stored with a dataset for runtime height
/// queries: the source grid cropped to the network, split into square tiles
/// stored row-major (tiles on the right and bottom edges are smaller).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElevationRasterHeader {
    pub schema_version: u32,
    pub source: DemSourceInfo,
    /// The stored, cropped grid in the source CRS.
    pub grid: RasterGrid,
    pub tile_size: u32,
}

impl ElevationRasterHeader {
    pub fn tile_columns(&self) -> usize {
        self.grid.columns.div_ceil(self.tile_size) as usize
    }

    pub fn tile_rows(&self) -> usize {
        self.grid.rows.div_ceil(self.tile_size) as usize
    }

    pub fn tile_count(&self) -> usize {
        self.tile_columns() * self.tile_rows()
    }

    /// `(columns, rows)` of the cells stored in a tile.
    pub fn tile_dimensions(&self, tile_index: usize) -> (usize, usize) {
        let tile_size = self.tile_size as usize;
        let tile_column = tile_index % self.tile_columns().max(1);
        let tile_row = tile_index / self.tile_columns().max(1);
        let columns = (self.grid.columns as usize - tile_column * tile_size).min(tile_size);
        let rows = (self.grid.rows as usize - tile_row * tile_size).min(tile_size);
        (columns, rows)
    }

    /// Tile index and row-major offset within it of a grid cell.
    pub fn cell_tile(&self, column: u32, row: u32) -> (usize, usize) {
        let tile_size = self.tile_size as usize;
        let (column, row) = (column as usize, row as usize);
        let tile_index = (row / tile_size) * self.tile_columns() + column / tile_size;
        let (tile_columns, _) = self.tile_dimensions(tile_index);
        (
            tile_index,
            (row % tile_size) * tile_columns + column % tile_size,
        )
    }
}

/// Elevation layer of a topology bundle. Present only when node elevations
/// were sampled from a DEM.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TopologyElevation {
    #[serde(default)]
    pub dem: Option<DemImportSummary>,
    /// Steepest uphill grade along each directed edge, in permille
    /// (`85` = 8.5 %). Empty without a DEM.
    pub edge_max_uphill_permille: Vec<u16>,
    /// Steepest downhill grade along each directed edge, in permille.
    pub edge_max_downhill_permille: Vec<u16>,
}

impl TopologyElevation {
    /// Steepest (uphill, downhill) grades of an edge as rise/run fractions.
    pub fn edge_max_grades(&self, edge_index: usize) -> Option<(f64, f64)> {
        let up = *self.edge_max_uphill_permille.get(edge_index)?;
        let down = *self.edge_max_downhill_permille.get(edge_index)?;
        Some((f64::from(up) / 1000.0, f64::from(down) / 1000.0))
    }
}

/// What the import did with the DEM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DemImportSummary {
    pub source: DemSourceInfo,
    /// Maximum spacing between elevation samples along an edge, in metres.
    pub sample_spacing_m: f64,
    pub sampled_node_count: u64,
    /// Nodes outside the raster or on nodata cells; their elevation stays
    /// unknown (`NaN`).
    pub missing_node_count: u64,
    /// Nodes of bridge/tunnel ways whose elevation was interpolated along
    /// the way between its ends instead of read from the terrain.
    pub structure_node_count: u64,
}

/// Grade (rise/run) to the stored permille value, saturating.
pub fn grade_to_permille(grade: f64) -> u16 {
    if !grade.is_finite() || grade <= 0.0 {
        return 0;
    }
    (grade * 1000.0).round().min(f64::from(u16::MAX)) as u16
}

/// Rounds a height to whole decimetres in the exact form the bundle codec
/// stores compactly (`k as f64 / 10.0`); non-finite values pass through.
pub fn round_height_decimetres(value_m: f64) -> f64 {
    if !value_m.is_finite() {
        return value_m;
    }
    (value_m * 10.0).round() / 10.0
}

/// Rounds a vertical distance to whole decimetres in the exact form the
/// bundle codec stores compactly (`k as f32 / 10.0`).
pub fn quantize_decimetres(value_m: f64) -> f32 {
    if !value_m.is_finite() {
        return 0.0;
    }
    ((value_m * 10.0).round() as i32) as f32 / 10.0
}

#[cfg(test)]
mod tests;
