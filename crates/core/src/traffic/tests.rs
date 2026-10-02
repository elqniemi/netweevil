use super::*;

#[test]
fn lookup_matches_direction_and_treats_unknown_orientation_as_forward() {
    let bundle = TrafficSpeedBundle {
        schema_version: TRAFFIC_BUNDLE_SCHEMA_VERSION,
        traffic_id: "t".into(),
        timezone: "UTC".into(),
        bucket_minutes: 60,
        pattern_speeds: vec![500; 2 * 168],
        feature_ids: vec![7, 7, 9],
        directions: vec![TRAFFIC_BACKWARD, TRAFFIC_FORWARD, TRAFFIC_FORWARD],
        feature_patterns: vec![1, 0, 1],
    };
    assert_eq!(bundle.validate(), Ok(()));
    assert_eq!(bundle.lookup(7, 1), Some(0));
    assert_eq!(bundle.lookup(7, -1), Some(1));
    assert_eq!(bundle.lookup(7, 0), Some(0));
    assert_eq!(bundle.lookup(9, -1), None);
    assert_eq!(bundle.lookup(8, 1), None);
}
