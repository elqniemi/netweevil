use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use netweevil_api::{ApiServeOptions, serve as serve_api};
use netweevil_core::{
    AccelerationBuildProfile, AccelerationBuildSettings, CacheBundleId, CompiledProfileBundle,
    DatasetAccelerationBundle, TopologyBundle,
};
use netweevil_ingest::{
    DatasetImportOptions, DatasetImportProgress, DatasetImportStage, import_dataset_with_progress,
};
use netweevil_persist::{
    WorkspacePaths, read_acceleration_bundle, read_compiled_profile_bundle,
    read_compiled_profile_manifests, read_dataset_manifest, read_dataset_manifests,
    read_edge_name_bundle, read_run_manifest, read_topology_bundle, write_compiled_profile_bundle,
    write_compiled_profile_manifest, write_json, write_run_manifest,
};
use netweevil_profile::{
    ProfileCompileProgress, ProfileCompileStage, ProfileDocument, ReturnGeometry,
    compile_profile_bundle_with_acceleration_with_progress, load_profile,
};
use netweevil_query::{
    AccessibilityCategoryRequest, AccessibilityRequest, AccessibilityResult, AnalysisKind,
    MatrixResult, OdResult, PreparedRoutingEngine, RouteBatchResult, RouteResult,
    ServiceAreaResult, analysis_failure, load_experiment, load_od_pairs, load_point_set,
    load_route_batch, load_route_request, load_service_area_request,
};
use netweevil_report::{
    BundleRef, CompiledProfileManifest, RunKind, RunStatus, SoftwareInfo, load_run_result_summary,
    new_run_manifest, render_run_html, render_run_markdown, write_matrix_result, write_od_result,
    write_route_batch_result, write_route_result, write_service_area_result,
};
use netweevil_transit::{
    OPENOV_GTFS_URL, PreparedTransitRouter, TransitFeedManifest, TransitImportOptions,
    TransitRouteRequest, TransitRouteResult, import_gtfs, load_transit_request,
    read_transit_bundle, transit_import_summary, write_transit_bundle,
};
use serde::{Deserialize, Serialize};
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

#[derive(Subcommand, Debug)]
enum DatasetCommand {
    Import(DatasetImportArgs),
    List,
}

#[derive(Subcommand, Debug)]
enum TransitCommand {
    Import(TransitImportArgs),
    List,
}

#[derive(Args, Debug)]
struct DatasetImportArgs {
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

#[derive(Args, Debug)]
struct TransitImportArgs {
    source: PathBuf,
    #[arg(long)]
    name: String,
    #[arg(long)]
    service_start: String,
    #[arg(long, default_value_t = 7)]
    service_days: u32,
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
    RouteBatch(RouteBatchArgs),
    Od(OdArgs),
    Matrix(MatrixArgs),
    Accessibility(AccessibilityArgs),
    ServiceArea(ServiceAreaArgs),
    TransitRoute(TransitRouteArgs),
    TransitBatch(TransitBatchArgs),
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
    #[arg(long = "transit-feed")]
    transit_feeds: Vec<String>,
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
struct RouteBatchArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long)]
    requests: PathBuf,
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

