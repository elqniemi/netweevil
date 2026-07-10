use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{ArrayRef, BinaryArray, Float64Array, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use netweevil_query::{MatrixResult, PointSetDocument};
use serde_json::json;

use super::common::*;
use super::gpkg::*;
use super::write_json;

pub(super) fn write_matrix_csv(
    path: &Path,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
    result: &MatrixResult,
) -> Result<()> {
    let component_columns = component_columns(result.cells.iter().map(|cell| &cell.components));
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
            let mut row = vec![
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
                linestring_wkt_z(&geometry),
            ];
            row.extend(component_csv_values(&cell.components, &component_columns));
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()?;

    let mut header = vec![
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

pub(super) fn write_matrix_geojson(
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
                    "components": cell.components,
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

pub(super) fn write_matrix_gpkg(
    path: &Path,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
    result: &MatrixResult,
) -> Result<()> {
    let component_columns = component_columns(result.cells.iter().map(|cell| &cell.components));
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
    let mut definitions = vec![
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
    ];
    definitions.extend(
        component_columns
            .iter()
            .map(|column| (column.column_name.as_str(), "REAL")),
    );
    gpkg.create_feature_table_3d(
        "matrix_cells",
        &definitions,
        "LINESTRING",
        Some(extent_for_features_z(extents.iter().map(Vec::as_slice))),
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
        let mut fields = vec![
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
        ];
        fields.extend(component_columns.iter().map(|column| {
            (
                column.column_name.as_str(),
                SqlValue::NullableReal(cell.components.get(&column.component_name).copied()),
            )
        }));
        gpkg.insert_feature_3d("matrix_cells", &fields, &geometry)?;
    }

    Ok(())
}

pub(super) fn write_matrix_parquet(
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
                linestring_wkt_z(&matrix_cell_coords(cell, origin, destination)),
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let component_columns = component_columns(result.cells.iter().map(|cell| &cell.components));
    let mut schema_fields = vec![
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
    ];
    schema_fields.extend(
        component_columns
            .iter()
            .map(|column| Field::new(&column.column_name, DataType::Float64, true)),
    );
    let schema = Schema::new(schema_fields);

    let mut arrays = vec![
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
    ];
    arrays.extend(component_arrow_arrays(
        result.cells.iter().map(|cell| &cell.components),
        &component_columns,
    ));
    write_parquet_record_batch(path, schema, arrays)
}

pub(super) fn write_matrix_geoparquet(
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
                wkb_linestring_z(&geometry),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let extent = extent_for_features_z(
        rows.iter()
            .map(|(_, _, _, geometry, _)| geometry.as_slice()),
    );
    let component_columns = component_columns(result.cells.iter().map(|cell| &cell.components));
    let mut schema_fields = vec![
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
    ];
    arrays.extend(component_arrow_arrays(
        result.cells.iter().map(|cell| &cell.components),
        &component_columns,
    ));
    arrays.push(Arc::new(BinaryArray::from(
        rows.iter()
            .map(|(_, _, _, _, wkb)| wkb.as_slice())
            .collect::<Vec<_>>(),
    )));
    write_parquet_record_batch(path, schema, arrays)
}

fn matrix_cell_coords(
    cell: &netweevil_query::MatrixCellResult,
    origin: &netweevil_query::LabeledPoint,
    destination: &netweevil_query::LabeledPoint,
) -> Vec<[f64; 3]> {
    coerce_linestring_coords_z(&cell.geometry.clone().unwrap_or_else(|| {
        vec![
            [origin.lon, origin.lat, 0.0],
            [destination.lon, destination.lat, 0.0],
        ]
    }))
}

fn point_lookup(document: &PointSetDocument) -> BTreeMap<&str, &netweevil_query::LabeledPoint> {
    document
        .points
        .iter()
        .map(|point| (point.id.as_str(), point))
        .collect()
}
