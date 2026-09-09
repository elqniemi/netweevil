use std::sync::Arc;

use netweevil_core::{
    AccessMask, CompiledEdgeMetric, EdgeId, NodeId, TopologyEdgeLayers, TopologyNode,
    TurnRestriction, TurnRestrictionKind,
};
use netweevil_profile::{ReturnConfig, ReturnGeometry};

use super::fixtures::{test_metrics, test_topology, with_edge_based_topology};
use crate::{
    LabeledPoint, PreparedRoutingEngine, SnapOptions, Waypoint, WaypointKind, WaypointRequest,
};

fn engine(edges: &[(u32, u32)], restricted: bool) -> PreparedRoutingEngine {
    let mut topology = test_topology();
    topology.nodes = [(6.0, 53.0), (6.001, 53.0), (6.001, 53.001), (6.002, 53.001)]
        .into_iter()
        .enumerate()
        .map(|(index, (lon, lat))| TopologyNode {
            node_id: NodeId(index as u32),
            lon,
            lat,
            z: 0.0,
        })
        .collect();
    let template = topology.edge(0);
    let edges = edges
        .iter()
        .enumerate()
        .map(|(index, &(from, to))| {
            let mut edge = template;
            edge.edge_id = EdgeId(index as u32);
            edge.from = NodeId(from);
            edge.to = NodeId(to);
            edge
        })
        .collect::<Vec<_>>();
    topology.edge_layers = TopologyEdgeLayers::from_directed_edges(&edges);
    topology.spatial_index = None;
    if restricted {
        topology.turn_restrictions = vec![TurnRestriction {
            relation_id: 1,
            kind: TurnRestrictionKind::NoTurn,
            edge_path: vec![EdgeId(0), EdgeId(1), EdgeId(2)],
            mode_mask: AccessMask::new(AccessMask::CAR),
        }];
    }
    topology = with_edge_based_topology(topology);
    let mut metrics = test_metrics();
    metrics.edge_metrics = edges
        .iter()
        .map(|edge| CompiledEdgeMetric {
            edge_id: edge.edge_id,
            travel_time_s: Some(10.0),
            generalized_cost: Some(10.0),
        })
        .collect();
    PreparedRoutingEngine::new(Arc::new(topology), Arc::new(metrics), None).unwrap()
}

fn point(id: &str, lon: f64, lat: f64, kind: WaypointKind) -> Waypoint {
    Waypoint {
        point: LabeledPoint {
            id: id.into(),
            lon,
            lat,
            z: None,
        },
        kind,
    }
}

fn request(waypoints: Vec<Waypoint>) -> WaypointRequest {
    WaypointRequest {
        route_id: "waypoints".into(),
        waypoints,
        snap: SnapOptions {
            max_distance_m: 1.0,
            ..Default::default()
        },
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            segment_rows: true,
            ..Default::default()
        },
        optimize_order: false,
        temporal: Default::default(),
    }
}

#[test]
fn through_keeps_direction_at_a_mid_edge_waypoint() {
    let engine = engine(&[(0, 1), (1, 0)], false);
    let mut request = request(vec![
        point("a", 6.0, 53.0, WaypointKind::Break),
        point("mid", 6.0005, 53.0, WaypointKind::Through),
        point("a_again", 6.0, 53.0, WaypointKind::Break),
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
        point("a", 6.0, 53.0, WaypointKind::Break),
        point("b", 6.001, 53.0, WaypointKind::Through),
        point("a_again", 6.0, 53.0, WaypointKind::Break),
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
        point("a", 6.0, 53.0, WaypointKind::Break),
        point("b", 6.001, 53.0, WaypointKind::Through),
        point("a_again", 6.0, 53.0, WaypointKind::Break),
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
        point("start", 6.0008, 53.0, WaypointKind::Break),
        point("via", 6.001, 53.001, WaypointKind::Through),
        point("end", 6.0002, 53.0, WaypointKind::Break),
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
        point("start", 6.0002, 53.0, WaypointKind::Break),
        point("via", 6.0007, 53.0, WaypointKind::Through),
        point("end", 6.0009, 53.0, WaypointKind::Break),
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
        point("a", 6.0, 53.0, WaypointKind::Break),
        point("c", 6.001, 53.001, WaypointKind::Break),
        point("b", 6.001, 53.0, WaypointKind::Break),
        point("d", 6.002, 53.001, WaypointKind::Break),
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
        point("a", 6.0, 53.0, WaypointKind::Break),
        point("b", 6.001, 53.0, WaypointKind::Break),
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