#[derive(Args, Debug)]
struct AccessibilityArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long)]
    origins: PathBuf,
    #[arg(long = "destination")]
    destinations: Vec<String>,
    #[arg(long, value_delimiter = ',', default_values_t = [300.0, 600.0, 900.0, 1200.0])]
    thresholds_s: Vec<f64>,
    #[arg(long, default_value_t = 1200.0)]
    max_travel_time_s: f64,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct ServiceAreaArgs {
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
struct TransitRouteArgs {
    #[arg(long)]
    feed: String,
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct TransitBatchArgs {
    #[arg(long)]
    feed: String,
    #[arg(long)]
    requests: PathBuf,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct TransitBatchDocument {
    #[serde(default)]
    requests: Vec<TransitRouteRequest>,
}

#[derive(Debug, Serialize)]
struct TransitBatchItemResult {
    request_index: usize,
    route_id: String,
    result: TransitRouteResult,
}

#[derive(Debug, Serialize)]
struct TransitBatchResult {
    route_count: usize,
    scheduled_count: usize,
    unreachable_count: usize,
    not_implemented_count: usize,
    failed_count: usize,
    items: Vec<TransitBatchItemResult>,
    failures: Vec<TransitBatchFailure>,
}

#[derive(Debug, Serialize)]
struct TransitBatchFailure {
    request_index: usize,
    route_id: String,
    error: String,
}

fn dataset_import(paths: &WorkspacePaths, args: DatasetImportArgs) -> Result<()> {
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

fn dataset_list(paths: &WorkspacePaths) -> Result<()> {
    for manifest in read_dataset_manifests(paths)? {
        println!(
            "{}\t{}\t{:?}\t{}",
            manifest.dataset_id.0, manifest.source_path, manifest.build_stage, manifest.imported_at
        );
    }
    Ok(())
}

fn transit_import(paths: &WorkspacePaths, args: TransitImportArgs) -> Result<()> {
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

fn transit_list(paths: &WorkspacePaths) -> Result<()> {
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

fn read_transit_manifests(paths: &WorkspacePaths) -> Result<Vec<TransitFeedManifest>> {
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

fn read_transit_manifest(paths: &WorkspacePaths, feed_id: &str) -> Result<TransitFeedManifest> {
    let path = paths.transit_feeds_dir.join(format!("{feed_id}.json"));
    netweevil_persist::read_json(&path)
        .with_context(|| format!("reading transit feed manifest {}", path.display()))
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
    let mut progress_line_len = 0_usize;
    let profile = load_profile(profile_path)?;
    profile.validate()?;
    render_profile_compile_message(
        "Load Dataset",
        format!("Reading dataset manifest for '{dataset}'"),
        false,
        &mut progress_line_len,
    );
    let dataset_manifest = read_dataset_manifest(paths, dataset)
        .with_context(|| format!("reading dataset manifest for '{dataset}'"))?;
    let topology_ref = dataset_manifest
        .topology_bundle
        .clone()
        .context("dataset is missing a topology bundle; run `netweevil dataset import` first")?;
    render_profile_compile_message(
        "Load Dataset",
        format!("Reading topology bundle {}", topology_ref.path),
        false,
        &mut progress_line_len,
    );
    let topology: TopologyBundle = read_topology_bundle(&topology_ref.path)
        .with_context(|| format!("reading topology bundle {}", topology_ref.path))?;
    render_profile_compile_message(
        "Load Dataset",
        "Reading acceleration bundle metadata".to_string(),
        false,
        &mut progress_line_len,
    );
    let acceleration: Option<(DatasetAccelerationBundle, CacheBundleId)> = dataset_manifest
        .acceleration_bundle
        .as_ref()
        .map(|bundle_ref| {
            read_acceleration_bundle(&bundle_ref.path)
                .with_context(|| format!("reading acceleration bundle {}", bundle_ref.path))
                .map(|bundle| (bundle, bundle_ref.bundle_id.clone()))
        })
        .transpose()?;
    let compiled_bundle = compile_profile_bundle_with_acceleration_with_progress(
        &profile,
        &topology,
        topology_ref.bundle_id.clone(),
        acceleration
            .as_ref()
            .map(|(bundle, bundle_id)| (bundle, bundle_id.clone())),
        |event| render_profile_compile_progress(&event, &mut progress_line_len),
    )
    .with_context(|| {
        format!(
            "compiling profile '{}' for dataset '{dataset}'",
            profile.profile.id
        )
    })?;
    let profile_hash = profile.fingerprint()?;
    let compile_id = format!("{dataset}-{}", &profile_hash[..12]);
    let bundle_path = paths
        .metric_bundles_dir
        .join(format!("metric-{compile_id}.bin"));
    render_profile_compile_message(
        "Write Bundle",
        format!("Writing compiled profile bundle {}", bundle_path.display()),
        false,
        &mut progress_line_len,
    );
    write_compiled_profile_bundle(&bundle_path, &compiled_bundle)?;
    let manifest = CompiledProfileManifest {
        compile_id: compile_id.clone(),
        dataset_id: netweevil_core::DatasetId::new(dataset.to_string()),
        profile_id: profile.profile.id.clone(),
        profile_hash,
        defaults_pack: profile.profile.defaults_pack.clone(),
        mode: profile.profile.mode,
        created_at: netweevil_report::now_rfc3339()?,
        topology_bundle_id: Some(topology_ref.bundle_id),
        edge_count: Some(compiled_bundle.edge_metrics.len() as u64),
        bundle: BundleRef {
            bundle_id: CacheBundleId::new(format!("metric-{compile_id}")),
            path: bundle_path.display().to_string(),
        },
    };
    render_profile_compile_message(
        "Write Manifest",
        format!(
            "Writing compiled profile manifest for '{}'",
            profile.profile.id
        ),
        false,
        &mut progress_line_len,
    );
    let path = write_compiled_profile_manifest(paths, &manifest)?;
    render_profile_compile_message(
        "Complete",
        format!(
            "Compiled profile '{}' for dataset '{}' into {} edge metrics",
            manifest.profile_id,
            manifest.dataset_id.0,
            compiled_bundle.edge_metrics.len()
        ),
        true,
        &mut progress_line_len,
    );
    println!(
        "compiled profile '{}' for dataset '{}' into {} edge metrics",
        manifest.profile_id,
        manifest.dataset_id.0,
        compiled_bundle.edge_metrics.len()
    );
    println!("compiled profile manifest written to {}", path.display());
    Ok(())
}

fn render_profile_compile_progress(event: &ProfileCompileProgress, last_line_len: &mut usize) {
    render_profile_compile_message(
        event.stage.label(),
        event.message.clone(),
        matches!(event.stage, ProfileCompileStage::Complete),
        last_line_len,
    );
}

fn render_profile_compile_message(
    stage_label: &str,
    message: String,
    done: bool,
    last_line_len: &mut usize,
) {
    let mut line = format!("[{stage_label}] {message}");
    if done {
        line.push_str(" [done]");
    }
    let padding = last_line_len.saturating_sub(line.len());
    eprint!("\r{line}{:padding$}", "");
    if done {
        eprintln!();
        *last_line_len = 0;
    } else {
        *last_line_len = line.len();
    }
}

fn analyze_route(paths: &WorkspacePaths, args: RouteArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let request = load_route_request(&args.request)?;
    let stored = match run_route_analysis(
        paths,
        &args.dataset,
        &profile,
        &args.request,
        request,
        args.out,
    ) {
        Ok(stored) => stored,
        Err(error) => {
            emit_structured_failure_diagnostics(&error);
            return Err(error);
        }
    };
    println!("route result written to {}", stored.result_path.display());
    println!("run manifest written to {}", stored.manifest_path.display());
    Ok(())
}

fn analyze_route_batch(paths: &WorkspacePaths, args: RouteBatchArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let mut requests = load_route_batch(&args.requests)?;
    if args.out.as_deref().is_some_and(output_needs_geometry) {
        for entry in &mut requests.requests {
            if matches!(entry.request.returns.geometry, ReturnGeometry::None) {
                entry.request.returns.geometry = ReturnGeometry::Full;
            }
        }
    }
    let stored = match run_route_batch_analysis(
        paths,
        &args.dataset,
        &profile,
        &args.requests,
        requests,
        args.out,
    ) {
        Ok(stored) => stored,
        Err(error) => {
            emit_structured_failure_diagnostics(&error);
            return Err(error);
        }
    };
    println!(
        "route batch result written to {}",
        stored.result_path.display()
    );
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
    let stored = match run_od_analysis(
        paths,
        &args.dataset,
        &profile,
        &args.pairs,
        request,
        args.out,
    ) {
        Ok(stored) => stored,
        Err(error) => {
            emit_structured_failure_diagnostics(&error);
            return Err(error);
        }
    };
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
    let stored = match run_matrix_analysis(
        paths,
        &args.dataset,
        &profile,
        &args.origins,
        &args.destinations,
        origins,
        destinations,
        args.out,
    ) {
        Ok(stored) => stored,
        Err(error) => {
            emit_structured_failure_diagnostics(&error);
            return Err(error);
        }
    };
    println!("matrix result written to {}", stored.result_path.display());
    println!("run manifest written to {}", stored.manifest_path.display());
    Ok(())
}

fn analyze_accessibility(paths: &WorkspacePaths, args: AccessibilityArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let origins = load_point_set(&args.origins)?;
    let categories = load_accessibility_categories(&args.destinations)?;
    let request = AccessibilityRequest {
        origins,
        categories,
        thresholds_s: args.thresholds_s,
        max_travel_time_s: args.max_travel_time_s,
    };
    let stored = match run_accessibility_analysis(
        paths,
        &args.dataset,
        &profile,
        &args.origins,
        &request,
        args.out,
    ) {
        Ok(stored) => stored,
        Err(error) => {
            emit_structured_failure_diagnostics(&error);
            return Err(error);
        }
    };
    println!(
        "accessibility result written to {}",
        stored.result_path.display()
    );
    println!("run manifest written to {}", stored.manifest_path.display());
    Ok(())
}

fn analyze_service_area(paths: &WorkspacePaths, args: ServiceAreaArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let mut request = load_service_area_request(&args.request)?;
    if args.out.as_deref().is_some_and(output_needs_geometry) {
        request.returns.geometry = true;
    }
    let stored = run_service_area_analysis(
        paths,
        &args.dataset,
        &profile,
        &args.request,
        &request,
        args.out,
    )?;
    println!(
        "service-area result written to {}",
        stored.result_path.display()
    );
    println!("run manifest written to {}", stored.manifest_path.display());
    Ok(())
}

fn analyze_transit_route(paths: &WorkspacePaths, args: TransitRouteArgs) -> Result<()> {
    let manifest = read_transit_manifest(paths, &args.feed)?;
    let bundle = read_transit_bundle(&manifest.bundle_path)
        .with_context(|| format!("reading transit bundle {}", manifest.bundle_path))?;
    let request = load_transit_request(&args.request)?;
    let router = PreparedTransitRouter::new(Arc::new(bundle));
    let result = router
        .execute_route(&request)
        .with_context(|| format!("executing transit route '{}'", request.route_id))?;
    let result_path = args.out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("transit-route-{}.json", request.route_id))
    });
    write_json(&result_path, &result)?;
    println!("transit route result written to {}", result_path.display());
    Ok(())
}

fn analyze_transit_batch(paths: &WorkspacePaths, args: TransitBatchArgs) -> Result<()> {
    let manifest = read_transit_manifest(paths, &args.feed)?;
    let bundle = read_transit_bundle(&manifest.bundle_path)
        .with_context(|| format!("reading transit bundle {}", manifest.bundle_path))?;
    let document = load_transit_batch(&args.requests)?;
    let router = PreparedTransitRouter::new(Arc::new(bundle));
    let mut items = Vec::new();
    let mut failures = Vec::new();
    let mut scheduled_count = 0_usize;
    let mut unreachable_count = 0_usize;
    let mut not_implemented_count = 0_usize;

    for (index, request) in document.requests.iter().enumerate() {
        match router.execute_route(request) {
            Ok(result) => {
                match result.outcome {
                    netweevil_transit::TransitOutcome::Scheduled => scheduled_count += 1,
                    netweevil_transit::TransitOutcome::Unreachable => unreachable_count += 1,
                    netweevil_transit::TransitOutcome::NotImplemented => not_implemented_count += 1,
                }
                items.push(TransitBatchItemResult {
                    request_index: index + 1,
                    route_id: request.route_id.clone(),
                    result,
                });
            }
            Err(error) => failures.push(TransitBatchFailure {
                request_index: index + 1,
                route_id: request.route_id.clone(),
                error: error.to_string(),
            }),
        }
    }

    let result = TransitBatchResult {
        route_count: document.requests.len(),
        scheduled_count,
        unreachable_count,
        not_implemented_count,
        failed_count: failures.len(),
        items,
        failures,
    };
    let result_path = args.out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("transit-batch-{}.json", args.feed))
    });
    write_json(&result_path, &result)?;
    println!("transit batch result written to {}", result_path.display());
    println!(
        "transit batch completed with {} scheduled, {} unreachable, {} not implemented, and {} failed routes",
        result.scheduled_count,
        result.unreachable_count,
        result.not_implemented_count,
        result.failed_count
    );
    Ok(())
}

fn load_transit_batch(path: &Path) -> Result<TransitBatchDocument> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading transit batch {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parsing transit batch {}", path.display()))
}

fn run_route_analysis(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    request_path: &Path,
    mut request: netweevil_query::RouteRequest,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    if out.as_deref().is_some_and(output_needs_geometry)
        && matches!(request.returns.geometry, ReturnGeometry::None)
    {
        request.returns.geometry = ReturnGeometry::Full;
    }
    let (topology, acceleration, compiled_manifest, compiled_bundle) =
        load_route_execution_inputs(paths, dataset_id, profile)?;
    let engine = engine_description(&topology);
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let edge_names = if request.returns.segment_rows {
        load_edge_names(paths, dataset_id)?
    } else {
        None
    };
    let result = if let Some(edge_names) = edge_names.as_ref() {
        prepared.execute_route_with_edge_names(&request, edge_names)
    } else {
        prepared.execute_route(&request)
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

fn run_route_batch_analysis(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    requests_path: &Path,
    requests: netweevil_query::RouteBatchDocument,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    let (topology, acceleration, compiled_manifest, compiled_bundle) =
        load_route_execution_inputs(paths, dataset_id, profile)?;
    let engine = engine_description(&topology);
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;

    let needs_edge_names = requests
        .requests
        .iter()
        .any(|entry| entry.request.returns.segment_rows);
    let edge_names = if needs_edge_names {
        load_edge_names(paths, dataset_id)?
    } else {
        None
    };

    let mut items = Vec::with_capacity(requests.requests.len());
    let mut succeeded_count = 0_usize;
    let mut failed_count = 0_usize;
    let mut warnings = Vec::new();
    let route_count = requests.requests.len();

    for (index, entry) in requests.requests.iter().enumerate() {
        if let Some(profile_id) = entry.profile_id.as_deref() {
            if profile_id != profile.profile.id {
                anyhow::bail!(
                    "route '{}' requests profile_id '{}' but CLI batch is using profile '{}'",
                    entry.request.route_id,
                    profile_id,
                    profile.profile.id
                );
            }
        }

        let execution = if let Some(edge_names) = edge_names.as_ref() {
            prepared.execute_route_with_edge_names(&entry.request, edge_names)
        } else {
            prepared.execute_route(&entry.request)
        };

        match execution {
            Ok(route) => {
                succeeded_count += 1;
                if !route.warnings.is_empty() {
                    warnings.extend(route.warnings.clone());
                }
                items.push(netweevil_query::RouteBatchItemResult {
                    route_id: route.route_id.clone(),
                    origin_id: entry.request.origin.id.clone(),
                    destination_id: entry.request.destination.id.clone(),
                    status: netweevil_query::BatchItemStatus::Succeeded,
                    route: Some(route),
                    error: None,
                });
            }
            Err(error) => {
                failed_count += 1;
                items.push(netweevil_query::RouteBatchItemResult {
                    route_id: entry.request.route_id.clone(),
                    origin_id: entry.request.origin.id.clone(),
                    destination_id: entry.request.destination.id.clone(),
                    status: netweevil_query::BatchItemStatus::Failed,
                    route: None,
                    error: Some(error.to_string()),
                });
            }
        }

        if route_count <= 20 || (index + 1) % 10 == 0 || index + 1 == route_count {
            eprintln!(
                "[route-batch] solved {}/{} routes ({} succeeded, {} failed)",
                index + 1,
                route_count,
                succeeded_count,
                failed_count
            );
        }
    }

    let result = RouteBatchResult {
        route_count: items.len(),
        succeeded_count,
        failed_count,
        items,
        warnings,
    };

    store_route_batch_run(
        paths,
        dataset_id,
        profile,
        requests_path,
        &requests,
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
    mut request: netweevil_query::OdPairsDocument,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    if out.as_deref().is_some_and(output_needs_geometry)
        && matches!(request.returns.geometry, ReturnGeometry::None)
    {
        request.returns.geometry = ReturnGeometry::Full;
    }
    let (topology, acceleration, compiled_manifest, compiled_bundle) =
        load_route_execution_inputs(paths, dataset_id, profile)?;
    let engine = engine_description(&topology);
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let result = prepared
        .execute_od(&request)
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
    mut origins: netweevil_query::PointSetDocument,
    mut destinations: netweevil_query::PointSetDocument,
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
    let (topology, acceleration, compiled_manifest, compiled_bundle) =
        load_route_execution_inputs(paths, dataset_id, profile)?;
    let engine = engine_description(&topology);
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let result = prepared
        .execute_matrix(&origins, &destinations)
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

fn load_accessibility_categories(specs: &[String]) -> Result<Vec<AccessibilityCategoryRequest>> {
    if specs.is_empty() {
        anyhow::bail!(
            "at least one --destination category=path argument is required for accessibility analysis"
        );
    }
    specs
        .iter()
        .map(|spec| {
            let (category_id, path) = spec.split_once('=').with_context(|| {
                format!("destination spec '{spec}' must use category=path syntax")
            })?;
            let category_id = category_id.trim();
            if category_id.is_empty() {
                anyhow::bail!("destination spec '{spec}' has an empty category");
            }
            let path = PathBuf::from(path.trim());
            let destinations = load_point_set(&path).with_context(|| {
                format!("loading accessibility destinations {}", path.display())
            })?;
            Ok(AccessibilityCategoryRequest {
                category_id: category_id.to_string(),
                destinations,
            })
        })
        .collect()
}

fn run_accessibility_analysis(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    origins_path: &Path,
    request: &AccessibilityRequest,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    let (topology, acceleration, compiled_manifest, compiled_bundle) =
        load_route_execution_inputs(paths, dataset_id, profile)?;
    let engine = engine_description(&topology);
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let result = prepared
        .execute_accessibility(request)
        .with_context(|| format!("executing accessibility from '{}'", origins_path.display()))?;
    store_accessibility_run(
        paths,
        dataset_id,
        profile,
        origins_path,
        request,
        &result,
        &compiled_manifest,
        engine,
        out,
    )
}

fn run_service_area_analysis(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    request_path: &Path,
    request: &netweevil_query::ServiceAreaRequest,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    let (topology, acceleration, compiled_manifest, compiled_bundle) =
        load_route_execution_inputs(paths, dataset_id, profile)?;
    let engine = engine_description(&topology);
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let result = prepared
        .execute_service_area(request)
        .with_context(|| format!("executing service-area '{}'", request.analysis_id))?;
    store_service_area_run(
        paths,
        dataset_id,
        profile,
        request_path,
        request,
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
    request: &netweevil_query::RouteRequest,
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
            "Route '{}' solved from node {} to node {} across {} edges with outcome '{}' and {} violation(s).",
            result.route_id,
            result.origin.snapped_node_id,
            result.destination.snapped_node_id,
            result.summary.segment_count,
            serde_json::to_string(&result.outcome)?.trim_matches('"'),
            result.summary.violation_count,
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
    )?;
    manifest.algorithm.engine = engine.route_engine.to_string();
    manifest.algorithm.acceleration = engine.acceleration.to_string();
    manifest.methods_summary.plain_language = engine.route_summary.to_string();
    manifest.connectivity_policy = Some(request.connectivity.clone());
    manifest.fallback_policy = Some(request.fallback.clone());
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
            "illegal_movement_penalty_s": result.summary.illegal_movement_penalty_s,
            "illegal_movement_penalty_cost": result.summary.illegal_movement_penalty_cost,
            "violation_count": result.summary.violation_count,
            "violation_types": result.summary.violation_types,
            "segment_count": result.summary.segment_count,
        })),
    })
}

