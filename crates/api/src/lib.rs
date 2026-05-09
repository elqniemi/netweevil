use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, bail};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use netan_core::{
    CacheBundleId, CompiledProfileBundle, ConnectedComponentsMeta, DatasetAccelerationBundle,
    DatasetId, TopologyBounds, TopologyBundle,
};
use netan_persist::{
    WorkspacePaths, read_acceleration_bundle, read_compiled_profile_bundle,
    read_compiled_profile_manifests, read_dataset_manifest, read_edge_name_bundle,
    read_topology_bundle, write_compiled_profile_bundle, write_compiled_profile_manifest,
};
use netan_profile::{
    ProfileDocument, ReturnGeometry, compile_profile_bundle_with_acceleration, load_profile,
};
use netan_query::{
    AnalysisDiagnostic, EffectiveEngineDescription, EngineMode, MatrixResult, OdPairsDocument,
    OdResult, PointSetDocument, PreparedRoutingEngine, RouteRequest, RouteResult,
    ServiceAreaRequest, ServiceAreaResult, analysis_failure,
};
use netan_report::{BundleRef, CompiledProfileManifest, DatasetManifest, now_rfc3339};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct ApiServeOptions {
    pub bind: SocketAddr,
    pub dataset_id: String,
    pub default_profile: PathBuf,
    pub profiles: Vec<PathBuf>,
}

#[derive(Clone)]
struct ApiState {
    service: Arc<ServiceRuntime>,
}

struct ServiceRuntime {
    workspace_root: PathBuf,
    dataset_manifest: DatasetManifest,
    topology: Arc<TopologyBundle>,
    edge_names: OnceLock<Option<Arc<[String]>>>,
    routing_workers: Arc<Semaphore>,
    default_profile_id: String,
    profiles: BTreeMap<String, LoadedProfile>,
    capabilities: ServiceCapabilities,
    engine: EngineDescription,
}

struct LoadedProfile {
    source_path: PathBuf,
    document: ProfileDocument,
    manifest: CompiledProfileManifest,
    engine: Arc<PreparedRoutingEngine>,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct EngineDescription {
    route_engine: &'static str,
    batch_engine: &'static str,
    acceleration: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct ServiceCapabilities {
    analyses: Vec<&'static str>,
    geometry: Vec<&'static str>,
    breakdown_metrics: Vec<&'static str>,
    connectivity_policies: Vec<&'static str>,
    failure_modes: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ServiceInfoResponse {
    workspace_root: String,
    dataset: DatasetInfo,
    default_profile_id: String,
    loaded_profiles: Vec<ProfileInfo>,
    capabilities: ServiceCapabilities,
    engine: EngineDescription,
}

#[derive(Debug, Serialize)]
pub struct DatasetInfo {
    dataset_id: String,
    label: String,
    source_path: String,
    source_sha256: String,
    imported_at: String,
    #[serde(default)]
    topology_bounds: Option<TopologyBounds>,
    #[serde(default)]
    node_count: Option<u64>,
    #[serde(default)]
    edge_count: Option<u64>,
    #[serde(default)]
    turn_count: Option<u64>,
    #[serde(default)]
    connected_components: Option<ConnectedComponentsMeta>,
}

#[derive(Debug, Serialize)]
pub struct ProfileInfo {
    profile_id: String,
    label: String,
    mode: String,
    defaults_pack: String,
    source_path: String,
    profile_hash: String,
    created_at: String,
    edge_count: Option<u64>,
    default_returns: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct RouteExecutionRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub engine_mode: EngineMode,
    pub request: RouteRequest,
}

#[derive(Debug, Deserialize)]
pub struct OdExecutionRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub engine_mode: EngineMode,
    pub request: OdPairsDocument,
}

#[derive(Debug, Deserialize)]
pub struct MatrixExecutionRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub engine_mode: EngineMode,
    pub request: MatrixRequest,
}

#[derive(Debug, Deserialize)]
pub struct ServiceAreaExecutionRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    pub request: ServiceAreaRequest,
}

#[derive(Debug, Deserialize)]
pub struct MatrixRequest {
    pub origins: PointSetDocument,
    pub destinations: PointSetDocument,
}

#[derive(Debug, Serialize)]
pub struct RouteExecutionResponse {
    service: ExecutionContext,
    result: RouteResult,
}

#[derive(Debug, Serialize)]
pub struct OdExecutionResponse {
    service: ExecutionContext,
    result: OdResult,
}

#[derive(Debug, Serialize)]
pub struct MatrixExecutionResponse {
    service: ExecutionContext,
    result: MatrixResult,
}

#[derive(Debug, Serialize)]
pub struct ServiceAreaExecutionResponse {
    service: ExecutionContext,
    result: ServiceAreaResult,
}

