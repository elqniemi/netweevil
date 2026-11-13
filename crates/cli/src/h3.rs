//! `netweevil h3`: H3 cell layers. Aggregate points, POIs and the network
//! into cells, cover areas, and convert cell tables for other commands.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use netweevil_h3::{
    CellIndex, WeightedCell, aggregate_points, cell_center, cells_geojson, compact_cells,
    cover_bbox, cover_polygon, parse_resolution, parse_resolutions, read_cells_csv, resample_cells,
    summarize_network,
};
use netweevil_ingest::scan_osm_tagged_points;
use netweevil_persist::{WorkspacePaths, read_dataset_manifest, read_topology_bundle};
use serde_json::{Map, Value, json};

/// Most cells one `h3 cover` call writes.
const COVER_CELL_LIMIT: usize = 2_000_000;

#[derive(Subcommand, Debug)]
pub(crate) enum H3Command {
    /// Count points per H3 cell on one or more resolutions.
    ///
    /// The points are a CSV with longitude and latitude columns, such as
    /// population points, POIs or addresses.
    Aggregate(H3AggregateArgs),
    /// Count tagged OSM nodes and ways per H3 cell.
    ///
    /// Reads an OSM PBF file and keeps the objects with a tag key such as
    /// `amenity` or `shop`.
    Pois(H3PoisArgs),
    /// Summarize the nodes and edges of a dataset per H3 cell.
    Network(H3NetworkArgs),
    /// List the H3 cells whose centre is in a bounding box or polygon.
    Cover(H3CoverArgs),
    /// Write the centres of H3 cells as a point set.
    ///
    /// The point ids are the cell ids, so the results of an analysis join
    /// back to the cells.
    Points(H3PointsArgs),
    /// Move a weighted cell table to another resolution.
    ///
    /// Finer cells add up in their parent. Coarser cells spread their
    /// weight evenly over their children.
    Resample(H3ResampleArgs),
}

#[derive(Args, Debug)]
pub(crate) struct H3AggregateArgs {
    /// Point CSV with a header row.
    points: PathBuf,
    /// H3 resolutions (0 to 15), separated by commas.
    #[arg(long, value_delimiter = ',', required = true)]
    resolutions: Vec<u8>,
    /// Longitude column.
    #[arg(long, default_value = "lon")]
    lon_column: String,
    /// Latitude column.
    #[arg(long, default_value = "lat")]
    lat_column: String,
    /// Column summed into `weight`; every point weighs 1 without it.
    #[arg(long)]
    weight_column: Option<String>,
    /// Output file: `.csv`, `.geojson` or `.json`.
    #[arg(long)]
    out: PathBuf,
}

#[derive(Args, Debug)]
pub(crate) struct H3PoisArgs {
    /// OSM extract (`.osm.pbf`).
    pbf: PathBuf,
    /// Tag key that marks a point of interest.
    #[arg(long, default_value = "amenity")]
    key: String,
    /// Tag values to keep, separated by commas; every value without it.
    #[arg(long, value_delimiter = ',')]
    values: Vec<String>,
    /// H3 resolutions (0 to 15), separated by commas.
    #[arg(long, value_delimiter = ',', required = true)]
    resolutions: Vec<u8>,
    /// Output file with the counts: `.csv`, `.geojson` or `.json`.
    #[arg(long)]
    out: PathBuf,
    /// Also write the points as a point-set CSV (`id,lon,lat,value,name`).
    #[arg(long)]
    points_out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct H3NetworkArgs {
    /// Id of an imported dataset.
    #[arg(long)]
    dataset: String,
    /// H3 resolution (0 to 15).
    #[arg(long)]
    resolution: u8,
    /// Output file: `.csv`, `.geojson` or `.json`.
    #[arg(long)]
    out: PathBuf,
}

#[derive(Args, Debug)]
pub(crate) struct H3CoverArgs {
    /// Bounding box: `min_lon,min_lat,max_lon,max_lat`.
    #[arg(
        long,
        value_delimiter = ',',
        allow_hyphen_values = true,
        conflicts_with = "polygon",
        required_unless_present = "polygon"
    )]
    bbox: Option<Vec<f64>>,
    /// GeoJSON file with a polygon; its outer ring is covered.
    #[arg(long)]
    polygon: Option<PathBuf>,
    /// H3 resolution (0 to 15).
    #[arg(long)]
    resolution: u8,
    /// Replace complete groups of sibling cells by their parent.
    #[arg(long)]
    compact: bool,
    /// Output file: `.csv`, `.geojson` or `.json`.
    #[arg(long)]
    out: PathBuf,
}

