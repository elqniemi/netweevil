//! `POST /v1/height`: DEM heights at points or along a polyline, read from
//! the elevation raster stored with a dataset imported with `--dem`.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use netweevil_core::RasterCrs;
use netweevil_core::geo::haversine_meters;
use serde::{Deserialize, Serialize};

use crate::dto::ExecutionResponse;
use crate::error::ApiError;
use crate::state::{ApiState, execute_on_routing_worker};

/// Largest number of positions sampled per request (after resampling).
const MAX_HEIGHT_POINTS: usize = 20_000;
/// Smallest accepted resampling interval, in metres.
const MIN_RESAMPLE_DISTANCE_M: f64 = 1.0;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HeightRequest {
    /// `[lon, lat]` positions.
    #[serde(default)]
    points: Vec<[f64; 2]>,
    /// Encoded polyline (Google algorithm, latitude first) instead of
    /// `points`.
    #[serde(default)]
    encoded_polyline: Option<String>,
    /// Decimal digits of `encoded_polyline` coordinates: 5 or 6.
    #[serde(default = "default_polyline_precision")]
    polyline_precision: u32,
    /// Return `[distance_m, height]` pairs along the shape.
    #[serde(default)]
    range: bool,
    /// Resample the shape every this many metres (its last point is kept)
    /// and return the resampled shape.
    #[serde(default)]
    resample_distance_m: Option<f64>,
}

fn default_polyline_precision() -> u32 {
    6
}

#[derive(Debug, Serialize)]
pub(crate) struct HeightContext {
    dataset_id: String,
    /// Vertical datum recorded at import, when one was given.
    vertical_datum: Option<String>,
    /// Horizontal CRS of the stored raster.
    crs: String,
}

#[derive(Debug, Default, Serialize, PartialEq)]
pub(crate) struct HeightResult {
    /// Heights in metres (null outside the raster or on nodata cells).
    #[serde(skip_serializing_if = "Option::is_none")]
    heights: Option<Vec<Option<f64>>>,
    /// `[distance_m, height]` along the shape, with `range: true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    range_heights: Option<Vec<(f64, Option<f64>)>>,
    /// The resampled shape as `[lon, lat]`, for point input.
    #[serde(skip_serializing_if = "Option::is_none")]
    shape: Option<Vec<[f64; 2]>>,
    /// The resampled shape, for encoded-polyline input.
    #[serde(skip_serializing_if = "Option::is_none")]
    encoded_polyline: Option<String>,
}