#[derive(Debug, Deserialize, Default)]
struct ResponseFormatQuery {
    format: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExecutionContext {
    dataset_id: String,
    profile_id: String,
    profile_hash: String,
    route_engine: String,
    batch_engine: String,
    acceleration: String,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    diagnostics: Vec<AnalysisDiagnostic>,
}

struct ApiError {
    status: StatusCode,
    message: String,
    diagnostics: Vec<AnalysisDiagnostic>,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            diagnostics: Vec::new(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
            diagnostics: Vec::new(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
            diagnostics: Vec::new(),
        }
    }

    fn from_execution_error(error: anyhow::Error) -> Self {
        if let Some(failure) = analysis_failure(&error) {
            Self {
                status: StatusCode::BAD_REQUEST,
                message: failure.message.clone(),
                diagnostics: failure.diagnostics.clone(),
            }
        } else {
            Self::bad_request(error.to_string())
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: self.message,
                diagnostics: self.diagnostics,
            }),
        )
            .into_response()
    }
}

pub async fn serve(paths: WorkspacePaths, options: ApiServeOptions) -> Result<()> {
    let state = ApiState {
        service: Arc::new(load_service_runtime(&paths, &options)?),
    };
    let app = router(state);

    info!(
        bind = %options.bind,
        dataset_id = %options.dataset_id,
        default_profile = %options.default_profile.display(),
        "starting netan api"
    );

    let listener = TcpListener::bind(options.bind)
        .await
        .with_context(|| format!("binding API listener on {}", options.bind))?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("running API server")
}

fn router(state: ApiState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/v1/service", get(service_info))
        .route("/v1/profiles", get(list_profiles))
        .route("/v1/profiles/{profile_id}", get(get_profile))
        .route("/v1/route", post(route_handler))
        .route("/v1/od", post(od_handler))
        .route("/v1/matrix", post(matrix_handler))
        .route("/v1/service-area", post(service_area_handler))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn load_service_runtime(
    paths: &WorkspacePaths,
    options: &ApiServeOptions,
) -> Result<ServiceRuntime> {
    info!(dataset_id = %options.dataset_id, "loading dataset manifest");
    let dataset_manifest = read_dataset_manifest(paths, &options.dataset_id)
        .with_context(|| format!("reading dataset manifest for '{}'", options.dataset_id))?;
    let topology_ref = dataset_manifest
        .topology_bundle
        .clone()
        .context("dataset is missing a topology bundle; run `netan dataset import` first")?;

    info!(
        bundle = %topology_ref.path,
        "loading topology bundle"
    );
    let topology = Arc::new(
        read_topology_bundle(&topology_ref.path)
            .with_context(|| format!("reading topology bundle {}", topology_ref.path))?,
    );
    let topology_meta = dataset_manifest.topology_meta.as_ref();
    info!(
        nodes = topology_meta.map(|m| m.node_count).unwrap_or(0),
        edges = topology_meta.map(|m| m.edge_count).unwrap_or(0),
        turns = topology_meta.map(|m| m.turn_count).unwrap_or(0),
        "topology loaded"
    );
    let engine = engine_description(topology.as_ref());
    let routing_workers = Arc::new(Semaphore::new(
        std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1)
            .max(1),
    ));

    let mut loaded_profiles: BTreeMap<String, LoadedProfile> = BTreeMap::new();
    let mut requested_paths = Vec::with_capacity(options.profiles.len() + 1);
    requested_paths.push(options.default_profile.clone());
    requested_paths.extend(options.profiles.iter().cloned());

    let compiled_manifests = read_compiled_profile_manifests(paths).unwrap_or_default();
    let mut default_profile_id = None;

    for profile_path in requested_paths {
        info!(path = %profile_path.display(), "loading profile");
        let loaded = load_or_compile_profile(
            paths,
            &dataset_manifest,
            topology_ref.bundle_id.clone(),
            topology.clone(),
            &compiled_manifests,
            &profile_path,
        )?;
        let profile_id = loaded.document.profile.id.clone();
        info!(
            profile_id = %profile_id,
            hash = %loaded.manifest.profile_hash,
            edge_count = loaded.manifest.edge_count.unwrap_or(0),
            "profile ready"
        );
        if profile_path == options.default_profile {
            default_profile_id = Some(profile_id.clone());
        }
        if let Some(existing) = loaded_profiles.get(&profile_id) {
            if existing.manifest.profile_hash != loaded.manifest.profile_hash {
                bail!(
                    "profile id '{}' was loaded from multiple files with different hashes",
                    profile_id
                );
            }
            continue;
        }
        loaded_profiles.insert(profile_id, loaded);
    }

    let default_profile_id =
        default_profile_id.context("default profile could not be loaded into the API runtime")?;

    info!(
        profile_count = loaded_profiles.len(),
        default_profile = %default_profile_id,
        route_engine = %engine.route_engine,
        "network ready"
    );

    Ok(ServiceRuntime {
        workspace_root: paths.root.clone(),
        dataset_manifest,
        topology,
        edge_names: OnceLock::new(),
        routing_workers,
        default_profile_id,
        profiles: loaded_profiles,
        capabilities: ServiceCapabilities {
            analyses: vec!["route", "od", "matrix", "service_area"],
            geometry: vec!["none", "full", "segments"],
            breakdown_metrics: vec!["time_s", "distance_m"],
            connectivity_policies: vec![
                "strict",
                "ignore_unreachable",
                "hop_origin_to_nearest_reachable_component",
                "hop_destination_to_nearest_reachable_component",
                "hop_either_end",
            ],
            failure_modes: vec![
                "auto_relax_unreachable",
                "allow_reverse_oneway",
                "allow_illegal_turn",
                "ignore_turn_restrictions",
                "allow_uturn_where_normally_forbidden",
            ],
        },
        engine,
    })
}

