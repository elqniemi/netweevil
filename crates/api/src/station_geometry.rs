//! Optional source station surfaces beside an imported GTFS file, plus its
//! actual graph-bound boarding points. These are reference features, not routes.
use std::path::PathBuf;

use axum::Json;
use axum::extract::{Path, State};
use netweevil_transit::TransitStopBindingTarget;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ApiError;
use crate::state::ApiState;

#[derive(Deserialize)]
pub(crate) struct StationGeometryRequest {
    bbox: [f64; 4],
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    2000
}

fn validate_bbox(b: &[f64; 4]) -> Result<(), ApiError> {
    if !b.iter().all(|v| v.is_finite())
        || b[0] < -180.0
        || b[2] > 180.0
        || b[1] < -90.0
        || b[3] > 90.0
        || b[0] >= b[2]
        || b[1] >= b[3]
    {
        return Err(ApiError::bad_request(
            "bbox must be [west, south, east, north] in WGS84 with increasing bounds",
        ));
    }
    Ok(())
}

fn coordinate_bounds(value: &Value, bounds: &mut [f64; 4]) {
    let Some(values) = value.as_array() else {
        return;
    };
    if let (Some(x), Some(y)) = (
        values.first().and_then(Value::as_f64),
        values.get(1).and_then(Value::as_f64),
    ) {
        bounds[0] = bounds[0].min(x);
        bounds[1] = bounds[1].min(y);
        bounds[2] = bounds[2].max(x);
        bounds[3] = bounds[3].max(y);
    } else {
        for child in values {
            coordinate_bounds(child, bounds);
        }
    }
}

fn intersects(feature: &Value, bbox: &[f64; 4]) -> bool {
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    coordinate_bounds(&feature["geometry"]["coordinates"], &mut bounds);
    bounds[0] <= bbox[2] && bounds[2] >= bbox[0] && bounds[1] <= bbox[3] && bounds[3] >= bbox[1]
}

fn read_surfaces(path: PathBuf) -> Result<(bool, Vec<Value>), ApiError> {
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((false, Vec::new()));
        }
        Err(error) => {
            return Err(ApiError::internal(format!(
                "reading station geometry: {error}"
            )));
        }
    };
    if metadata.len() > 64 * 1024 * 1024 {
        return Err(ApiError::bad_request(
            "station geometry sidecar exceeds 64 MiB",
        ));
    }
    let value: Value = serde_json::from_slice(
        &std::fs::read(path).map_err(|error| ApiError::internal(error.to_string()))?,
    )
    .map_err(|error| ApiError::internal(format!("invalid station geometry: {error}")))?;
    if value["type"] != "FeatureCollection" {
        return Err(ApiError::internal(
            "station geometry must be a GeoJSON FeatureCollection",
        ));
    }
    let features = value["features"]
        .as_array()
        .ok_or_else(|| ApiError::internal("station geometry has no features array"))?;
    Ok((
        true,
        features
            .iter()
            .filter(|feature| {
                matches!(
                    feature["geometry"]["type"].as_str(),
                    Some("Polygon" | "MultiPolygon")
                )
            })
            .cloned()
            .collect(),
    ))
}

pub(crate) async fn station_geometry_handler(
    State(state): State<ApiState>,
    Path(feed_id): Path<String>,
    Json(request): Json<StationGeometryRequest>,
) -> Result<Json<Value>, ApiError> {
    validate_bbox(&request.bbox)?;
    let runtime = state.runtime()?;
    let feed = runtime
        .transit_feed(&feed_id)
        .ok_or_else(|| ApiError::not_found(format!("unknown transit feed '{feed_id}'")))?;
    // The filename is derived from the registered source, never a client path.
    // example.gtfs.zip -> example.gtfs.stations.geojson
    let sidecar = runtime
        .workspace_root
        .join(&feed.manifest.source_path)
        .with_extension("stations.geojson");
    let (surface_source_available, mut features) =
        tokio::task::spawn_blocking(move || read_surfaces(sidecar))
            .await
            .map_err(|error| ApiError::internal(error.to_string()))??;
    for feature in &mut features {
        if !feature["properties"].is_object() {
            feature["properties"] = json!({});
        }
        feature["properties"]["kind"] = json!("platform_surface");
        feature["properties"]["reference_only"] = json!(true);
    }
    for stop in &feed.router.bundle().stops {
        let Some(binding) = &stop.binding else {
            continue;
        };
        let (coordinates, attributes) = match binding {
            TransitStopBindingTarget::Coordinate {
                lon,
                lat,
                z,
                attribute_filter,
                ..
            } => {
                let coords = z.map_or_else(|| json!([lon, lat]), |z| json!([lon, lat, z]));
                (coords, json!(attribute_filter))
            }
            TransitStopBindingTarget::Node { node_id } => {
                let Some(node) = runtime.topology.nodes.get(*node_id as usize) else {
                    continue;
                };
                (json!([node.lon, node.lat, node.z]), json!({}))
            }
            // An edge binding needs its exact interpolated source geometry;
            // never fabricate a midpoint for a reference boarding marker.
            TransitStopBindingTarget::Edge { .. } => continue,
        };
        features.push(json!({"type":"Feature", "geometry":{"type":"Point", "coordinates":coordinates}, "properties":{
            "kind":"boarding_point", "stop_id":stop.stop_id, "name":stop.name,
            "station_code":attributes["mtr_station_code"], "binding_attributes":attributes,
            "source":"imported GTFS graph binding", "source_geometry_preserved":true, "reference_only":true
        }}));
    }
    let available = surface_source_available || !features.is_empty();
    features.retain(|feature| intersects(feature, &request.bbox));
    let matched = features.len();
    features.truncate(request.limit.clamp(1, 5000));
    Ok(Json(json!({"type":"FeatureCollection", "metadata":{
        "feed_id":feed_id, "available":available, "surface_source_available":surface_source_available,
        "matched":matched, "returned":features.len(), "truncated":features.len() < matched
    }, "features":features})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_geographic_viewports() {
        assert!(validate_bbox(&[114.1, 22.2, 114.3, 22.4]).is_ok());
        for bbox in [
            [114.3, 22.2, 114.1, 22.4],
            [0.0, 0.0, 181.0, 1.0],
            [0.0, 0.0, 1.0, 91.0],
            [0.0, 0.0, f64::NAN, 1.0],
        ] {
            assert!(validate_bbox(&bbox).is_err());
        }
    }

    #[test]
    fn intersecting_platform_surfaces_keep_their_original_xyz_and_holes() {
        let surface = json!({"type":"Feature", "geometry":{"type":"Polygon", "coordinates":[
            [[114.0,22.0,-30.93],[114.2,22.0,-30.93],[114.2,22.2,-30.93],[114.0,22.2,-30.93],[114.0,22.0,-30.93]],
            [[114.05,22.05,-30.93],[114.06,22.05,-30.93],[114.05,22.06,-30.93],[114.05,22.05,-30.93]]
        ]}});
        let original = surface.clone();
        assert!(intersects(&surface, &[114.08, 22.08, 114.12, 22.12]));
        assert!(!intersects(&surface, &[114.3, 22.3, 114.4, 22.4]));
        assert_eq!(surface, original);
        assert!(!intersects(&json!({}), &[114.0, 22.0, 114.2, 22.2]));
    }
}
