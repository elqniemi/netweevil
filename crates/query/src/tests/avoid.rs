use serde_json::json;

use crate::{
    AvoidOptions, EngineMode, PreparedRoutingEngine, RouteRequest, RouteResult, resolve_avoid,
};

use super::fixtures::*;

const GRID: u32 = 5;
const STEP_DEG: f64 = 0.001;

/// A 5x5 two-way street grid with varied integer costs; each street block
/// is one source feature (`id = 100 + block`) in both directions.
fn grid() -> (
    netweevil_core::TopologyBundle,
    netweevil_core::CompiledProfileBundle,
) {
    let nodes = (0..GRID * GRID)
        .map(|id| {
            (
                6.0 + f64::from(id % GRID) * STEP_DEG,
                53.0 + f64::from(id / GRID) * STEP_DEG,
            )
        })
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    let mut costs = Vec::new();
    let mut block = 0;
    for id in 0..GRID * GRID {
        let neighbours = [
            (id % GRID + 1 < GRID).then_some(id + 1),
            (id / GRID + 1 < GRID).then_some(id + GRID),
        ];
        for neighbour in neighbours.into_iter().flatten() {
            for (from, to) in [(id, neighbour), (neighbour, id)] {
                edges.push(edge(edges.len() as u32, from, to, 100 + block, 80));
                costs.push(f64::from(5 + (block as u32 * 7 + from) % 11));
            }
            block += 1;
        }
    }
    (topology("avoid-grid", &nodes, &edges), metrics(&costs))
}

fn grid_point(id: &str, x: f64, y: f64) -> crate::LabeledPoint {
    point(id, 6.0 + x * STEP_DEG, 53.0 + y * STEP_DEG)
}

fn route(
    origin: crate::LabeledPoint,
    destination: crate::LabeledPoint,
    avoid: AvoidOptions,
) -> RouteRequest {
    let mut request = route_request("avoid", origin, destination);
    request.snap = snap_within(150.0);
    request.returns = full_geometry();
    request.temporal.avoid = avoid;
    request
}

fn avoid(value: serde_json::Value) -> AvoidOptions {
    serde_json::from_value(value).expect("avoid options parse")
}

/// Deterministic pseudo-random sequence for scenario generation.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn assert_same_outcome(
    label: &str,
    actual: &anyhow::Result<RouteResult>,
    expected: &anyhow::Result<RouteResult>,
) {
    match (actual, expected) {
        (Ok(actual), Ok(expected)) => assert!(
            (actual.summary.total_generalized_cost - expected.summary.total_generalized_cost).abs()
                < 1e-9,
            "{label}: cost {} differs from reference {}",
            actual.summary.total_generalized_cost,
            expected.summary.total_generalized_cost
        ),
        (Err(_), Err(_)) => {}
        (actual, expected) => panic!(
            "{label}: outcome {:?} differs from reference {:?}",
            actual
                .as_ref()
                .map(|route| route.summary.total_generalized_cost)
                .map_err(ToString::to_string),
            expected
                .as_ref()
                .map(|route| route.summary.total_generalized_cost)
                .map_err(ToString::to_string),
        ),
    }
}

