//! Stored cell layers, the surge balance and ride dispatch.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use netweevil_h3::{
    CellLayer, CellLayerSummary, ColumnSpec, LayerBuilder, PaintOp, RebalanceMove, SurgeCell,
    SurgeConfig, WeightedCell, aggregate_points, cells_geojson, compute_surge, cover_bbox,
    cover_polygon, default_column_specs, parse_cell, parse_resolution, plan_rebalance,
    validate_layer_id,
};
use netweevil_query::{DispatchRequest, DispatchResult, execute_dispatch};
use serde::{Deserialize, Serialize};
use tracing::info;

use super::{H3FormatQuery, bad_request, properties, wants_geojson};
use crate::error::ApiError;
use crate::geojson::geojson_response;
use crate::state::{ApiState, execute_on_routing_worker, resolve_profile};

/// Most cells one paint request covers by area.
const PAINT_CELL_LIMIT: usize = 200_000;

/// Serializes the read-modify-write of layer files.
static LAYER_WRITES: Mutex<()> = Mutex::new(());

fn layers_dir(state: &ApiState) -> PathBuf {
    state.workspace.paths.cell_layers_dir.clone()
}

pub(crate) fn load_layer(state: &ApiState, layer_id: &str) -> Result<CellLayer, ApiError> {
    validate_layer_id(layer_id).map_err(bad_request)?;
    let dir = layers_dir(state);
    if !CellLayer::path(&dir, layer_id).exists() {
        return Err(ApiError::not_found(format!("no cell layer '{layer_id}'")));
    }
    CellLayer::load(&dir, layer_id).map_err(|error| ApiError::internal(format!("{error:#}")))
}

#[derive(Serialize)]
pub(crate) struct LayerListResponse {
    layers: Vec<CellLayerSummary>,
}

