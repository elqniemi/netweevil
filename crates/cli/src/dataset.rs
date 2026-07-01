use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use netweevil_core::SourceFormat;
use netweevil_ingest::{
    DatasetImportOptions, DatasetImportProgress, DatasetImportStage, import_dataset_with_progress,
};
use netweevil_persist::{WorkspacePaths, read_dataset_manifests};

#[derive(Subcommand, Debug)]
pub(crate) enum DatasetCommand {
    Import(DatasetImportArgs),
    List,
}

#[derive(Args, Debug)]
pub(crate) struct DatasetImportArgs {
    /// OSM `.osm.pbf` extract, an Overture transportation GeoParquet file,
    /// or a directory of Overture parquet files.
    source: PathBuf,
    #[arg(long)]
    name: String,
    /// Source format; detected from the path when omitted.
    #[arg(long, value_enum)]
    format: Option<SourceFormatArg>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum SourceFormatArg {
    OsmPbf,
    Overture,
}

impl From<SourceFormatArg> for SourceFormat {
    fn from(value: SourceFormatArg) -> Self {
        match value {
            SourceFormatArg::OsmPbf => Self::OsmPbf,
            SourceFormatArg::Overture => Self::OvertureParquet,
        }
    }
}

pub(crate) fn dataset_import(paths: &WorkspacePaths, args: DatasetImportArgs) -> Result<()> {
    let mut progress_line_len = 0_usize;
    let manifest = import_dataset_with_progress(
        paths,
        &args.source,
        DatasetImportOptions {
            name: args.name,
            source: args.source.display().to_string(),
            format: args.format.map(SourceFormat::from),
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
