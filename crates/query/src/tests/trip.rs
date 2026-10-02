use netweevil_core::DirectedEdge;

use super::fixtures::{
    cch_engine, edge, exact_engine, metrics, point, route_request, snap_within, topology,
};
use crate::{
    PreparedRoutingEngine, TripMode, TripRequest, TripResult, TripStop, TripTimeWindow, Waypoint,
    WaypointKind, WaypointRequest,
};

const STEP: f64 = 0.001;

fn node_point(x: usize, y: usize) -> (f64, f64) {
    (6.0 + x as f64 * STEP, 53.0 + y as f64 * STEP)
}

/// A two-way `width` x `height` grid of 100 m edges. Eastbound edges cost
/// `10 + y`, westbound 14, northbound 12 and southbound 9, so costs differ
/// by direction and the two directions around a block differ too.
fn grid(width: usize, height: usize) -> (Vec<(f64, f64)>, Vec<DirectedEdge>, Vec<f64>) {
    let nodes = (0..height)
        .flat_map(|y| (0..width).map(move |x| node_point(x, y)))
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    let mut costs = Vec::new();
    let mut connect = |from: usize, to: usize, way: i64, cost: f64| {
        edges.push(edge(edges.len() as u32, from as u32, to as u32, way, 100));
        costs.push(cost);
    };
    for y in 0..height {
        for x in 0..width {
            let here = y * width + x;
            if x + 1 < width {
                connect(here, here + 1, here as i64, 10.0 + y as f64);
                connect(here + 1, here, here as i64, 14.0);
            }
            if y + 1 < height {
                let way = (1_000 + here) as i64;
                connect(here, here + width, way, 12.0);
                connect(here + width, here, way, 9.0);
            }
        }
    }
    (nodes, edges, costs)
}

fn grid_engines(width: usize, height: usize) -> [PreparedRoutingEngine; 2] {
    let (nodes, edges, costs) = grid(width, height);
    let topology = topology("trip-grid", &nodes, &edges);
    let metrics = metrics(&costs);
    [
        exact_engine(topology.clone(), metrics.clone()),
        cch_engine(topology, &metrics),
    ]
}

/// A one-way ring of `count` nodes whose edge `i` costs `10 + i`.
fn ring_engine(count: usize) -> (PreparedRoutingEngine, Vec<(f64, f64)>) {
    let nodes = (0..count)
        .map(|index| {
            let angle = index as f64 / count as f64 * std::f64::consts::TAU;
            (6.0 + 0.003 * angle.cos(), 53.0 + 0.002 * angle.sin())
        })
        .collect::<Vec<_>>();
    let edges = (0..count)
        .map(|index| {
            edge(
                index as u32,
                index as u32,
                ((index + 1) % count) as u32,
                1,
                100,
            )
        })
        .collect::<Vec<_>>();
    let costs = (0..count)
        .map(|index| 10.0 + index as f64)
        .collect::<Vec<_>>();
    (
        exact_engine(topology("trip-ring", &nodes, &edges), metrics(&costs)),
        nodes,
    )
}

fn stop(id: &str, (lon, lat): (f64, f64)) -> TripStop {
    TripStop {
        point: point(id, lon, lat),
        service_s: 0.0,
        time_window: None,
    }
}

fn trip(stops: Vec<TripStop>, mode: TripMode) -> TripRequest {
    TripRequest {
        trip_id: "trip".into(),
        stops,
        mode,
        departure_s: 0.0,
        allow_late: false,
        snap: snap_within(1.0),
        returns: Default::default(),
        optimizer: Default::default(),
    }
}

/// Grid stops in a scrambled input order.
fn grid_stops(cells: &[(usize, usize)]) -> Vec<TripStop> {
    cells
        .iter()
        .enumerate()
        .map(|(index, &(x, y))| stop(&format!("s{index}"), node_point(x, y)))
        .collect()
}

/// Pairwise route costs between request stops.
fn route_costs(engine: &PreparedRoutingEngine, request: &TripRequest) -> Vec<Vec<f64>> {
    let points = request
        .stops
        .iter()
        .map(|stop| stop.point.clone())
        .collect::<Vec<_>>();
    points
        .iter()
        .map(|from| {
            points
                .iter()
                .map(|to| {
                    if from.id == to.id {
                        return 0.0;
                    }
                    engine
                        .execute_route(&route_request("pair", from.clone(), to.clone()))
                        .map_or(f64::INFINITY, |route| route.summary.total_generalized_cost)
                })
                .collect()
        })
        .collect()
}

