use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use netweevil_core::{AccelerationBuildProfile, AccelerationBuildSettings};
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
    source: PathBuf,
    #[arg(long)]
    name: String,
    #[arg(long, value_enum, default_value_t = AccelerationProfileArg::Compact)]
    acceleration_profile: AccelerationProfileArg,
    #[arg(long)]
    max_shortcut_path_len: Option<u32>,
    #[arg(long)]
    max_shortcuts_per_contracted_edge: Option<u32>,
    #[arg(long)]
    max_shortcut_budget_per_edge: Option<u32>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AccelerationProfileArg {
    Compact,
    Balanced,
    Aggressive,
}

impl From<AccelerationProfileArg> for AccelerationBuildProfile {
    fn from(value: AccelerationProfileArg) -> Self {
        match value {
            AccelerationProfileArg::Compact => Self::Compact,
            AccelerationProfileArg::Balanced => Self::Balanced,
            AccelerationProfileArg::Aggressive => Self::Aggressive,
        }
    }
}

pub(crate) fn dataset_import(paths: &WorkspacePaths, args: DatasetImportArgs) -> Result<()> {
    let mut progress_line_len = 0_usize;
    let mut acceleration_settings =
        AccelerationBuildSettings::for_profile(args.acceleration_profile.into());
    if let Some(value) = args.max_shortcut_path_len {
        acceleration_settings.max_shortcut_path_len = value;
    }
    if let Some(value) = args.max_shortcuts_per_contracted_edge {
        acceleration_settings.max_shortcuts_per_contracted_edge = value;
    }
    if let Some(value) = args.max_shortcut_budget_per_edge {
        acceleration_settings.max_shortcut_budget_per_edge = value;
    }
    let manifest = import_dataset_with_progress(
        paths,
        &args.source,
        DatasetImportOptions {
            name: args.name,
            source: args.source.display().to_string(),
            acceleration_settings,
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
