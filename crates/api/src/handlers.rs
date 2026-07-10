use std::sync::Arc;

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::response::{IntoResponse, Response};
use netweevil_profile::ReturnGeometry;
use netweevil_query::{
    EffectiveEngineDescription, EngineMode, TemporalRequestOptions, execute_scenario_batch,
};
use tracing::{info, warn};

use crate::dto::{
    AccessibilityExecutionRequest, AccessibilityExecutionResponse, BetweennessExecutionRequest,
    BetweennessExecutionResponse, DatasetInfo, ExecutionContext, HealthResponse,
    MatrixExecutionRequest, MatrixExecutionResponse, OdExecutionRequest, OdExecutionResponse,
    ProfileInfo, ResponseFormatQuery, RouteExecutionRequest, RouteExecutionResponse,
    ScenarioBatchExecutionRequest, ScenarioBatchExecutionResponse, ServiceAreaExecutionRequest,
    ServiceAreaExecutionResponse, ServiceAreaSequenceExecutionRequest,
    ServiceAreaSequenceExecutionResponse, ServiceInfoResponse, TransitFeedInfo,
};
use crate::error::ApiError;
use crate::geojson::{
    betweenness_result_geojson, geojson_response, matrix_result_geojson, od_result_geojson,
    route_result_geojson, scenario_batch_result_geojson, service_area_result_geojson,
    service_area_sequence_result_geojson, wants_geojson,
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
    let effective_engine = profile
        .engine
        .effective_route_engine_description(&request, engine_mode);
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
    let effective_engine = effective_engine_for_temporal_options(
        profile.engine.effective_engine_description(engine_mode),
        &request.temporal,
        "time_dependent_exact_pairwise_label_setting",
    );
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
    let temporal = if request.origins.temporal.is_temporal() {
        &request.origins.temporal
    } else {
        &request.destinations.temporal
    };
    let effective_engine = effective_engine_for_temporal_options(
        profile.engine.effective_engine_description(engine_mode),
        temporal,
        "time_dependent_exact_pairwise_label_setting",
    );
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

pub(crate) async fn accessibility_handler(
    State(state): State<ApiState>,
    Json(payload): Json<AccessibilityExecutionRequest>,
) -> Result<Response, ApiError> {
    let profile = resolve_profile(state.service.as_ref(), payload.profile_id.as_deref())?;
    info!(
        endpoint = "accessibility",
        profile_id = %profile.document.profile.id,
        origins = payload.request.origins.points.len(),
        categories = payload.request.categories.len(),
        engine_mode = ?payload.engine_mode,
        "request"
    );
    let effective_engine = effective_engine_for_temporal_options(
        profile
            .engine
            .effective_engine_description(payload.engine_mode),
        &payload.request.origins.temporal,
        "time_dependent_bounded_accessibility",
    );
    let service = execution_context(state.service.as_ref(), profile, effective_engine);
    let engine = Arc::clone(&profile.engine);
    let engine_mode = payload.engine_mode;
    let request = payload.request;
    let result = execute_on_routing_worker(state.service.as_ref(), move || {
        engine.execute_accessibility_with_mode(&request, engine_mode)
    })
    .await
    .map_err(|error| {
        warn!(endpoint = "accessibility", %error, "request failed");
        ApiError::from_execution_error(error)
    })?;
    info!(
        endpoint = "accessibility",
        rows = result.row_count,
        succeeded = result.succeeded_count,
        failed = result.failed_count,
        "response"
    );
    Ok(Json(AccessibilityExecutionResponse { service, result }).into_response())
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
    let effective_engine = effective_engine_for_temporal_options(
        profile
            .engine
            .effective_engine_description(EngineMode::Auto),
        &request.temporal,
        "time_dependent_exact_service_area",
    );
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

pub(crate) async fn service_area_sequence_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<ServiceAreaSequenceExecutionRequest>,
) -> Result<Response, ApiError> {
    let profile = resolve_profile(state.service.as_ref(), payload.profile_id.as_deref())?;
    let mut request = payload.request;
    if wants_geojson(&query) {
        request.request.returns.geometry = true;
    }
    let effective_engine = EffectiveEngineDescription {
        route_engine: "time_dependent_nondominated_label_setting",
        batch_engine: "time_dependent_exact_service_area_sequence",
        acceleration: "spatial_index+edge_phantoms+turn_automaton",
    };
    let service = execution_context(state.service.as_ref(), profile, effective_engine);
    let sequence_id = request.sequence_id.clone();
    let engine = Arc::clone(&profile.engine);
    info!(
        endpoint = "service_area_sequence",
        profile_id = %profile.document.profile.id,
        sequence_id = %sequence_id,
        format = query.format.as_deref().unwrap_or("json"),
        "request"
    );
    let result = execute_on_routing_worker(state.service.as_ref(), move || {
        engine.execute_service_area_sequence(&request)
    })
    .await
    .map_err(|error| {
        warn!(endpoint = "service_area_sequence", sequence_id = %sequence_id, %error, "request failed");
        ApiError::from_execution_error(error)
    })?;
    if wants_geojson(&query) {
        return geojson_response(service_area_sequence_result_geojson(&service, &result));
    }
    Ok(Json(ServiceAreaSequenceExecutionResponse { service, result }).into_response())
}

pub(crate) async fn betweenness_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<BetweennessExecutionRequest>,
) -> Result<Response, ApiError> {
    let profile = resolve_profile(state.service.as_ref(), payload.profile_id.as_deref())?;
    info!(
        endpoint = "betweenness",
        profile_id = %profile.document.profile.id,
        analysis_id = %payload.request.analysis_id,
        origins = payload.request.origins.len(),
        destinations = payload.request.destinations.len(),
        "request"
    );
    let effective_engine = effective_engine_for_temporal_options(
        profile
            .engine
            .effective_engine_description(EngineMode::Auto),
        &payload.request.temporal,
        "time_dependent_exact_betweenness",
    );
    let service = execution_context(state.service.as_ref(), profile, effective_engine);
    let engine = Arc::clone(&profile.engine);
    let result = execute_on_routing_worker(state.service.as_ref(), move || {
        engine.execute_betweenness(&payload.request)
    })
    .await
    .map_err(ApiError::from_execution_error)?;
    if wants_geojson(&query) {
        return geojson_response(betweenness_result_geojson(&service, &result));
    }
    Ok(Json(BetweennessExecutionResponse { service, result }).into_response())
}