fn load_or_compile_profile(
    paths: &WorkspacePaths,
    dataset_manifest: &DatasetManifest,
    topology_bundle_id: CacheBundleId,
    topology: Arc<TopologyBundle>,
    compiled_manifests: &[CompiledProfileManifest],
    profile_path: &Path,
) -> Result<LoadedProfile> {
    let document = load_profile(profile_path)
        .with_context(|| format!("loading {}", profile_path.display()))?;
    document.validate()?;
    let profile_hash = document.fingerprint()?;
    let dataset_acceleration: Option<Arc<DatasetAccelerationBundle>> = dataset_manifest
        .acceleration_bundle
        .as_ref()
        .map(|bundle_ref| {
            read_acceleration_bundle(&bundle_ref.path)
                .with_context(|| format!("reading acceleration bundle {}", bundle_ref.path))
                .map(Arc::new)
        })
        .transpose()?;

    let manifest = if let Some(existing) = compiled_manifests.iter().find(|manifest| {
        manifest.dataset_id.0 == dataset_manifest.dataset_id.0
            && manifest.profile_hash == profile_hash
    }) {
        existing.clone()
    } else {
        let compiled_bundle = compile_profile_bundle_with_acceleration(
            &document,
            topology.as_ref(),
            topology_bundle_id,
            dataset_manifest
                .acceleration_bundle
                .as_ref()
                .zip(dataset_acceleration.as_ref())
                .map(|(bundle_ref, bundle)| (bundle.as_ref(), bundle_ref.bundle_id.clone())),
        )
        .with_context(|| {
            format!(
                "compiling profile '{}' for dataset '{}'",
                document.profile.id, dataset_manifest.dataset_id.0
            )
        })?;
        let compile_id = format!("{}-{}", dataset_manifest.dataset_id.0, &profile_hash[..12]);
        let bundle_path = paths
            .metric_bundles_dir
            .join(format!("metric-{compile_id}.bin"));
        write_compiled_profile_bundle(&bundle_path, &compiled_bundle)?;
        let manifest = CompiledProfileManifest {
            compile_id: compile_id.clone(),
            dataset_id: DatasetId::new(dataset_manifest.dataset_id.0.clone()),
            profile_id: document.profile.id.clone(),
            profile_hash: profile_hash.clone(),
            defaults_pack: document.profile.defaults_pack.clone(),
            mode: document.profile.mode,
            created_at: now_rfc3339()?,
            topology_bundle_id: Some(compiled_bundle.source_topology_bundle_id.clone()),
            edge_count: Some(compiled_bundle.edge_metrics.len() as u64),
            bundle: BundleRef {
                bundle_id: CacheBundleId::new(format!("metric-{compile_id}")),
                path: bundle_path.display().to_string(),
            },
        };
        write_compiled_profile_manifest(paths, &manifest)?;
        manifest
    };

    let compiled_bundle: CompiledProfileBundle =
        read_compiled_profile_bundle(&manifest.bundle.path)
            .with_context(|| format!("reading compiled profile bundle {}", manifest.bundle.path))?;
    let engine = Arc::new(
        PreparedRoutingEngine::new(topology, Arc::new(compiled_bundle), dataset_acceleration)
            .with_context(|| {
                format!(
                    "preparing in-memory routing engine for profile '{}'",
                    document.profile.id
                )
            })?,
    );

    Ok(LoadedProfile {
        source_path: profile_path.to_path_buf(),
        document,
        manifest,
        engine,
    })
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn readyz(State(state): State<ApiState>) -> Json<ServiceInfoResponse> {
    Json(build_service_info(state.service.as_ref()))
}

async fn service_info(State(state): State<ApiState>) -> Json<ServiceInfoResponse> {
    Json(build_service_info(state.service.as_ref()))
}

async fn list_profiles(State(state): State<ApiState>) -> Json<Vec<ProfileInfo>> {
    Json(build_profile_infos(state.service.as_ref()))
}

async fn get_profile(
    State(state): State<ApiState>,
    AxumPath(profile_id): AxumPath<String>,
) -> Result<Json<ProfileInfo>, ApiError> {
    let profile = state
        .service
        .profiles
        .get(&profile_id)
        .ok_or_else(|| ApiError::not_found(format!("unknown profile_id '{}'", profile_id)))?;
    Ok(Json(profile_info(profile)))
}

async fn route_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<RouteExecutionRequest>,
) -> Result<Response, ApiError> {
    let profile = resolve_profile(state.service.as_ref(), payload.profile_id.as_deref())?;
    let profile_id = &profile.document.profile.id;
    info!(
        endpoint = "route",
        profile_id = %profile_id,
        route_id = %payload.request.route_id,
        engine_mode = ?payload.engine_mode,
        format = query.format.as_deref().unwrap_or("json"),
        "request"
    );
    let mut request = payload.request;
    let engine_mode = payload.engine_mode;
    if wants_geojson(&query) {
        request.returns.geometry = ReturnGeometry::Full;
    }
    let effective_engine = profile.engine.effective_engine_description(engine_mode);
    let service = execution_context(state.service.as_ref(), profile, effective_engine);
    let engine = Arc::clone(&profile.engine);
    let route_id = request.route_id.clone();
    let edge_names = if request.returns.segment_rows {
        load_edge_names(state.service.as_ref())?
    } else {
        None
    };
    let result = execute_on_routing_worker(state.service.as_ref(), move || {
        if let Some(edge_names) = edge_names.as_deref() {
            engine.execute_route_with_edge_names_and_mode(&request, edge_names, engine_mode)
        } else {
            engine.execute_route_with_mode(&request, engine_mode)
        }
    })
    .await
    .map_err(|error| {
        warn!(endpoint = "route", route_id = %route_id, %error, "request failed");
        ApiError::from_execution_error(error)
    })?;
    info!(
        endpoint = "route",
        route_id = %result.route_id,
        distance_m = result.summary.total_distance_m,
        time_s = result.summary.total_travel_time_s,
        segments = result.summary.segment_count,
        "response"
    );
    if wants_geojson(&query) {
        return geojson_response(route_result_geojson(
            state.service.as_ref(),
            &service,
            &result,
        ));
    }
    Ok(Json(RouteExecutionResponse { service, result }).into_response())
}

