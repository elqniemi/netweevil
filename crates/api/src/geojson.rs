use axum::http::HeaderValue;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use netweevil_query::{MatrixResult, OdResult, RouteResult, ServiceAreaResult};
use netweevil_transit::TransitServiceAreaResult;
use serde_json::{Value, json};

use crate::dto::{ExecutionContext, ResponseFormatQuery, TransitExecutionContext};
use crate::error::ApiError;
use crate::state::ServiceRuntime;

pub(crate) fn wants_geojson(query: &ResponseFormatQuery) -> bool {
    query
        .format
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("geojson"))
}

pub(crate) fn geojson_response(value: Value) -> Result<Response, ApiError> {
    let body = serde_json::to_vec(&value)
        .map_err(|error| ApiError::bad_request(format!("serializing GeoJSON response: {error}")))?;
    Ok((
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/geo+json"),
        )],
        body,
    )
        .into_response())
}

pub(crate) fn route_result_geojson(
    service: &ServiceRuntime,
    execution: &ExecutionContext,
    result: &RouteResult,
) -> Value {
    let coordinates = if let Some(geometry) = result.geometry.as_ref() {
        geometry.clone()
    } else {
        result
            .node_path
            .iter()
            .filter_map(|node_id| service.topology.nodes.get(*node_id as usize))
            .map(|node| [node.lon, node.lat])
            .collect()
    };
    let mut features = vec![json!({
        "type": "Feature",
        "geometry": {
            "type": "LineString",
            "coordinates": coordinates,
        },
        "properties": {
            "dataset_id": execution.dataset_id,
            "profile_id": execution.profile_id,
            "profile_hash": execution.profile_hash,
            "route_id": result.route_id,
            "route_rank": 0,
            "alternative_index": Value::Null,
            "outcome": result.outcome,
            "fallback_used": result.fallback_used,
            "origin_id": result.origin.point_id,
            "destination_id": result.destination.point_id,
            "origin_component_id": result.origin.component_id,
            "destination_component_id": result.destination.component_id,
            "origin_snap_distance_m": result.origin.snap_distance_m,
            "destination_snap_distance_m": result.destination.snap_distance_m,
            "origin_hop_distance_m": result.origin_hop_distance_m,
            "destination_hop_distance_m": result.destination_hop_distance_m,
            "total_distance_m": result.summary.total_distance_m,
            "total_travel_time_s": result.summary.total_travel_time_s,
            "total_generalized_cost": result.summary.total_generalized_cost,
            "illegal_movement_penalty_s": result.summary.illegal_movement_penalty_s,
            "illegal_movement_penalty_cost": result.summary.illegal_movement_penalty_cost,
            "violation_count": result.summary.violation_count,
            "violation_types": result.summary.violation_types,
            "violations": result.violations,
            "segment_count": result.summary.segment_count,
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
                    "dataset_id": execution.dataset_id,
                    "profile_id": execution.profile_id,
                    "profile_hash": execution.profile_hash,
                    "route_id": result.route_id,
                    "route_rank": alternative.rank,
                    "alternative_index": alternative.alternative_index,
                    "origin_id": result.origin.point_id,
                    "destination_id": result.destination.point_id,
                    "total_distance_m": alternative.summary.total_distance_m,
                    "total_travel_time_s": alternative.summary.total_travel_time_s,
                    "total_generalized_cost": alternative.summary.total_generalized_cost,
                    "violation_count": alternative.summary.violation_count,
                    "violation_types": alternative.summary.violation_types,
                    "violations": alternative.violations,
                    "segment_count": alternative.summary.segment_count,
                    "warnings": alternative.warnings,
                }
            }));
        }
    }
    json!({
        "type": "FeatureCollection",
        "features": features
    })
}

