//! Search expansion as GeoJSON: the edges the exact search settles for a
//! route or service area, in settle order, for debugging and animation.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use netweevil_query::{ExpansionRequest, ExpansionResult};
use serde::Serialize;
use serde_json::{Value, json};
use tracing::info;

use crate::dto::ProfiledRequest;
use crate::error::ApiError;
use crate::state::{ApiState, execute_on_routing_worker, resolve_profile};

#[derive(Debug, Serialize)]
pub(crate) struct ExpansionResponse {
    r#type: &'static str,
    features: Vec<Value>,
    meta: ExpansionMeta,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExpansionMeta {
    dataset_id: String,
    profile_id: String,
    action: netweevil_query::ExpansionAction,
    direction: netweevil_query::ServiceAreaDirection,
    metric: String,
    settled_edge_count: usize,
    truncated: bool,
    route_cost: Option<f64>,
}

pub(crate) async fn expansion_handler(
    State(state): State<ApiState>,
    Json(payload): Json<ProfiledRequest<ExpansionRequest>>,
) -> Result<Json<ExpansionResponse>, ApiError> {
    let runtime = state.runtime()?;
    let profile = resolve_profile(&runtime, payload.profile_id.as_deref())?;
    let request = payload.request;
    info!(
        endpoint = "expansion",
        profile_id = %profile.document.profile.id,
        action = ?request.action,
        max_edges = request.max_edges,
        "request"
    );
    let dataset_id = runtime.dataset_manifest.dataset_id.0.clone();
    let profile_id = profile.document.profile.id.clone();
    let engine = Arc::clone(&profile.engine);
    let topology = runtime.topology.clone();
    let response = execute_on_routing_worker(&runtime, move || {
        let result = engine.execute_expansion(&request)?;
        Ok(expansion_geojson(&topology, dataset_id, profile_id, result))
    })
    .await
    .map_err(ApiError::from_execution_error)?;
    Ok(Json(response))
}

fn expansion_geojson(
    topology: &netweevil_core::TopologyBundle,
    dataset_id: String,
    profile_id: String,
    result: ExpansionResult,
) -> ExpansionResponse {
    let features = result
        .edges
        .iter()
        .map(|expanded| {
            let edge = topology.routing_edge(expanded.edge_index as usize);
            let from = &topology.nodes[edge.from.0 as usize];
            let to = &topology.nodes[edge.to.0 as usize];
            json!({
                "type": "Feature",
                "id": expanded.order,
                "geometry": {
                    "type": "LineString",
                    "coordinates": [[from.lon, from.lat], [to.lon, to.lat]],
                },
                "properties": {
                    "order": expanded.order,
                    "edge_index": expanded.edge_index,
                    "edge_id": edge.edge_id.0,
                    "cost": expanded.cost,
                    "previous_edge_index": expanded.previous_edge_index,
                },
            })
        })
        .collect::<Vec<_>>();
    ExpansionResponse {
        r#type: "FeatureCollection",
        meta: ExpansionMeta {
            dataset_id,
            profile_id,
            action: result.action,
            direction: result.direction,
            metric: result.metric,
            settled_edge_count: features.len(),
            truncated: result.truncated,
            route_cost: result.route_cost,
        },
        features,
    }
}