async fn od_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<OdExecutionRequest>,
) -> Result<Response, ApiError> {
    let profile = resolve_profile(state.service.as_ref(), payload.profile_id.as_deref())?;
    let profile_id = &profile.document.profile.id;
    let pair_count = payload.request.pairs.len();
    info!(
        endpoint = "od",
        profile_id = %profile_id,
        pair_count = pair_count,
        engine_mode = ?payload.engine_mode,
        format = query.format.as_deref().unwrap_or("json"),
        "request"
    );
    let mut request = payload.request;
    let engine_mode = payload.engine_mode;
    if wants_geojson(&query) {
        request.returns.geometry = ReturnGeometry::Full;
    }
    let effective_engine = profile.engine.effective_engine_description(engine_mode);
    let service = execution_context(state.service.as_ref(), profile, effective_engine);
    let engine = Arc::clone(&profile.engine);
    let result = execute_on_routing_worker(state.service.as_ref(), move || {
        engine.execute_od_with_mode(&request, engine_mode)
    })
    .await
    .map_err(|error| {
        warn!(endpoint = "od", %error, "request failed");
        ApiError::from_execution_error(error)
    })?;
    info!(
        endpoint = "od",
        pair_count = result.pair_count,
        succeeded = result.succeeded_count,
        ignored = result.ignored_count,
        failed = result.failed_count,
        "response"
    );
    if wants_geojson(&query) {
        return geojson_response(od_result_geojson(&service, &result));
    }
    Ok(Json(OdExecutionResponse { service, result }).into_response())
}

async fn matrix_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<MatrixExecutionRequest>,
) -> Result<Response, ApiError> {
    let profile = resolve_profile(state.service.as_ref(), payload.profile_id.as_deref())?;
    let profile_id = &profile.document.profile.id;
    let origin_count = payload.request.origins.points.len();
    let destination_count = payload.request.destinations.points.len();
    info!(
        endpoint = "matrix",
        profile_id = %profile_id,
        engine_mode = ?payload.engine_mode,
        origins = origin_count,
        destinations = destination_count,
        cells = origin_count * destination_count,
        format = query.format.as_deref().unwrap_or("json"),
        "request"
    );
    let mut request = payload.request;
    let engine_mode = payload.engine_mode;
    if wants_geojson(&query) {
        request.origins.returns.geometry = ReturnGeometry::Full;
        request.destinations.returns.geometry = ReturnGeometry::Full;
    }
    let effective_engine = profile.engine.effective_engine_description(engine_mode);
    let service = execution_context(state.service.as_ref(), profile, effective_engine);
    let engine = Arc::clone(&profile.engine);
    let result = execute_on_routing_worker(state.service.as_ref(), move || {
        engine.execute_matrix_with_mode(&request.origins, &request.destinations, engine_mode)
    })
    .await
    .map_err(|error| {
        warn!(endpoint = "matrix", %error, "request failed");
        ApiError::from_execution_error(error)
    })?;
    info!(
        endpoint = "matrix",
        cells = result.cell_count,
        succeeded = result.succeeded_count,
        ignored = result.ignored_count,
        failed = result.failed_count,
        "response"
    );
    if wants_geojson(&query) {
        return geojson_response(matrix_result_geojson(&service, &result));
    }
    Ok(Json(MatrixExecutionResponse { service, result }).into_response())
}

