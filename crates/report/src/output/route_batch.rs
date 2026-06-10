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

#[cfg(test)]
mod tests {
    use crate::output::{temp_path, write_route_batch_result};

    use netweevil_core::{RoadClass, SurfaceClass};
    use netweevil_profile::ReturnConfig;
    use netweevil_query::{
        AnalysisOutcome, BatchItemStatus, LabeledPoint, MetricBreakdown, RouteBatchDocument,
        RouteBatchEntry, RouteBatchItemResult, RouteBatchResult, RouteBreakdowns, RouteRequest,
        RouteResult, RouteSegment, RouteSummary, RouteViolation, RouteViolationType, SnappedPoint,
    };
    use rusqlite::Connection;
    use std::collections::BTreeMap;
    use std::fs;

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
}