/// The cheapest visit order allowed by `mode`, by enumeration.
fn brute_force(costs: &[Vec<f64>], mode: TripMode) -> f64 {
    fn permutations(items: Vec<usize>) -> Vec<Vec<usize>> {
        if items.len() <= 1 {
            return vec![items];
        }
        (0..items.len())
            .flat_map(|index| {
                let mut rest = items.clone();
                let head = rest.remove(index);
                permutations(rest).into_iter().map(move |mut tail| {
                    tail.insert(0, head);
                    tail
                })
            })
            .collect()
    }
    let last = costs.len() - 1;
    permutations((0..costs.len()).collect())
        .into_iter()
        .filter_map(|mut order| {
            let valid = match mode {
                TripMode::Roundtrip => {
                    order.push(order[0]);
                    order[0] == 0
                }
                TripMode::FixedStart => order[0] == 0,
                TripMode::FixedEnd => order[last] == last,
                TripMode::FixedEndpoints => order[0] == 0 && order[last] == last,
                TripMode::Open => true,
            };
            valid.then(|| {
                order
                    .windows(2)
                    .map(|pair| costs[pair[0]][pair[1]])
                    .sum::<f64>()
            })
        })
        .fold(f64::INFINITY, f64::min)
}

fn assert_consistent(result: &TripResult, request: &TripRequest) {
    assert_eq!(result.legs.len() + 1, result.stop_order.len());
    assert_eq!(result.visits.len(), result.stop_order.len());
    let mut visited = result.stop_order.clone();
    if request.mode == TripMode::Roundtrip {
        assert_eq!(visited.pop(), Some(0));
    }
    visited.sort_unstable();
    assert_eq!(visited, (0..request.stops.len()).collect::<Vec<_>>());
    let legs_cost = result
        .legs
        .iter()
        .map(|leg| leg.summary.total_generalized_cost)
        .sum::<f64>();
    assert!((legs_cost - result.total_generalized_cost).abs() < 1e-6);
    assert!((result.optimization.cost - result.total_generalized_cost).abs() < 1e-6);
    for (index, leg) in result.legs.iter().enumerate() {
        assert_eq!(leg.route_id, format!("{}:{index}", request.trip_id));
        assert_eq!(
            leg.origin.point_id,
            request.stops[result.stop_order[index]].point.id
        );
    }
}

#[test]
fn every_mode_finds_the_cheapest_directed_order() {
    let cells = [(3, 2), (0, 0), (2, 3), (1, 1), (3, 0), (0, 3), (2, 1)];
    for engine in grid_engines(4, 4) {
        for mode in [
            TripMode::Roundtrip,
            TripMode::FixedStart,
            TripMode::FixedEnd,
            TripMode::FixedEndpoints,
            TripMode::Open,
        ] {
            let request = trip(grid_stops(&cells), mode);
            let result = engine.execute_trip(&request).unwrap();
            assert_consistent(&result, &request);
            let expected = brute_force(&route_costs(&engine, &request), mode);
            assert!(
                (result.total_generalized_cost - expected).abs() < 1e-6,
                "{mode:?}: {} vs {expected}",
                result.total_generalized_cost
            );
            assert_eq!(result.optimization.method, "held_karp_exact");
            assert!(result.optimization.proven_optimal);
            let order = &result.stop_order;
            let last = cells.len() - 1;
            match mode {
                TripMode::Roundtrip => assert!(order[0] == 0 && order[order.len() - 1] == 0),
                TripMode::FixedStart => assert_eq!(order[0], 0),
                TripMode::FixedEnd => assert_eq!(order[order.len() - 1], last),
                TripMode::FixedEndpoints => {
                    assert!(order[0] == 0 && order[order.len() - 1] == last)
                }
                TripMode::Open => {}
            }
        }
    }
}

