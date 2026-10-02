use netweevil_core::{TRAFFIC_BUNDLE_SCHEMA_VERSION, TrafficSpeedBundle};

use super::{read_traffic_bundle, write_traffic_bundle};

/// Many directed features share a few smooth weekly patterns; the bundle
/// must round-trip exactly and stay far below its raw column size.
#[test]
fn traffic_bundle_round_trips_compactly() {
    let buckets = 7 * 96;
    let pattern_speeds = (0..12_u16)
        .flat_map(|pattern| {
            (0..buckets as u16).map(move |bucket| {
                let dip = if (bucket % 96) / 4 == 8 { 150 } else { 0 };
                300 + pattern * 50 - dip
            })
        })
        .collect::<Vec<_>>();
    let mut feature_ids = Vec::new();
    let mut directions = Vec::new();
    let mut feature_patterns = Vec::new();
    for feature in -500_i64..20_000 {
        for direction in [-1_i8, 1] {
            feature_ids.push(feature * 3);
            directions.push(direction);
            feature_patterns.push((feature.unsigned_abs() % 12) as u32);
        }
    }
    let bundle = TrafficSpeedBundle {
        schema_version: TRAFFIC_BUNDLE_SCHEMA_VERSION,
        traffic_id: "round_trip".into(),
        timezone: "Europe/Amsterdam".into(),
        bucket_minutes: 15,
        pattern_speeds,
        feature_ids,
        directions,
        feature_patterns,
    };
    let path = std::env::temp_dir().join(format!(
        "netweevil-persist-traffic-{}.bin",
        std::process::id()
    ));
    write_traffic_bundle(&path, &bundle).unwrap();
    let size = std::fs::metadata(&path).unwrap().len();
    let read = read_traffic_bundle(&path);
    let _ = std::fs::remove_file(&path);
    assert_eq!(read.unwrap(), bundle);
    let raw = bundle.pattern_speeds.len() * 2 + bundle.feature_ids.len() * (8 + 1 + 4);
    assert!(size * 4 < raw as u64, "{size} bytes vs {raw} raw");
}
