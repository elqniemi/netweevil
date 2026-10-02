use std::collections::BTreeMap;

use super::fixtures::*;
use crate::*;

#[test]
fn buffered_bundle_write_flushes_complete_compatible_bytes() {
    let bundle = import(line_feed(), 1);
    let path = temp_path("bundle");
    write_transit_bundle(&path, &bundle).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bincode::serialize(&bundle).unwrap()
    );
    assert_eq!(
        read_transit_bundle(&path).unwrap().connections.len(),
        bundle.connections.len()
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn hardens_bom_blank_stop_times_sentinel_calendar_and_after_midnight_times() {
    let bundle = import(hardened_feed(), 2);

    // The sentinel calendar activates Mondays, but calendar_dates removes the
    // Monday and adds Tuesday. Only the Tuesday copy should be materialized.
    let tuesday = 86_400;
    assert_eq!(bundle.connections.len(), 3);
    assert_eq!(bundle.connections[0].departure_s, tuesday + hm(25, 0));
    assert_eq!(bundle.connections[0].arrival_s, tuesday + hm(25, 20));
    assert_eq!(bundle.connections[1].departure_s, tuesday + hm(25, 20));
    assert_eq!(bundle.connections[2].arrival_s, tuesday + hm(26, 0));
}

#[test]
fn rejects_unbounded_blank_stop_times() {
    let mut files = hardened_feed();
    files.insert(
        "stop_times.txt",
        stop_times(&["T1,,,A,1", "T1,,,B,2", "T1,26:00:00,26:00:00,D,3"]),
    );
    let error = try_import(files, SERVICE_START, 2).unwrap_err();
    assert!(error.to_string().contains("only intermediate blanks"));
}

#[test]
fn gtfs_import_requires_one_valid_shared_agency_timezone() {
    for raw in [
        "agency_timezone\nNot/AZone\n",
        "agency_timezone\nEurope/Amsterdam\nEurope/London\n",
        "agency_timezone\n",
        "agency_name\nMissing timezone\n",
    ] {
        let mut files = line_feed();
        files.insert("agency.txt", raw.to_string());
        assert!(
            try_import(files, SERVICE_START, 1).is_err(),
            "invalid agency data should fail: {raw}"
        );
    }
}

#[test]
fn frequency_departures_and_service_days_have_distinct_vehicle_runs() {
    let bundle = import(frequency_feed(), 7);
    let mut runs = BTreeMap::<u32, Vec<&TransitConnection>>::new();
    for connection in &bundle.connections {
        runs.entry(connection.run_index)
            .or_default()
            .push(connection);
    }
    assert_eq!(runs.len(), 4 * 7);
    for connections in runs.values() {
        assert_eq!(connections.len(), 2);
        assert_eq!(connections[0].trip_index, connections[1].trip_index);
        assert_eq!(connections[0].arrival_s, connections[1].departure_s);
    }
    let imported_again = import(frequency_feed(), 7);
    assert_eq!(
        bincode::serialize(&bundle).unwrap(),
        bincode::serialize(&imported_again).unwrap()
    );
}

#[test]
fn applies_explicit_stop_bindings_with_3d_attribute_filters() {
    let mut bundle = import(line_feed(), 1);
    let platform_binding = TransitStopBindingTarget::Coordinate {
        lon: 114.17,
        lat: 22.3,
        z: Some(-8.5),
        z_window_m: Some(1.5),
        attribute_filter: BTreeMap::from([("indoor_location".to_string(), "platform".to_string())]),
    };
    let table = TransitStopBindingTable {
        schema_version: TRANSIT_STOP_BINDING_SCHEMA_VERSION,
        feed_id: Some("fixture".to_string()),
        bindings: vec![
            TransitStopBinding {
                stop_id: "A".to_string(),
                target: TransitStopBindingTarget::Node { node_id: 42 },
            },
            TransitStopBinding {
                stop_id: "C".to_string(),
                target: platform_binding.clone(),
            },
        ],
    };
    // Binding files are authored as JSON with snake_case target tags.
    let json = serde_json::to_string(&table).unwrap();
    assert!(json.contains("\"node\"") && json.contains("\"coordinate\""));
    assert_eq!(
        serde_json::from_str::<TransitStopBindingTable>(&json).unwrap(),
        table
    );

    let summary = apply_transit_stop_bindings(&mut bundle, &table).expect("bindings apply");
    assert_eq!(summary.bound_stop_count, 2);
    assert_eq!(summary.unbound_stop_count, 1);
    assert_eq!(
        bundle.stops[0].binding,
        Some(TransitStopBindingTarget::Node { node_id: 42 })
    );
    assert_eq!(bundle.stops[2].binding, Some(platform_binding));
}