pub(crate) async fn height_handler(
    State(state): State<ApiState>,
    Json(request): Json<HeightRequest>,
) -> Result<Json<ExecutionResponse<HeightContext, HeightResult>>, ApiError> {
    let runtime = state.runtime()?;
    let sampler = runtime
        .elevation_sampler()
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "dataset '{}' has no elevation raster; re-import it with --dem",
                runtime.dataset_manifest.dataset_id.0
            ))
        })?;
    let (shape, encoded) = request_shape(&request).map_err(ApiError::bad_request)?;
    let source = &sampler.header().source;
    let service = HeightContext {
        dataset_id: runtime.dataset_manifest.dataset_id.0.clone(),
        vertical_datum: source.vertical_datum.clone(),
        crs: match &source.crs {
            RasterCrs::Geographic => "EPSG:4326".to_string(),
            RasterCrs::Proj4 { definition, label } => label.clone().unwrap_or(definition.clone()),
        },
    };
    let sampler = Arc::clone(&sampler);
    let result = execute_on_routing_worker(&runtime, move || {
        let heights = shape
            .iter()
            .map(|&[lon, lat]| {
                sampler
                    .sample(lon, lat)
                    .map(|height| height.map(|height| (height * 10.0).round() / 10.0))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(height_result(&request, shape, heights, encoded))
    })
    .await
    .map_err(ApiError::from_execution_error)?;
    Ok(Json(ExecutionResponse { service, result }))
}

/// The positions to sample and whether the input was an encoded polyline.
fn request_shape(request: &HeightRequest) -> Result<(Vec<[f64; 2]>, bool), String> {
    let (shape, encoded) = match (&request.encoded_polyline, request.points.is_empty()) {
        (Some(_), false) => return Err("give either points or encoded_polyline, not both".into()),
        (None, true) => return Err("height requires points or encoded_polyline".into()),
        (Some(polyline), true) => {
            if !(5..=6).contains(&request.polyline_precision) {
                return Err("polyline_precision must be 5 or 6".into());
            }
            (decode_polyline(polyline, request.polyline_precision)?, true)
        }
        (None, false) => (request.points.clone(), false),
    };
    if shape.is_empty() {
        return Err("the shape has no positions".into());
    }
    for [lon, lat] in &shape {
        if !(lon.is_finite() && lat.is_finite())
            || !(-180.0..=180.0).contains(lon)
            || !(-90.0..=90.0).contains(lat)
        {
            return Err(format!(
                "position [{lon}, {lat}] must have longitude in [-180, 180] and latitude in [-90, 90]"
            ));
        }
    }
    let shape = match request.resample_distance_m {
        None => shape,
        Some(interval) if interval.is_finite() && interval >= MIN_RESAMPLE_DISTANCE_M => {
            resample(&shape, interval, MAX_HEIGHT_POINTS)?
        }
        Some(_) => {
            return Err(format!(
                "resample_distance_m must be at least {MIN_RESAMPLE_DISTANCE_M}"
            ));
        }
    };
    if shape.len() > MAX_HEIGHT_POINTS {
        return Err(format!(
            "height accepts at most {MAX_HEIGHT_POINTS} positions; got {}",
            shape.len()
        ));
    }
    Ok((shape, encoded))
}

fn height_result(
    request: &HeightRequest,
    shape: Vec<[f64; 2]>,
    heights: Vec<Option<f64>>,
    encoded: bool,
) -> HeightResult {
    let mut result = HeightResult::default();
    if request.range {
        let mut distance = 0.0;
        result.range_heights = Some(
            heights
                .into_iter()
                .enumerate()
                .map(|(index, height)| {
                    if index > 0 {
                        let ([lon0, lat0], [lon1, lat1]) = (shape[index - 1], shape[index]);
                        distance += haversine_meters(lon0, lat0, lon1, lat1);
                    }
                    ((distance * 10.0).round() / 10.0, height)
                })
                .collect(),
        );
    } else {
        result.heights = Some(heights);
    }
    if request.resample_distance_m.is_some() {
        if encoded {
            result.encoded_polyline = Some(encode_polyline(&shape, request.polyline_precision));
        } else {
            result.shape = Some(shape);
        }
    }
    result
}

/// Points every `interval_m` metres along `shape` from its start, plus its
/// last point.
fn resample(shape: &[[f64; 2]], interval_m: f64, limit: usize) -> Result<Vec<[f64; 2]>, String> {
    let mut output = vec![shape[0]];
    let mut carried = 0.0;
    for pair in shape.windows(2) {
        let ([lon0, lat0], [lon1, lat1]) = (pair[0], pair[1]);
        let length = haversine_meters(lon0, lat0, lon1, lat1);
        let mut along = interval_m - carried;
        while along <= length {
            let t = along / length;
            output.push([lon0 + (lon1 - lon0) * t, lat0 + (lat1 - lat0) * t]);
            if output.len() > limit {
                return Err(format!(
                    "resampling produces more than {limit} positions; use a larger resample_distance_m"
                ));
            }
            along += interval_m;
        }
        carried = length - (along - interval_m);
    }
    let last = *shape.last().expect("shape is not empty");
    if output.last() != Some(&last) && (carried > 1e-6 || output.len() == 1) {
        output.push(last);
    }
    Ok(output)
}

/// Decodes an encoded polyline into `[lon, lat]` positions.
fn decode_polyline(encoded: &str, precision: u32) -> Result<Vec<[f64; 2]>, String> {
    let scale = 10_f64.powi(precision as i32);
    let mut bytes = encoded.bytes();
    let mut next = || -> Result<Option<i64>, String> {
        let mut result = 0_i64;
        let mut shift = 0;
        loop {
            let Some(byte) = bytes.next() else {
                return if shift == 0 {
                    Ok(None)
                } else {
                    Err("encoded_polyline ends inside a value".into())
                };
            };
            if !(63..=126).contains(&byte) || shift > 60 {
                return Err("encoded_polyline contains an invalid character".into());
            }
            let chunk = i64::from(byte - 63);
            result |= (chunk & 0x1f) << shift;
            shift += 5;
            if chunk < 0x20 {
                return Ok(Some(if result & 1 == 1 {
                    !(result >> 1)
                } else {
                    result >> 1
                }));
            }
        }
    };
    let (mut lat, mut lon) = (0_i64, 0_i64);
    let mut points = Vec::new();
    while let Some(delta_lat) = next()? {
        let delta_lon = next()?.ok_or("encoded_polyline has an odd number of values")?;
        lat += delta_lat;
        lon += delta_lon;
        points.push([lon as f64 / scale, lat as f64 / scale]);
    }
    Ok(points)
}

fn encode_polyline(points: &[[f64; 2]], precision: u32) -> String {
    let scale = 10_f64.powi(precision as i32);
    let mut output = String::new();
    let mut previous = (0_i64, 0_i64);
    let push = |value: i64, output: &mut String| {
        let mut value = if value < 0 { !(value << 1) } else { value << 1 };
        while value >= 0x20 {
            output.push(char::from((0x20 | (value & 0x1f)) as u8 + 63));
            value >>= 5;
        }
        output.push(char::from(value as u8 + 63));
    };
    for &[lon, lat] in points {
        let current = ((lat * scale).round() as i64, (lon * scale).round() as i64);
        push(current.0 - previous.0, &mut output);
        push(current.1 - previous.1, &mut output);
        previous = current;
    }
    output
}

#[cfg(test)]
mod tests;