/// Avoided routes on the CCH engine (fast path where it applies, otherwise a
/// re-customized avoid engine) cost exactly what an exact engine finds on
/// the metric with excluded edges removed and penalties applied, never use
/// or snap onto an excluded edge, and both paths are exercised.
#[test]
fn avoided_routes_match_an_exact_engine_on_the_reduced_metric() {
    let (topology, metrics) = grid();
    let engine = cch_engine(topology.clone(), &metrics);
    let index_graph = crate::build_routing_graph(&topology, &metrics).unwrap();
    let span = f64::from(GRID - 1);
    let mut random = Lcg(7);
    let (mut fast, mut slow) = (0, 0);
    for case in 0..60 {
        let mut coordinate = || random.next() * span;
        let origin = grid_point("o", coordinate(), coordinate());
        let destination = grid_point("d", coordinate(), coordinate());
        let (x, y, size) = (coordinate(), coordinate(), 0.3 + coordinate() / 4.0);
        let lon = 6.0 + x * STEP_DEG;
        let lat = 53.0 + y * STEP_DEG;
        let half = size * STEP_DEG / 2.0;
        let ring = json!([
            [lon - half, lat - half],
            [lon + half, lat - half],
            [lon + half, lat + half],
            [lon - half, lat + half]
        ]);
        let avoid = match case % 4 {
            0 => {
                avoid(json!({"points": [{"lon": lon, "lat": lat, "radius_m": 20.0 + size * 40.0}]}))
            }
            1 => avoid(json!({"polygons": [{"rings": [ring]}]})),
            2 => avoid(json!({"polygons": [{"rings": [ring], "penalty_factor": 3.0}]})),
            _ => avoid(json!({"source_feature_ids": [100 + (case % 40)]})),
        };
        let request = route(origin, destination, avoid.clone());

        let resolved = resolve_avoid(&topology, &metrics, &index_graph.edge_spatial_index, &avoid)
            .expect("avoid resolves");
        let reference_engine = exact_engine(topology.clone(), resolved.apply(&metrics));
        let mut reference_request = request.clone();
        reference_request.temporal.avoid = AvoidOptions::default();
        let expected = reference_engine.execute_route(&reference_request);

        let actual = engine.execute_route(&request);
        assert_same_outcome(&format!("case {case}"), &actual, &expected);
        if let Ok(result) = &actual {
            for edge in &result.edge_path {
                assert!(
                    !resolved.excludes(*edge as usize),
                    "case {case} uses excluded edge {edge}"
                );
            }
            for snapped in [&result.origin, &result.destination] {
                if let Some(edge) = snapped.snapped_edge_id {
                    assert!(
                        !resolved.excludes(edge as usize),
                        "case {case} snapped onto {edge}"
                    );
                }
            }
        }
        if resolved.is_empty() {
            continue;
        }
        let forced = engine
            .avoid_engine(&resolved, EngineMode::Auto)
            .unwrap()
            .execute_route(&request);
        assert_same_outcome(&format!("case {case} (re-customized)"), &forced, &expected);
        match engine
            .try_route_avoiding(&request, &resolved, None, EngineMode::Auto)
            .unwrap()
        {
            Some(result) => {
                fast += 1;
                assert_same_outcome(&format!("case {case} (fast path)"), &Ok(result), &expected);
            }
            None => slow += 1,
        }
    }
    assert!(fast > 5 && slow > 5, "fast {fast}, re-customized {slow}");
}

/// Matrices, OD documents and service areas run on the avoid engine: matrix
/// cells equal avoided pairwise routes, and service areas never reach an
/// excluded edge.
#[test]
fn batch_analyses_honour_avoid_sets() {
    let (topology, metrics) = grid();
    let engine = cch_engine(topology, &metrics);
    let wall = avoid(
        json!({"polygons": [{"rings": [[[6.0015, 52.9995], [6.0025, 52.9995], [6.0025, 53.0035], [6.0015, 53.0035]]]}]}),
    );
    let resolved = engine
        .resolve_request_avoid(&wall)
        .unwrap()
        .expect("wall avoids edges");

    let points = vec![grid_point("a", 0.0, 0.0), grid_point("b", 0.0, 4.0)];
    let targets = vec![grid_point("c", 4.0, 0.0), grid_point("d", 4.0, 3.0)];
    let mut origins = point_set(points.clone());
    origins.snap = snap_within(150.0);
    origins.temporal.avoid = wall.clone();
    let mut destinations = point_set(targets.clone());
    destinations.snap = snap_within(150.0);
    let matrix = engine.execute_matrix(&origins, &destinations).unwrap();
    for (row, origin) in points.iter().enumerate() {
        for (column, destination) in targets.iter().enumerate() {
            let route = engine
                .execute_route(&route(origin.clone(), destination.clone(), wall.clone()))
                .unwrap();
            let cell = &matrix.cells[row * targets.len() + column];
            assert_eq!(
                cell.total_generalized_cost,
                Some(route.summary.total_generalized_cost)
            );
        }
    }

    let mut od = od_pairs(vec![od_pair("a_c", points[0].clone(), targets[0].clone())]);
    od.snap = snap_within(150.0);
    od.temporal.avoid = wall.clone();
    let unavoided = engine
        .execute_route(&route(
            points[0].clone(),
            targets[0].clone(),
            AvoidOptions::default(),
        ))
        .unwrap();
    let od = engine.execute_od(&od).unwrap();
    assert_eq!(
        od.pairs[0].total_generalized_cost,
        matrix.cells[0].total_generalized_cost
    );
    assert!(od.pairs[0].total_generalized_cost > Some(unavoided.summary.total_generalized_cost));

    let service_area: crate::ServiceAreaRequest = serde_json::from_value(json!({
        "analysis_id": "avoid",
        "origins": [{"id": "a", "lon": 6.0, "lat": 53.0}],
        "thresholds": [{"limit": 10000.0, "metric": "travel_time_s"}],
        "output_mode": "network",
        "returns": {"segments": true},
        "avoid": wall,
    }))
    .unwrap();
    let area = engine.execute_service_area(&service_area).unwrap();
    let reached = area
        .features
        .iter()
        .filter_map(|feature| feature.edge_index)
        .chain(area.segments.iter().map(|segment| segment.edge_index))
        .collect::<Vec<_>>();
    assert!(!reached.is_empty());
    assert!(
        reached
            .iter()
            .all(|&edge| !resolved.excludes(edge as usize))
    );
}