fn store_route_batch_run(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    requests_path: &Path,
    requests: &netweevil_query::RouteBatchDocument,
    result: &RouteBatchResult,
    compiled_manifest: &CompiledProfileManifest,
    engine: EngineDescription,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    let mut manifest = new_run_manifest(
        RunKind::RouteBatch,
        dataset_id.to_string(),
        profile,
        requests_path.display().to_string(),
        RunStatus::Succeeded,
        format!(
            "Route batch completed with {} succeeded routes and {} failed routes.",
            result.succeeded_count, result.failed_count
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
    )?;
    manifest.algorithm.engine = engine.route_engine.to_string();
    manifest.algorithm.acceleration = engine.acceleration.to_string();
    manifest.methods_summary.plain_language = format!(
        "{} Route-batch execution reused one prepared routing engine for {} route requests and wrote one consolidated result file.",
        engine.route_summary, result.route_count,
    );
    manifest.connectivity_policy = requests
        .requests
        .first()
        .map(|entry| entry.request.connectivity.clone());
    manifest.fallback_policy = requests
        .requests
        .first()
        .map(|entry| entry.request.fallback.clone());

    let result_path = out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("{}-result.json", manifest.run_id))
    });
    write_route_batch_result(&result_path, requests, result)?;
    manifest.result_path = Some(result_path.display().to_string());
    let manifest_path = write_run_manifest(paths, &manifest)?;
    Ok(StoredRun {
        result_path,
        manifest_path,
        summary: Some(serde_json::json!({
            "route_count": result.route_count,
            "succeeded_count": result.succeeded_count,
            "failed_count": result.failed_count,
        })),
    })
}

