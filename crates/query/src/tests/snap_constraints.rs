use std::collections::BTreeMap;

use super::fixtures::{
    edge, exact_engine, full_geometry, metrics, node, route_request, set_edge_cost, test_metrics,
    test_topology, topology, uniform_metrics, with_edge_based_topology,
};
use crate::*;

/// Two stacked two-way streets along lat 53: nodes 0-2 at ground level and
/// nodes 3-5 directly above them at 20 m. With `one_way`, the odd (westbound)
/// edges are not traversable.
fn engine(one_way: bool) -> PreparedRoutingEngine {
    let nodes = (0..6)
        .map(|index| (6.0 + f64::from(index % 3) * 0.001, 53.0))
        .collect::<Vec<_>>();
    let edges = [
        (0, 1),
        (1, 0),
        (1, 2),
        (2, 1),
        (3, 4),
        (4, 3),
        (4, 5),
        (5, 4),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (from, to))| edge(index as u32, from, to, index as i64 / 2, 100))
    .collect::<Vec<_>>();
    let mut topology = topology("test", &nodes, &edges);
    for node in &mut topology.nodes[3..] {
        node.z = 20.0;
    }
    topology.spatial_index = None;
    let mut metrics = metrics(&[10.0; 8]);
    if one_way {
        for westbound in (1..8).step_by(2) {
            set_edge_cost(&mut metrics, westbound, None);
        }
    }
    exact_engine(topology, metrics)
}

fn point(id: &str, lon: f64, lat: f64) -> LabeledPoint {
    LabeledPoint {
        id: id.into(),
        lon,
        lat,
        z: Some(0.0),
    }
}

fn options(id: &str, constraint: PointSnapConstraint) -> SnapOptions {
    SnapOptions {
        max_distance_m: 20.0,
        z_window_m: Some(1.0),
        point_constraints: BTreeMap::from([(id.into(), constraint)]),
        ..Default::default()
    }
}

fn bearing(degrees: f64) -> PointSnapConstraint {
    PointSnapConstraint {
        bearing: Some(BearingConstraint {
            degrees,
            tolerance_degrees: 1.0,
        }),
        ..Default::default()
    }
}

fn candidate_edges(
    engine: &PreparedRoutingEngine,
    point: &LabeledPoint,
    options: &SnapOptions,
    origin: bool,
) -> Vec<u32> {
    engine
        .snap_route_candidates_with_options(point, options, origin)
        .unwrap()
        .iter()
        .map(|candidate| {
            candidate
                .snapped_edge_id
                .expect("constraint preserves direction")
        })
        .collect()
}

#[test]
fn bearings_preserve_directed_endpoint_seeds_and_stack_elevation() {
    let engine = engine(false);
    let mut point = point("p", 6.0, 53.0);
    let east = options("p", bearing(90.0));
    let west = options("p", bearing(270.0));
    let candidates = engine
        .snap_route_candidates_with_options(&point, &east, true)
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].snapped_edge_id, Some(0));
    assert_eq!(candidates[0].snapped_edge_fraction, Some(0.0));
    assert!(
        engine
            .snap_route_candidates_with_options(&point, &west, true)
            .is_err()
    );
    assert_eq!(candidate_edges(&engine, &point, &west, false), vec![1]);
    assert!(
        engine
            .snap_route_candidates_with_options(&point, &east, false)
            .is_err()
    );
    point.z = Some(20.0);
    assert_eq!(candidate_edges(&engine, &point, &east, true), vec![4]);
    point.lon = 6.0005;
    assert_eq!(candidate_edges(&engine, &point, &east, true), vec![4]);
    assert_eq!(candidate_edges(&engine, &point, &west, false), vec![5]);
}

#[test]
fn curb_opposite_and_driving_side_filter_direction_without_inventing_one_way_edges() {
    let engine = engine(false);
    let mut point = point("p", 6.0005, 52.9999);
    let mut snap = options(
        "p",
        PointSnapConstraint {
            approach: SnapApproach::Curb,
            ..Default::default()
        },
    );
    assert_eq!(candidate_edges(&engine, &point, &snap, true), vec![0]);
    assert_eq!(candidate_edges(&engine, &point, &snap, false), vec![0]);
    snap.point_constraints.get_mut("p").unwrap().approach = SnapApproach::Opposite;
    assert_eq!(candidate_edges(&engine, &point, &snap, true), vec![1]);
    snap.point_constraints.get_mut("p").unwrap().approach = SnapApproach::Curb;
    snap.point_constraints.get_mut("p").unwrap().driving_side = DrivingSide::Left;
    assert_eq!(candidate_edges(&engine, &point, &snap, true), vec![1]);
    point.lat = 53.0;
    let mut edges = candidate_edges(&engine, &point, &snap, true);
    edges.sort_unstable();
    assert_eq!(edges, vec![0, 1]);
    let one_way = self::engine(true);
    assert!(
        one_way
            .snap_route_candidates_with_options(&point, &options("p", bearing(270.0)), true)
            .is_err()
    );
    point.lat = 52.9999;
    assert!(
        one_way
            .snap_route_candidates_with_options(&point, &snap, true)
            .is_err()
    );
}