/// Resolution semantics: point radii use the snapper's metric, polygon holes
/// are not part of the area, exclusion beats penalties, and malformed sets
/// are rejected.
#[test]
fn avoid_sets_resolve_to_the_edges_they_touch() {
    let (topology, metrics) = grid();
    let graph = crate::build_routing_graph(&topology, &metrics).unwrap();
    let resolve = |value| {
        resolve_avoid(
            &topology,
            &metrics,
            &graph.edge_spatial_index,
            &avoid(value),
        )
    };
    let edges_between = |a: u32, b: u32| {
        (0..topology.edge_count())
            .filter(|&edge| {
                let edge = topology.routing_edge(edge);
                (edge.from.0, edge.to.0) == (a, b) || (edge.from.0, edge.to.0) == (b, a)
            })
            .collect::<Vec<_>>()
    };

    // Mid-block between nodes 0 and 1, 5 m away: only that block.
    let near =
        resolve(json!({"points": [{"lon": 6.0005, "lat": 53.00004, "radius_m": 6.0}]})).unwrap();
    assert_eq!(
        near.edges
            .iter()
            .map(|&(edge, _)| edge as usize)
            .collect::<Vec<_>>(),
        edges_between(0, 1)
    );

    // A ring around node 12 (grid centre) with a hole around the node: the
    // four blocks crossing the ring are matched, nothing lies wholly in the
    // hole, and the overlapping exclusion wins over the polygon's penalty.
    let (lon, lat) = (6.002, 53.002);
    let square = |half: f64| {
        json!([
            [lon - half, lat - half],
            [lon + half, lat - half],
            [lon + half, lat + half],
            [lon - half, lat + half]
        ])
    };
    let ring = resolve(json!({
        "polygons": [{"rings": [square(0.0004), square(0.0002)], "penalty_factor": 2.0}],
        "points": [{"lon": 6.0024, "lat": lat, "radius_m": 1.0}]
    }))
    .unwrap();
    let mut expected = [7, 11, 13, 17]
        .into_iter()
        .flat_map(|neighbour| edges_between(12, neighbour))
        .collect::<Vec<_>>();
    expected.sort_unstable();
    assert_eq!(
        ring.edges
            .iter()
            .map(|&(edge, _)| edge as usize)
            .collect::<Vec<_>>(),
        expected
    );
    for edge in edges_between(12, 13) {
        assert!(ring.excludes(edge));
    }
    for edge in edges_between(12, 7) {
        assert!(ring.affects(edge) && !ring.excludes(edge));
    }
    let hole_only =
        resolve(json!({"polygons": [{"rings": [square(0.0004), square(0.00039)]}]})).unwrap();
    assert!(hole_only.edges.iter().all(|&(edge, _)| {
        let edge = topology.routing_edge(edge as usize);
        edge.from.0 == 12 || edge.to.0 == 12
    }));

    for invalid in [
        json!({"points": [{"lon": 6.0, "lat": 53.0, "radius_m": 0.0}]}),
        json!({"points": [{"lon": 6.0, "lat": 53.0, "penalty_factor": 0.5}]}),
        json!({"polygons": [{"rings": [[[6.0, 53.0], [6.001, 53.0]]]}]}),
    ] {
        assert!(resolve(invalid.clone()).is_err(), "{invalid}");
    }
    assert!(serde_json::from_value::<AvoidOptions>(json!({"polygon": []})).is_err());
}

/// One-shot entry points have no engine cache and refuse avoid sets rather
/// than ignoring them.
#[test]
fn one_shot_entry_points_refuse_avoid_sets() {
    let (topology, metrics) = grid();
    let request = route(
        grid_point("o", 0.0, 0.0),
        grid_point("d", 4.0, 4.0),
        avoid(json!({"source_feature_ids": [100]})),
    );
    assert!(crate::execute_route(&topology, &metrics, &request).is_err());
    let engine: PreparedRoutingEngine = exact_engine(topology, metrics);
    assert!(engine.execute_route(&request).is_ok());
}