fn store_od_run(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    pairs_path: &Path,
    request: &netweevil_query::OdPairsDocument,
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
            "OD batch completed with {} succeeded pairs, {} ignored pairs, and {} failed pairs.",
            result.succeeded_count, result.ignored_count, result.failed_count
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
    )?;
    manifest.algorithm.engine = engine.batch_engine.to_string();
    manifest.algorithm.acceleration = engine.acceleration.to_string();
    manifest.methods_summary.plain_language = engine.batch_summary.to_string();
    manifest.connectivity_policy = Some(request.connectivity.clone());
    manifest.fallback_policy = Some(request.fallback.clone());
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
            "ignored_count": result.ignored_count,
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
    origins: &netweevil_query::PointSetDocument,
    destinations: &netweevil_query::PointSetDocument,
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
            "Matrix batch completed with {} succeeded cells, {} ignored cells, and {} failed cells.",
            result.succeeded_count, result.ignored_count, result.failed_count
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
    )?;
    manifest.algorithm.engine = engine.batch_engine.to_string();
    manifest.algorithm.acceleration = engine.acceleration.to_string();
    manifest.methods_summary.plain_language = engine.batch_summary.to_string();
    manifest.connectivity_policy = Some(
        if origins.connectivity == netweevil_query::ConnectivityPolicy::default() {
            destinations.connectivity.clone()
        } else {
            origins.connectivity.clone()
        },
    );
    manifest.fallback_policy = Some(
        if origins.fallback == netweevil_query::FallbackPolicy::default() {
            destinations.fallback.clone()
        } else {
            origins.fallback.clone()
        },
    );
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
            "ignored_count": result.ignored_count,
            "failed_count": result.failed_count,
        })),
    })
}

