use super::edge_traversal_cost;
use crate::schema::{FacilityCost, ProfileDocument};
use crate::test_support::{edge, profile};
use netweevil_core::{AccessMask, DirectedEdge, HighwayClass, RoadClass};

/// Car profile driving primary roads at 80 km/h.
fn car_profile(posted_limits: &str) -> ProfileDocument {
    profile(
        "car",
        &format!(
            "speeds: {{posted_limits: {posted_limits}}}\n\
             speed_rules:\n  - {{match: {{highway: primary}}, speed_kph: 80}}"
        ),
    )
}

fn primary_edge(max_speed_kph: Option<f32>) -> DirectedEdge {
    DirectedEdge {
        max_speed_kph,
        ..edge(0, RoadClass::Primary, AccessMask::CAR)
    }
}

/// Effective speed over a flat primary edge with no facility.
fn speed_kph(profile: &ProfileDocument, edge: &DirectedEdge) -> f64 {
    let length_m = f64::from(edge.length_m);
    let cost = edge_traversal_cost(
        profile,
        edge,
        length_m,
        HighwayClass::Primary,
        0.0,
        0.0,
        None,
        &|_, _| false,
    )
    .expect("travel time");
    length_m / cost.travel_time_s * 3.6
}

#[test]
fn combines_posted_limits_per_policy() {
    // Posted limits below (50), above (100) and absent from the 80 km/h
    // profile speed, per policy.
    for (policy, expected) in [
        ("cap", [50.0, 80.0, 80.0]),
        ("prefer", [50.0, 100.0, 80.0]),
        ("ignore", [80.0, 80.0, 80.0]),
    ] {
        let profile = car_profile(policy);
        for (limit, expected) in [Some(50.0), Some(100.0), None].into_iter().zip(expected) {
            let actual = speed_kph(&profile, &primary_edge(limit));
            assert!(
                (actual - expected).abs() < 1e-6,
                "{policy} with limit {limit:?}: {actual} km/h"
            );
        }
    }
}

#[test]
fn posted_limits_never_apply_to_foot_profiles() {
    let mut profile = car_profile("prefer");
    profile.profile.mode = netweevil_core::TravelMode::Foot;
    profile.speed_rules.clear();
    // Foot speed comes from the road-class default, not the limit.
    assert!(speed_kph(&profile, &primary_edge(Some(100.0))) < 100.0);
}

#[test]
fn omitted_preferences_are_neutral() {
    let omitted: crate::schema::PreferencesConfig = Default::default();
    let empty: crate::schema::PreferencesConfig = serde_yaml::from_str("{}").unwrap();
    for (value, neutral) in [
        (omitted.use_highways, empty.use_highways),
        (omitted.use_tolls, empty.use_tolls),
        (omitted.use_tracks, empty.use_tracks),
    ] {
        assert_eq!(value, neutral);
        assert_eq!(super::preference_multiplier(value), 1.0);
    }
}

/// Foot profile walking paths at 3.6 km/h (1 m/s) with a steep-floored
/// Tobler slope model.
fn foot_profile() -> ProfileDocument {
    profile(
        "foot",
        "speed_rules:\n  - {match: {highway: path}, speed_kph: 3.6}\n\
         slope_model: {kind: tobler, min_speed_factor: 0.1}",
    )
}

fn facility_time(facility: &FacilityCost, length_m: f64, descent_m: f32) -> f64 {
    edge_traversal_cost(
        &foot_profile(),
        &edge(0, RoadClass::Path, AccessMask::FOOT),
        length_m,
        HighwayClass::Path,
        0.0,
        descent_m,
        Some(facility),
        &|_, _| false,
    )
    .expect("travel time")
    .travel_time_s
}

#[test]
fn stairs_use_vertical_speed_without_the_walking_slope_model() {
    let stairs = FacilityCost::Stairs {
        vertical_speed_mps: 0.4,
        boarding_penalty_s: 1.5,
        burden_per_vertical_m: 0.0,
        burden_component: "stair_burden".to_string(),
    };
    // A 6 m flight descending 4 m: the vertical rule's 10 s, not 6 m at the
    // slope model's 0.1 floor.
    assert!((facility_time(&stairs, 6.0, 4.0) - 11.5).abs() < 1e-9);
    // A flat landing piece of the same staircase pays no boarding again.
    assert!((facility_time(&stairs, 2.0, 0.0) - 2.0).abs() < 1e-9);
}

#[test]
fn lift_wait_is_charged_on_the_vertical_piece_only() {
    let lift = FacilityCost::Lift {
        wait_s: 25.0,
        vertical_speed_mps: 1.0,
        wait_component: "lift_wait".to_string(),
    };
    // Door stub, shaft, door stub: one wait for one ride.
    let ride = facility_time(&lift, 0.5, 0.0)
        + facility_time(&lift, 6.0, 6.0)
        + facility_time(&lift, 0.5, 0.0);
    assert!((ride - 32.0).abs() < 1e-9, "{ride}");
}
