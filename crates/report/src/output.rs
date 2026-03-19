use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use arrow_array::{ArrayRef, BinaryArray, Float64Array, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use netan_query::{
    BatchItemStatus, MatrixResult, OdPairsDocument, OdResult, PointSetDocument, RouteRequest,
    RouteResult,
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

    write_csv(
        path,
        &[
            "route_id",
            "origin_id",
            "destination_id",
            "origin_snap_distance_m",
            "destination_snap_distance_m",
            "total_distance_m",
            "total_travel_time_s",
            "total_generalized_cost",
            "segment_count",
            "geometry_wkt",
            "warnings",
        ],
        vec![vec![
            result.route_id.clone(),
            request.origin.id.clone(),
            request.destination.id.clone(),
            result.origin.snap_distance_m.to_string(),
            result.destination.snap_distance_m.to_string(),
            result.summary.total_distance_m.to_string(),
            result.summary.total_travel_time_s.to_string(),
            result.summary.total_generalized_cost.to_string(),
            result.summary.segment_count.to_string(),
            geometry_wkt,
            warnings,
        ]],
    )
}

fn write_route_geojson(path: &Path, request: &RouteRequest, result: &RouteResult) -> Result<()> {
    let geometry = route_geometry_geojson(result)?;
    let feature_collection = json!({
        "type": "FeatureCollection",
        "features": [
            {
                "type": "Feature",
                "geometry": geometry,
                "properties": {
                    "route_id": result.route_id,
                    "origin_id": request.origin.id,
                    "destination_id": request.destination.id,
                    "origin_snap_distance_m": result.origin.snap_distance_m,
                    "destination_snap_distance_m": result.destination.snap_distance_m,
                    "total_distance_m": result.summary.total_distance_m,
                    "total_travel_time_s": result.summary.total_travel_time_s,
                    "total_generalized_cost": result.summary.total_generalized_cost,
                    "segment_count": result.summary.segment_count,
                    "warnings": result.warnings,
                }
            }
        ]
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
            ("origin_snap_distance_m", "REAL NOT NULL"),
            ("destination_snap_distance_m", "REAL NOT NULL"),
            ("total_distance_m", "INTEGER NOT NULL"),
            ("total_travel_time_s", "REAL NOT NULL"),
            ("total_generalized_cost", "REAL NOT NULL"),
            ("segment_count", "INTEGER NOT NULL"),
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
                "segment_count",
                SqlValue::Integer(result.summary.segment_count as i64),
            ),
            ("warnings", SqlValue::Text(result.warnings.join(" | "))),
        ],
        &coords,
    )?;
    Ok(())
}

fn write_route_parquet(path: &Path, request: &RouteRequest, result: &RouteResult) -> Result<()> {
    let geometry_wkt = result
        .geometry
        .as_deref()
        .map(linestring_wkt)
        .unwrap_or_default();
    let warnings = result.warnings.join(" | ");
    let schema = Schema::new(vec![
        Field::new("route_id", DataType::Utf8, false),
        Field::new("origin_id", DataType::Utf8, false),
        Field::new("destination_id", DataType::Utf8, false),
        Field::new("origin_snap_distance_m", DataType::Float64, false),
        Field::new("destination_snap_distance_m", DataType::Float64, false),
        Field::new("total_distance_m", DataType::UInt64, false),
        Field::new("total_travel_time_s", DataType::Float64, false),
        Field::new("total_generalized_cost", DataType::Float64, false),
        Field::new("segment_count", DataType::UInt64, false),
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
            Arc::new(Float64Array::from(vec![result.origin.snap_distance_m])),
            Arc::new(Float64Array::from(vec![result.destination.snap_distance_m])),
            Arc::new(UInt64Array::from(vec![result.summary.total_distance_m])),
            Arc::new(Float64Array::from(vec![result.summary.total_travel_time_s])),
            Arc::new(Float64Array::from(vec![
                result.summary.total_generalized_cost,
            ])),
            Arc::new(UInt64Array::from(vec![result.summary.segment_count as u64])),
            Arc::new(StringArray::from(vec![geometry_wkt.as_str()])),
            Arc::new(StringArray::from(vec![warnings.as_str()])),
        ],
    )
}

