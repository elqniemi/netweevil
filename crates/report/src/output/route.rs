use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{ArrayRef, BinaryArray, Float64Array, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use netweevil_query::{RouteRequest, RouteResult};
use serde_json::json;

use super::common::*;
use super::gpkg::*;
use super::write_json;

pub(super) fn write_route_csv(
    path: &Path,
    request: &RouteRequest,
    result: &RouteResult,
) -> Result<()> {
    let geometry_wkt = result
        .geometry
        .as_deref()
        .map(linestring_wkt)
        .unwrap_or_default();
    let warnings = result.warnings.join(" | ");
    let diagnostics = diagnostics_json(&result.diagnostics);

    write_csv(
        path,
        &[
            "route_id",
            "origin_id",
            "destination_id",
            "outcome",
            "fallback_used",
            "origin_component_id",
            "destination_component_id",
            "origin_hop_distance_m",
            "destination_hop_distance_m",
            "origin_snap_distance_m",
            "destination_snap_distance_m",
            "total_distance_m",
            "total_travel_time_s",
            "total_generalized_cost",
            "illegal_movement_penalty_s",
            "illegal_movement_penalty_cost",
            "violation_count",
            "violation_types_json",
            "violations_json",
            "segment_count",
            "diagnostics_json",
            "geometry_wkt",
            "warnings",
        ],
        vec![vec![
            result.route_id.clone(),
            request.origin.id.clone(),
            request.destination.id.clone(),
            outcome_name(result.outcome).to_string(),
            result.fallback_used.to_string(),
            optional_string(result.origin.component_id),
            optional_string(result.destination.component_id),
            optional_string(result.origin_hop_distance_m),
            optional_string(result.destination_hop_distance_m),
            result.origin.snap_distance_m.to_string(),
            result.destination.snap_distance_m.to_string(),
            result.summary.total_distance_m.to_string(),
            result.summary.total_travel_time_s.to_string(),
            result.summary.total_generalized_cost.to_string(),
            result.summary.illegal_movement_penalty_s.to_string(),
            result.summary.illegal_movement_penalty_cost.to_string(),
            result.summary.violation_count.to_string(),
            diagnostics_json(&result.summary.violation_types),
            diagnostics_json(&result.violations),
            result.summary.segment_count.to_string(),
            diagnostics,
            geometry_wkt,
            warnings,
        ]],
    )
}

pub(super) fn write_route_geojson(
    path: &Path,
    request: &RouteRequest,
    result: &RouteResult,
) -> Result<()> {
    let geometry = route_geometry_geojson(result)?;
    let mut features = vec![json!({
        "type": "Feature",
        "geometry": geometry,
        "properties": {
            "route_id": result.route_id,
            "route_rank": 0,
            "alternative_index": serde_json::Value::Null,
            "origin_id": request.origin.id,
            "destination_id": request.destination.id,
            "outcome": outcome_name(result.outcome),
            "fallback_used": result.fallback_used,
            "origin_component_id": result.origin.component_id,
            "destination_component_id": result.destination.component_id,
            "origin_hop_distance_m": result.origin_hop_distance_m,
            "destination_hop_distance_m": result.destination_hop_distance_m,
            "origin_snap_distance_m": result.origin.snap_distance_m,
            "destination_snap_distance_m": result.destination.snap_distance_m,
            "total_distance_m": result.summary.total_distance_m,
            "total_travel_time_s": result.summary.total_travel_time_s,
            "total_generalized_cost": result.summary.total_generalized_cost,
            "illegal_movement_penalty_s": result.summary.illegal_movement_penalty_s,
            "illegal_movement_penalty_cost": result.summary.illegal_movement_penalty_cost,
            "violation_count": result.summary.violation_count,
            "violation_types": result.summary.violation_types,
            "violations": result.violations,
            "segment_count": result.summary.segment_count,
            "diagnostics": result.diagnostics,
            "warnings": result.warnings,
        }
    })];
    for alternative in &result.alternatives {
        if let Some(geometry) = alternative.geometry.as_ref() {
            features.push(json!({
                "type": "Feature",
                "geometry": {
                    "type": "LineString",
                    "coordinates": geometry,
                },
                "properties": {
                    "route_id": result.route_id,
                    "route_rank": alternative.rank,
                    "alternative_index": alternative.alternative_index,
                    "origin_id": request.origin.id,
                    "destination_id": request.destination.id,
                    "total_distance_m": alternative.summary.total_distance_m,
                    "total_travel_time_s": alternative.summary.total_travel_time_s,
                    "total_generalized_cost": alternative.summary.total_generalized_cost,
                    "violation_count": alternative.summary.violation_count,
                    "violation_types": alternative.summary.violation_types,
                    "violations": alternative.violations,
                    "segment_count": alternative.summary.segment_count,
                    "diagnostics": alternative.diagnostics,
                    "warnings": alternative.warnings,
                }
            }));
        }
    }
    let feature_collection = json!({
        "type": "FeatureCollection",
        "features": features
    });
    write_json(path, &feature_collection)
}

pub(super) fn write_route_gpkg(
    path: &Path,
    request: &RouteRequest,
    result: &RouteResult,
) -> Result<()> {
    let coords = route_coords(result)?;
    let mut gpkg = GeoPackageWriter::create(path)?;
    gpkg.create_feature_table(
        "route_result",
        &[
            ("route_id", "TEXT NOT NULL"),
            ("origin_id", "TEXT NOT NULL"),
            ("destination_id", "TEXT NOT NULL"),
            ("outcome", "TEXT NOT NULL"),
            ("fallback_used", "INTEGER NOT NULL"),
            ("origin_component_id", "INTEGER"),
            ("destination_component_id", "INTEGER"),
            ("origin_hop_distance_m", "REAL"),
            ("destination_hop_distance_m", "REAL"),
            ("origin_snap_distance_m", "REAL NOT NULL"),
            ("destination_snap_distance_m", "REAL NOT NULL"),
            ("total_distance_m", "INTEGER NOT NULL"),
            ("total_travel_time_s", "REAL NOT NULL"),
            ("total_generalized_cost", "REAL NOT NULL"),
            ("illegal_movement_penalty_s", "REAL NOT NULL"),
            ("illegal_movement_penalty_cost", "REAL NOT NULL"),
            ("violation_count", "INTEGER NOT NULL"),
            ("violation_types_json", "TEXT NOT NULL"),
            ("violations_json", "TEXT NOT NULL"),
            ("segment_count", "INTEGER NOT NULL"),
            ("diagnostics_json", "TEXT NOT NULL"),
            ("warnings", "TEXT NOT NULL"),
        ],
        "LINESTRING",
        Some(extent_for_features([coords.as_slice()])),
    )?;
    gpkg.insert_feature(
        "route_result",
        &[
            ("route_id", SqlValue::Text(result.route_id.clone())),
            ("origin_id", SqlValue::Text(request.origin.id.clone())),
            (
                "destination_id",
                SqlValue::Text(request.destination.id.clone()),
            ),
            (
                "outcome",
                SqlValue::Text(outcome_name(result.outcome).to_string()),
            ),
            (
                "fallback_used",
                SqlValue::Integer(if result.fallback_used { 1 } else { 0 }),
            ),
            (
                "origin_component_id",
                SqlValue::NullableInteger(optional_u32_as_i64(result.origin.component_id)),
            ),
            (
                "destination_component_id",
                SqlValue::NullableInteger(optional_u32_as_i64(result.destination.component_id)),
            ),
            (
                "origin_hop_distance_m",
                SqlValue::NullableReal(result.origin_hop_distance_m),
            ),
            (
                "destination_hop_distance_m",
                SqlValue::NullableReal(result.destination_hop_distance_m),
            ),
            (
                "origin_snap_distance_m",
                SqlValue::Real(result.origin.snap_distance_m),
            ),
            (
                "destination_snap_distance_m",
                SqlValue::Real(result.destination.snap_distance_m),
            ),
            (
                "total_distance_m",
                SqlValue::Integer(result.summary.total_distance_m as i64),
            ),
            (
                "total_travel_time_s",
                SqlValue::Real(result.summary.total_travel_time_s),
            ),
            (
                "total_generalized_cost",
                SqlValue::Real(result.summary.total_generalized_cost),
            ),
            (
                "illegal_movement_penalty_s",
                SqlValue::Real(result.summary.illegal_movement_penalty_s),
            ),
            (
                "illegal_movement_penalty_cost",
                SqlValue::Real(result.summary.illegal_movement_penalty_cost),
            ),
            (
                "violation_count",
                SqlValue::Integer(result.summary.violation_count as i64),
            ),
            (
                "violation_types_json",
                SqlValue::Text(diagnostics_json(&result.summary.violation_types)),
            ),
            (
                "violations_json",
                SqlValue::Text(diagnostics_json(&result.violations)),
            ),
            (
                "segment_count",
                SqlValue::Integer(result.summary.segment_count as i64),
            ),
            (
                "diagnostics_json",
                SqlValue::Text(diagnostics_json(&result.diagnostics)),
            ),
            ("warnings", SqlValue::Text(result.warnings.join(" | "))),
        ],
        &coords,
    )?;
    Ok(())
}

pub(super) fn write_route_parquet(
    path: &Path,
    request: &RouteRequest,
    result: &RouteResult,
) -> Result<()> {
    let geometry_wkt = result
        .geometry
        .as_deref()
        .map(linestring_wkt)
        .unwrap_or_default();
    let warnings = result.warnings.join(" | ");
    let diagnostics = diagnostics_json(&result.diagnostics);
    let schema = Schema::new(vec![
        Field::new("route_id", DataType::Utf8, false),
        Field::new("origin_id", DataType::Utf8, false),
        Field::new("destination_id", DataType::Utf8, false),
        Field::new("outcome", DataType::Utf8, false),
        Field::new("fallback_used", DataType::Utf8, false),
        Field::new("origin_component_id", DataType::UInt64, true),
        Field::new("destination_component_id", DataType::UInt64, true),
        Field::new("origin_hop_distance_m", DataType::Float64, true),
        Field::new("destination_hop_distance_m", DataType::Float64, true),
        Field::new("origin_snap_distance_m", DataType::Float64, false),
        Field::new("destination_snap_distance_m", DataType::Float64, false),
        Field::new("total_distance_m", DataType::UInt64, false),
        Field::new("total_travel_time_s", DataType::Float64, false),
        Field::new("total_generalized_cost", DataType::Float64, false),
        Field::new("illegal_movement_penalty_s", DataType::Float64, false),
        Field::new("illegal_movement_penalty_cost", DataType::Float64, false),
        Field::new("violation_count", DataType::UInt64, false),
        Field::new("violation_types_json", DataType::Utf8, false),
        Field::new("violations_json", DataType::Utf8, false),
        Field::new("segment_count", DataType::UInt64, false),
        Field::new("diagnostics_json", DataType::Utf8, false),
        Field::new("geometry_wkt", DataType::Utf8, false),
        Field::new("warnings", DataType::Utf8, false),
    ]);
    write_parquet_record_batch(
        path,
        schema,
        vec![
            Arc::new(StringArray::from(vec![result.route_id.as_str()])) as ArrayRef,
            Arc::new(StringArray::from(vec![request.origin.id.as_str()])),
            Arc::new(StringArray::from(vec![request.destination.id.as_str()])),
            Arc::new(StringArray::from(vec![outcome_name(result.outcome)])),
            Arc::new(StringArray::from(vec![if result.fallback_used {
                "true"
            } else {
                "false"
            }])),
            Arc::new(UInt64Array::from(vec![
                result.origin.component_id.map(u64::from),
            ])),
            Arc::new(UInt64Array::from(vec![
                result.destination.component_id.map(u64::from),
            ])),
            Arc::new(Float64Array::from(vec![result.origin_hop_distance_m])),
            Arc::new(Float64Array::from(vec![result.destination_hop_distance_m])),
            Arc::new(Float64Array::from(vec![result.origin.snap_distance_m])),
            Arc::new(Float64Array::from(vec![result.destination.snap_distance_m])),
            Arc::new(UInt64Array::from(vec![result.summary.total_distance_m])),
            Arc::new(Float64Array::from(vec![result.summary.total_travel_time_s])),
            Arc::new(Float64Array::from(vec![
                result.summary.total_generalized_cost,
            ])),
            Arc::new(Float64Array::from(vec![
                result.summary.illegal_movement_penalty_s,
            ])),
            Arc::new(Float64Array::from(vec![
                result.summary.illegal_movement_penalty_cost,
            ])),
            Arc::new(UInt64Array::from(vec![
                result.summary.violation_count as u64,
            ])),
            Arc::new(StringArray::from(vec![diagnostics_json(
                &result.summary.violation_types,
            )])),
            Arc::new(StringArray::from(vec![diagnostics_json(
                &result.violations,
            )])),
            Arc::new(UInt64Array::from(vec![result.summary.segment_count as u64])),
            Arc::new(StringArray::from(vec![diagnostics.as_str()])),
            Arc::new(StringArray::from(vec![geometry_wkt.as_str()])),
            Arc::new(StringArray::from(vec![warnings.as_str()])),
        ],
    )
}

pub(super) fn write_route_geoparquet(
    path: &Path,
    request: &RouteRequest,
    result: &RouteResult,
) -> Result<()> {
    let coords = route_coords(result)?;
    let geometry = wkb_linestring(&coords);
    let warnings = result.warnings.join(" | ");
    let diagnostics = diagnostics_json(&result.diagnostics);
    let extent = extent_for_features([coords.as_slice()]);
    let schema = geoparquet_schema(
        vec![
            Field::new("route_id", DataType::Utf8, false),
            Field::new("origin_id", DataType::Utf8, false),
            Field::new("destination_id", DataType::Utf8, false),
            Field::new("outcome", DataType::Utf8, false),
            Field::new("fallback_used", DataType::Utf8, false),
            Field::new("origin_component_id", DataType::UInt64, true),
            Field::new("destination_component_id", DataType::UInt64, true),
            Field::new("origin_hop_distance_m", DataType::Float64, true),
            Field::new("destination_hop_distance_m", DataType::Float64, true),
            Field::new("origin_snap_distance_m", DataType::Float64, false),
            Field::new("destination_snap_distance_m", DataType::Float64, false),
            Field::new("total_distance_m", DataType::UInt64, false),
            Field::new("total_travel_time_s", DataType::Float64, false),
            Field::new("total_generalized_cost", DataType::Float64, false),
            Field::new("illegal_movement_penalty_s", DataType::Float64, false),
            Field::new("illegal_movement_penalty_cost", DataType::Float64, false),
            Field::new("violation_count", DataType::UInt64, false),
            Field::new("violation_types_json", DataType::Utf8, false),
            Field::new("violations_json", DataType::Utf8, false),
            Field::new("segment_count", DataType::UInt64, false),
            Field::new("diagnostics_json", DataType::Utf8, false),
            Field::new("warnings", DataType::Utf8, false),
            Field::new("geometry", DataType::Binary, false),
        ],
        extent,
    );
    write_parquet_record_batch(
        path,
        schema,
        vec![
            Arc::new(StringArray::from(vec![result.route_id.as_str()])) as ArrayRef,
            Arc::new(StringArray::from(vec![request.origin.id.as_str()])),
            Arc::new(StringArray::from(vec![request.destination.id.as_str()])),
            Arc::new(StringArray::from(vec![outcome_name(result.outcome)])),
            Arc::new(StringArray::from(vec![if result.fallback_used {
                "true"
            } else {
                "false"
            }])),
            Arc::new(UInt64Array::from(vec![
                result.origin.component_id.map(u64::from),
            ])),
            Arc::new(UInt64Array::from(vec![
                result.destination.component_id.map(u64::from),
            ])),
            Arc::new(Float64Array::from(vec![result.origin_hop_distance_m])),
            Arc::new(Float64Array::from(vec![result.destination_hop_distance_m])),
            Arc::new(Float64Array::from(vec![result.origin.snap_distance_m])),
            Arc::new(Float64Array::from(vec![result.destination.snap_distance_m])),
            Arc::new(UInt64Array::from(vec![result.summary.total_distance_m])),
            Arc::new(Float64Array::from(vec![result.summary.total_travel_time_s])),
            Arc::new(Float64Array::from(vec![
                result.summary.total_generalized_cost,
            ])),
            Arc::new(Float64Array::from(vec![
                result.summary.illegal_movement_penalty_s,
            ])),
            Arc::new(Float64Array::from(vec![
                result.summary.illegal_movement_penalty_cost,
            ])),
            Arc::new(UInt64Array::from(vec![
                result.summary.violation_count as u64,
            ])),
            Arc::new(StringArray::from(vec![diagnostics_json(
                &result.summary.violation_types,
            )])),
            Arc::new(StringArray::from(vec![diagnostics_json(
                &result.violations,
            )])),
            Arc::new(UInt64Array::from(vec![result.summary.segment_count as u64])),
            Arc::new(StringArray::from(vec![diagnostics.as_str()])),
            Arc::new(StringArray::from(vec![warnings.as_str()])),
            Arc::new(BinaryArray::from(vec![geometry.as_slice()])),
        ],
    )
}

pub(super) fn route_coords(result: &RouteResult) -> Result<Vec<[f64; 2]>> {
    let geometry = result.geometry.as_ref().context(
        "route export requires geometry; request `returns.geometry: full` or use a JSON output",
    )?;
    Ok(coerce_linestring_coords(geometry))
}

fn route_geometry_geojson(result: &RouteResult) -> Result<serde_json::Value> {
    Ok(json!({
        "type": "LineString",
        "coordinates": route_coords(result)?,
    }))
}

#[cfg(test)]
mod tests {
    use crate::output::{temp_path, write_route_result};

    use netweevil_profile::ReturnConfig;
    use netweevil_query::{
        AnalysisOutcome, LabeledPoint, RouteRequest, RouteResult, RouteSummary, SnappedPoint,
    };
    use rusqlite::Connection;

    use std::fs;

    #[test]
    fn writes_route_geopackage() {
        let path = temp_path("route.gpkg");
        let request = RouteRequest {
            route_id: "route_1".to_string(),
            origin: LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: LabeledPoint {
                id: "destination".to_string(),
                lon: 6.2,
                lat: 53.2,
            },
            snap: Default::default(),
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
            alternatives: Default::default(),
        };
        let result = RouteResult {
            route_id: "route_1".to_string(),
            origin: SnappedPoint {
                point_id: "origin".to_string(),
                requested_lon: 6.0,
                requested_lat: 53.0,
                snapped_node_id: 1,
                snapped_lon: 6.0,
                snapped_lat: 53.0,
                snap_distance_m: 10.0,
                snapped_edge_id: None,
                snapped_edge_fraction: None,
                snapped_from_node_id: None,
                snapped_to_node_id: None,
                component_id: Some(0),
            },
            destination: SnappedPoint {
                point_id: "destination".to_string(),
                requested_lon: 6.2,
                requested_lat: 53.2,
                snapped_node_id: 2,
                snapped_lon: 6.2,
                snapped_lat: 53.2,
                snap_distance_m: 20.0,
                snapped_edge_id: None,
                snapped_edge_fraction: None,
                snapped_from_node_id: None,
                snapped_to_node_id: None,
                component_id: Some(0),
            },
            outcome: AnalysisOutcome::Legal,
            fallback_used: false,
            origin_hop_distance_m: None,
            destination_hop_distance_m: None,
            summary: RouteSummary {
                network_distance_m: 1_000,
                network_travel_time_s: 120.0,
                network_generalized_cost: 120.0,
                illegal_movement_penalty_s: 0.0,
                illegal_movement_penalty_cost: 0.0,
                violation_count: 0,
                violation_types: vec![],
                total_distance_m: 1_000,
                total_travel_time_s: 120.0,
                total_generalized_cost: 120.0,
                segment_count: 2,
            },
            node_path: vec![1, 2, 3],
            edge_path: vec![10, 11],
            geometry: Some(vec![[6.0, 53.0], [6.1, 53.1], [6.2, 53.2]]),
            hop_segments: vec![],
            segments: None,
            breakdowns: None,
            violations: vec![],
            diagnostics: vec![],
            warnings: vec![],
            alternatives: vec![],
        };

        write_route_result(&path, &request, &result).expect("gpkg written");

        let connection = Connection::open(&path).expect("gpkg opens");
        let feature_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM route_result", [], |row| row.get(0))
            .expect("count query succeeds");
        assert_eq!(feature_count, 1);

        fs::remove_file(path).ok();
    }
}