#[derive(Args, Debug)]
pub(crate) struct H3PointsArgs {
    /// Cell table: CSV with a `cell` column, or JSON list of `{cell, weight}`.
    cells: PathBuf,
    /// Weight column of a CSV table; `weight` when present without it.
    #[arg(long)]
    weight_column: Option<String>,
    /// Output point-set CSV (`id,lon,lat,weight`).
    #[arg(long)]
    out: PathBuf,
}

#[derive(Args, Debug)]
pub(crate) struct H3ResampleArgs {
    /// Cell table: CSV with a `cell` column, or JSON list of `{cell, weight}`.
    cells: PathBuf,
    /// Target H3 resolution (0 to 15).
    #[arg(long)]
    resolution: u8,
    /// Weight column of a CSV table; `weight` when present without it.
    #[arg(long)]
    weight_column: Option<String>,
    /// Output file: `.csv`, `.geojson` or `.json`.
    #[arg(long)]
    out: PathBuf,
}

pub(crate) fn h3_command(paths: &WorkspacePaths, command: H3Command) -> Result<()> {
    match command {
        H3Command::Aggregate(args) => h3_aggregate(args),
        H3Command::Pois(args) => h3_pois(args),
        H3Command::Network(args) => h3_network(paths, args),
        H3Command::Cover(args) => h3_cover(args),
        H3Command::Points(args) => h3_points(args),
        H3Command::Resample(args) => h3_resample(args),
    }
}

/// Cells with named values, written as CSV, GeoJSON or JSON.
struct CellTable {
    columns: Vec<&'static str>,
    rows: Vec<(CellIndex, Vec<Value>)>,
}

impl CellTable {
    fn weighted(cells: &[WeightedCell]) -> Self {
        Self {
            columns: vec!["weight"],
            rows: cells
                .iter()
                .map(|row| (row.cell, vec![json!(row.weight)]))
                .collect(),
        }
    }

    fn properties(&self, values: &[Value]) -> Map<String, Value> {
        self.columns
            .iter()
            .zip(values)
            .map(|(column, value)| (column.to_string(), value.clone()))
            .collect()
    }

    fn write(&self, out: &Path) -> Result<()> {
        if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let context = || format!("writing {}", out.display());
        match out.extension().and_then(|extension| extension.to_str()) {
            Some("csv") => {
                let mut writer = csv::Writer::from_path(out).with_context(context)?;
                writer.write_record(["cell", "resolution"].iter().chain(&self.columns))?;
                for (cell, values) in &self.rows {
                    let mut record =
                        vec![cell.to_string(), u8::from(cell.resolution()).to_string()];
                    record.extend(values.iter().map(|value| match value {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    }));
                    writer.write_record(record)?;
                }
                writer.flush().with_context(context)
            }
            Some("geojson") => {
                let collection = cells_geojson(
                    self.rows
                        .iter()
                        .map(|(cell, values)| (*cell, self.properties(values))),
                );
                std::fs::write(out, serde_json::to_string(&collection)?).with_context(context)
            }
            Some("json") => {
                let rows = self
                    .rows
                    .iter()
                    .map(|(cell, values)| {
                        let mut row = Map::new();
                        row.insert("cell".into(), json!(cell.to_string()));
                        row.insert("resolution".into(), json!(u8::from(cell.resolution())));
                        row.extend(self.properties(values));
                        Value::Object(row)
                    })
                    .collect::<Vec<_>>();
                std::fs::write(out, serde_json::to_string_pretty(&rows)?).with_context(context)
            }
            other => bail!("unsupported output extension {other:?}; use .csv, .geojson or .json"),
        }
    }
}

fn read_cells(path: &Path, weight_column: Option<&str>) -> Result<Vec<WeightedCell>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let cells = match path.extension().and_then(|extension| extension.to_str()) {
        Some("csv") => read_cells_csv(&text, weight_column),
        Some("json") => serde_json::from_str(&text).context("parsing the cell list"),
        other => bail!("unsupported cell table extension {other:?}; use .csv or .json"),
    }
    .with_context(|| format!("reading {}", path.display()))?;
    if cells.is_empty() {
        bail!("{} has no cells", path.display());
    }
    Ok(cells)
}

fn stats_table(stats: &[netweevil_h3::CellStat]) -> CellTable {
    CellTable {
        columns: vec!["count", "weight"],
        rows: stats
            .iter()
            .map(|stat| (stat.cell, vec![json!(stat.count), json!(stat.weight)]))
            .collect(),
    }
}

