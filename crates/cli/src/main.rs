//! `netweevil` — the command-line interface for OSM routing research:
//! dataset import, profile compilation, analyses (route/OD/matrix/service
//! areas/accessibility), GTFS transit, traffic simulation, reports,
//! performance benchmarks, and the local HTTP API server.

mod analyze;
mod api;
mod bench;
mod cache;
mod dataset;
mod experiment;
mod profile;
mod report;
mod simulate;
mod traffic;
mod transit;

use std::env;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use netweevil_persist::WorkspacePaths;
use netweevil_persist::manifest::{AlgorithmInfo, SoftwareInfo};
use tracing_subscriber::EnvFilter;

use crate::analyze::{
    AnalyzeCommand, analyze_accessibility, analyze_betweenness, analyze_directions, analyze_matrix,
    analyze_od, analyze_route, analyze_route_batch, analyze_scenario_batch, analyze_service_area,
    analyze_service_area_sequence, analyze_transit_batch, analyze_transit_route, analyze_trip,
};
use crate::api::{ApiCommand, BootstrapArgs, api_serve, bootstrap};
use crate::bench::{
    BenchCommand, bench_compare, bench_corpus, bench_http, bench_run, bench_traffic, bench_trip,
};
use crate::cache::{CacheCommand, cache_list, cache_prune};
use crate::dataset::{DatasetCommand, dataset_audit, dataset_import, dataset_list, dataset_update};
use crate::experiment::{ExperimentCommand, experiment_run};
use crate::profile::{ProfileCommand, profile_compile, profile_validate};
use crate::report::{ReportCommand, report_render};
use crate::simulate::{SimulateCommand, simulate_run, simulate_validate};
use crate::traffic::{TrafficCommand, traffic_command};
use crate::transit::{
    TransitBindingsCommand, TransitCommand, TransitTransfersCommand, transit_apply_bindings,
    transit_build_transfers, transit_import, transit_list,
};

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
            DatasetCommand::Audit(args) => dataset_audit(&paths, args),
            DatasetCommand::Update(args) => dataset_update(&paths, args),
            DatasetCommand::List => dataset_list(&paths),
        },
        Command::Transit { command: transit } => match transit {
            TransitCommand::Import(args) => transit_import(&paths, args),
            TransitCommand::Bindings { command } => match command {
                TransitBindingsCommand::Apply(args) => transit_apply_bindings(&paths, args),
            },
            TransitCommand::Transfers { command } => match command {
                TransitTransfersCommand::Build(args) => transit_build_transfers(&paths, args),
            },
            TransitCommand::List => transit_list(&paths),
        },
        Command::Traffic { command } => traffic_command(&paths, command),
        Command::Profile { command: profile } => match profile {
            ProfileCommand::Validate { profile } => profile_validate(&profile),
            ProfileCommand::Compile { dataset, profile } => {
                profile_compile(&paths, &dataset, &profile)
            }
        },
        Command::Analyze { command: analyze } => match analyze {
            AnalyzeCommand::Route(args) => analyze_route(&paths, args),
            AnalyzeCommand::Directions(args) => analyze_directions(&paths, args),
            AnalyzeCommand::RouteBatch(args) => analyze_route_batch(&paths, args),
            AnalyzeCommand::Od(args) => analyze_od(&paths, args),
            AnalyzeCommand::Matrix(args) => analyze_matrix(&paths, args),
            AnalyzeCommand::Accessibility(args) => analyze_accessibility(&paths, args),
            AnalyzeCommand::ServiceArea(args) => analyze_service_area(&paths, args),
            AnalyzeCommand::ServiceAreaSequence(args) => {
                analyze_service_area_sequence(&paths, args)
            }
            AnalyzeCommand::Betweenness(args) => analyze_betweenness(&paths, args),
            AnalyzeCommand::ScenarioBatch(args) => analyze_scenario_batch(&paths, args),
            AnalyzeCommand::TransitRoute(args) => analyze_transit_route(&paths, args),
            AnalyzeCommand::TransitBatch(args) => analyze_transit_batch(&paths, args),
            AnalyzeCommand::Trip(args) => analyze_trip(&paths, args),
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
            CacheCommand::Prune { dry_run } => cache_prune(&paths, dry_run),
        },
        Command::Bench { command: bench } => match bench {
            BenchCommand::Corpus(args) => bench_corpus(&paths, args),
            BenchCommand::Run(args) => bench_run(&paths, args),
            BenchCommand::Http(args) => bench_http(args),
            BenchCommand::Compare(args) => bench_compare(args),
            BenchCommand::Trip(args) => bench_trip(&paths, args),
            BenchCommand::Traffic(args) => bench_traffic(&paths, args),
        },
        Command::Api { command: api } => match api {
            ApiCommand::Serve(args) => api_serve(paths, args),
        },
        Command::Bootstrap(args) => bootstrap(paths, args),
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "netweevil",
    version,
    about = "Analyze OSM road networks, GTFS transit and traffic on your computer"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Import, update, audit and list road network datasets.
    Dataset {
        #[command(subcommand)]
        command: DatasetCommand,
    },
    /// Import GTFS feeds and prepare them for transit routing.
    Transit {
        #[command(subcommand)]
        command: TransitCommand,
    },
    /// Import historical speeds and route with traffic.
    Traffic {
        #[command(subcommand)]
        command: TrafficCommand,
    },
    /// Validate routing profiles and compile them for a dataset.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// Run routing analyses and save each result as a run.
    Analyze {
        #[command(subcommand)]
        command: AnalyzeCommand,
    },
    /// Run reproducible study files.
    Experiment {
        #[command(subcommand)]
        command: ExperimentCommand,
    },
    /// Validate and run agent-based traffic simulations.
    Simulate {
        #[command(subcommand)]
        command: SimulateCommand,
    },
    /// Render reports from run manifests.
    Report {
        #[command(subcommand)]
        command: ReportCommand,
    },
    /// List and prune compiled profiles and bundle files.
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    /// Measure routing speed and quality.
    Bench {
        #[command(subcommand)]
        command: BenchCommand,
    },
    /// Run the HTTP API server.
    Api {
        #[command(subcommand)]
        command: ApiCommand,
    },
    /// Start the API and web console in setup mode for a new workspace.
    Bootstrap(BootstrapArgs),
}

