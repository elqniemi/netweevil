use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use arrow_array::{ArrayRef, BinaryArray, Float64Array, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use netweevil_query::{
    AnalysisOutcome, BatchItemStatus, MatrixResult, OdPairsDocument, OdResult, PointSetDocument,
    RouteBatchDocument, RouteBatchResult, RouteRequest, RouteResult, ServiceAreaGeometryType,
    ServiceAreaRequest, ServiceAreaResult,
};
use parquet::arrow::ArrowWriter;
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::json;

const EPSG_4326: i32 = 4326;

pub fn write_route_result(
    path: impl AsRef<Path>,
    request: &RouteRequest,
    result: &RouteResult,
) -> Result<()> {
    let path = path.as_ref();
    match output_format(path)? {
        OutputFormat::Json => write_json(path, result),
        OutputFormat::Csv => write_route_csv(path, request, result),
        OutputFormat::GeoJson => write_route_geojson(path, request, result),
        OutputFormat::GeoPackage => write_route_gpkg(path, request, result),
        OutputFormat::Parquet => write_route_parquet(path, request, result),
        OutputFormat::GeoParquet => write_route_geoparquet(path, request, result),
    }
}

pub fn write_route_batch_result(
    path: impl AsRef<Path>,
    requests: &RouteBatchDocument,
    result: &RouteBatchResult,
) -> Result<()> {
    let path = path.as_ref();
    match output_format(path)? {
        OutputFormat::Json => write_json(path, result),
        OutputFormat::GeoPackage => write_route_batch_gpkg(path, requests, result),
        other => bail!(
            "route batch output does not support {:?}; use .json or .gpkg",
            other
        ),
    }
}

pub fn write_od_result(
    path: impl AsRef<Path>,
    request: &OdPairsDocument,
    result: &OdResult,
) -> Result<()> {
    let path = path.as_ref();
    match output_format(path)? {
        OutputFormat::Json => write_json(path, result),
        OutputFormat::Csv => write_od_csv(path, request, result),
        OutputFormat::GeoJson => write_od_geojson(path, request, result),
        OutputFormat::GeoPackage => write_od_gpkg(path, request, result),
        OutputFormat::Parquet => write_od_parquet(path, request, result),
        OutputFormat::GeoParquet => write_od_geoparquet(path, request, result),
    }
}

pub fn write_matrix_result(
    path: impl AsRef<Path>,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
    result: &MatrixResult,
) -> Result<()> {
    let path = path.as_ref();
    match output_format(path)? {
        OutputFormat::Json => write_json(path, result),
        OutputFormat::Csv => write_matrix_csv(path, origins, destinations, result),
        OutputFormat::GeoJson => write_matrix_geojson(path, origins, destinations, result),
        OutputFormat::GeoPackage => write_matrix_gpkg(path, origins, destinations, result),
        OutputFormat::Parquet => write_matrix_parquet(path, origins, destinations, result),
        OutputFormat::GeoParquet => write_matrix_geoparquet(path, origins, destinations, result),
    }
}

pub fn write_service_area_result(
    path: impl AsRef<Path>,
    request: &ServiceAreaRequest,
    result: &ServiceAreaResult,
) -> Result<()> {
    let path = path.as_ref();
    match output_format(path)? {
        OutputFormat::Json => write_json(path, result),
        OutputFormat::Csv => write_service_area_csv(path, request, result),
        OutputFormat::GeoJson => write_service_area_geojson(path, request, result),
        OutputFormat::GeoPackage => write_service_area_gpkg(path, request, result),
        OutputFormat::Parquet => write_service_area_parquet(path, request, result),
        OutputFormat::GeoParquet => write_service_area_geoparquet(path, request, result),
    }
}

#[derive(Debug, Clone, Copy)]
enum OutputFormat {
    Json,
    Csv,
    GeoJson,
    GeoPackage,
    Parquet,
    GeoParquet,
}

fn output_format(path: &Path) -> Result<OutputFormat> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
    {
        None => Ok(OutputFormat::Json),
        Some(ext) => match ext.as_str() {
            "json" => Ok(OutputFormat::Json),
            "csv" => Ok(OutputFormat::Csv),
            "geojson" => Ok(OutputFormat::GeoJson),
            "gpkg" | "geopackage" => Ok(OutputFormat::GeoPackage),
            "parquet" => Ok(OutputFormat::Parquet),
            "geoparquet" | "gpq" => Ok(OutputFormat::GeoParquet),
            other => {
                bail!(
                    "unsupported output extension '{other}'; use .json, .csv, .geojson, .gpkg, .parquet, or .geoparquet"
                )
            }
        },
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    ensure_parent_dir(path)?;
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    serde_json::to_writer_pretty(BufWriter::new(file), value).context("serializing JSON")?;
    Ok(())
}

fn write_route_csv(path: &Path, request: &RouteRequest, result: &RouteResult) -> Result<()> {
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

fn write_route_geojson(path: &Path, request: &RouteRequest, result: &RouteResult) -> Result<()> {
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

fn write_route_gpkg(path: &Path, request: &RouteRequest, result: &RouteResult) -> Result<()> {
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

fn write_route_batch_gpkg(
    path: &Path,
    requests: &RouteBatchDocument,
    result: &RouteBatchResult,
) -> Result<()> {
    if requests.requests.len() != result.items.len() {
        bail!(
            "route batch request/result length mismatch: {} requests vs {} items",
            requests.requests.len(),
            result.items.len()
        );
    }

    let successful_extents = result
        .items
        .iter()
        .filter_map(|item| item.route.as_ref())
        .map(route_coords)
        .collect::<Result<Vec<_>>>()?;

    let mut gpkg = GeoPackageWriter::create(path)?;
    gpkg.create_feature_table(
        "routes",
        &[
            ("request_index", "INTEGER NOT NULL"),
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
            ("node_path_json", "TEXT NOT NULL"),
            ("edge_path_json", "TEXT NOT NULL"),
            ("road_breakdowns_json", "TEXT"),
            ("surface_breakdowns_json", "TEXT"),
            ("diagnostics_json", "TEXT NOT NULL"),
            ("warnings_json", "TEXT NOT NULL"),
        ],
        "LINESTRING",
        (!successful_extents.is_empty())
            .then(|| extent_for_features(successful_extents.iter().map(Vec::as_slice))),
    )?;
    gpkg.create_attribute_table(
        "route_segments",
        &[
            ("request_index", "INTEGER NOT NULL"),
            ("route_id", "TEXT NOT NULL"),
            ("segment_index", "INTEGER NOT NULL"),
            ("edge_id", "INTEGER NOT NULL"),
            ("from_node_id", "INTEGER NOT NULL"),
            ("to_node_id", "INTEGER NOT NULL"),
            ("source_way_id", "INTEGER NOT NULL"),
            ("length_m", "INTEGER NOT NULL"),
            ("travel_time_s", "REAL NOT NULL"),
            ("generalized_cost", "REAL NOT NULL"),
            ("road_class", "TEXT NOT NULL"),
            ("surface", "TEXT NOT NULL"),
            ("name", "TEXT"),
            ("violation_type", "TEXT"),
        ],
    )?;
    gpkg.create_attribute_table(
        "route_breakdown_road_class",
        &[
            ("request_index", "INTEGER NOT NULL"),
            ("route_id", "TEXT NOT NULL"),
            ("road_class", "TEXT NOT NULL"),
            ("distance_m", "INTEGER"),
            ("time_s", "REAL"),
        ],
    )?;
    gpkg.create_attribute_table(
        "route_breakdown_surface",
        &[
            ("request_index", "INTEGER NOT NULL"),
            ("route_id", "TEXT NOT NULL"),
            ("surface", "TEXT NOT NULL"),
            ("distance_m", "INTEGER"),
            ("time_s", "REAL"),
        ],
    )?;
    gpkg.create_attribute_table(
        "route_violations",
        &[
            ("request_index", "INTEGER NOT NULL"),
            ("route_id", "TEXT NOT NULL"),
            ("violation_index", "INTEGER NOT NULL"),
            ("violation_type", "TEXT NOT NULL"),
            ("edge_id", "INTEGER"),
            ("from_edge_id", "INTEGER"),
            ("to_edge_id", "INTEGER"),
            ("distance_m", "REAL"),
            ("penalty_s", "REAL NOT NULL"),
            ("penalty_generalized_cost", "REAL NOT NULL"),
        ],
    )?;
    gpkg.create_attribute_table(
        "route_failures",
        &[
            ("request_index", "INTEGER NOT NULL"),
            ("route_id", "TEXT NOT NULL"),
            ("origin_id", "TEXT NOT NULL"),
            ("destination_id", "TEXT NOT NULL"),
            ("error", "TEXT NOT NULL"),
        ],
    )?;

    gpkg.begin_transaction()?;

    for (index, (entry, item)) in requests.requests.iter().zip(&result.items).enumerate() {
        let request_index = index as i64 + 1;
        let request = &entry.request;
        if request.route_id != item.route_id {
            bail!(
                "route batch result '{}' does not match request '{}'",
                item.route_id,
                request.route_id
            );
        }

        if let Some(route) = item.route.as_ref() {
            let coords = route_coords(route)?;
            let road_breakdowns = route
                .breakdowns
                .as_ref()
                .map(|breakdowns| diagnostics_json(&breakdowns.road_class));
            let surface_breakdowns = route
                .breakdowns
                .as_ref()
                .map(|breakdowns| diagnostics_json(&breakdowns.surface));
            gpkg.insert_feature(
                "routes",
                &[
                    ("request_index", SqlValue::Integer(request_index)),
                    ("route_id", SqlValue::Text(route.route_id.clone())),
                    ("origin_id", SqlValue::Text(request.origin.id.clone())),
                    (
                        "destination_id",
                        SqlValue::Text(request.destination.id.clone()),
                    ),
                    (
                        "outcome",
                        SqlValue::Text(outcome_name(route.outcome).to_string()),
                    ),
                    (
                        "fallback_used",
                        SqlValue::Integer(if route.fallback_used { 1 } else { 0 }),
                    ),
                    (
                        "origin_component_id",
                        SqlValue::NullableInteger(optional_u32_as_i64(route.origin.component_id)),
                    ),
                    (
                        "destination_component_id",
                        SqlValue::NullableInteger(optional_u32_as_i64(
                            route.destination.component_id,
                        )),
                    ),
                    (
                        "origin_hop_distance_m",
                        SqlValue::NullableReal(route.origin_hop_distance_m),
                    ),
                    (
                        "destination_hop_distance_m",
                        SqlValue::NullableReal(route.destination_hop_distance_m),
                    ),
                    (
                        "origin_snap_distance_m",
                        SqlValue::Real(route.origin.snap_distance_m),
                    ),
                    (
                        "destination_snap_distance_m",
                        SqlValue::Real(route.destination.snap_distance_m),
                    ),
                    (
                        "total_distance_m",
                        SqlValue::Integer(route.summary.total_distance_m as i64),
                    ),
                    (
                        "total_travel_time_s",
                        SqlValue::Real(route.summary.total_travel_time_s),
                    ),
                    (
                        "total_generalized_cost",
                        SqlValue::Real(route.summary.total_generalized_cost),
                    ),
                    (
                        "illegal_movement_penalty_s",
                        SqlValue::Real(route.summary.illegal_movement_penalty_s),
                    ),
                    (
                        "illegal_movement_penalty_cost",
                        SqlValue::Real(route.summary.illegal_movement_penalty_cost),
                    ),
                    (
                        "violation_count",
                        SqlValue::Integer(route.summary.violation_count as i64),
                    ),
                    (
                        "violation_types_json",
                        SqlValue::Text(diagnostics_json(&route.summary.violation_types)),
                    ),
                    (
                        "violations_json",
                        SqlValue::Text(diagnostics_json(&route.violations)),
                    ),
                    (
                        "segment_count",
                        SqlValue::Integer(route.summary.segment_count as i64),
                    ),
                    (
                        "node_path_json",
                        SqlValue::Text(diagnostics_json(&route.node_path)),
                    ),
                    (
                        "edge_path_json",
                        SqlValue::Text(diagnostics_json(&route.edge_path)),
                    ),
                    (
                        "road_breakdowns_json",
                        SqlValue::NullableText(road_breakdowns),
                    ),
                    (
                        "surface_breakdowns_json",
                        SqlValue::NullableText(surface_breakdowns),
                    ),
                    (
                        "diagnostics_json",
                        SqlValue::Text(diagnostics_json(&route.diagnostics)),
                    ),
                    (
                        "warnings_json",
                        SqlValue::Text(diagnostics_json(&route.warnings)),
                    ),
                ],
                &coords,
            )?;

            if let Some(segments) = route.segments.as_ref() {
                for (segment_index, segment) in segments.iter().enumerate() {
                    gpkg.insert_row(
                        "route_segments",
                        &[
                            ("request_index", SqlValue::Integer(request_index)),
                            ("route_id", SqlValue::Text(route.route_id.clone())),
                            ("segment_index", SqlValue::Integer(segment_index as i64 + 1)),
                            ("edge_id", SqlValue::Integer(segment.edge_id as i64)),
                            (
                                "from_node_id",
                                SqlValue::Integer(segment.from_node_id as i64),
                            ),
                            ("to_node_id", SqlValue::Integer(segment.to_node_id as i64)),
                            ("source_way_id", SqlValue::Integer(segment.source_way_id)),
                            ("length_m", SqlValue::Integer(segment.length_m as i64)),
                            ("travel_time_s", SqlValue::Real(segment.travel_time_s)),
                            ("generalized_cost", SqlValue::Real(segment.generalized_cost)),
                            (
                                "road_class",
                                SqlValue::Text(format!("{:?}", segment.road_class).to_lowercase()),
                            ),
                            (
                                "surface",
                                SqlValue::Text(format!("{:?}", segment.surface).to_lowercase()),
                            ),
                            ("name", SqlValue::NullableText(segment.name.clone())),
                            (
                                "violation_type",
                                SqlValue::NullableText(segment.violation_type.map(|value| {
                                    serde_json::to_string(&value)
                                        .unwrap_or_default()
                                        .trim_matches('"')
                                        .to_string()
                                })),
                            ),
                        ],
                    )?;
                }
            }

            if let Some(breakdowns) = route.breakdowns.as_ref() {
                for (road_class, metrics) in &breakdowns.road_class {
                    gpkg.insert_row(
                        "route_breakdown_road_class",
                        &[
                            ("request_index", SqlValue::Integer(request_index)),
                            ("route_id", SqlValue::Text(route.route_id.clone())),
                            ("road_class", SqlValue::Text(road_class.clone())),
                            (
                                "distance_m",
                                SqlValue::NullableInteger(
                                    metrics.distance_m.map(|value| value as i64),
                                ),
                            ),
                            ("time_s", SqlValue::NullableReal(metrics.time_s)),
                        ],
                    )?;
                }
                for (surface, metrics) in &breakdowns.surface {
                    gpkg.insert_row(
                        "route_breakdown_surface",
                        &[
                            ("request_index", SqlValue::Integer(request_index)),
                            ("route_id", SqlValue::Text(route.route_id.clone())),
                            ("surface", SqlValue::Text(surface.clone())),
                            (
                                "distance_m",
                                SqlValue::NullableInteger(
                                    metrics.distance_m.map(|value| value as i64),
                                ),
                            ),
                            ("time_s", SqlValue::NullableReal(metrics.time_s)),
                        ],
                    )?;
                }
            }

            for (violation_index, violation) in route.violations.iter().enumerate() {
                gpkg.insert_row(
                    "route_violations",
                    &[
                        ("request_index", SqlValue::Integer(request_index)),
                        ("route_id", SqlValue::Text(route.route_id.clone())),
                        (
                            "violation_index",
                            SqlValue::Integer(violation_index as i64 + 1),
                        ),
                        (
                            "violation_type",
                            SqlValue::Text(
                                serde_json::to_string(&violation.violation_type)
                                    .unwrap_or_default()
                                    .trim_matches('"')
                                    .to_string(),
                            ),
                        ),
                        (
                            "edge_id",
                            SqlValue::NullableInteger(violation.edge_id.map(i64::from)),
                        ),
                        (
                            "from_edge_id",
                            SqlValue::NullableInteger(violation.from_edge_id.map(i64::from)),
                        ),
                        (
                            "to_edge_id",
                            SqlValue::NullableInteger(violation.to_edge_id.map(i64::from)),
                        ),
                        ("distance_m", SqlValue::NullableReal(violation.distance_m)),
                        ("penalty_s", SqlValue::Real(violation.penalty_s)),
                        (
                            "penalty_generalized_cost",
                            SqlValue::Real(violation.penalty_generalized_cost),
                        ),
                    ],
                )?;
            }
        } else {
            gpkg.insert_row(
                "route_failures",
                &[
                    ("request_index", SqlValue::Integer(request_index)),
                    ("route_id", SqlValue::Text(item.route_id.clone())),
                    ("origin_id", SqlValue::Text(item.origin_id.clone())),
                    (
                        "destination_id",
                        SqlValue::Text(item.destination_id.clone()),
                    ),
                    (
                        "error",
                        SqlValue::Text(
                            item.error
                                .clone()
                                .unwrap_or_else(|| "route execution failed".to_string()),
                        ),
                    ),
                ],
            )?;
        }
    }

    gpkg.commit_transaction()?;

    Ok(())
}

fn write_route_parquet(path: &Path, request: &RouteRequest, result: &RouteResult) -> Result<()> {
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

fn write_route_geoparquet(path: &Path, request: &RouteRequest, result: &RouteResult) -> Result<()> {
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

fn write_od_csv(path: &Path, request: &OdPairsDocument, result: &OdResult) -> Result<()> {
    let request_lookup: BTreeMap<_, _> = request
        .pairs
        .iter()
        .map(|pair| (pair.pair_id.as_str(), pair))
        .collect();
    let rows = result
        .pairs
        .iter()
        .map(|pair| {
            let request_pair = request_lookup
                .get(pair.pair_id.as_str())
                .with_context(|| format!("OD result references unknown pair '{}'", pair.pair_id))?;
            let geometry = od_pair_coords(pair, request_pair);
            Ok(vec![
                pair.pair_id.clone(),
                pair.origin_id.clone(),
                pair.destination_id.clone(),
                request_pair.origin.lon.to_string(),
                request_pair.origin.lat.to_string(),
                request_pair.destination.lon.to_string(),
                request_pair.destination.lat.to_string(),
                batch_status_name(pair.status).to_string(),
                outcome_name(pair.outcome).to_string(),
                pair.fallback_used.to_string(),
                optional_string(pair.origin_component_id),
                optional_string(pair.destination_component_id),
                optional_string(pair.origin_hop_distance_m),
                optional_string(pair.destination_hop_distance_m),
                optional_string(pair.origin_snap_distance_m),
                optional_string(pair.destination_snap_distance_m),
                optional_string(pair.total_distance_m),
                optional_string(pair.total_travel_time_s),
                optional_string(pair.total_generalized_cost),
                optional_string(pair.illegal_movement_penalty_s),
                optional_string(pair.illegal_movement_penalty_cost),
                pair.violation_count.to_string(),
                diagnostics_json(&pair.violation_types),
                pair.error.clone().unwrap_or_default(),
                diagnostics_json(&pair.diagnostics),
                linestring_wkt(&geometry),
            ])
        })
        .collect::<Result<Vec<_>>>()?;

    write_csv(
        path,
        &[
            "pair_id",
            "origin_id",
            "destination_id",
            "source_x",
            "source_y",
            "target_x",
            "target_y",
            "status",
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
            "error",
            "diagnostics_json",
            "geometry_wkt",
        ],
        rows,
    )
}

fn write_od_geojson(path: &Path, request: &OdPairsDocument, result: &OdResult) -> Result<()> {
    let request_lookup: BTreeMap<_, _> = request
        .pairs
        .iter()
        .map(|pair| (pair.pair_id.as_str(), pair))
        .collect();
    let features = result
        .pairs
        .iter()
        .map(|pair| {
            let request_pair = request_lookup
                .get(pair.pair_id.as_str())
                .with_context(|| format!("OD result references unknown pair '{}'", pair.pair_id))?;
            let geometry = od_pair_coords(pair, request_pair);
            Ok(json!({
                "type": "Feature",
                "geometry": {
                    "type": "LineString",
                    "coordinates": geometry
                },
                "properties": {
                    "pair_id": pair.pair_id,
                    "origin_id": pair.origin_id,
                    "destination_id": pair.destination_id,
                    "status": batch_status_name(pair.status),
                    "outcome": outcome_name(pair.outcome),
                    "fallback_used": pair.fallback_used,
                    "origin_component_id": pair.origin_component_id,
                    "destination_component_id": pair.destination_component_id,
                    "origin_hop_distance_m": pair.origin_hop_distance_m,
                    "destination_hop_distance_m": pair.destination_hop_distance_m,
                    "origin_snap_distance_m": pair.origin_snap_distance_m,
                    "destination_snap_distance_m": pair.destination_snap_distance_m,
                    "total_distance_m": pair.total_distance_m,
                    "total_travel_time_s": pair.total_travel_time_s,
                    "total_generalized_cost": pair.total_generalized_cost,
                    "illegal_movement_penalty_s": pair.illegal_movement_penalty_s,
                    "illegal_movement_penalty_cost": pair.illegal_movement_penalty_cost,
                    "violation_count": pair.violation_count,
                    "violation_types": pair.violation_types,
                    "error": pair.error,
                    "diagnostics": pair.diagnostics,
                }
            }))
        })
        .collect::<Result<Vec<_>>>()?;

    write_json(
        path,
        &json!({
            "type": "FeatureCollection",
            "features": features,
        }),
    )
}

fn write_od_gpkg(path: &Path, request: &OdPairsDocument, result: &OdResult) -> Result<()> {
    let request_lookup: BTreeMap<_, _> = request
        .pairs
        .iter()
        .map(|pair| (pair.pair_id.as_str(), pair))
        .collect();
    let extents = result
        .pairs
        .iter()
        .map(|pair| {
            let request_pair = request_lookup
                .get(pair.pair_id.as_str())
                .with_context(|| format!("OD result references unknown pair '{}'", pair.pair_id))?;
            Ok(od_pair_coords(pair, request_pair))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut gpkg = GeoPackageWriter::create(path)?;
    gpkg.create_feature_table(
        "od_pairs",
        &[
            ("pair_id", "TEXT NOT NULL"),
            ("origin_id", "TEXT NOT NULL"),
            ("destination_id", "TEXT NOT NULL"),
            ("source_x", "REAL NOT NULL"),
            ("source_y", "REAL NOT NULL"),
            ("target_x", "REAL NOT NULL"),
            ("target_y", "REAL NOT NULL"),
            ("status", "TEXT NOT NULL"),
            ("outcome", "TEXT NOT NULL"),
            ("fallback_used", "INTEGER NOT NULL"),
            ("origin_component_id", "INTEGER"),
            ("destination_component_id", "INTEGER"),
            ("origin_hop_distance_m", "REAL"),
            ("destination_hop_distance_m", "REAL"),
            ("origin_snap_distance_m", "REAL"),
            ("destination_snap_distance_m", "REAL"),
            ("total_distance_m", "INTEGER"),
            ("total_travel_time_s", "REAL"),
            ("total_generalized_cost", "REAL"),
            ("illegal_movement_penalty_s", "REAL"),
            ("illegal_movement_penalty_cost", "REAL"),
            ("violation_count", "INTEGER NOT NULL"),
            ("violation_types_json", "TEXT NOT NULL"),
            ("error", "TEXT"),
            ("diagnostics_json", "TEXT NOT NULL"),
        ],
        "LINESTRING",
        Some(extent_for_features(extents.iter().map(Vec::as_slice))),
    )?;

    for pair in &result.pairs {
        let request_pair = request_lookup
            .get(pair.pair_id.as_str())
            .with_context(|| format!("OD result references unknown pair '{}'", pair.pair_id))?;
        let geometry = od_pair_coords(pair, request_pair);
        gpkg.insert_feature(
            "od_pairs",
            &[
                ("pair_id", SqlValue::Text(pair.pair_id.clone())),
                ("origin_id", SqlValue::Text(pair.origin_id.clone())),
                (
                    "destination_id",
                    SqlValue::Text(pair.destination_id.clone()),
                ),
                ("source_x", SqlValue::Real(request_pair.origin.lon)),
                ("source_y", SqlValue::Real(request_pair.origin.lat)),
                ("target_x", SqlValue::Real(request_pair.destination.lon)),
                ("target_y", SqlValue::Real(request_pair.destination.lat)),
                (
                    "status",
                    SqlValue::Text(batch_status_name(pair.status).to_string()),
                ),
                (
                    "outcome",
                    SqlValue::Text(outcome_name(pair.outcome).to_string()),
                ),
                (
                    "fallback_used",
                    SqlValue::Integer(if pair.fallback_used { 1 } else { 0 }),
                ),
                (
                    "origin_component_id",
                    SqlValue::NullableInteger(optional_u32_as_i64(pair.origin_component_id)),
                ),
                (
                    "destination_component_id",
                    SqlValue::NullableInteger(optional_u32_as_i64(pair.destination_component_id)),
                ),
                (
                    "origin_hop_distance_m",
                    SqlValue::NullableReal(pair.origin_hop_distance_m),
                ),
                (
                    "destination_hop_distance_m",
                    SqlValue::NullableReal(pair.destination_hop_distance_m),
                ),
                (
                    "origin_snap_distance_m",
                    SqlValue::NullableReal(pair.origin_snap_distance_m),
                ),
                (
                    "destination_snap_distance_m",
                    SqlValue::NullableReal(pair.destination_snap_distance_m),
                ),
                (
                    "total_distance_m",
                    SqlValue::NullableInteger(pair.total_distance_m.map(|value| value as i64)),
                ),
                (
                    "total_travel_time_s",
                    SqlValue::NullableReal(pair.total_travel_time_s),
                ),
                (
                    "total_generalized_cost",
                    SqlValue::NullableReal(pair.total_generalized_cost),
                ),
                (
                    "illegal_movement_penalty_s",
                    SqlValue::NullableReal(pair.illegal_movement_penalty_s),
                ),
                (
                    "illegal_movement_penalty_cost",
                    SqlValue::NullableReal(pair.illegal_movement_penalty_cost),
                ),
                (
                    "violation_count",
                    SqlValue::Integer(pair.violation_count as i64),
                ),
                (
                    "violation_types_json",
                    SqlValue::Text(diagnostics_json(&pair.violation_types)),
                ),
                ("error", SqlValue::NullableText(pair.error.clone())),
                (
                    "diagnostics_json",
                    SqlValue::Text(diagnostics_json(&pair.diagnostics)),
                ),
            ],
            &geometry,
        )?;
    }

    Ok(())
}

fn write_od_parquet(path: &Path, request: &OdPairsDocument, result: &OdResult) -> Result<()> {
    let request_lookup: BTreeMap<_, _> = request
        .pairs
        .iter()
        .map(|pair| (pair.pair_id.as_str(), pair))
        .collect();
    let rows = result
        .pairs
        .iter()
        .map(|pair| {
            let request_pair = request_lookup
                .get(pair.pair_id.as_str())
                .with_context(|| format!("OD result references unknown pair '{}'", pair.pair_id))?;
            Ok((
                pair,
                request_pair,
                linestring_wkt(&od_pair_coords(pair, request_pair)),
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let schema = Schema::new(vec![
        Field::new("pair_id", DataType::Utf8, false),
        Field::new("origin_id", DataType::Utf8, false),
        Field::new("destination_id", DataType::Utf8, false),
        Field::new("source_x", DataType::Float64, false),
        Field::new("source_y", DataType::Float64, false),
        Field::new("target_x", DataType::Float64, false),
        Field::new("target_y", DataType::Float64, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("outcome", DataType::Utf8, false),
        Field::new("fallback_used", DataType::Utf8, false),
        Field::new("origin_component_id", DataType::UInt64, true),
        Field::new("destination_component_id", DataType::UInt64, true),
        Field::new("origin_hop_distance_m", DataType::Float64, true),
        Field::new("destination_hop_distance_m", DataType::Float64, true),
        Field::new("origin_snap_distance_m", DataType::Float64, true),
        Field::new("destination_snap_distance_m", DataType::Float64, true),
        Field::new("total_distance_m", DataType::UInt64, true),
        Field::new("total_travel_time_s", DataType::Float64, true),
        Field::new("total_generalized_cost", DataType::Float64, true),
        Field::new("illegal_movement_penalty_s", DataType::Float64, true),
        Field::new("illegal_movement_penalty_cost", DataType::Float64, true),
        Field::new("violation_count", DataType::UInt64, false),
        Field::new("violation_types_json", DataType::Utf8, false),
        Field::new("error", DataType::Utf8, true),
        Field::new("diagnostics_json", DataType::Utf8, false),
        Field::new("geometry_wkt", DataType::Utf8, false),
    ]);

    let pair_ids = rows
        .iter()
        .map(|(pair, _, _)| pair.pair_id.as_str())
        .collect::<Vec<_>>();
    let origin_ids = rows
        .iter()
        .map(|(pair, _, _)| pair.origin_id.as_str())
        .collect::<Vec<_>>();
    let destination_ids = rows
        .iter()
        .map(|(pair, _, _)| pair.destination_id.as_str())
        .collect::<Vec<_>>();
    let source_x = rows
        .iter()
        .map(|(_, request_pair, _)| request_pair.origin.lon)
        .collect::<Vec<_>>();
    let source_y = rows
        .iter()
        .map(|(_, request_pair, _)| request_pair.origin.lat)
        .collect::<Vec<_>>();
    let target_x = rows
        .iter()
        .map(|(_, request_pair, _)| request_pair.destination.lon)
        .collect::<Vec<_>>();
    let target_y = rows
        .iter()
        .map(|(_, request_pair, _)| request_pair.destination.lat)
        .collect::<Vec<_>>();
    let status = rows
        .iter()
        .map(|(pair, _, _)| batch_status_name(pair.status))
        .collect::<Vec<_>>();
    let outcome = rows
        .iter()
        .map(|(pair, _, _)| outcome_name(pair.outcome))
        .collect::<Vec<_>>();
    let fallback_used = rows
        .iter()
        .map(|(pair, _, _)| if pair.fallback_used { "true" } else { "false" })
        .collect::<Vec<_>>();
    let origin_component_id = rows
        .iter()
        .map(|(pair, _, _)| pair.origin_component_id.map(u64::from))
        .collect::<Vec<_>>();
    let destination_component_id = rows
        .iter()
        .map(|(pair, _, _)| pair.destination_component_id.map(u64::from))
        .collect::<Vec<_>>();
    let origin_hop_distance = rows
        .iter()
        .map(|(pair, _, _)| pair.origin_hop_distance_m)
        .collect::<Vec<_>>();
    let destination_hop_distance = rows
        .iter()
        .map(|(pair, _, _)| pair.destination_hop_distance_m)
        .collect::<Vec<_>>();
    let origin_snap = rows
        .iter()
        .map(|(pair, _, _)| pair.origin_snap_distance_m)
        .collect::<Vec<_>>();
    let destination_snap = rows
        .iter()
        .map(|(pair, _, _)| pair.destination_snap_distance_m)
        .collect::<Vec<_>>();
    let total_distance = rows
        .iter()
        .map(|(pair, _, _)| pair.total_distance_m)
        .collect::<Vec<_>>();
    let total_travel_time = rows
        .iter()
        .map(|(pair, _, _)| pair.total_travel_time_s)
        .collect::<Vec<_>>();
    let total_cost = rows
        .iter()
        .map(|(pair, _, _)| pair.total_generalized_cost)
        .collect::<Vec<_>>();
    let illegal_penalty_s = rows
        .iter()
        .map(|(pair, _, _)| pair.illegal_movement_penalty_s)
        .collect::<Vec<_>>();
    let illegal_penalty_cost = rows
        .iter()
        .map(|(pair, _, _)| pair.illegal_movement_penalty_cost)
        .collect::<Vec<_>>();
    let violation_count = rows
        .iter()
        .map(|(pair, _, _)| pair.violation_count as u64)
        .collect::<Vec<_>>();
    let violation_types = rows
        .iter()
        .map(|(pair, _, _)| diagnostics_json(&pair.violation_types))
        .collect::<Vec<_>>();
    let error = rows
        .iter()
        .map(|(pair, _, _)| pair.error.as_deref())
        .collect::<Vec<_>>();
    let diagnostics = rows
        .iter()
        .map(|(pair, _, _)| diagnostics_json(&pair.diagnostics))
        .collect::<Vec<_>>();
    let geometry_wkt = rows
        .iter()
        .map(|(_, _, geometry)| geometry.as_str())
        .collect::<Vec<_>>();

    write_parquet_record_batch(
        path,
        schema,
        vec![
            Arc::new(StringArray::from(pair_ids)) as ArrayRef,
            Arc::new(StringArray::from(origin_ids)),
            Arc::new(StringArray::from(destination_ids)),
            Arc::new(Float64Array::from(source_x)),
            Arc::new(Float64Array::from(source_y)),
            Arc::new(Float64Array::from(target_x)),
            Arc::new(Float64Array::from(target_y)),
            Arc::new(StringArray::from(status)),
            Arc::new(StringArray::from(outcome)),
            Arc::new(StringArray::from(fallback_used)),
            Arc::new(UInt64Array::from(origin_component_id)),
            Arc::new(UInt64Array::from(destination_component_id)),
            Arc::new(Float64Array::from(origin_hop_distance)),
            Arc::new(Float64Array::from(destination_hop_distance)),
            Arc::new(Float64Array::from(origin_snap)),
            Arc::new(Float64Array::from(destination_snap)),
            Arc::new(UInt64Array::from(total_distance)),
            Arc::new(Float64Array::from(total_travel_time)),
            Arc::new(Float64Array::from(total_cost)),
            Arc::new(Float64Array::from(illegal_penalty_s)),
            Arc::new(Float64Array::from(illegal_penalty_cost)),
            Arc::new(UInt64Array::from(violation_count)),
            Arc::new(StringArray::from(
                violation_types
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(error)),
            Arc::new(StringArray::from(
                diagnostics.iter().map(String::as_str).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(geometry_wkt)),
        ],
    )
}

fn write_od_geoparquet(path: &Path, request: &OdPairsDocument, result: &OdResult) -> Result<()> {
    let request_lookup: BTreeMap<_, _> = request
        .pairs
        .iter()
        .map(|pair| (pair.pair_id.as_str(), pair))
        .collect();
    let rows = result
        .pairs
        .iter()
        .map(|pair| {
            let request_pair = request_lookup
                .get(pair.pair_id.as_str())
                .with_context(|| format!("OD result references unknown pair '{}'", pair.pair_id))?;
            let geometry = od_pair_coords(pair, request_pair);
            let wkb = wkb_linestring(&geometry);
            Ok((pair, request_pair, geometry, wkb))
        })
        .collect::<Result<Vec<_>>>()?;
    let extent = extent_for_features(rows.iter().map(|(_, _, geometry, _)| geometry.as_slice()));
    let schema = geoparquet_schema(
        vec![
            Field::new("pair_id", DataType::Utf8, false),
            Field::new("origin_id", DataType::Utf8, false),
            Field::new("destination_id", DataType::Utf8, false),
            Field::new("source_x", DataType::Float64, false),
            Field::new("source_y", DataType::Float64, false),
            Field::new("target_x", DataType::Float64, false),
            Field::new("target_y", DataType::Float64, false),
            Field::new("status", DataType::Utf8, false),
            Field::new("outcome", DataType::Utf8, false),
            Field::new("fallback_used", DataType::Utf8, false),
            Field::new("origin_component_id", DataType::UInt64, true),
            Field::new("destination_component_id", DataType::UInt64, true),
            Field::new("origin_hop_distance_m", DataType::Float64, true),
            Field::new("destination_hop_distance_m", DataType::Float64, true),
            Field::new("origin_snap_distance_m", DataType::Float64, true),
            Field::new("destination_snap_distance_m", DataType::Float64, true),
            Field::new("total_distance_m", DataType::UInt64, true),
            Field::new("total_travel_time_s", DataType::Float64, true),
            Field::new("total_generalized_cost", DataType::Float64, true),
            Field::new("illegal_movement_penalty_s", DataType::Float64, true),
            Field::new("illegal_movement_penalty_cost", DataType::Float64, true),
            Field::new("violation_count", DataType::UInt64, false),
            Field::new("violation_types_json", DataType::Utf8, false),
            Field::new("error", DataType::Utf8, true),
            Field::new("diagnostics_json", DataType::Utf8, false),
            Field::new("geometry", DataType::Binary, false),
        ],
        extent,
    );

    write_parquet_record_batch(
        path,
        schema,
        vec![
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.pair_id.as_str())
                    .collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.origin_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.destination_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(_, request_pair, _, _)| request_pair.origin.lon)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(_, request_pair, _, _)| request_pair.origin.lat)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(_, request_pair, _, _)| request_pair.destination.lon)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(_, request_pair, _, _)| request_pair.destination.lat)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(pair, _, _, _)| batch_status_name(pair.status))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(pair, _, _, _)| outcome_name(pair.outcome))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(pair, _, _, _)| if pair.fallback_used { "true" } else { "false" })
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.origin_component_id.map(u64::from))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.destination_component_id.map(u64::from))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.origin_hop_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.destination_hop_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.origin_snap_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.destination_snap_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.total_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.total_travel_time_s)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.total_generalized_cost)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.illegal_movement_penalty_s)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.illegal_movement_penalty_cost)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.violation_count as u64)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(pair, _, _, _)| diagnostics_json(&pair.violation_types))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.error.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(pair, _, _, _)| diagnostics_json(&pair.diagnostics))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(BinaryArray::from(
                rows.iter()
                    .map(|(_, _, _, wkb)| wkb.as_slice())
                    .collect::<Vec<_>>(),
            )),
        ],
    )
}

