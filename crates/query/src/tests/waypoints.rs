use netweevil_profile::ReturnConfig;

use super::fixtures::{
    edge, exact_engine, full_geometry, metrics, point, snap_within, topology, with_no_turn,
};
use crate::{PreparedRoutingEngine, Waypoint, WaypointKind, WaypointRequest};

/// 100 m edges costing 10 between nodes a (6.0, 53.0), b (6.001, 53.0),
/// c (6.001, 53.001) and d (6.002, 53.001); `restricted` forbids 0, 1, 2.
fn engine(connections: &[(u32, u32)], restricted: bool) -> PreparedRoutingEngine {
    let edges = connections
        .iter()
        .enumerate()
        .map(|(index, &(from, to))| edge(index as u32, from, to, 10, 100))
        .collect::<Vec<_>>();
    let mut topology = topology(
        "test",
        &[(6.0, 53.0), (6.001, 53.0), (6.001, 53.001), (6.002, 53.001)],
        &edges,
    );
    topology.spatial_index = None;
    if restricted {
        topology = with_no_turn(topology, 1, &[0, 1, 2]);
    }
    exact_engine(topology, metrics(&vec![10.0; edges.len()]))
}

fn waypoint(id: &str, lon: f64, lat: f64, kind: WaypointKind) -> Waypoint {
    Waypoint {
        point: point(id, lon, lat),
        kind,
    }
}

fn request(waypoints: Vec<Waypoint>) -> WaypointRequest {
    WaypointRequest {
        route_id: "waypoints".into(),
        waypoints,
        snap: snap_within(1.0),
        returns: ReturnConfig {
            segment_rows: true,
            ..full_geometry()
        },
        optimize_order: false,
        temporal: Default::default(),
    }
}

#[test]
fn through_keeps_direction_at_a_mid_edge_waypoint() {
    let engine = engine(&[(0, 1), (1, 0)], false);
    let mut request = request(vec![
        waypoint("a", 6.0, 53.0, WaypointKind::Break),
        waypoint("mid", 6.0005, 53.0, WaypointKind::Through),
        waypoint("a_again", 6.0, 53.0, WaypointKind::Break),
    ]);
    let through = engine.execute_waypoints(&request).unwrap();
    assert_eq!(through.legs.len(), 1);
    assert_eq!(through.legs[0].edge_path, vec![0, 1]);
    assert_eq!(through.total_generalized_cost, 20.0);
    request.waypoints[1].kind = WaypointKind::Break;
    let breaks = engine.execute_waypoints(&request).unwrap();
    assert_eq!(breaks.legs.len(), 2);
    assert!((breaks.total_generalized_cost - 10.0).abs() < 1e-8);
}

#[test]
fn through_at_a_node_forbids_an_immediate_uturn() {
    let engine = engine(&[(0, 1), (1, 0)], false);
    let mut request = request(vec![
        waypoint("a", 6.0, 53.0, WaypointKind::Break),
        waypoint("b", 6.001, 53.0, WaypointKind::Through),
        waypoint("a_again", 6.0, 53.0, WaypointKind::Break),
    ]);
    assert!(engine.execute_waypoints(&request).is_err());
    request.waypoints[1].kind = WaypointKind::Break;
    assert_eq!(
        engine
            .execute_waypoints(&request)
            .unwrap()
            .total_generalized_cost,
        20.0
    );
}

#[test]
fn through_preserves_multi_edge_turn_restrictions_across_waypoints() {
    let engine = engine(&[(0, 1), (1, 2), (2, 0)], true);
    let mut request = request(vec![
        waypoint("a", 6.0, 53.0, WaypointKind::Break),
        waypoint("b", 6.001, 53.0, WaypointKind::Through),
        waypoint("a_again", 6.0, 53.0, WaypointKind::Break),
    ]);
    assert!(engine.execute_waypoints(&request).is_err());
    request.waypoints[1].kind = WaypointKind::Break;
    assert_eq!(
        engine
            .execute_waypoints(&request)
            .unwrap()
            .total_generalized_cost,
        30.0
    );
}