#[test]
fn snap_constraint_angles_validate_and_wrap_at_true_north() {
    let mut topology = test_topology();
    for (index, node) in topology.nodes.iter_mut().enumerate() {
        node.lon = 6.0;
        node.lat = 53.0 + index as f64 * 0.001;
    }
    topology.spatial_index = None;
    let engine = exact_engine(topology, test_metrics());
    let point = point("p", 6.0, 53.0005);
    for degrees in [0.0, 360.0, 359.5, 0.5] {
        assert!(
            !candidate_edges(&engine, &point, &options("p", bearing(degrees)), true).is_empty()
        );
    }
    for degrees in [-1.0, 361.0, f64::NAN, f64::INFINITY] {
        let error = engine
            .snap_route_candidates_with_options(&point, &options("p", bearing(degrees)), true)
            .unwrap_err();
        assert!(error.to_string().contains("bearing degrees"));
    }
    for tolerance in [-1.0, 181.0, f64::NAN, f64::INFINITY] {
        let mut constraint = bearing(0.0);
        constraint.bearing.as_mut().unwrap().tolerance_degrees = tolerance;
        let error = engine
            .snap_route_candidates_with_options(&point, &options("p", constraint), true)
            .unwrap_err();
        assert!(error.to_string().contains("tolerance_degrees"));
    }
    for tolerance in [-1.0, f64::NAN, f64::INFINITY] {
        let constraint = PointSnapConstraint {
            street_side_tolerance_m: tolerance,
            ..Default::default()
        };
        assert!(
            engine
                .snap_route_candidates_with_options(&point, &options("p", constraint), true)
                .unwrap_err()
                .to_string()
                .contains("street_side_tolerance_m")
        );
    }
}

fn point_set(points: Vec<LabeledPoint>, snap: SnapOptions) -> PointSetDocument {
    PointSetDocument {
        snap,
        ..super::fixtures::point_set(points)
    }
}

fn waypoint_request(
    route_id: &str,
    waypoints: Vec<Waypoint>,
    snap: SnapOptions,
) -> WaypointRequest {
    WaypointRequest {
        route_id: route_id.into(),
        waypoints,
        snap,
        returns: full_geometry(),
        optimize_order: false,
        temporal: Default::default(),
    }
}

fn stop(point: LabeledPoint, kind: WaypointKind) -> Waypoint {
    Waypoint { point, kind }
}

#[test]
fn matrix_presnap_cache_separates_constraints_at_identical_coordinates() {
    let engine = engine(true);
    let mut snap = options("east", bearing(90.0));
    snap.point_constraints.insert("west".into(), bearing(270.0));
    let origins = point_set(
        vec![point("east", 6.0005, 53.0), point("west", 6.0005, 53.0)],
        snap,
    );
    let mut destinations = point_set(
        vec![point("end", 6.0015, 53.0)],
        options("end", bearing(90.0)),
    );
    for departure_time in [None, Some("2026-07-10T10:00:00Z".into())] {
        destinations.temporal.departure_time = departure_time;
        let result = engine.execute_matrix(&origins, &destinations).unwrap();
        assert_eq!(result.cells[0].origin_id, "east");
        assert!((result.cells[0].total_generalized_cost.unwrap() - 10.0).abs() < 1e-8);
        assert_eq!(result.cells[1].origin_id, "west");
        assert_eq!(result.cells[1].status, BatchItemStatus::Failed);
    }
}

#[test]
fn matrix_rejects_shared_ids_with_conflicting_snap_constraints() {
    let engine = engine(false);
    let origins = point_set(vec![point("p", 6.0005, 53.0)], options("p", bearing(90.0)));
    let mut destinations = point_set(vec![point("p", 6.0015, 53.0)], options("p", bearing(270.0)));
    assert!(
        engine
            .execute_matrix(&origins, &destinations)
            .unwrap_err()
            .to_string()
            .contains("conflicting snap constraints")
    );
    destinations.temporal.departure_time = Some("2026-07-10T10:00:00Z".into());
    assert!(
        engine
            .execute_matrix(&origins, &destinations)
            .unwrap_err()
            .to_string()
            .contains("conflicting snap constraints")
    );
}

