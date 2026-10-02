use netweevil_query::{RouteBatchDocument, RouteBatchResult, RouteViolationType};
use rusqlite::Connection;
use serde_json::json;

use crate::output::write_route_batch_result;
use crate::test_support::{TempFile, route_result, row_count};

#[test]
fn writes_route_batch_geopackage_with_detail_tables() {
    let requests: RouteBatchDocument = serde_json::from_value(json!({"requests": [
        {"profile_id": "car_research_v3", "request": {
            "route_id": "route_1",
            "origin": {"id": "origin", "lon": 6.0, "lat": 53.0},
            "destination": {"id": "destination", "lon": 6.2, "lat": 53.2}
        }},
        {"profile_id": "car_research_v3", "request": {
            "route_id": "route_2",
            "origin": {"id": "origin_2", "lon": 6.3, "lat": 53.3},
            "destination": {"id": "destination_2", "lon": 6.4, "lat": 53.4}
        }}
    ]}))
    .unwrap();

    // Two segments (the second an illegal turn), single-entry breakdowns, and
    // one violation, so every detail table gets rows.
    let mut route = route_result("route_1");
    route.summary.violation_count = 1;
    route.summary.violation_types = vec![RouteViolationType::IllegalTurn];
    route.segments = Some(
        serde_json::from_value(json!([
            {"edge_id": 10, "from_node_id": 1, "to_node_id": 2, "source_way_id": 100,
             "length_m": 500, "travel_time_s": 60.0, "generalized_cost": 60.0,
             "road_class": "residential", "surface": "paved", "name": "Alpha"},
            {"edge_id": 11, "from_node_id": 2, "to_node_id": 3, "source_way_id": 101,
             "length_m": 500, "travel_time_s": 60.0, "generalized_cost": 60.0,
             "road_class": "residential", "surface": "paved", "name": "Beta",
             "violation_type": "illegal_turn"}
        ]))
        .unwrap(),
    );
    route.breakdowns = Some(
        serde_json::from_value(json!({
            "road_class": {"residential": {"distance_m": 1000, "time_s": 120.0}},
            "surface": {"paved": {"distance_m": 1000, "time_s": 120.0}}
        }))
        .unwrap(),
    );
    route.violations = serde_json::from_value(json!([{
        "violation_type": "illegal_turn", "edge_id": 11, "from_edge_id": 10, "to_edge_id": 11,
        "distance_m": 12.0, "penalty_s": 4.0, "penalty_generalized_cost": 4.0
    }]))
    .unwrap();
    let result: RouteBatchResult = serde_json::from_value(json!({
        "route_count": 2, "succeeded_count": 1, "failed_count": 1,
        "items": [
            {"route_id": "route_1", "origin_id": "origin", "destination_id": "destination",
             "status": "succeeded", "route": route},
            {"route_id": "route_2", "origin_id": "origin_2", "destination_id": "destination_2",
             "status": "failed", "error": "snap failed"}
        ]
    }))
    .unwrap();

    let path = TempFile::new("route-batch.gpkg");
    write_route_batch_result(&path, &requests, &result).expect("route batch gpkg written");

    let connection = Connection::open(&path).expect("gpkg opens");
    for (table, expected) in [
        ("routes", 1),
        ("route_segments", 2),
        ("route_breakdown_road_class", 1),
        ("route_breakdown_surface", 1),
        ("route_violations", 1),
        ("route_failures", 1),
    ] {
        assert_eq!(row_count(&connection, table), expected, "{table}");
    }
}
