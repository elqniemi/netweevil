use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use netweevil_core::{CompiledProfileBundle, DatasetAccelerationBundle, TopologyBundle};
use netweevil_persist::{
    WorkspacePaths, read_acceleration_bundle, read_compiled_profile_bundle,
    read_compiled_profile_manifests, read_dataset_manifest, read_edge_name_bundle,
    read_topology_bundle, write_json, write_run_manifest,
};
use netweevil_profile::{ProfileDocument, ReturnGeometry, load_profile};
use netweevil_query::{
    AccessibilityCategoryRequest, AccessibilityRequest, AccessibilityResult, BetweennessRequest,
    MatrixResult, OdResult, PreparedRoutingEngine, RouteBatchResult, RouteResult,
    ScenarioBatchRequest, ScenarioBatchResult, ServiceAreaResult, ServiceAreaSequenceRequest,
    ServiceAreaSequenceResult, TemporalRequestOptions, analysis_failure, execute_scenario_batch,
    load_betweenness_request, load_od_pairs, load_point_set, load_route_batch, load_route_request,
    load_scenario_batch_request, load_service_area_request, load_service_area_sequence_request,
};
use netweevil_report::{
    CompiledProfileManifest, RunKind, RunStatus, new_run_manifest, write_betweenness_result,
    write_matrix_result, write_od_result, write_route_batch_result, write_route_result,
    write_service_area_result, write_service_area_sequence_result,
};
use netweevil_transit::{
    AccessMode, TransitRouteRequest, TransitRouteResult, TransitStreetAccessModel,
    load_transit_request, read_transit_bundle,
};
use serde::{Deserialize, Serialize};

use crate::transit::{
    prepare_cli_transit_street_estimator, prepare_registered_transit_router, read_transit_manifest,
};
use crate::{algorithm_info, software_info};

#[derive(Debug)]
pub(crate) struct StoredRun {
    pub(crate) result_path: PathBuf,
    pub(crate) manifest_path: PathBuf,
    pub(crate) summary: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy)]
struct EngineDescription {
    route_engine: &'static str,
    route_summary: &'static str,
    batch_engine: &'static str,
    batch_summary: &'static str,
    acceleration: &'static str,
}

#[derive(Subcommand, Debug)]
pub(crate) enum AnalyzeCommand {
    Route(RouteArgs),
    RouteBatch(RouteBatchArgs),
    Od(OdArgs),
    Matrix(MatrixArgs),
    Accessibility(AccessibilityArgs),
    ServiceArea(ServiceAreaArgs),
    ServiceAreaSequence(ServiceAreaSequenceArgs),
    Betweenness(BetweennessArgs),
    ScenarioBatch(ScenarioBatchArgs),
    TransitRoute(TransitRouteArgs),
    TransitBatch(TransitBatchArgs),
}

#[derive(Args, Debug)]
pub(crate) struct RouteArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long)]
    request: PathBuf,
    #[command(flatten)]
    temporal: TemporalOverrideArgs,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct RouteBatchArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long)]
    requests: PathBuf,
    #[command(flatten)]
    temporal: TemporalOverrideArgs,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct OdArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long)]
    pairs: PathBuf,
    #[command(flatten)]
    temporal: TemporalOverrideArgs,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct MatrixArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long)]
    origins: PathBuf,
    #[arg(long)]
    destinations: PathBuf,
    #[command(flatten)]
    temporal: TemporalOverrideArgs,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct AccessibilityArgs {
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
    #[command(flatten)]
    temporal: TemporalOverrideArgs,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct ServiceAreaArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long)]
    request: PathBuf,
    #[command(flatten)]
    temporal: TemporalOverrideArgs,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct ServiceAreaSequenceArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long)]
    request: PathBuf,
    #[command(flatten)]
    temporal: TemporalOverrideArgs,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct BetweennessArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long)]
    request: PathBuf,
    #[command(flatten)]
    temporal: TemporalOverrideArgs,
    #[arg(long)]
    out: Option<PathBuf>,
}

/// Command-line overrides for the temporal fields embedded in request files.
///
/// Optional scalar flags replace only their matching request-file field. CLI
/// overlays are appended so request-file overlays remain active and the later
/// CLI entries retain their precedence.
#[derive(Args, Debug, Clone, Default)]
pub(crate) struct TemporalOverrideArgs {
    /// Override the request departure time (RFC 3339).
    #[arg(long)]
    departure_time: Option<String>,
    /// Override the request scenario file.
    #[arg(long)]
    scenario: Option<PathBuf>,
    /// Override the request holiday calendar file.
    #[arg(long)]
    holiday_calendar: Option<PathBuf>,
    /// Append a temporal overlay file; may be specified more than once.
    #[arg(long = "overlay")]
    overlays: Vec<PathBuf>,
}

