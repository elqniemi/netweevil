use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use arrow_array::{ArrayRef, BinaryArray, Float64Array, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use netweevil_query::{ServiceAreaGeometryType, ServiceAreaRequest, ServiceAreaResult};
use serde_json::json;

use super::common::*;
use super::gpkg::*;
use super::write_json;

pub(super) fn write_service_area_csv(
    path: &Path,
    _request: &ServiceAreaRequest,
    result: &ServiceAreaResult,
) -> Result<()> {
    let rows = result
        .features
        .iter()
        .map(|feature| {
            vec![
                result.analysis_id.clone(),
                optional_string(feature.origin_id.clone()),
                optional_string(feature.threshold_id.clone()),
                optional_string(feature.band_start_limit),
                feature.threshold_limit.to_string(),
                service_area_threshold_metric_name(feature.threshold_metric).to_string(),
                service_area_geometry_type_name(feature.geometry_type).to_string(),
                feature.fallback_used.to_string(),
                optional_string(feature.origin_component_id),
                optional_string(feature.origin_hop_distance_m),
                optional_string(feature.reachable_network_length_m),
                optional_string(feature.reachable_edge_count),
                optional_string(feature.edge_id),
                optional_string(feature.edge_index),
                optional_string(feature.source_way_id),
                optional_string(feature.from_node_id),
                optional_string(feature.to_node_id),
                optional_string(feature.start_fraction),
                optional_string(feature.end_fraction),
                optional_string(feature.start_cost),
                optional_string(feature.end_cost),
                optional_string(feature.segment_distance_m),
                optional_string(feature.segment_travel_time_s),
                feature
                    .geometry
                    .clone()
                    .unwrap_or(serde_json::Value::Null)
                    .to_string(),
            ]
        })
        .collect::<Vec<_>>();

    write_csv(
        path,
        &[
            "analysis_id",
            "origin_id",
            "threshold_id",
            "band_start_limit",
            "threshold_limit",
            "threshold_metric",
            "geometry_type",
            "fallback_used",
            "origin_component_id",
            "origin_hop_distance_m",
            "reachable_network_length_m",
            "reachable_edge_count",
            "edge_id",
            "edge_index",
            "source_way_id",
            "from_node_id",
            "to_node_id",
            "start_fraction",
            "end_fraction",
            "start_cost",
            "end_cost",
            "segment_distance_m",
            "segment_travel_time_s",
            "geometry_json",
        ],
        rows,
    )
}

pub(super) fn write_service_area_geojson(
    path: &Path,
    _request: &ServiceAreaRequest,
    result: &ServiceAreaResult,
) -> Result<()> {
    let features = result
        .features
        .iter()
        .map(|feature| {
            json!({
                "type": "Feature",
                "geometry": feature.geometry.clone().unwrap_or(serde_json::Value::Null),
                "properties": {
                    "analysis_id": result.analysis_id,
                    "origin_id": feature.origin_id,
                    "threshold_id": feature.threshold_id,
                    "band_start_limit": feature.band_start_limit,
                    "threshold_limit": feature.threshold_limit,
                    "threshold_metric": service_area_threshold_metric_name(feature.threshold_metric),
                    "geometry_type": service_area_geometry_type_name(feature.geometry_type),
                    "fallback_used": feature.fallback_used,
                    "origin_component_id": feature.origin_component_id,
                    "origin_hop_distance_m": feature.origin_hop_distance_m,
                    "reachable_network_length_m": feature.reachable_network_length_m,
                    "reachable_edge_count": feature.reachable_edge_count,
                    "edge_id": feature.edge_id,
                    "edge_index": feature.edge_index,
                    "source_way_id": feature.source_way_id,
                    "from_node_id": feature.from_node_id,
                    "to_node_id": feature.to_node_id,
                    "start_fraction": feature.start_fraction,
                    "end_fraction": feature.end_fraction,
                    "start_cost": feature.start_cost,
                    "end_cost": feature.end_cost,
                    "segment_distance_m": feature.segment_distance_m,
                    "segment_travel_time_s": feature.segment_travel_time_s,
                }
            })
        })
        .collect::<Vec<_>>();

    write_json(
        path,
        &json!({
            "type": "FeatureCollection",
            "features": features,
        }),
    )
}

pub(super) fn write_service_area_gpkg(
    path: &Path,
    _request: &ServiceAreaRequest,
    result: &ServiceAreaResult,
) -> Result<()> {
    let mut gpkg = GeoPackageWriter::create(path)?;
    write_service_area_gpkg_table(
        &mut gpkg,
        result,
        ServiceAreaGeometryType::Network,
        "service_area_network",
        "MULTILINESTRING",
    )?;
    write_service_area_gpkg_table(
        &mut gpkg,
        result,
        ServiceAreaGeometryType::Segment,
        "service_area_segments",
        "MULTILINESTRING",
    )?;
    write_service_area_gpkg_table(
        &mut gpkg,
        result,
        ServiceAreaGeometryType::Polygon,
        "service_area_polygon",
        "MULTIPOLYGON",
    )?;
    Ok(())
}

fn write_service_area_gpkg_table(
    gpkg: &mut GeoPackageWriter,
    result: &ServiceAreaResult,
    geometry_type: ServiceAreaGeometryType,
    table_name: &str,
    gpkg_geometry_type: &str,
) -> Result<()> {
    let rows = result
        .features
        .iter()
        .filter(|feature| feature.geometry_type == geometry_type)
        .filter_map(|feature| {
            let geometry = feature.geometry.as_ref()?;
            let wkb = service_area_geometry_wkb(geometry, geometry_type)?;
            let extent = extent_for_geometry_json(geometry)?;
            Some((feature, wkb, extent))
        })
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return Ok(());
    }

    let extent = rows
        .iter()
        .map(|(_, _, extent)| *extent)
        .reduce(combine_extents)
        .unwrap_or_default();

    gpkg.create_feature_table(
        table_name,
        &[
            ("analysis_id", "TEXT NOT NULL"),
            ("origin_id", "TEXT"),
            ("threshold_id", "TEXT"),
            ("band_start_limit", "REAL"),
            ("threshold_limit", "REAL NOT NULL"),
            ("threshold_metric", "TEXT NOT NULL"),
            ("geometry_type", "TEXT NOT NULL"),
            ("fallback_used", "INTEGER NOT NULL"),
            ("origin_component_id", "INTEGER"),
            ("origin_hop_distance_m", "REAL"),
            ("reachable_network_length_m", "REAL"),
            ("reachable_edge_count", "INTEGER"),
            ("edge_id", "INTEGER"),
            ("edge_index", "INTEGER"),
            ("source_way_id", "INTEGER"),
            ("from_node_id", "INTEGER"),
            ("to_node_id", "INTEGER"),
            ("start_fraction", "REAL"),
            ("end_fraction", "REAL"),
            ("start_cost", "REAL"),
            ("end_cost", "REAL"),
            ("segment_distance_m", "REAL"),
            ("segment_travel_time_s", "REAL"),
        ],
        gpkg_geometry_type,
        Some(extent),
    )?;

    for (feature, wkb, _) in rows {
        gpkg.insert_feature_wkb(
            table_name,
            &[
                ("analysis_id", SqlValue::Text(result.analysis_id.clone())),
                (
                    "origin_id",
                    SqlValue::NullableText(feature.origin_id.clone()),
                ),
                (
                    "threshold_id",
                    SqlValue::NullableText(feature.threshold_id.clone()),
                ),
                (
                    "band_start_limit",
                    SqlValue::NullableReal(feature.band_start_limit),
                ),
                ("threshold_limit", SqlValue::Real(feature.threshold_limit)),
                (
                    "threshold_metric",
                    SqlValue::Text(
                        service_area_threshold_metric_name(feature.threshold_metric).to_string(),
                    ),
                ),
                (
                    "geometry_type",
                    SqlValue::Text(
                        service_area_geometry_type_name(feature.geometry_type).to_string(),
                    ),
                ),
                (
                    "fallback_used",
                    SqlValue::Integer(if feature.fallback_used { 1 } else { 0 }),
                ),
                (
                    "origin_component_id",
                    SqlValue::NullableInteger(optional_u32_as_i64(feature.origin_component_id)),
                ),
                (
                    "origin_hop_distance_m",
                    SqlValue::NullableReal(feature.origin_hop_distance_m),
                ),
                (
                    "reachable_network_length_m",
                    SqlValue::NullableReal(feature.reachable_network_length_m),
                ),
                (
                    "reachable_edge_count",
                    SqlValue::NullableInteger(
                        feature.reachable_edge_count.map(|value| value as i64),
                    ),
                ),
                (
                    "edge_id",
                    SqlValue::NullableInteger(feature.edge_id.map(i64::from)),
                ),
                (
                    "edge_index",
                    SqlValue::NullableInteger(feature.edge_index.map(i64::from)),
                ),
                (
                    "source_way_id",
                    SqlValue::NullableInteger(feature.source_way_id),
                ),
                (
                    "from_node_id",
                    SqlValue::NullableInteger(feature.from_node_id.map(i64::from)),
                ),
                (
                    "to_node_id",
                    SqlValue::NullableInteger(feature.to_node_id.map(i64::from)),
                ),
                (
                    "start_fraction",
                    SqlValue::NullableReal(feature.start_fraction),
                ),
                ("end_fraction", SqlValue::NullableReal(feature.end_fraction)),
                ("start_cost", SqlValue::NullableReal(feature.start_cost)),
                ("end_cost", SqlValue::NullableReal(feature.end_cost)),
                (
                    "segment_distance_m",
                    SqlValue::NullableReal(feature.segment_distance_m),
                ),
                (
                    "segment_travel_time_s",
                    SqlValue::NullableReal(feature.segment_travel_time_s),
                ),
            ],
            &wkb,
        )?;
    }

    Ok(())
}