async fn service_area_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<ServiceAreaExecutionRequest>,
) -> Result<Response, ApiError> {
    let profile = resolve_profile(state.service.as_ref(), payload.profile_id.as_deref())?;
    let profile_id = &profile.document.profile.id;
    info!(
        endpoint = "service_area",
        profile_id = %profile_id,
        analysis_id = %payload.request.analysis_id,
        origins = payload.request.origins.len(),
        thresholds = payload.request.thresholds.len(),
        format = query.format.as_deref().unwrap_or("json"),
        "request"
    );
    let mut request = payload.request;
    if wants_geojson(&query) {
        request.returns.geometry = true;
    }
    let effective_engine = profile
        .engine
        .effective_engine_description(EngineMode::Auto);
    let service = execution_context(state.service.as_ref(), profile, effective_engine);
    let engine = Arc::clone(&profile.engine);
    let result = execute_on_routing_worker(state.service.as_ref(), move || {
        engine.execute_service_area(&request)
    })
    .await
    .map_err(|error| {
        warn!(endpoint = "service_area", %error, "request failed");
        ApiError::from_execution_error(error)
    })?;
    info!(
        endpoint = "service_area",
        analysis_id = %result.analysis_id,
        features = result.features.len(),
        processed_origins = result.processed_origin_count,
        skipped_origins = result.skipped_origin_count,
        "response"
    );
    if wants_geojson(&query) {
        return geojson_response(service_area_result_geojson(&service, &result));
    }
    Ok(Json(ServiceAreaExecutionResponse { service, result }).into_response())
}

fn resolve_profile<'a>(
    service: &'a ServiceRuntime,
    requested_profile_id: Option<&str>,
) -> Result<&'a LoadedProfile, ApiError> {
    let profile_id = requested_profile_id.unwrap_or(&service.default_profile_id);
    service
        .profiles
        .get(profile_id)
        .ok_or_else(|| ApiError::not_found(format!("unknown profile_id '{}'", profile_id)))
}

async fn execute_on_routing_worker<T, F>(
    service: &ServiceRuntime,
    job: F,
) -> Result<T, anyhow::Error>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let permit = service
        .routing_workers
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| anyhow::anyhow!("routing worker pool closed: {error}"))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        job()
    })
    .await
    .map_err(|error| anyhow::anyhow!("routing worker panicked: {error}"))?
}

fn load_edge_names(service: &ServiceRuntime) -> Result<Option<Arc<[String]>>, ApiError> {
    if let Some(edge_names) = service.edge_names.get() {
        return Ok(edge_names.clone());
    }

    let Some(bundle_ref) = service.dataset_manifest.edge_name_bundle.as_ref() else {
        return Err(ApiError::internal(format!(
            "dataset '{}' is missing the edge-name bundle required by the current format; remove the old cached dataset and re-import it",
            service.dataset_manifest.dataset_id.0
        )));
    };
    let loaded = {
        info!(bundle = %bundle_ref.path, "loading edge-name bundle");
        let bundle = read_edge_name_bundle(&bundle_ref.path).map_err(|error| {
            ApiError::internal(format!(
                "reading edge-name bundle {}: {error}",
                bundle_ref.path
            ))
        })?;
        Some(Arc::<[String]>::from(bundle.names))
    };

    let _ = service.edge_names.set(loaded.clone());
    Ok(loaded)
}

fn build_service_info(service: &ServiceRuntime) -> ServiceInfoResponse {
    let topology_meta = service.dataset_manifest.topology_meta.as_ref();
    let topology_bounds = service
        .topology
        .spatial_index
        .as_ref()
        .map(|index| index.bounds);
    ServiceInfoResponse {
        workspace_root: service.workspace_root.display().to_string(),
        dataset: DatasetInfo {
            dataset_id: service.dataset_manifest.dataset_id.0.clone(),
            label: service.dataset_manifest.label.clone(),
            source_path: service.dataset_manifest.source_path.clone(),
            source_sha256: service.dataset_manifest.source_sha256.clone(),
            imported_at: service.dataset_manifest.imported_at.clone(),
            topology_bounds,
            node_count: topology_meta.map(|meta| meta.node_count),
            edge_count: topology_meta.map(|meta| meta.edge_count),
            turn_count: topology_meta.map(|meta| meta.turn_count),
            connected_components: topology_meta.and_then(|meta| meta.connected_components.clone()),
        },
        default_profile_id: service.default_profile_id.clone(),
        loaded_profiles: build_profile_infos(service),
        capabilities: service.capabilities.clone(),
        engine: service.engine,
    }
}