impl TemporalOverrideArgs {
    fn apply_to(&self, temporal: &mut TemporalRequestOptions) {
        if let Some(departure_time) = &self.departure_time {
            temporal.departure_time = Some(departure_time.clone());
            // `departure_time` and `arrive_by` are alternative time anchors;
            // an explicit CLI departure must be able to override either form.
            temporal.arrive_by = None;
        }
        self.apply_non_departure_to(temporal);
    }

    fn apply_non_departure_to(&self, temporal: &mut TemporalRequestOptions) {
        if let Some(scenario) = &self.scenario {
            temporal.scenario = Some(scenario.clone());
        }
        if let Some(holiday_calendar) = &self.holiday_calendar {
            temporal.holiday_calendar = Some(holiday_calendar.clone());
        }
        temporal.overlays.extend(self.overlays.iter().cloned());
    }

    fn apply_to_service_area_sequence(&self, request: &mut ServiceAreaSequenceRequest) {
        self.apply_non_departure_to(&mut request.request.temporal);
        if let Some(departure_time) = &self.departure_time {
            request.departure_times = vec![departure_time.clone()];
            request.start_time = None;
            request.end_time = None;
            request.step_s = None;
        }
    }
}

fn apply_matrix_temporal_overrides(
    overrides: &TemporalOverrideArgs,
    origins: &mut netweevil_query::PointSetDocument,
    destinations: &mut netweevil_query::PointSetDocument,
) {
    match (
        origins.temporal.requires_exact_labels(),
        destinations.temporal.requires_exact_labels(),
    ) {
        (true, true) => {
            overrides.apply_to(&mut origins.temporal);
            overrides.apply_to(&mut destinations.temporal);
        }
        (false, true) => overrides.apply_to(&mut destinations.temporal),
        _ => overrides.apply_to(&mut origins.temporal),
    }
}

