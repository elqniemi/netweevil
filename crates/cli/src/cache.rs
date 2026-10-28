use anyhow::Result;
use clap::Subcommand;
use netweevil_persist::{
    WorkspacePaths, prune_bundle_cache, read_compiled_profile_manifests, read_dataset_manifests,
};

use crate::transit::read_transit_manifests;

#[derive(Subcommand, Debug)]
pub(crate) enum CacheCommand {
    /// List datasets, compiled profiles and transit feeds in the workspace.
    List,
    /// Remove stale compiled profiles and unreferenced bundle files.
    ///
    /// A compiled profile is stale when its dataset was removed or
    /// re-imported, its bundle is missing or in an old format, or a newer
    /// compile of the same profile id exists. Stale profiles recompile on
    /// demand. Transit bundles are not touched.
    Prune {
        /// List what would be removed without deleting anything.
        #[arg(long)]
        dry_run: bool,
    },
}

pub(crate) fn cache_list(paths: &WorkspacePaths) -> Result<()> {
    println!("datasets:");
    for dataset in read_dataset_manifests(paths)? {
        println!(
            "  {}\t{}\t{:?}",
            dataset.dataset_id.0, dataset.source_path, dataset.build_stage
        );
    }
    println!("compiled profiles:");
    for profile in read_compiled_profile_manifests(paths)? {
        println!(
            "  {}\t{}\t{}",
            profile.compile_id, profile.profile_id, profile.bundle.bundle_id.0
        );
    }
    println!("transit feeds:");
    for transit in read_transit_manifests(paths)? {
        println!(
            "  {}\t{} stops\t{} connections\t{}",
            transit.feed_id, transit.stop_count, transit.connection_count, transit.bundle_path
        );
    }
    Ok(())
}

pub(crate) fn cache_prune(paths: &WorkspacePaths, dry_run: bool) -> Result<()> {
    let report = prune_bundle_cache(paths, dry_run)?;
    let verb = if dry_run { "would remove" } else { "removed" };
    for (path, reason) in &report.compiled_manifests {
        println!("{verb} compiled profile {} ({reason})", path.display());
    }
    for (path, bytes) in &report.bundle_files {
        println!("{verb} bundle {} ({bytes} bytes)", path.display());
    }
    println!(
        "{verb} {} compiled profiles and {} bundle files, {} bytes",
        report.compiled_manifests.len(),
        report.bundle_files.len(),
        report.reclaimed_bytes()
    );
    Ok(())
}
