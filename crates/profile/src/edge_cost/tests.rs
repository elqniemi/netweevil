use super::edge_traversal_cost;
use crate::schema::ProfileDocument;
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
