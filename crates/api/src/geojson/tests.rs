use super::{matrix_result_geojson, od_result_geojson, transit_service_area_result_geojson};
use crate::dto::{ExecutionContext, TransitExecutionContext};
use serde_json::json;

fn execution() -> ExecutionContext {
    ExecutionContext {
        dataset_id: "dataset_a".to_string(),
        profile_id: "car_research_v1".to_string(),
        profile_hash: "abc123".to_string(),
        route_engine: "route".to_string(),
        batch_engine: "batch".to_string(),
        acceleration: "spatial".to_string(),
    }
}

#[test]
fn builds_geojson_for_od_results() {
    let result = serde_json::from_value(json!({
        "departure_time": "2026-07-10T09:55:00+08:00", "scenario_id": "central_flip",
        "pair_count": 1, "succeeded_count": 1, "failed_count": 0,
        "pairs": [{
            "pair_id": "pair_1", "origin_id": "a", "destination_id": "b", "status": "succeeded",
            "total_distance_m": 100, "total_travel_time_s": 12.5,
            "components": {"sun_time": 4.5},
            "geometry": [[2.0, 48.0, 10.0], [2.1, 48.1, 12.0]],
            "alternatives": [{
                "alternative_index": 0, "rank": 1, "total_distance_m": 110,
                "components": {"sun_time": 2.0},
                "geometry": [[2.0, 48.0, 10.0], [2.2, 48.2, 13.0]]
            }]
        }]
    }))
    .unwrap();

    let geojson = od_result_geojson(&execution(), &result);
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
    let result = serde_json::from_value(json!({
        "origin_count": 1, "destination_count": 1, "cell_count": 1,
        "succeeded_count": 0, "failed_count": 1,
        "cells": [{
            "origin_id": "a", "destination_id": "b", "status": "failed",
            "outcome": "unreachable", "error": "no route",
            "components": {"slope": 1.25},
            "alternatives": [{"alternative_index": 0, "rank": 1, "components": {"slope": 0.75}}]
        }]
    }))
    .unwrap();

    let geojson = matrix_result_geojson(&execution(), &result);
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
    let context = TransitExecutionContext {
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
    let result = serde_json::from_value(json!({
        "analysis_id": "dst-area", "outcome": "scheduled",
        "time_context": {"agency_timezone": "Europe/Amsterdam", "time_origin_unix_s": time_origin_unix_s},
        "origin_count": 1, "processed_origin_count": 1, "skipped_origin_count": 0,
        "max_travel_time_s": 7200,
        "stops": [{"origin_id": "A", "stop_id": "C", "stop_name": "C", "lon": 6.02, "lat": 53.0,
            "arrival_s": 98100, "travel_time_s": 3600, "boarding_count": 1}],
        "stop_segments": [{"origin_id": "A", "from_stop_id": "A", "to_stop_id": "C", "from_stop_name": "A", "to_stop_name": "C",
            "departure_s": 94500, "arrival_s": 98100, "duration_s": 3600, "travel_time_s": 3600, "boarding_count": 1,
            "geometry": [[6.0, 53.0], [6.02, 53.0]]}]
    }))
    .unwrap();
    let geojson = transit_service_area_result_geojson(&context, &result);
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