#[derive(Args, Debug)]
pub(crate) struct ScenarioBatchArgs {
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
pub(crate) struct TransitRouteArgs {
    #[arg(long)]
    feed: String,
    #[arg(long)]
    request: PathBuf,
    /// Street dataset used when the request selects network access/egress.
    #[arg(long)]
    street_dataset: Option<String>,
    /// Foot profile used when the request selects network access/egress.
    #[arg(long)]
    street_profile: Option<PathBuf>,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct TransitBatchArgs {
    #[arg(long)]
    feed: String,
    #[arg(long)]
    requests: PathBuf,
    /// Street dataset used by network access/egress requests in the batch.
    #[arg(long)]
    street_dataset: Option<String>,
    /// Foot profile used by network access/egress requests in the batch.
    #[arg(long)]
    street_profile: Option<PathBuf>,
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

pub(crate) fn analyze_route(paths: &WorkspacePaths, args: RouteArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let mut request = load_route_request(&args.request)?;
    args.temporal.apply_to(&mut request.temporal);
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

pub(crate) fn analyze_route_batch(paths: &WorkspacePaths, args: RouteBatchArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let mut requests = load_route_batch(&args.requests)?;
    for entry in &mut requests.requests {
        args.temporal.apply_to(&mut entry.request.temporal);
    }
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

pub(crate) fn analyze_od(paths: &WorkspacePaths, args: OdArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let mut request = load_od_pairs(&args.pairs)?;
    args.temporal.apply_to(&mut request.temporal);
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

pub(crate) fn analyze_matrix(paths: &WorkspacePaths, args: MatrixArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let mut origins = load_point_set(&args.origins)?;
    let mut destinations = load_point_set(&args.destinations)?;
    apply_matrix_temporal_overrides(&args.temporal, &mut origins, &mut destinations);
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

pub(crate) fn analyze_accessibility(paths: &WorkspacePaths, args: AccessibilityArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let mut origins = load_point_set(&args.origins)?;
    args.temporal.apply_to(&mut origins.temporal);
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

pub(crate) fn analyze_service_area(paths: &WorkspacePaths, args: ServiceAreaArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let mut request = load_service_area_request(&args.request)?;
    args.temporal.apply_to(&mut request.temporal);
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

pub(crate) fn analyze_service_area_sequence(
    paths: &WorkspacePaths,
    args: ServiceAreaSequenceArgs,
) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let mut request = load_service_area_sequence_request(&args.request)?;
    args.temporal.apply_to_service_area_sequence(&mut request);
    if args.out.as_deref().is_some_and(output_needs_geometry) {
        request.request.returns.geometry = true;
    }
    let stored = run_service_area_sequence_analysis(
        paths,
        &args.dataset,
        &profile,
        &args.request,
        &request,
        args.out,
    )?;
    println!(
        "service-area sequence result written to {}",
        stored.result_path.display()
    );
    println!("run manifest written to {}", stored.manifest_path.display());
    Ok(())
}

pub(crate) fn analyze_betweenness(paths: &WorkspacePaths, args: BetweennessArgs) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let mut request = load_betweenness_request(&args.request)?;
    args.temporal.apply_to(&mut request.temporal);
    let stored = run_betweenness_analysis(
        paths,
        &args.dataset,
        &profile,
        &args.request,
        &request,
        args.out,
    )?;
    println!(
        "betweenness result written to {}",
        stored.result_path.display()
    );
    println!("run manifest written to {}", stored.manifest_path.display());
    Ok(())
}

pub(crate) fn analyze_scenario_batch(
    paths: &WorkspacePaths,
    args: ScenarioBatchArgs,
) -> Result<()> {
    let profile = load_profile(&args.profile)?;
    profile.validate()?;
    let request = load_scenario_batch_request(&args.request)?;
    let stored = run_scenario_batch_analysis(
        paths,
        &args.dataset,
        &profile,
        &args.request,
        &request,
        args.out,
    )?;
    println!(
        "scenario-batch result written to {}",
        stored.result_path.display()
    );
    println!("run manifest written to {}", stored.manifest_path.display());
    Ok(())
}

pub(crate) fn analyze_transit_route(paths: &WorkspacePaths, args: TransitRouteArgs) -> Result<()> {
    let manifest = read_transit_manifest(paths, &args.feed)?;
    let bundle = read_transit_bundle(&manifest.bundle_path)
        .with_context(|| format!("reading transit bundle {}", manifest.bundle_path))?;
    let request = load_transit_request(&args.request)?;
    let street_estimator = if validate_network_street_access_modes(&request)? {
        let (Some(dataset_id), Some(profile_path)) = (
            args.street_dataset.as_deref(),
            args.street_profile.as_deref(),
        ) else {
            anyhow::bail!(
                "transit route '{}' sets modes.street_access=network; provide both --street-dataset and --street-profile",
                request.route_id
            );
        };
        Some(prepare_cli_transit_street_estimator(
            paths,
            dataset_id,
            profile_path,
            &bundle,
        )?)
    } else {
        None
    };
    let router = prepare_registered_transit_router(&manifest, bundle)?;
    let result = router
        .execute_route_with_street_estimator(
            &request,
            street_estimator
                .as_ref()
                .map(|estimator| estimator as &dyn netweevil_transit::StreetTimeEstimator),
        )
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

pub(crate) fn analyze_transit_batch(paths: &WorkspacePaths, args: TransitBatchArgs) -> Result<()> {
    let manifest = read_transit_manifest(paths, &args.feed)?;
    let bundle = read_transit_bundle(&manifest.bundle_path)
        .with_context(|| format!("reading transit bundle {}", manifest.bundle_path))?;
    let document = load_transit_batch(&args.requests)?;
    let mut first_network_request = None;
    for request in &document.requests {
        if validate_network_street_access_modes(request)? && first_network_request.is_none() {
            first_network_request = Some(request);
        }
    }
    let street_estimator = if let Some(request) = first_network_request {
        let (Some(dataset_id), Some(profile_path)) = (
            args.street_dataset.as_deref(),
            args.street_profile.as_deref(),
        ) else {
            anyhow::bail!(
                "transit batch route '{}' sets modes.street_access=network; provide both --street-dataset and --street-profile",
                request.route_id
            );
        };
        Some(prepare_cli_transit_street_estimator(
            paths,
            dataset_id,
            profile_path,
            &bundle,
        )?)
    } else {
        None
    };
    let router = prepare_registered_transit_router(&manifest, bundle)?;
    let mut items = Vec::new();
    let mut failures = Vec::new();
    let mut scheduled_count = 0_usize;
    let mut unreachable_count = 0_usize;
    let mut not_implemented_count = 0_usize;

    for (index, request) in document.requests.iter().enumerate() {
        match router.execute_route_with_street_estimator(
            request,
            street_estimator
                .as_ref()
                .map(|estimator| estimator as &dyn netweevil_transit::StreetTimeEstimator),
        ) {
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

fn validate_network_street_access_modes(request: &TransitRouteRequest) -> Result<bool> {
    if request.modes.street_access != TransitStreetAccessModel::Network {
        return Ok(false);
    }

    let access = request.modes.validated_access_modes()?;
    let egress = request.modes.validated_egress_modes()?;
    if access
        .iter()
        .chain(egress.iter())
        .any(|mode| *mode != AccessMode::Walk)
    {
        anyhow::bail!(
            "transit route '{}' uses modes.street_access=network, but the CLI street estimator currently supports walk-only access and egress",
            request.route_id
        );
    }
    Ok(true)
}

fn record_effective_request<T: Serialize>(
    manifest: &mut netweevil_report::RunManifest,
    request: &T,
) -> Result<()> {
    manifest.effective_request =
        Some(serde_json::to_value(request).context("serializing effective analysis request")?);
    Ok(())
}

pub(crate) fn run_route_analysis(
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
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let engine = engine_description_for_route(&prepared, &request);
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
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let engine = engine_description_for_route_batch(&prepared, &requests);

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
        if let Some(profile_id) = entry.profile_id.as_deref()
            && profile_id != profile.profile.id
        {
            anyhow::bail!(
                "route '{}' requests profile_id '{}' but CLI batch is using profile '{}'",
                entry.request.route_id,
                profile_id,
                profile.profile.id
            );
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

fn run_betweenness_analysis(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    request_path: &Path,
    request: &BetweennessRequest,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    let (topology, acceleration, compiled_manifest, compiled_bundle) =
        load_route_execution_inputs(paths, dataset_id, profile)?;
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let engine = engine_description_for_temporal_options(&prepared, &request.temporal);
    let result = prepared
        .execute_betweenness(request)
        .with_context(|| format!("executing betweenness '{}'", request.analysis_id))?;
    let mut manifest = new_run_manifest(
        RunKind::Betweenness,
        dataset_id.to_string(),
        profile,
        request_path.display().to_string(),
        RunStatus::Succeeded,
        format!(
            "Betweenness '{}' routed {} of {} weighted OD pairs and scored {} directed edges.",
            result.analysis_id,
            result.routed_pair_count,
            result.requested_pair_count,
            result.edges.len(),
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
        algorithm_info(
            if request.temporal.is_temporal() {
                "time_dependent_exact_edge_dijkstra"
            } else {
                "demand_weighted_one_to_many_edge_trees"
            },
            engine.acceleration,
        ),
        "Weighted origin-to-destination shortest paths are accumulated on every traversed directed edge; static unrestricted origins reuse one-to-many trees, while temporal or sequence-restricted requests use exact pairwise routing.",
    )?;
    record_effective_request(&mut manifest, request)?;
    let result_path = out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("{}-result.json", manifest.run_id))
    });
    write_betweenness_result(&result_path, &result)?;
    manifest.result_path = Some(result_path.display().to_string());
    let manifest_path = write_run_manifest(paths, &manifest)?;
    Ok(StoredRun {
        result_path,
        manifest_path,
        summary: Some(serde_json::json!({
            "analysis_id": result.analysis_id,
            "requested_pair_count": result.requested_pair_count,
            "routed_pair_count": result.routed_pair_count,
            "unreachable_pair_count": result.unreachable_pair_count,
            "routed_demand": result.routed_demand,
            "edge_count": result.edges.len(),
            "time_dependent": result.time_dependent,
        })),
    })
}

fn run_scenario_batch_analysis(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    request_path: &Path,
    request: &ScenarioBatchRequest,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    let (topology, acceleration, compiled_manifest, compiled_bundle) =
        load_route_execution_inputs(paths, dataset_id, profile)?;
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let result = execute_scenario_batch(&prepared, request)
        .with_context(|| format!("executing scenario batch '{}'", request.batch_id))?;
    let analysis_count = request.routes.len()
        + request.service_areas.len()
        + request.accessibility.len()
        + request.od.len()
        + request.matrices.len()
        + request.betweenness.len();
    let failure_count = scenario_batch_failure_count(&result);
    let mut manifest = new_run_manifest(
        RunKind::ScenarioBatch,
        dataset_id.to_string(),
        profile,
        request_path.display().to_string(),
        RunStatus::Succeeded,
        format!(
            "Scenario batch '{}' evaluated {} analysis request(s) against {} scenario(s), with {} individual execution failure(s).",
            result.batch_id,
            analysis_count,
            result.scenarios.len(),
            failure_count,
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
        algorithm_info(
            "scenario_batch_exact_overlay_replay",
            "static_baseline+exact_temporal_scenarios",
        ),
        "Each analysis is executed once as a baseline and once per scenario overlay; route time, disconnected demand, reachable network, and accessibility changes are diffed against the baseline.",
    )?;
    record_effective_request(&mut manifest, request)?;
    let result_path = out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("{}-result.json", manifest.run_id))
    });
    if result_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| !extension.eq_ignore_ascii_case("json"))
    {
        anyhow::bail!("scenario-batch output must be JSON; use a .json path");
    }
    write_json(&result_path, &result)?;
    manifest.result_path = Some(result_path.display().to_string());
    let manifest_path = write_run_manifest(paths, &manifest)?;
    Ok(StoredRun {
        result_path,
        manifest_path,
        summary: Some(serde_json::json!({
            "batch_id": result.batch_id,
            "scenario_count": result.scenarios.len(),
            "analysis_count": analysis_count,
            "failure_count": failure_count,
        })),
    })
}

fn scenario_batch_failure_count(result: &ScenarioBatchResult) -> usize {
    fn snapshot_failure_count(snapshot: &netweevil_query::ScenarioAnalysisSnapshot) -> usize {
        snapshot
            .routes
            .iter()
            .filter(|item| item.error.is_some())
            .count()
            + snapshot
                .service_areas
                .iter()
                .filter(|item| item.error.is_some())
                .count()
            + snapshot
                .accessibility
                .iter()
                .filter(|item| item.error.is_some())
                .count()
            + snapshot
                .od
                .iter()
                .filter(|item| item.error.is_some())
                .count()
            + snapshot
                .matrices
                .iter()
                .filter(|item| item.error.is_some())
                .count()
            + snapshot
                .betweenness
                .iter()
                .filter(|item| item.error.is_some())
                .count()
    }

    snapshot_failure_count(&result.baseline)
        + result
            .scenarios
            .iter()
            .map(|scenario| snapshot_failure_count(&scenario.analyses))
            .sum::<usize>()
}

pub(crate) fn run_od_analysis(
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
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let engine = engine_description_for_temporal_options(&prepared, &request.temporal);
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

pub(crate) fn run_matrix_analysis(
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
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let engine = if origins.temporal.is_temporal() {
        engine_description_for_temporal_options(&prepared, &origins.temporal)
    } else {
        engine_description_for_temporal_options(&prepared, &destinations.temporal)
    };
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
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let mut engine = engine_description_for_temporal_options(&prepared, &request.origins.temporal);
    if request.origins.temporal.is_temporal() {
        engine.batch_engine = "time_dependent_bounded_accessibility";
        engine.batch_summary = "Exact time-dependent bounded one-to-many expansion evaluates edge-entry rules, scenario overrides, waiting, and overlays before reducing reachable destination categories.";
    }
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

pub(crate) fn run_service_area_analysis(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    request_path: &Path,
    request: &netweevil_query::ServiceAreaRequest,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    let (topology, acceleration, compiled_manifest, compiled_bundle) =
        load_route_execution_inputs(paths, dataset_id, profile)?;
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let mut engine = engine_description_for_temporal_options(&prepared, &request.temporal);
    if request.temporal.is_temporal() {
        engine.batch_engine = "time_dependent_exact_service_area";
        engine.batch_summary = "Exact time-dependent one-to-many edge expansion evaluates rules, scenario overrides, waiting, and overlays at edge entry for every reachable service-area segment.";
    }
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

fn run_service_area_sequence_analysis(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    request_path: &Path,
    request: &ServiceAreaSequenceRequest,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    let (topology, acceleration, compiled_manifest, compiled_bundle) =
        load_route_execution_inputs(paths, dataset_id, profile)?;
    let prepared = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;
    let result = prepared
        .execute_service_area_sequence(request)
        .with_context(|| format!("executing service-area sequence '{}'", request.sequence_id))?;
    store_service_area_sequence_run(
        paths,
        dataset_id,
        profile,
        request_path,
        request,
        &result,
        &compiled_manifest,
        out,
    )
}

fn store_service_area_sequence_run(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    request_path: &Path,
    request: &ServiceAreaSequenceRequest,
    result: &ServiceAreaSequenceResult,
    compiled_manifest: &CompiledProfileManifest,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    let feature_count = result
        .frames
        .iter()
        .map(|frame| frame.result.features.len())
        .sum::<usize>();
    let mut manifest = new_run_manifest(
        RunKind::ServiceAreaSequence,
        dataset_id.to_string(),
        profile,
        request_path.display().to_string(),
        RunStatus::Succeeded,
        format!(
            "Service-area sequence '{}' completed {} temporal frame(s) with {} output feature(s).",
            result.sequence_id, result.frame_count, feature_count
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
        algorithm_info(
            "time_dependent_exact_fifo_service_area_sequence",
            "none_temporal_exact",
        ),
        "The same service-area request was replayed at each requested departure datetime with exact edge-entry temporal rules; GeoJSON frames carry departure_time for QGIS instant-mode animation.",
    )?;
    record_effective_request(&mut manifest, request)?;
    manifest.connectivity_policy = Some(request.request.connectivity.clone());
    manifest.fallback_policy = Some(request.request.fallback.clone());
    let result_path = out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("{}-result.json", manifest.run_id))
    });
    write_service_area_sequence_result(&result_path, result)?;
    manifest.result_path = Some(result_path.display().to_string());
    let manifest_path = write_run_manifest(paths, &manifest)?;
    Ok(StoredRun {
        result_path,
        manifest_path,
        summary: Some(serde_json::json!({
            "sequence_id": result.sequence_id,
            "frame_count": result.frame_count,
            "feature_count": feature_count,
        })),
    })
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
        algorithm_info(engine.route_engine, engine.acceleration),
        engine.route_summary,
    )?;
    record_effective_request(&mut manifest, request)?;
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
        algorithm_info(engine.route_engine, engine.acceleration),
        format!(
            "{} Route-batch execution reused one prepared routing engine for {} route requests and wrote one consolidated result file.",
            engine.route_summary, result.route_count,
        ),
    )?;
    record_effective_request(&mut manifest, requests)?;
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
        algorithm_info(engine.batch_engine, engine.acceleration),
        engine.batch_summary,
    )?;
    record_effective_request(&mut manifest, request)?;
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
        algorithm_info(engine.batch_engine, engine.acceleration),
        engine.batch_summary,
    )?;
    manifest.effective_request = Some(serde_json::json!({
        "origins": origins,
        "destinations": destinations,
    }));
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
        algorithm_info(
            if request.origins.temporal.is_temporal() {
                engine.batch_engine
            } else {
                "bounded_single_origin_accessibility"
            },
            engine.acceleration,
        ),
        format!(
            "{} Accessibility execution performs one exact bounded legal-network expansion per unique snapped origin up to {:.0}s, snaps all destinations once, and reduces each category to nearest destination and threshold counts without materializing all OD cells.",
            engine.batch_summary, result.max_travel_time_s
        ),
    )?;
    record_effective_request(&mut manifest, request)?;
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
        algorithm_info(engine.batch_engine, engine.acceleration),
        format!(
            "{} Service-area execution reuses one exact legal-network expansion per unique snapped origin and threshold metric, then emits cumulative or ring bands as network and/or polygon outputs.",
            engine.batch_summary
        ),
    )?;
    record_effective_request(&mut manifest, request)?;
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
                "reading compiled profile bundle {}; if the dataset was re-imported with a newer format, run `netweevil profile compile` again",
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

fn engine_description_from(prepared: &PreparedRoutingEngine) -> EngineDescription {
    let effective = prepared.effective_engine_description(netweevil_query::EngineMode::Auto);
    let (route_summary, batch_summary) = match effective.route_engine {
        "cch_with_restriction_sequence_validation" => (
            "Customizable contraction hierarchy query over the compiled directed edge graph with edge-phantom snapping; candidate paths are validated against multi-edge turn-restriction sequences with an exact automaton fallback.",
            "Customizable contraction hierarchy queries with per-batch snap reuse and duplicate snapped-pair solve reuse; candidate paths are validated against multi-edge turn-restriction sequences with an exact automaton fallback.",
        ),
        "accelerated_pairwise_turns" => (
            "Customizable contraction hierarchy query over the compiled directed edge graph with edge-phantom snapping for endpoints and pairwise turn prohibitions.",
            "Customizable contraction hierarchy queries with per-batch snap reuse and duplicate snapped-pair solve reuse for OD and matrix execution.",
        ),
        "astar_exact_multi_edge_turns" => (
            "Exact forward A* shortest-path search over the compiled directed edge graph with persisted topology spatial indexing for snapping and multi-edge turn-restriction sequences.",
            "Exact forward A* shortest-path searches over the compiled directed edge graph with persisted topology spatial indexing for snapping and multi-edge turn-restriction sequences, with per-batch snap reuse and duplicate snapped-pair solve reuse for OD and matrix execution.",
        ),
        _ => (
            "Exact bidirectional shortest-path search over the compiled directed edge graph with edge-phantom snapping for endpoints and pairwise turn prohibitions.",
            "Exact bidirectional shortest-path searches over the compiled directed edge graph with edge-phantom snapping for endpoints and pairwise turn prohibitions, with per-batch snap reuse and duplicate snapped-pair solve reuse for OD and matrix execution.",
        ),
    };
    EngineDescription {
        route_engine: effective.route_engine,
        route_summary,
        batch_engine: effective.batch_engine,
        batch_summary,
        acceleration: effective.acceleration,
    }
}

fn engine_description_for_route(
    prepared: &PreparedRoutingEngine,
    request: &netweevil_query::RouteRequest,
) -> EngineDescription {
    let mut description = engine_description_from(prepared);
    if !request.temporal.constraints.is_empty() || request.temporal.pareto.is_some() {
        description.route_engine = "component_nondominated_label_setting";
        description.route_summary = "Exact nondominated label-setting over generalized cost and named component dimensions, with hard component budgets and a per-state label guard.";
        description.acceleration = "spatial_index+edge_phantoms+turn_automaton";
    } else if request.temporal.is_temporal() {
        description.route_engine = "time_dependent_nondominated_label_setting";
        description.route_summary = "Exact time-dependent nondominated label-setting evaluates edge rules, scenario overrides, and temporal overlays at edge entry time.";
        description.acceleration = "spatial_index+edge_phantoms+turn_automaton";
    }
    description
}

fn engine_description_for_temporal_options(
    prepared: &PreparedRoutingEngine,
    temporal: &netweevil_query::TemporalRequestOptions,
) -> EngineDescription {
    let mut description = engine_description_from(prepared);
    if !temporal.constraints.is_empty() || temporal.pareto.is_some() {
        description.route_engine = "component_nondominated_label_setting";
        description.route_summary = "Exact nondominated label-setting over generalized cost and named component dimensions, with hard component budgets and a per-state label guard.";
        description.batch_engine = "component_nondominated_pairwise_label_setting";
        description.batch_summary = "Exact pairwise nondominated label-setting over generalized cost, temporal state, and named component dimensions; label guards fail rather than approximate.";
        description.acceleration = "spatial_index+edge_phantoms+turn_automaton";
    } else if temporal.is_temporal() {
        description.route_engine = "time_dependent_nondominated_label_setting";
        description.route_summary = "Exact time-dependent nondominated label-setting evaluates edge rules, scenario overrides, and temporal overlays at edge entry time.";
        description.batch_engine = "time_dependent_exact_pairwise_label_setting";
        description.batch_summary = "Exact time-dependent pairwise label-setting evaluates edge-entry rules, scenario overrides, waiting, and overlays independently for each requested pair.";
        description.acceleration = "spatial_index+edge_phantoms+turn_automaton";
    }
    description
}

fn engine_description_for_route_batch(
    prepared: &PreparedRoutingEngine,
    requests: &netweevil_query::RouteBatchDocument,
) -> EngineDescription {
    let has_component_search = requests.requests.iter().any(|entry| {
        !entry.request.temporal.constraints.is_empty() || entry.request.temporal.pareto.is_some()
    });
    let has_temporal_search = requests
        .requests
        .iter()
        .any(|entry| entry.request.temporal.is_temporal());
    let has_static_search = requests
        .requests
        .iter()
        .any(|entry| !entry.request.temporal.is_temporal());
    let representative = requests.requests.iter().find(|entry| {
        if has_component_search {
            !entry.request.temporal.constraints.is_empty()
                || entry.request.temporal.pareto.is_some()
        } else {
            entry.request.temporal.is_temporal()
        }
    });
    let mut description = representative.map_or_else(
        || engine_description_from(prepared),
        |entry| engine_description_for_route(prepared, &entry.request),
    );
    if has_static_search && has_temporal_search {
        description.route_engine = "mixed_static_and_exact_temporal_dispatch";
        description.route_summary = "Each route is dispatched by its effective request: static routes use the prepared accelerated engine, while temporal/component routes use exact label-setting with edge-entry evaluation.";
        description.acceleration = "mixed_cch_static+exact_temporal";
    }
    description
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn parses_repeatable_temporal_analyze_overrides() {
        let cli = crate::Cli::try_parse_from([
            "netweevil",
            "analyze",
            "route",
            "--dataset",
            "hk-pedestrian",
            "--profile",
            "pedestrian.yml",
            "--request",
            "route.json",
            "--departure-time",
            "2026-07-10T09:55:00+08:00",
            "--scenario",
            "central.yml",
            "--holiday-calendar",
            "holidays.yml",
            "--overlay",
            "shade.csv",
            "--overlay",
            "heat.csv",
        ])
        .expect("temporal route options parse");
        let crate::Command::Analyze {
            command: AnalyzeCommand::Route(args),
        } = cli.command
        else {
            panic!("expected analyze route command");
        };
        assert_eq!(
            args.temporal.departure_time.as_deref(),
            Some("2026-07-10T09:55:00+08:00")
        );
        assert_eq!(
            args.temporal.scenario.as_deref(),
            Some(Path::new("central.yml"))
        );
        assert_eq!(
            args.temporal.holiday_calendar.as_deref(),
            Some(Path::new("holidays.yml"))
        );
        assert_eq!(
            args.temporal.overlays,
            [PathBuf::from("shade.csv"), PathBuf::from("heat.csv")]
        );
    }

    #[test]
    fn temporal_overrides_preserve_unmentioned_request_fields() {
        let mut temporal = TemporalRequestOptions {
            departure_time: Some("2026-07-10T08:00:00+08:00".to_string()),
            scenario: Some(PathBuf::from("request-scenario.yml")),
            holiday_calendar: Some(PathBuf::from("request-holidays.yml")),
            overlays: vec![PathBuf::from("request-overlay.csv")],
            ..TemporalRequestOptions::default()
        };
        TemporalOverrideArgs {
            scenario: Some(PathBuf::from("cli-scenario.yml")),
            overlays: vec![
                PathBuf::from("cli-overlay-a.csv"),
                PathBuf::from("cli-overlay-b.csv"),
            ],
            ..TemporalOverrideArgs::default()
        }
        .apply_to(&mut temporal);

        assert_eq!(
            temporal.departure_time.as_deref(),
            Some("2026-07-10T08:00:00+08:00")
        );
        assert_eq!(
            temporal.scenario.as_deref(),
            Some(Path::new("cli-scenario.yml"))
        );
        assert_eq!(
            temporal.holiday_calendar.as_deref(),
            Some(Path::new("request-holidays.yml"))
        );
        assert_eq!(
            temporal.overlays,
            [
                PathBuf::from("request-overlay.csv"),
                PathBuf::from("cli-overlay-a.csv"),
                PathBuf::from("cli-overlay-b.csv"),
            ]
        );
    }

    #[test]
    fn parses_transit_network_street_inputs() {
        let cli = crate::Cli::try_parse_from([
            "netweevil",
            "analyze",
            "transit-route",
            "--feed",
            "hk-mtr",
            "--request",
            "transit.json",
            "--street-dataset",
            "hk-pedestrian",
            "--street-profile",
            "pedestrian.yml",
        ])
        .expect("transit street inputs parse");
        let crate::Command::Analyze {
            command: AnalyzeCommand::TransitRoute(args),
        } = cli.command
        else {
            panic!("expected analyze transit-route command");
        };
        assert_eq!(args.street_dataset.as_deref(), Some("hk-pedestrian"));
        assert_eq!(
            args.street_profile.as_deref(),
            Some(Path::new("pedestrian.yml"))
        );
    }

    #[test]
    fn parses_transit_batch_network_street_inputs() {
        let cli = crate::Cli::try_parse_from([
            "netweevil",
            "analyze",
            "transit-batch",
            "--feed",
            "hk-mtr",
            "--requests",
            "transit-batch.json",
            "--street-dataset",
            "hk-pedestrian",
            "--street-profile",
            "step-free.yml",
        ])
        .expect("transit batch street inputs parse");
        let crate::Command::Analyze {
            command: AnalyzeCommand::TransitBatch(args),
        } = cli.command
        else {
            panic!("expected analyze transit-batch command");
        };
        assert_eq!(args.street_dataset.as_deref(), Some("hk-pedestrian"));
        assert_eq!(
            args.street_profile.as_deref(),
            Some(Path::new("step-free.yml"))
        );
    }

    #[test]
    fn cli_network_street_estimator_rejects_non_walk_modes() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut request = load_transit_request(
            repository.join("examples/requests/transit_openov_groningen.json"),
        )
        .expect("transit request loads");
        request.modes.street_access = TransitStreetAccessModel::Network;
        assert!(
            validate_network_street_access_modes(&request)
                .expect("walk-only network modes are valid")
        );

        request.modes.access = vec![AccessMode::Bicycle];
        request.modes.egress = vec![AccessMode::Bicycle];
        let error = validate_network_street_access_modes(&request)
            .expect_err("bicycle network modes must be rejected");
        assert!(error.to_string().contains("walk-only"));
    }

    #[test]
    fn hong_kong_escalator_fixture_uses_materialized_source_feature() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mapping = netweevil_ingest::load_gpkg_mapping(
            repository.join("examples/ingest/hong_kong_pedestrian_mapping.yml"),
        )
        .expect("Hong Kong mapping loads");
        assert_eq!(mapping.materialize_both_directions_for.len(), 33);
        assert!(
            mapping
                .materialize_both_directions_for
                .contains(&250_009_423)
        );

        let scenario = netweevil_query::load_scenario_overlay(
            repository.join("examples/scenarios/hong_kong_central_escalator_example.yml"),
        )
        .expect("Central-Mid-Levels scenario loads");
        assert_eq!(scenario.features.len(), 1);
        assert_eq!(scenario.features[0].source_feature_id, 250_009_423);
        assert!(scenario.features[0].replace_rules);
    }
}