fn build_profile_infos(service: &ServiceRuntime) -> Vec<ProfileInfo> {
    service.profiles.values().map(profile_info).collect()
}

fn profile_info(profile: &LoadedProfile) -> ProfileInfo {
    ProfileInfo {
        profile_id: profile.document.profile.id.clone(),
        label: profile.document.profile.label.clone(),
        mode: serde_json::to_string(&profile.document.profile.mode)
            .unwrap_or_else(|_| "\"unknown\"".to_string())
            .trim_matches('"')
            .to_string(),
        defaults_pack: profile.document.profile.defaults_pack.clone(),
        source_path: profile.source_path.display().to_string(),
        profile_hash: profile.manifest.profile_hash.clone(),
        created_at: profile.manifest.created_at.clone(),
        edge_count: profile.manifest.edge_count,
        default_returns: serde_json::to_value(&profile.document.returns).unwrap_or_default(),
    }
}

fn execution_context(
    service: &ServiceRuntime,
    profile: &LoadedProfile,
    engine: EffectiveEngineDescription,
) -> ExecutionContext {
    ExecutionContext {
        dataset_id: service.dataset_manifest.dataset_id.0.clone(),
        profile_id: profile.document.profile.id.clone(),
        profile_hash: profile.manifest.profile_hash.clone(),
        route_engine: engine.route_engine.to_string(),
        batch_engine: engine.batch_engine.to_string(),
        acceleration: engine.acceleration.to_string(),
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
            batch_engine: "astar_exact_multi_edge_turns_batch_reuse",
            acceleration: "spatial_index+a_star+turn_automaton",
        }
    } else {
        EngineDescription {
            route_engine: "bidirectional_exact_pairwise_turns",
            batch_engine: "bidirectional_exact_pairwise_turns_batch_reuse",
            acceleration: "spatial_index+edge_phantoms",
        }
    }
}

fn wants_geojson(query: &ResponseFormatQuery) -> bool {
    query
        .format
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("geojson"))
}

fn geojson_response(value: Value) -> Result<Response, ApiError> {
    let body = serde_json::to_vec(&value)
        .map_err(|error| ApiError::bad_request(format!("serializing GeoJSON response: {error}")))?;
    Ok((
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/geo+json"),
        )],
        body,
    )
        .into_response())
}

fn route_result_geojson(
    service: &ServiceRuntime,
    execution: &ExecutionContext,
    result: &RouteResult,
) -> Value {
    let coordinates = if let Some(geometry) = result.geometry.as_ref() {
        geometry.clone()
    } else {
        result
            .node_path
            .iter()
            .filter_map(|node_id| service.topology.nodes.get(*node_id as usize))
            .map(|node| [node.lon, node.lat])
            .collect()
    };
    json!({
        "type": "FeatureCollection",
        "features": [
            {
                "type": "Feature",
                "geometry": {
                    "type": "LineString",
                    "coordinates": coordinates,
                },
                "properties": {
                    "dataset_id": execution.dataset_id,
                    "profile_id": execution.profile_id,
                    "profile_hash": execution.profile_hash,
                    "route_id": result.route_id,
                    "outcome": result.outcome,
                    "fallback_used": result.fallback_used,
                    "origin_id": result.origin.point_id,
                    "destination_id": result.destination.point_id,
                    "origin_component_id": result.origin.component_id,
                    "destination_component_id": result.destination.component_id,
                    "origin_snap_distance_m": result.origin.snap_distance_m,
                    "destination_snap_distance_m": result.destination.snap_distance_m,
                    "origin_hop_distance_m": result.origin_hop_distance_m,
                    "destination_hop_distance_m": result.destination_hop_distance_m,
                    "total_distance_m": result.summary.total_distance_m,
                    "total_travel_time_s": result.summary.total_travel_time_s,
                    "total_generalized_cost": result.summary.total_generalized_cost,
                    "illegal_movement_penalty_s": result.summary.illegal_movement_penalty_s,
                    "illegal_movement_penalty_cost": result.summary.illegal_movement_penalty_cost,
                    "violation_count": result.summary.violation_count,
                    "violation_types": result.summary.violation_types,
                    "violations": result.violations,
                    "segment_count": result.summary.segment_count,
                    "warnings": result.warnings,
                }
            }
        ]
    })
}

