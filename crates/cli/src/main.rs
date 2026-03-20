use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use netan_api::{ApiServeOptions, serve as serve_api};
use netan_core::{CacheBundleId, CompiledProfileBundle, TopologyBundle};
use netan_gui::launch;
use netan_ingest::{
    DatasetImportOptions, DatasetImportProgress, DatasetImportStage, import_dataset_with_progress,
};
use netan_persist::{
    WorkspacePaths, read_compiled_profile_bundle, read_compiled_profile_manifests,
    read_dataset_manifest, read_dataset_manifests, read_edge_name_bundle, read_run_manifest,
    read_topology_bundle, write_compiled_profile_bundle, write_compiled_profile_manifest,
    write_json, write_run_manifest,
};
use netan_profile::{ProfileDocument, ReturnGeometry, compile_profile_bundle, load_profile};
use netan_query::{
    AnalysisKind, MatrixResult, OdResult, RouteResult, execute_matrix, execute_od, execute_route,
    execute_route_with_edge_names, load_experiment, load_od_pairs, load_point_set,
    load_route_request,
};
use netan_report::{
    BundleRef, CompiledProfileManifest, RunKind, RunStatus, SoftwareInfo, load_run_result_summary,
    new_run_manifest, render_run_html, render_run_markdown, write_matrix_result, write_od_result,
    write_route_result,
};
use serde::Serialize;
use tracing_subscriber::EnvFilter;

#[derive(Debug)]
struct StoredRun {
    result_path: PathBuf,
    manifest_path: PathBuf,
    summary: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct ExperimentRunSummary {
    experiment_id: String,
    label: String,
    dataset: String,
    created_at: String,
    status: String,
    scenario_count: usize,
    succeeded_count: usize,
    failed_count: usize,
    scenarios: Vec<ExperimentScenarioSummary>,
}

#[derive(Debug, Serialize)]
struct ExperimentScenarioSummary {
    scenario_id: String,
    analysis: &'static str,
    profile_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origins_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    destinations_path: Option<String>,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_manifest_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct EngineDescription {
    route_engine: &'static str,
    route_summary: &'static str,
    batch_engine: &'static str,
    batch_summary: &'static str,
    acceleration: &'static str,
}

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
        Command::Profile { command: profile } => match profile {
            ProfileCommand::Validate { profile } => profile_validate(&profile),
            ProfileCommand::Compile { dataset, profile } => {
                profile_compile(&paths, &dataset, &profile)
            }
        },
        Command::Analyze { command: analyze } => match analyze {
            AnalyzeCommand::Route(args) => analyze_route(&paths, args),
            AnalyzeCommand::Od(args) => analyze_od(&paths, args),
            AnalyzeCommand::Matrix(args) => analyze_matrix(&paths, args),
        },
        Command::Experiment {
            command: experiment,
        } => match experiment {
            ExperimentCommand::Run { study } => experiment_run(&paths, &study),
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
        Command::Gui => launch(paths),
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "netan",
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
    Gui,
}

#[derive(Subcommand, Debug)]
enum DatasetCommand {
    Import(DatasetImportArgs),
    List,
}

#[derive(Args, Debug)]
struct DatasetImportArgs {
    source: PathBuf,
    #[arg(long)]
    name: String,
}

#[derive(Subcommand, Debug)]
enum ProfileCommand {
    Validate {
        profile: PathBuf,
    },
    Compile {
        #[arg(long)]
        dataset: String,
        #[arg(long)]
        profile: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
enum AnalyzeCommand {
    Route(RouteArgs),
    Od(OdArgs),
    Matrix(MatrixArgs),
}

#[derive(Subcommand, Debug)]
enum ExperimentCommand {
    Run { study: PathBuf },
}

#[derive(Subcommand, Debug)]
enum ReportCommand {
    Render {
        manifest: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
enum CacheCommand {
    List,
}

#[derive(Subcommand, Debug)]
enum ApiCommand {
    Serve(ApiServeArgs),
}

#[derive(Args, Debug)]
struct ApiServeArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    default_profile: PathBuf,
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: SocketAddr,
    #[arg(long = "profile")]
    profiles: Vec<PathBuf>,
}

#[derive(Args, Debug)]
struct RouteArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct OdArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long)]
    pairs: PathBuf,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct MatrixArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long)]
    origins: PathBuf,
    #[arg(long)]
    destinations: PathBuf,
    #[arg(long)]
    out: Option<PathBuf>,
}