pub(crate) fn od_result_geojson(execution: &ExecutionContext, result: &OdResult) -> Value {
    let mut features = Vec::new();
    for pair in &result.pairs {
        features.push(json!({
            "type": "Feature",
            "geometry": pair.geometry.as_ref().map(|geometry| json!({
                "type": "LineString",
                "coordinates": geometry,
            })).unwrap_or(Value::Null),
            "properties": {
                "dataset_id": execution.dataset_id,
                "profile_id": execution.profile_id,
                "profile_hash": execution.profile_hash,
                "pair_id": pair.pair_id,
                "origin_id": pair.origin_id,
                "destination_id": pair.destination_id,
                "status": pair.status,
                "outcome": pair.outcome,
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
            }
        }));
        for alternative in &pair.alternatives {
            features.push(json!({
                "type": "Feature",
                "geometry": alternative.geometry.as_ref().map(|geometry| json!({
                    "type": "LineString",
                    "coordinates": geometry,
                })).unwrap_or(Value::Null),
                "properties": {
                    "dataset_id": execution.dataset_id,
                    "profile_id": execution.profile_id,
                    "profile_hash": execution.profile_hash,
                    "pair_id": pair.pair_id,
                    "origin_id": pair.origin_id,
                    "destination_id": pair.destination_id,
                    "route_rank": alternative.rank,
                    "alternative_index": alternative.alternative_index,
                    "total_distance_m": alternative.total_distance_m,
                    "total_travel_time_s": alternative.total_travel_time_s,
                    "total_generalized_cost": alternative.total_generalized_cost,
                    "violation_count": alternative.violation_count,
                    "violation_types": alternative.violation_types,
                }
            }));
        }
    }
    json!({
        "type": "FeatureCollection",
        "features": features,
        "metadata": {
            "dataset_id": execution.dataset_id,
            "profile_id": execution.profile_id,
            "profile_hash": execution.profile_hash,
            "pair_count": result.pair_count,
            "succeeded_count": result.succeeded_count,
            "failed_count": result.failed_count,
            "warnings": result.warnings,
        }
    })
}

pub(crate) fn matrix_result_geojson(execution: &ExecutionContext, result: &MatrixResult) -> Value {
    let mut features = Vec::new();
    for cell in &result.cells {
        features.push(json!({
            "type": "Feature",
            "geometry": cell.geometry.as_ref().map(|geometry| json!({
                "type": "LineString",
                "coordinates": geometry,
            })).unwrap_or(Value::Null),
            "properties": {
                "dataset_id": execution.dataset_id,
                "profile_id": execution.profile_id,
                "profile_hash": execution.profile_hash,
                "origin_id": cell.origin_id,
                "destination_id": cell.destination_id,
                "status": cell.status,
                "outcome": cell.outcome,
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
            }
        }));
        for alternative in &cell.alternatives {
            features.push(json!({
                "type": "Feature",
                "geometry": alternative.geometry.as_ref().map(|geometry| json!({
                    "type": "LineString",
                    "coordinates": geometry,
                })).unwrap_or(Value::Null),
                "properties": {
                    "dataset_id": execution.dataset_id,
                    "profile_id": execution.profile_id,
                    "profile_hash": execution.profile_hash,
                    "origin_id": cell.origin_id,
                    "destination_id": cell.destination_id,
                    "route_rank": alternative.rank,
                    "alternative_index": alternative.alternative_index,
                    "total_distance_m": alternative.total_distance_m,
                    "total_travel_time_s": alternative.total_travel_time_s,
                    "total_generalized_cost": alternative.total_generalized_cost,
                    "violation_count": alternative.violation_count,
                    "violation_types": alternative.violation_types,
                }
            }));
        }
    }
    json!({
        "type": "FeatureCollection",
        "features": features,
        "metadata": {
            "dataset_id": execution.dataset_id,
            "profile_id": execution.profile_id,
            "profile_hash": execution.profile_hash,
            "origin_count": result.origin_count,
            "destination_count": result.destination_count,
            "cell_count": result.cell_count,
            "succeeded_count": result.succeeded_count,
            "failed_count": result.failed_count,
            "warnings": result.warnings,
        }
    })
}

