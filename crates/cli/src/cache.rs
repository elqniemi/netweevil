use anyhow::Result;
use clap::Subcommand;
use netweevil_persist::{WorkspacePaths, read_compiled_profile_manifests, read_dataset_manifests};

use crate::transit::read_transit_manifests;

#[derive(Subcommand, Debug)]
pub(crate) enum CacheCommand {
    List,
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