fn write_route_geoparquet(path: &Path, request: &RouteRequest, result: &RouteResult) -> Result<()> {
    let coords = route_coords(result)?;
    let geometry = wkb_linestring(&coords);
    let warnings = result.warnings.join(" | ");
    let extent = extent_for_features([coords.as_slice()]);
    let schema = geoparquet_schema(
        vec![
            Field::new("route_id", DataType::Utf8, false),
            Field::new("origin_id", DataType::Utf8, false),
            Field::new("destination_id", DataType::Utf8, false),
            Field::new("origin_snap_distance_m", DataType::Float64, false),
            Field::new("destination_snap_distance_m", DataType::Float64, false),
            Field::new("total_distance_m", DataType::UInt64, false),
            Field::new("total_travel_time_s", DataType::Float64, false),
            Field::new("total_generalized_cost", DataType::Float64, false),
            Field::new("segment_count", DataType::UInt64, false),
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
            Arc::new(Float64Array::from(vec![result.origin.snap_distance_m])),
            Arc::new(Float64Array::from(vec![result.destination.snap_distance_m])),
            Arc::new(UInt64Array::from(vec![result.summary.total_distance_m])),
            Arc::new(Float64Array::from(vec![result.summary.total_travel_time_s])),
            Arc::new(Float64Array::from(vec![
                result.summary.total_generalized_cost,
            ])),
            Arc::new(UInt64Array::from(vec![result.summary.segment_count as u64])),
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
                optional_string(pair.origin_snap_distance_m),
                optional_string(pair.destination_snap_distance_m),
                optional_string(pair.total_distance_m),
                optional_string(pair.total_travel_time_s),
                optional_string(pair.total_generalized_cost),
                pair.error.clone().unwrap_or_default(),
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
            "origin_snap_distance_m",
            "destination_snap_distance_m",
            "total_distance_m",
            "total_travel_time_s",
            "total_generalized_cost",
            "error",
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
                    "origin_snap_distance_m": pair.origin_snap_distance_m,
                    "destination_snap_distance_m": pair.destination_snap_distance_m,
                    "total_distance_m": pair.total_distance_m,
                    "total_travel_time_s": pair.total_travel_time_s,
                    "total_generalized_cost": pair.total_generalized_cost,
                    "error": pair.error,
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
            ("origin_snap_distance_m", "REAL"),
            ("destination_snap_distance_m", "REAL"),
            ("total_distance_m", "INTEGER"),
            ("total_travel_time_s", "REAL"),
            ("total_generalized_cost", "REAL"),
            ("error", "TEXT"),
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
                ("error", SqlValue::NullableText(pair.error.clone())),
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
        Field::new("origin_snap_distance_m", DataType::Float64, true),
        Field::new("destination_snap_distance_m", DataType::Float64, true),
        Field::new("total_distance_m", DataType::UInt64, true),
        Field::new("total_travel_time_s", DataType::Float64, true),
        Field::new("total_generalized_cost", DataType::Float64, true),
        Field::new("error", DataType::Utf8, true),
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
    let error = rows
        .iter()
        .map(|(pair, _, _)| pair.error.as_deref())
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
            Arc::new(Float64Array::from(origin_snap)),
            Arc::new(Float64Array::from(destination_snap)),
            Arc::new(UInt64Array::from(total_distance)),
            Arc::new(Float64Array::from(total_travel_time)),
            Arc::new(Float64Array::from(total_cost)),
            Arc::new(StringArray::from(error)),
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
            Field::new("origin_snap_distance_m", DataType::Float64, true),
            Field::new("destination_snap_distance_m", DataType::Float64, true),
            Field::new("total_distance_m", DataType::UInt64, true),
            Field::new("total_travel_time_s", DataType::Float64, true),
            Field::new("total_generalized_cost", DataType::Float64, true),
            Field::new("error", DataType::Utf8, true),
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
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(pair, _, _, _)| pair.error.as_deref())
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
                optional_string(cell.origin_snap_distance_m),
                optional_string(cell.destination_snap_distance_m),
                optional_string(cell.total_distance_m),
                optional_string(cell.total_travel_time_s),
                optional_string(cell.total_generalized_cost),
                cell.error.clone().unwrap_or_default(),
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
            "origin_snap_distance_m",
            "destination_snap_distance_m",
            "total_distance_m",
            "total_travel_time_s",
            "total_generalized_cost",
            "error",
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
                    "origin_snap_distance_m": cell.origin_snap_distance_m,
                    "destination_snap_distance_m": cell.destination_snap_distance_m,
                    "total_distance_m": cell.total_distance_m,
                    "total_travel_time_s": cell.total_travel_time_s,
                    "total_generalized_cost": cell.total_generalized_cost,
                    "error": cell.error,
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
            ("origin_snap_distance_m", "REAL"),
            ("destination_snap_distance_m", "REAL"),
            ("total_distance_m", "INTEGER"),
            ("total_travel_time_s", "REAL"),
            ("total_generalized_cost", "REAL"),
            ("error", "TEXT"),
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
                ("error", SqlValue::NullableText(cell.error.clone())),
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
        Field::new("origin_snap_distance_m", DataType::Float64, true),
        Field::new("destination_snap_distance_m", DataType::Float64, true),
        Field::new("total_distance_m", DataType::UInt64, true),
        Field::new("total_travel_time_s", DataType::Float64, true),
        Field::new("total_generalized_cost", DataType::Float64, true),
        Field::new("error", DataType::Utf8, true),
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
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _)| cell.error.as_deref())
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
            Field::new("origin_snap_distance_m", DataType::Float64, true),
            Field::new("destination_snap_distance_m", DataType::Float64, true),
            Field::new("total_distance_m", DataType::UInt64, true),
            Field::new("total_travel_time_s", DataType::Float64, true),
            Field::new("total_generalized_cost", DataType::Float64, true),
            Field::new("error", DataType::Utf8, true),
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
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|(cell, _, _, _, _)| cell.error.as_deref())
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

