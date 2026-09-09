//! Routing-analysis endpoints. Every POST handler follows the same lifecycle:
//! resolve the profile, log the request, upgrade geometry when GeoJSON was
//! asked for, describe the effective engine, run the analysis on the routing
//! worker pool ([`run_analysis`]), then render JSON or GeoJSON
//! ([`analysis_response`]).

use std::sync::Arc;

use anyhow::Result;
use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::response::{IntoResponse, Response};
use netweevil_profile::ReturnGeometry;
use netweevil_query::{
    EffectiveEngineDescription, EngineMode, TemporalRequestOptions, execute_scenario_batch,
};
use serde::Serialize;
use serde_json::Value;
use tracing::{info, warn};

use crate::dto::{
    AccessibilityExecutionRequest, AnalysisResponse, BetweennessExecutionRequest, DatasetInfo,
    ExecutionContext, HealthResponse, MatrixExecutionRequest, OdExecutionRequest, ProfileInfo,
    ResponseFormatQuery, RouteExecutionRequest, ScenarioBatchExecutionRequest,
    ServiceAreaExecutionRequest, ServiceAreaSequenceExecutionRequest, ServiceInfoResponse,
    TransitFeedInfo,
};
use crate::dynamic_profiles::resolve_dynamic_profile;
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

/// Runs one analysis on the routing worker pool, logging and translating a
/// failure the way every analysis endpoint does.
async fn run_analysis<T>(
    state: &ApiState,
    endpoint: &'static str,
    analysis_id: Option<&str>,
    job: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T, ApiError>
where
    T: Send + 'static,
{
    execute_on_routing_worker(state.service.as_ref(), job)
        .await
        .map_err(|error| {
            warn!(
                endpoint,
                analysis_id = analysis_id.unwrap_or("-"),
                %error,
                "request failed"
            );
            ApiError::from_execution_error(error)
        })
}

/// Renders an analysis result as the `{service, result}` JSON envelope, or as
/// the endpoint's GeoJSON when `?format=geojson` was requested.
fn analysis_response<T: Serialize>(
    query: &ResponseFormatQuery,
    service: ExecutionContext,
    result: T,
    to_geojson: impl FnOnce(&ExecutionContext, &T) -> Value,
) -> Result<Response, ApiError> {
    if wants_geojson(query) {
        return geojson_response(to_geojson(&service, &result));
    }
    Ok(Json(AnalysisResponse { service, result }).into_response())
}

pub(crate) async fn route_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<RouteExecutionRequest>,
) -> Result<Response, ApiError> {
    execute_route_response(state, query, payload, false).await
}

pub(crate) async fn directions_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<RouteExecutionRequest>,
) -> Result<Response, ApiError> {
    execute_route_response(state, query, payload, true).await
}

