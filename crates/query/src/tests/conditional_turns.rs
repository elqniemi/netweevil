//! Time-conditional turn restrictions in time-dependent searches. The
//! fixture bans the turn 1 -> 2 of the a-b-c-d line; the through route
//! 0, 1, 2 (30 s) reaches the junction 20 s after leaving a, and the bypass
//! 3, 2 costs 35 s. Times are UTC: 2026-10-05 is a Monday and 2026-10-03 a
//! Saturday.

use netweevil_core::{
    FRIDAY, MONDAY, MinuteInterval, SATURDAY, SUNDAY, THURSDAY, TUESDAY, TemporalEffect,
    TemporalRule, WEDNESDAY,
};
use serde_json::json;

use super::fixtures::*;
use crate::{
    AccessibilityRequest, ComponentConstraint, PreparedRoutingEngine, RouteRequest,
    ServiceAreaRequest, TemporalRequestOptions, parse_datetime,
};

const THROUGH: [u32; 3] = [0, 1, 2];
const BYPASS: [u32; 2] = [3, 2];

/// `no_left_turn @ (Mo-Fr 07:00-09:00)`.
fn weekday_rush_ban() -> Vec<TemporalRule> {
    vec![TemporalRule::new(
        MONDAY | TUESDAY | WEDNESDAY | THURSDAY | FRIDAY,
        vec![MinuteInterval {
            start_minute: 7 * 60,
            end_minute: 9 * 60,
        }],
        TemporalEffect::Closed,
    )]
}

/// `no_left_turn` with `restriction:conditional=none @ (Sa,Su)`.
fn ban_lifted_at_weekends() -> Vec<TemporalRule> {
    vec![TemporalRule::new(
        SATURDAY | SUNDAY,
        Vec::new(),
        TemporalEffect::OpenOnly,
    )]
}

fn engine(rules: Vec<TemporalRule>, applies_statically: bool) -> PreparedRoutingEngine {
    exact_engine(
        conditionally_restricted_topology(rules, applies_statically),
        conditionally_restricted_metrics(),
    )
}

fn a_to_d(temporal: TemporalRequestOptions) -> RouteRequest {
    let mut request = route_request("turn", point("a", 6.0, 53.0), point("d", 6.003, 53.0));
    request.snap = snap_within(20.0);
    request.returns = full_geometry();
    request.temporal = temporal;
    request
}

fn departing(at: &str) -> TemporalRequestOptions {
    TemporalRequestOptions {
        departure_time: Some(at.to_string()),
        ..Default::default()
    }
}

fn arriving(at: &str) -> TemporalRequestOptions {
    TemporalRequestOptions {
        arrive_by: Some(at.to_string()),
        arrive_by_lookback_s: 3_600.0,
        ..Default::default()
    }
}

fn path(engine: &PreparedRoutingEngine, temporal: TemporalRequestOptions) -> Vec<u32> {
    engine
        .execute_route(&a_to_d(temporal))
        .expect("route succeeds")
        .edge_path
}

#[test]
fn departures_inside_the_window_avoid_the_banned_turn() {
    let engine = engine(weekday_rush_ban(), false);
    // Static routing ignores a ban that holds only under its condition.
    assert_eq!(path(&engine, TemporalRequestOptions::default()), THROUGH);
    for (departure, expected) in [
        ("2026-10-05T08:00:00+00:00", &BYPASS[..]),
        ("2026-10-05T10:00:00+00:00", &THROUGH[..]),
        ("2026-10-03T08:00:00+00:00", &THROUGH[..]),
        // The junction is reached 20 s after departure: at 06:59:50 the
        // turn is still allowed, at 07:00:05 it is banned.
        ("2026-10-05T06:59:30+00:00", &THROUGH[..]),
        ("2026-10-05T06:59:45+00:00", &BYPASS[..]),
    ] {
        assert_eq!(path(&engine, departing(departure)), expected, "{departure}");
    }
}

#[test]
fn a_lifted_unconditional_ban_holds_statically_and_outside_its_lift() {
    let engine = engine(ban_lifted_at_weekends(), true);
    assert_eq!(path(&engine, TemporalRequestOptions::default()), BYPASS);
    assert_eq!(
        path(&engine, departing("2026-10-05T08:00:00+00:00")),
        BYPASS
    );
    assert_eq!(
        path(&engine, departing("2026-10-03T08:00:00+00:00")),
        THROUGH
    );
}

