use axum::http::HeaderValue;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use netweevil_query::{
    BetweennessResult, MatrixResult, OdResult, RouteResult, ScenarioAnalysisSnapshot,
    ScenarioBatchResult, ServiceAreaResult, ServiceAreaSequenceResult,
};
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
            .map(|node| {
                [
                    node.lon,
                    node.lat,
                    if node.z.is_finite() { node.z } else { 0.0 },
                ]
            })
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
            "components": result.summary.components,
            "departure_time": result.summary.departure_time,
            "arrival_time": result.summary.arrival_time,
            "scenario_id": result.summary.scenario_id,
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
                    "components": alternative.summary.components,
                    "departure_time": alternative.summary.departure_time,
                    "arrival_time": alternative.summary.arrival_time,
                    "scenario_id": alternative.summary.scenario_id,
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

pub(crate) fn betweenness_result_geojson(
    execution: &ExecutionContext,
    result: &BetweennessResult,
) -> Value {
    json!({
        "type": "FeatureCollection",
        "features": result.edges.iter().map(|edge| json!({
            "type": "Feature",
            "id": edge.edge_id,
            "geometry": {"type": "LineString", "coordinates": edge.geometry},
            "properties": {
                "dataset_id": execution.dataset_id,
                "profile_id": execution.profile_id,
                "profile_hash": execution.profile_hash,
                "analysis_id": result.analysis_id,
                "edge_id": edge.edge_id,
                "source_way_id": edge.source_way_id,
                "from_node_id": edge.from_node_id,
                "to_node_id": edge.to_node_id,
                "score": edge.score,
                "normalized_score": edge.normalized_score,
                "routed_pair_count": edge.routed_pair_count,
            }
        })).collect::<Vec<_>>(),
        "metadata": {
            "analysis_id": result.analysis_id,
            "requested_pair_count": result.requested_pair_count,
            "routed_pair_count": result.routed_pair_count,
            "unreachable_pair_count": result.unreachable_pair_count,
            "routed_demand": result.routed_demand,
            "time_dependent": result.time_dependent,
            "departure_time": result.departure_time,
            "arrive_by": result.arrive_by,
            "scenario_id": result.scenario_id,
            "warnings": result.warnings,
        }
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
                "components": pair.components,
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
                    "components": alternative.components,
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
            "departure_time": result.departure_time,
            "arrive_by": result.arrive_by,
            "scenario_id": result.scenario_id,
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
                "components": cell.components,
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
                    "components": alternative.components,
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
            "departure_time": result.departure_time,
            "arrive_by": result.arrive_by,
            "scenario_id": result.scenario_id,
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
                    "departure_time": result.departure_time,
                    "scenario_id": result.scenario_id,
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
                    "start_components": feature.start_components,
                    "end_components": feature.end_components,
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
            "departure_time": result.departure_time,
            "scenario_id": result.scenario_id,
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

pub(crate) fn service_area_sequence_result_geojson(
    execution: &ExecutionContext,
    result: &ServiceAreaSequenceResult,
) -> Value {
    let mut features = Vec::new();
    for frame in &result.frames {
        let mut collection = service_area_result_geojson(execution, &frame.result);
        if let Some(frame_features) = collection.get_mut("features").and_then(Value::as_array_mut) {
            for mut feature in frame_features.drain(..) {
                if let Some(properties) =
                    feature.get_mut("properties").and_then(Value::as_object_mut)
                {
                    properties.insert(
                        "sequence_id".to_string(),
                        Value::String(result.sequence_id.clone()),
                    );
                    properties.insert("frame_index".to_string(), Value::from(frame.frame_index));
                    properties.insert(
                        "departure_time".to_string(),
                        Value::String(frame.departure_time.clone()),
                    );
                }
                features.push(feature);
            }
        }
    }
    json!({
        "type": "FeatureCollection",
        "features": features,
        "metadata": {
            "dataset_id": execution.dataset_id,
            "profile_id": execution.profile_id,
            "profile_hash": execution.profile_hash,
            "sequence_id": result.sequence_id,
            "frame_count": result.frame_count,
            "departure_times": result.frames.iter().map(|frame| &frame.departure_time).collect::<Vec<_>>(),
        }
    })
}

pub(crate) fn scenario_batch_result_geojson(
    service: &ServiceRuntime,
    execution: &ExecutionContext,
    result: &ScenarioBatchResult,
) -> Value {
    let mut features = Vec::new();
    append_scenario_snapshot_features(&mut features, service, execution, None, &result.baseline);
    for scenario in &result.scenarios {
        append_scenario_snapshot_features(
            &mut features,
            service,
            execution,
            Some(&scenario.scenario_id),
            &scenario.analyses,
        );
    }
    json!({
        "type": "FeatureCollection",
        "features": features,
        "metadata": {
            "dataset_id": execution.dataset_id,
            "profile_id": execution.profile_id,
            "profile_hash": execution.profile_hash,
            "batch_id": result.batch_id,
            "scenarios": result.scenarios.iter().map(|scenario| json!({
                "scenario_id": scenario.scenario_id,
                "selector": scenario.selector,
                "closed_source_feature_ids": scenario.closed_source_feature_ids,
                "diff": scenario.diff,
            })).collect::<Vec<_>>(),
        }
    })
}

fn append_scenario_snapshot_features(
    features: &mut Vec<Value>,
    service: &ServiceRuntime,
    execution: &ExecutionContext,
    scenario_case_id: Option<&str>,
    snapshot: &ScenarioAnalysisSnapshot,
) {
    for route in &snapshot.routes {
        if let Some(result) = route.result.as_ref() {
            let mut collection = route_result_geojson(service, execution, result);
            if let Some(route_features) =
                collection.get_mut("features").and_then(Value::as_array_mut)
            {
                for mut feature in route_features.drain(..) {
                    tag_scenario_feature(&mut feature, "route", &route.route_id, scenario_case_id);
                    features.push(feature);
                }
            }
        } else {
            features.push(scenario_error_feature(
                "route",
                &route.route_id,
                scenario_case_id,
                route.error.as_deref(),
            ));
        }
    }
    for service_area in &snapshot.service_areas {
        if let Some(result) = service_area.result.as_ref() {
            let mut collection = service_area_result_geojson(execution, result);
            if let Some(area_features) =
                collection.get_mut("features").and_then(Value::as_array_mut)
            {
                for mut feature in area_features.drain(..) {
                    tag_scenario_feature(
                        &mut feature,
                        "service_area",
                        &service_area.analysis_id,
                        scenario_case_id,
                    );
                    features.push(feature);
                }
            }
        } else {
            features.push(scenario_error_feature(
                "service_area",
                &service_area.analysis_id,
                scenario_case_id,
                service_area.error.as_deref(),
            ));
        }
    }
    for accessibility in &snapshot.accessibility {
        if let Some(result) = accessibility.result.as_ref() {
            features.extend(result.rows.iter().map(|row| {
                json!({
                    "type": "Feature",
                    "geometry": Value::Null,
                    "properties": {
                        "dataset_id": execution.dataset_id,
                        "profile_id": execution.profile_id,
                        "profile_hash": execution.profile_hash,
                        "analysis_kind": "accessibility",
                        "analysis_id": accessibility.analysis_id,
                        "scenario_batch_case_id": scenario_case_id,
                        "origin_id": row.origin_id,
                        "category_id": row.category_id,
                        "status": row.status,
                        "outcome": row.outcome,
                        "nearest_destination_id": row.nearest_destination_id,
                        "nearest_travel_time_s": row.nearest_travel_time_s,
                        "counts_within_threshold_s": row.counts_within_threshold_s,
                        "error": row.error,
                    }
                })
            }));
        } else {
            features.push(scenario_error_feature(
                "accessibility",
                &accessibility.analysis_id,
                scenario_case_id,
                accessibility.error.as_deref(),
            ));
        }
    }
    for od in &snapshot.od {
        if let Some(result) = od.result.as_ref() {
            append_tagged_collection(
                features,
                od_result_geojson(execution, result),
                "od",
                &od.analysis_id,
                scenario_case_id,
            );
        } else {
            features.push(scenario_error_feature(
                "od",
                &od.analysis_id,
                scenario_case_id,
                od.error.as_deref(),
            ));
        }
    }
    for matrix in &snapshot.matrices {
        if let Some(result) = matrix.result.as_ref() {
            append_tagged_collection(
                features,
                matrix_result_geojson(execution, result),
                "matrix",
                &matrix.analysis_id,
                scenario_case_id,
            );
        } else {
            features.push(scenario_error_feature(
                "matrix",
                &matrix.analysis_id,
                scenario_case_id,
                matrix.error.as_deref(),
            ));
        }
    }
    for betweenness in &snapshot.betweenness {
        if let Some(result) = betweenness.result.as_ref() {
            append_tagged_collection(
                features,
                betweenness_result_geojson(execution, result),
                "betweenness",
                &betweenness.analysis_id,
                scenario_case_id,
            );
        } else {
            features.push(scenario_error_feature(
                "betweenness",
                &betweenness.analysis_id,
                scenario_case_id,
                betweenness.error.as_deref(),
            ));
        }
    }
}

fn append_tagged_collection(
    target: &mut Vec<Value>,
    mut collection: Value,
    analysis_kind: &str,
    analysis_id: &str,
    scenario_case_id: Option<&str>,
) {
    let Some(features) = collection.get_mut("features").and_then(Value::as_array_mut) else {
        return;
    };
    for mut feature in features.drain(..) {
        tag_scenario_feature(&mut feature, analysis_kind, analysis_id, scenario_case_id);
        target.push(feature);
    }
}

fn tag_scenario_feature(
    feature: &mut Value,
    analysis_kind: &str,
    analysis_id: &str,
    scenario_case_id: Option<&str>,
) {
    let Some(properties) = feature.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    properties.insert("analysis_kind".to_string(), json!(analysis_kind));
    properties.insert("analysis_id".to_string(), json!(analysis_id));
    properties.insert(
        "scenario_batch_case_id".to_string(),
        scenario_case_id.map_or(Value::Null, |id| json!(id)),
    );
}

fn scenario_error_feature(
    analysis_kind: &str,
    analysis_id: &str,
    scenario_case_id: Option<&str>,
    error: Option<&str>,
) -> Value {
    json!({
        "type": "Feature",
        "geometry": Value::Null,
        "properties": {
            "analysis_kind": analysis_kind,
            "analysis_id": analysis_id,
            "scenario_batch_case_id": scenario_case_id,
            "error": error,
        }
    })
}

pub(crate) fn transit_service_area_result_geojson(
    execution: &TransitExecutionContext,
    result: &TransitServiceAreaResult,
) -> Value {
    let mut features = Vec::new();
    for feature in &result.features {
        let mut properties = serde_json::to_value(feature).expect("serializable isochrone feature");
        properties.as_object_mut().unwrap().remove("geometry");
        properties["feed_id"] = json!(execution.feed_id);
        properties["analysis_id"] = json!(result.analysis_id);
        properties["agency_timezone"] = json!(result.time_context.agency_timezone);
        properties["time_origin_unix_s"] = json!(result.time_context.time_origin_unix_s);
        features.push(
            json!({"type": "Feature", "geometry": feature.geometry, "properties": properties}),
        );
    }
    for stop in &result.stops {
        features.push(json!({
            "type": "Feature",
            "geometry": {
                "type": "Point",
                "coordinates": [stop.lon, stop.lat],
            },
            "properties": {
                "feed_id": execution.feed_id,
                "agency_timezone": result.time_context.agency_timezone,
                "time_origin_unix_s": result.time_context.time_origin_unix_s,
                "analysis_id": result.analysis_id,
                "geometry_type": "stop",
                "origin_id": stop.origin_id,
                "stop_id": stop.stop_id,
                "stop_name": stop.stop_name,
                "arrival_s": stop.arrival_s,
                "travel_time_s": stop.travel_time_s,
                "boarding_count": stop.boarding_count,
                "access_mode": stop.access_mode,
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
                "agency_timezone": result.time_context.agency_timezone,
                "time_origin_unix_s": result.time_context.time_origin_unix_s,
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
                "agency_timezone": result.time_context.agency_timezone,
                "time_origin_unix_s": result.time_context.time_origin_unix_s,
            "service_start_date": execution.service_start_date,
            "service_days": execution.service_days,
            "transfer_profile_id": execution.transfer_profile_id,
            "analysis_id": result.analysis_id,
            "outcome": result.outcome,
            "origin_count": result.origin_count,
            "processed_origin_count": result.processed_origin_count,
            "skipped_origin_count": result.skipped_origin_count,
            "max_travel_time_s": result.max_travel_time_s,
            "stop_count": result.stops.len(),
            "catchment_mode": result.catchment_mode,
            "isochrone_feature_count": result.features.len(),
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
        AnalysisOutcome, BatchAlternativeResult, BatchItemStatus, MatrixCellResult, MatrixResult,
        OdPairResult, OdResult,
    };
    use std::collections::BTreeMap;

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
            departure_time: Some("2026-07-10T09:55:00+08:00".to_string()),
            arrive_by: None,
            scenario_id: Some("central_flip".to_string()),
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
                components: BTreeMap::from([("sun_time".to_string(), 4.5)]),
                illegal_movement_penalty_s: Some(0.0),
                illegal_movement_penalty_cost: Some(0.0),
                violation_count: 0,
                violation_types: vec![],
                geometry: Some(vec![[2.0, 48.0, 10.0], [2.1, 48.1, 12.0]]),
                diagnostics: vec![],
                error: None,
                alternatives: vec![BatchAlternativeResult {
                    alternative_index: 0,
                    rank: 1,
                    total_distance_m: Some(110),
                    total_travel_time_s: Some(13.0),
                    total_generalized_cost: Some(14.0),
                    components: BTreeMap::from([("sun_time".to_string(), 2.0)]),
                    geometry: Some(vec![[2.0, 48.0, 10.0], [2.2, 48.2, 13.0]]),
                    violation_count: 0,
                    violation_types: vec![],
                }],
            }],
            diagnostics: vec![],
            warnings: vec!["ok".to_string()],
        };

        let geojson = od_result_geojson(&execution, &result);
        assert_eq!(geojson["type"], "FeatureCollection");
        assert_eq!(geojson["features"][0]["geometry"]["type"], "LineString");
        assert_eq!(geojson["features"][0]["properties"]["pair_id"], "pair_1");
        assert_eq!(
            geojson["features"][0]["properties"]["components"]["sun_time"],
            4.5
        );
        assert_eq!(
            geojson["features"][1]["properties"]["components"]["sun_time"],
            2.0
        );
        assert_eq!(geojson["metadata"]["scenario_id"], "central_flip");
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
            departure_time: None,
            arrive_by: None,
            scenario_id: None,
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
                components: BTreeMap::from([("slope".to_string(), 1.25)]),
                illegal_movement_penalty_s: None,
                illegal_movement_penalty_cost: None,
                violation_count: 0,
                violation_types: vec![],
                geometry: None,
                diagnostics: vec![],
                error: Some("no route".to_string()),
                alternatives: vec![BatchAlternativeResult {
                    alternative_index: 0,
                    rank: 1,
                    total_distance_m: None,
                    total_travel_time_s: None,
                    total_generalized_cost: None,
                    components: BTreeMap::from([("slope".to_string(), 0.75)]),
                    geometry: None,
                    violation_count: 0,
                    violation_types: vec![],
                }],
            }],
            diagnostics: vec![],
            warnings: vec![],
        };

        let geojson = matrix_result_geojson(&execution, &result);
        assert!(geojson["features"][0]["geometry"].is_null());
        assert_eq!(geojson["features"][0]["properties"]["status"], "failed");
        assert_eq!(
            geojson["features"][0]["properties"]["components"]["slope"],
            1.25
        );
        assert_eq!(
            geojson["features"][1]["properties"]["components"]["slope"],
            0.75
        );
    }

    #[test]
    fn transit_geojson_features_keep_the_time_origin_when_exported_alone() {
        let time_origin_unix_s = 1_792_792_800_i64;
        let context = crate::dto::TransitExecutionContext {
            feed_id: "dst".to_string(),
            service_start_date: "2026-10-24".to_string(),
            service_days: 1,
            agency_timezone: "Europe/Amsterdam".to_string(),
            time_origin_unix_s,
            route_engine: "scheduled".to_string(),
            walking_geometry: "straight_line".to_string(),
            transfer_profile_id: None,
            pedestrian_profile_id: None,
            access_profile_id: None,
            egress_profile_id: None,
        };
        let result: netweevil_transit::TransitServiceAreaResult = serde_json::from_value(serde_json::json!({
            "analysis_id": "dst-area", "outcome": "scheduled",
            "time_context": {"agency_timezone": "Europe/Amsterdam", "time_origin_unix_s": time_origin_unix_s},
            "origin_count": 1, "processed_origin_count": 1, "skipped_origin_count": 0,
            "max_travel_time_s": 7200,
            "stops": [{"origin_id": "A", "stop_id": "C", "stop_name": "C", "lon": 6.02, "lat": 53.0,
                "arrival_s": 98100, "travel_time_s": 3600, "boarding_count": 1}],
            "stop_segments": [{"origin_id": "A", "from_stop_id": "A", "to_stop_id": "C", "from_stop_name": "A", "to_stop_name": "C",
                "departure_s": 94500, "arrival_s": 98100, "duration_s": 3600, "travel_time_s": 3600, "boarding_count": 1,
                "geometry": [[6.0, 53.0], [6.02, 53.0]]}]
        })).unwrap();
        let geojson = super::transit_service_area_result_geojson(&context, &result);
        assert_eq!(
            geojson["metadata"]["time_origin_unix_s"],
            time_origin_unix_s
        );
        assert_eq!(geojson["metadata"]["agency_timezone"], "Europe/Amsterdam");
        let features = geojson["features"].as_array().unwrap();
        assert_eq!(features.len(), 2);
        for feature in features {
            // A consumer can recover an instant from a single exported row.
            let properties = &feature["properties"];
            assert_eq!(properties["agency_timezone"], "Europe/Amsterdam");
            assert_eq!(properties["time_origin_unix_s"], time_origin_unix_s);
            assert_eq!(
                properties["time_origin_unix_s"].as_i64().unwrap()
                    + properties["arrival_s"].as_i64().unwrap(),
                1_792_890_900
            );
        }
    }
}
