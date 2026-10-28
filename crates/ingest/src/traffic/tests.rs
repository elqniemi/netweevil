use netweevil_core::{TRAFFIC_BACKWARD, TRAFFIC_FORWARD};

use super::*;

fn document(yaml: &str) -> TrafficSourceDocument {
    serde_yaml::from_str(yaml).expect("document parses")
}

/// Inline and CSV speed vectors that are identical share one pattern, `both`
/// expands to two directed features, day profiles repeat across the week
/// with their weekend override, and unreferenced patterns are dropped.
#[test]
fn bundle_interns_patterns_and_expands_directions() {
    let doc = document(
        r#"
timezone: Europe/Amsterdam
bucket_minutes: 60
patterns:
  - id: peak
    speeds_kph: [50, 50, 50, 50, 50, 50, 50, 20, 20, 50, 50, 50, 50, 50, 50, 50, 25, 25, 50, 50, 50, 50, 50, 50]
    weekend_speeds_kph: [50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50]
  - id: unused
    speeds_kph: [10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10]
assignments:
  - {feature_id: 42, direction: both, pattern: peak}
"#,
    );
    let flat = vec!["33.3"; 24].join(" ");
    let csv = format!(
        "feature_id,direction,speeds_kph\n7,forward,{flat}\n-3,backward,{flat}\n9,forward,0 {}\n",
        vec!["33.3"; 23].join(" ")
    );
    let bundle = build_traffic_bundle("t", &doc, None, Some(&csv)).unwrap();

    assert_eq!(bundle.feature_ids, vec![-3, 7, 9, 42, 42]);
    assert_eq!(
        bundle.directions,
        vec![
            TRAFFIC_BACKWARD,
            TRAFFIC_FORWARD,
            TRAFFIC_FORWARD,
            TRAFFIC_BACKWARD,
            TRAFFIC_FORWARD
        ]
    );
    // -3 and 7 share a vector; 9 differs by its no-data first bucket.
    assert_eq!(bundle.pattern_count(), 3);
    assert_eq!(bundle.feature_patterns[0], bundle.feature_patterns[1]);
    let peak = bundle.pattern(bundle.lookup(42, 1).unwrap());
    assert_eq!(peak[7], 200, "Monday 07:00 is 20 km/h in 0.1 km/h units");
    assert_eq!(peak[4 * 24 + 17], 250, "Friday 17:00 keeps the weekday dip");
    assert_eq!(peak[5 * 24 + 7], 500, "Saturday uses the weekend profile");
    assert_eq!(bundle.pattern(bundle.lookup(9, 1).unwrap())[0], 0);
    assert_eq!(bundle.pattern(bundle.lookup(7, 1).unwrap())[0], 333);

    let conflicting = "feature_id,direction,pattern_id\n42,forward,peak\n";
    let error = build_traffic_bundle("t", &doc, None, Some(conflicting)).unwrap_err();
    assert!(format!("{error:#}").contains("more than one assignment"));
}