#[test]
fn arrive_by_departs_late_enough_only_where_the_turn_is_allowed() {
    let engine = engine(weekday_rush_ban(), false);
    for (deadline, expected, departure) in [
        (
            "2026-10-05T10:00:00+00:00",
            &THROUGH[..],
            "2026-10-05T09:59:30+00:00",
        ),
        (
            "2026-10-05T08:00:00+00:00",
            &BYPASS[..],
            "2026-10-05T07:59:25+00:00",
        ),
        // Through traffic would reach the junction at 08:59:55, in the ban.
        (
            "2026-10-05T09:00:05+00:00",
            &BYPASS[..],
            "2026-10-05T08:59:30+00:00",
        ),
        // ... but at 09:00:10 the ban has ended.
        (
            "2026-10-05T09:00:20+00:00",
            &THROUGH[..],
            "2026-10-05T08:59:50+00:00",
        ),
    ] {
        let route = engine
            .execute_route(&a_to_d(arriving(deadline)))
            .expect("arrive-by route succeeds");
        assert_eq!(route.edge_path, expected, "{deadline}");
        let departed = route.summary.departure_time.as_deref().expect("departure");
        assert_eq!(
            parse_datetime(departed).unwrap(),
            parse_datetime(departure).unwrap(),
            "{deadline}"
        );
    }
}

#[test]
fn component_constrained_routes_respect_the_window() {
    let mut metrics = conditionally_restricted_metrics();
    metrics.components = vec![cost_component("length", &[1.0; 5])];
    let engine = exact_engine(
        conditionally_restricted_topology(weekday_rush_ban(), false),
        metrics,
    );
    let constrained = |departure: &str| TemporalRequestOptions {
        constraints: vec![ComponentConstraint {
            component: "length".to_string(),
            max_value: 10.0,
        }],
        ..departing(departure)
    };
    assert_eq!(
        path(&engine, constrained("2026-10-05T08:00:00+00:00")),
        BYPASS
    );
    assert_eq!(
        path(&engine, constrained("2026-10-05T10:00:00+00:00")),
        THROUGH
    );
}

#[test]
fn service_areas_and_accessibility_follow_the_window() {
    let engine = engine(weekday_rush_ban(), false);
    // Within 24 s the through route reaches edge 2; the bypass does not.
    let reaches_edge_2 = |departure: &str| {
        let request: ServiceAreaRequest = serde_json::from_value(json!({
            "analysis_id": "turn-area",
            "origins": [point("a", 6.0, 53.0)],
            "thresholds": [{"limit": 24.0, "metric": "travel_time_s"}],
            "snap": {"max_distance_m": 20.0},
            "output_mode": "network",
            "returns": {"segments": true, "geometry": false},
            "departure_time": departure,
        }))
        .unwrap();
        engine
            .execute_service_area(&request)
            .expect("service area succeeds")
            .segments
            .iter()
            .any(|segment| segment.edge_index == 2)
    };
    assert!(!reaches_edge_2("2026-10-05T08:00:00+00:00"));
    assert!(reaches_edge_2("2026-10-05T10:00:00+00:00"));

    let nearest = |departure: &str| {
        let request: AccessibilityRequest = serde_json::from_value(json!({
            "origins": {
                "points": [point("a", 6.0, 53.0)],
                "snap": {"max_distance_m": 20.0},
                "departure_time": departure,
            },
            "categories": [{
                "category_id": "d",
                "destinations": {
                    "points": [point("d", 6.003, 53.0)],
                    "snap": {"max_distance_m": 20.0},
                },
            }],
            "thresholds_s": [60.0],
            "max_travel_time_s": 60.0,
        }))
        .unwrap();
        engine
            .execute_accessibility(&request)
            .expect("accessibility succeeds")
            .rows[0]
            .nearest_travel_time_s
            .expect("destination reached")
    };
    assert_eq!(nearest("2026-10-05T08:00:00+00:00"), 35.0);
    assert_eq!(nearest("2026-10-05T10:00:00+00:00"), 30.0);
}
