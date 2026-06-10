use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use netweevil_api::{ApiServeOptions, serve as serve_api};
use netweevil_persist::WorkspacePaths;

#[derive(Subcommand, Debug)]
pub(crate) enum ApiCommand {
    Serve(ApiServeArgs),
}

#[derive(Args, Debug)]
pub(crate) struct ApiServeArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    default_profile: PathBuf,
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: SocketAddr,
    #[arg(long = "profile")]
    profiles: Vec<PathBuf>,
    #[arg(long = "transit-feed")]
    transit_feeds: Vec<String>,
}

pub(crate) fn api_serve(paths: WorkspacePaths, args: ApiServeArgs) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("creating API runtime")?;
    runtime.block_on(serve_api(
        paths,
        ApiServeOptions {
            bind: args.bind,
            dataset_id: args.dataset,
            default_profile: args.default_profile,
            profiles: args.profiles,
            transit_feeds: args.transit_feeds,
        },
    ))
}
