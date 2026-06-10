use std::sync::Arc;

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::response::{IntoResponse, Response};
use netweevil_profile::ReturnGeometry;
use netweevil_query::{EffectiveEngineDescription, EngineMode};
use tracing::{info, warn};

use crate::dto::{
    DatasetInfo, ExecutionContext, HealthResponse, MatrixExecutionRequest, MatrixExecutionResponse,
    OdExecutionRequest, OdExecutionResponse, ProfileInfo, ResponseFormatQuery,
    RouteExecutionRequest, RouteExecutionResponse, ServiceAreaExecutionRequest,
    ServiceAreaExecutionResponse, ServiceInfoResponse, TransitFeedInfo,
};
use crate::error::ApiError;
use crate::geojson::{
    geojson_response, matrix_result_geojson, od_result_geojson, route_result_geojson,
    service_area_result_geojson, wants_geojson,
};
use crate::state::{
    ApiState, LoadedProfile, ServiceRuntime, execute_on_routing_worker, load_edge_names,
    resolve_profile,
};

pub(crate) async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

pub(crate) async fn readyz(State(state): State<ApiState>) -> Json<ServiceInfoResponse> {
    Json(build_service_info(state.service.as_ref()))
}

pub(crate) async fn service_info(State(state): State<ApiState>) -> Json<ServiceInfoResponse> {
    Json(build_service_info(state.service.as_ref()))
}

pub(crate) async fn list_profiles(State(state): State<ApiState>) -> Json<Vec<ProfileInfo>> {
    Json(build_profile_infos(state.service.as_ref()))
}

pub(crate) async fn get_profile(
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

pub(crate) async fn route_handler(
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

pub(crate) async fn od_handler(
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

pub(crate) async fn matrix_handler(
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

pub(crate) async fn service_area_handler(
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

pub(crate) fn build_service_info(service: &ServiceRuntime) -> ServiceInfoResponse {
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
        loaded_transit_feeds: build_transit_feed_infos(service),
        capabilities: service.capabilities.clone(),
        engine: service.engine,
    }
}

pub(crate) fn build_profile_infos(service: &ServiceRuntime) -> Vec<ProfileInfo> {
    service.profiles.values().map(profile_info).collect()
}

pub(crate) fn build_transit_feed_infos(service: &ServiceRuntime) -> Vec<TransitFeedInfo> {
    service
        .transit_feeds
        .values()
        .map(|feed| TransitFeedInfo {
            feed_id: feed.manifest.feed_id.clone(),
            source_path: feed.manifest.source_path.clone(),
            service_start_date: feed.manifest.service_start_date.clone(),
            service_days: feed.manifest.service_days,
            stop_count: feed.manifest.stop_count,
            route_count: feed.manifest.route_count,
            trip_count: feed.manifest.trip_count,
            connection_count: feed.manifest.connection_count,
        })
        .collect()
}

pub(crate) fn profile_info(profile: &LoadedProfile) -> ProfileInfo {
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

pub(crate) fn execution_context(
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