pub(crate) async fn scenario_batch_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<ScenarioBatchExecutionRequest>,
) -> Result<Response, ApiError> {
    let profile = resolve_profile(state.service.as_ref(), payload.profile_id.as_deref())?;
    let mut request = payload.request;
    info!(
        endpoint = "scenario_batch",
        profile_id = %profile.document.profile.id,
        batch_id = %request.batch_id,
        scenarios = request.scenarios.len(),
        routes = request.routes.len(),
        service_areas = request.service_areas.len(),
        accessibility = request.accessibility.len(),
        od = request.od.len(),
        matrices = request.matrices.len(),
        betweenness = request.betweenness.len(),
        "request"
    );
    if wants_geojson(&query) {
        for route in &mut request.routes {
            route.returns.geometry = ReturnGeometry::Full;
        }
        for service_area in &mut request.service_areas {
            service_area.returns.geometry = true;
        }
        for od in &mut request.od {
            od.request.returns.geometry = ReturnGeometry::Full;
        }
        for matrix in &mut request.matrices {
            matrix.origins.returns.geometry = ReturnGeometry::Full;
            matrix.destinations.returns.geometry = ReturnGeometry::Full;
        }
    }
    let effective_engine = EffectiveEngineDescription {
        route_engine: "scenario_batch_overlay_replay",
        batch_engine: "scenario_batch_overlay_replay",
        acceleration: "static_baseline+exact_temporal_scenarios",
    };
    let service = execution_context(state.service.as_ref(), profile, effective_engine);
    let engine = Arc::clone(&profile.engine);
    let batch_id = request.batch_id.clone();
    let result = execute_on_routing_worker(state.service.as_ref(), move || {
        execute_scenario_batch(engine.as_ref(), &request)
    })
    .await
    .map_err(|error| {
        warn!(endpoint = "scenario_batch", batch_id = %batch_id, %error, "request failed");
        ApiError::from_execution_error(error)
    })?;
    if wants_geojson(&query) {
        return geojson_response(scenario_batch_result_geojson(
            state.service.as_ref(),
            &service,
            &result,
        ));
    }
    Ok(Json(ScenarioBatchExecutionResponse { service, result }).into_response())
}

fn effective_engine_for_temporal_options(
    base: EffectiveEngineDescription,
    temporal: &TemporalRequestOptions,
    temporal_batch_engine: &'static str,
) -> EffectiveEngineDescription {
    if !temporal.constraints.is_empty() || temporal.pareto.is_some() {
        return EffectiveEngineDescription {
            route_engine: "component_nondominated_label_setting",
            batch_engine: "component_nondominated_pairwise_label_setting",
            acceleration: "spatial_index+edge_phantoms+turn_automaton",
        };
    }
    if temporal.is_temporal() {
        return EffectiveEngineDescription {
            route_engine: "time_dependent_nondominated_label_setting",
            batch_engine: temporal_batch_engine,
            acceleration: "spatial_index+edge_phantoms+turn_automaton",
        };
    }
    base
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
            bound_stop_count: feed.manifest.bound_stop_count,
            transfer_profile_ids: feed
                .router
                .transfer_profile_ids()
                .into_iter()
                .map(str::to_string)
                .collect(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporal_execution_context_does_not_advertise_static_acceleration() {
        let base = EffectiveEngineDescription {
            route_engine: "accelerated_pairwise_turns",
            batch_engine: "accelerated_pairwise_turns_batch_reuse",
            acceleration: "spatial_index+edge_phantoms+cch",
        };
        let temporal = TemporalRequestOptions {
            departure_time: Some("2026-07-10T09:59:00+08:00".to_string()),
            ..TemporalRequestOptions::default()
        };
        let effective = effective_engine_for_temporal_options(
            base,
            &temporal,
            "time_dependent_bounded_accessibility",
        );
        assert_eq!(
            effective.batch_engine,
            "time_dependent_bounded_accessibility"
        );
        assert_eq!(
            effective.acceleration,
            "spatial_index+edge_phantoms+turn_automaton"
        );
    }
}