fn write_matrix_csv(
    path: &Path,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
    result: &MatrixResult,
) -> Result<()> {
    let origin_lookup = point_lookup(origins);
    let destination_lookup = point_lookup(destinations);
    let rows = result
        .cells
        .iter()
        .map(|cell| {
            let origin = origin_lookup
                .get(cell.origin_id.as_str())
                .with_context(|| {
                    format!(
                        "matrix result references unknown origin '{}'",
                        cell.origin_id
                    )
                })?;
            let destination = destination_lookup
                .get(cell.destination_id.as_str())
                .with_context(|| {
                    format!(
                        "matrix result references unknown destination '{}'",
                        cell.destination_id
                    )
                })?;
            let geometry = matrix_cell_coords(cell, origin, destination);
            Ok(vec![
                cell.origin_id.clone(),
                cell.destination_id.clone(),
                origin.lon.to_string(),
                origin.lat.to_string(),
                destination.lon.to_string(),
                destination.lat.to_string(),
                batch_status_name(cell.status).to_string(),
                outcome_name(cell.outcome).to_string(),
                cell.fallback_used.to_string(),
                optional_string(cell.origin_component_id),
                optional_string(cell.destination_component_id),
                optional_string(cell.origin_hop_distance_m),
                optional_string(cell.destination_hop_distance_m),
                optional_string(cell.origin_snap_distance_m),
                optional_string(cell.destination_snap_distance_m),
                optional_string(cell.total_distance_m),
                optional_string(cell.total_travel_time_s),
                optional_string(cell.total_generalized_cost),
                optional_string(cell.illegal_movement_penalty_s),
                optional_string(cell.illegal_movement_penalty_cost),
                cell.violation_count.to_string(),
                diagnostics_json(&cell.violation_types),
                cell.error.clone().unwrap_or_default(),
                diagnostics_json(&cell.diagnostics),
                linestring_wkt(&geometry),
            ])
        })
        .collect::<Result<Vec<_>>>()?;

    write_csv(
        path,
        &[
            "origin_id",
            "destination_id",
            "source_x",
            "source_y",
            "target_x",
            "target_y",
            "status",
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
            "error",
            "diagnostics_json",
            "geometry_wkt",
        ],
        rows,
    )
}

