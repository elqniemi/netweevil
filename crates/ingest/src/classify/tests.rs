use super::{
    EdgeDirection, classify_access, classify_direction, classify_directional_access,
    classify_highway, classify_speed_and_lanes, parse_duration_seconds, parse_maxspeed_kph,
};
use netweevil_core::{
    AccessMask, EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW, EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY,
    HighwayClass, RoadClass,
};
use osmpbfreader::Tags;

fn tags(pairs: &[(&str, &str)]) -> Tags {
    Tags::from_iter(pairs.iter().map(|&(key, value)| (key.into(), value.into())))
}

#[test]
fn classifies_major_highways() {
    for (value, expected) in [
        ("motorway", Some(HighwayClass::Motorway)),
        ("primary_link", Some(HighwayClass::PrimaryLink)),
        ("footway", Some(HighwayClass::Footway)),
        ("construction", None),
    ] {
        assert_eq!(classify_highway(value), expected, "highway={value}");
    }
}

#[test]
fn classifies_access_from_defaults_and_mode_overrides() {
    let access =
        |pairs: &[(&str, &str)], highway, route| classify_access(&tags(pairs), highway, route).0;
    use AccessMask as A;
    assert_eq!(access(&[], "motorway", None), A::CAR | A::HGV);
    assert_eq!(access(&[], "cycleway", None), A::BICYCLE | A::FOOT);
    let foot_only = [("access", "no"), ("foot", "yes")];
    assert_eq!(access(&foot_only, "residential", None), A::FOOT);
    assert_eq!(access(&[("bicycle", "no")], "cycleway", None), A::FOOT);
    let ferry = [
        ("route", "ferry"),
        ("motor_vehicle", "no"),
        ("bicycle", "no"),
    ];
    assert_eq!(access(&ferry, "", Some("ferry")), A::FOOT);
}

#[test]
fn infers_oneway_behavior() {
    let direction =
        |pairs: &[(&str, &str)]| classify_direction(&tags(pairs), RoadClass::Residential);
    assert_eq!(direction(&[("oneway", "-1")]), EdgeDirection::ReverseOnly);
    assert_eq!(
        direction(&[("junction", "roundabout")]),
        EdgeDirection::ForwardOnly
    );
    assert_eq!(direction(&[]), EdgeDirection::Both);
}

#[test]
fn builds_mode_specific_oneway_access() {
    let access = classify_directional_access(
        &tags(&[("highway", "residential"), ("oneway", "yes")]),
        HighwayClass::Residential,
        RoadClass::Residential,
        AccessMask::new(AccessMask::CAR | AccessMask::BICYCLE | AccessMask::FOOT),
    );
    assert!(access.forward_access.contains(AccessMask::CAR));
    assert!(!access.reverse_access.contains(AccessMask::CAR));
    assert!(access.reverse_access.contains(AccessMask::FOOT));
    assert!(access.reverse_access.contains(AccessMask::BICYCLE));
    assert_ne!(
        access.reverse_flags & EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY,
        0
    );
    assert_ne!(
        access.reverse_flags & EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW,
        0
    );

    let access = classify_directional_access(
        &tags(&[("highway", "cycleway"), ("oneway", "yes")]),
        HighwayClass::Cycleway,
        RoadClass::Path,
        AccessMask::new(AccessMask::BICYCLE | AccessMask::FOOT),
    );
    assert!(access.forward_access.contains(AccessMask::BICYCLE));
    assert!(!access.reverse_access.contains(AccessMask::BICYCLE));
    assert!(access.reverse_access.contains(AccessMask::FOOT));
}

#[test]
fn parses_maxspeed_values_with_units() {
    for (value, expected) in [
        ("50", Some(50.0)),
        ("30 mph", Some(30.0 * 1.609_344)),
        ("10 knots", Some(18.52)),
        ("none", None),
        ("walk", None),
        ("DE:zone30", None),
    ] {
        assert_eq!(parse_maxspeed_kph(value), expected, "maxspeed={value}");
    }
}

#[test]
fn splits_lanes_by_direction() {
    // (forward km/h, reverse km/h, forward lanes, reverse lanes)
    let classify = |pairs: &[(&str, &str)]| {
        let split = classify_speed_and_lanes(&tags(pairs), RoadClass::Primary);
        (
            split.forward_max_speed_kph,
            split.reverse_max_speed_kph,
            split.forward_lanes,
            split.reverse_lanes,
        )
    };
    let bidirectional = [("maxspeed", "80"), ("lanes", "3")];
    assert_eq!(
        classify(&bidirectional),
        (Some(80.0), Some(80.0), Some(2), Some(1))
    );
    let oneway = [
        ("oneway", "yes"),
        ("lanes", "2"),
        ("maxspeed:forward", "100"),
    ];
    assert_eq!(classify(&oneway), (Some(100.0), None, Some(2), None));
    let explicit = [("lanes", "3"), ("lanes:forward", "1")];
    assert_eq!(classify(&explicit), (None, None, Some(1), Some(2)));
}

#[test]
fn parses_common_duration_formats() {
    for (value, expected) in [
        ("5400", 5400.0),
        ("01:30", 5400.0),
        ("01:02:03", 3723.0),
        ("PT45M", 2700.0),
        ("1h 15m", 4500.0),
    ] {
        assert_eq!(parse_duration_seconds(value), Some(expected), "{value}");
    }
}
