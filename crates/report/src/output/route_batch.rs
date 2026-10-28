use std::path::Path;

use anyhow::{Result, bail};
use netweevil_query::{RouteBatchDocument, RouteBatchResult};

use super::common::*;
use super::gpkg::*;
use super::route::route_coords;

pub(super) fn write_route_batch_gpkg(
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
    let route_component_columns = component_columns(
        result
            .items
            .iter()
            .filter_map(|item| item.route.as_ref())
            .map(|route| &route.summary.components),
    );
    let segment_component_columns = component_columns(
        result
            .items
            .iter()
            .filter_map(|item| item.route.as_ref())
            .filter_map(|route| route.segments.as_ref())
            .flatten()
            .map(|segment| &segment.components),
    );

    let mut gpkg = GeoPackageWriter::create(path)?;
    let mut route_columns = vec![
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
        ("waiting_time_s", "REAL NOT NULL"),
        ("departure_time", "TEXT"),
        ("arrival_time", "TEXT"),
        ("scenario_id", "TEXT"),
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
    ];
    route_columns.extend(
        route_component_columns
            .iter()
            .map(|column| (column.column_name.as_str(), "REAL")),
    );
    gpkg.create_feature_table_3d(
        "routes",
        &route_columns,
        "LINESTRING",
        (!successful_extents.is_empty())
            .then(|| extent_for_features_z(successful_extents.iter().map(Vec::as_slice))),
    )?;
    let mut segment_columns = vec![
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
        ("waiting_time_s", "REAL NOT NULL"),
        ("entry_time", "TEXT"),
        ("exit_time", "TEXT"),
        ("road_class", "TEXT NOT NULL"),
        ("surface", "TEXT NOT NULL"),
        ("name", "TEXT"),
        ("violation_type", "TEXT"),
    ];
    segment_columns.extend(
        segment_component_columns
            .iter()
            .map(|column| (column.column_name.as_str(), "REAL")),
    );
    gpkg.create_attribute_table("route_segments", &segment_columns)?;
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
            let mut route_fields = vec![
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
                    SqlValue::NullableInteger(optional_u32_as_i64(route.destination.component_id)),
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
                    "waiting_time_s",
                    SqlValue::Real(route.summary.waiting_time_s),
                ),
                (
                    "departure_time",
                    SqlValue::NullableText(route.summary.departure_time.clone()),
                ),
                (
                    "arrival_time",
                    SqlValue::NullableText(route.summary.arrival_time.clone()),
                ),
                (
                    "scenario_id",
                    SqlValue::NullableText(route.summary.scenario_id.clone()),
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
            ];
            route_fields.extend(route_component_columns.iter().map(|column| {
                (
                    column.column_name.as_str(),
                    SqlValue::NullableReal(
                        route
                            .summary
                            .components
                            .get(&column.component_name)
                            .copied(),
                    ),
                )
            }));
            gpkg.insert_feature_3d("routes", &route_fields, &coords)?;

            if let Some(segments) = route.segments.as_ref() {
                for (segment_index, segment) in segments.iter().enumerate() {
                    let mut segment_fields = vec![
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
                        ("waiting_time_s", SqlValue::Real(segment.waiting_time_s)),
                        (
                            "entry_time",
                            SqlValue::NullableText(segment.entry_time.clone()),
                        ),
                        (
                            "exit_time",
                            SqlValue::NullableText(segment.exit_time.clone()),
                        ),
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
                    ];
                    segment_fields.extend(segment_component_columns.iter().map(|column| {
                        (
                            column.column_name.as_str(),
                            SqlValue::NullableReal(
                                segment.components.get(&column.component_name).copied(),
                            ),
                        )
                    }));
                    gpkg.insert_row("route_segments", &segment_fields)?;
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

#[cfg(test)]
mod tests;
