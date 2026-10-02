use super::*;

#[test]
fn main_vehicle_leg_keeps_intermediate_platform_elevation() {
    let mut legs: Vec<TransitLeg> = serde_json::from_value(serde_json::json!([{
        "leg_type": "transit", "from_id": "A", "to_id": "C", "from_name": "A", "to_name": "C",
        "departure_s": 0, "arrival_s": 20, "trip_id": "T", "geometry": [[0.0,0.0,-20.0],[2.0,0.0,-20.0]]
    }])).unwrap();
    let segments: Vec<netweevil_transit::TransitRouteStopSegment> = serde_json::from_value(serde_json::json!([
        {"segment_index":1,"from_stop_id":"A","to_stop_id":"B","from_stop_name":"A","to_stop_name":"B",
         "departure_s":0,"arrival_s":10,"duration_s":10,"trip_id":"T","geometry":[[0.0,0.0,-20.0],[1.0,0.0,-5.0]],
         "geometry_elevation_source":"interpolated_between_stop_bindings"},
        {"segment_index":2,"from_stop_id":"B","to_stop_id":"C","from_stop_name":"B","to_stop_name":"C",
         "departure_s":10,"arrival_s":20,"duration_s":10,"trip_id":"T","geometry":[[1.0,0.0,-5.0],[2.0,0.0,-20.0]],
         "geometry_elevation_source":"interpolated_between_stop_bindings"}
    ])).unwrap();
    rebuild_transit_leg_geometry(&mut legs, &segments);
    assert_eq!(
        legs[0].geometry,
        vec![[0.0, 0.0, -20.0], [1.0, 0.0, -5.0], [2.0, 0.0, -20.0]]
    );
    assert_eq!(
        legs[0].geometry_elevation_source.as_deref(),
        Some("interpolated_between_stop_bindings")
    );
}

#[test]
fn rail_display_joins_platforms_and_labels_interpolated_elevations() {
    let mut geometry = vec![
        [114.0, 22.0, 0.0],
        [114.005, 22.0, 0.0],
        [114.01, 22.0, 0.0],
    ];
    let from = [114.0, 22.0, -20.0];
    let to = [114.01, 22.0, -10.0];
    let source = anchor_geometry(&mut geometry, Some(&from), Some(&to));
    assert_eq!(geometry[0], from);
    assert_eq!(geometry[2], to);
    assert!((geometry[1][2] + 15.0).abs() < 1e-6);
    assert_eq!(
        source.as_deref(),
        Some("interpolated_between_stop_bindings")
    );
}

#[test]
fn partial_binding_keeps_unknown_elevations_explicit() {
    let mut geometry = vec![[114.0, 22.0, 0.0], [114.01, 22.0, 0.0]];
    let source = anchor_geometry(&mut geometry, Some(&[114.001, 22.0, -15.0]), None);
    assert_eq!(geometry[0], [114.001, 22.0, -15.0]);
    assert_eq!(geometry[1][2], 0.0);
    assert_eq!(
        source.as_deref(),
        Some("bound_endpoint_only_other_elevations_unknown")
    );
}

#[test]
fn coincident_platform_points_do_not_produce_nan() {
    let mut geometry = vec![[114.0, 22.0, 0.0]; 3];
    anchor_geometry(
        &mut geometry,
        Some(&[114.0, 22.0, -20.0]),
        Some(&[114.0, 22.0, -10.0]),
    );
    assert_eq!(geometry[1][2], -15.0);
}