pub(crate) fn service_area_result_geojson(
    execution: &ExecutionContext,
    result: &ServiceAreaResult,
) -> Value {
    let features = result
        .features
        .iter()
        .map(|feature| {
            json!({
                "type": "Feature",
                "geometry": feature.geometry.clone().unwrap_or(Value::Null),
                "properties": {
                    "dataset_id": execution.dataset_id,
                    "profile_id": execution.profile_id,
                    "profile_hash": execution.profile_hash,
                    "analysis_id": result.analysis_id,
                    "origin_id": feature.origin_id,
                    "threshold_id": feature.threshold_id,
                    "band_start_limit": feature.band_start_limit,
                    "threshold_limit": feature.threshold_limit,
                    "threshold_metric": feature.threshold_metric,
                    "geometry_type": feature.geometry_type,
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
    json!({
        "type": "FeatureCollection",
        "features": features,
        "metadata": {
            "dataset_id": execution.dataset_id,
            "profile_id": execution.profile_id,
            "profile_hash": execution.profile_hash,
            "analysis_id": result.analysis_id,
            "outcome": result.outcome,
            "origin_count": result.origin_count,
            "processed_origin_count": result.processed_origin_count,
            "skipped_origin_count": result.skipped_origin_count,
            "fallback_origin_count": result.fallback_origin_count,
            "threshold_count": result.threshold_count,
            "segment_count": result.segments.len(),
            "warnings": result.warnings,
        }
    })
}

pub(crate) fn transit_service_area_result_geojson(
    execution: &TransitExecutionContext,
    result: &TransitServiceAreaResult,
) -> Value {
    let mut features = Vec::new();
    for stop in &result.stops {
        features.push(json!({
            "type": "Feature",
            "geometry": {
                "type": "Point",
                "coordinates": [stop.lon, stop.lat],
            },
            "properties": {
                "feed_id": execution.feed_id,
                "analysis_id": result.analysis_id,
                "geometry_type": "stop",
                "origin_id": stop.origin_id,
                "stop_id": stop.stop_id,
                "stop_name": stop.stop_name,
                "arrival_s": stop.arrival_s,
                "travel_time_s": stop.travel_time_s,
                "boarding_count": stop.boarding_count,
            }
        }));
    }
    for segment in &result.stop_segments {
        features.push(json!({
            "type": "Feature",
            "geometry": if segment.geometry.is_empty() {
                Value::Null
            } else {
                json!({
                    "type": "LineString",
                    "coordinates": segment.geometry,
                })
            },
            "properties": {
                "feed_id": execution.feed_id,
                "analysis_id": result.analysis_id,
                "geometry_type": "stop_segment",
                "origin_id": segment.origin_id,
                "from_stop_id": segment.from_stop_id,
                "to_stop_id": segment.to_stop_id,
                "from_stop_name": segment.from_stop_name,
                "to_stop_name": segment.to_stop_name,
                "departure_s": segment.departure_s,
                "arrival_s": segment.arrival_s,
                "duration_s": segment.duration_s,
                "travel_time_s": segment.travel_time_s,
                "boarding_count": segment.boarding_count,
                "mode": segment.mode,
                "route_id": segment.route_id,
                "route_short_name": segment.route_short_name,
                "trip_id": segment.trip_id,
            }
        }));
    }
    json!({
        "type": "FeatureCollection",
        "features": features,
        "metadata": {
            "feed_id": execution.feed_id,
            "service_start_date": execution.service_start_date,
            "service_days": execution.service_days,
            "analysis_id": result.analysis_id,
            "outcome": result.outcome,
            "origin_count": result.origin_count,
            "processed_origin_count": result.processed_origin_count,
            "skipped_origin_count": result.skipped_origin_count,
            "max_travel_time_s": result.max_travel_time_s,
            "stop_count": result.stops.len(),
            "stop_segment_count": result.stop_segments.len(),
            "diagnostics": result.diagnostics,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{matrix_result_geojson, od_result_geojson};
    use crate::dto::ExecutionContext;
    use netweevil_query::{
        AnalysisOutcome, BatchItemStatus, MatrixCellResult, MatrixResult, OdPairResult, OdResult,
    };

    #[test]
    fn builds_geojson_for_od_results() {
        let execution = ExecutionContext {
            dataset_id: "dataset_a".to_string(),
            profile_id: "car_research_v1".to_string(),
            profile_hash: "abc123".to_string(),
            route_engine: "route".to_string(),
            batch_engine: "batch".to_string(),
            acceleration: "spatial".to_string(),
        };
        let result = OdResult {
            pair_count: 1,
            succeeded_count: 1,
            failed_count: 0,
            ignored_count: 0,
            pairs: vec![OdPairResult {
                pair_id: "pair_1".to_string(),
                origin_id: "a".to_string(),
                destination_id: "b".to_string(),
                status: BatchItemStatus::Succeeded,
                outcome: AnalysisOutcome::Legal,
                fallback_used: false,
                origin_component_id: None,
                destination_component_id: None,
                origin_hop_distance_m: None,
                destination_hop_distance_m: None,
                origin_snap_distance_m: Some(1.0),
                destination_snap_distance_m: Some(2.0),
                total_distance_m: Some(100),
                total_travel_time_s: Some(12.5),
                total_generalized_cost: Some(13.5),
                illegal_movement_penalty_s: Some(0.0),
                illegal_movement_penalty_cost: Some(0.0),
                violation_count: 0,
                violation_types: vec![],
                geometry: Some(vec![[2.0, 48.0], [2.1, 48.1]]),
                diagnostics: vec![],
                error: None,
                alternatives: vec![],
            }],
            diagnostics: vec![],
            warnings: vec!["ok".to_string()],
        };

        let geojson = od_result_geojson(&execution, &result);
        assert_eq!(geojson["type"], "FeatureCollection");
        assert_eq!(geojson["features"][0]["geometry"]["type"], "LineString");
        assert_eq!(geojson["features"][0]["properties"]["pair_id"], "pair_1");
    }

    #[test]
    fn preserves_null_geometry_in_matrix_geojson() {
        let execution = ExecutionContext {
            dataset_id: "dataset_a".to_string(),
            profile_id: "car_research_v1".to_string(),
            profile_hash: "abc123".to_string(),
            route_engine: "route".to_string(),
            batch_engine: "batch".to_string(),
            acceleration: "spatial".to_string(),
        };
        let result = MatrixResult {
            origin_count: 1,
            destination_count: 1,
            cell_count: 1,
            succeeded_count: 0,
            failed_count: 1,
            ignored_count: 0,
            cells: vec![MatrixCellResult {
                origin_id: "a".to_string(),
                destination_id: "b".to_string(),
                status: BatchItemStatus::Failed,
                outcome: AnalysisOutcome::Unreachable,
                fallback_used: false,
                origin_component_id: None,
                destination_component_id: None,
                origin_hop_distance_m: None,
                destination_hop_distance_m: None,
                origin_snap_distance_m: None,
                destination_snap_distance_m: None,
                total_distance_m: None,
                total_travel_time_s: None,
                total_generalized_cost: None,
                illegal_movement_penalty_s: None,
                illegal_movement_penalty_cost: None,
                violation_count: 0,
                violation_types: vec![],
                geometry: None,
                diagnostics: vec![],
                error: Some("no route".to_string()),
                alternatives: vec![],
            }],
            diagnostics: vec![],
            warnings: vec![],
        };

        let geojson = matrix_result_geojson(&execution, &result);
        assert!(geojson["features"][0]["geometry"].is_null());
        assert_eq!(geojson["features"][0]["properties"]["status"], "failed");
    }
}