fn write_matrix_geojson(
    path: &Path,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
    result: &MatrixResult,
) -> Result<()> {
    let origin_lookup = point_lookup(origins);
    let destination_lookup = point_lookup(destinations);
    let features = result
        .cells
        .iter()
        .map(|cell| {
            let origin = origin_lookup
                .get(cell.origin_id.as_str())
                .with_context(|| {
                    format!(
                        "matrix result references unknown origin '{}'",
                        cell.origin_id
                    )
                })?;
            let destination = destination_lookup
                .get(cell.destination_id.as_str())
                .with_context(|| {
                    format!(
                        "matrix result references unknown destination '{}'",
                        cell.destination_id
                    )
                })?;
            let geometry = matrix_cell_coords(cell, origin, destination);
            Ok(json!({
                "type": "Feature",
                "geometry": {
                    "type": "LineString",
                    "coordinates": geometry
                },
                "properties": {
                    "origin_id": cell.origin_id,
                    "destination_id": cell.destination_id,
                    "status": batch_status_name(cell.status),
                    "outcome": outcome_name(cell.outcome),
                    "fallback_used": cell.fallback_used,
                    "origin_component_id": cell.origin_component_id,
                    "destination_component_id": cell.destination_component_id,
                    "origin_hop_distance_m": cell.origin_hop_distance_m,
                    "destination_hop_distance_m": cell.destination_hop_distance_m,
                    "origin_snap_distance_m": cell.origin_snap_distance_m,
                    "destination_snap_distance_m": cell.destination_snap_distance_m,
                    "total_distance_m": cell.total_distance_m,
                    "total_travel_time_s": cell.total_travel_time_s,
                    "total_generalized_cost": cell.total_generalized_cost,
                    "illegal_movement_penalty_s": cell.illegal_movement_penalty_s,
                    "illegal_movement_penalty_cost": cell.illegal_movement_penalty_cost,
                    "violation_count": cell.violation_count,
                    "violation_types": cell.violation_types,
                    "error": cell.error,
                    "diagnostics": cell.diagnostics,
                }
            }))
        })
        .collect::<Result<Vec<_>>>()?;

    write_json(
        path,
        &json!({
            "type": "FeatureCollection",
            "features": features,
        }),
    )
}

