//! `netweevil h3 layer`, `h3 surge` and `h3 dispatch`: stored cell layers,
//! the demand and supply balance, and ride dispatch.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use netweevil_h3::{
    CellIndex, CellLayer, ColumnSpec, LayerBuilder, PaintOp, Resolution, SurgeConfig, WeightedCell,
    compute_surge, cover_bbox, cover_polygon, default_column_specs, parse_cell, parse_resolution,
    plan_rebalance,
};
use netweevil_persist::WorkspacePaths;
use netweevil_profile::load_profile;
use netweevil_query::{DispatchRequest, PreparedRoutingEngine, execute_dispatch};
use serde_json::json;

use super::{COVER_CELL_LIMIT, CellTable, polygon_ring, read_cells};

#[derive(Subcommand, Debug)]
pub(crate) enum H3LayerCommand {
    /// Import a cell table or a point table into a layer.
    ///
    /// A table with a `cell` column sets values per cell. With
    /// `--resolution`, the rows are points and aggregate into cells.
    Import(H3LayerImportArgs),
    /// List the stored cell layers.
    List,
    /// Print the columns and statistics of a layer.
    Show {
        /// Layer id.
        layer: String,
    },
    /// Write a layer as CSV, GeoJSON or JSON.
    Export {
        /// Layer id.
        layer: String,
        /// Output file: `.csv`, `.geojson` or `.json`.
        #[arg(long)]
        out: PathBuf,
    },
    /// Set, add to, scale or clear one column on cells or on an area.
    Paint(H3LayerPaintArgs),
    /// Delete a stored layer.
    Remove {
        /// Layer id.
        layer: String,
    },
}