fn od_result_geojson(execution: &ExecutionContext, result: &OdResult) -> Value {
    let features = result
        .pairs
        .iter()
        .map(|pair| {
            json!({
                "type": "Feature",
                "geometry": pair.geometry.as_ref().map(|geometry| json!({
                    "type": "LineString",
                    "coordinates": geometry,
                })).unwrap_or(Value::Null),
                "properties": {
                    "dataset_id": execution.dataset_id,
                    "profile_id": execution.profile_id,
                    "profile_hash": execution.profile_hash,
                    "pair_id": pair.pair_id,
                    "origin_id": pair.origin_id,
                    "destination_id": pair.destination_id,
                    "status": pair.status,
                    "outcome": pair.outcome,
                    "fallback_used": pair.fallback_used,
                    "origin_component_id": pair.origin_component_id,
                    "destination_component_id": pair.destination_component_id,
                    "origin_hop_distance_m": pair.origin_hop_distance_m,
                    "destination_hop_distance_m": pair.destination_hop_distance_m,
                    "origin_snap_distance_m": pair.origin_snap_distance_m,
                    "destination_snap_distance_m": pair.destination_snap_distance_m,
                    "total_distance_m": pair.total_distance_m,
                    "total_travel_time_s": pair.total_travel_time_s,
                    "total_generalized_cost": pair.total_generalized_cost,
                    "illegal_movement_penalty_s": pair.illegal_movement_penalty_s,
                    "illegal_movement_penalty_cost": pair.illegal_movement_penalty_cost,
                    "violation_count": pair.violation_count,
                    "violation_types": pair.violation_types,
                    "error": pair.error,
                }
            })
        })
        .collect::<Vec<_>>();
    json!({
        "type": "FeatureCollection",
        "features": features,
        "metadata": {
            "dataset_id": execution.dataset_id,
            "profile_id": execution.profile_id,
            "profile_hash": execution.profile_hash,
            "pair_count": result.pair_count,
            "succeeded_count": result.succeeded_count,
            "failed_count": result.failed_count,
            "warnings": result.warnings,
        }
    })
}

fn matrix_result_geojson(execution: &ExecutionContext, result: &MatrixResult) -> Value {
    let features = result
        .cells
        .iter()
        .map(|cell| {
            json!({
                "type": "Feature",
                "geometry": cell.geometry.as_ref().map(|geometry| json!({
                    "type": "LineString",
                    "coordinates": geometry,
                })).unwrap_or(Value::Null),
                "properties": {
                    "dataset_id": execution.dataset_id,
                    "profile_id": execution.profile_id,
                    "profile_hash": execution.profile_hash,
                    "origin_id": cell.origin_id,
                    "destination_id": cell.destination_id,
                    "status": cell.status,
                    "outcome": cell.outcome,
                    "fallback_used": cell.fallback_used,
                    "origin_component_id": cell.origin_component_id,
                    "destination_component_id": cell.destination_component_id,
                    "origin_hop_distance_m": cell.origin_hop_distance_m,
                    "destination_hop_distance_m": cell.destination_hop_distance_m,
                    "origin_snap_distance_m": cell.origin_snap_distance_m,
                    "destination_snap_distance_m": cell.destination_snap_distance_m,
                    "total_distance_m": cell.total_distance_m,
                    "total_travel_time_s": cell.total_travel_time_s,
                    "total_generalized_cost": cell.total_generalized_cost,
                    "illegal_movement_penalty_s": cell.illegal_movement_penalty_s,
                    "illegal_movement_penalty_cost": cell.illegal_movement_penalty_cost,
                    "violation_count": cell.violation_count,
                    "violation_types": cell.violation_types,
                    "error": cell.error,
                }
            })
        })
        .collect::<Vec<_>>();
    json!({
        "type": "FeatureCollection",
        "features": features,
        "metadata": {
            "dataset_id": execution.dataset_id,
            "profile_id": execution.profile_id,
            "profile_hash": execution.profile_hash,
            "origin_count": result.origin_count,
            "destination_count": result.destination_count,
            "cell_count": result.cell_count,
            "succeeded_count": result.succeeded_count,
            "failed_count": result.failed_count,
            "warnings": result.warnings,
        }
    })
}

