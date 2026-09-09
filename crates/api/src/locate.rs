//! Profile-aware inspection of routing snap candidates.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use netweevil_query::{LabeledPoint, SnapOptions, SnappedPoint};
use serde::{Deserialize, Serialize};

use crate::dto::{ExecutionResponse, ProfiledRequest};
use crate::error::ApiError;
use crate::state::{ApiState, execute_on_routing_worker, resolve_profile};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocateRequest {
    points: Vec<LabeledPoint>,
    #[serde(default)]
    snap: SnapOptions,
    #[serde(default)]
    direction: LocateDirection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LocateDirection {
    #[default]
    Origin,
    Destination,
}

#[derive(Debug, Serialize)]
pub(crate) struct LocateContext {
    dataset_id: String,
    profile_id: String,
    profile_hash: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct LocatedPoint {
    point_id: String,
    candidates: Vec<SnappedPoint>,
}

pub(crate) async fn locate_handler(
    State(state): State<ApiState>,
    Json(payload): Json<ProfiledRequest<LocateRequest>>,
) -> Result<Json<ExecutionResponse<LocateContext, Vec<LocatedPoint>>>, ApiError> {
    let profile = resolve_profile(&state.service, payload.profile_id.as_deref())?;
    if payload.request.points.is_empty() {
        return Err(ApiError::bad_request("locate requires at least one point"));
    }
    for point in &payload.request.points {
        if !(-180.0..=180.0).contains(&point.lon) || !(-90.0..=90.0).contains(&point.lat) {
            return Err(ApiError::bad_request(format!(
                "point '{}' must have longitude in [-180, 180] and latitude in [-90, 90]",
                point.id
            )));
        }
    }
    let service = LocateContext {
        dataset_id: state.service.dataset_manifest.dataset_id.0.clone(),
        profile_id: profile.document.profile.id.clone(),
        profile_hash: profile.manifest.profile_hash.clone(),
    };
    let engine = Arc::clone(&profile.engine);
    let result = execute_on_routing_worker(&state.service, move || {
        payload
            .request
            .points
            .into_iter()
            .map(|point| {
                let candidates = engine.snap_route_candidates_with_options(
                    &point,
                    &payload.request.snap,
                    matches!(payload.request.direction, LocateDirection::Origin),
                )?;
                Ok(LocatedPoint {
                    point_id: point.id,
                    candidates,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()
    })
    .await
    .map_err(ApiError::from_execution_error)?;
    Ok(Json(ExecutionResponse { service, result }))
}