fn write_matrix_gpkg(
    path: &Path,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
    result: &MatrixResult,
) -> Result<()> {
    let origin_lookup = point_lookup(origins);
    let destination_lookup = point_lookup(destinations);
    let extents = result
        .cells
        .iter()
        .map(|cell| {
            let origin = origin_lookup
                .get(cell.origin_id.as_str())
                .with_context(|| {
                    format!(
                        "matrix result references unknown origin '{}'",
                        cell.origin_id
                    )
                })?;
            let destination = destination_lookup
                .get(cell.destination_id.as_str())
                .with_context(|| {
                    format!(
                        "matrix result references unknown destination '{}'",
                        cell.destination_id
                    )
                })?;
            Ok(matrix_cell_coords(cell, origin, destination))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut gpkg = GeoPackageWriter::create(path)?;
    gpkg.create_feature_table(
        "matrix_cells",
        &[
            ("origin_id", "TEXT NOT NULL"),
            ("destination_id", "TEXT NOT NULL"),
            ("source_x", "REAL NOT NULL"),
            ("source_y", "REAL NOT NULL"),
            ("target_x", "REAL NOT NULL"),
            ("target_y", "REAL NOT NULL"),
            ("status", "TEXT NOT NULL"),
            ("outcome", "TEXT NOT NULL"),
            ("fallback_used", "INTEGER NOT NULL"),
            ("origin_component_id", "INTEGER"),
            ("destination_component_id", "INTEGER"),
            ("origin_hop_distance_m", "REAL"),
            ("destination_hop_distance_m", "REAL"),
            ("origin_snap_distance_m", "REAL"),
            ("destination_snap_distance_m", "REAL"),
            ("total_distance_m", "INTEGER"),
            ("total_travel_time_s", "REAL"),
            ("total_generalized_cost", "REAL"),
            ("illegal_movement_penalty_s", "REAL"),
            ("illegal_movement_penalty_cost", "REAL"),
            ("violation_count", "INTEGER NOT NULL"),
            ("violation_types_json", "TEXT NOT NULL"),
            ("error", "TEXT"),
            ("diagnostics_json", "TEXT NOT NULL"),
        ],
        "LINESTRING",
        Some(extent_for_features(extents.iter().map(Vec::as_slice))),
    )?;

    for cell in &result.cells {
        let origin = origin_lookup
            .get(cell.origin_id.as_str())
            .with_context(|| {
                format!(
                    "matrix result references unknown origin '{}'",
                    cell.origin_id
                )
            })?;
        let destination = destination_lookup
            .get(cell.destination_id.as_str())
            .with_context(|| {
                format!(
                    "matrix result references unknown destination '{}'",
                    cell.destination_id
                )
            })?;
        let geometry = matrix_cell_coords(cell, origin, destination);
        gpkg.insert_feature(
            "matrix_cells",
            &[
                ("origin_id", SqlValue::Text(cell.origin_id.clone())),
                (
                    "destination_id",
                    SqlValue::Text(cell.destination_id.clone()),
                ),
                ("source_x", SqlValue::Real(origin.lon)),
                ("source_y", SqlValue::Real(origin.lat)),
                ("target_x", SqlValue::Real(destination.lon)),
                ("target_y", SqlValue::Real(destination.lat)),
                (
                    "status",
                    SqlValue::Text(batch_status_name(cell.status).to_string()),
                ),
                (
                    "outcome",
                    SqlValue::Text(outcome_name(cell.outcome).to_string()),
                ),
                (
                    "fallback_used",
                    SqlValue::Integer(if cell.fallback_used { 1 } else { 0 }),
                ),
                (
                    "origin_component_id",
                    SqlValue::NullableInteger(optional_u32_as_i64(cell.origin_component_id)),
                ),
                (
                    "destination_component_id",
                    SqlValue::NullableInteger(optional_u32_as_i64(cell.destination_component_id)),
                ),
                (
                    "origin_hop_distance_m",
                    SqlValue::NullableReal(cell.origin_hop_distance_m),
                ),
                (
                    "destination_hop_distance_m",
                    SqlValue::NullableReal(cell.destination_hop_distance_m),
                ),
                (
                    "origin_snap_distance_m",
                    SqlValue::NullableReal(cell.origin_snap_distance_m),
                ),
                (
                    "destination_snap_distance_m",
                    SqlValue::NullableReal(cell.destination_snap_distance_m),
                ),
                (
                    "total_distance_m",
                    SqlValue::NullableInteger(cell.total_distance_m.map(|value| value as i64)),
                ),
                (
                    "total_travel_time_s",
                    SqlValue::NullableReal(cell.total_travel_time_s),
                ),
                (
                    "total_generalized_cost",
                    SqlValue::NullableReal(cell.total_generalized_cost),
                ),
                (
                    "illegal_movement_penalty_s",
                    SqlValue::NullableReal(cell.illegal_movement_penalty_s),
                ),
                (
                    "illegal_movement_penalty_cost",
                    SqlValue::NullableReal(cell.illegal_movement_penalty_cost),
                ),
                (
                    "violation_count",
                    SqlValue::Integer(cell.violation_count as i64),
                ),
                (
                    "violation_types_json",
                    SqlValue::Text(diagnostics_json(&cell.violation_types)),
                ),
                ("error", SqlValue::NullableText(cell.error.clone())),
                (
                    "diagnostics_json",
                    SqlValue::Text(diagnostics_json(&cell.diagnostics)),
                ),
            ],
            &geometry,
        )?;
    }

    Ok(())
}

fn write_matrix_parquet(
    path: &Path,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
    result: &MatrixResult,
) -> Result<()> {
    let origin_lookup = point_lookup(origins);
    let destination_lookup = point_lookup(destinations);
    let rows = result
        .cells
        .iter()
        .map(|cell| {
            let origin = origin_lookup
                .get(cell.origin_id.as_str())
                .with_context(|| {
                    format!(
                        "matrix result references unknown origin '{}'",
                        cell.origin_id
                    )
                })?;
            let destination = destination_lookup
                .get(cell.destination_id.as_str())
                .with_context(|| {
                    format!(
                        "matrix result references unknown destination '{}'",
                        cell.destination_id
                    )
                })?;
            Ok((
                cell,
                *origin,
                *destination,
                linestring_wkt(&matrix_cell_coords(cell, origin, destination)),
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let schema = Schema::new(vec![
        Field::new("origin_id", DataType::Utf8, false),
        Field::new("destination_id", DataType::Utf8, false),
        Field::new("source_x", DataType::Float64, false),
        Field::new("source_y", DataType::Float64, false),
        Field::new("target_x", DataType::Float64, false),
        Field::new("target_y", DataType::Float64, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("outcome", DataType::Utf8, false),
        Field::new("fallback_used", DataType::Utf8, false),
        Field::new("origin_component_id", DataType::UInt64, true),
        Field::new("destination_component_id", DataType::UInt64, true),
        Field::new("origin_hop_distance_m", DataType::Float64, true),
        Field::new("destination_hop_distance_m", DataType::Float64, true),
        Field::new("origin_snap_distance_m", DataType::Float64, true),
        Field::new("destination_snap_distance_m", DataType::Float64, true),
        Field::new("total_distance_m", DataType::UInt64, true),
        Field::new("total_travel_time_s", DataType::Float64, true),
        Field::new("total_generalized_cost", DataType::Float64, true),
        Field::new("illegal_movement_penalty_s", DataType::Float64, true),
        Field::new("illegal_movement_penalty_cost", DataType::Float64, true),
        Field::new("violation_count", DataType::UInt64, false),
        Field::new("violation_types_json", DataType::Utf8, false),
        Field::new("error", DataType::Utf8, true),
        Field::new("diagnostics_json", DataType::Utf8, false),
        Field::new("geometry_wkt", DataType::Utf8, false),
    ]);

    write_parquet_record_batch(
        path,
        schema,
        vec![
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.origin_id.as_str())
                    .collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.destination_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(_, origin, _, _)| origin.lon)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(_, origin, _, _)| origin.lat)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(_, _, destination, _)| destination.lon)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(_, _, destination, _)| destination.lat)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _)| batch_status_name(cell.status))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _)| outcome_name(cell.outcome))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _)| if cell.fallback_used { "true" } else { "false" })
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.origin_component_id.map(u64::from))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.destination_component_id.map(u64::from))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.origin_hop_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.destination_hop_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.origin_snap_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.destination_snap_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.total_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.total_travel_time_s)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.total_generalized_cost)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.illegal_movement_penalty_s)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.illegal_movement_penalty_cost)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.violation_count as u64)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _)| diagnostics_json(&cell.violation_types))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.error.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _)| diagnostics_json(&cell.diagnostics))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(_, _, _, geometry)| geometry.as_str())
                    .collect::<Vec<_>>(),
            )),
        ],
    )
}