fn service_area_result_geojson(execution: &ExecutionContext, result: &ServiceAreaResult) -> Value {
    let features = result
        .features
        .iter()
        .map(|feature| {
            json!({
                "type": "Feature",
                "geometry": feature.geometry.clone().unwrap_or(Value::Null),
                "properties": {
                    "dataset_id": execution.dataset_id,
                    "profile_id": execution.profile_id,
                    "profile_hash": execution.profile_hash,
                    "analysis_id": result.analysis_id,
                    "origin_id": feature.origin_id,
                    "threshold_id": feature.threshold_id,
                    "band_start_limit": feature.band_start_limit,
                    "threshold_limit": feature.threshold_limit,
                    "threshold_metric": feature.threshold_metric,
                    "geometry_type": feature.geometry_type,
                    "fallback_used": feature.fallback_used,
                    "origin_component_id": feature.origin_component_id,
                    "origin_hop_distance_m": feature.origin_hop_distance_m,
                    "reachable_network_length_m": feature.reachable_network_length_m,
                    "reachable_edge_count": feature.reachable_edge_count,
                }
            })
        })
        .collect::<Vec<_>>();
    json!({
        "type": "FeatureCollection",
        "features": features,
        "metadata": {
            "dataset_id": execution.dataset_id,
            "profile_id": execution.profile_id,
            "profile_hash": execution.profile_hash,
            "analysis_id": result.analysis_id,
            "outcome": result.outcome,
            "origin_count": result.origin_count,
            "processed_origin_count": result.processed_origin_count,
            "skipped_origin_count": result.skipped_origin_count,
            "fallback_origin_count": result.fallback_origin_count,
            "threshold_count": result.threshold_count,
            "warnings": result.warnings,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        EngineDescription, ExecutionContext, engine_description, matrix_result_geojson,
        od_result_geojson,
    };
    use netan_core::{EdgeBasedTopology, TopologyBundle};
    use netan_query::{
        AnalysisOutcome, BatchItemStatus, MatrixCellResult, MatrixResult, OdPairResult, OdResult,
    };

    #[test]
    fn reports_pairwise_engine_when_only_simple_turns_exist() {
        let topology = TopologyBundle {
            schema_version: 1,
            source_path: "test".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![],
            edge_layers: Default::default(),
            edges: vec![],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
            node_component_ids: vec![0, 0],
            edge_component_ids: vec![0],
        };

        let engine = engine_description(&topology);
        assert_eq!(
            engine.route_engine,
            EngineDescription {
                route_engine: "bidirectional_exact_pairwise_turns",
                batch_engine: "bidirectional_exact_pairwise_turns_batch_reuse",
                acceleration: "spatial_index+edge_phantoms",
            }
            .route_engine
        );
    }

    #[test]
    fn builds_geojson_for_od_results() {
        let execution = ExecutionContext {
            dataset_id: "dataset_a".to_string(),
            profile_id: "car_research_v1".to_string(),
            profile_hash: "abc123".to_string(),
            route_engine: "route".to_string(),
            batch_engine: "batch".to_string(),
            acceleration: "spatial".to_string(),
        };
        let result = OdResult {
            pair_count: 1,
            succeeded_count: 1,
            failed_count: 0,
            ignored_count: 0,
            pairs: vec![OdPairResult {
                pair_id: "pair_1".to_string(),
                origin_id: "a".to_string(),
                destination_id: "b".to_string(),
                status: BatchItemStatus::Succeeded,
                outcome: AnalysisOutcome::Legal,
                fallback_used: false,
                origin_component_id: None,
                destination_component_id: None,
                origin_hop_distance_m: None,
                destination_hop_distance_m: None,
                origin_snap_distance_m: Some(1.0),
                destination_snap_distance_m: Some(2.0),
                total_distance_m: Some(100),
                total_travel_time_s: Some(12.5),
                total_generalized_cost: Some(13.5),
                illegal_movement_penalty_s: Some(0.0),
                illegal_movement_penalty_cost: Some(0.0),
                violation_count: 0,
                violation_types: vec![],
                geometry: Some(vec![[2.0, 48.0], [2.1, 48.1]]),
                diagnostics: vec![],
                error: None,
            }],
            diagnostics: vec![],
            warnings: vec!["ok".to_string()],
        };

        let geojson = od_result_geojson(&execution, &result);
        assert_eq!(geojson["type"], "FeatureCollection");
        assert_eq!(geojson["features"][0]["geometry"]["type"], "LineString");
        assert_eq!(geojson["features"][0]["properties"]["pair_id"], "pair_1");
    }

    #[test]
    fn preserves_null_geometry_in_matrix_geojson() {
        let execution = ExecutionContext {
            dataset_id: "dataset_a".to_string(),
            profile_id: "car_research_v1".to_string(),
            profile_hash: "abc123".to_string(),
            route_engine: "route".to_string(),
            batch_engine: "batch".to_string(),
            acceleration: "spatial".to_string(),
        };
        let result = MatrixResult {
            origin_count: 1,
            destination_count: 1,
            cell_count: 1,
            succeeded_count: 0,
            failed_count: 1,
            ignored_count: 0,
            cells: vec![MatrixCellResult {
                origin_id: "a".to_string(),
                destination_id: "b".to_string(),
                status: BatchItemStatus::Failed,
                outcome: AnalysisOutcome::Unreachable,
                fallback_used: false,
                origin_component_id: None,
                destination_component_id: None,
                origin_hop_distance_m: None,
                destination_hop_distance_m: None,
                origin_snap_distance_m: None,
                destination_snap_distance_m: None,
                total_distance_m: None,
                total_travel_time_s: None,
                total_generalized_cost: None,
                illegal_movement_penalty_s: None,
                illegal_movement_penalty_cost: None,
                violation_count: 0,
                violation_types: vec![],
                geometry: None,
                diagnostics: vec![],
                error: Some("no route".to_string()),
            }],
            diagnostics: vec![],
            warnings: vec![],
        };

        let geojson = matrix_result_geojson(&execution, &result);
        assert!(geojson["features"][0]["geometry"].is_null());
        assert_eq!(geojson["features"][0]["properties"]["status"], "failed");
    }
}