#[test]
fn waypoint_node_constraints_preserve_arrival_departure_and_through_continuity() {
    let engine = engine(false);
    let mut snap = options("a", bearing(90.0));
    snap.point_constraints.insert("mid".into(), bearing(90.0));
    snap.point_constraints.insert("end".into(), bearing(90.0));
    let mut request = waypoint_request(
        "directed",
        vec![
            stop(point("a", 6.0, 53.0), WaypointKind::Break),
            stop(point("mid", 6.001, 53.0), WaypointKind::Through),
            stop(point("end", 6.002, 53.0), WaypointKind::Break),
        ],
        snap,
    );
    let through = engine.execute_waypoints(&request).unwrap();
    assert_eq!(through.legs[0].edge_path, vec![0, 2]);
    request.waypoints[1].kind = WaypointKind::Break;
    let breaks = engine.execute_waypoints(&request).unwrap();
    assert_eq!(breaks.legs[0].edge_path, vec![0]);
    assert_eq!(breaks.legs[1].edge_path, vec![2]);
    request.optimize_order = true;
    assert_eq!(
        engine
            .execute_waypoints(&request)
            .unwrap()
            .total_generalized_cost,
        20.0
    );
    request.optimize_order = false;
    request.waypoints[1].kind = WaypointKind::Through;
    request.waypoints[2].point.lon = 6.0;
    request
        .snap
        .point_constraints
        .insert("mid".into(), bearing(270.0));
    request
        .snap
        .point_constraints
        .insert("end".into(), bearing(270.0));
    // The only immediate reversal at mid is forbidden. Reaching the far end
    // first is allowed, then the westbound mid heading is physically traversed.
    let detour = engine.execute_waypoints(&request).unwrap();
    assert_eq!(detour.legs[0].edge_path, vec![0, 2, 3, 1]);
}

#[test]
fn route_bearing_constraints_apply_to_both_arrival_and_departure() {
    let engine = engine(false);
    let mut snap = options("start", bearing(90.0));
    snap.point_constraints.insert("end".into(), bearing(90.0));
    let mut request = RouteRequest {
        snap,
        returns: full_geometry(),
        ..route_request(
            "bearing-route",
            point("start", 6.0, 53.0),
            point("end", 6.002, 53.0),
        )
    };
    let route = engine.execute_route(&request).unwrap();
    assert_eq!(route.edge_path, vec![0, 2]);
    request
        .snap
        .point_constraints
        .insert("end".into(), bearing(270.0));
    assert!(
        engine.execute_route(&request).is_err(),
        "arrival cannot claim the departing westbound edge at the final node"
    );
    request
        .snap
        .point_constraints
        .insert("end".into(), bearing(90.0));
    request
        .snap
        .point_constraints
        .insert("start".into(), bearing(270.0));
    assert!(
        engine.execute_route(&request).is_err(),
        "departure cannot claim the arriving westbound edge at the first node"
    );
}

#[test]
fn coincident_waypoints_cannot_use_departure_as_arrival_or_skip_opposite_headings() {
    let engine = engine(false);
    let mut snap = options("start", bearing(90.0));
    for id in ["first", "second", "end"] {
        snap.point_constraints.insert(id.into(), bearing(90.0));
    }
    let mut request = waypoint_request(
        "coincident-breaks",
        [
            ("start", 6.0),
            ("first", 6.001),
            ("second", 6.001),
            ("end", 6.002),
        ]
        .into_iter()
        .map(|(id, lon)| stop(point(id, lon, 53.0), WaypointKind::Break))
        .collect(),
        snap,
    );
    let matching_headings = engine.execute_waypoints(&request).unwrap();
    // At a node the intermediate break offers incoming and outgoing edge
    // candidates. Its outgoing fraction0 must never finish the preceding leg.
    assert_eq!(matching_headings.legs[1].edge_path, vec![2, 3, 1, 0]);
    assert_eq!(
        matching_headings.legs[1].destination.snapped_edge_fraction,
        Some(1.0)
    );
    request
        .snap
        .point_constraints
        .insert("second".into(), bearing(270.0));
    let opposite_headings = engine.execute_waypoints(&request).unwrap();
    assert_eq!(opposite_headings.legs[1].edge_path, vec![2, 3]);
    assert_eq!(opposite_headings.legs[1].origin.snapped_edge_id, Some(2));
    assert_eq!(
        opposite_headings.legs[1].destination.snapped_edge_id,
        Some(3)
    );
    request.waypoints[1].kind = WaypointKind::Through;
    request.waypoints[2].kind = WaypointKind::Through;
    let through = engine.execute_waypoints(&request).unwrap();
    assert_eq!(through.legs[0].edge_path, vec![0, 2, 3, 1, 0, 2]);

    request.waypoints = ["first", "second"]
        .into_iter()
        .map(|id| stop(point(id, 6.0005, 53.0), WaypointKind::Break))
        .collect();
    request
        .snap
        .point_constraints
        .insert("first".into(), bearing(270.0));
    let mut either_direction = bearing(90.0);
    either_direction.bearing.as_mut().unwrap().tolerance_degrees = 180.0;
    request
        .snap
        .point_constraints
        .insert("second".into(), either_direction);
    let stationary = engine.execute_waypoints(&request).unwrap();
    assert!(stationary.legs[0].edge_path.is_empty());
    assert_eq!(stationary.legs[0].origin.snapped_edge_id, Some(1));
    assert_eq!(
        stationary.legs[0].destination.snapped_edge_id,
        Some(1),
        "zero-length result must return the matching candidate, not the first candidate"
    );
}