#[test]
fn local_search_matches_exact_orders_on_both_engines() {
    let cells = [
        (5, 4),
        (0, 0),
        (2, 5),
        (4, 1),
        (1, 3),
        (5, 0),
        (0, 5),
        (3, 3),
        (2, 0),
        (4, 5),
        (1, 1),
        (3, 2),
    ];
    let mut costs = Vec::new();
    for engine in grid_engines(6, 6) {
        let mut request = trip(grid_stops(&cells), TripMode::Roundtrip);
        let exact = engine.execute_trip(&request).unwrap();
        request.optimizer.exact_max_stops = Some(0);
        request.optimizer.seed = 3;
        let heuristic = engine.execute_trip(&request).unwrap();
        assert_consistent(&heuristic, &request);
        assert_eq!(heuristic.optimization.method, "iterated_local_search");
        assert!(!heuristic.optimization.proven_optimal);
        assert!((heuristic.total_generalized_cost - exact.total_generalized_cost).abs() < 1e-6);
        assert!(
            heuristic.optimization.input_order_cost.unwrap() >= heuristic.total_generalized_cost
        );
        let again = engine.execute_trip(&request).unwrap();
        assert_eq!(again.stop_order, heuristic.stop_order);
        costs.push(exact.total_generalized_cost);
    }
    assert!((costs[0] - costs[1]).abs() < 1e-6, "exact and CCH disagree");
}

#[test]
fn one_way_ring_is_visited_in_driving_order() {
    let (engine, nodes) = ring_engine(8);
    // Input order: 0, 5, 2, 7, 3.
    let request = trip(
        [0, 5, 2, 7, 3]
            .iter()
            .map(|&node| stop(&format!("n{node}"), nodes[node]))
            .collect(),
        TripMode::Roundtrip,
    );
    let result = engine.execute_trip(&request).unwrap();
    assert_consistent(&result, &request);
    assert_eq!(result.stop_order, vec![0, 2, 4, 1, 3, 0]);
    // One lap: edges cost 10 + i for i in 0..8.
    assert!((result.total_generalized_cost - 108.0).abs() < 1e-6);
    assert!(result.optimization.input_order_cost.unwrap() > 108.0);
    assert!(result.time_windows_met);
}

#[test]
fn time_windows_reorder_stops_and_report_waiting() {
    let [engine, _] = grid_engines(4, 4);
    let mut stops = grid_stops(&[(0, 0), (3, 0), (3, 3), (0, 3)]);
    let unconstrained = engine
        .execute_trip(&trip(stops.clone(), TripMode::Roundtrip))
        .unwrap();
    // The cheapest loop goes east first and reaches (0, 3) last.
    assert_eq!(unconstrained.stop_order, vec![0, 1, 2, 3, 0]);
    // Going east first reaches (0, 3) at 123 s, after its window closes.
    stops[3].time_window = Some(TripTimeWindow {
        earliest_s: 100.0,
        latest_s: Some(110.0),
    });
    stops[3].service_s = 30.0;
    stops[1].service_s = 15.0;
    let request = trip(stops, TripMode::Roundtrip);
    let result = engine.execute_trip(&request).unwrap();
    assert_consistent(&result, &request);
    assert_eq!(result.stop_order, vec![0, 3, 2, 1, 0]);
    assert!(result.time_windows_met);
    let first = &result.visits[1];
    // Three northbound edges cost 36 s; the stop opens at 100 s.
    assert_eq!(first.stop_index, 3);
    assert!((first.arrival_s - 36.0).abs() < 1e-6);
    assert!((first.wait_s - 64.0).abs() < 1e-6);
    assert!((first.departure_s - 130.0).abs() < 1e-6);
    assert!((result.total_wait_s - 64.0).abs() < 1e-6);
    assert!((result.total_service_s - 45.0).abs() < 1e-6);
    let travel = result.total_travel_time_s;
    assert!((result.end_s - (travel + 64.0 + 45.0)).abs() < 1e-6);
    assert_eq!(result.optimization.method, "held_karp_time_windows_exact");
}

