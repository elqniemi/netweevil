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
