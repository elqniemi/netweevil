use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use netweevil_core::SourceFormat;
use netweevil_ingest::{
    DEFAULT_MAX_FILL_GROWTH, DatasetImportOptions, DatasetImportProgress, DatasetImportStage,
    DatasetUpdateInput, DatasetUpdateOptions, DemImportOptions, ReorderPolicy,
    audit_geopackage_dataset, import_dataset, load_gpkg_mapping, update_dataset,
};
use netweevil_persist::{WorkspacePaths, read_dataset_manifests, read_topology_bundle};

#[derive(Subcommand, Debug)]
pub(crate) enum DatasetCommand {
    /// Import a road network from OSM, Overture or GeoPackage sources.
    Import(DatasetImportArgs),
    /// Apply OSM change files or a newer extract to an imported OSM dataset.
    ///
    /// The update reuses the CCH elimination order of the dataset.
    Update(DatasetUpdateArgs),
    /// Compare a GeoPackage dataset with its source files.
    Audit(DatasetAuditArgs),
    /// List the imported datasets.
    List,
}

#[derive(Args, Debug)]
pub(crate) struct DatasetImportArgs {
    /// OSM `.osm.pbf` extract, an Overture transportation GeoParquet file,
    /// or a directory of Overture parquet files.
    #[arg(required = true, num_args = 1..)]
    sources: Vec<PathBuf>,
    /// Id of the new dataset.
    #[arg(long)]
    name: String,
    /// Source format; detected from the path when omitted.
    #[arg(long, value_enum)]
    format: Option<SourceFormatArg>,
    /// GeoPackage field/semantic mapping (YAML or JSON).
    #[arg(long)]
    mapping: Option<PathBuf>,
    /// Country (admin_level 2) and state (admin_level 4) boundaries as
    /// GeoJSON or OSM PBF, used instead of the boundary relations in an OSM
    /// extract (for extracts that cut them).
    #[arg(long)]
    admin_boundaries: Option<PathBuf>,
    /// DEM (ESRI ASCII grid `.asc` or GeoTIFF) sampled for node heights and
    /// edge grades of OSM and Overture sources.
    #[arg(long)]
    dem: Option<PathBuf>,
    /// DEM CRS as `EPSG:<code>` or a `+proj=` definition; GeoTIFFs normally
    /// carry their own, ASCII grids default to longitude/latitude.
    #[arg(long, requires = "dem")]
    dem_crs: Option<String>,
    /// Vertical datum label recorded with DEM heights (for example `HKPD`).
    #[arg(long, requires = "dem")]
    dem_vertical_datum: Option<String>,
}