#[test]
fn infeasible_time_windows_fail_clearly_unless_lateness_is_allowed() {
    let [engine, _] = grid_engines(4, 4);
    let mut stops = grid_stops(&[(0, 0), (3, 0), (3, 3), (0, 3)]);
    // (3, 3) is at least 30 + 36 s away; (0, 3) closes after 50 s.
    stops[2].time_window = Some(TripTimeWindow {
        earliest_s: 0.0,
        latest_s: Some(20.0),
    });
    stops[3].time_window = Some(TripTimeWindow {
        earliest_s: 0.0,
        latest_s: Some(50.0),
    });
    let mut request = trip(stops, TripMode::Roundtrip);
    let message = engine.execute_trip(&request).unwrap_err().to_string();
    assert!(
        message.contains("proved that no order meets every time window"),
        "{message}"
    );
    assert!(message.contains("stop 2 ('s2')"), "{message}");
    assert!(
        message.contains("even driving straight from the start cannot reach stop 2 ('s2')"),
        "{message}"
    );
    assert!(message.contains("allow_late"), "{message}");

    request.allow_late = true;
    let result = engine.execute_trip(&request).unwrap();
    assert_consistent(&result, &request);
    assert!(!result.time_windows_met);
    assert!(result.optimization.proven_infeasible);
    assert!(result.late_stops.contains(&2));
    assert!(result.total_lateness_s > 0.0);
    let late = result
        .visits
        .iter()
        .find(|visit| visit.stop_index == 2)
        .unwrap();
    assert!(late.lateness_s >= 66.0 - 20.0 - 1e-6);
}

#[test]
fn unreachable_stops_are_named() {
    let nodes = [(6.0, 53.0), (6.001, 53.0), (6.01, 53.0), (6.011, 53.0)];
    let edges = [
        edge(0, 0, 1, 10, 100),
        edge(1, 1, 0, 10, 100),
        edge(2, 2, 3, 11, 100),
        edge(3, 3, 2, 11, 100),
    ];
    let engine = exact_engine(topology("islands", &nodes, &edges), metrics(&[5.0; 4]));
    let request = trip(
        vec![
            stop("a", (6.0, 53.0)),
            stop("b", (6.001, 53.0)),
            stop("island", (6.011, 53.0)),
        ],
        TripMode::Open,
    );
    let message = engine.execute_trip(&request).unwrap_err().to_string();
    assert!(
        message.contains("no visiting order connects every stop"),
        "{message}"
    );
    assert!(message.contains("stop 2 ('island')"), "{message}");
}

#[test]
fn invalid_requests_are_rejected() {
    let [engine, _] = grid_engines(2, 2);
    let mut request = trip(grid_stops(&[(0, 0)]), TripMode::Roundtrip);
    assert!(engine.execute_trip(&request).is_err());
    request.stops = grid_stops(&[(0, 0), (1, 1)]);
    request.stops[1].time_window = Some(TripTimeWindow {
        earliest_s: 50.0,
        latest_s: Some(10.0),
    });
    let message = engine.execute_trip(&request).unwrap_err().to_string();
    assert!(message.contains("latest_s"), "{message}");
    request.stops[1].time_window = None;
    request.optimizer.exact_max_stops = Some(40);
    assert!(engine.execute_trip(&request).is_err());
    request.optimizer.exact_max_stops = None;
    request.stops[1].point.lon = 7.0;
    let message = engine.execute_trip(&request).unwrap_err().to_string();
    assert!(message.contains("stop 1"), "{message}");
}

#[test]
fn waypoint_ordering_scales_past_exact_search() {
    let (nodes, edges, costs) = grid(6, 4);
    let engine = exact_engine(topology("trip-grid", &nodes, &edges), metrics(&costs));
    let waypoints = (0..20)
        .map(|index| {
            let cell = (index * 7) % 24;
            let (lon, lat) = node_point(cell % 6, cell / 6);
            Waypoint {
                point: point(&format!("w{index}"), lon, lat),
                kind: WaypointKind::Break,
            }
        })
        .collect::<Vec<_>>();
    let request = WaypointRequest {
        route_id: "many".into(),
        waypoints,
        snap: snap_within(1.0),
        returns: Default::default(),
        optimize_order: true,
        temporal: Default::default(),
    };
    let result = engine.execute_waypoints(&request).unwrap();
    assert_eq!(result.optimization_method, "iterated_local_search");
    assert_eq!(result.waypoint_order.first(), Some(&0));
    assert_eq!(result.waypoint_order.last(), Some(&19));
    let input = engine
        .execute_waypoints(&WaypointRequest {
            optimize_order: false,
            ..request.clone()
        })
        .unwrap();
    assert!(result.total_generalized_cost < input.total_generalized_cost);
}
