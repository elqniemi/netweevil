//! `netweevil` — the command-line interface for OSM routing research:
//! dataset import, profile compilation, analyses (route/OD/matrix/service
//! areas/accessibility), GTFS transit, traffic simulation, reports, and the
//! local HTTP API server.

mod analyze;
mod api;
mod cache;
mod dataset;
mod experiment;
mod profile;
mod report;
mod simulate;
mod transit;

use std::env;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use netweevil_persist::WorkspacePaths;
use netweevil_report::SoftwareInfo;
use tracing_subscriber::EnvFilter;

use crate::analyze::{
    AnalyzeCommand, analyze_accessibility, analyze_matrix, analyze_od, analyze_route,
    analyze_route_batch, analyze_service_area, analyze_transit_batch, analyze_transit_route,
};
use crate::api::{ApiCommand, api_serve};
use crate::cache::{CacheCommand, cache_list};
use crate::dataset::{DatasetCommand, dataset_import, dataset_list};
use crate::experiment::{ExperimentCommand, experiment_run};
use crate::profile::{ProfileCommand, profile_compile, profile_validate};
use crate::report::{ReportCommand, report_render};
use crate::simulate::{SimulateCommand, simulate_run, simulate_validate};
use crate::transit::{TransitCommand, transit_import, transit_list};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();
    let cwd = env::current_dir().context("resolving current directory")?;
    let paths = WorkspacePaths::discover(cwd)?;

    match cli.command {
        Command::Dataset { command: dataset } => match dataset {
            DatasetCommand::Import(args) => dataset_import(&paths, args),
            DatasetCommand::List => dataset_list(&paths),
        },
        Command::Transit { command: transit } => match transit {
            TransitCommand::Import(args) => transit_import(&paths, args),
            TransitCommand::List => transit_list(&paths),
        },
        Command::Profile { command: profile } => match profile {
            ProfileCommand::Validate { profile } => profile_validate(&profile),
            ProfileCommand::Compile { dataset, profile } => {
                profile_compile(&paths, &dataset, &profile)
            }
        },
        Command::Analyze { command: analyze } => match analyze {
            AnalyzeCommand::Route(args) => analyze_route(&paths, args),
            AnalyzeCommand::RouteBatch(args) => analyze_route_batch(&paths, args),
            AnalyzeCommand::Od(args) => analyze_od(&paths, args),
            AnalyzeCommand::Matrix(args) => analyze_matrix(&paths, args),
            AnalyzeCommand::Accessibility(args) => analyze_accessibility(&paths, args),
            AnalyzeCommand::ServiceArea(args) => analyze_service_area(&paths, args),
            AnalyzeCommand::TransitRoute(args) => analyze_transit_route(&paths, args),
            AnalyzeCommand::TransitBatch(args) => analyze_transit_batch(&paths, args),
        },
        Command::Experiment {
            command: experiment,
        } => match experiment {
            ExperimentCommand::Run { study } => experiment_run(&paths, &study),
        },
        Command::Simulate { command: simulate } => match simulate {
            SimulateCommand::Validate { scenario } => simulate_validate(&scenario),
            SimulateCommand::Run(args) => simulate_run(&paths, args),
        },
        Command::Report { command: report } => match report {
            ReportCommand::Render { manifest, out } => report_render(&manifest, out.as_deref()),
        },
        Command::Cache { command: cache } => match cache {
            CacheCommand::List => cache_list(&paths),
        },
        Command::Api { command: api } => match api {
            ApiCommand::Serve(args) => api_serve(paths, args),
        },
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "netweevil",
    version,
    about = "Rust-first OSM network analysis tool"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Dataset {
        #[command(subcommand)]
        command: DatasetCommand,
    },
    Transit {
        #[command(subcommand)]
        command: TransitCommand,
    },
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    Analyze {
        #[command(subcommand)]
        command: AnalyzeCommand,
    },
    Experiment {
        #[command(subcommand)]
        command: ExperimentCommand,
    },
    Simulate {
        #[command(subcommand)]
        command: SimulateCommand,
    },
    Report {
        #[command(subcommand)]
        command: ReportCommand,
    },
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    Api {
        #[command(subcommand)]
        command: ApiCommand,
    },
}

pub(crate) fn software_info() -> SoftwareInfo {
    SoftwareInfo {
        executable: "netweevil".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_commit: option_env!("NETWEEVIL_GIT_COMMIT").map(ToString::to_string),
    }
}