fn dataset_import(paths: &WorkspacePaths, args: DatasetImportArgs) -> Result<()> {
    let mut progress_line_len = 0_usize;
    let manifest = import_dataset_with_progress(
        paths,
        &args.source,
        DatasetImportOptions {
            name: args.name,
            source: args.source.display().to_string(),
        },
        |event| render_import_progress(&event, &mut progress_line_len),
    )?;
    println!(
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

fn dataset_list(paths: &WorkspacePaths) -> Result<()> {
    for manifest in read_dataset_manifests(paths)? {
        println!(
            "{}\t{}\t{:?}\t{}",
            manifest.dataset_id.0, manifest.source_path, manifest.build_stage, manifest.imported_at
        );
    }
    Ok(())
}

fn profile_validate(path: &Path) -> Result<()> {
    let profile = load_profile(path)?;
    profile.validate()?;
    println!(
        "profile '{}' is valid (hash {})",
        profile.profile.id,
        profile.fingerprint()?
    );
    Ok(())
}

fn profile_compile(paths: &WorkspacePaths, dataset: &str, profile_path: &Path) -> Result<()> {
    let profile = load_profile(profile_path)?;
    profile.validate()?;
    let dataset_manifest = read_dataset_manifest(paths, dataset)
        .with_context(|| format!("reading dataset manifest for '{dataset}'"))?;
    let topology_ref = dataset_manifest
        .topology_bundle
        .clone()
        .context("dataset is missing a topology bundle; run `netan dataset import` first")?;
    let topology: TopologyBundle = read_topology_bundle(&topology_ref.path)
        .with_context(|| format!("reading topology bundle {}", topology_ref.path))?;
    let compiled_bundle =
        compile_profile_bundle(&profile, &topology, topology_ref.bundle_id.clone()).with_context(
            || {
                format!(
                    "compiling profile '{}' for dataset '{dataset}'",
                    profile.profile.id
                )
            },
        )?;
    let profile_hash = profile.fingerprint()?;
    let compile_id = format!("{dataset}-{}", &profile_hash[..12]);
    let bundle_path = paths
        .metric_bundles_dir
        .join(format!("metric-{compile_id}.bin"));
    write_compiled_profile_bundle(&bundle_path, &compiled_bundle)?;
    let manifest = CompiledProfileManifest {
        compile_id: compile_id.clone(),
        dataset_id: netan_core::DatasetId::new(dataset.to_string()),
        profile_id: profile.profile.id.clone(),
        profile_hash,
        defaults_pack: profile.profile.defaults_pack.clone(),
        mode: profile.profile.mode,
        created_at: netan_report::now_rfc3339()?,
        topology_bundle_id: Some(topology_ref.bundle_id),
        edge_count: Some(compiled_bundle.edge_metrics.len() as u64),
        bundle: BundleRef {
            bundle_id: CacheBundleId::new(format!("metric-{compile_id}")),
            path: bundle_path.display().to_string(),
        },
    };
    let path = write_compiled_profile_manifest(paths, &manifest)?;
    println!(
        "compiled profile '{}' for dataset '{}' into {} edge metrics",
        manifest.profile_id,
        manifest.dataset_id.0,
        compiled_bundle.edge_metrics.len()
    );
    println!("compiled profile manifest written to {}", path.display());
    Ok(())
}

fn analyze_route(paths: &WorkspacePaths, args: RouteArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let request = load_route_request(&args.request)?;
    let stored = run_route_analysis(
        paths,
        &args.dataset,
        &profile,
        &args.request,
        request,
        args.out,
    )?;
    println!("route result written to {}", stored.result_path.display());
    println!("run manifest written to {}", stored.manifest_path.display());
    Ok(())
}

fn analyze_od(paths: &WorkspacePaths, args: OdArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let mut request = load_od_pairs(&args.pairs)?;
    if args.out.as_deref().is_some_and(output_needs_geometry)
        && matches!(request.returns.geometry, ReturnGeometry::None)
    {
        request.returns.geometry = ReturnGeometry::Full;
    }
    let stored = run_od_analysis(
        paths,
        &args.dataset,
        &profile,
        &args.pairs,
        request,
        args.out,
    )?;
    println!("OD result written to {}", stored.result_path.display());
    println!("run manifest written to {}", stored.manifest_path.display());
    Ok(())
}

fn analyze_matrix(paths: &WorkspacePaths, args: MatrixArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let mut origins = load_point_set(&args.origins)?;
    let mut destinations = load_point_set(&args.destinations)?;
    if args.out.as_deref().is_some_and(output_needs_geometry) {
        if matches!(origins.returns.geometry, ReturnGeometry::None) {
            origins.returns.geometry = ReturnGeometry::Full;
        }
        if matches!(destinations.returns.geometry, ReturnGeometry::None) {
            destinations.returns.geometry = ReturnGeometry::Full;
        }
    }
    let stored = run_matrix_analysis(
        paths,
        &args.dataset,
        &profile,
        &args.origins,
        &args.destinations,
        origins,
        destinations,
        args.out,
    )?;
    println!("matrix result written to {}", stored.result_path.display());
    println!("run manifest written to {}", stored.manifest_path.display());
    Ok(())
}

fn run_route_analysis(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    request_path: &Path,
    mut request: netan_query::RouteRequest,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    if out.as_deref().is_some_and(output_needs_geometry)
        && matches!(request.returns.geometry, ReturnGeometry::None)
    {
        request.returns.geometry = ReturnGeometry::Full;
    }
    let (topology, compiled_manifest, compiled_bundle) =
        load_route_execution_inputs(paths, dataset_id, profile)?;
    let engine = engine_description(&topology);
    let edge_names = if request.returns.segment_rows {
        load_edge_names(paths, dataset_id)?
    } else {
        None
    };
    let result = if let Some(edge_names) = edge_names.as_ref() {
        execute_route_with_edge_names(&topology, &compiled_bundle, &request, edge_names)
    } else {
        execute_route(&topology, &compiled_bundle, &request)
    }
    .with_context(|| format!("executing route '{}'", request.route_id))?;
    store_route_run(
        paths,
        dataset_id,
        profile,
        request_path,
        &request,
        &result,
        &compiled_manifest,
        engine,
        out,
    )
}

fn run_od_analysis(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    pairs_path: &Path,
    mut request: netan_query::OdPairsDocument,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    if out.as_deref().is_some_and(output_needs_geometry)
        && matches!(request.returns.geometry, ReturnGeometry::None)
    {
        request.returns.geometry = ReturnGeometry::Full;
    }
    let (topology, compiled_manifest, compiled_bundle) =
        load_route_execution_inputs(paths, dataset_id, profile)?;
    let engine = engine_description(&topology);
    let result = execute_od(&topology, &compiled_bundle, &request)
        .with_context(|| format!("executing OD pairs from '{}'", pairs_path.display()))?;
    store_od_run(
        paths,
        dataset_id,
        profile,
        pairs_path,
        &request,
        &result,
        &compiled_manifest,
        engine,
        out,
    )
}

fn run_matrix_analysis(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    origins_path: &Path,
    destinations_path: &Path,
    mut origins: netan_query::PointSetDocument,
    mut destinations: netan_query::PointSetDocument,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    if out.as_deref().is_some_and(output_needs_geometry) {
        if matches!(origins.returns.geometry, ReturnGeometry::None) {
            origins.returns.geometry = ReturnGeometry::Full;
        }
        if matches!(destinations.returns.geometry, ReturnGeometry::None) {
            destinations.returns.geometry = ReturnGeometry::Full;
        }
    }
    let (topology, compiled_manifest, compiled_bundle) =
        load_route_execution_inputs(paths, dataset_id, profile)?;
    let engine = engine_description(&topology);
    let result = execute_matrix(&topology, &compiled_bundle, &origins, &destinations)
        .with_context(|| {
            format!(
                "executing matrix from '{}' to '{}'",
                origins_path.display(),
                destinations_path.display()
            )
        })?;
    store_matrix_run(
        paths,
        dataset_id,
        profile,
        origins_path,
        destinations_path,
        &origins,
        &destinations,
        &result,
        &compiled_manifest,
        engine,
        out,
    )
}

fn store_route_run(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    request_path: &Path,
    request: &netan_query::RouteRequest,
    result: &RouteResult,
    compiled_manifest: &CompiledProfileManifest,
    engine: EngineDescription,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    let mut manifest = new_run_manifest(
        RunKind::Route,
        dataset_id.to_string(),
        profile,
        request_path.display().to_string(),
        RunStatus::Succeeded,
        format!(
            "Route '{}' solved from node {} to node {} across {} edges.",
            result.route_id,
            result.origin.snapped_node_id,
            result.destination.snapped_node_id,
            result.summary.segment_count
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
    )?;
    manifest.algorithm.engine = engine.route_engine.to_string();
    manifest.algorithm.acceleration = engine.acceleration.to_string();
    manifest.methods_summary.plain_language = engine.route_summary.to_string();
    let result_path = out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("{}-result.json", manifest.run_id))
    });
    write_route_result(&result_path, request, result)?;
    manifest.result_path = Some(result_path.display().to_string());
    let manifest_path = write_run_manifest(paths, &manifest)?;
    Ok(StoredRun {
        result_path,
        manifest_path,
        summary: Some(serde_json::json!({
            "route_id": result.route_id,
            "total_distance_m": result.summary.total_distance_m,
            "total_travel_time_s": result.summary.total_travel_time_s,
            "total_generalized_cost": result.summary.total_generalized_cost,
            "segment_count": result.summary.segment_count,
        })),
    })
}