#[test]
fn repeated_edge_occurrences_apply_partial_costs_only_at_route_ends() {
    let engine = engine(&[(0, 1), (1, 2), (2, 0)], false);
    let request = request(vec![
        waypoint("start", 6.0008, 53.0, WaypointKind::Break),
        waypoint("via", 6.001, 53.001, WaypointKind::Through),
        waypoint("end", 6.0002, 53.0, WaypointKind::Break),
    ]);
    let route = engine.execute_waypoints(&request).unwrap();
    let leg = &route.legs[0];
    assert_eq!(leg.edge_path, vec![0, 1, 2, 0]);
    assert!((route.total_generalized_cost - 24.0).abs() < 1e-8);
    assert_eq!(route.total_distance_m, 240);
    let segments = leg.segments.as_ref().unwrap();
    assert_eq!(
        segments
            .iter()
            .map(|segment| segment.length_m)
            .collect::<Vec<_>>(),
        vec![20, 100, 100, 20]
    );
    let geometry = leg.geometry.as_ref().unwrap();
    assert_eq!(geometry.last().unwrap()[0], 6.0002);
    assert_eq!(geometry[geometry.len() - 2][0], 6.0);
}

#[test]
fn ordered_same_edge_through_points_clip_the_final_geometry() {
    let engine = engine(&[(0, 1)], false);
    let request = request(vec![
        waypoint("start", 6.0002, 53.0, WaypointKind::Break),
        waypoint("via", 6.0007, 53.0, WaypointKind::Through),
        waypoint("end", 6.0009, 53.0, WaypointKind::Break),
    ]);
    let result = engine.execute_waypoints(&request).unwrap();
    assert!((result.total_generalized_cost - 7.0).abs() < 1e-8);
    let geometry = result.legs[0].geometry.as_ref().unwrap();
    assert_eq!(geometry.len(), 2);
    assert_eq!(geometry[1][0], 6.0009);
}

#[test]
fn stop_ordering_uses_directed_costs_and_fixes_the_endpoints() {
    let engine = engine(&[(0, 1), (1, 2), (2, 3)], false);
    let mut request = request(vec![
        waypoint("a", 6.0, 53.0, WaypointKind::Break),
        waypoint("c", 6.001, 53.001, WaypointKind::Break),
        waypoint("b", 6.001, 53.0, WaypointKind::Break),
        waypoint("d", 6.002, 53.001, WaypointKind::Break),
    ]);
    assert!(engine.execute_waypoints(&request).is_err());
    request.optimize_order = true;
    let result = engine.execute_waypoints(&request).unwrap();
    assert_eq!(result.waypoint_order, vec![0, 2, 1, 3]);
    assert_eq!(result.optimization_method, "held_karp_exact");
    assert_eq!(result.total_generalized_cost, 30.0);
    request.waypoints[1].kind = WaypointKind::Through;
    assert!(
        engine
            .execute_waypoints(&request)
            .unwrap_err()
            .to_string()
            .contains("break locations only")
    );
}

#[test]
fn temporal_and_unsnappable_waypoints_are_rejected() {
    let engine = engine(&[(0, 1)], false);
    let mut request = request(vec![
        waypoint("a", 6.0, 53.0, WaypointKind::Break),
        waypoint("b", 6.001, 53.0, WaypointKind::Break),
    ]);
    request.temporal.departure_time = Some("2026-09-09T12:00:00Z".into());
    assert!(
        engine
            .execute_waypoints(&request)
            .unwrap_err()
            .to_string()
            .contains("static costs only")
    );
    request.temporal.departure_time = None;
    request.waypoints[1].point.lon = 40.0;
    assert!(engine.execute_waypoints(&request).is_err());
}
