use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use netweevil_persist::{WorkspacePaths, write_json};
use netweevil_transit::{
    OPENOV_GTFS_URL, TransitFeedManifest, TransitImportOptions, import_gtfs,
    transit_import_summary, write_transit_bundle,
};

#[derive(Subcommand, Debug)]
pub(crate) enum TransitCommand {
    Import(TransitImportArgs),
    List,
}

#[derive(Args, Debug)]
pub(crate) struct TransitImportArgs {
    source: PathBuf,
    #[arg(long)]
    name: String,
    #[arg(long)]
    service_start: String,
    #[arg(long, default_value_t = 7)]
    service_days: u32,
}

pub(crate) fn transit_import(paths: &WorkspacePaths, args: TransitImportArgs) -> Result<()> {
    let bundle = import_gtfs(
        &args.source,
        TransitImportOptions {
            name: args.name.clone(),
            source_label: args.source.display().to_string(),
            service_start_date: args.service_start.clone(),
            service_days: args.service_days,
        },
    )
    .with_context(|| format!("importing GTFS feed {}", args.source.display()))?;
    let summary = transit_import_summary(&bundle);
    let bundle_path = paths.transit_bundles_dir.join(format!(
        "transit-{}-{}.bin",
        args.name,
        &summary.source_sha256[..12]
    ));
    write_transit_bundle(&bundle_path, &bundle)?;
    let manifest = TransitFeedManifest {
        feed_id: args.name.clone(),
        label: format!("GTFS transit feed {}", args.name),
        source_path: args.source.display().to_string(),
        source_sha256: summary.source_sha256.clone(),
        imported_at: netweevil_report::now_rfc3339()?,
        service_start_date: args.service_start,
        service_days: args.service_days,
        stop_count: summary.stop_count as u64,
        route_count: summary.route_count as u64,
        trip_count: summary.trip_count as u64,
        connection_count: summary.connection_count as u64,
        bundle_path: bundle_path.display().to_string(),
    };
    let manifest_path = paths
        .transit_feeds_dir
        .join(format!("{}.json", manifest.feed_id));
    write_json(&manifest_path, &manifest)?;
    println!(
        "imported transit feed '{}' with {} stops, {} routes, {} trips, {} scheduled connections",
        manifest.feed_id,
        manifest.stop_count,
        manifest.route_count,
        manifest.trip_count,
        manifest.connection_count
    );
    println!("transit bundle written to {}", bundle_path.display());
    println!(
        "transit feed manifest written to {}",
        manifest_path.display()
    );
    println!("openov source URL: {OPENOV_GTFS_URL}");
    Ok(())
}

pub(crate) fn transit_list(paths: &WorkspacePaths) -> Result<()> {
    for manifest in read_transit_manifests(paths)? {
        println!(
            "{}\t{}..+{}d\t{} stops\t{} connections\t{}",
            manifest.feed_id,
            manifest.service_start_date,
            manifest.service_days,
            manifest.stop_count,
            manifest.connection_count,
            manifest.source_path
        );
    }
    Ok(())
}

pub(crate) fn read_transit_manifests(paths: &WorkspacePaths) -> Result<Vec<TransitFeedManifest>> {
    let mut manifests = Vec::new();
    if !paths.transit_feeds_dir.exists() {
        return Ok(manifests);
    }
    for entry in fs::read_dir(&paths.transit_feeds_dir)
        .with_context(|| format!("reading {}", paths.transit_feeds_dir.display()))?
    {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            manifests.push(netweevil_persist::read_json(entry.path())?);
        }
    }
    manifests.sort_by(|left, right| left.feed_id.cmp(&right.feed_id));
    Ok(manifests)
}

pub(crate) fn read_transit_manifest(
    paths: &WorkspacePaths,
    feed_id: &str,
) -> Result<TransitFeedManifest> {
    let path = paths.transit_feeds_dir.join(format!("{feed_id}.json"));
    netweevil_persist::read_json(&path)
        .with_context(|| format!("reading transit feed manifest {}", path.display()))
}