pub(super) fn write_service_area_parquet(
    path: &Path,
    _request: &ServiceAreaRequest,
    result: &ServiceAreaResult,
) -> Result<()> {
    let schema = Schema::new(vec![
        Field::new("analysis_id", DataType::Utf8, false),
        Field::new("origin_id", DataType::Utf8, true),
        Field::new("threshold_id", DataType::Utf8, true),
        Field::new("band_start_limit", DataType::Float64, true),
        Field::new("threshold_limit", DataType::Float64, false),
        Field::new("threshold_metric", DataType::Utf8, false),
        Field::new("geometry_type", DataType::Utf8, false),
        Field::new("fallback_used", DataType::Utf8, false),
        Field::new("origin_component_id", DataType::UInt64, true),
        Field::new("origin_hop_distance_m", DataType::Float64, true),
        Field::new("reachable_network_length_m", DataType::Float64, true),
        Field::new("reachable_edge_count", DataType::UInt64, true),
        Field::new("geometry_json", DataType::Utf8, false),
    ]);

    write_parquet_record_batch(
        path,
        schema,
        vec![
            Arc::new(StringArray::from(
                result
                    .features
                    .iter()
                    .map(|_| result.analysis_id.as_str())
                    .collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(StringArray::from(
                result
                    .features
                    .iter()
                    .map(|feature| feature.origin_id.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                result
                    .features
                    .iter()
                    .map(|feature| feature.threshold_id.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                result
                    .features
                    .iter()
                    .map(|feature| feature.band_start_limit)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                result
                    .features
                    .iter()
                    .map(|feature| feature.threshold_limit)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                result
                    .features
                    .iter()
                    .map(|feature| service_area_threshold_metric_name(feature.threshold_metric))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                result
                    .features
                    .iter()
                    .map(|feature| service_area_geometry_type_name(feature.geometry_type))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                result
                    .features
                    .iter()
                    .map(|feature| {
                        if feature.fallback_used {
                            "true"
                        } else {
                            "false"
                        }
                    })
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                result
                    .features
                    .iter()
                    .map(|feature| feature.origin_component_id.map(u64::from))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                result
                    .features
                    .iter()
                    .map(|feature| feature.origin_hop_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                result
                    .features
                    .iter()
                    .map(|feature| feature.reachable_network_length_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                result
                    .features
                    .iter()
                    .map(|feature| feature.reachable_edge_count)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                result
                    .features
                    .iter()
                    .map(|feature| {
                        feature
                            .geometry
                            .clone()
                            .unwrap_or(serde_json::Value::Null)
                            .to_string()
                    })
                    .collect::<Vec<_>>(),
            )),
        ],
    )
}

pub(super) fn write_service_area_geoparquet(
    path: &Path,
    _request: &ServiceAreaRequest,
    result: &ServiceAreaResult,
) -> Result<()> {
    let rows = result
        .features
        .iter()
        .filter_map(|feature| {
            let geometry = feature.geometry.as_ref()?;
            let wkb = service_area_geometry_wkb(geometry, feature.geometry_type)?;
            let extent = extent_for_geometry_json(geometry)?;
            Some((feature, wkb, extent))
        })
        .collect::<Vec<_>>();

    let extent = rows
        .iter()
        .map(|(_, _, extent)| *extent)
        .reduce(combine_extents)
        .unwrap_or_default();

    let schema = geoparquet_schema_with_types(
        vec![
            Field::new("analysis_id", DataType::Utf8, false),
            Field::new("origin_id", DataType::Utf8, true),
            Field::new("threshold_id", DataType::Utf8, true),
            Field::new("band_start_limit", DataType::Float64, true),
            Field::new("threshold_limit", DataType::Float64, false),
            Field::new("threshold_metric", DataType::Utf8, false),
            Field::new("geometry_type", DataType::Utf8, false),
            Field::new("fallback_used", DataType::Utf8, false),
            Field::new("origin_component_id", DataType::UInt64, true),
            Field::new("origin_hop_distance_m", DataType::Float64, true),
            Field::new("reachable_network_length_m", DataType::Float64, true),
            Field::new("reachable_edge_count", DataType::UInt64, true),
            Field::new("geometry", DataType::Binary, false),
        ],
        extent,
        &["MultiLineString", "MultiPolygon"],
    );

    write_parquet_record_batch(
        path,
        schema,
        vec![
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|_| result.analysis_id.as_str())
                    .collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(feature, _, _)| feature.origin_id.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(feature, _, _)| feature.threshold_id.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(feature, _, _)| feature.band_start_limit)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(feature, _, _)| feature.threshold_limit)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(feature, _, _)| {
                        service_area_threshold_metric_name(feature.threshold_metric)
                    })
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(feature, _, _)| service_area_geometry_type_name(feature.geometry_type))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(feature, _, _)| {
                        if feature.fallback_used {
                            "true"
                        } else {
                            "false"
                        }
                    })
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|(feature, _, _)| feature.origin_component_id.map(u64::from))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(feature, _, _)| feature.origin_hop_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(feature, _, _)| feature.reachable_network_length_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|(feature, _, _)| feature.reachable_edge_count)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(BinaryArray::from(
                rows.iter()
                    .map(|(_, wkb, _)| wkb.as_slice())
                    .collect::<Vec<_>>(),
            )),
        ],
    )
}