async fn execute_route_response(
    state: ApiState,
    query: ResponseFormatQuery,
    payload: RouteExecutionRequest,
    directions: bool,
) -> Result<Response, ApiError> {
    let dynamic = if payload.profile.is_some() || payload.profile_overrides.is_some() {
        Some(
            resolve_dynamic_profile(
                Arc::clone(&state.service),
                payload.profile_id.as_deref(),
                payload.profile,
                payload.profile_overrides,
            )
            .await?,
        )
    } else {
        None
    };
    let engine = if let Some(dynamic) = &dynamic {
        Arc::clone(&dynamic.engine)
    } else {
        Arc::clone(&resolve_profile(state.service.as_ref(), payload.profile_id.as_deref())?.engine)
    };
    info!(
        endpoint = "route",
        profile_id = %engine.metrics().profile_id,
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
    let effective_engine = engine.effective_route_engine_description(&request, engine_mode);
    let service = ExecutionContext {
        dataset_id: state.service.dataset_manifest.dataset_id.0.clone(),
        profile_id: engine.metrics().profile_id.clone(),
        profile_hash: engine.metrics().profile_hash.clone(),
        route_engine: effective_engine.route_engine.to_string(),
        batch_engine: effective_engine.batch_engine.to_string(),
        acceleration: effective_engine.acceleration.to_string(),
    };
    let route_id = request.route_id.clone();
    let edge_names = if request.returns.segment_rows || directions {
        Some(load_edge_names(state.service.as_ref())?)
    } else {
        None
    };
    let (result, maneuvers) = run_analysis(&state, "route", Some(&route_id), move || {
        if directions {
            let result = engine.execute_directions(
                &request,
                edge_names.as_deref().unwrap_or_default(),
                engine_mode,
            )?;
            return Ok((result.route, Some(result.maneuvers)));
        }
        let result = if let Some(edge_names) = edge_names.as_deref() {
            engine.execute_route_with_edge_names_and_mode(&request, edge_names, engine_mode)
        } else {
            engine.execute_route_with_mode(&request, engine_mode)
        }?;
        Ok((result, None))
    })
    .await?;
    info!(
        endpoint = "route",
        route_id = %result.route_id,
        distance_m = result.summary.total_distance_m,
        time_s = result.summary.total_travel_time_s,
        segments = result.summary.segment_count,
        "response"
    );
    let mut response = if let Some(maneuvers) = maneuvers {
        let result = netweevil_query::DirectionsResult {
            language: "en".into(),
            route: result,
            maneuvers,
        };
        analysis_response(&query, service, result, |context, result| {
            let mut geojson = route_result_geojson(state.service.as_ref(), context, &result.route);
            geojson["maneuvers"] =
                serde_json::to_value(&result.maneuvers).expect("serializable maneuvers");
            geojson["language"] = "en".into();
            geojson
        })?
    } else {
        analysis_response(&query, service, result, |context, result| {
            route_result_geojson(state.service.as_ref(), context, result)
        })?
    };
    if let Some(dynamic) = dynamic {
        response.headers_mut().insert(
            "x-netweevil-profile-cache",
            dynamic.cache_status.parse().unwrap(),
        );
        response.headers_mut().insert(
            "x-netweevil-profile-prepare-ms",
            format!("{:.3}", dynamic.prepare_ms).parse().unwrap(),
        );
    }
    Ok(response)
}

pub(crate) async fn od_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<OdExecutionRequest>,
) -> Result<Response, ApiError> {
    let profile = resolve_profile(state.service.as_ref(), payload.profile_id.as_deref())?;
    info!(
        endpoint = "od",
        profile_id = %profile.document.profile.id,
        pair_count = payload.request.pairs.len(),
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
    let result = run_analysis(&state, "od", None, move || {
        engine.execute_od_with_mode(&request, engine_mode)
    })
    .await?;
    info!(
        endpoint = "od",
        pair_count = result.pair_count,
        succeeded = result.succeeded_count,
        ignored = result.ignored_count,
        failed = result.failed_count,
        "response"
    );
    analysis_response(&query, service, result, od_result_geojson)
}

pub(crate) async fn matrix_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<MatrixExecutionRequest>,
) -> Result<Response, ApiError> {
    let profile = resolve_profile(state.service.as_ref(), payload.profile_id.as_deref())?;
    let origin_count = payload.request.origins.points.len();
    let destination_count = payload.request.destinations.points.len();
    info!(
        endpoint = "matrix",
        profile_id = %profile.document.profile.id,
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
    let result = run_analysis(&state, "matrix", None, move || {
        engine.execute_matrix_with_mode(&request.origins, &request.destinations, engine_mode)
    })
    .await?;
    info!(
        endpoint = "matrix",
        cells = result.cell_count,
        succeeded = result.succeeded_count,
        ignored = result.ignored_count,
        failed = result.failed_count,
        "response"
    );
    analysis_response(&query, service, result, matrix_result_geojson)
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
    let result = run_analysis(&state, "accessibility", None, move || {
        engine.execute_accessibility_with_mode(&request, engine_mode)
    })
    .await?;
    info!(
        endpoint = "accessibility",
        rows = result.row_count,
        succeeded = result.succeeded_count,
        failed = result.failed_count,
        "response"
    );
    Ok(Json(AnalysisResponse { service, result }).into_response())
}

pub(crate) async fn service_area_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<ServiceAreaExecutionRequest>,
) -> Result<Response, ApiError> {
    let profile = resolve_profile(state.service.as_ref(), payload.profile_id.as_deref())?;
    info!(
        endpoint = "service_area",
        profile_id = %profile.document.profile.id,
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
    let analysis_id = request.analysis_id.clone();
    let result = run_analysis(&state, "service_area", Some(&analysis_id), move || {
        engine.execute_service_area(&request)
    })
    .await?;
    info!(
        endpoint = "service_area",
        analysis_id = %result.analysis_id,
        features = result.features.len(),
        processed_origins = result.processed_origin_count,
        skipped_origins = result.skipped_origin_count,
        "response"
    );
    analysis_response(&query, service, result, service_area_result_geojson)
}

pub(crate) async fn service_area_sequence_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<ServiceAreaSequenceExecutionRequest>,
) -> Result<Response, ApiError> {
    let profile = resolve_profile(state.service.as_ref(), payload.profile_id.as_deref())?;
    info!(
        endpoint = "service_area_sequence",
        profile_id = %profile.document.profile.id,
        sequence_id = %payload.request.sequence_id,
        format = query.format.as_deref().unwrap_or("json"),
        "request"
    );
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
    let engine = Arc::clone(&profile.engine);
    let sequence_id = request.sequence_id.clone();
    let result = run_analysis(
        &state,
        "service_area_sequence",
        Some(&sequence_id),
        move || engine.execute_service_area_sequence(&request),
    )
    .await?;
    analysis_response(
        &query,
        service,
        result,
        service_area_sequence_result_geojson,
    )
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
    let request = payload.request;
    let analysis_id = request.analysis_id.clone();
    let result = run_analysis(&state, "betweenness", Some(&analysis_id), move || {
        engine.execute_betweenness(&request)
    })
    .await?;
    analysis_response(&query, service, result, betweenness_result_geojson)
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
    let result = run_analysis(&state, "scenario_batch", Some(&batch_id), move || {
        execute_scenario_batch(engine.as_ref(), &request)
    })
    .await?;
    analysis_response(&query, service, result, |context, result| {
        scenario_batch_result_geojson(state.service.as_ref(), context, result)
    })
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

fn build_profile_infos(service: &ServiceRuntime) -> Vec<ProfileInfo> {
    service.profiles.values().map(profile_info).collect()
}

fn build_transit_feed_infos(service: &ServiceRuntime) -> Vec<TransitFeedInfo> {
    service
        .transit_feeds
        .values()
        .map(|feed| TransitFeedInfo {
            feed_id: feed.manifest.feed_id.clone(),
            source_path: feed.manifest.source_path.clone(),
            service_start_date: feed.manifest.service_start_date.clone(),
            service_days: feed.manifest.service_days,
            agency_timezone: feed.manifest.agency_timezone.clone(),
            time_origin_unix_s: feed.manifest.time_origin_unix_s,
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