fn store_accessibility_run(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    origins_path: &Path,
    request: &AccessibilityRequest,
    result: &AccessibilityResult,
    compiled_manifest: &CompiledProfileManifest,
    engine: EngineDescription,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    let category_list = request
        .categories
        .iter()
        .map(|category| category.category_id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let mut manifest = new_run_manifest(
        RunKind::Accessibility,
        dataset_id.to_string(),
        profile,
        format!("{} | {}", origins_path.display(), category_list),
        RunStatus::Succeeded,
        format!(
            "Accessibility reduction completed with {} succeeded origin-category rows and {} failed rows across {} origin(s), {} category/categories, and {} destination(s).",
            result.succeeded_count,
            result.failed_count,
            result.origin_count,
            result.category_count,
            result.destination_count
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
    )?;
    manifest.algorithm.engine = "bounded_single_origin_accessibility".to_string();
    manifest.algorithm.acceleration = engine.acceleration.to_string();
    manifest.methods_summary.plain_language = format!(
        "{} Accessibility execution performs one exact bounded legal-network expansion per unique snapped origin up to {:.0}s, snaps all destinations once, and reduces each category to nearest destination and threshold counts without materializing all OD cells.",
        engine.batch_summary, result.max_travel_time_s
    );
    manifest.connectivity_policy = Some(request.origins.connectivity.clone());
    manifest.fallback_policy = Some(request.origins.fallback.clone());
    let result_path = out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("{}-result.json", manifest.run_id))
    });
    write_accessibility_result(&result_path, result)?;
    manifest.result_path = Some(result_path.display().to_string());
    let manifest_path = write_run_manifest(paths, &manifest)?;
    Ok(StoredRun {
        result_path,
        manifest_path,
        summary: Some(serde_json::json!({
            "origin_count": result.origin_count,
            "category_count": result.category_count,
            "destination_count": result.destination_count,
            "row_count": result.row_count,
            "succeeded_count": result.succeeded_count,
            "failed_count": result.failed_count,
            "skipped_origin_count": result.skipped_origin_count,
        })),
    })
}

