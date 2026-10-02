//! Digital elevation models: ESRI ASCII grid and GeoTIFF readers, the
//! WGS84-to-raster CRS transform, bilinear sampling, and the tiled raster
//! kept with a dataset for runtime height queries.
//!
//! Heights are returned exactly as stored in the raster, in its vertical
//! datum; nothing converts between height references.

use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use netweevil_core::{
    DemFormat, DemSourceInfo, ELEVATION_RASTER_SCHEMA_VERSION, ElevationRasterHeader, RasterCrs,
    RasterGrid, elevation::bilinear,
};
use netweevil_persist::ElevationRasterReader;
use proj4rs::Proj;
use rayon::prelude::*;

const WGS84_PROJ: &str = "+proj=longlat +datum=WGS84 +no_defs";

/// Resolves a user CRS: `EPSG:<code>` for the built-in table, or a PROJ.4
/// definition starting with `+proj=`.
pub fn resolve_crs(spec: &str) -> Result<RasterCrs> {
    let spec = spec.trim();
    if spec.starts_with("+proj=") {
        Proj::from_proj_string(spec).with_context(|| format!("invalid PROJ.4 CRS '{spec}'"))?;
        return Ok(RasterCrs::Proj4 {
            definition: spec.to_string(),
            label: None,
        });
    }
    let upper = spec.to_ascii_uppercase();
    let code = upper
        .strip_prefix("EPSG:")
        .unwrap_or(&upper)
        .parse::<u32>()
        .with_context(|| {
            format!("unsupported CRS '{spec}'; use EPSG:<code> or a '+proj=' definition")
        })?;
    epsg_crs(code).with_context(|| {
        format!("EPSG:{code} is not in the built-in table; pass its '+proj=' definition instead")
    })
}

/// Built-in EPSG definitions. Datum shifts use the published seven-parameter
/// Helmert approximations, which are well inside DEM accuracy.
fn epsg_crs(code: u32) -> Option<RasterCrs> {
    let definition = match code {
        4326 | 4258 | 4979 => return Some(RasterCrs::Geographic),
        3857 => "+proj=merc +a=6378137 +b=6378137 +lat_ts=0 +lon_0=0 +x_0=0 +y_0=0 +k=1 +units=m +no_defs".to_string(),
        2326 => "+proj=tmerc +lat_0=22.3121333333333 +lon_0=114.178555555556 +k=1 +x_0=836694.05 +y_0=819069.8 +ellps=intl +towgs84=-162.619,-276.959,-161.764,0.067753,-2.243649,-1.158827,-1.094246 +units=m +no_defs".to_string(),
        28992 => "+proj=sterea +lat_0=52.1561605555556 +lon_0=5.38763888888889 +k=0.9999079 +x_0=155000 +y_0=463000 +ellps=bessel +towgs84=565.417,50.3319,465.552,-0.398957,0.343988,-1.8774,4.0725 +units=m +no_defs".to_string(),
        27700 => "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.9996012717 +x_0=400000 +y_0=-100000 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs".to_string(),
        32601..=32660 => format!("+proj=utm +zone={} +datum=WGS84 +units=m +no_defs", code - 32600),
        32701..=32760 => format!("+proj=utm +zone={} +south +datum=WGS84 +units=m +no_defs", code - 32700),
        25828..=25838 => format!("+proj=utm +zone={} +ellps=GRS80 +towgs84=0,0,0,0,0,0,0 +units=m +no_defs", code - 25800),
        _ => return None,
    };
    Some(RasterCrs::Proj4 {
        definition,
        label: Some(format!("EPSG:{code}")),
    })
}

/// Transforms WGS84 longitude/latitude degrees into a raster CRS.
pub struct CrsTransform {
    projected: Option<(Proj, Proj)>,
}

impl CrsTransform {
    pub fn new(crs: &RasterCrs) -> Result<Self> {
        Ok(Self {
            projected: match crs {
                RasterCrs::Geographic => None,
                RasterCrs::Proj4 { definition, .. } => Some((
                    Proj::from_proj_string(WGS84_PROJ).context("building the WGS84 CRS")?,
                    Proj::from_proj_string(definition)
                        .with_context(|| format!("invalid raster CRS '{definition}'"))?,
                )),
            },
        })
    }

    pub fn to_raster(&self, lon: f64, lat: f64) -> Option<(f64, f64)> {
        let Some((wgs84, target)) = &self.projected else {
            return Some((lon, lat));
        };
        let mut point = (lon.to_radians(), lat.to_radians(), 0.0);
        proj4rs::transform::transform(wgs84, target, &mut point).ok()?;
        (point.0.is_finite() && point.1.is_finite()).then_some((point.0, point.1))
    }