fn store_od_run(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    pairs_path: &Path,
    request: &netan_query::OdPairsDocument,
    result: &OdResult,
    compiled_manifest: &CompiledProfileManifest,
    engine: EngineDescription,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    let mut manifest = new_run_manifest(
        RunKind::Od,
        dataset_id.to_string(),
        profile,
        pairs_path.display().to_string(),
        RunStatus::Succeeded,
        format!(
            "OD batch completed with {} succeeded pairs and {} failed pairs.",
            result.succeeded_count, result.failed_count
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
    )?;
    manifest.algorithm.engine = engine.batch_engine.to_string();
    manifest.algorithm.acceleration = engine.acceleration.to_string();
    manifest.methods_summary.plain_language = engine.batch_summary.to_string();
    let result_path = out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("{}-result.json", manifest.run_id))
    });
    write_od_result(&result_path, request, result)?;
    manifest.result_path = Some(result_path.display().to_string());
    let manifest_path = write_run_manifest(paths, &manifest)?;
    Ok(StoredRun {
        result_path,
        manifest_path,
        summary: Some(serde_json::json!({
            "pair_count": result.pair_count,
            "succeeded_count": result.succeeded_count,
            "failed_count": result.failed_count,
        })),
    })
}

fn store_matrix_run(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    origins_path: &Path,
    destinations_path: &Path,
    origins: &netan_query::PointSetDocument,
    destinations: &netan_query::PointSetDocument,
    result: &MatrixResult,
    compiled_manifest: &CompiledProfileManifest,
    engine: EngineDescription,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    let mut manifest = new_run_manifest(
        RunKind::Matrix,
        dataset_id.to_string(),
        profile,
        format!(
            "{} | {}",
            origins_path.display(),
            destinations_path.display()
        ),
        RunStatus::Succeeded,
        format!(
            "Matrix batch completed with {} succeeded cells and {} failed cells.",
            result.succeeded_count, result.failed_count
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
    )?;
    manifest.algorithm.engine = engine.batch_engine.to_string();
    manifest.algorithm.acceleration = engine.acceleration.to_string();
    manifest.methods_summary.plain_language = engine.batch_summary.to_string();
    let result_path = out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("{}-result.json", manifest.run_id))
    });
    write_matrix_result(&result_path, origins, destinations, result)?;
    manifest.result_path = Some(result_path.display().to_string());
    let manifest_path = write_run_manifest(paths, &manifest)?;
    Ok(StoredRun {
        result_path,
        manifest_path,
        summary: Some(serde_json::json!({
            "origin_count": result.origin_count,
            "destination_count": result.destination_count,
            "cell_count": result.cell_count,
            "succeeded_count": result.succeeded_count,
            "failed_count": result.failed_count,
        })),
    })
}

