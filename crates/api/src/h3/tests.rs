use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::json;

use super::*;
use crate::traffic::tests::{body, state};

#[tokio::test]
async fn network_summary_counts_nodes_and_edges_per_cell() {
    let state = state();
    let query = |resolution, format: Option<&str>| H3NetworkQuery {
        resolution,
        format: format.map(str::to_string),
    };
    let summary = body(
        h3_network_handler(State(state.clone()), Query(query(6, None)))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(summary["resolution"], 6);
    let cells = summary["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0]["node_count"], 3);
    assert_eq!(cells[0]["edge_count"], 3);
    assert_eq!(cells[0]["edge_length_m"], 800.0);

    let response = h3_network_handler(State(state.clone()), Query(query(12, Some("geojson"))))
        .await
        .unwrap();
    assert_eq!(response.headers()["content-type"], "application/geo+json");
    let collection = body(response).await;
    let features = collection["features"].as_array().unwrap();
    assert!(features.len() >= 3);
    assert_eq!(features[0]["geometry"]["type"], "Polygon");
    assert_eq!(features[0]["properties"]["resolution"], 12);
    assert!(features[0]["properties"]["cell"].is_string());

    let error = h3_network_handler(State(state), Query(query(16, None)))
        .await
        .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn aggregate_counts_points_on_each_resolution() {
    let request = || {
        serde_json::from_value::<H3AggregateRequest>(json!({
            "points": [
                {"lon": 6.5665, "lat": 53.2194, "weight": 120},
                {"lon": 6.5666, "lat": 53.2194, "weight": 30},
                {"lon": 6.70, "lat": 53.25}
            ],
            "resolutions": [9, 6]
        }))
        .unwrap()
    };
    let result = body(
        h3_aggregate_handler(Query(H3FormatQuery { format: None }), Json(request()))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(result["point_count"], 3);
    assert_eq!(result["skipped_point_count"], 0);
    let cells = result["cells"].as_array().unwrap();
    let fine = cells
        .iter()
        .filter(|cell| cell["resolution"] == 9)
        .collect::<Vec<_>>();
    assert_eq!(fine.len(), 2);
    assert!(
        fine.iter()
            .any(|cell| cell["count"] == 2 && cell["weight"] == 150.0)
    );
    assert_eq!(
        cells
            .iter()
            .filter(|cell| cell["resolution"] == 6)
            .map(|cell| cell["weight"].as_f64().unwrap())
            .sum::<f64>(),
        151.0
    );

    let collection = body(
        h3_aggregate_handler(
            Query(H3FormatQuery {
                format: Some("geojson".into()),
            }),
            Json(request()),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(
        collection["features"].as_array().unwrap().len(),
        cells.len()
    );

    let mut empty = request();
    empty.resolutions.clear();
    let error = h3_aggregate_handler(Query(H3FormatQuery { format: None }), Json(empty))
        .await
        .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn layers_import_paint_and_feed_surge() {
    use super::layers::*;
    use axum::extract::Path as AxumPath;

    let state = state();
    let write = |payload: serde_json::Value| {
        let state = state.clone();
        async move { write_layer(State(state), Json(serde_json::from_value(payload).unwrap())).await }
    };
    let written = write(json!({
        "layer_id": "city",
        "label": "City",
        "csv": "lon,lat,riders,drivers\n6.5665,53.2194,30,4\n6.5666,53.2194,10,\n6.70,53.25,2,9\n",
        "resolution": 8,
        "columns": [
            {"source": "riders"},
            {"source": "drivers", "aggregation": "max"},
            {"name": "points", "aggregation": "count"}
        ]
    }))
    .await
    .unwrap()
    .0;
    let written = serde_json::to_value(written).unwrap();
    assert_eq!(written["csv_rows"], json!([3, 0]));
    assert_eq!(written["layer"]["cell_count"], 2);
    assert_eq!(written["layer"]["columns"][0]["sum"], 42.0);
    assert_eq!(written["layer"]["columns"][2]["max"], 2.0);

    // Merging inline cells keeps the imported values.
    let busy = netweevil_h3::cell_at(6.5665, 53.2194, parse_resolution(8).unwrap())
        .unwrap()
        .to_string();
    let _ = write(json!({"layer_id": "city", "merge": true,
        "cells": [{"cell": busy, "values": {"jobs": 500}}]}))
    .await
    .unwrap();

    let paint = |payload: serde_json::Value| {
        let state = state.clone();
        async move {
            paint_layer(
                State(state),
                AxumPath("city".to_string()),
                Json(serde_json::from_value(payload).unwrap()),
            )
            .await
        }
    };
    let painted = serde_json::to_value(
        paint(json!({"column": "drivers", "op": "add", "value": 6,
            "bbox": [6.5665, 53.2194, 6.5665, 53.2194], "resolution": 8}))
        .await
        .unwrap()
        .0,
    )
    .unwrap();
    assert_eq!(painted["changed"], 1);
    // A stroke of positions paints each cell once.
    let stroke = serde_json::to_value(
        paint(
            json!({"column": "stroke", "op": "add", "value": 2, "resolution": 8,
            "points": [[6.5665, 53.2194], [6.5666, 53.2194], [6.70, 53.25]], "cells": [busy]}),
        )
        .await
        .unwrap()
        .0,
    )
    .unwrap();
    assert_eq!(stroke["changed"], 2);
    assert_eq!(stroke["layer"]["columns"][4]["sum"], 4.0);
    let _ = paint(
        json!({"column": "stroke", "op": "clear", "points": [[6.5665, 53.2194], [6.70, 53.25]],
        "resolution": 8}),
    )
    .await
    .unwrap();
    let conflict = write(json!({"layer_id": "city"})).await.err().unwrap();
    assert_eq!(conflict.into_response().status(), StatusCode::BAD_REQUEST);
    for invalid in [
        json!({"column": "drivers", "value": 1}),
        json!({"column": "drivers", "value": 1, "bbox": [6.5, 53.2, 6.6, 53.3]}),
        json!({"column": "drivers", "value": 1, "cells": ["nope"]}),
    ] {
        let error = paint(invalid).await.err().unwrap();
        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
    }

    let get = |format: Option<&str>| {
        let state = state.clone();
        let format = format.map(str::to_string);
        async move {
            get_layer(
                State(state),
                AxumPath("city".to_string()),
                Query(H3FormatQuery { format }),
            )
            .await
            .unwrap()
        }
    };
    let layer = body(get(None).await).await;
    assert_eq!(
        layer["layer"]["columns"],
        json!(["riders", "drivers", "points", "jobs", "stroke"])
    );
    assert_eq!(
        layer["layer"]["cells"][&busy],
        json!([40.0, 10.0, 2.0, 500.0, null])
    );
    let collection = body(get(Some("geojson")).await).await;
    assert_eq!(collection["features"].as_array().unwrap().len(), 2);
    let listed = serde_json::to_value(list_layers(State(state.clone())).await.unwrap().0).unwrap();
    assert_eq!(listed["layers"][0]["layer_id"], "city");

    let surge = body(
        surge_handler(
            State(state.clone()),
            Query(H3FormatQuery { format: None }),
            Json(
                serde_json::from_value(json!({
                    "demand": {"layer_id": "city", "column": "riders"},
                    "supply": {"layer_id": "city", "column": "drivers"},
                    "config": {"smoothing_rings": 0},
                    "rebalance_rings": 60
                }))
                .unwrap(),
            ),
        )
        .await
        .unwrap(),
    )
    .await;
    let busy_cell = surge["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|cell| cell["cell"] == busy)
        .unwrap();
    assert_eq!(busy_cell["ratio"], 4.0);
    assert_eq!(busy_cell["multiplier"], 2.5);
    assert_eq!(surge["moves"][0]["to"], busy);

    let missing = surge_handler(
        State(state.clone()),
        Query(H3FormatQuery { format: None }),
        Json(
            serde_json::from_value(json!({
                "demand": {"layer_id": "city", "column": "nope"},
                "supply": {"cells": []}
            }))
            .unwrap(),
        ),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(missing.into_response().status(), StatusCode::BAD_REQUEST);

    let deleted = delete_layer(State(state.clone()), AxumPath("city".to_string()))
        .await
        .unwrap();
    assert_eq!(deleted, StatusCode::NO_CONTENT);
    let gone = delete_layer(State(state), AxumPath("city".to_string()))
        .await
        .err()
        .unwrap();
    assert_eq!(gone.into_response().status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn dispatch_assigns_vehicles_and_prices_trips() {
    use super::layers::dispatch_handler;

    let response = dispatch_handler(
        State(state()),
        Json(
            serde_json::from_value(json!({
                "request": {
                    "vehicles": [
                        {"id": "near", "lon": 6.001, "lat": 53.0, "vehicle_type": "van"},
                        {"id": "far", "lon": 6.0, "lat": 53.0}
                    ],
                    "requests": [
                        {"id": "trip", "pickup": {"lon": 6.001, "lat": 53.0},
                         "dropoff": {"lon": 6.002, "lat": 53.0}},
                        {"id": "van_only", "pickup": {"lon": 6.002, "lat": 53.0},
                         "vehicle_type": "van"},
                        {"id": "bus_only", "pickup": {"lon": 6.001, "lat": 53.0},
                         "vehicle_type": "bus"}
                    ],
                    "snap": {"max_distance_m": 10},
                    "surge": {"config": {"resolution": 9, "smoothing_rings": 0}},
                    "pricing": {"base_fare": 2.0, "per_km": 10.0}
                }
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap()
    .0;
    let response = serde_json::to_value(response).unwrap();
    let result = &response["result"];
    assert_eq!(result["summary"]["assigned_count"], 2);
    assert_eq!(result["summary"]["strategy"], "optimal");
    let assignment = |id: &str| {
        result["assignments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|assignment| assignment["request_id"] == id)
            .unwrap()
            .clone()
    };
    // The van takes the request that needs one; the other car drives further.
    assert_eq!(assignment("van_only")["vehicle_id"], "near");
    let trip = assignment("trip");
    assert_eq!(trip["vehicle_id"], "far");
    assert_eq!(trip["pickup_distance_m"], 100.0);
    assert_eq!(trip["trip_distance_m"], 200.0);
    let multiplier = trip["surge_multiplier"].as_f64().unwrap();
    assert!(multiplier >= 1.0);
    assert_eq!(trip["fare"].as_f64().unwrap(), 4.0 * multiplier);
    assert_eq!(result["unassigned"][0]["request_id"], "bus_only");
    assert_eq!(result["unassigned"][0]["reason"], "no candidate vehicle");
    assert!(
        result["surge"]
            .as_array()
            .is_some_and(|cells| !cells.is_empty())
    );
    assert_eq!(result["idle_vehicle_ids"], json!([]));
}