fn service_area_threshold_metric_name(
    metric: netweevil_query::ServiceAreaThresholdMetric,
) -> &'static str {
    match metric {
        netweevil_query::ServiceAreaThresholdMetric::DistanceM => "distance_m",
        netweevil_query::ServiceAreaThresholdMetric::TravelTimeS => "travel_time_s",
    }
}

fn service_area_geometry_type_name(geometry_type: ServiceAreaGeometryType) -> &'static str {
    match geometry_type {
        ServiceAreaGeometryType::Network => "network",
        ServiceAreaGeometryType::Polygon => "polygon",
        ServiceAreaGeometryType::Segment => "segment",
    }
}

fn extent_for_geometry_json(geometry: &serde_json::Value) -> Option<Extent> {
    match geometry.get("type")?.as_str()? {
        "LineString" => {
            let coords = geojson_coords_vec(geometry.get("coordinates")?)?;
            Some(extent_for_features([coords.as_slice()]))
        }
        "MultiLineString" => {
            let lines = geometry.get("coordinates")?.as_array()?;
            let coords = lines
                .iter()
                .map(geojson_coords_vec)
                .collect::<Option<Vec<_>>>()?;
            Some(extent_for_features(coords.iter().map(Vec::as_slice)))
        }
        "Polygon" => {
            let rings = geometry.get("coordinates")?.as_array()?;
            let coords = rings
                .iter()
                .map(geojson_coords_vec)
                .collect::<Option<Vec<_>>>()?;
            Some(extent_for_features(coords.iter().map(Vec::as_slice)))
        }
        "MultiPolygon" => {
            let polygons = geometry.get("coordinates")?.as_array()?;
            let coords = polygons
                .iter()
                .flat_map(|polygon| polygon.as_array().into_iter().flatten())
                .map(geojson_coords_vec)
                .collect::<Option<Vec<_>>>()?;
            Some(extent_for_features(coords.iter().map(Vec::as_slice)))
        }
        _ => None,
    }
}

