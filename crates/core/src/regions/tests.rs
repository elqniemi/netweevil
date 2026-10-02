use super::*;

#[test]
fn left_hand_traffic_table_is_sorted_for_binary_search() {
    assert!(LEFT_HAND_TRAFFIC.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn driving_side_follows_country_code() {
    assert_eq!(country_driving_side("GB"), DrivingSide::Left);
    assert_eq!(country_driving_side("hk"), DrivingSide::Left);
    assert_eq!(country_driving_side("JP"), DrivingSide::Left);
    assert_eq!(country_driving_side("NL"), DrivingSide::Right);
    assert_eq!(country_driving_side("US"), DrivingSide::Right);
    assert_eq!(AdminArea::default().driving_side(), None);
}

#[test]
fn node_lookups_resolve_interned_values() {
    let regions = TopologyRegions {
        admin_source: Some(AdminBoundarySource::SourceExtract),
        admin_areas: vec![AdminArea {
            country_code: "NL".to_string(),
            state_code: Some("NL-GR".to_string()),
            ..AdminArea::default()
        }],
        node_admin_area: vec![0, NO_ADMIN_AREA],
        timezones: vec!["Europe/Amsterdam".to_string()],
        node_timezone: vec![0, NO_TIMEZONE],
    };
    assert_eq!(regions.node_country(0), Some("NL"));
    assert_eq!(regions.node_country(1), None);
    assert_eq!(regions.node_country(2), None);
    assert_eq!(regions.node_timezone(0), Some("Europe/Amsterdam"));
    assert_eq!(regions.node_timezone(1), None);
    assert!(regions.has_admin_areas() && regions.has_timezones());
    assert!(!TopologyRegions::default().has_admin_areas());
}
