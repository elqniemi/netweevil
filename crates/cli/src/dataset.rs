use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use netweevil_core::SourceFormat;
use netweevil_ingest::{
    DatasetImportOptions, DatasetImportProgress, DatasetImportStage, audit_geopackage_dataset,
    import_dataset_sources_with_progress, load_gpkg_mapping,
};
use netweevil_persist::{WorkspacePaths, read_dataset_manifests, read_topology_bundle};

#[derive(Subcommand, Debug)]
pub(crate) enum DatasetCommand {
    Import(DatasetImportArgs),
    Audit(DatasetAuditArgs),
    List,
}

#[derive(Args, Debug)]
pub(crate) struct DatasetImportArgs {
    /// OSM `.osm.pbf` extract, an Overture transportation GeoParquet file,
    /// or a directory of Overture parquet files.
    #[arg(required = true, num_args = 1..)]
    sources: Vec<PathBuf>,
    #[arg(long)]
    name: String,
    /// Source format; detected from the path when omitted.
    #[arg(long, value_enum)]
    format: Option<SourceFormatArg>,
    /// GeoPackage field/semantic mapping (YAML or JSON).
    #[arg(long)]
    mapping: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct DatasetAuditArgs {
    #[arg(long)]
    dataset: String,
    /// Original GeoPackage source(s) to compare against the retained bundle.
    #[arg(long, required = true, num_args = 1..)]
    against: Vec<PathBuf>,
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
    let manifest = import_dataset_sources_with_progress(
        paths,
        &args.sources,
        DatasetImportOptions {
            name: args.name,
            source: source_label,
            format: args.format.map(SourceFormat::from),
            mapping: args.mapping,
        },
        |event| render_import_progress(&event, &mut progress_line_len),
    )?;
    let mut summary = format!(
        "imported dataset '{}' with {} nodes and {} directed edges (sha256 {})",
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
        manifest.source_sha256
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
