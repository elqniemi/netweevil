use super::{
    classify_highway, classify_speed_and_lanes, conditional_speed_rules, parse_duration_seconds,
    parse_maxspeed_kph,
};
use crate::test_util::tags;
use netweevil_core::{AccessMask, HighwayClass, MinuteInterval, RoadClass, TemporalEffect};

#[test]
fn classifies_major_highways() {
    for (value, expected) in [
        ("motorway", Some(HighwayClass::Motorway)),
        ("primary_link", Some(HighwayClass::PrimaryLink)),
        ("footway", Some(HighwayClass::Footway)),
        ("busway", Some(HighwayClass::Busway)),
        ("construction", None),
    ] {
        assert_eq!(classify_highway(value), expected, "highway={value}");
    }
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

#[test]
fn imports_single_time_conditional_speed_limits_per_direction() {
    // (tags, forward kph, backward kph)
    let cases: Vec<(Vec<(&str, &str)>, Option<f32>, Option<f32>)> = vec![
        (
            vec![
                ("maxspeed", "100"),
                ("maxspeed:conditional", "130 @ (19:00-06:00)"),
            ],
            Some(130.0),
            Some(130.0),
        ),
        (
            vec![("maxspeed:forward:conditional", "60 @ (Mo-Fr 07:00-09:00)")],
            Some(60.0),
            None,
        ),
        (vec![("maxspeed:conditional", "60 @ wet")], None, None),
        (
            vec![("maxspeed:conditional", "80 @ (weight>3.5)")],
            None,
            None,
        ),
        (
            vec![(
                "maxspeed:conditional",
                "30 @ (07:00-09:00); 50 @ (09:00-17:00)",
            )],
            None,
            None,
        ),
    ];
    for (pairs, forward, backward) in cases {
        let (forward_rules, backward_rules, _) = conditional_speed_rules(&tags(&pairs));
        let speed = |rules: &[netweevil_core::TemporalRule]| {
            rules.first().map(|rule| match rule.effect {
                TemporalEffect::MaxSpeed(kph) => kph,
                other => panic!("unexpected effect {other:?}"),
            })
        };
        assert_eq!(speed(&forward_rules), forward, "{pairs:?}");
        assert_eq!(speed(&backward_rules), backward, "{pairs:?}");
    }
    let (rules, _, counts) =
        conditional_speed_rules(&tags(&[("maxspeed:conditional", "130 @ (19:00-06:00)")]));
    assert_eq!(rules[0].modes, AccessMask::MOTOR_VEHICLES);
    assert_eq!(
        rules[0].intervals,
        vec![MinuteInterval {
            start_minute: 19 * 60,
            end_minute: 6 * 60
        }]
    );
    assert_eq!((counts.imported, counts.skipped), (1, 0));
}