fn experiment_run(paths: &WorkspacePaths, study: &Path) -> Result<()> {
    let experiment = load_experiment(study)?;
    let study_dir = study.parent().unwrap_or_else(|| Path::new("."));
    let mut scenarios = Vec::with_capacity(experiment.scenarios.len());
    let mut succeeded_count = 0_usize;

    for (index, scenario) in experiment.scenarios.iter().enumerate() {
        let scenario_id = scenario_id(index, scenario);
        let profile_path = resolve_study_path(study_dir, &scenario.profile);
        let request_path = scenario
            .request
            .as_ref()
            .map(|path| resolve_study_path(study_dir, path));
        let origins_path = scenario
            .origins
            .as_ref()
            .map(|path| resolve_study_path(study_dir, path));
        let destinations_path = scenario
            .destinations
            .as_ref()
            .map(|path| resolve_study_path(study_dir, path));
        let out_path = scenario
            .out
            .as_ref()
            .map(|path| resolve_study_path(study_dir, path))
            .or_else(|| {
                Some(paths.runs_dir.join(format!(
                    "experiment-{}-{}-result.json",
                    experiment.experiment.id, scenario_id
                )))
            });

        let scenario_result = match run_experiment_scenario(
            paths,
            &experiment.experiment.dataset,
            scenario.analysis,
            &profile_path,
            request_path.as_deref(),
            origins_path.as_deref(),
            destinations_path.as_deref(),
            out_path.clone(),
        ) {
            Ok(stored) => {
                succeeded_count += 1;
                ExperimentScenarioSummary {
                    scenario_id,
                    analysis: analysis_name(scenario.analysis),
                    profile_path: profile_path.display().to_string(),
                    request_path: request_path.as_ref().map(|path| path.display().to_string()),
                    origins_path: origins_path.as_ref().map(|path| path.display().to_string()),
                    destinations_path: destinations_path
                        .as_ref()
                        .map(|path| path.display().to_string()),
                    status: "succeeded".to_string(),
                    result_path: Some(stored.result_path.display().to_string()),
                    run_manifest_path: Some(stored.manifest_path.display().to_string()),
                    summary: stored.summary,
                    error: None,
                }
            }
            Err(error) => ExperimentScenarioSummary {
                scenario_id,
                analysis: analysis_name(scenario.analysis),
                profile_path: profile_path.display().to_string(),
                request_path: request_path.as_ref().map(|path| path.display().to_string()),
                origins_path: origins_path.as_ref().map(|path| path.display().to_string()),
                destinations_path: destinations_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                status: "failed".to_string(),
                result_path: out_path.map(|path| path.display().to_string()),
                run_manifest_path: None,
                summary: None,
                error: Some(error.to_string()),
            },
        };
        scenarios.push(scenario_result);
    }

    let failed_count = scenarios.len().saturating_sub(succeeded_count);
    let summary = ExperimentRunSummary {
        experiment_id: experiment.experiment.id.clone(),
        label: experiment.experiment.label.clone(),
        dataset: experiment.experiment.dataset.clone(),
        created_at: netan_report::now_rfc3339()?,
        status: if failed_count == 0 {
            "succeeded".to_string()
        } else if succeeded_count == 0 {
            "failed".to_string()
        } else {
            "partial".to_string()
        },
        scenario_count: scenarios.len(),
        succeeded_count,
        failed_count,
        scenarios,
    };
    let summary_path = paths
        .runs_dir
        .join(format!("experiment-{}.json", experiment.experiment.id));
    write_json(&summary_path, &summary)?;
    println!(
        "experiment summary written to {} ({} succeeded, {} failed)",
        summary_path.display(),
        summary.succeeded_count,
        summary.failed_count
    );
    Ok(())
}