    /// Approximate ground size in metres of one CRS unit at a location.
    fn metres_per_unit(&self, lat: f64) -> f64 {
        match self.projected {
            Some(_) => 1.0,
            None => 111_320.0 * lat.to_radians().cos().abs().max(0.01),
        }
    }
}

/// Longitude/latitude bounding box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LonLatBounds {
    pub min_lon: f64,
    pub min_lat: f64,
    pub max_lon: f64,
    pub max_lat: f64,
}

/// A DEM held in memory, in its own CRS. `NaN` marks nodata.
pub struct DemRaster {
    pub grid: RasterGrid,
    pub crs: RasterCrs,
    pub format: DemFormat,
    /// Grid of the file before cropping.
    pub source_grid: RasterGrid,
    values: Vec<f32>,
    transform: CrsTransform,
}

impl DemRaster {
    /// Reads `path` (`.asc` ESRI ASCII grid, otherwise GeoTIFF), cropped to
    /// the cells covering `crop` plus a margin. `crs` overrides the file's
    /// CRS; an ASCII grid without one must be geographic.
    pub fn read(path: &Path, crs: Option<&str>, crop: Option<LonLatBounds>) -> Result<Self> {
        let override_crs = crs.map(resolve_crs).transpose()?;
        let ascii = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("asc"));
        let (format, source_grid, grid, values, file_crs) = if ascii {
            let (source_grid, grid, values) = read_ascii_grid(path, override_crs.as_ref(), crop)?;
            (DemFormat::AsciiGrid, source_grid, grid, values, None)
        } else {
            let (grid, values, file_crs) = read_geotiff(path)?;
            (DemFormat::GeoTiff, grid, grid, values, file_crs)
        };
        let crs = match (override_crs, file_crs) {
            (Some(crs), _) | (None, Some(crs)) => crs,
            (None, None) if looks_geographic(&grid) => RasterCrs::Geographic,
            (None, None) => bail!(
                "{} has no CRS and its coordinates are not longitude/latitude; pass --dem-crs",
                path.display()
            ),
        };
        let transform = CrsTransform::new(&crs)?;
        let mut raster = Self {
            grid,
            crs,
            format,
            source_grid,
            values,
            transform,
        };
        if let Some(crop) = crop {
            raster.crop(crop)?;
        }
        Ok(raster)
    }

    /// Builds a raster from row-major values (tests and synthetic DEMs).
    pub fn from_values(grid: RasterGrid, crs: RasterCrs, values: Vec<f32>) -> Result<Self> {
        if values.len() != grid.columns as usize * grid.rows as usize {
            bail!(
                "raster has {} values for a {}x{} grid",
                values.len(),
                grid.columns,
                grid.rows
            );
        }
        let transform = CrsTransform::new(&crs)?;
        Ok(Self {
            grid,
            crs,
            format: DemFormat::AsciiGrid,
            source_grid: grid,
            values,
            transform,
        })
    }

    pub fn value(&self, column: u32, row: u32) -> Option<f32> {
        let value = self.values[row as usize * self.grid.columns as usize + column as usize];
        value.is_finite().then_some(value)
    }

    /// Bilinear height at a WGS84 position; `None` outside the raster or on
    /// nodata.
    pub fn sample(&self, lon: f64, lat: f64) -> Option<f64> {
        let (x, y) = self.transform.to_raster(lon, lat)?;
        let (column, row) = self.grid.cell_position(x, y)?;
        bilinear(&self.grid, column, row, |column, row| {
            self.value(column, row)
        })
    }

    /// Approximate ground size of a cell in metres near `lat`.
    pub fn cell_size_m(&self, lat: f64) -> f64 {
        self.grid.cell_width.max(self.grid.cell_height) * self.transform.metres_per_unit(lat)
    }

    pub fn source_info(
        &self,
        path: &Path,
        sha256: &str,
        vertical_datum: Option<String>,
    ) -> DemSourceInfo {
        DemSourceInfo {
            path: path.display().to_string(),
            sha256: sha256.to_string(),
            format: self.format,
            crs: self.crs.clone(),
            vertical_datum,
            grid: self.source_grid,
        }
    }

    /// Cell window `(first_column, first_row, columns, rows)` covering
    /// `bounds` plus a two-cell margin, or `None` when they do not overlap.
    fn window(&self, bounds: LonLatBounds) -> Option<(u32, u32, u32, u32)> {
        window_for(&self.grid, &self.transform, bounds)
    }

    fn crop(&mut self, bounds: LonLatBounds) -> Result<()> {
        let Some((first_column, first_row, columns, rows)) = self.window(bounds) else {
            bail!("the DEM does not cover the network");
        };
        if (columns, rows) == (self.grid.columns, self.grid.rows) {
            return Ok(());
        }
        let source_columns = self.grid.columns as usize;
        let values = (first_row..first_row + rows)
            .flat_map(|row| {
                let start = row as usize * source_columns + first_column as usize;
                self.values[start..start + columns as usize].iter().copied()
            })
            .collect();
        self.values = values;
        self.grid = cropped_grid(&self.grid, first_column, first_row, columns, rows);
        Ok(())
    }

    /// Splits the raster into the tiled, decimetre-rounded storage form.
    pub fn to_tiles(
        &self,
        source: DemSourceInfo,
        tile_size: u32,
    ) -> (ElevationRasterHeader, Vec<Vec<f64>>) {
        let header = ElevationRasterHeader {
            schema_version: ELEVATION_RASTER_SCHEMA_VERSION,
            source,
            grid: self.grid,
            tile_size,
        };
        let tiles = (0..header.tile_count())
            .into_par_iter()
            .map(|tile| {
                let (columns, rows) = header.tile_dimensions(tile);
                let first_column = (tile % header.tile_columns()) * tile_size as usize;
                let first_row = (tile / header.tile_columns()) * tile_size as usize;
                let mut values = Vec::with_capacity(columns * rows);
                for row in first_row..first_row + rows {
                    let start = row * self.grid.columns as usize + first_column;
                    values.extend(self.values[start..start + columns].iter().map(|value| {
                        if value.is_finite() {
                            (f64::from(*value) * 10.0).round() / 10.0
                        } else {
                            f64::NAN
                        }
                    }));
                }
                values
            })
            .collect();
        (header, tiles)
    }
}

