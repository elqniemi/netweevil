use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use netweevil_profile::ReturnGeometry;
use netweevil_query::TripRequest;
use serde_json::json;

use crate::dto::{AnalysisResponse, ExecutionContext, ProfiledRequest, ResponseFormatQuery};
use crate::error::ApiError;
use crate::geojson::{geojson_response, route_result_geojson, wants_geojson};
use crate::state::{ApiState, execute_inline_on_routing_worker, resolve_profile};

/// `POST /v1/trip`: orders stops by directed costs, then routes the legs.
pub(crate) async fn trip_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<ProfiledRequest<TripRequest>>,
) -> Result<Response, ApiError> {
    let runtime = state.runtime()?;
    let profile = resolve_profile(&runtime, payload.profile_id.as_deref())?;
    let engine = Arc::clone(&profile.engine);
    let mut request = payload.request;
    if wants_geojson(&query) {
        request.returns.geometry = ReturnGeometry::Full;
    }
    let result = execute_inline_on_routing_worker(&runtime, move || engine.execute_trip(&request))
        .await
        .map_err(ApiError::from_execution_error)?;
    let service = ExecutionContext {
        dataset_id: runtime.dataset_manifest.dataset_id.0.clone(),
        profile_id: profile.document.profile.id.clone(),
        profile_hash: profile.manifest.profile_hash.clone(),
        route_engine: "trip_legs_many_to_many".into(),
        batch_engine: result.optimization.method.clone(),
        acceleration: "stop_matrix+held_karp_or_iterated_local_search".into(),
    };
    if wants_geojson(&query) {
        let mut features = Vec::new();
        for (leg_index, leg) in result.legs.iter().enumerate() {
            let collection = route_result_geojson(&runtime, &service, leg);
            for feature in collection["features"].as_array().into_iter().flatten() {
                let mut feature = feature.clone();
                feature["properties"]["leg_index"] = json!(leg_index);
                features.push(feature);
            }
        }
        for (position, visit) in result.visits.iter().enumerate() {
            let leg = result
                .legs
                .get(position)
                .map(|leg| &leg.origin)
                .or_else(|| result.legs.last().map(|leg| &leg.destination));
            let Some(snapped) = leg else {
                continue;
            };
            features.push(json!({
                "type": "Feature",
                "geometry": {
                    "type": "Point",
                    "coordinates": [snapped.snapped_lon, snapped.snapped_lat],
                },
                "properties": {
                    "kind": "trip_stop",
                    "visit_index": position,
                    "stop_index": visit.stop_index,
                    "point_id": visit.point_id,
                    "arrival_s": visit.arrival_s,
                    "departure_s": visit.departure_s,
                    "wait_s": visit.wait_s,
                    "lateness_s": visit.lateness_s,
                },
            }));
        }
        return geojson_response(json!({
            "type": "FeatureCollection",
            "service": service,
            "trip_id": result.trip_id,
            "mode": result.mode,
            "stop_order": result.stop_order,
            "total_distance_m": result.total_distance_m,
            "total_travel_time_s": result.total_travel_time_s,
            "total_generalized_cost": result.total_generalized_cost,
            "time_windows_met": result.time_windows_met,
            "optimization": result.optimization,
            "features": features,
        }));
    }
    Ok(Json(AnalysisResponse { service, result }).into_response())
}
