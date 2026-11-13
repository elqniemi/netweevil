//! Indoor site endpoints: site listing, details and GeoJSON, and indoor
//! routes. Sites load on first use into the workspace's [`SiteCatalog`] and
//! need no street dataset.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::response::{IntoResponse, Response};
use netweevil_core::LocalFrame;
use netweevil_persist::indoor::{IndoorSiteCounts, IndoorSiteManifest};
use netweevil_site::geojson::{indoor_route_geojson, site_geojson};
use netweevil_site::{
    DoorToDoorRequest, DoorToDoorResult, IndoorRouteRequest, IndoorSite, door_to_door_geojson,
    door_to_door_route, route_indoor,
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::dto::ResponseFormatQuery;
use crate::dynamic_profiles::resolve_dynamic_profile;
use crate::error::ApiError;
use crate::geojson::{geojson_response, wants_geojson};
use crate::handlers::run_analysis;
use crate::state::{ApiState, resolve_profile};

#[cfg(test)]
mod tests;

/// One row of `GET /v1/indoor/sites`.
#[derive(Debug, Serialize)]
pub(crate) struct IndoorSiteSummary {
    site_id: String,
    label: String,
    anchor: LocalFrame,
    levels: Vec<String>,
    entrance_count: usize,
    bbox: [f64; 4],
    counts: IndoorSiteCounts,
    created_at: String,
}

impl From<IndoorSiteManifest> for IndoorSiteSummary {
    fn from(m: IndoorSiteManifest) -> Self {
        Self {
            site_id: m.site_id,
            label: m.label,
            anchor: m.anchor,
            levels: m.levels.into_iter().map(|l| l.id).collect(),
            entrance_count: m.entrances.len(),
            bbox: m.bbox,
            counts: m.counts,
            created_at: m.created_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SiteGeojsonQuery {
    level: Option<String>,
}

/// A site error: unknown sites are 404, everything else 400.
pub(crate) fn site_error(error: anyhow::Error) -> ApiError {
    let message = format!("{error:#}");
    if message.starts_with("unknown indoor site") || message.starts_with("invalid site id") {
        ApiError::not_found(message)
    } else {
        ApiError::bad_request(message)
    }
}

/// Runs blocking site work off the async executor.
pub(crate) async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|e| ApiError::internal(format!("indoor worker failed: {e}")))?
        .map_err(site_error)
}

async fn load_site(state: &ApiState, site_id: String) -> Result<Arc<IndoorSite>, ApiError> {
    let workspace = state.workspace.clone();
    blocking(move || workspace.indoor.get(&site_id)).await
}

pub(crate) async fn list_sites(
    State(state): State<ApiState>,
) -> Result<Json<Vec<IndoorSiteSummary>>, ApiError> {
    let workspace = state.workspace.clone();
    let manifests = blocking(move || workspace.indoor.manifests()).await?;
    Ok(Json(manifests.into_iter().map(Into::into).collect()))
}

pub(crate) async fn get_site(
    State(state): State<ApiState>,
    AxumPath(site_id): AxumPath<String>,
) -> Result<Json<IndoorSiteManifest>, ApiError> {
    let site = load_site(&state, site_id).await?;
    Ok(Json(site.manifest.clone()))
}

pub(crate) async fn site_geojson_handler(
    State(state): State<ApiState>,
    AxumPath(site_id): AxumPath<String>,
    Query(query): Query<SiteGeojsonQuery>,
) -> Result<Response, ApiError> {
    let site = load_site(&state, site_id).await?;
    if let Some(level) = &query.level
        && !site.manifest.levels.iter().any(|l| l.id == *level)
    {
        return Err(ApiError::bad_request(format!(
            "site '{}' has no level '{level}'",
            site.site_id()
        )));
    }
    geojson_response(site_geojson(&site, query.level.as_deref()))
}

pub(crate) async fn indoor_route_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(request): Json<IndoorRouteRequest>,
) -> Result<Response, ApiError> {
    info!(endpoint = "indoor_route", site_id = %request.site_id, "request");
    let site = load_site(&state, request.site_id.clone()).await?;
    let result = blocking(move || route_indoor(&site, &request))
        .await
        .inspect_err(|e| warn!(endpoint = "indoor_route", error = ?e, "request failed"))?;
    if wants_geojson(&query) {
        return geojson_response(indoor_route_geojson(&result));
    }
    Ok(Json(result).into_response())
}

/// `POST /v1/door-to-door/route`: a street profile chosen like `/v1/route`
/// plus a door-to-door request.
#[derive(Debug, Deserialize)]
pub(crate) struct DoorToDoorExecutionRequest {
    #[serde(default)]
    profile_id: Option<String>,
    #[serde(default)]
    profile: Option<serde_json::Value>,
    #[serde(default)]
    profile_overrides: Option<serde_json::Value>,
    request: DoorToDoorRequest,
}

#[derive(Debug, Serialize)]
struct DoorToDoorResponse {
    service: DoorToDoorContext,
    result: DoorToDoorResult,
}

#[derive(Debug, Serialize)]
struct DoorToDoorContext {
    dataset_id: String,
    profile_id: String,
}

pub(crate) async fn door_to_door_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<DoorToDoorExecutionRequest>,
) -> Result<Response, ApiError> {
    let runtime = state.runtime()?;
    let engine = if payload.profile.is_some() || payload.profile_overrides.is_some() {
        resolve_dynamic_profile(
            Arc::clone(&runtime),
            payload.profile_id.as_deref(),
            payload.profile,
            payload.profile_overrides,
        )
        .await?
        .engine
    } else {
        Arc::clone(&resolve_profile(runtime.as_ref(), payload.profile_id.as_deref())?.engine)
    };
    let request = payload.request;
    let route_id = request.route_id.clone();
    info!(endpoint = "door_to_door", route_id = %route_id, profile_id = %engine.metrics().profile_id, "request");
    let service = DoorToDoorContext {
        dataset_id: runtime.dataset_manifest.dataset_id.0.clone(),
        profile_id: engine.metrics().profile_id.clone(),
    };
    let workspace = state.workspace.clone();
    let result = run_analysis(&state, "door_to_door", Some(&route_id), move || {
        door_to_door_route(&engine, &workspace.indoor, &request)
            .map_err(|e| anyhow::anyhow!("{e:#}"))
    })
    .await?;
    if wants_geojson(&query) {
        let mut geojson = door_to_door_geojson(&result);
        geojson["service"] = serde_json::to_value(&service).expect("serializable context");
        return geojson_response(geojson);
    }
    Ok(Json(DoorToDoorResponse { service, result }).into_response())
}