fn h3_aggregate(args: H3AggregateArgs) -> Result<()> {
    let resolutions = parse_resolutions(&args.resolutions)?;
    let mut reader = csv::Reader::from_path(&args.points)
        .with_context(|| format!("reading {}", args.points.display()))?;
    let headers = reader.headers()?.clone();
    let column = |name: &str| {
        headers
            .iter()
            .position(|header| header.trim() == name)
            .with_context(|| format!("{} has no '{name}' column", args.points.display()))
    };
    let lon_column = column(&args.lon_column)?;
    let lat_column = column(&args.lat_column)?;
    let weight_column = args.weight_column.as_deref().map(column).transpose()?;
    let mut points = Vec::new();
    for (row, record) in reader.records().enumerate() {
        let record = record?;
        let number = |index: usize| {
            let raw = record.get(index).unwrap_or("").trim();
            raw.parse::<f64>()
                .with_context(|| format!("row {}: invalid number '{raw}'", row + 2))
        };
        points.push((
            number(lon_column)?,
            number(lat_column)?,
            weight_column.map(number).transpose()?.unwrap_or(1.0),
        ));
    }
    let point_count = points.len();
    let (stats, skipped) = aggregate_points(points, &resolutions);
    stats_table(&stats).write(&args.out)?;
    println!(
        "aggregated {} points into {} cells on {} resolutions ({} skipped), written to {}",
        point_count - skipped,
        stats.len(),
        resolutions.len(),
        skipped,
        args.out.display()
    );
    Ok(())
}

fn h3_pois(args: H3PoisArgs) -> Result<()> {
    let resolutions = parse_resolutions(&args.resolutions)?;
    let points = scan_osm_tagged_points(&args.pbf, &args.key, &args.values)?;
    if points.is_empty() {
        bail!(
            "{} has no node or way with the tag '{}'",
            args.pbf.display(),
            args.key
        );
    }
    let (stats, _) = aggregate_points(
        points.iter().map(|point| (point.lon, point.lat, 1.0)),
        &resolutions,
    );
    stats_table(&stats).write(&args.out)?;
    println!(
        "counted {} '{}' points in {} cells, written to {}",
        points.len(),
        args.key,
        stats.len(),
        args.out.display()
    );
    if let Some(points_out) = &args.points_out {
        let mut writer = csv::Writer::from_path(points_out)
            .with_context(|| format!("writing {}", points_out.display()))?;
        writer.write_record(["id", "lon", "lat", "value", "name"])?;
        for point in &points {
            writer.write_record([
                point.id.as_str(),
                &point.lon.to_string(),
                &point.lat.to_string(),
                point.value.as_str(),
                point.name.as_deref().unwrap_or(""),
            ])?;
        }
        writer.flush()?;
        println!("points written to {}", points_out.display());
    }
    Ok(())
}

fn h3_network(paths: &WorkspacePaths, args: H3NetworkArgs) -> Result<()> {
    let resolution = parse_resolution(args.resolution)?;
    let dataset = read_dataset_manifest(paths, &args.dataset)
        .with_context(|| format!("reading dataset manifest for '{}'", args.dataset))?;
    let topology_ref = dataset
        .topology_bundle
        .context("dataset is missing a topology bundle")?;
    let topology = read_topology_bundle(&topology_ref.path)?;
    let stats = summarize_network(&topology, resolution);
    CellTable {
        columns: vec!["node_count", "edge_count", "edge_length_m"],
        rows: stats
            .iter()
            .map(|stat| {
                (
                    stat.cell,
                    vec![
                        json!(stat.node_count),
                        json!(stat.edge_count),
                        json!(stat.edge_length_m),
                    ],
                )
            })
            .collect(),
    }
    .write(&args.out)?;
    println!(
        "dataset '{}' covers {} cells at resolution {}, written to {}",
        args.dataset,
        stats.len(),
        args.resolution,
        args.out.display()
    );
    Ok(())
}

/// Outer ring of the first polygon in a GeoJSON document.
fn polygon_ring(document: &Value) -> Result<Vec<[f64; 2]>> {
    let geometry = match document["type"].as_str() {
        Some("FeatureCollection") => &document["features"][0]["geometry"],
        Some("Feature") => &document["geometry"],
        _ => document,
    };
    let ring = match geometry["type"].as_str() {
        Some("Polygon") => &geometry["coordinates"][0],
        Some("MultiPolygon") => &geometry["coordinates"][0][0],
        other => bail!("expected a Polygon or MultiPolygon geometry, got {other:?}"),
    };
    ring.as_array()
        .context("polygon has no outer ring")?
        .iter()
        .map(|position| {
            Ok([
                position[0].as_f64().context("invalid polygon coordinate")?,
                position[1].as_f64().context("invalid polygon coordinate")?,
            ])
        })
        .collect()
}

