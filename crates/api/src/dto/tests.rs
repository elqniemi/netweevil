use super::*;

#[test]
fn accessibility_execution_request_accepts_temporal_origin_options() {
    let payload: AccessibilityExecutionRequest = serde_json::from_value(serde_json::json!({
        "profile_id": "pedestrian_multilayer",
        "request": {
            "origins": {
                "points": [{"id":"origin","lon":114.15,"lat":22.28,"z":12.0}],
                "departure_time": "2026-07-10T09:55:00+08:00",
                "scenario": {"id": "central", "features": []}
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

/// Request bodies carry scenario, holiday and overlay content inline. A
/// server path in any of those fields is a type error, and the CLI-only
/// `*_file` keys are not request fields, so no payload can make the server
/// open a file.
#[test]
fn analysis_payloads_cannot_name_server_files() {
    let route = |extra: serde_json::Value| {
        let mut request = serde_json::json!({
            "route_id": "r",
            "origin": {"id": "o", "lon": 6.0, "lat": 53.0},
            "destination": {"id": "d", "lon": 6.1, "lat": 53.0}
        });
        request
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value::<RouteExecutionRequest>(serde_json::json!({"request": request}))
    };
    for rejected in [
        serde_json::json!({"scenario": "/etc/passwd"}),
        serde_json::json!({"overlays": ["/etc/passwd"]}),
        serde_json::json!({"holidays": "/etc/passwd"}),
    ] {
        assert!(
            route(rejected.clone()).is_err(),
            "{rejected} must be rejected"
        );
    }
    let ignored = route(serde_json::json!({
        "scenario_file": "/etc/passwd",
        "holiday_calendar_file": "/etc/passwd",
        "overlay_files": ["/etc/passwd"]
    }))
    .expect("unknown keys are ignored");
    assert_eq!(
        ignored.request.temporal,
        netweevil_query::TemporalRequestOptions::default()
    );

    let batch = |scenario: serde_json::Value| {
        serde_json::from_value::<ScenarioBatchExecutionRequest>(serde_json::json!({
            "request": {"batch_id": "b", "scenarios": [scenario]}
        }))
    };
    for rejected in [
        serde_json::json!({"id": "s", "overlay": "/etc/passwd"}),
        serde_json::json!({"id": "s", "overlay_file": "/etc/passwd"}),
        serde_json::json!({"id": "s", "top_k": {"ranking": "/etc/passwd", "count": 1}}),
    ] {
        assert!(
            batch(rejected.clone()).is_err(),
            "{rejected} must be rejected"
        );
    }
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