/// `GET /v1/h3/layers`
pub(crate) async fn list_layers(
    State(state): State<ApiState>,
) -> Result<Json<LayerListResponse>, ApiError> {
    Ok(Json(LayerListResponse {
        layers: CellLayer::list(&layers_dir(&state))
            .map_err(|error| ApiError::internal(format!("{error:#}")))?,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LayerCellInput {
    cell: String,
    #[serde(default)]
    values: BTreeMap<String, f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LayerWriteRequest {
    layer_id: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    description: Option<String>,
    /// Keep the cells and columns the layer has and merge the new values.
    #[serde(default)]
    merge: bool,
    /// Replace a stored layer of the same id. Without `merge` or `replace`,
    /// the request only creates a new layer.
    #[serde(default)]
    replace: bool,
    /// Values per cell; a value replaces the one the cell had.
    #[serde(default)]
    cells: Vec<LayerCellInput>,
    /// A cell table or a point table as CSV text.
    #[serde(default)]
    csv: Option<String>,
    /// Aggregates the CSV rows as points into cells of this resolution.
    #[serde(default)]
    resolution: Option<u8>,
    #[serde(default = "default_lon_column")]
    lon_column: String,
    #[serde(default = "default_lat_column")]
    lat_column: String,
    /// CSV columns to import. Without it, every numeric column is summed.
    #[serde(default)]
    columns: Vec<ColumnSpec>,
}

fn default_lon_column() -> String {
    "lon".to_string()
}

fn default_lat_column() -> String {
    "lat".to_string()
}

#[derive(Serialize)]
pub(crate) struct LayerWriteResponse {
    layer: CellLayerSummary,
    /// CSV rows read and rows skipped for an invalid position.
    #[serde(skip_serializing_if = "Option::is_none")]
    csv_rows: Option<[usize; 2]>,
}

/// `POST /v1/h3/layers`: create, replace or extend a layer from inline
/// cells and from a CSV table.
pub(crate) async fn write_layer(
    State(state): State<ApiState>,
    Json(payload): Json<LayerWriteRequest>,
) -> Result<Json<LayerWriteResponse>, ApiError> {
    let dir = layers_dir(&state);
    let response = tokio::task::spawn_blocking(move || -> anyhow::Result<LayerWriteResponse> {
        let _guard = LAYER_WRITES.lock().unwrap_or_else(|e| e.into_inner());
        validate_layer_id(&payload.layer_id)?;
        let exists = CellLayer::path(&dir, &payload.layer_id).exists();
        if exists && !payload.merge && !payload.replace {
            anyhow::bail!(
                "cell layer '{}' exists; set merge or replace",
                payload.layer_id
            );
        }
        let mut layer = if payload.merge && exists {
            CellLayer::load(&dir, &payload.layer_id)?
        } else {
            CellLayer::new(&payload.layer_id)?
        };
        if let Some(label) = payload.label {
            layer.label = label;
        }
        if let Some(description) = payload.description {
            layer.description = description;
        }
        let mut csv_rows = None;
        if let Some(text) = &payload.csv {
            let resolution = payload.resolution.map(parse_resolution).transpose()?;
            let specs = if payload.columns.is_empty() {
                let mut specs =
                    default_column_specs(text, &[&payload.lon_column, &payload.lat_column])?;
                if specs.is_empty() {
                    specs.push(ColumnSpec::parse(":count=count")?);
                }
                specs
            } else {
                payload.columns.clone()
            };
            let mut builder = LayerBuilder::new(specs)?;
            let (rows, skipped) =
                builder.add_csv(text, resolution, &payload.lon_column, &payload.lat_column)?;
            csv_rows = Some([rows, skipped]);
            layer.merge(builder)?;
        }
        for input in &payload.cells {
            let cell = parse_cell(&input.cell)?;
            for (column, value) in &input.values {
                layer.paint([cell], column, PaintOp::Set, *value)?;
            }
        }
        layer.save(&dir)?;
        Ok(LayerWriteResponse {
            layer: layer.summary(),
            csv_rows,
        })
    })
    .await
    .map_err(|error| ApiError::internal(format!("writing the layer: {error}")))?
    .map_err(bad_request)?;
    info!(layer_id = %response.layer.layer_id, cells = response.layer.cell_count, "cell layer written");
    Ok(Json(response))
}

#[derive(Serialize)]
pub(crate) struct LayerResponse {
    summary: CellLayerSummary,
    layer: CellLayer,
}

/// `GET /v1/h3/layers/{layer_id}`: the layer as JSON, GeoJSON hexagons
/// (`format=geojson`) or CSV (`format=csv`).
pub(crate) async fn get_layer(
    State(state): State<ApiState>,
    AxumPath(layer_id): AxumPath<String>,
    Query(query): Query<H3FormatQuery>,
) -> Result<Response, ApiError> {
    let layer = load_layer(&state, &layer_id)?;
    if wants_geojson(query.format.as_deref()) {
        return geojson_response(layer.geojson());
    }
    if query
        .format
        .as_deref()
        .is_some_and(|format| format.eq_ignore_ascii_case("csv"))
    {
        let text = layer
            .to_csv()
            .map_err(|error| ApiError::internal(format!("{error:#}")))?;
        return Ok((
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/csv; charset=utf-8"),
            )],
            text,
        )
            .into_response());
    }
    Ok(Json(LayerResponse {
        summary: layer.summary(),
        layer,
    })
    .into_response())
}

/// `DELETE /v1/h3/layers/{layer_id}`
pub(crate) async fn delete_layer(
    State(state): State<ApiState>,
    AxumPath(layer_id): AxumPath<String>,
) -> Result<StatusCode, ApiError> {
    let _guard = LAYER_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    if CellLayer::remove(&layers_dir(&state), &layer_id).map_err(bad_request)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("no cell layer '{layer_id}'")))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PaintRequest {
    column: String,
    #[serde(default)]
    op: PaintOp,
    #[serde(default)]
    value: f64,
    /// Cells to paint.
    #[serde(default)]
    cells: Vec<String>,
    /// Paints every cell with its centre in `[min_lon, min_lat, max_lon,
    /// max_lat]`, at `resolution`.
    #[serde(default)]
    bbox: Option<[f64; 4]>,
    /// Paints every cell with its centre in this ring, at `resolution`.
    #[serde(default)]
    polygon: Option<Vec<[f64; 2]>>,
    /// Paints the cell of each `[lon, lat]` position, at `resolution`.
    #[serde(default)]
    points: Vec<[f64; 2]>,
    #[serde(default)]
    resolution: Option<u8>,
}

#[derive(Serialize)]
pub(crate) struct PaintResponse {
    changed: usize,
    layer: CellLayerSummary,
}

/// `POST /v1/h3/layers/{layer_id}/paint`: set, add to, scale or clear one
/// column on a list of cells or on the cells of an area.
pub(crate) async fn paint_layer(
    State(state): State<ApiState>,
    AxumPath(layer_id): AxumPath<String>,
    Json(payload): Json<PaintRequest>,
) -> Result<Json<PaintResponse>, ApiError> {
    let mut cells = payload
        .cells
        .iter()
        .map(|cell| parse_cell(cell))
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(bad_request)?;
    if payload.bbox.is_some() || payload.polygon.is_some() || !payload.points.is_empty() {
        let resolution = payload
            .resolution
            .ok_or_else(|| ApiError::bad_request("painting an area or points needs a resolution"))
            .and_then(|resolution| parse_resolution(resolution).map_err(bad_request))?;
        cells.extend(
            payload
                .points
                .iter()
                .filter_map(|point| netweevil_h3::cell_at(point[0], point[1], resolution)),
        );
        if let Some(bbox) = payload.bbox {
            cells.extend(cover_bbox(bbox, resolution, PAINT_CELL_LIMIT).map_err(bad_request)?);
        }
        if let Some(ring) = &payload.polygon {
            cells.extend(cover_polygon(ring, resolution, PAINT_CELL_LIMIT).map_err(bad_request)?);
        }
    }
    if cells.is_empty() {
        return Err(ApiError::bad_request(
            "pass cells, points, a bbox or a polygon to paint",
        ));
    }
    let _guard = LAYER_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let mut layer = load_layer(&state, &layer_id)?;
    let changed = layer
        .paint(cells, &payload.column, payload.op, payload.value)
        .map_err(bad_request)?;
    layer
        .save(&layers_dir(&state))
        .map_err(|error| ApiError::internal(format!("{error:#}")))?;
    Ok(Json(PaintResponse {
        changed,
        layer: layer.summary(),
    }))
}

fn default_weight() -> f64 {
    1.0
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WeightedPoint {
    lon: f64,
    lat: f64,
    #[serde(default = "default_weight")]
    weight: f64,
}

/// Weighted cells from a stored layer column, inline cells, or points.
#[derive(Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum CellSource {
    Layer {
        layer_id: String,
        column: String,
    },
    Cells {
        cells: Vec<WeightedCell>,
    },
    Points {
        points: Vec<WeightedPoint>,
        /// Resolution of the cells the points count in (default 8).
        #[serde(default)]
        resolution: Option<u8>,
    },
}

impl CellSource {
    pub(crate) fn resolve(&self, state: &ApiState) -> Result<Vec<WeightedCell>, ApiError> {
        match self {
            Self::Layer { layer_id, column } => load_layer(state, layer_id)?
                .weighted(column)
                .map_err(bad_request),
            Self::Cells { cells } => Ok(cells.clone()),
            Self::Points { points, resolution } => {
                let resolution = parse_resolution(resolution.unwrap_or(8)).map_err(bad_request)?;
                Ok(aggregate_points(
                    points
                        .iter()
                        .map(|point| (point.lon, point.lat, point.weight)),
                    &[resolution],
                )
                .0
                .into_iter()
                .map(|stat| WeightedCell {
                    cell: stat.cell,
                    weight: stat.weight,
                })
                .collect())
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SurgeRequest {
    demand: CellSource,
    supply: CellSource,
    #[serde(default)]
    config: SurgeConfig,
    /// Also plan moves of surplus supply to cells with unmet demand, from
    /// at most this many rings away.
    #[serde(default)]
    rebalance_rings: Option<u32>,
}

#[derive(Serialize)]
pub(crate) struct SurgeResponse {
    cells: Vec<SurgeCell>,
    #[serde(skip_serializing_if = "Option::is_none")]
    moves: Option<Vec<RebalanceMove>>,
}

/// `POST /v1/h3/surge`: the demand and supply balance per cell with its
/// price multiplier. Works without a loaded dataset.
pub(crate) async fn surge_handler(
    State(state): State<ApiState>,
    Query(query): Query<H3FormatQuery>,
    Json(payload): Json<SurgeRequest>,
) -> Result<Response, ApiError> {
    let demand = payload.demand.resolve(&state)?;
    let supply = payload.supply.resolve(&state)?;
    let cells = compute_surge(&demand, &supply, &payload.config).map_err(bad_request)?;
    if wants_geojson(query.format.as_deref()) {
        return geojson_response(cells_geojson(
            cells.iter().map(|cell| (cell.cell, properties(cell))),
        ));
    }
    let moves = payload
        .rebalance_rings
        .map(|rings| plan_rebalance(&cells, &payload.config, rings.min(50)));
    Ok(Json(SurgeResponse { cells, moves }).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispatchExecutionRequest {
    #[serde(default)]
    profile_id: Option<String>,
    request: DispatchRequest,
    /// Surge demand from a layer, cells or points; replaces
    /// `request.surge.demand`.
    #[serde(default)]
    surge_demand: Option<CellSource>,
    /// Surge supply from a layer, cells or points; replaces
    /// `request.surge.supply`.
    #[serde(default)]
    surge_supply: Option<CellSource>,
}

#[derive(Serialize)]
pub(crate) struct DispatchResponse {
    dataset_id: String,
    profile_id: String,
    result: DispatchResult,
}

/// `POST /v1/dispatch`: match ride requests to vehicles by network travel
/// time, with optional surge multipliers and fares.
pub(crate) async fn dispatch_handler(
    State(state): State<ApiState>,
    Json(payload): Json<DispatchExecutionRequest>,
) -> Result<Json<DispatchResponse>, ApiError> {
    let runtime = state.runtime()?;
    let profile = resolve_profile(runtime.as_ref(), payload.profile_id.as_deref())?;
    let profile_id = profile.document.profile.id.clone();
    let engine = profile.engine.clone();
    let mut request = payload.request;
    if payload.surge_demand.is_some() || payload.surge_supply.is_some() {
        let surge = request.surge.get_or_insert_with(Default::default);
        if let Some(source) = &payload.surge_demand {
            surge.demand = Some(source.resolve(&state)?);
        }
        if let Some(source) = &payload.surge_supply {
            surge.supply = Some(source.resolve(&state)?);
        }
    }
    info!(
        endpoint = "dispatch",
        profile_id = %profile_id,
        vehicles = request.vehicles.len(),
        requests = request.requests.len(),
        "request"
    );
    let result = execute_on_routing_worker(&runtime, move || execute_dispatch(&engine, &request))
        .await
        .map_err(ApiError::from_execution_error)?;
    Ok(Json(DispatchResponse {
        dataset_id: runtime.dataset_manifest.dataset_id.0.clone(),
        profile_id,
        result,
    }))
}