fn write_matrix_geoparquet(
    path: &Path,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
    result: &MatrixResult,
) -> Result<()> {
    let origin_lookup = point_lookup(origins);
    let destination_lookup = point_lookup(destinations);
    let rows = result
        .cells
        .iter()
        .map(|cell| {
            let origin = origin_lookup
                .get(cell.origin_id.as_str())
                .with_context(|| {
                    format!(
                        "matrix result references unknown origin '{}'",
                        cell.origin_id
                    )
                })?;
            let destination = destination_lookup
                .get(cell.destination_id.as_str())
                .with_context(|| {
                    format!(
                        "matrix result references unknown destination '{}'",
                        cell.destination_id
                    )
                })?;
            let geometry = matrix_cell_coords(cell, origin, destination);
            Ok((
                cell,
                *origin,
                *destination,
                geometry.clone(),
                wkb_linestring(&geometry),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let extent = extent_for_features(
        rows.iter()
            .map(|(_, _, _, geometry, _)| geometry.as_slice()),
    );
    let schema = geoparquet_schema(
        vec![
            Field::new("origin_id", DataType::Utf8, false),
            Field::new("destination_id", DataType::Utf8, false),
            Field::new("source_x", DataType::Float64, false),
            Field::new("source_y", DataType::Float64, false),
            Field::new("target_x", DataType::Float64, false),
            Field::new("target_y", DataType::Float64, false),
            Field::new("status", DataType::Utf8, false),
            Field::new("outcome", DataType::Utf8, false),
            Field::new("fallback_used", DataType::Utf8, false),
            Field::new("origin_component_id", DataType::UInt64, true),
            Field::new("destination_component_id", DataType::UInt64, true),
            Field::new("origin_hop_distance_m", DataType::Float64, true),
            Field::new("destination_hop_distance_m", DataType::Float64, true),
            Field::new("origin_snap_distance_m", DataType::Float64, true),
            Field::new("destination_snap_distance_m", DataType::Float64, true),
            Field::new("total_distance_m", DataType::UInt64, true),
            Field::new("total_travel_time_s", DataType::Float64, true),
            Field::new("total_generalized_cost", DataType::Float64, true),
            Field::new("illegal_movement_penalty_s", DataType::Float64, true),
            Field::new("illegal_movement_penalty_cost", DataType::Float64, true),
            Field::new("violation_count", DataType::UInt64, false),
            Field::new("violation_types_json", DataType::Utf8, false),
            Field::new("error", DataType::Utf8, true),
            Field::new("diagnostics_json", DataType::Utf8, false),
            Field::new("geometry", DataType::Binary, false),
        ],
        extent,
    );

    write_parquet_record_batch(
        path,
        schema,
        vec![
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.origin_id.as_str())
                    .collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.destination_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(_, origin, _, _, _)| origin.lon)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(_, origin, _, _, _)| origin.lat)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(_, _, destination, _, _)| destination.lon)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(_, _, destination, _, _)| destination.lat)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| batch_status_name(cell.status))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| outcome_name(cell.outcome))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| if cell.fallback_used { "true" } else { "false" })
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.origin_component_id.map(u64::from))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.destination_component_id.map(u64::from))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.origin_hop_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.destination_hop_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.origin_snap_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.destination_snap_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.total_distance_m)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.total_travel_time_s)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.total_generalized_cost)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.illegal_movement_penalty_s)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(Float64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.illegal_movement_penalty_cost)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.violation_count as u64)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| diagnostics_json(&cell.violation_types))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.error.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| diagnostics_json(&cell.diagnostics))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(BinaryArray::from(
                rows.iter()
                    .map(|(_, _, _, _, wkb)| wkb.as_slice())
                    .collect::<Vec<_>>(),
            )),
        ],
    )
}

