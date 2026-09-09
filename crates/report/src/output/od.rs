use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{ArrayRef, BinaryArray, Float64Array, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use netweevil_query::{OdPairsDocument, OdResult};
use serde_json::json;

use super::common::*;
use super::gpkg::*;
use super::write_json;

pub(super) fn write_od_csv(
    path: &Path,
    request: &OdPairsDocument,
    result: &OdResult,
) -> Result<()> {
    let component_columns = component_columns(result.pairs.iter().map(|pair| &pair.components));
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
            let mut row = vec![
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
                linestring_wkt_z(&geometry),
            ];
            row.extend(component_csv_values(&pair.components, &component_columns));
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()?;

    let mut header = vec![
        "pair_id",
        "origin_id",
        "destination_id",
        "source_lon",
        "source_lat",
        "target_lon",
        "target_lat",
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
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    header.extend(
        component_columns
            .iter()
            .map(|column| column.column_name.clone()),
    );
    write_csv(path, &header, rows)
}

pub(super) fn write_od_geojson(
    path: &Path,
    request: &OdPairsDocument,
    result: &OdResult,
) -> Result<()> {
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
                    "components": pair.components,
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

pub(super) fn write_od_gpkg(
    path: &Path,
    request: &OdPairsDocument,
    result: &OdResult,
) -> Result<()> {
    let component_columns = component_columns(result.pairs.iter().map(|pair| &pair.components));
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
    let mut definitions = vec![
        ("pair_id", "TEXT NOT NULL"),
        ("origin_id", "TEXT NOT NULL"),
        ("destination_id", "TEXT NOT NULL"),
        ("source_lon", "REAL NOT NULL"),
        ("source_lat", "REAL NOT NULL"),
        ("target_lon", "REAL NOT NULL"),
        ("target_lat", "REAL NOT NULL"),
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
    ];
    definitions.extend(
        component_columns
            .iter()
            .map(|column| (column.column_name.as_str(), "REAL")),
    );
    gpkg.create_feature_table_3d(
        "od_pairs",
        &definitions,
        "LINESTRING",
        Some(extent_for_features_z(extents.iter().map(Vec::as_slice))),
    )?;

    for pair in &result.pairs {
        let request_pair = request_lookup
            .get(pair.pair_id.as_str())
            .with_context(|| format!("OD result references unknown pair '{}'", pair.pair_id))?;
        let geometry = od_pair_coords(pair, request_pair);
        let mut fields = vec![
            ("pair_id", SqlValue::Text(pair.pair_id.clone())),
            ("origin_id", SqlValue::Text(pair.origin_id.clone())),
            (
                "destination_id",
                SqlValue::Text(pair.destination_id.clone()),
            ),
            ("source_lon", SqlValue::Real(request_pair.origin.lon)),
            ("source_lat", SqlValue::Real(request_pair.origin.lat)),
            ("target_lon", SqlValue::Real(request_pair.destination.lon)),
            ("target_lat", SqlValue::Real(request_pair.destination.lat)),
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
        ];
        fields.extend(component_columns.iter().map(|column| {
            (
                column.column_name.as_str(),
                SqlValue::NullableReal(pair.components.get(&column.component_name).copied()),
            )
        }));
        gpkg.insert_feature_3d("od_pairs", &fields, &geometry)?;
    }

    Ok(())
}

pub(super) fn write_od_parquet(
    path: &Path,
    request: &OdPairsDocument,
    result: &OdResult,
) -> Result<()> {
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
                linestring_wkt_z(&od_pair_coords(pair, request_pair)),
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let component_columns = component_columns(result.pairs.iter().map(|pair| &pair.components));
    let mut schema_fields = vec![
        Field::new("pair_id", DataType::Utf8, false),
        Field::new("origin_id", DataType::Utf8, false),
        Field::new("destination_id", DataType::Utf8, false),
        Field::new("source_lon", DataType::Float64, false),
        Field::new("source_lat", DataType::Float64, false),
        Field::new("target_lon", DataType::Float64, false),
        Field::new("target_lat", DataType::Float64, false),
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
    ];
    schema_fields.extend(
        component_columns
            .iter()
            .map(|column| Field::new(&column.column_name, DataType::Float64, true)),
    );
    let schema = Schema::new(schema_fields);

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
    let source_lon = rows
        .iter()
        .map(|(_, request_pair, _)| request_pair.origin.lon)
        .collect::<Vec<_>>();
    let source_lat = rows
        .iter()
        .map(|(_, request_pair, _)| request_pair.origin.lat)
        .collect::<Vec<_>>();
    let target_lon = rows
        .iter()
        .map(|(_, request_pair, _)| request_pair.destination.lon)
        .collect::<Vec<_>>();
    let target_lat = rows
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

    let mut arrays = vec![
        Arc::new(StringArray::from(pair_ids)) as ArrayRef,
        Arc::new(StringArray::from(origin_ids)),
        Arc::new(StringArray::from(destination_ids)),
        Arc::new(Float64Array::from(source_lon)),
        Arc::new(Float64Array::from(source_lat)),
        Arc::new(Float64Array::from(target_lon)),
        Arc::new(Float64Array::from(target_lat)),
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
    ];
    arrays.extend(component_arrow_arrays(
        result.pairs.iter().map(|pair| &pair.components),
        &component_columns,
    ));
    write_parquet_record_batch(path, schema, arrays)
}

pub(super) fn write_od_geoparquet(
    path: &Path,
    request: &OdPairsDocument,
    result: &OdResult,
) -> Result<()> {
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
            let wkb = wkb_linestring_z(&geometry);
            Ok((pair, request_pair, geometry, wkb))
        })
        .collect::<Result<Vec<_>>>()?;
    let extent = extent_for_features_z(rows.iter().map(|(_, _, geometry, _)| geometry.as_slice()));
    let component_columns = component_columns(result.pairs.iter().map(|pair| &pair.components));
    let mut schema_fields = vec![
        Field::new("pair_id", DataType::Utf8, false),
        Field::new("origin_id", DataType::Utf8, false),
        Field::new("destination_id", DataType::Utf8, false),
        Field::new("source_lon", DataType::Float64, false),
        Field::new("source_lat", DataType::Float64, false),
        Field::new("target_lon", DataType::Float64, false),
        Field::new("target_lat", DataType::Float64, false),
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
    ];
    schema_fields.extend(
        component_columns
            .iter()
            .map(|column| Field::new(&column.column_name, DataType::Float64, true)),
    );
    schema_fields.push(Field::new("geometry", DataType::Binary, false));
    let schema = geoparquet_schema_z(schema_fields, extent);

    let mut arrays = vec![
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
    ];
    arrays.extend(component_arrow_arrays(
        result.pairs.iter().map(|pair| &pair.components),
        &component_columns,
    ));
    arrays.push(Arc::new(BinaryArray::from(
        rows.iter()
            .map(|(_, _, _, wkb)| wkb.as_slice())
            .collect::<Vec<_>>(),
    )));
    write_parquet_record_batch(path, schema, arrays)
}

fn od_pair_coords(
    pair: &netweevil_query::OdPairResult,
    request_pair: &netweevil_query::OdPair,
) -> Vec<[f64; 3]> {
    coerce_linestring_coords_z(&pair.geometry.clone().unwrap_or_else(|| {
        vec![
            [request_pair.origin.lon, request_pair.origin.lat, 0.0],
            [
                request_pair.destination.lon,
                request_pair.destination.lat,
                0.0,
            ],
        ]
    }))
}

#[cfg(test)]
mod tests {
    use crate::output::{temp_path, write_od_result};

    use netweevil_profile::ReturnConfig;
    use netweevil_query::{
        AnalysisOutcome, BatchItemStatus, LabeledPoint, OdPair, OdPairResult, OdPairsDocument,
        OdResult,
    };

    use std::collections::BTreeMap;
    use std::fs;

    #[test]
    fn writes_od_components_across_tabular_formats() {
        let path = temp_path("od.csv");
        let request = OdPairsDocument {
            pairs: vec![OdPair {
                pair_id: "pair_1".to_string(),
                origin: LabeledPoint {
                    id: "origin".to_string(),
                    lon: 6.0,
                    lat: 53.0,
                    z: None,
                },
                destination: LabeledPoint {
                    id: "destination".to_string(),
                    lon: 6.2,
                    lat: 53.2,
                    z: None,
                },
            }],
            snap: Default::default(),
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
            alternatives: Default::default(),
            temporal: Default::default(),
        };
        let result = OdResult {
            departure_time: None,
            arrive_by: None,
            scenario_id: None,
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
                components: BTreeMap::from([("Slope Cost".to_string(), 3.5)]),
                illegal_movement_penalty_s: Some(0.0),
                illegal_movement_penalty_cost: Some(0.0),
                violation_count: 0,
                violation_types: vec![],
                geometry: Some(vec![
                    [6.0, 53.0, 12.0],
                    [6.1, 53.1, 15.0],
                    [6.2, 53.2, 18.0],
                ]),
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
        assert!(raw.lines().next().unwrap().contains("component_slope_cost"));
        assert!(raw.lines().nth(1).unwrap().ends_with(",3.5"));
        assert!(raw.contains("legal"));
        assert!(raw.contains("LINESTRING Z(6 53 12, 6.1 53.1 15, 6.2 53.2 18)"));

        let gpkg_path = temp_path("od-components.gpkg");
        write_od_result(&gpkg_path, &request, &result).expect("OD GeoPackage written");
        let parquet_path = temp_path("od-components.parquet");
        write_od_result(&parquet_path, &request, &result).expect("OD Parquet written");
        let geoparquet_path = temp_path("od-components.geoparquet");
        write_od_result(&geoparquet_path, &request, &result).expect("OD GeoParquet written");
        let geojson_path = temp_path("od-components.geojson");
        write_od_result(&geojson_path, &request, &result).expect("OD GeoJSON written");
        let geojson = fs::read_to_string(&geojson_path).expect("OD GeoJSON readable");
        assert!(geojson.contains("\"components\""));
        assert!(geojson.contains("\"Slope Cost\": 3.5"));

        fs::remove_file(path).ok();
        fs::remove_file(gpkg_path).ok();
        fs::remove_file(parquet_path).ok();
        fs::remove_file(geoparquet_path).ok();
        fs::remove_file(geojson_path).ok();
    }
}
