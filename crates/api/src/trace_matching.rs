use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use netweevil_query::{TraceMatchRequest, TraceMatchResult};
use serde_json::{Value, json};

use crate::dto::{AnalysisResponse, ExecutionContext, ProfiledRequest, ResponseFormatQuery};
use crate::error::ApiError;
use crate::geojson::{geojson_response, wants_geojson};
use crate::state::{ApiState, execute_on_routing_worker, resolve_profile};

pub(crate) async fn trace_match_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<ProfiledRequest<TraceMatchRequest>>,
) -> Result<Response, ApiError> {
    let runtime = state.runtime()?;
    let profile = resolve_profile(&runtime, payload.profile_id.as_deref())?;
    let engine = Arc::clone(&profile.engine);
    let service = ExecutionContext {
        dataset_id: runtime.dataset_manifest.dataset_id.0.clone(),
        profile_id: profile.document.profile.id.clone(),
        profile_hash: profile.manifest.profile_hash.clone(),
        route_engine: "viterbi_with_legal_network_distance".into(),
        batch_engine: "bounded_distance_dijkstra".into(),
        acceleration: "spatial_index+edge_phantoms+turn_automaton".into(),
    };
    let result = execute_on_routing_worker(&runtime, move || {
        engine.execute_trace_match(&payload.request)
    })
    .await
    .map_err(ApiError::from_execution_error)?;
    if wants_geojson(&query) {
        return geojson_response(trace_match_geojson(&service, &result));
    }
    Ok(Json(AnalysisResponse { service, result }).into_response())
}

fn trace_match_geojson(service: &ExecutionContext, result: &TraceMatchResult) -> Value {
    let features = result
        .matchings
        .iter()
        .enumerate()
        .map(|(index, matching)| {
            // Stationary observations can produce one network coordinate. GeoJSON
            // LineStrings require two positions; a Point preserves that geometry.
            let geometry = if matching.geometry.len() == 1 {
                json!({"type":"Point", "coordinates": matching.geometry[0]})
            } else {
                json!({"type":"LineString", "coordinates": matching.geometry})
            };
            json!({
                "type": "Feature", "geometry": geometry,
                "properties": {
                    "trace_id": result.trace_id, "matching_index": index,
                    "dataset_id": service.dataset_id, "profile_id": service.profile_id,
                    "profile_hash": service.profile_hash,
                    "observation_indices": matching.observation_indices,
                    "edge_path": matching.edge_path,
                    "total_distance_m": matching.total_distance_m,
                    "score": matching.score, "confidence": matching.confidence,
                    "confidence_method": result.confidence_method,
                }
            })
        })
        .collect::<Vec<_>>();
    json!({
        "type": "FeatureCollection", "service": service,
        "trace_id": result.trace_id, "features": features,
        "tracepoints": result.tracepoints, "gaps": result.gaps,
        "confidence_method": result.confidence_method,
    })
}