fn report_render(manifest_path: &Path, out: Option<&Path>) -> Result<()> {
    let manifest = read_run_manifest(manifest_path)?;
    let result_summary = load_run_result_summary(&manifest)?;
    let markdown = render_run_markdown(&manifest, result_summary.as_ref());
    let html = render_run_html(&manifest, result_summary.as_ref());
    match out {
        None => println!("{markdown}"),
        Some(out) if markdown_output_path(out) => {
            fs::write(out, markdown).with_context(|| format!("writing {}", out.display()))?;
            println!("report written to {}", out.display());
        }
        Some(out) if html_output_path(out) => {
            fs::write(out, html).with_context(|| format!("writing {}", out.display()))?;
            println!("report written to {}", out.display());
        }
        Some(out) => {
            write_report_bundle(
                out,
                manifest_path,
                manifest.result_path.as_deref(),
                &markdown,
                &html,
            )?;
            println!("report bundle written to {}", out.display());
        }
    }
    Ok(())
}

fn cache_list(paths: &WorkspacePaths) -> Result<()> {
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
    Ok(())
}

fn api_serve(paths: WorkspacePaths, args: ApiServeArgs) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("creating API runtime")?;
    runtime.block_on(serve_api(
        paths,
        ApiServeOptions {
            bind: args.bind,
            dataset_id: args.dataset,
            default_profile: args.default_profile,
            profiles: args.profiles,
        },
    ))
}