fn geojson_coords_vec(value: &serde_json::Value) -> Option<Vec<[f64; 2]>> {
    serde_json::from_value::<Vec<[f64; 2]>>(value.clone()).ok()
}

fn service_area_geometry_wkb(
    geometry: &serde_json::Value,
    geometry_type: ServiceAreaGeometryType,
) -> Option<Vec<u8>> {
    match geometry_type {
        ServiceAreaGeometryType::Network | ServiceAreaGeometryType::Segment => {
            let lines = match geometry.get("type")?.as_str()? {
                "LineString" => vec![
                    serde_json::from_value::<Vec<[f64; 2]>>(geometry.get("coordinates")?.clone())
                        .ok()?,
                ],
                "MultiLineString" => serde_json::from_value::<Vec<Vec<[f64; 2]>>>(
                    geometry.get("coordinates")?.clone(),
                )
                .ok()?,
                _ => return None,
            };
            Some(wkb_multilinestring(&lines))
        }
        ServiceAreaGeometryType::Polygon => {
            let polygons = match geometry.get("type")?.as_str()? {
                "Polygon" => vec![
                    serde_json::from_value::<Vec<Vec<[f64; 2]>>>(
                        geometry.get("coordinates")?.clone(),
                    )
                    .ok()?,
                ],
                "MultiPolygon" => serde_json::from_value::<Vec<Vec<Vec<[f64; 2]>>>>(
                    geometry.get("coordinates")?.clone(),
                )
                .ok()?,
                _ => return None,
            };
            Some(wkb_multipolygon(&polygons))
        }
    }
}

