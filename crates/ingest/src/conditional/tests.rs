use super::{Condition, parse_conditional};
use netweevil_core::{EVERY_DAY, MinuteInterval, TemporalEffect, VehicleRestrictionKind};

fn kinds(raw: &str) -> Vec<(&str, &'static str)> {
    parse_conditional(raw)
        .unwrap_or_else(|| panic!("{raw} should parse"))
        .into_iter()
        .map(|entry| {
            let kind = match entry.condition {
                Condition::Time(_) => "time",
                Condition::VehicleAbove(_) => "vehicle",
                Condition::Unsupported(_) => "unsupported",
            };
            (entry.value, kind)
        })
        .collect()
}

#[test]
fn splits_entries_and_classifies_conditions() {
    let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("no @ (Mo-Fr 07:00-18:00)", vec![("no", "time")]),
        ("delivery @ (05:00-12:00)", vec![("delivery", "time")]),
        ("130 @ (19:00-06:00)", vec![("130", "time")]),
        (
            "permissive @ (sunrise-sunset); no @ (sunset-sunrise)",
            vec![("permissive", "unsupported"), ("no", "unsupported")],
        ),
        (
            "yes @ (Mo-Th 09:00-18:00; Fr 09:00-21:00; Sa 08:30-17:30)",
            vec![("yes", "time")],
        ),
        ("no @ (weight>7.5)", vec![("no", "vehicle")]),
        ("no @ weight > 3.5", vec![("no", "vehicle")]),
        ("none @ destination", vec![("none", "unsupported")]),
        (
            "no @ (Mo-Fr 07:00-19:00 AND weight>7.5)",
            vec![("no", "unsupported")],
        ),
        (
            "no @ (weight>7.5 AND length>12)",
            vec![("no", "unsupported")],
        ),
        ("60 @ wet", vec![("60", "unsupported")]),
        ("no @ (2025 Jan 13-2025 Apr 4)", vec![("no", "time")]),
    ];
    for (raw, expected) in cases {
        assert_eq!(kinds(raw), expected, "{raw}");
    }
    assert!(parse_conditional("no").is_none());
    assert!(parse_conditional("").is_none());
}

#[test]
fn rescopes_time_domains_and_maps_vehicle_bounds() {
    let entries = parse_conditional("no @ (05:00-12:00)").unwrap();
    let rules = entries[0]
        .condition
        .time_rules(TemporalEffect::OpenOnly, 1 << 4)
        .unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].effect, TemporalEffect::OpenOnly);
    assert_eq!(rules[0].modes, 1 << 4);
    assert_eq!(rules[0].day_mask, EVERY_DAY);
    assert_eq!(
        rules[0].intervals,
        vec![MinuteInterval {
            start_minute: 300,
            end_minute: 720
        }]
    );
    let entries = parse_conditional("no @ (axleload>=10)").unwrap();
    assert_eq!(
        entries[0].condition,
        Condition::VehicleAbove(VehicleRestrictionKind::MaxAxleLoadKg(10_000))
    );
    let entries = parse_conditional("yes @ (weight<3.5)").unwrap();
    assert!(matches!(entries[0].condition, Condition::Unsupported(_)));
}