#[derive(Args, Debug)]
pub(crate) struct H3LayerImportArgs {
    /// CSV with a header row.
    table: PathBuf,
    /// Layer id. An existing layer keeps its other cells and columns.
    #[arg(long)]
    name: String,
    /// Aggregate the rows as points into cells of this resolution.
    #[arg(long)]
    resolution: Option<u8>,
    /// Column to import: `column`, `column:aggregation` or
    /// `column:aggregation=name`. Aggregations: sum, mean, min, max, count,
    /// last. `:count=name` counts rows. Repeat the flag for more columns.
    /// Without it, every numeric column is summed.
    #[arg(long = "value")]
    values: Vec<String>,
    /// Longitude column of a point table.
    #[arg(long, default_value = "lon")]
    lon_column: String,
    /// Latitude column of a point table.
    #[arg(long, default_value = "lat")]
    lat_column: String,
    /// Label of the layer.
    #[arg(long)]
    label: Option<String>,
    /// Start from an empty layer instead of merging into the stored one.
    #[arg(long)]
    replace: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum PaintOpArg {
    Set,
    Add,
    Multiply,
    Clear,
}

#[derive(Args, Debug)]
pub(crate) struct H3LayerPaintArgs {
    /// Layer id; a new layer is created when there is none.
    layer: String,
    /// Column to change.
    #[arg(long)]
    column: String,
    /// How the value changes the cells.
    #[arg(long, value_enum, default_value = "set")]
    op: PaintOpArg,
    /// Value to paint.
    #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
    value: f64,
    /// Cell ids, separated by commas.
    #[arg(long, value_delimiter = ',')]
    cells: Vec<String>,
    /// Paint the cells with their centre in `min_lon,min_lat,max_lon,max_lat`.
    #[arg(long, value_delimiter = ',', allow_hyphen_values = true)]
    bbox: Option<Vec<f64>>,
    /// Paint the cells with their centre in the polygon of a GeoJSON file.
    #[arg(long)]
    polygon: Option<PathBuf>,
    /// Resolution of the cells of `--bbox` and `--polygon`.
    #[arg(long)]
    resolution: Option<u8>,
}

#[derive(Args, Debug)]
pub(crate) struct H3SurgeArgs {
    /// Demand: `layer:column`, or a cell table (`.csv` or `.json`).
    #[arg(long)]
    demand: String,
    /// Supply: `layer:column`, or a cell table (`.csv` or `.json`).
    #[arg(long)]
    supply: String,
    /// Surge settings (JSON); the defaults apply without it.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Resolution of the surge cells; replaces the one of `--config`.
    #[arg(long)]
    resolution: Option<u8>,
    /// Also plan moves of surplus supply from at most this many rings away.
    #[arg(long)]
    rebalance_rings: Option<u32>,
    /// Output file: `.csv`, `.geojson` or `.json`.
    #[arg(long)]
    out: PathBuf,
}

#[derive(Args, Debug)]
pub(crate) struct H3DispatchArgs {
    /// Id of an imported dataset.
    #[arg(long)]
    dataset: String,
    /// Routing profile file (YAML or TOML).
    #[arg(long)]
    profile: PathBuf,
    /// Dispatch request (JSON) with `vehicles` and `requests`.
    #[arg(long)]
    request: PathBuf,
    /// Surge demand: `layer:column`, or a cell table.
    #[arg(long)]
    surge_demand: Option<String>,
    /// Surge supply: `layer:column`, or a cell table.
    #[arg(long)]
    surge_supply: Option<String>,
    /// Result file (JSON). Omit it to write to the runs directory.
    #[arg(long)]
    out: Option<PathBuf>,
}

/// Weighted cells of `layer:column`, or of a cell table file.
pub(crate) fn cell_source(paths: &WorkspacePaths, source: &str) -> Result<Vec<WeightedCell>> {
    let path = Path::new(source);
    if path.is_file() {
        return read_cells(path, None);
    }
    let (layer, column) = source.split_once(':').with_context(|| {
        format!("'{source}' is not a file; name a stored layer column as layer:column")
    })?;
    CellLayer::load(&paths.cell_layers_dir, layer)?.weighted(column)
}

/// Cells with their centre in a bbox and in the polygon of a GeoJSON file.
pub(super) fn area_cells(
    bbox: Option<&[f64]>,
    polygon: Option<&Path>,
    resolution: Resolution,
) -> Result<Vec<CellIndex>> {
    let mut cells = Vec::new();
    if let Some(bbox) = bbox {
        let bbox: [f64; 4] = bbox
            .try_into()
            .ok()
            .context("--bbox needs four values: min_lon,min_lat,max_lon,max_lat")?;
        cells.extend(cover_bbox(bbox, resolution, COVER_CELL_LIMIT)?);
    }
    if let Some(path) = polygon {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let ring = polygon_ring(&serde_json::from_str(&text).context("parsing GeoJSON")?)
            .with_context(|| format!("reading {}", path.display()))?;
        cells.extend(cover_polygon(&ring, resolution, COVER_CELL_LIMIT)?);
    }
    Ok(cells)
}

fn print_layer(layer: &CellLayer) {
    let summary = layer.summary();
    println!(
        "{}\t{} cells\tresolutions {:?}\t{}",
        summary.layer_id, summary.cell_count, summary.resolutions, summary.label
    );
    for column in summary.columns {
        println!(
            "  {}\tcells={}\tsum={}\tmin={}\tmax={}",
            column.name,
            column.count,
            column.sum,
            column.min.map_or("-".into(), |value| value.to_string()),
            column.max.map_or("-".into(), |value| value.to_string()),
        );
    }
}

fn load_or_new(dir: &Path, layer_id: &str) -> Result<CellLayer> {
    if CellLayer::path(dir, layer_id).exists() {
        CellLayer::load(dir, layer_id)
    } else {
        CellLayer::new(layer_id)
    }
}

pub(crate) fn h3_layer(paths: &WorkspacePaths, command: H3LayerCommand) -> Result<()> {
    let dir = &paths.cell_layers_dir;
    match command {
        H3LayerCommand::Import(args) => {
            let text = std::fs::read_to_string(&args.table)
                .with_context(|| format!("reading {}", args.table.display()))?;
            let resolution = args.resolution.map(parse_resolution).transpose()?;
            let mut specs = args
                .values
                .iter()
                .map(|spec| ColumnSpec::parse(spec))
                .collect::<Result<Vec<_>>>()?;
            if specs.is_empty() {
                specs = default_column_specs(&text, &[&args.lon_column, &args.lat_column])?;
            }
            if specs.is_empty() {
                specs.push(ColumnSpec::parse(":count=count")?);
            }
            let mut builder = LayerBuilder::new(specs)?;
            let (rows, skipped) =
                builder.add_csv(&text, resolution, &args.lon_column, &args.lat_column)?;
            let mut layer = if args.replace {
                CellLayer::new(&args.name)?
            } else {
                load_or_new(dir, &args.name)?
            };
            if let Some(label) = args.label {
                layer.label = label;
            }
            layer.merge(builder)?;
            layer.save(dir)?;
            println!("imported {rows} rows ({skipped} skipped)");
            print_layer(&layer);
        }
        H3LayerCommand::List => {
            for summary in CellLayer::list(dir)? {
                println!(
                    "{}\t{} cells\tcolumns: {}",
                    summary.layer_id,
                    summary.cell_count,
                    summary
                        .columns
                        .iter()
                        .map(|column| column.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        H3LayerCommand::Show { layer } => print_layer(&CellLayer::load(dir, &layer)?),
        H3LayerCommand::Export { layer, out } => {
            let layer = CellLayer::load(dir, &layer)?;
            let text = match out.extension().and_then(|extension| extension.to_str()) {
                Some("csv") => layer.to_csv()?,
                Some("geojson") => serde_json::to_string(&layer.geojson())?,
                Some("json") => serde_json::to_string_pretty(&layer)?,
                other => {
                    bail!("unsupported output extension {other:?}; use .csv, .geojson or .json")
                }
            };
            std::fs::write(&out, text).with_context(|| format!("writing {}", out.display()))?;
            println!("{} cells written to {}", layer.cells.len(), out.display());
        }
        H3LayerCommand::Paint(args) => {
            let mut cells = args
                .cells
                .iter()
                .map(|cell| parse_cell(cell))
                .collect::<Result<Vec<_>>>()?;
            if args.bbox.is_some() || args.polygon.is_some() {
                let resolution = parse_resolution(
                    args.resolution
                        .context("--bbox and --polygon need --resolution")?,
                )?;
                cells.extend(area_cells(
                    args.bbox.as_deref(),
                    args.polygon.as_deref(),
                    resolution,
                )?);
            }
            if cells.is_empty() {
                bail!("pass --cells, --bbox or --polygon");
            }
            let mut layer = load_or_new(dir, &args.layer)?;
            let op = match args.op {
                PaintOpArg::Set => PaintOp::Set,
                PaintOpArg::Add => PaintOp::Add,
                PaintOpArg::Multiply => PaintOp::Multiply,
                PaintOpArg::Clear => PaintOp::Clear,
            };
            let changed = layer.paint(cells, &args.column, op, args.value)?;
            layer.save(dir)?;
            println!("painted {changed} cells");
            print_layer(&layer);
        }
        H3LayerCommand::Remove { layer } => {
            if !CellLayer::remove(dir, &layer)? {
                bail!("no cell layer '{layer}'");
            }
            println!("removed cell layer '{layer}'");
        }
    }
    Ok(())
}

pub(crate) fn h3_surge(paths: &WorkspacePaths, args: H3SurgeArgs) -> Result<()> {
    let mut config: SurgeConfig = match &args.config {
        Some(path) => serde_json::from_str(
            &std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?,
        )
        .context("parsing the surge settings")?,
        None => SurgeConfig::default(),
    };
    if args.resolution.is_some() {
        config.resolution = args.resolution;
    }
    let cells = compute_surge(
        &cell_source(paths, &args.demand)?,
        &cell_source(paths, &args.supply)?,
        &config,
    )?;
    CellTable {
        columns: vec![
            "demand",
            "supply",
            "smoothed_demand",
            "smoothed_supply",
            "ratio",
            "multiplier",
        ],
        rows: cells
            .iter()
            .map(|cell| {
                (
                    cell.cell,
                    vec![
                        json!(cell.demand),
                        json!(cell.supply),
                        json!(cell.smoothed_demand),
                        json!(cell.smoothed_supply),
                        json!(cell.ratio),
                        json!(cell.multiplier),
                    ],
                )
            })
            .collect(),
    }
    .write(&args.out)?;
    println!(
        "{} cells, {} above 1.0 (max {:.2}), written to {}",
        cells.len(),
        cells.iter().filter(|cell| cell.multiplier > 1.0).count(),
        cells.iter().map(|cell| cell.multiplier).fold(1.0, f64::max),
        args.out.display()
    );
    if let Some(rings) = args.rebalance_rings {
        for step in plan_rebalance(&cells, &config, rings) {
            println!(
                "move {:.1} from {} to {} ({} cells)",
                step.amount, step.from, step.to, step.grid_distance
            );
        }
    }
    Ok(())
}

pub(crate) fn h3_dispatch(paths: &WorkspacePaths, args: H3DispatchArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let mut request: DispatchRequest = serde_json::from_str(
        &std::fs::read_to_string(&args.request)
            .with_context(|| format!("reading {}", args.request.display()))?,
    )
    .context("parsing the dispatch request")?;
    if args.surge_demand.is_some() || args.surge_supply.is_some() {
        let surge = request.surge.get_or_insert_with(Default::default);
        if let Some(source) = &args.surge_demand {
            surge.demand = Some(cell_source(paths, source)?);
        }
        if let Some(source) = &args.surge_supply {
            surge.supply = Some(cell_source(paths, source)?);
        }
    }
    let (topology, acceleration, _, compiled) =
        crate::analyze::load_route_execution_inputs(paths, &args.dataset, &profile)?;
    let engine = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let result = execute_dispatch(&engine, &request)?;
    let out = args
        .out
        .unwrap_or_else(|| paths.runs_dir.join("dispatch-result.json"));
    netweevil_persist::write_json(&out, &result)?;
    let summary = &result.summary;
    println!(
        "assigned {} of {} requests to {} vehicles ({:?}); mean pickup {}, total fare {}",
        summary.assigned_count,
        summary.request_count,
        summary.vehicle_count,
        summary.strategy,
        summary
            .mean_pickup_time_s
            .map_or("-".into(), |time| format!("{time:.0} s")),
        summary
            .total_fare
            .map_or("-".into(), |fare| format!("{fare:.2}")),
    );
    println!("written to {}", out.display());
    Ok(())
}