fn store_service_area_run(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    request_path: &Path,
    request: &netweevil_query::ServiceAreaRequest,
    result: &ServiceAreaResult,
    compiled_manifest: &CompiledProfileManifest,
    engine: EngineDescription,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    let mut manifest = new_run_manifest(
        RunKind::ServiceArea,
        dataset_id.to_string(),
        profile,
        request_path.display().to_string(),
        RunStatus::Succeeded,
        format!(
            "Service-area '{}' completed with {} feature(s) across {} threshold(s) using output_mode={:?} and multi_origin_mode={:?}; {} origin(s) processed, {} skipped, {} with fallback.",
            result.analysis_id,
            result.features.len(),
            result.threshold_count,
            result.output_mode,
            result.multi_origin_mode,
            result.processed_origin_count,
            result.skipped_origin_count,
            result.fallback_origin_count
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
    )?;
    manifest.algorithm.engine = engine.batch_engine.to_string();
    manifest.algorithm.acceleration = engine.acceleration.to_string();
    manifest.methods_summary.plain_language = format!(
        "{} Service-area execution reuses one exact legal-network expansion per unique snapped origin and threshold metric, then emits cumulative or ring bands as network and/or polygon outputs.",
        engine.batch_summary
    );
    manifest.connectivity_policy = Some(request.connectivity.clone());
    manifest.fallback_policy = Some(request.fallback.clone());
    let result_path = out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("{}-result.json", manifest.run_id))
    });
    write_service_area_result(&result_path, request, result)?;
    manifest.result_path = Some(result_path.display().to_string());
    let manifest_path = write_run_manifest(paths, &manifest)?;

    Ok(StoredRun {
        result_path,
        manifest_path,
        summary: Some(serde_json::json!({
            "analysis_id": result.analysis_id,
            "outcome": result.outcome,
            "origin_count": result.origin_count,
            "processed_origin_count": result.processed_origin_count,
            "skipped_origin_count": result.skipped_origin_count,
            "fallback_origin_count": result.fallback_origin_count,
            "threshold_count": result.threshold_count,
            "feature_count": result.features.len(),
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
        created_at: netweevil_report::now_rfc3339()?,
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
    println!("transit feeds:");
    for transit in read_transit_manifests(paths)? {
        println!(
            "  {}\t{} stops\t{} connections\t{}",
            transit.feed_id, transit.stop_count, transit.connection_count, transit.bundle_path
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
            transit_feeds: args.transit_feeds,
        },
    ))
}

fn emit_structured_failure_diagnostics(error: &anyhow::Error) {
    let Some(failure) = analysis_failure(error) else {
        return;
    };

    eprintln!("analysis failed: {}", failure.message);
    for diagnostic in &failure.diagnostics {
        eprintln!(
            "  - [{:?}] {:?}: {}",
            diagnostic.severity, diagnostic.code, diagnostic.message
        );
        if !diagnostic.point_ids.is_empty() {
            eprintln!("    points: {}", diagnostic.point_ids.join(", "));
        }
        if !diagnostic.component_ids.is_empty() {
            eprintln!(
                "    components: {}",
                diagnostic
                    .component_ids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        for suggestion in &diagnostic.suggested_actions {
            eprintln!("    suggestion: {suggestion}");
        }
    }
}

fn load_route_execution_inputs(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
) -> Result<(
    TopologyBundle,
    Option<DatasetAccelerationBundle>,
    CompiledProfileManifest,
    CompiledProfileBundle,
)> {
    let dataset_manifest = read_dataset_manifest(paths, dataset_id)
        .with_context(|| format!("reading dataset manifest for '{dataset_id}'"))?;
    let topology_ref = dataset_manifest
        .topology_bundle
        .clone()
        .context("dataset is missing a topology bundle; run `netweevil dataset import` first")?;
    let topology: TopologyBundle = read_topology_bundle(&topology_ref.path)
        .with_context(|| format!("reading topology bundle {}", topology_ref.path))?;
    let acceleration = dataset_manifest
        .acceleration_bundle
        .as_ref()
        .map(|bundle_ref| {
            read_acceleration_bundle(&bundle_ref.path)
                .with_context(|| format!("reading acceleration bundle {}", bundle_ref.path))
        })
        .transpose()?;
    let wanted_hash = profile.fingerprint()?;
    let compiled_manifest = read_compiled_profile_manifests(paths)?
        .into_iter()
        .find(|manifest| {
            manifest.dataset_id.0 == dataset_id && manifest.profile_hash == wanted_hash
        })
        .context("compiled profile bundle not found; run `netweevil profile compile` first")?;
    let compiled_bundle: CompiledProfileBundle =
        read_compiled_profile_bundle(&compiled_manifest.bundle.path).with_context(|| {
            format!(
                "reading compiled profile bundle {}",
                compiled_manifest.bundle.path
            )
        })?;
    Ok((topology, acceleration, compiled_manifest, compiled_bundle))
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
        executable: "netweevil".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_commit: option_env!("NETWEEVIL_GIT_COMMIT").map(ToString::to_string),
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
            route_summary: "Exact forward A* shortest-path search over the compiled directed edge graph with persisted topology spatial indexing for snapping and multi-edge turn-restriction sequences.",
            batch_engine: "astar_exact_multi_edge_turns_batch_reuse",
            batch_summary: "Exact forward A* shortest-path searches over the compiled directed edge graph with persisted topology spatial indexing for snapping and multi-edge turn-restriction sequences, with per-batch snap reuse and duplicate snapped-pair solve reuse for OD and matrix execution.",
            acceleration: "spatial_index+a_star+turn_automaton",
        }
    } else {
        EngineDescription {
            route_engine: "bidirectional_exact_pairwise_turns",
            route_summary: "Exact bidirectional shortest-path search over the compiled directed edge graph with edge-phantom snapping for endpoints and pairwise turn prohibitions.",
            batch_engine: "bidirectional_exact_pairwise_turns_batch_reuse",
            batch_summary: "Exact bidirectional shortest-path searches over the compiled directed edge graph with edge-phantom snapping for endpoints and pairwise turn prohibitions, with per-batch snap reuse and duplicate snapped-pair solve reuse for OD and matrix execution.",
            acceleration: "spatial_index+edge_phantoms",
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

fn write_accessibility_result(path: &Path, result: &AccessibilityResult) -> Result<()> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("json") => write_json(path, result),
        Some(ext) if ext.eq_ignore_ascii_case("csv") => write_accessibility_csv(path, result),
        other => anyhow::bail!(
            "accessibility output extension {:?} is unsupported; use .json or .csv",
            other
        ),
    }
}

fn write_accessibility_csv(path: &Path, result: &AccessibilityResult) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let threshold_keys = result
        .thresholds_s
        .iter()
        .map(|threshold| accessibility_threshold_key(*threshold))
        .collect::<Vec<_>>();
    let mut output = String::new();
    let mut header = vec![
        "origin_id".to_string(),
        "category_id".to_string(),
        "status".to_string(),
        "outcome".to_string(),
        "fallback_used".to_string(),
        "origin_component_id".to_string(),
        "origin_hop_distance_m".to_string(),
        "origin_snap_distance_m".to_string(),
        "destination_count".to_string(),
        "snapped_destination_count".to_string(),
        "nearest_destination_id".to_string(),
        "nearest_travel_time_s".to_string(),
        "nearest_destination_snap_distance_m".to_string(),
    ];
    header.extend(
        threshold_keys
            .iter()
            .map(|threshold| format!("count_within_{threshold}s")),
    );
    header.push("error".to_string());
    push_csv_line(&mut output, &header);

    for row in &result.rows {
        let mut values = vec![
            row.origin_id.clone(),
            row.category_id.clone(),
            batch_status_name(row.status).to_string(),
            outcome_name(row.outcome).to_string(),
            row.fallback_used.to_string(),
            optional_csv(row.origin_component_id),
            optional_csv(row.origin_hop_distance_m),
            optional_csv(row.origin_snap_distance_m),
            row.destination_count.to_string(),
            row.snapped_destination_count.to_string(),
            row.nearest_destination_id.clone().unwrap_or_default(),
            optional_csv(row.nearest_travel_time_s),
            optional_csv(row.nearest_destination_snap_distance_m),
        ];
        values.extend(threshold_keys.iter().map(|threshold| {
            row.counts_within_threshold_s
                .get(threshold)
                .copied()
                .unwrap_or_default()
                .to_string()
        }));
        values.push(row.error.clone().unwrap_or_default());
        push_csv_line(&mut output, &values);
    }
    fs::write(path, output).with_context(|| format!("writing {}", path.display()))
}

fn accessibility_threshold_key(threshold_s: f64) -> String {
    if (threshold_s.fract()).abs() <= f64::EPSILON {
        format!("{threshold_s:.0}")
    } else {
        threshold_s.to_string()
    }
}

fn batch_status_name(status: netweevil_query::BatchItemStatus) -> &'static str {
    match status {
        netweevil_query::BatchItemStatus::Succeeded => "succeeded",
        netweevil_query::BatchItemStatus::Ignored => "ignored",
        netweevil_query::BatchItemStatus::Failed => "failed",
    }
}

fn outcome_name(outcome: netweevil_query::AnalysisOutcome) -> &'static str {
    match outcome {
        netweevil_query::AnalysisOutcome::Legal => "legal",
        netweevil_query::AnalysisOutcome::Degraded => "degraded",
        netweevil_query::AnalysisOutcome::Partial => "partial",
        netweevil_query::AnalysisOutcome::Unreachable => "unreachable",
        netweevil_query::AnalysisOutcome::NotImplemented => "not_implemented",
    }
}

fn optional_csv<T: ToString>(value: Option<T>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn push_csv_line(output: &mut String, values: &[String]) {
    output.push_str(
        &values
            .iter()
            .map(|value| csv_escape(value))
            .collect::<Vec<_>>()
            .join(","),
    );
    output.push('\n');
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
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
        AnalysisKind::ServiceArea => {
            let request_path =
                request_path.context("service-area scenario is missing a request path")?;
            let request = load_service_area_request(request_path)?;
            run_service_area_analysis(paths, dataset_id, &profile, request_path, &request, out)
        }
    }
}

fn scenario_id(index: usize, scenario: &netweevil_query::ScenarioSpec) -> String {
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
        AnalysisKind::ServiceArea => "service_area",
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
        let path = std::env::temp_dir().join(format!("netweevil-cli-{label}-{unique}"));
        fs::create_dir_all(&path).expect("temp dir created");
        path
    }
}