#[derive(Args, Debug)]
#[command(group(
    clap::ArgGroup::new("input").required(true).args(["diff", "source"])
))]
pub(crate) struct DatasetUpdateArgs {
    /// Id of an imported OSM dataset.
    #[arg(long)]
    dataset: String,
    /// osmChange file (`.osc` or `.osc.gz`); repeat to apply several in
    /// order.
    #[arg(long, num_args = 1..)]
    diff: Vec<PathBuf>,
    /// A newer `.osm.pbf` extract of the same area, used instead of change
    /// files.
    #[arg(long)]
    source: Option<PathBuf>,
    /// `auto` patches the previous CCH order and runs a full nested
    /// dissection when the shortcut fill grows past `--max-fill-growth`;
    /// `always` reorders from scratch; `never` always patches.
    #[arg(long, value_enum, default_value_t = ReorderArg::Auto)]
    reorder: ReorderArg,
    /// Shortcuts per edge state relative to the last full ordering above
    /// which `--reorder auto` reorders.
    #[arg(long, default_value_t = DEFAULT_MAX_FILL_GROWTH)]
    max_fill_growth: f64,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ReorderArg {
    Auto,
    Always,
    Never,
}

#[derive(Args, Debug)]
pub(crate) struct DatasetAuditArgs {
    /// Id of an imported GeoPackage dataset.
    #[arg(long)]
    dataset: String,
    /// Original GeoPackage source(s) to compare against the retained bundle.
    #[arg(long, required = true, num_args = 1..)]
    against: Vec<PathBuf>,
    /// GeoPackage field/semantic mapping (YAML or JSON) used at import.
    #[arg(long)]
    mapping: PathBuf,
    /// Emit the full machine-readable audit report.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum SourceFormatArg {
    OsmPbf,
    Overture,
    Gpkg,
}

impl From<SourceFormatArg> for SourceFormat {
    fn from(value: SourceFormatArg) -> Self {
        match value {
            SourceFormatArg::OsmPbf => Self::OsmPbf,
            SourceFormatArg::Overture => Self::OvertureParquet,
            SourceFormatArg::Gpkg => Self::GeoPackage,
        }
    }
}

pub(crate) fn dataset_import(paths: &WorkspacePaths, args: DatasetImportArgs) -> Result<()> {
    let mut progress_line_len = 0_usize;
    let source_label = args
        .sources
        .iter()
        .map(|source| source.display().to_string())
        .collect::<Vec<_>>()
        .join(";");
    let manifest = import_dataset(
        paths,
        &args.sources,
        DatasetImportOptions {
            name: args.name,
            source: source_label,
            format: args.format.map(SourceFormat::from),
            mapping: args.mapping,
            admin_boundaries: args.admin_boundaries,
            dem: args.dem.map(|path| DemImportOptions {
                path,
                crs: args.dem_crs,
                vertical_datum: args.dem_vertical_datum,
            }),
        },
        |event| render_import_progress(&event, &mut progress_line_len),
    )?;
    let mut summary = format!(
        "imported dataset '{}' with {} nodes and {} directed edges",
        manifest.dataset_id.0,
        manifest
            .topology_meta
            .as_ref()
            .map(|meta| meta.node_count)
            .unwrap_or_default(),
        manifest
            .topology_meta
            .as_ref()
            .map(|meta| meta.edge_count)
            .unwrap_or_default(),
    );
    if let Some(stats) = manifest.acceleration_stats.as_ref() {
        summary.push_str(&format!(
            ", acceleration arcs {} ({} base, {} shortcuts)",
            stats.total_arc_count, stats.base_arc_count, stats.shortcut_arc_count
        ));
    }
    println!("{summary}");
    Ok(())
}

pub(crate) fn dataset_update(paths: &WorkspacePaths, args: DatasetUpdateArgs) -> Result<()> {
    let mut progress_line_len = 0_usize;
    let input = match args.source {
        Some(source) => DatasetUpdateInput::Source(source),
        None => DatasetUpdateInput::OsmChanges(args.diff),
    };
    let reorder = match args.reorder {
        ReorderArg::Auto => ReorderPolicy::Auto {
            max_fill_growth: args.max_fill_growth,
        },
        ReorderArg::Always => ReorderPolicy::Always,
        ReorderArg::Never => ReorderPolicy::Never,
    };
    let report = update_dataset(
        paths,
        DatasetUpdateOptions {
            dataset_id: args.dataset,
            input,
            reorder,
        },
        |event| render_import_progress(&event, &mut progress_line_len),
    )?;
    let manifest = &report.manifest;
    let record = manifest
        .last_update
        .as_ref()
        .expect("an update records itself in the manifest");
    if let Some(changed) = report.changed_objects {
        println!(
            "applied changes to {} nodes, {} ways and {} relations",
            changed.nodes, changed.ways, changed.relations
        );
    }
    println!(
        "updated dataset '{}' to {} nodes and {} directed edges in {:.2} s",
        manifest.dataset_id.0,
        manifest
            .topology_meta
            .as_ref()
            .map(|meta| meta.node_count)
            .unwrap_or_default(),
        manifest
            .topology_meta
            .as_ref()
            .map(|meta| meta.edge_count)
            .unwrap_or_default(),
        record.elapsed_s
    );
    println!(
        "CCH order: {} edge states reused, {} inserted; patched order {} shortcuts (fill growth {:.4}){}",
        record.reused_edge_states,
        record.inserted_edge_states,
        report.patched_shortcut_arc_count,
        report.patched_fill_growth,
        if record.reordered {
            format!(
                "; reordered by nested dissection: {} shortcuts",
                manifest
                    .acceleration_stats
                    .as_ref()
                    .map(|stats| stats.shortcut_arc_count)
                    .unwrap_or_default()
            )
        } else {
            String::new()
        }
    );
    if let Some(ordering) = manifest.acceleration_ordering.as_ref() {
        println!(
            "updates since the last full ordering: {}",
            ordering.patched_updates
        );
    }
    let stages = report
        .stage_seconds
        .iter()
        .map(|(stage, seconds)| format!("{stage} {seconds:.2} s"))
        .collect::<Vec<_>>()
        .join(", ");
    println!("stages: {stages}");
    println!(
        "compiled profiles for the previous build are stale: rerun `netweevil profile compile`, or POST /v1/workspace/reload to a running API"
    );
    Ok(())
}

pub(crate) fn dataset_audit(paths: &WorkspacePaths, args: DatasetAuditArgs) -> Result<()> {
    let manifest = read_dataset_manifests(paths)?
        .into_iter()
        .find(|manifest| manifest.dataset_id.0 == args.dataset)
        .ok_or_else(|| anyhow::anyhow!("dataset '{}' not found", args.dataset))?;
    if manifest.source_format != SourceFormat::GeoPackage {
        anyhow::bail!("dataset audit --against currently supports GeoPackage datasets only");
    }
    let bundle_ref = manifest
        .topology_bundle
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("dataset '{}' has no topology bundle", args.dataset))?;
    let topology = read_topology_bundle(&bundle_ref.path)?;
    let mapping = load_gpkg_mapping(&args.mapping)?;
    let report = audit_geopackage_dataset(&topology, &args.against, &mapping)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "dataset audit {}: {} retained features, {} source fields, {} segments; {} missing features, {} unexpected features, {} missing fields, {} unexpected fields, {} schema drifts, {} value drifts, {} geometry-order drifts, {} edge/attribute-link drifts, {} missing segments, {} unexpected segments",
            if report.passed { "passed" } else { "failed" },
            report.retained_feature_count,
            report.retained_field_count,
            report.compared_segment_count,
            report.missing_features.len(),
            report.unexpected_features.len(),
            report.missing_fields.len(),
            report.unexpected_fields.len(),
            report.attribute_schema_drifts.len(),
            report.attribute_drifts.len(),
            report.geometry_order_drifts.len(),
            report.edge_attribute_link_drift_count,
            report.missing_segments,
            report.unexpected_segments,
        );
    }
    if !report.passed {
        anyhow::bail!("dataset audit detected source retention or geometry drift");
    }
    Ok(())
}

fn render_import_progress(event: &DatasetImportProgress, last_line_len: &mut usize) {
    let mut line = format!("[{}] {}", event.stage.label(), event.message);
    if matches!(event.stage, DatasetImportStage::Complete) {
        line.push_str(" [done]");
    }
    let padding = last_line_len.saturating_sub(line.len());
    eprint!("\r{line}{:padding$}", "");
    if matches!(event.stage, DatasetImportStage::Complete) {
        eprintln!();
        *last_line_len = 0;
    } else {
        *last_line_len = line.len();
    }
}

pub(crate) fn dataset_list(paths: &WorkspacePaths) -> Result<()> {
    for manifest in read_dataset_manifests(paths)? {
        println!(
            "{}\t{}\t{:?}\t{}",
            manifest.dataset_id.0, manifest.source_path, manifest.build_stage, manifest.imported_at
        );
    }
    Ok(())
}