fn load_route_execution_inputs(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
) -> Result<(
    TopologyBundle,
    CompiledProfileManifest,
    CompiledProfileBundle,
)> {
    let dataset_manifest = read_dataset_manifest(paths, dataset_id)
        .with_context(|| format!("reading dataset manifest for '{dataset_id}'"))?;
    let topology_ref = dataset_manifest
        .topology_bundle
        .clone()
        .context("dataset is missing a topology bundle; run `netan dataset import` first")?;
    let topology: TopologyBundle = read_topology_bundle(&topology_ref.path)
        .with_context(|| format!("reading topology bundle {}", topology_ref.path))?;
    let wanted_hash = profile.fingerprint()?;
    let compiled_manifest = read_compiled_profile_manifests(paths)?
        .into_iter()
        .find(|manifest| {
            manifest.dataset_id.0 == dataset_id && manifest.profile_hash == wanted_hash
        })
        .context("compiled profile bundle not found; run `netan profile compile` first")?;
    let compiled_bundle: CompiledProfileBundle =
        read_compiled_profile_bundle(&compiled_manifest.bundle.path).with_context(|| {
            format!(
                "reading compiled profile bundle {}",
                compiled_manifest.bundle.path
            )
        })?;
    Ok((topology, compiled_manifest, compiled_bundle))
}

fn load_edge_names(paths: &WorkspacePaths, dataset_id: &str) -> Result<Option<Vec<String>>> {
    let dataset_manifest = read_dataset_manifest(paths, dataset_id)
        .with_context(|| format!("reading dataset manifest for '{dataset_id}'"))?;
    if let Some(bundle_ref) = dataset_manifest.edge_name_bundle.as_ref() {
        let bundle = read_edge_name_bundle(&bundle_ref.path)
            .with_context(|| format!("reading edge-name bundle {}", bundle_ref.path))?;
        return Ok(Some(bundle.names));
    }
    anyhow::bail!(
        "dataset '{}' is missing the edge-name bundle required by the current format; remove the old cached dataset and re-import it",
        dataset_id
    )
}

fn software_info() -> SoftwareInfo {
    SoftwareInfo {
        executable: "netan".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_commit: option_env!("NETAN_GIT_COMMIT").map(ToString::to_string),
    }
}

fn engine_description(topology: &TopologyBundle) -> EngineDescription {
    let has_multi_edge_restrictions = topology
        .turn_restrictions
        .iter()
        .any(|restriction| restriction.edge_path.len() > 2);
    if has_multi_edge_restrictions {
        EngineDescription {
            route_engine: "astar_exact_multi_edge_turns",
            route_summary: "Exact forward A* shortest-path search over the compiled directed edge graph with persisted topology spatial indexing for snapping and multi-edge turn-restriction sequences. Turn penalties are not modeled yet.",
            batch_engine: "astar_exact_multi_edge_turns_repeated",
            batch_summary: "Repeated exact forward A* shortest-path searches over the compiled directed edge graph, one OD pair or matrix cell at a time, with persisted topology spatial indexing for snapping and multi-edge turn-restriction sequences. Turn penalties are not modeled yet.",
            acceleration: "spatial_index+a_star+turn_automaton",
        }
    } else {
        EngineDescription {
            route_engine: "astar_exact_pairwise_turns",
            route_summary: "Exact forward A* shortest-path search over the compiled directed edge graph with persisted topology spatial indexing for snapping and pairwise turn prohibitions. Turn penalties are not modeled yet.",
            batch_engine: "astar_exact_pairwise_turns_repeated",
            batch_summary: "Repeated exact forward A* shortest-path searches over the compiled directed edge graph, one OD pair or matrix cell at a time, with persisted topology spatial indexing for snapping and pairwise turn prohibitions. Turn penalties are not modeled yet.",
            acceleration: "spatial_index+a_star+turn_automaton",
        }
    }
}

fn output_needs_geometry(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            ext.eq_ignore_ascii_case("csv")
                || ext.eq_ignore_ascii_case("geojson")
                || ext.eq_ignore_ascii_case("gpkg")
                || ext.eq_ignore_ascii_case("geopackage")
                || ext.eq_ignore_ascii_case("geoparquet")
                || ext.eq_ignore_ascii_case("gpq")
        })
        .unwrap_or(false)
}

