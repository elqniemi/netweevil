use std::sync::Arc;

use netweevil_core::{EDGE_FLAG_ROUNDABOUT, TopologyBundle, TopologyEdgeLayers};

use super::fixtures::{test_metrics, test_topology, with_edge_based_topology};
use crate::{EngineMode, ManeuverType, PreparedRoutingEngine, RouteRequest};

fn request() -> RouteRequest {
    serde_json::from_value(serde_json::json!({
        "route_id":"directions", "origin":{"id":"a","lon":6.0,"lat":53.0},
        "destination":{"id":"b","lon":6.002,"lat":53.0},
        "snap":{"max_distance_m":1.0}
    }))
    .unwrap()
}

fn engine(topology: TopologyBundle) -> PreparedRoutingEngine {
    PreparedRoutingEngine::new(Arc::new(topology), Arc::new(test_metrics()), None).unwrap()
}

#[test]
fn contiguous_road_has_one_departure_and_arrival_with_complete_edge_ranges() {
    let result = engine(test_topology())
        .execute_directions(&request(), &[], EngineMode::Auto)
        .unwrap();
    assert_eq!(result.route.edge_path, [0, 1]);
    assert_eq!(
        result.maneuvers.iter().map(|m| m.kind).collect::<Vec<_>>(),
        [ManeuverType::Depart, ManeuverType::Arrive]
    );
    assert_eq!(result.maneuvers[0].begin_edge_index, 0);
    assert_eq!(result.maneuvers[0].end_edge_index, 2);
    assert_eq!(
        result.maneuvers[0].distance_m,
        result.route.summary.network_distance_m
    );
    assert_eq!(result.maneuvers[0].edge_time_s, 30.0);
    assert_eq!(result.maneuvers[1].location, [6.002, 53.0]);
}

#[test]
fn named_turn_reports_correct_direction_and_name() {
    let mut topology = test_topology();
    topology.nodes[2].lon = 6.001;
    topology.nodes[2].lat = 53.001;
    topology.spatial_index = None;
    let mut edges = (0..3).map(|i| topology.edge(i)).collect::<Vec<_>>();
    edges[0].name_index = Some(0);
    edges[1].name_index = Some(1);
    topology.edge_layers = TopologyEdgeLayers::from_directed_edges(&edges);
    let mut request = request();
    request.destination.lon = 6.001;
    request.destination.lat = 53.001;
    let result = engine(topology)
        .execute_directions(
            &request,
            &["First Street".into(), "Second Street".into()],
            EngineMode::Auto,
        )
        .unwrap();
    assert_eq!(result.maneuvers[1].kind, ManeuverType::Left);
    assert_eq!(
        result.maneuvers[1].instruction,
        "Turn left on Second Street."
    );
    assert_eq!(result.maneuvers[1].begin_edge_index, 1);
}

#[test]
fn no_false_turn_on_an_unnamed_unbranched_bend() {
    let mut topology = test_topology();
    topology.nodes[2].lon = 6.001;
    topology.nodes[2].lat = 53.001;
    topology.spatial_index = None;
    let mut request = request();
    request.destination.lon = 6.001;
    request.destination.lat = 53.001;
    let result = engine(topology)
        .execute_directions(&request, &[], EngineMode::Auto)
        .unwrap();
    assert_eq!(result.maneuvers.len(), 2);
    assert!(result.maneuvers[1].bearing_before.unwrap().abs() < 0.001);
}

#[test]
fn ending_inside_roundabout_does_not_invent_an_exit() {
    let mut topology = test_topology();
    let mut edges = (0..3).map(|i| topology.edge(i)).collect::<Vec<_>>();
    edges[1].flags |= EDGE_FLAG_ROUNDABOUT;
    topology.edge_layers = TopologyEdgeLayers::from_directed_edges(&edges);
    let result = engine(with_edge_based_topology(topology))
        .execute_directions(&request(), &[], EngineMode::Auto)
        .unwrap();
    assert_eq!(result.maneuvers[1].kind, ManeuverType::EnterRoundabout);
    assert_eq!(result.maneuvers[1].roundabout_exit_count, None);
}

#[test]
fn temporal_directions_are_rejected_explicitly() {
    let mut request = request();
    request.temporal.departure_time = Some("2026-09-09T08:00:00".into());
    let error = engine(test_topology())
        .execute_directions(&request, &[], EngineMode::Auto)
        .unwrap_err();
    assert!(error.to_string().contains("static street route"));
}

#[test]
fn roundabout_exit_count_excludes_a_restricted_exit() {
    use super::fixtures::uniform_metrics;
    use netweevil_core::{
        AccessMask, EdgeId, NodeId, TopologyNode, TurnRestriction, TurnRestrictionKind,
    };
    for restricted in [false, true] {
        let mut topology = test_topology();
        let template = topology.edge(0);
        topology.nodes = (0..6)
            .map(|i| TopologyNode {
                node_id: NodeId(i),
                lon: 6.0 + f64::from(i) * 0.001,
                lat: 53.0,
                z: 0.0,
            })
            .collect();
        let edges = [(0, 1), (1, 2), (2, 3), (3, 4), (2, 5)]
            .into_iter()
            .enumerate()
            .map(|(i, (from, to))| {
                let mut edge = template;
                edge.edge_id = EdgeId(i as u32);
                edge.from = NodeId(from);
                edge.to = NodeId(to);
                edge.flags = if i == 1 || i == 2 {
                    EDGE_FLAG_ROUNDABOUT
                } else {
                    0
                };
                edge
            })
            .collect::<Vec<_>>();
        topology.edge_layers = TopologyEdgeLayers::from_directed_edges(&edges);
        topology.spatial_index = None;
        if restricted {
            topology.turn_restrictions = vec![TurnRestriction {
                relation_id: 1,
                kind: TurnRestrictionKind::NoTurn,
                edge_path: vec![EdgeId(0), EdgeId(1), EdgeId(4)],
                mode_mask: AccessMask::new(AccessMask::CAR),
            }];
        }
        let engine = PreparedRoutingEngine::new(
            Arc::new(with_edge_based_topology(topology)),
            Arc::new(uniform_metrics(5, 10.0)),
            None,
        )
        .unwrap();
        let mut request = request();
        request.destination.lon = 6.004;
        let result = engine
            .execute_directions(&request, &[], EngineMode::Auto)
            .unwrap();
        assert_eq!(result.route.edge_path, [0, 1, 2, 3]);
        assert_eq!(
            result.maneuvers[1].roundabout_exit_count,
            Some(if restricted { 1 } else { 2 })
        );
        assert_eq!(result.maneuvers[2].kind, ManeuverType::ExitRoundabout);
    }
}