fn cropped_grid(
    grid: &RasterGrid,
    first_column: u32,
    first_row: u32,
    columns: u32,
    rows: u32,
) -> RasterGrid {
    RasterGrid {
        columns,
        rows,
        min_x: grid.min_x + f64::from(first_column) * grid.cell_width,
        max_y: grid.max_y - f64::from(first_row) * grid.cell_height,
        cell_width: grid.cell_width,
        cell_height: grid.cell_height,
    }
}

/// Cell window covering the CRS image of `bounds` (its outline sampled
/// densely, since projected boxes are curved) plus two cells.
fn window_for(
    grid: &RasterGrid,
    transform: &CrsTransform,
    bounds: LonLatBounds,
) -> Option<(u32, u32, u32, u32)> {
    const STEPS: usize = 16;
    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for step in 0..=STEPS {
        let t = step as f64 / STEPS as f64;
        let lon = bounds.min_lon + (bounds.max_lon - bounds.min_lon) * t;
        let lat = bounds.min_lat + (bounds.max_lat - bounds.min_lat) * t;
        for (lon, lat) in [
            (lon, bounds.min_lat),
            (lon, bounds.max_lat),
            (bounds.min_lon, lat),
            (bounds.max_lon, lat),
        ] {
            if let Some((x, y)) = transform.to_raster(lon, lat) {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if !(min_x <= max_x && min_y <= max_y) {
        return None;
    }
    let first_column = ((min_x - grid.min_x) / grid.cell_width).floor() - 2.0;
    let last_column = ((max_x - grid.min_x) / grid.cell_width).ceil() + 2.0;
    let first_row = ((grid.max_y - max_y) / grid.cell_height).floor() - 2.0;
    let last_row = ((grid.max_y - min_y) / grid.cell_height).ceil() + 2.0;
    let first_column = first_column.max(0.0) as u32;
    let first_row = first_row.max(0.0) as u32;
    let last_column = last_column.min(f64::from(grid.columns)) as u32;
    let last_row = last_row.min(f64::from(grid.rows)) as u32;
    (first_column < last_column && first_row < last_row).then(|| {
        (
            first_column,
            first_row,
            last_column - first_column,
            last_row - first_row,
        )
    })
}

fn tokens(line: &[u8]) -> Vec<&[u8]> {
    line.split(|byte| byte.is_ascii_whitespace())
        .filter(|token| !token.is_empty())
        .collect()
}

fn looks_geographic(grid: &RasterGrid) -> bool {
    grid.min_x >= -180.0 && grid.max_x() <= 360.0 && grid.min_y() >= -90.0 && grid.max_y <= 90.0
}

// --- ESRI ASCII grid --------------------------------------------------------

/// Parses the header and the rows inside the crop window, returning the
/// file's grid, the cropped grid and its values. Rows normally sit one per
/// line and are parsed in parallel; files that wrap rows across lines fall
/// back to a sequential token scan.
fn read_ascii_grid(
    path: &Path,
    crs: Option<&RasterCrs>,
    crop: Option<LonLatBounds>,
) -> Result<(RasterGrid, RasterGrid, Vec<f32>)> {
    let file = File::open(path).with_context(|| format!("opening DEM {}", path.display()))?;
    let bytes = unsafe { memmap2::Mmap::map(&file) }
        .with_context(|| format!("memory-mapping DEM {}", path.display()))?;
    let mut offset = 0;
    let mut header = std::collections::BTreeMap::new();
    while header.len() < 7 {
        let line_end = bytes[offset..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |position| offset + position);
        let line = std::str::from_utf8(&bytes[offset..line_end])
            .context("DEM header is not UTF-8")?
            .trim();
        let mut fields = line.split_whitespace();
        let Some(key) = fields.next() else {
            offset = line_end + 1;
            continue;
        };
        if key.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '.') {
            break;
        }
        let value = fields
            .next()
            .with_context(|| format!("DEM header line '{line}' has no value"))?
            .parse::<f64>()
            .with_context(|| format!("DEM header line '{line}' has an invalid value"))?;
        header.insert(key.to_ascii_lowercase(), value);
        offset = (line_end + 1).min(bytes.len());
    }
    let get = |key: &str| header.get(key).copied();
    let columns = get("ncols").context("DEM header lacks ncols")? as u32;
    let rows = get("nrows").context("DEM header lacks nrows")? as u32;
    let cell_width = get("cellsize")
        .or_else(|| get("dx"))
        .context("DEM header lacks cellsize")?;
    let cell_height = get("cellsize")
        .or_else(|| get("dy"))
        .context("DEM header lacks cellsize")?;
    let min_x = match (get("xllcorner"), get("xllcenter")) {
        (Some(corner), _) => corner,
        (None, Some(center)) => center - cell_width / 2.0,
        _ => bail!("DEM header lacks xllcorner/xllcenter"),
    };
    let min_y = match (get("yllcorner"), get("yllcenter")) {
        (Some(corner), _) => corner,
        (None, Some(center)) => center - cell_height / 2.0,
        _ => bail!("DEM header lacks yllcorner/yllcenter"),
    };
    let nodata = get("nodata_value").map(|value| value as f32);
    if columns == 0 || rows == 0 || !(cell_width > 0.0 && cell_height > 0.0) {
        bail!("DEM header describes an empty grid");
    }
    let grid = RasterGrid {
        columns,
        rows,
        min_x,
        max_y: min_y + cell_height * f64::from(rows),
        cell_width,
        cell_height,
    };
    let window = match crop {
        None => (0, 0, columns, rows),
        Some(bounds) => {
            let probe_crs = match crs {
                Some(crs) => crs.clone(),
                None if looks_geographic(&grid) => RasterCrs::Geographic,
                None => bail!(
                    "{} has no CRS and its coordinates are not longitude/latitude; pass --dem-crs",
                    path.display()
                ),
            };
            window_for(&grid, &CrsTransform::new(&probe_crs)?, bounds)
                .context("the DEM does not cover the network")?
        }
    };
    let (first_column, first_row, window_columns, window_rows) = window;
    let parse = |token: &[u8]| -> Result<f32> {
        let value = std::str::from_utf8(token)
            .ok()
            .and_then(|text| text.parse::<f32>().ok())
            .with_context(|| format!("invalid DEM value '{}'", String::from_utf8_lossy(token)))?;
        Ok(if Some(value) == nodata || !value.is_finite() {
            f32::NAN
        } else {
            value
        })
    };
    let data = &bytes[offset..];
    let lines = data
        .split(|byte| *byte == b'\n')
        .filter(|line| line.iter().any(|byte| !byte.is_ascii_whitespace()))
        .collect::<Vec<_>>();
    let one_row_per_line = lines.len() == rows as usize
        && lines
            .first()
            .is_some_and(|line| tokens(line).len() == columns as usize);
    let values = if one_row_per_line {
        lines[first_row as usize..(first_row + window_rows) as usize]
            .par_iter()
            .map(|line| {
                let row = tokens(line);
                if row.len() != columns as usize {
                    bail!("DEM row has {} values, expected {columns}", row.len());
                }
                row[first_column as usize..(first_column + window_columns) as usize]
                    .iter()
                    .map(|token| parse(token))
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?
            .concat()
    } else {
        let all = data
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        if all.len() != columns as usize * rows as usize {
            bail!(
                "DEM has {} values, expected {}",
                all.len(),
                columns as usize * rows as usize
            );
        }
        (first_row..first_row + window_rows)
            .flat_map(|row| {
                let start = row as usize * columns as usize + first_column as usize;
                all[start..start + window_columns as usize].iter()
            })
            .map(|token| parse(token))
            .collect::<Result<Vec<_>>>()?
    };
    Ok((
        grid,
        cropped_grid(&grid, first_column, first_row, window_columns, window_rows),
        values,
    ))
}

// --- GeoTIFF ----------------------------------------------------------------

const GEOKEY_MODEL_TYPE: u16 = 1024;
const GEOKEY_RASTER_TYPE: u16 = 1025;
const GEOKEY_GEOGRAPHIC_TYPE: u16 = 2048;
const GEOKEY_PROJECTED_TYPE: u16 = 3072;
const RASTER_PIXEL_IS_POINT: u16 = 2;

/// Reads the first band of a north-up GeoTIFF, with its georeferencing from
/// ModelPixelScale/ModelTiepoint (or a north-up ModelTransformation) and
/// its CRS from the GeoKey directory when the EPSG code is in the built-in
/// table.
fn read_geotiff(path: &Path) -> Result<(RasterGrid, Vec<f32>, Option<RasterCrs>)> {
    use tiff::decoder::{Decoder, DecodingResult, Limits};
    use tiff::tags::Tag;

    let file = File::open(path).with_context(|| format!("opening DEM {}", path.display()))?;
    let mut decoder = Decoder::new(BufReader::new(file))
        .with_context(|| format!("reading GeoTIFF {}", path.display()))?
        .with_limits(Limits::unlimited());
    let (columns, rows) = decoder.dimensions()?;
    let geokeys = decoder
        .find_tag_unsigned_vec::<u16>(Tag::GeoKeyDirectoryTag)?
        .unwrap_or_default();
    let geokey = |id: u16| {
        geokeys
            .get(4..)
            .unwrap_or_default()
            .chunks_exact(4)
            .find(|entry| entry[0] == id && entry[1] == 0)
            .map(|entry| entry[3])
    };
    let scale = decoder
        .find_tag(Tag::ModelPixelScaleTag)?
        .map(|value| value.into_f64_vec())
        .transpose()?;
    let tiepoint = decoder
        .find_tag(Tag::ModelTiepointTag)?
        .map(|value| value.into_f64_vec())
        .transpose()?;
    let transformation = decoder
        .find_tag(Tag::ModelTransformationTag)?
        .map(|value| value.into_f64_vec())
        .transpose()?;
    let (cell_width, cell_height, origin_x, origin_y) = match (scale, tiepoint, transformation) {
        (Some(scale), Some(tiepoint), _) if scale.len() >= 2 && tiepoint.len() >= 6 => (
            scale[0],
            scale[1],
            tiepoint[3] - tiepoint[0] * scale[0],
            tiepoint[4] + tiepoint[1] * scale[1],
        ),
        (_, _, Some(matrix)) if matrix.len() >= 8 => {
            if matrix[1] != 0.0 || matrix[4] != 0.0 || matrix[5] >= 0.0 {
                bail!(
                    "{} is rotated or not north-up; reproject it first",
                    path.display()
                );
            }
            (matrix[0], -matrix[5], matrix[3], matrix[7])
        }
        _ => bail!("{} has no GeoTIFF georeferencing tags", path.display()),
    };
    // PixelIsPoint places the tie point at the first cell's centre.
    let (min_x, max_y) = if geokey(GEOKEY_RASTER_TYPE) == Some(RASTER_PIXEL_IS_POINT) {
        (origin_x - cell_width / 2.0, origin_y + cell_height / 2.0)
    } else {
        (origin_x, origin_y)
    };
    let crs = match geokey(GEOKEY_MODEL_TYPE) {
        Some(2) => Some(RasterCrs::Geographic),
        _ => geokey(GEOKEY_PROJECTED_TYPE)
            .or_else(|| geokey(GEOKEY_GEOGRAPHIC_TYPE))
            .and_then(|code| epsg_crs(u32::from(code))),
    };
    let nodata = decoder
        .find_tag(Tag::GdalNodata)?
        .and_then(|value| value.into_string().ok())
        .and_then(|text| text.trim_end_matches('\0').trim().parse::<f64>().ok());
    let samples = match decoder.colortype()? {
        tiff::ColorType::Gray(_) => 1,
        other => bail!("{} is not a single-band raster ({other:?})", path.display()),
    };
    let image = decoder.read_image()?;
    let to_f32 = |value: f64| -> f32 {
        if Some(value) == nodata || !value.is_finite() {
            f32::NAN
        } else {
            value as f32
        }
    };
    let values: Vec<f32> = match image {
        DecodingResult::U8(data) => data.into_iter().map(|v| to_f32(f64::from(v))).collect(),
        DecodingResult::U16(data) => data.into_iter().map(|v| to_f32(f64::from(v))).collect(),
        DecodingResult::U32(data) => data.into_iter().map(|v| to_f32(f64::from(v))).collect(),
        DecodingResult::I8(data) => data.into_iter().map(|v| to_f32(f64::from(v))).collect(),
        DecodingResult::I16(data) => data.into_iter().map(|v| to_f32(f64::from(v))).collect(),
        DecodingResult::I32(data) => data.into_iter().map(|v| to_f32(f64::from(v))).collect(),
        DecodingResult::F32(data) => data.into_iter().map(|v| to_f32(f64::from(v))).collect(),
        DecodingResult::F64(data) => data.into_iter().map(to_f32).collect(),
        _ => bail!("{} uses an unsupported sample type", path.display()),
    };
    if values.len() != columns as usize * rows as usize * samples {
        bail!(
            "{} decoded to an unexpected number of samples",
            path.display()
        );
    }
    let grid = RasterGrid {
        columns,
        rows,
        min_x,
        max_y,
        cell_width,
        cell_height,
    };
    Ok((grid, values, crs))
}

// --- Runtime sampling -------------------------------------------------------

/// Number of decoded tiles kept per sampler (~16 MiB at 256-cell tiles).
const TILE_CACHE_CAPACITY: usize = 64;

/// Bilinear sampling over a stored elevation raster, decoding tiles on
/// demand and keeping the most recently used ones.
pub struct ElevationSampler {
    reader: ElevationRasterReader,
    transform: CrsTransform,
    cache: Mutex<VecDeque<(usize, Arc<Vec<f32>>)>>,
}

impl ElevationSampler {
    pub fn open(path: &Path) -> Result<Self> {
        let reader = ElevationRasterReader::open(path)?;
        let transform = CrsTransform::new(&reader.header().source.crs)?;
        Ok(Self {
            reader,
            transform,
            cache: Mutex::new(VecDeque::with_capacity(TILE_CACHE_CAPACITY)),
        })
    }

    pub fn header(&self) -> &ElevationRasterHeader {
        self.reader.header()
    }

    fn tile(&self, tile_index: usize) -> Result<Arc<Vec<f32>>> {
        {
            let mut cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(position) = cache.iter().position(|(index, _)| *index == tile_index) {
                let entry = cache.remove(position).expect("position is in range");
                let tile = entry.1.clone();
                cache.push_front(entry);
                return Ok(tile);
            }
        }
        let tile = Arc::new(self.reader.tile(tile_index)?);
        let mut cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        if cache.len() == TILE_CACHE_CAPACITY {
            cache.pop_back();
        }
        cache.push_front((tile_index, tile.clone()));
        Ok(tile)
    }

    /// Height at a WGS84 position, `Ok(None)` outside coverage or on nodata.
    pub fn sample(&self, lon: f64, lat: f64) -> Result<Option<f64>> {
        let header = self.reader.header();
        let Some((x, y)) = self.transform.to_raster(lon, lat) else {
            return Ok(None);
        };
        let Some((column, row)) = header.grid.cell_position(x, y) else {
            return Ok(None);
        };
        let mut failure = None;
        let value = bilinear(&header.grid, column, row, |column, row| {
            let (tile_index, offset) = header.cell_tile(column, row);
            match self.tile(tile_index) {
                Ok(tile) => tile.get(offset).copied().filter(|value| value.is_finite()),
                Err(error) => {
                    failure.get_or_insert(error);
                    None
                }
            }
        });
        match failure {
            Some(error) => Err(error),
            None => Ok(value),
        }
    }
}

#[cfg(test)]
mod tests;
