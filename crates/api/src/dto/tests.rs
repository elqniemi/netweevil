use super::*;

#[test]
fn accessibility_execution_request_accepts_temporal_origin_options() {
    let payload: AccessibilityExecutionRequest = serde_json::from_value(serde_json::json!({
        "profile_id": "pedestrian_multilayer",
        "request": {
            "origins": {
                "points": [{"id":"origin","lon":114.15,"lat":22.28,"z":12.0}],
                "departure_time": "2026-07-10T09:55:00+08:00",
                "scenario": "central.yml"
            },
            "categories": [{
                "category_id": "stations",
                "destinations": {"points":[{"id":"station","lon":114.16,"lat":22.29}]}
            }],
            "thresholds_s": [300.0, 600.0],
            "max_travel_time_s": 600.0
        }
    }))
    .expect("accessibility request parses");
    assert_eq!(payload.profile_id.as_deref(), Some("pedestrian_multilayer"));
    assert_eq!(
        payload.request.origins.temporal.departure_time.as_deref(),
        Some("2026-07-10T09:55:00+08:00")
    );
    assert_eq!(payload.request.categories.len(), 1);
}

#[test]
fn transit_route_request_reads_transfer_profile_from_modes() {
    let payload: TransitRouteExecutionRequest = serde_json::from_value(serde_json::json!({
        "feed_id": "hk_example_mtr",
        "request": {
            "route_id": "central_to_admiralty",
            "origin": {"id": "origin", "lon": 114.158, "lat": 22.281},
            "destination": {"id": "destination", "lon": 114.164, "lat": 22.279},
            "time": {"datetime": "2026-07-10T09:55:00+08:00"},
            "modes": {"transfer_profile_id": "pedestrian_step_free_multilayer"}
        }
    }))
    .expect("transit route request parses");
    assert_eq!(
        payload.request.modes.transfer_profile_id.as_deref(),
        Some("pedestrian_step_free_multilayer")
    );
}