fn route_coords(result: &RouteResult) -> Result<Vec<[f64; 2]>> {
    let geometry = result.geometry.as_ref().context(
        "route export requires geometry; request `returns.geometry: full` or use a JSON output",
    )?;
    Ok(coerce_linestring_coords(geometry))
}

fn od_pair_coords(
    pair: &netan_query::OdPairResult,
    request_pair: &netan_query::OdPair,
) -> Vec<[f64; 2]> {
    coerce_linestring_coords(&pair.geometry.clone().unwrap_or_else(|| {
        vec![
            [request_pair.origin.lon, request_pair.origin.lat],
            [request_pair.destination.lon, request_pair.destination.lat],
        ]
    }))
}

fn matrix_cell_coords(
    cell: &netan_query::MatrixCellResult,
    origin: &netan_query::LabeledPoint,
    destination: &netan_query::LabeledPoint,
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

fn point_lookup(document: &PointSetDocument) -> BTreeMap<&str, &netan_query::LabeledPoint> {
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

fn batch_status_name(status: BatchItemStatus) -> &'static str {
    match status {
        BatchItemStatus::Succeeded => "succeeded",
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

#[derive(Debug, Clone, Copy)]
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
    let mut binary = Vec::new();
    binary.extend_from_slice(b"GP");
    binary.push(0);
    binary.push(1);
    binary.extend_from_slice(&EPSG_4326.to_le_bytes());

    binary.push(1);
    binary.extend_from_slice(&2_u32.to_le_bytes());
    binary.extend_from_slice(&(coords.len() as u32).to_le_bytes());
    for [x, y] in coords {
        binary.extend_from_slice(&x.to_le_bytes());
        binary.extend_from_slice(&y.to_le_bytes());
    }
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

fn geoparquet_schema(fields: Vec<Field>, extent: Extent) -> Schema {
    let geo_metadata = json!({
        "version": "1.1.0",
        "primary_column": "geometry",
        "columns": {
            "geometry": {
                "encoding": "WKB",
                "geometry_types": ["LineString"],
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
        coerce_linestring_coords, csv_escape, linestring_wkt, write_od_result, write_route_result,
    };
    use netan_profile::ReturnConfig;
    use netan_query::{
        BatchItemStatus, LabeledPoint, OdPair, OdPairResult, OdPairsDocument, OdResult,
        RouteRequest, RouteResult, RouteSummary, SnappedPoint,
    };
    use rusqlite::Connection;
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
            returns: ReturnConfig::default(),
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
            },
            destination: SnappedPoint {
                point_id: "destination".to_string(),
                requested_lon: 6.2,
                requested_lat: 53.2,
                snapped_node_id: 2,
                snapped_lon: 6.2,
                snapped_lat: 53.2,
                snap_distance_m: 20.0,
            },
            summary: RouteSummary {
                total_distance_m: 1_000,
                total_travel_time_s: 120.0,
                total_generalized_cost: 120.0,
                segment_count: 2,
            },
            node_path: vec![1, 2, 3],
            edge_path: vec![10, 11],
            geometry: Some(vec![[6.0, 53.0], [6.1, 53.1], [6.2, 53.2]]),
            segments: None,
            breakdowns: None,
            warnings: vec![],
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
            returns: ReturnConfig::default(),
        };
        let result = OdResult {
            pair_count: 1,
            succeeded_count: 1,
            failed_count: 0,
            pairs: vec![OdPairResult {
                pair_id: "pair_1".to_string(),
                origin_id: "origin".to_string(),
                destination_id: "destination".to_string(),
                status: BatchItemStatus::Succeeded,
                origin_snap_distance_m: Some(1.0),
                destination_snap_distance_m: Some(2.0),
                total_distance_m: Some(1_000),
                total_travel_time_s: Some(120.0),
                total_generalized_cost: Some(120.0),
                geometry: Some(vec![[6.0, 53.0], [6.1, 53.1], [6.2, 53.2]]),
                error: None,
            }],
            warnings: vec![],
        };

        write_od_result(&path, &request, &result).expect("csv written");

        let raw = fs::read_to_string(&path).expect("csv readable");
        assert!(raw.contains("LINESTRING(6 53, 6.1 53.1, 6.2 53.2)"));

        fs::remove_file(path).ok();
    }

    fn temp_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time works")
            .as_nanos();
        std::env::temp_dir().join(format!("netan-report-{unique}-{name}"))
    }
}