/// Radius widening must return exactly the candidates of a single scan at
/// the full snap radius, including across spatial-index cell borders.
#[test]
fn widening_snap_radius_matches_a_full_radius_scan() {
    const SIDE: u32 = 24;
    const STEP_DEG: f64 = 0.0005;
    let mut topology = test_topology();
    topology.nodes = (0..SIDE * SIDE)
        .map(|index| {
            node(
                index,
                6.0 + f64::from(index % SIDE) * STEP_DEG,
                53.0 + f64::from(index / SIDE) * STEP_DEG * 0.6,
            )
        })
        .collect();
    let mut edges = Vec::new();
    for index in 0..SIDE * SIDE {
        let (col, row) = (index % SIDE, index / SIDE);
        // Gaps leave some nodes reachable only as node candidates.
        let neighbours = [
            (col + 1 < SIDE && row % 3 != 1, index + 1),
            (row + 1 < SIDE && col % 4 != 2, index + SIDE),
        ];
        for (present, other) in neighbours {
            if !present {
                continue;
            }
            for (from, to) in [(index, other), (other, index)] {
                edges.push(edge(edges.len() as u32, from, to, 10, 100));
            }
        }
    }
    topology.edge_layers = netweevil_core::TopologyEdgeLayers::from_directed_edges(&edges);
    // A 5 x 5 grid of index cells.
    let (min_lon, min_lat) = (6.0, 53.0);
    let cell_deg = STEP_DEG * f64::from(SIDE) / 5.0;
    let mut cells = vec![Vec::new(); 25];
    for node in &topology.nodes {
        let col = (((node.lon - min_lon) / cell_deg).floor() as usize).min(4);
        let row = (((node.lat - min_lat) / cell_deg).floor() as usize).min(4);
        cells[row * 5 + col].push(node.node_id.0);
    }
    let mut node_ids = Vec::new();
    let cells = cells
        .into_iter()
        .map(|ids| {
            let cell = netweevil_core::SpatialIndexCell {
                node_start: node_ids.len() as u32,
                node_len: ids.len() as u32,
            };
            node_ids.extend(ids);
            cell
        })
        .collect();
    topology.spatial_index = Some(netweevil_core::NodeSpatialIndex {
        bounds: netweevil_core::TopologyBounds {
            min_lon,
            min_lat,
            max_lon: min_lon + STEP_DEG * f64::from(SIDE - 1),
            max_lat: min_lat + STEP_DEG * 0.6 * f64::from(SIDE - 1),
        },
        columns: 5,
        rows: 5,
        cell_width_deg: cell_deg,
        cell_height_deg: cell_deg,
        cells,
        node_ids,
    });
    topology = with_edge_based_topology(topology);
    let metrics = uniform_metrics(edges.len(), 10.0);
    let graph = build_routing_graph(&topology, &metrics).unwrap();
    // The reference scans every node, so it also checks the grid window.
    let mut unindexed = topology.clone();
    unindexed.spatial_index = None;

    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1_u64 << 53) as f64
    };
    let key = |candidates: Option<Vec<SnappedPoint>>| {
        candidates.map(|candidates| {
            candidates
                .iter()
                .map(|candidate| {
                    (
                        candidate.snapped_node_id,
                        candidate.snapped_edge_id,
                        candidate.snapped_edge_fraction.map(f64::to_bits),
                        candidate.snap_distance_m.to_bits(),
                    )
                })
                .collect::<Vec<_>>()
        })
    };
    for case in 0..400 {
        let point = LabeledPoint {
            id: format!("p{case}"),
            lon: 5.998 + next() * STEP_DEG * f64::from(SIDE + 8),
            lat: 52.998 + next() * STEP_DEG * 0.6 * f64::from(SIDE + 8),
            z: None,
        };
        for max_distance_m in [0.5, 30.0, 120.0, 500.0] {
            let options = SnapOptions {
                max_distance_m,
                ..Default::default()
            };
            for is_origin in [true, false] {
                let widened =
                    snap_candidates_with_options(&topology, &graph, &point, &options, is_origin);
                let reference = crate::snapping::snap_candidates_single_scan(
                    &unindexed, &graph, &point, &options, is_origin,
                );
                assert_eq!(
                    key(widened.ok()),
                    key(reference.ok()),
                    "case {case} radius {max_distance_m}"
                );
            }
        }
    }
}
