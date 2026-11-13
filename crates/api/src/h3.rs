//! H3 cell layers: the network of the loaded dataset per cell, and points
//! aggregated into cells.

use axum::Json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use netweevil_h3::{
    CellStat, NetworkCellStat, aggregate_points, cells_geojson, parse_resolution,
    parse_resolutions, summarize_network,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::ApiError;
use crate::geojson::geojson_response;
use crate::state::{ApiState, execute_on_routing_worker};

/// Most cells one network summary returns.
const NETWORK_CELL_LIMIT: usize = 250_000;

fn bad_request(error: anyhow::Error) -> ApiError {
    ApiError::bad_request(format!("{error:#}"))
}

fn wants_geojson(format: Option<&str>) -> bool {
    format.is_some_and(|value| value.eq_ignore_ascii_case("geojson"))
}

fn properties(value: &impl Serialize) -> Map<String, Value> {
    let mut properties = match serde_json::to_value(value) {
        Ok(Value::Object(properties)) => properties,
        _ => Map::new(),
    };
    properties.remove("cell");
    properties.remove("resolution");
    properties
}

#[derive(Debug, Deserialize)]
pub(crate) struct H3NetworkQuery {
    pub(crate) resolution: u8,
    /// `geojson` for a feature collection of hexagons.
    pub(crate) format: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct H3NetworkResponse {
    dataset_id: String,
    resolution: u8,
    cells: Vec<NetworkCellStat>,
}

/// `GET /v1/h3/network`: nodes and directed edges of the loaded dataset
/// per cell.
pub(crate) async fn h3_network_handler(
    State(state): State<ApiState>,
    Query(query): Query<H3NetworkQuery>,
) -> Result<Response, ApiError> {
    let runtime = state.runtime()?;
    let resolution = parse_resolution(query.resolution).map_err(bad_request)?;
    let topology = runtime.topology.clone();
    let cells = execute_on_routing_worker(&runtime, move || {
        Ok(summarize_network(&topology, resolution))
    })
    .await
    .map_err(ApiError::from_execution_error)?;
    if cells.len() > NETWORK_CELL_LIMIT {
        return Err(ApiError::bad_request(format!(
            "the dataset covers {} cells at resolution {}; the limit is {NETWORK_CELL_LIMIT}, use a coarser resolution",
            cells.len(),
            query.resolution
        )));
    }
    if wants_geojson(query.format.as_deref()) {
        return geojson_response(cells_geojson(
            cells.iter().map(|stat| (stat.cell, properties(stat))),
        ));
    }
    Ok(Json(H3NetworkResponse {
        dataset_id: runtime.dataset_manifest.dataset_id.0.clone(),
        resolution: query.resolution,
        cells,
    })
    .into_response())
}

fn default_weight() -> f64 {
    1.0
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct H3Point {
    lon: f64,
    lat: f64,
    #[serde(default = "default_weight")]
    weight: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct H3AggregateRequest {
    points: Vec<H3Point>,
    resolutions: Vec<u8>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct H3FormatQuery {
    /// `geojson` for a feature collection of hexagons.
    pub(crate) format: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct H3AggregateResponse {
    point_count: usize,
    skipped_point_count: usize,
    cells: Vec<CellStat>,
}

/// `POST /v1/h3/aggregate`: count and weigh points per cell on one or more
/// resolutions. Works without a loaded dataset.
pub(crate) async fn h3_aggregate_handler(
    Query(query): Query<H3FormatQuery>,
    Json(payload): Json<H3AggregateRequest>,
) -> Result<Response, ApiError> {
    let resolutions = parse_resolutions(&payload.resolutions).map_err(bad_request)?;
    if payload.points.iter().any(|point| !point.weight.is_finite()) {
        return Err(ApiError::bad_request("point weights must be finite"));
    }
    let point_count = payload.points.len();
    let (cells, skipped_point_count) = aggregate_points(
        payload
            .points
            .iter()
            .map(|point| (point.lon, point.lat, point.weight)),
        &resolutions,
    );
    if wants_geojson(query.format.as_deref()) {
        return geojson_response(cells_geojson(
            cells.iter().map(|stat| (stat.cell, properties(stat))),
        ));
    }
    Ok(Json(H3AggregateResponse {
        point_count,
        skipped_point_count,
        cells,
    })
    .into_response())
}

#[cfg(test)]
mod tests;
