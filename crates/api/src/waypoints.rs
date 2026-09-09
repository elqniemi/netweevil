use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use netweevil_profile::ReturnGeometry;
use netweevil_query::WaypointRequest;
use serde_json::json;

use crate::dto::{AnalysisResponse, ExecutionContext, ProfiledRequest, ResponseFormatQuery};
use crate::error::ApiError;
use crate::geojson::{geojson_response, route_result_geojson, wants_geojson};
use crate::state::{ApiState, execute_on_routing_worker, resolve_profile};

pub(crate) async fn waypoints_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<ProfiledRequest<WaypointRequest>>,
) -> Result<Response, ApiError> {
    let profile = resolve_profile(&state.service, payload.profile_id.as_deref())?;
    let engine = Arc::clone(&profile.engine);
    let service = ExecutionContext {
        dataset_id: state.service.dataset_manifest.dataset_id.0.clone(),
        profile_id: profile.document.profile.id.clone(),
        profile_hash: profile.manifest.profile_hash.clone(),
        route_engine: "waypoint_dijkstra_with_turn_history".into(),
        batch_engine: if payload.request.optimize_order {
            "held_karp_exact"
        } else {
            "input_order"
        }
        .into(),
        acceleration: "spatial_index+edge_phantoms+turn_automaton".into(),
    };
    let mut request = payload.request;
    if wants_geojson(&query) {
        request.returns.geometry = ReturnGeometry::Full;
    }
    let result =
        execute_on_routing_worker(&state.service, move || engine.execute_waypoints(&request))
            .await
            .map_err(ApiError::from_execution_error)?;
    if wants_geojson(&query) {
        let mut features = Vec::new();
        for (leg_index, leg) in result.legs.iter().enumerate() {
            let collection = route_result_geojson(&state.service, &service, leg);
            if let Some(leg_features) = collection["features"].as_array() {
                for feature in leg_features {
                    let mut feature = feature.clone();
                    feature["properties"]["leg_index"] = json!(leg_index);
                    features.push(feature);
                }
            }
        }
        return geojson_response(json!({
            "type": "FeatureCollection",
            "service": service,
            "route_id": result.route_id,
            "waypoint_order": result.waypoint_order,
            "optimization_method": result.optimization_method,
            "total_distance_m": result.total_distance_m,
            "total_travel_time_s": result.total_travel_time_s,
            "total_generalized_cost": result.total_generalized_cost,
            "features": features,
        }));
    }
    Ok(Json(AnalysisResponse { service, result }).into_response())
}
