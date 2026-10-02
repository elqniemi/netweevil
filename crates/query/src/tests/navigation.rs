use netweevil_core::{DirectedEdge, EDGE_FLAG_ROUNDABOUT, TopologyBundle};

use super::fixtures::*;
use crate::{EngineMode, ManeuverType, PreparedRoutingEngine, RouteRequest};

fn request() -> RouteRequest {
    RouteRequest {
        snap: snap_within(1.0),
        ..route_request("directions", point("a", 6.0, 53.0), point("b", 6.002, 53.0))
    }
}

fn engine(topology: TopologyBundle) -> PreparedRoutingEngine {
    exact_engine(topology, test_metrics())
}

/// Moves node 2 north of node 1 so the road bends left at node 1.
fn bent_topology() -> TopologyBundle {
    let mut topology = test_topology();
    topology.nodes[2].lon = 6.001;
    topology.nodes[2].lat = 53.001;
    topology.spatial_index = None;
    topology
}

fn bent_request() -> RouteRequest {
    let mut request = request();
    request.destination.lon = 6.001;
    request.destination.lat = 53.001;
    request
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
    let mut topology = bent_topology();
    topology.set_edge_name_index(0, Some(0));
    topology.set_edge_name_index(1, Some(1));
    let result = engine(topology)
        .execute_directions(
            &bent_request(),
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
    let result = engine(bent_topology())
        .execute_directions(&bent_request(), &[], EngineMode::Auto)
        .unwrap();
    assert_eq!(result.maneuvers.len(), 2);
    assert!(result.maneuvers[1].bearing_before.unwrap().abs() < 0.001);
}

#[test]
fn ending_inside_roundabout_does_not_invent_an_exit() {
    let mut topology = test_topology();
    topology.set_edge_flags(1, EDGE_FLAG_ROUNDABOUT);
    let result = engine(topology)
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
    // Edges 1 and 2 form the roundabout. Edge 4 leaves it at node 2 before
    // the route takes edge 3 at node 3, unless 0 -> 1 -> 4 is restricted.
    let nodes = (0..6)
        .map(|i| (6.0 + f64::from(i) * 0.001, 53.0))
        .collect::<Vec<_>>();
    let edges = [(0, 1), (1, 2), (2, 3), (3, 4), (2, 5)]
        .into_iter()
        .enumerate()
        .map(|(i, (from, to))| DirectedEdge {
            flags: if i == 1 || i == 2 {
                EDGE_FLAG_ROUNDABOUT
            } else {
                0
            },
            ..edge(i as u32, from, to, 10, 100)
        })
        .collect::<Vec<_>>();
    for restricted in [false, true] {
        let mut topology = topology("test", &nodes, &edges);
        topology.spatial_index = None;
        if restricted {
            topology = with_no_turn(topology, 1, &[0, 1, 4]);
        }
        let engine = exact_engine(topology, uniform_metrics(5, 10.0));
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