fn run_experiment_scenario(
    paths: &WorkspacePaths,
    dataset_id: &str,
    analysis: AnalysisKind,
    profile_path: &Path,
    request_path: Option<&Path>,
    origins_path: Option<&Path>,
    destinations_path: Option<&Path>,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    profile_compile(paths, dataset_id, profile_path)?;
    let profile = load_profile(profile_path)?;
    profile.validate()?;
    match analysis {
        AnalysisKind::Route => {
            let request_path = request_path.context("route scenario is missing a request path")?;
            let request = load_route_request(request_path)?;
            run_route_analysis(paths, dataset_id, &profile, request_path, request, out)
        }
        AnalysisKind::Od => {
            let request_path = request_path.context("OD scenario is missing a request path")?;
            let request = load_od_pairs(request_path)?;
            run_od_analysis(paths, dataset_id, &profile, request_path, request, out)
        }
        AnalysisKind::Matrix => {
            let origins_path =
                origins_path.context("matrix scenario is missing an origins path")?;
            let destinations_path =
                destinations_path.context("matrix scenario is missing a destinations path")?;
            let origins = load_point_set(origins_path)?;
            let destinations = load_point_set(destinations_path)?;
            run_matrix_analysis(
                paths,
                dataset_id,
                &profile,
                origins_path,
                destinations_path,
                origins,
                destinations,
                out,
            )
        }
    }
}

fn scenario_id(index: usize, scenario: &netan_query::ScenarioSpec) -> String {
    scenario
        .id
        .clone()
        .unwrap_or_else(|| format!("{:02}_{}", index + 1, analysis_name(scenario.analysis)))
}

fn analysis_name(analysis: AnalysisKind) -> &'static str {
    match analysis {
        AnalysisKind::Route => "route",
        AnalysisKind::Od => "od",
        AnalysisKind::Matrix => "matrix",
    }
}

fn resolve_study_path(base_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn markdown_output_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

fn html_output_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))
}

fn write_report_bundle(
    out_dir: &Path,
    manifest_path: &Path,
    result_path: Option<&str>,
    markdown: &str,
    html: &str,
) -> Result<()> {
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let markdown_path = out_dir.join("index.md");
    let html_path = out_dir.join("index.html");
    let manifest_copy_path = out_dir.join("run-manifest.json");
    fs::write(&markdown_path, markdown)
        .with_context(|| format!("writing {}", markdown_path.display()))?;
    fs::write(&html_path, html).with_context(|| format!("writing {}", html_path.display()))?;
    fs::copy(manifest_path, &manifest_copy_path).with_context(|| {
        format!(
            "copying run manifest {} to {}",
            manifest_path.display(),
            manifest_copy_path.display()
        )
    })?;
    if let Some(result_path) = result_path {
        let source = Path::new(result_path);
        if source.exists() {
            let result_copy_path = out_dir.join(
                source
                    .file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new("result.json")),
            );
            fs::copy(source, &result_copy_path).with_context(|| {
                format!(
                    "copying result {} to {}",
                    source.display(),
                    result_copy_path.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{html_output_path, markdown_output_path, write_report_bundle};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn detects_report_output_formats() {
        assert!(markdown_output_path(Path::new("report.md")));
        assert!(html_output_path(Path::new("report.html")));
        assert!(!markdown_output_path(Path::new("report.bundle")));
    }

    #[test]
    fn writes_report_bundle_with_manifest_and_result() {
        let temp_dir = temp_dir("bundle");
        let out_dir = temp_dir.join("report-bundle");
        let manifest_path = temp_dir.join("run.json");
        let result_path = temp_dir.join("result.json");
        fs::write(&manifest_path, "{\"run_id\":\"run-1\"}").expect("manifest written");
        fs::write(&result_path, "{\"route_id\":\"route-1\"}").expect("result written");

        write_report_bundle(
            &out_dir,
            &manifest_path,
            Some(result_path.to_str().expect("utf-8 path")),
            "# Report\n",
            "<html></html>",
        )
        .expect("bundle write succeeds");

        assert!(out_dir.join("index.md").exists());
        assert!(out_dir.join("index.html").exists());
        assert!(out_dir.join("run-manifest.json").exists());
        assert!(out_dir.join("result.json").exists());
        fs::remove_dir_all(temp_dir).ok();
    }

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time works")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("netan-cli-{label}-{unique}"));
        fs::create_dir_all(&path).expect("temp dir created");
        path
    }
}