fn combine_extents(left: Extent, right: Extent) -> Extent {
    Extent {
        min_x: left.min_x.min(right.min_x),
        min_y: left.min_y.min(right.min_y),
        max_x: left.max_x.max(right.max_x),
        max_y: left.max_y.max(right.max_y),
    }
}

#[cfg(test)]
mod tests {
    use crate::output::{temp_path, write_service_area_result};

    use netweevil_query::{
        AnalysisOutcome, LabeledPoint, ServiceAreaBandMode, ServiceAreaBoundaryMode,
        ServiceAreaFeature, ServiceAreaGeometryType, ServiceAreaMultiOriginMode,
        ServiceAreaOutputMode, ServiceAreaRequest, ServiceAreaResult, ServiceAreaThresholdMetric,
    };

    use std::fs;

    #[test]
    fn writes_service_area_geojson() {
        let path = temp_path("service_area.geojson");
        let request = ServiceAreaRequest {
            analysis_id: "sa_demo".to_string(),
            origins: vec![LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0,
                lat: 53.0,
            }],
            thresholds: vec![],
            snap: Default::default(),
            connectivity: Default::default(),
            fallback: Default::default(),
            output_mode: ServiceAreaOutputMode::Both,
            band_mode: ServiceAreaBandMode::Cumulative,
            boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
            multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
            polygon: Default::default(),
            returns: Default::default(),
        };
        let result = ServiceAreaResult {
            analysis_id: "sa_demo".to_string(),
            outcome: AnalysisOutcome::Legal,
            output_mode: ServiceAreaOutputMode::Both,
            band_mode: ServiceAreaBandMode::Cumulative,
            boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
            multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
            origin_count: 1,
            processed_origin_count: 1,
            skipped_origin_count: 0,
            fallback_origin_count: 0,
            threshold_count: 1,
            features: vec![ServiceAreaFeature {
                origin_id: Some("origin".to_string()),
                band_start_limit: None,
                threshold_id: Some("t1".to_string()),
                threshold_limit: 300.0,
                threshold_metric: ServiceAreaThresholdMetric::DistanceM,
                geometry_type: ServiceAreaGeometryType::Network,
                fallback_used: false,
                origin_component_id: Some(0),
                origin_hop_distance_m: None,
                reachable_network_length_m: Some(300.0),
                reachable_edge_count: Some(2),
                edge_id: None,
                edge_index: None,
                source_way_id: None,
                from_node_id: None,
                to_node_id: None,
                start_fraction: None,
                end_fraction: None,
                start_cost: None,
                end_cost: None,
                segment_distance_m: None,
                segment_travel_time_s: None,
                geometry: Some(serde_json::json!({
                    "type": "MultiLineString",
                    "coordinates": [[[6.0, 53.0], [6.1, 53.1]]],
                })),
            }],
            summaries: vec![],
            segments: vec![],
            diagnostics: vec![],
            warnings: vec![],
        };

        write_service_area_result(&path, &request, &result).expect("geojson written");

        let raw = fs::read_to_string(&path).expect("geojson readable");
        assert!(raw.contains("FeatureCollection"));
        assert!(raw.contains("sa_demo"));
        assert!(raw.contains("MultiLineString"));

        fs::remove_file(path).ok();
    }
}