fn write_service_area_csv(
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

fn write_service_area_geojson(
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

fn write_service_area_gpkg(
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

fn write_service_area_parquet(
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

fn write_service_area_geoparquet(
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

fn route_coords(result: &RouteResult) -> Result<Vec<[f64; 2]>> {
    let geometry = result.geometry.as_ref().context(
        "route export requires geometry; request `returns.geometry: full` or use a JSON output",
    )?;
    Ok(coerce_linestring_coords(geometry))
}

fn od_pair_coords(
    pair: &netweevil_query::OdPairResult,
    request_pair: &netweevil_query::OdPair,
) -> Vec<[f64; 2]> {
    coerce_linestring_coords(&pair.geometry.clone().unwrap_or_else(|| {
        vec![
            [request_pair.origin.lon, request_pair.origin.lat],
            [request_pair.destination.lon, request_pair.destination.lat],
        ]
    }))
}

fn matrix_cell_coords(
    cell: &netweevil_query::MatrixCellResult,
    origin: &netweevil_query::LabeledPoint,
    destination: &netweevil_query::LabeledPoint,
) -> Vec<[f64; 2]> {
    coerce_linestring_coords(
        &cell
            .geometry
            .clone()
            .unwrap_or_else(|| vec![[origin.lon, origin.lat], [destination.lon, destination.lat]]),
    )
}

fn route_geometry_geojson(result: &RouteResult) -> Result<serde_json::Value> {
    Ok(json!({
        "type": "LineString",
        "coordinates": route_coords(result)?,
    }))
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

fn point_lookup(document: &PointSetDocument) -> BTreeMap<&str, &netweevil_query::LabeledPoint> {
    document
        .points
        .iter()
        .map(|point| (point.id.as_str(), point))
        .collect()
}

fn write_csv(path: &Path, header: &[&str], rows: Vec<Vec<String>>) -> Result<()> {
    ensure_parent_dir(path)?;
    let mut output = String::new();
    push_csv_row(
        &mut output,
        &header
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>(),
    );
    for row in rows {
        push_csv_row(&mut output, &row);
    }
    fs::write(path, output).with_context(|| format!("writing {}", path.display()))
}

fn push_csv_row(output: &mut String, row: &[String]) {
    let escaped = row
        .iter()
        .map(|value| csv_escape(value))
        .collect::<Vec<_>>();
    output.push_str(&escaped.join(","));
    output.push('\n');
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn optional_string<T: ToString>(value: Option<T>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn optional_u32_as_i64(value: Option<u32>) -> Option<i64> {
    value.map(i64::from)
}

fn outcome_name(outcome: AnalysisOutcome) -> &'static str {
    match outcome {
        AnalysisOutcome::Legal => "legal",
        AnalysisOutcome::Degraded => "degraded",
        AnalysisOutcome::Partial => "partial",
        AnalysisOutcome::Unreachable => "unreachable",
        AnalysisOutcome::NotImplemented => "not_implemented",
    }
}

fn diagnostics_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[]".to_string())
}

fn batch_status_name(status: BatchItemStatus) -> &'static str {
    match status {
        BatchItemStatus::Succeeded => "succeeded",
        BatchItemStatus::Ignored => "ignored",
        BatchItemStatus::Failed => "failed",
    }
}

fn coerce_linestring_coords(coords: &[[f64; 2]]) -> Vec<[f64; 2]> {
    match coords {
        [] => Vec::new(),
        [only] => vec![*only, *only],
        _ => coords.to_vec(),
    }
}

fn linestring_wkt(coords: &[[f64; 2]]) -> String {
    let coords = coerce_linestring_coords(coords);
    if coords.is_empty() {
        return String::new();
    }
    let body = coords
        .iter()
        .map(|[x, y]| format!("{x} {y}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("LINESTRING({body})")
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
struct Extent {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

fn extent_for_features<'a>(features: impl IntoIterator<Item = &'a [[f64; 2]]>) -> Extent {
    let mut extent = Extent {
        min_x: f64::INFINITY,
        min_y: f64::INFINITY,
        max_x: f64::NEG_INFINITY,
        max_y: f64::NEG_INFINITY,
    };
    for feature in features {
        for [x, y] in feature {
            extent.min_x = extent.min_x.min(*x);
            extent.min_y = extent.min_y.min(*y);
            extent.max_x = extent.max_x.max(*x);
            extent.max_y = extent.max_y.max(*y);
        }
    }
    extent
}

fn combine_extents(left: Extent, right: Extent) -> Extent {
    Extent {
        min_x: left.min_x.min(right.min_x),
        min_y: left.min_y.min(right.min_y),
        max_x: left.max_x.max(right.max_x),
        max_y: left.max_y.max(right.max_y),
    }
}

struct GeoPackageWriter {
    connection: Connection,
}

impl GeoPackageWriter {
    fn create(path: &Path) -> Result<Self> {
        ensure_parent_dir(path)?;
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("removing existing {}", path.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("creating GeoPackage {}", path.display()))?;
        let writer = Self { connection };
        writer.initialize()?;
        Ok(writer)
    }

    fn initialize(&self) -> Result<()> {
        self.connection.execute_batch(
            "
            CREATE TABLE gpkg_spatial_ref_sys (
                srs_name TEXT NOT NULL,
                srs_id INTEGER NOT NULL PRIMARY KEY,
                organization TEXT NOT NULL,
                organization_coordsys_id INTEGER NOT NULL,
                definition TEXT NOT NULL,
                description TEXT
            );
            CREATE TABLE gpkg_contents (
                table_name TEXT NOT NULL PRIMARY KEY,
                data_type TEXT NOT NULL,
                identifier TEXT UNIQUE,
                description TEXT DEFAULT '',
                last_change DATETIME NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                min_x DOUBLE,
                min_y DOUBLE,
                max_x DOUBLE,
                max_y DOUBLE,
                srs_id INTEGER,
                CONSTRAINT fk_gc_r_srs_id FOREIGN KEY (srs_id) REFERENCES gpkg_spatial_ref_sys(srs_id)
            );
            CREATE TABLE gpkg_geometry_columns (
                table_name TEXT NOT NULL,
                column_name TEXT NOT NULL,
                geometry_type_name TEXT NOT NULL,
                srs_id INTEGER NOT NULL,
                z TINYINT NOT NULL,
                m TINYINT NOT NULL,
                PRIMARY KEY (table_name, column_name),
                CONSTRAINT fk_ggc_tn FOREIGN KEY (table_name) REFERENCES gpkg_contents(table_name),
                CONSTRAINT fk_ggc_srs FOREIGN KEY (srs_id) REFERENCES gpkg_spatial_ref_sys(srs_id)
            );
            ",
        )?;
        self.connection.execute(
            "INSERT INTO gpkg_spatial_ref_sys (srs_name, srs_id, organization, organization_coordsys_id, definition, description)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "Undefined Cartesian",
                -1,
                "NONE",
                -1,
                "undefined",
                "undefined Cartesian coordinate reference system",
            ],
        )?;
        self.connection.execute(
            "INSERT INTO gpkg_spatial_ref_sys (srs_name, srs_id, organization, organization_coordsys_id, definition, description)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "Undefined Geographic",
                0,
                "NONE",
                0,
                "undefined",
                "undefined geographic coordinate reference system",
            ],
        )?;
        self.connection.execute(
            "INSERT INTO gpkg_spatial_ref_sys (srs_name, srs_id, organization, organization_coordsys_id, definition, description)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "WGS 84 geodetic",
                EPSG_4326,
                "EPSG",
                EPSG_4326,
                r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]]"#,
                "longitude/latitude coordinates in decimal degrees on the WGS 84 spheroid",
            ],
        )?;
        Ok(())
    }

    fn create_feature_table(
        &mut self,
        table_name: &str,
        columns: &[(&str, &str)],
        geometry_type_name: &str,
        extent: Option<Extent>,
    ) -> Result<()> {
        let mut sql = format!("CREATE TABLE {table_name} (id INTEGER PRIMARY KEY AUTOINCREMENT");
        for (name, definition) in columns {
            sql.push_str(&format!(", {name} {definition}"));
        }
        sql.push_str(", geom BLOB NOT NULL)");
        self.connection.execute_batch(&sql)?;
        self.connection.execute(
            "INSERT INTO gpkg_contents
             (table_name, data_type, identifier, description, min_x, min_y, max_x, max_y, srs_id)
             VALUES (?1, 'features', ?1, '', ?2, ?3, ?4, ?5, ?6)",
            params![
                table_name,
                extent.map(|value| value.min_x),
                extent.map(|value| value.min_y),
                extent.map(|value| value.max_x),
                extent.map(|value| value.max_y),
                EPSG_4326,
            ],
        )?;
        self.connection.execute(
            "INSERT INTO gpkg_geometry_columns
             (table_name, column_name, geometry_type_name, srs_id, z, m)
             VALUES (?1, 'geom', ?2, ?3, 0, 0)",
            params![table_name, geometry_type_name, EPSG_4326],
        )?;
        Ok(())
    }

    fn create_attribute_table(&mut self, table_name: &str, columns: &[(&str, &str)]) -> Result<()> {
        let mut sql = format!("CREATE TABLE {table_name} (id INTEGER PRIMARY KEY AUTOINCREMENT");
        for (name, definition) in columns {
            sql.push_str(&format!(", {name} {definition}"));
        }
        sql.push(')');
        self.connection.execute_batch(&sql)?;
        self.connection.execute(
            "INSERT INTO gpkg_contents
             (table_name, data_type, identifier, description, srs_id)
             VALUES (?1, 'attributes', ?1, '', NULL)",
            params![table_name],
        )?;
        Ok(())
    }

    fn insert_feature(
        &mut self,
        table_name: &str,
        fields: &[(&str, SqlValue)],
        coords: &[[f64; 2]],
    ) -> Result<()> {
        let mut field_names = fields.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        field_names.push("geom");
        let mut sql = format!(
            "INSERT INTO {table_name} ({}) VALUES ({})",
            field_names.join(", "),
            vec!["?"; field_names.len()].join(", ")
        );
        let geometry = gpkg_linestring(coords);
        let mut statement = self.connection.prepare(&sql)?;
        let mut values = fields
            .iter()
            .map(|(_, value)| value.as_param())
            .collect::<Vec<_>>();
        values.push(rusqlite::types::Value::Blob(geometry));
        statement.execute(rusqlite::params_from_iter(values))?;
        sql.clear();
        Ok(())
    }

    fn insert_feature_wkb(
        &mut self,
        table_name: &str,
        fields: &[(&str, SqlValue)],
        wkb: &[u8],
    ) -> Result<()> {
        let mut field_names = fields.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        field_names.push("geom");
        let sql = format!(
            "INSERT INTO {table_name} ({}) VALUES ({})",
            field_names.join(", "),
            vec!["?"; field_names.len()].join(", ")
        );
        let geometry = gpkg_wkb(wkb);
        let mut statement = self.connection.prepare(&sql)?;
        let mut values = fields
            .iter()
            .map(|(_, value)| value.as_param())
            .collect::<Vec<_>>();
        values.push(rusqlite::types::Value::Blob(geometry));
        statement.execute(rusqlite::params_from_iter(values))?;
        Ok(())
    }

    fn insert_row(&mut self, table_name: &str, fields: &[(&str, SqlValue)]) -> Result<()> {
        let field_names = fields.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        let sql = format!(
            "INSERT INTO {table_name} ({}) VALUES ({})",
            field_names.join(", "),
            vec!["?"; field_names.len()].join(", ")
        );
        let mut statement = self.connection.prepare(&sql)?;
        let values = fields
            .iter()
            .map(|(_, value)| value.as_param())
            .collect::<Vec<_>>();
        statement.execute(rusqlite::params_from_iter(values))?;
        Ok(())
    }

    fn begin_transaction(&mut self) -> Result<()> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE TRANSACTION")
            .context("starting GeoPackage transaction")
    }

    fn commit_transaction(&mut self) -> Result<()> {
        self.connection
            .execute_batch("COMMIT")
            .context("committing GeoPackage transaction")
    }
}

enum SqlValue {
    Text(String),
    NullableText(Option<String>),
    Integer(i64),
    NullableInteger(Option<i64>),
    Real(f64),
    NullableReal(Option<f64>),
}

impl SqlValue {
    fn as_param(&self) -> rusqlite::types::Value {
        use rusqlite::types::Value;

        match self {
            SqlValue::Text(value) => Value::Text(value.clone()),
            SqlValue::NullableText(value) => value
                .as_ref()
                .map_or(Value::Null, |value| Value::Text(value.clone())),
            SqlValue::Integer(value) => Value::Integer(*value),
            SqlValue::NullableInteger(value) => value.map_or(Value::Null, Value::Integer),
            SqlValue::Real(value) => Value::Real(*value),
            SqlValue::NullableReal(value) => value.map_or(Value::Null, Value::Real),
        }
    }
}

fn gpkg_linestring(coords: &[[f64; 2]]) -> Vec<u8> {
    let coords = coerce_linestring_coords(coords);
    gpkg_wkb(&wkb_linestring(&coords))
}

fn gpkg_wkb(wkb: &[u8]) -> Vec<u8> {
    let mut binary = Vec::new();
    binary.extend_from_slice(b"GP");
    binary.push(0);
    binary.push(1);
    binary.extend_from_slice(&EPSG_4326.to_le_bytes());
    binary.extend_from_slice(wkb);
    binary
}

fn wkb_linestring(coords: &[[f64; 2]]) -> Vec<u8> {
    let coords = coerce_linestring_coords(coords);
    let mut binary = Vec::with_capacity(1 + 4 + 4 + coords.len() * 16);
    binary.push(1);
    binary.extend_from_slice(&2_u32.to_le_bytes());
    binary.extend_from_slice(&(coords.len() as u32).to_le_bytes());
    for [x, y] in coords {
        binary.extend_from_slice(&x.to_le_bytes());
        binary.extend_from_slice(&y.to_le_bytes());
    }
    binary
}

fn wkb_multilinestring(lines: &[Vec<[f64; 2]>]) -> Vec<u8> {
    let mut binary = Vec::new();
    binary.push(1);
    binary.extend_from_slice(&5_u32.to_le_bytes());
    binary.extend_from_slice(&(lines.len() as u32).to_le_bytes());
    for line in lines {
        binary.extend_from_slice(&wkb_linestring(line));
    }
    binary
}

fn wkb_polygon(rings: &[Vec<[f64; 2]>]) -> Vec<u8> {
    let mut binary = Vec::new();
    binary.push(1);
    binary.extend_from_slice(&3_u32.to_le_bytes());
    binary.extend_from_slice(&(rings.len() as u32).to_le_bytes());
    for ring in rings {
        binary.extend_from_slice(&(ring.len() as u32).to_le_bytes());
        for [x, y] in ring {
            binary.extend_from_slice(&x.to_le_bytes());
            binary.extend_from_slice(&y.to_le_bytes());
        }
    }
    binary
}

fn wkb_multipolygon(polygons: &[Vec<Vec<[f64; 2]>>]) -> Vec<u8> {
    let mut binary = Vec::new();
    binary.push(1);
    binary.extend_from_slice(&6_u32.to_le_bytes());
    binary.extend_from_slice(&(polygons.len() as u32).to_le_bytes());
    for polygon in polygons {
        binary.extend_from_slice(&wkb_polygon(polygon));
    }
    binary
}

fn geoparquet_schema(fields: Vec<Field>, extent: Extent) -> Schema {
    geoparquet_schema_with_types(fields, extent, &["LineString"])
}

fn geoparquet_schema_with_types(
    fields: Vec<Field>,
    extent: Extent,
    geometry_types: &[&str],
) -> Schema {
    let geo_metadata = json!({
        "version": "1.1.0",
        "primary_column": "geometry",
        "columns": {
            "geometry": {
                "encoding": "WKB",
                "geometry_types": geometry_types,
                "bbox": [extent.min_x, extent.min_y, extent.max_x, extent.max_y]
            }
        }
    });
    Schema::new_with_metadata(
        fields,
        std::collections::HashMap::from([("geo".to_string(), geo_metadata.to_string())]),
    )
}

fn write_parquet_record_batch(path: &Path, schema: Schema, columns: Vec<ArrayRef>) -> Result<()> {
    ensure_parent_dir(path)?;
    let schema = Arc::new(schema);
    let batch =
        RecordBatch::try_new(schema.clone(), columns).context("building Parquet record batch")?;
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut writer = ArrowWriter::try_new(file, schema, None)
        .with_context(|| format!("opening Parquet writer {}", path.display()))?;
    writer
        .write(&batch)
        .with_context(|| format!("writing Parquet rows to {}", path.display()))?;
    writer
        .close()
        .with_context(|| format!("finalizing Parquet {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        coerce_linestring_coords, csv_escape, linestring_wkt, write_od_result,
        write_route_batch_result, write_route_result, write_service_area_result,
    };
    use netweevil_core::{RoadClass, SurfaceClass};
    use netweevil_profile::ReturnConfig;
    use netweevil_query::{
        AnalysisOutcome, BatchItemStatus, LabeledPoint, MetricBreakdown, OdPair, OdPairResult,
        OdPairsDocument, OdResult, RouteBatchDocument, RouteBatchEntry, RouteBatchItemResult,
        RouteBatchResult, RouteBreakdowns, RouteRequest, RouteResult, RouteSegment, RouteSummary,
        RouteViolation, RouteViolationType, ServiceAreaBandMode, ServiceAreaBoundaryMode,
        ServiceAreaFeature, ServiceAreaGeometryType, ServiceAreaMultiOriginMode,
        ServiceAreaOutputMode, ServiceAreaRequest, ServiceAreaResult, ServiceAreaThresholdMetric,
        SnappedPoint,
    };
    use rusqlite::Connection;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn duplicates_single_vertex_linestrings() {
        assert_eq!(
            coerce_linestring_coords(&[[1.0, 2.0]]),
            vec![[1.0, 2.0], [1.0, 2.0]]
        );
    }

    #[test]
    fn escapes_csv_fields() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn formats_linestring_wkt() {
        assert_eq!(
            linestring_wkt(&[[6.0, 53.0], [6.1, 53.1]]),
            "LINESTRING(6 53, 6.1 53.1)"
        );
    }

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

    #[test]
    fn writes_route_batch_geopackage_with_detail_tables() {
        let path = temp_path("route-batch.gpkg");
        let requests = RouteBatchDocument {
            requests: vec![
                RouteBatchEntry {
                    profile_id: Some("car_research_v3".to_string()),
                    request: RouteRequest {
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
                    },
                },
                RouteBatchEntry {
                    profile_id: Some("car_research_v3".to_string()),
                    request: RouteRequest {
                        route_id: "route_2".to_string(),
                        origin: LabeledPoint {
                            id: "origin_2".to_string(),
                            lon: 6.3,
                            lat: 53.3,
                        },
                        destination: LabeledPoint {
                            id: "destination_2".to_string(),
                            lon: 6.4,
                            lat: 53.4,
                        },
                        snap: Default::default(),
                        connectivity: Default::default(),
                        fallback: Default::default(),
                        returns: ReturnConfig::default(),
                        alternatives: Default::default(),
                    },
                },
            ],
        };
        let result = RouteBatchResult {
            route_count: 2,
            succeeded_count: 1,
            failed_count: 1,
            items: vec![
                RouteBatchItemResult {
                    route_id: "route_1".to_string(),
                    origin_id: "origin".to_string(),
                    destination_id: "destination".to_string(),
                    status: BatchItemStatus::Succeeded,
                    route: Some(RouteResult {
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
                            violation_count: 1,
                            violation_types: vec![RouteViolationType::IllegalTurn],
                            total_distance_m: 1_000,
                            total_travel_time_s: 120.0,
                            total_generalized_cost: 120.0,
                            segment_count: 2,
                        },
                        node_path: vec![1, 2, 3],
                        edge_path: vec![10, 11],
                        geometry: Some(vec![[6.0, 53.0], [6.1, 53.1], [6.2, 53.2]]),
                        hop_segments: vec![],
                        segments: Some(vec![
                            RouteSegment {
                                edge_id: 10,
                                from_node_id: 1,
                                to_node_id: 2,
                                source_way_id: 100,
                                length_m: 500,
                                travel_time_s: 60.0,
                                generalized_cost: 60.0,
                                road_class: RoadClass::Residential,
                                surface: SurfaceClass::Paved,
                                name: Some("Alpha".to_string()),
                                violation_type: None,
                            },
                            RouteSegment {
                                edge_id: 11,
                                from_node_id: 2,
                                to_node_id: 3,
                                source_way_id: 101,
                                length_m: 500,
                                travel_time_s: 60.0,
                                generalized_cost: 60.0,
                                road_class: RoadClass::Residential,
                                surface: SurfaceClass::Paved,
                                name: Some("Beta".to_string()),
                                violation_type: Some(RouteViolationType::IllegalTurn),
                            },
                        ]),
                        breakdowns: Some(RouteBreakdowns {
                            road_class: BTreeMap::from([(
                                "residential".to_string(),
                                MetricBreakdown {
                                    distance_m: Some(1_000),
                                    time_s: Some(120.0),
                                },
                            )]),
                            surface: BTreeMap::from([(
                                "paved".to_string(),
                                MetricBreakdown {
                                    distance_m: Some(1_000),
                                    time_s: Some(120.0),
                                },
                            )]),
                        }),
                        violations: vec![RouteViolation {
                            violation_type: RouteViolationType::IllegalTurn,
                            edge_id: Some(11),
                            from_edge_id: Some(10),
                            to_edge_id: Some(11),
                            distance_m: Some(12.0),
                            penalty_s: 4.0,
                            penalty_generalized_cost: 4.0,
                        }],
                        diagnostics: vec![],
                        warnings: vec![],
                        alternatives: vec![],
                    }),
                    error: None,
                },
                RouteBatchItemResult {
                    route_id: "route_2".to_string(),
                    origin_id: "origin_2".to_string(),
                    destination_id: "destination_2".to_string(),
                    status: BatchItemStatus::Failed,
                    route: None,
                    error: Some("snap failed".to_string()),
                },
            ],
            warnings: vec![],
        };

        write_route_batch_result(&path, &requests, &result).expect("route batch gpkg written");

        let connection = Connection::open(&path).expect("gpkg opens");
        let routes_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM routes", [], |row| row.get(0))
            .expect("routes count query succeeds");
        let segments_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM route_segments", [], |row| row.get(0))
            .expect("segments count query succeeds");
        let road_breakdown_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM route_breakdown_road_class",
                [],
                |row| row.get(0),
            )
            .expect("road breakdown count query succeeds");
        let surface_breakdown_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM route_breakdown_surface", [], |row| {
                row.get(0)
            })
            .expect("surface breakdown count query succeeds");
        let violations_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM route_violations", [], |row| {
                row.get(0)
            })
            .expect("violations count query succeeds");
        let failures_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM route_failures", [], |row| row.get(0))
            .expect("failures count query succeeds");

        assert_eq!(routes_count, 1);
        assert_eq!(segments_count, 2);
        assert_eq!(road_breakdown_count, 1);
        assert_eq!(surface_breakdown_count, 1);
        assert_eq!(violations_count, 1);
        assert_eq!(failures_count, 1);

        fs::remove_file(path).ok();
    }

    #[test]
    fn writes_od_csv_with_actual_route_geometry_when_present() {
        let path = temp_path("od.csv");
        let request = OdPairsDocument {
            pairs: vec![OdPair {
                pair_id: "pair_1".to_string(),
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
            }],
            snap: Default::default(),
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
            alternatives: Default::default(),
        };
        let result = OdResult {
            pair_count: 1,
            succeeded_count: 1,
            failed_count: 0,
            ignored_count: 0,
            pairs: vec![OdPairResult {
                pair_id: "pair_1".to_string(),
                origin_id: "origin".to_string(),
                destination_id: "destination".to_string(),
                status: BatchItemStatus::Succeeded,
                outcome: AnalysisOutcome::Legal,
                fallback_used: false,
                origin_component_id: Some(0),
                destination_component_id: Some(0),
                origin_hop_distance_m: None,
                destination_hop_distance_m: None,
                origin_snap_distance_m: Some(1.0),
                destination_snap_distance_m: Some(2.0),
                total_distance_m: Some(1_000),
                total_travel_time_s: Some(120.0),
                total_generalized_cost: Some(120.0),
                illegal_movement_penalty_s: Some(0.0),
                illegal_movement_penalty_cost: Some(0.0),
                violation_count: 0,
                violation_types: vec![],
                geometry: Some(vec![[6.0, 53.0], [6.1, 53.1], [6.2, 53.2]]),
                diagnostics: vec![],
                error: None,
                alternatives: vec![],
            }],
            diagnostics: vec![],
            warnings: vec![],
        };

        write_od_result(&path, &request, &result).expect("csv written");

        let raw = fs::read_to_string(&path).expect("csv readable");
        assert!(raw.contains("outcome"));
        assert!(raw.contains("diagnostics_json"));
        assert!(raw.contains("legal"));
        assert!(raw.contains("LINESTRING(6 53, 6.1 53.1, 6.2 53.2)"));

        fs::remove_file(path).ok();
    }

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

    fn temp_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time works")
            .as_nanos();
        std::env::temp_dir().join(format!("netweevil-report-{unique}-{name}"))
    }
}
