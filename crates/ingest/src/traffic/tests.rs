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
    let bundle = build_traffic_bundle("t", &doc, None, Some(&csv), None, None).unwrap();

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
    let error = build_traffic_bundle("t", &doc, None, Some(conflicting), None, None).unwrap_err();
    assert!(format!("{error:#}").contains("more than one assignment"));
}

/// Cell assignments resolve to the motorized features inside the cells: the
/// finest cell wins, road-class rules select within a cell, a feature that
/// spans cells takes the pattern covering most of its length, and an
/// explicit feature assignment wins over a cell.
#[test]
fn cell_assignments_resolve_to_features() {
    use crate::test_util::{edge, topology};
    use netweevil_core::{AccessMask, RoadClass};
    use netweevil_h3::{cell_at, cell_center, parse_resolution};

    let coarse = cell_at(6.5665, 53.2194, parse_resolution(7).unwrap()).unwrap();
    let mut children = coarse.children(parse_resolution(9).unwrap());
    let fine = children.next().unwrap();
    let other = children.last().unwrap();
    let [fine_lon, fine_lat] = cell_center(fine);
    let [other_lon, other_lat] = cell_center(other);
    // Nodes 0-1 sit in the fine cell, nodes 2-3 elsewhere in the coarse cell.
    let nodes = [
        (fine_lon, fine_lat, 0.0),
        (fine_lon + 1e-5, fine_lat, 0.0),
        (other_lon, other_lat, 0.0),
        (other_lon + 1e-5, other_lat, 0.0),
        (0.0, 0.0, 0.0),
        (1e-5, 0.0, 0.0),
    ];
    let mut backward = edge(1, 1, 0, 10);
    backward.source_direction = -1;
    let mut primary = edge(3, 2, 3, 30);
    primary.road_class = RoadClass::Primary;
    let mut spanning_short = edge(4, 0, 1, 40);
    spanning_short.length_m = 50;
    let mut spanning_long = edge(5, 2, 3, 40);
    spanning_long.length_m = 500;
    let mut footpath = edge(6, 0, 1, 50);
    footpath.access_mask = AccessMask::new(AccessMask::FOOT);
    let edges = [
        edge(0, 0, 1, 10),
        backward,
        edge(2, 2, 3, 20),
        primary,
        spanning_short,
        spanning_long,
        footpath,
        edge(7, 0, 1, 60),
        edge(8, 4, 5, 70),
    ];
    let topology = topology(&nodes, &edges);

    let day = |speed: u32| vec![speed.to_string(); 24].join(", ");
    let doc = document(&format!(
        r#"
timezone: Europe/Amsterdam
bucket_minutes: 60
patterns:
  - {{id: district, speeds_kph: [{}]}}
  - {{id: arterial, speeds_kph: [{}]}}
  - {{id: centre, speeds_kph: [{}]}}
  - {{id: measured, speeds_kph: [{}]}}
assignments:
  - {{feature_id: 60, direction: forward, pattern: measured}}
cell_assignments:
  - {{cell: {coarse}, pattern: arterial, road_classes: [primary, secondary]}}
  - {{cell: {coarse}, pattern: district}}
"#,
        day(30),
        day(50),
        day(15),
        day(42),
    ));
    let csv = format!("cell,pattern_id,road_classes\n{fine},centre,residential service\n");

    let error = build_traffic_bundle("t", &doc, None, None, Some(&csv), None).unwrap_err();
    assert!(format!("{error:#}").contains("resolve against a dataset"));

    let bundle = build_traffic_bundle("t", &doc, None, None, Some(&csv), Some(&topology)).unwrap();
    let speed = |feature: i64, direction: i8| {
        bundle
            .lookup(feature, direction)
            .map(|pattern| bundle.pattern(pattern)[0])
    };
    assert_eq!(speed(10, TRAFFIC_FORWARD), Some(150), "fine cell wins");
    assert_eq!(speed(10, TRAFFIC_BACKWARD), Some(150));
    assert_eq!(speed(20, TRAFFIC_FORWARD), Some(300), "coarse fallback");
    assert_eq!(speed(20, TRAFFIC_BACKWARD), None, "no such edge");
    assert_eq!(speed(30, TRAFFIC_FORWARD), Some(500), "road-class rule");
    assert_eq!(speed(40, TRAFFIC_FORWARD), Some(300), "longest share");
    assert_eq!(speed(50, TRAFFIC_FORWARD), None, "not motorized");
    assert_eq!(speed(60, TRAFFIC_FORWARD), Some(420), "feature wins");
    assert_eq!(speed(70, TRAFFIC_FORWARD), None, "outside every cell");

    let bad = "cell,pattern_id,road_classes\nnope,centre,\n";
    assert!(build_traffic_bundle("t", &doc, None, None, Some(bad), Some(&topology)).is_err());
    let bad_class = format!("cell,pattern_id,road_classes\n{fine},centre,boulevard\n");
    let error =
        build_traffic_bundle("t", &doc, None, None, Some(&bad_class), Some(&topology)).unwrap_err();
    assert!(format!("{error:#}").contains("unknown road class 'boulevard'"));
}