pub(crate) fn software_info() -> SoftwareInfo {
    SoftwareInfo {
        executable: "netweevil".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_commit: option_env!("NETWEEVIL_GIT_COMMIT").map(ToString::to_string),
    }
}

/// Every CLI run executes over the same directed-edge graph model, so callers
/// only vary the engine and acceleration labels.
pub(crate) fn algorithm_info(
    engine: impl Into<String>,
    acceleration: impl Into<String>,
) -> AlgorithmInfo {
    AlgorithmInfo {
        engine: engine.into(),
        graph_model: "directed_edge_graph".to_string(),
        acceleration: acceleration.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::CommandFactory;

    use super::Cli;

    /// Every subcommand needs its own `about` text. A text shared by siblings
    /// means the doc comment of a flattened argument struct leaked.
    #[test]
    fn every_subcommand_has_its_own_about_text() {
        fn walk(command: &clap::Command, path: &str, problems: &mut Vec<String>) {
            let mut seen = BTreeSet::new();
            for sub in command.get_subcommands() {
                let sub_path = format!("{path} {}", sub.get_name());
                let about = sub.get_about().map(ToString::to_string).unwrap_or_default();
                if about.trim().is_empty() {
                    problems.push(format!("{sub_path}: no about text"));
                } else if !seen.insert(about.clone()) {
                    problems.push(format!("{sub_path}: about text repeats a sibling: {about}"));
                }
                walk(sub, &sub_path, problems);
            }
        }
        let command = Cli::command();
        command.clone().debug_assert();
        let mut problems = Vec::new();
        walk(&command, "netweevil", &mut problems);
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }
}