fn h3_cover(args: H3CoverArgs) -> Result<()> {
    let resolution = parse_resolution(args.resolution)?;
    let mut cells = match (&args.bbox, &args.polygon) {
        (Some(bbox), _) => {
            let bbox: [f64; 4] = bbox
                .as_slice()
                .try_into()
                .ok()
                .context("--bbox needs four values: min_lon,min_lat,max_lon,max_lat")?;
            cover_bbox(bbox, resolution, COVER_CELL_LIMIT)?
        }
        (None, Some(path)) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            let ring = polygon_ring(&serde_json::from_str(&text).context("parsing GeoJSON")?)
                .with_context(|| format!("reading {}", path.display()))?;
            cover_polygon(&ring, resolution, COVER_CELL_LIMIT)?
        }
        (None, None) => bail!("pass --bbox or --polygon"),
    };
    if args.compact {
        cells = compact_cells(cells)?;
    }
    CellTable {
        columns: Vec::new(),
        rows: cells.iter().map(|cell| (*cell, Vec::new())).collect(),
    }
    .write(&args.out)?;
    println!("{} cells written to {}", cells.len(), args.out.display());
    Ok(())
}

fn h3_points(args: H3PointsArgs) -> Result<()> {
    let cells = read_cells(&args.cells, args.weight_column.as_deref())?;
    let mut writer = csv::Writer::from_path(&args.out)
        .with_context(|| format!("writing {}", args.out.display()))?;
    writer.write_record(["id", "lon", "lat", "weight"])?;
    for row in &cells {
        let [lon, lat] = cell_center(row.cell);
        writer.write_record([
            row.cell.to_string(),
            lon.to_string(),
            lat.to_string(),
            row.weight.to_string(),
        ])?;
    }
    writer.flush()?;
    println!("{} points written to {}", cells.len(), args.out.display());
    Ok(())
}

fn h3_resample(args: H3ResampleArgs) -> Result<()> {
    let cells = read_cells(&args.cells, args.weight_column.as_deref())?;
    let resampled = resample_cells(&cells, parse_resolution(args.resolution)?)?;
    CellTable::weighted(&resampled).write(&args.out)?;
    println!(
        "{} cells resampled to {} cells at resolution {}, written to {}",
        cells.len(),
        resampled.len(),
        args.resolution,
        args.out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use netweevil_h3::{cell_at, parse_resolution};

    use super::*;

    #[test]
    fn aggregate_points_and_resample_round_trip_through_files() {
        let dir = std::env::temp_dir().join(format!("netweevil-h3-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let points = dir.join("points.csv");
        std::fs::write(
            &points,
            "id,x,y,population\na,6.5665,53.2194,120\nb,6.5666,53.2194,30\nc,6.70,53.25,50\n",
        )
        .unwrap();
        let cells_csv = dir.join("cells.csv");
        h3_aggregate(H3AggregateArgs {
            points,
            resolutions: vec![9, 7],
            lon_column: "x".into(),
            lat_column: "y".into(),
            weight_column: Some("population".into()),
            out: cells_csv.clone(),
        })
        .unwrap();
        let text = std::fs::read_to_string(&cells_csv).unwrap();
        let home = cell_at(6.5665, 53.2194, parse_resolution(9).unwrap()).unwrap();
        assert!(text.starts_with("cell,resolution,count,weight\n"));
        assert!(text.contains(&format!("{home},9,2,150.0\n")), "{text}");

        // The mixed-resolution table holds every point twice.
        let cells = read_cells(&cells_csv, None).unwrap();
        assert_eq!(cells.iter().map(|row| row.weight).sum::<f64>(), 400.0);

        let coarse = dir.join("coarse.json");
        h3_resample(H3ResampleArgs {
            cells: cells_csv.clone(),
            resolution: 6,
            weight_column: None,
            out: coarse.clone(),
        })
        .unwrap();
        let coarse_cells = read_cells(&coarse, None).unwrap();
        assert_eq!(
            coarse_cells.iter().map(|row| row.weight).sum::<f64>(),
            400.0
        );

        let centers = dir.join("centers.csv");
        h3_points(H3PointsArgs {
            cells: coarse,
            weight_column: None,
            out: centers.clone(),
        })
        .unwrap();
        let document = netweevil_query::load_point_set(&centers).unwrap();
        assert_eq!(document.points.len(), coarse_cells.len());
        assert_eq!(document.points[0].id, coarse_cells[0].cell.to_string());

        let cover = dir.join("cover.geojson");
        h3_cover(H3CoverArgs {
            bbox: Some(vec![6.50, 53.18, 6.62, 53.25]),
            polygon: None,
            resolution: 8,
            compact: false,
            out: cover.clone(),
        })
        .unwrap();
        let collection: Value =
            serde_json::from_str(&std::fs::read_to_string(&cover).unwrap()).unwrap();
        let ring = polygon_ring(&collection).unwrap();
        assert_eq!(ring.len(), 7);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
