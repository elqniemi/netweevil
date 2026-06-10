use crate::{
    AlternativeRouteOptions, AnalysisKind, EngineMode, OdPair, OdPairsDocument, PointSetDocument,
    PreparedRoutingEngine, RouteRequest, SnapOptions, build_routing_graph, execute_matrix,
    execute_od, execute_route, execute_route_with_edge_names, load_experiment, load_od_pairs,
    load_point_set,
};
use netweevil_core::{
    CacheBundleId, CompiledEdgeMetric, CompiledProfileBundle, CompiledTurnCostConfig, EdgeId,
    TravelMode,
};
use netweevil_profile::{BreakdownMetric, ReturnConfig, ReturnGeometry};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use super::fixtures::*;
use super::fixtures_disconnected::*;

#[test]
fn executes_exact_route_and_breakdowns() {
    let topology = test_topology();
    let metrics = CompiledProfileBundle {
        schema_version: 2,
        profile_id: "test".to_string(),
        profile_hash: "abc".to_string(),
        mode: TravelMode::Car,
        turn_costs: CompiledTurnCostConfig::default(),
        source_topology_bundle_id: CacheBundleId::new("topology-test"),
        acceleration: None,
        edge_metrics: vec![
            CompiledEdgeMetric {
                edge_id: EdgeId(0),
                travel_time_s: Some(10.0),
                generalized_cost: Some(10.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(1),
                travel_time_s: Some(20.0),
                generalized_cost: Some(20.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(2),
                travel_time_s: Some(100.0),
                generalized_cost: Some(100.0),
            },
        ],
    };
    let request = RouteRequest {
        route_id: "route".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            segment_rows: true,
            road_type_breakdown: vec![BreakdownMetric::DistanceM, BreakdownMetric::TimeS],
            surface_breakdown: vec![BreakdownMetric::DistanceM],
            penalty_breakdown: false,
            explain_cost_derivation: false,
        },
        alternatives: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
    assert_eq!(result.edge_path, vec![0, 1]);
    assert_eq!(result.summary.segment_count, 2);
    assert_eq!(result.summary.total_distance_m, 300);
    assert_eq!(result.summary.total_travel_time_s, 30.0);
    assert!(result.geometry.is_some());
    assert!(result.segments.is_some());
    assert_eq!(
        result
            .breakdowns
            .as_ref()
            .and_then(|value| value.road_class.get("residential"))
            .and_then(|value| value.distance_m),
        Some(300)
    );
}

#[test]
fn returns_opt_in_alternative_routes() {
    let topology = test_topology();
    let metrics = test_metrics();
    let request = RouteRequest {
        route_id: "route_alternatives".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: AlternativeRouteOptions {
            max_routes: 2,
            max_cost_ratio: 4.0,
            ..AlternativeRouteOptions::default()
        },
    };

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
    assert_eq!(result.edge_path, vec![0, 1]);
    assert_eq!(result.alternatives.len(), 1);
    assert_eq!(result.alternatives[0].rank, 1);
    assert_eq!(result.alternatives[0].edge_path, vec![2]);
    assert_eq!(result.alternatives[0].summary.total_generalized_cost, 100.0);
}

#[test]
fn uses_external_edge_name_bundle_for_segment_rows() {
    let mut topology = test_topology();
    topology.names.clear();
    topology.set_edge_name_index(0, Some(0));
    topology.set_edge_name_index(1, Some(1));
    let request = RouteRequest {
        route_id: "route".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::None,
            segment_rows: true,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };

    let result = execute_route_with_edge_names(
        &topology,
        &test_metrics(),
        &request,
        &["alpha".to_string(), "beta".to_string()],
    )
    .expect("route succeeds");

    let segments = result.segments.expect("segments requested");
    assert_eq!(segments[0].name.as_deref(), Some("alpha"));
    assert_eq!(segments[1].name.as_deref(), Some("beta"));
}

#[test]
fn prepared_engine_reuses_prebuilt_graph() {
    let engine =
        PreparedRoutingEngine::new(Arc::new(test_topology()), Arc::new(test_metrics()), None)
            .expect("prepared engine builds");
    let request = RouteRequest {
        route_id: "prepared-route".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };

    let route = engine.execute_route(&request).expect("route succeeds");
    assert_eq!(route.edge_path, vec![0, 1]);
    assert_eq!(route.summary.total_distance_m, 300);
}

#[test]
fn accelerated_query_returns_an_unpacked_path() {
    let topology = test_topology();
    let graph = build_routing_graph(&topology, &accelerated_test_metrics()).expect("graph builds");

    let path = crate::accelerated_route_query(&topology, &graph, 0, 2)
        .expect("accelerated query succeeds")
        .expect("accelerated path exists");

    assert_eq!(path.edge_indexes, vec![0, 1]);
    assert_eq!(path.total_generalized_cost, 30.0);
}

#[test]
fn pure_summary_routes_omit_path_payloads() {
    let request = RouteRequest {
        route_id: "summary-route".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
    };

    let route = execute_route(&test_topology(), &test_metrics(), &request).expect("route succeeds");
    assert!(route.node_path.is_empty());
    assert!(route.edge_path.is_empty());
    assert!(route.geometry.is_none());
    assert!(route.segments.is_none());
}

#[test]
fn routes_between_phantom_edge_snaps_with_partial_edge_costs() {
    let request = RouteRequest {
        route_id: "phantom-route".to_string(),
        origin: crate::LabeledPoint {
            id: "a_mid".to_string(),
            lon: 6.0005,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "b_mid".to_string(),
            lon: 6.0015,
            lat: 53.0,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            segment_rows: true,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };

    let route = execute_route(&test_topology(), &test_metrics(), &request).expect("route succeeds");

    assert_eq!(route.edge_path, vec![0, 1]);
    assert_eq!(route.summary.total_distance_m, 150);
    assert!((route.summary.total_travel_time_s - 15.0).abs() < 1.0e-6);
    assert_eq!(route.origin.snapped_edge_id, Some(0));
    assert_eq!(route.destination.snapped_edge_id, Some(1));
    let geometry = route.geometry.expect("geometry requested");
    assert_eq!(geometry.first().copied(), Some([6.0005, 53.0]));
    assert_eq!(geometry.last().copied(), Some([6.0015, 53.0]));
    let segments = route.segments.expect("segments requested");
    assert_eq!(segments[0].length_m, 50);
    assert!((segments[0].travel_time_s - 5.0).abs() < 1.0e-6);
    assert_eq!(segments[1].length_m, 100);
    assert!((segments[1].travel_time_s - 10.0).abs() < 1.0e-6);
}

#[test]
fn rejects_snap_beyond_threshold() {
    let request = RouteRequest {
        route_id: "route".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 0.0,
            lat: 0.0,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
        },
        snap: SnapOptions {
            max_distance_m: 10.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
    };

    let error =
        execute_route(&test_topology(), &test_metrics(), &request).expect_err("snap should fail");
    assert!(
        error
            .to_string()
            .contains("no traversable candidate node or edge within")
    );
}

#[test]
fn snap_candidates_keep_only_the_nearest_eight() {
    let topology = snap_test_topology();
    let metrics = uniform_metrics(topology.edge_count(), 1.0);
    let graph = build_routing_graph(&topology, &metrics).expect("graph builds");
    let point = crate::LabeledPoint {
        id: "snap".to_string(),
        lon: 6.0,
        lat: 53.0,
    };

    let candidates =
        crate::snap_candidates(&topology, &graph, &point, 500.0, true).expect("snap works");

    assert_eq!(candidates.len(), 8);
    assert!(
        candidates
            .windows(2)
            .all(|window| window[0].snap_distance_m <= window[1].snap_distance_m)
    );
    assert_eq!(candidates[0].snapped_node_id, 0);
    assert_eq!(candidates[7].snapped_node_id, 7);
}

#[test]
fn executes_od_batch_with_failures() {
    let document = OdPairsDocument {
        pairs: vec![
            OdPair {
                pair_id: "ok".to_string(),
                origin: crate::LabeledPoint {
                    id: "a".to_string(),
                    lon: 6.0,
                    lat: 53.0,
                },
                destination: crate::LabeledPoint {
                    id: "c".to_string(),
                    lon: 6.002,
                    lat: 53.0,
                },
            },
            OdPair {
                pair_id: "bad".to_string(),
                origin: crate::LabeledPoint {
                    id: "far".to_string(),
                    lon: 0.0,
                    lat: 0.0,
                },
                destination: crate::LabeledPoint {
                    id: "c".to_string(),
                    lon: 6.002,
                    lat: 53.0,
                },
            },
        ],
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };

    let result = execute_od(&test_topology(), &test_metrics(), &document).expect("OD succeeds");
    assert_eq!(result.succeeded_count, 1);
    assert_eq!(result.failed_count, 1);
    assert_eq!(result.pairs[0].total_distance_m, Some(300));
    assert!(
        result.pairs[0]
            .geometry
            .as_ref()
            .is_some_and(|coords| coords.len() >= 2)
    );
    assert_eq!(result.pairs[1].status, crate::BatchItemStatus::Failed);
}

#[test]
fn executes_matrix_batch() {
    let origins = PointSetDocument {
        points: vec![
            crate::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            crate::LabeledPoint {
                id: "b".to_string(),
                lon: 6.001,
                lat: 53.0,
            },
        ],
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };
    let destinations = PointSetDocument {
        points: vec![crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
        }],
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };

    let result = execute_matrix(&test_topology(), &test_metrics(), &origins, &destinations)
        .expect("matrix succeeds");
    assert_eq!(result.cell_count, 2);
    assert_eq!(result.succeeded_count, 2);
    assert_eq!(result.cells[0].total_distance_m, Some(300));
    assert_eq!(result.cells[1].total_distance_m, Some(200));
    assert!(
        result.cells[0]
            .geometry
            .as_ref()
            .is_some_and(|coords| coords.len() >= 2)
    );
}

#[test]
fn executes_matrix_batch_with_presnapped_failures() {
    let origins = PointSetDocument {
        points: vec![
            crate::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            crate::LabeledPoint {
                id: "far".to_string(),
                lon: 0.0,
                lat: 0.0,
            },
        ],
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };
    let destinations = PointSetDocument {
        points: vec![
            crate::LabeledPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
            },
            crate::LabeledPoint {
                id: "b".to_string(),
                lon: 6.001,
                lat: 53.0,
            },
        ],
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };

    let result = execute_matrix(&test_topology(), &test_metrics(), &origins, &destinations)
        .expect("matrix succeeds");

    assert_eq!(result.cell_count, 4);
    assert_eq!(result.succeeded_count, 2);
    assert_eq!(result.failed_count, 2);
    assert_eq!(result.cells[0].status, crate::BatchItemStatus::Succeeded);
    assert_eq!(result.cells[1].status, crate::BatchItemStatus::Succeeded);
    assert_eq!(result.cells[2].status, crate::BatchItemStatus::Failed);
    assert_eq!(result.cells[3].status, crate::BatchItemStatus::Failed);
    assert!(
        result.cells[2]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("no traversable candidate node or edge within"))
    );
}

#[test]
fn matrix_matches_repeated_exact_route_execution() {
    let topology = test_topology();
    let metrics = test_metrics();
    let origins = PointSetDocument {
        points: vec![
            crate::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            crate::LabeledPoint {
                id: "b".to_string(),
                lon: 6.001,
                lat: 53.0,
            },
        ],
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };
    let destinations = PointSetDocument {
        points: vec![
            crate::LabeledPoint {
                id: "b".to_string(),
                lon: 6.001,
                lat: 53.0,
            },
            crate::LabeledPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
            },
        ],
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };

    let matrix =
        execute_matrix(&topology, &metrics, &origins, &destinations).expect("matrix succeeds");

    for (cell, (origin, destination)) in
        matrix
            .cells
            .iter()
            .zip(origins.points.iter().flat_map(|origin| {
                destinations
                    .points
                    .iter()
                    .map(move |destination| (origin, destination))
            }))
    {
        let route = execute_route(
            &topology,
            &metrics,
            &RouteRequest {
                route_id: format!("{}__{}", origin.id, destination.id),
                origin: origin.clone(),
                destination: destination.clone(),
                snap: origins.snap.clone(),
                connectivity: origins.connectivity.clone(),
                fallback: origins.fallback.clone(),
                returns: origins.returns.clone(),
                alternatives: origins.alternatives.clone(),
            },
        )
        .expect("route succeeds");
        assert_eq!(cell.total_distance_m, Some(route.summary.total_distance_m));
        assert_eq!(
            cell.total_travel_time_s,
            Some(route.summary.total_travel_time_s)
        );
        assert_eq!(
            cell.total_generalized_cost,
            Some(route.summary.total_generalized_cost)
        );
        assert_eq!(cell.geometry, route.geometry);
    }
}

#[test]
fn accelerated_engine_matches_exact_engine_on_small_topology() {
    let topology = test_topology();
    let exact_engine =
        PreparedRoutingEngine::new(Arc::new(topology.clone()), Arc::new(test_metrics()), None)
            .expect("exact engine builds");
    let accelerated_engine = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(accelerated_test_metrics()),
        None,
    )
    .expect("accelerated engine builds");

    let requests = [
        RouteRequest {
            route_id: "a_to_c".to_string(),
            origin: crate::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: crate::LabeledPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
            alternatives: Default::default(),
        },
        RouteRequest {
            route_id: "a_to_b".to_string(),
            origin: crate::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: crate::LabeledPoint {
                id: "b".to_string(),
                lon: 6.001,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
            alternatives: Default::default(),
        },
    ];

    for request in requests {
        let exact = exact_engine
            .execute_route(&request)
            .expect("exact route succeeds");
        let accelerated = accelerated_engine
            .execute_route(&request)
            .expect("accelerated route succeeds");
        assert_eq!(
            accelerated.summary.total_distance_m,
            exact.summary.total_distance_m
        );
        assert_eq!(
            accelerated.summary.total_travel_time_s,
            exact.summary.total_travel_time_s
        );
        assert_eq!(
            accelerated.summary.total_generalized_cost,
            exact.summary.total_generalized_cost
        );
        assert_eq!(accelerated.geometry, exact.geometry);
        assert_eq!(accelerated.edge_path, exact.edge_path);
    }
}

#[test]
fn accelerated_engine_falls_back_to_exact_when_shortcuts_miss_pair() {
    let topology = test_topology();
    let engine = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(sparse_acceleration_metrics()),
        None,
    )
    .expect("accelerated engine builds");
    let request = RouteRequest {
        route_id: "a_to_c_sparse_acceleration".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
    };

    let route = engine
        .execute_route(&request)
        .expect("exact fallback route succeeds");
    assert_eq!(route.summary.total_distance_m, 300);
    assert_eq!(route.summary.total_travel_time_s, 30.0);
}

#[test]
fn accelerated_engine_uses_shortcut_route_only_as_exact_upper_bound() {
    let topology = test_topology();
    let engine = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(direct_only_acceleration_metrics()),
        None,
    )
    .expect("accelerated engine builds");
    let request = RouteRequest {
        route_id: "a_to_c_direct_upper_bound".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "c".to_string(),
            lon: 6.002,
            lat: 53.0,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            segment_rows: true,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };

    let route = engine
        .execute_route(&request)
        .expect("exact route succeeds");
    assert_eq!(route.edge_path, vec![0, 1]);
    assert_eq!(route.summary.total_distance_m, 300);
    assert_eq!(route.summary.total_travel_time_s, 30.0);
}

#[test]
fn respects_turn_restrictions() {
    let topology = restricted_topology();
    let metrics = restricted_metrics();
    let request = RouteRequest {
        route_id: "restricted".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "d".to_string(),
            lon: 6.003,
            lat: 53.0,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
    assert_eq!(result.edge_path, vec![3, 2]);
    assert_eq!(result.summary.total_distance_m, 300);
}

#[test]
fn builds_pairwise_turn_table_without_automaton_for_two_edge_restrictions() {
    let graph =
        build_routing_graph(&restricted_topology(), &restricted_metrics()).expect("graph builds");

    assert!(!graph.has_restriction_sequences());
    assert!(!graph.has_edge_transition(1, 2));
    assert!(graph.has_edge_transition(0, 1));
}

#[test]
fn builds_routing_graph_from_persisted_edge_based_topology() {
    let mut topology = restricted_topology();
    topology.edge_based_topology = crate::build_edge_based_topology_fallback(&topology);

    let graph =
        build_routing_graph(&topology, &restricted_metrics()).expect("graph builds from bundle");

    assert!(!graph.has_restriction_sequences());
    assert!(!graph.has_edge_transition(1, 2));
    assert!(graph.has_edge_transition(0, 1));
}

#[test]
fn applies_turn_penalties_when_selecting_routes() {
    let topology = turn_penalty_topology();
    let mut metrics = turn_penalty_metrics();
    let request = RouteRequest {
        route_id: "turn-penalty".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "d".to_string(),
            lon: 6.002,
            lat: 53.001,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };

    let without_penalty =
        execute_route(&topology, &metrics, &request).expect("route without turn penalty");
    assert_eq!(without_penalty.edge_path, vec![0, 1, 2]);
    assert_eq!(without_penalty.summary.total_travel_time_s, 15.0);

    metrics.turn_costs.left_penalty_s = 10.0;
    metrics.turn_costs.right_penalty_s = 5.0;

    let with_penalty =
        execute_route(&topology, &metrics, &request).expect("route with turn penalty");
    assert_eq!(with_penalty.edge_path, vec![3, 4]);
    assert_eq!(with_penalty.summary.total_travel_time_s, 25.0);
}

#[test]
fn applies_traffic_signal_penalties() {
    let topology = traffic_signal_penalty_topology();
    let mut metrics = turn_penalty_metrics();
    let request = RouteRequest {
        route_id: "traffic-signal-penalty".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "d".to_string(),
            lon: 6.002,
            lat: 53.001,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };

    let without_penalty =
        execute_route(&topology, &metrics, &request).expect("route without signal penalty");
    assert_eq!(without_penalty.edge_path, vec![0, 1, 2]);
    assert_eq!(without_penalty.summary.total_travel_time_s, 15.0);

    metrics.turn_costs.traffic_signal_penalty_s = 10.0;

    let with_penalty =
        execute_route(&topology, &metrics, &request).expect("route with signal penalty");
    assert_eq!(with_penalty.edge_path, vec![3, 4]);
    assert_eq!(with_penalty.summary.total_travel_time_s, 20.0);
}

#[test]
fn applies_roundabout_entry_penalties() {
    let topology = roundabout_entry_penalty_topology();
    let mut metrics = roundabout_entry_penalty_metrics();
    let request = RouteRequest {
        route_id: "roundabout-entry-penalty".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "d".to_string(),
            lon: 6.002,
            lat: 53.001,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };

    let without_penalty =
        execute_route(&topology, &metrics, &request).expect("route without roundabout penalty");
    assert_eq!(without_penalty.edge_path, vec![0, 1]);
    assert_eq!(without_penalty.summary.total_travel_time_s, 10.0);

    metrics.turn_costs.roundabout_entry_penalty_s = 11.0;

    let with_penalty =
        execute_route(&topology, &metrics, &request).expect("route with roundabout penalty");
    assert_eq!(with_penalty.edge_path, vec![2, 3]);
    assert_eq!(with_penalty.summary.total_travel_time_s, 12.0);
}

#[test]
fn uses_automaton_for_multi_edge_restriction_sequences() {
    let graph = build_routing_graph(&multi_edge_restricted_topology(), &restricted_metrics())
        .expect("graph builds");

    assert!(graph.has_restriction_sequences());
}

#[test]
fn respects_multi_edge_restriction_sequences() {
    let topology = multi_edge_restricted_topology();
    let metrics = restricted_metrics();
    let request = RouteRequest {
        route_id: "multi-edge-restricted".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "d".to_string(),
            lon: 6.003,
            lat: 53.0,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
    assert_eq!(result.edge_path, vec![3, 2]);
    assert_eq!(result.summary.total_distance_m, 300);
}

#[test]
fn alternative_routes_fall_back_to_restricted_search_when_needed() {
    let topology = multi_edge_restricted_topology();
    let metrics = restricted_metrics();
    let request = RouteRequest {
        route_id: "multi-edge-restricted-alternative".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "d".to_string(),
            lon: 6.003,
            lat: 53.0,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: AlternativeRouteOptions {
            max_routes: 2,
            max_cost_ratio: 2.0,
            ..AlternativeRouteOptions::default()
        },
    };

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");

    assert_eq!(result.edge_path, vec![3, 2]);
    assert_eq!(result.alternatives.len(), 1);
    assert_eq!(result.alternatives[0].edge_path, vec![0, 1, 4]);
}

#[test]
fn detects_when_a_path_violates_multi_edge_restriction_sequences() {
    let graph = build_routing_graph(&multi_edge_restricted_topology(), &restricted_metrics())
        .expect("graph builds");

    assert!(!crate::path_respects_restriction_sequences(
        &graph,
        &[0, 1, 2]
    ));
    assert!(crate::path_respects_restriction_sequences(&graph, &[3, 2]));
}

#[test]
fn can_ignore_multi_edge_restriction_sequences_via_engine_mode() {
    let topology = multi_edge_restricted_topology();
    let mut metrics = restricted_metrics();
    metrics.edge_metrics[3].travel_time_s = Some(25.0);
    metrics.edge_metrics[3].generalized_cost = Some(25.0);
    let engine = PreparedRoutingEngine::new(Arc::new(topology), Arc::new(metrics), None)
        .expect("prepared engine builds");
    let request = RouteRequest {
        route_id: "multi-edge-override".to_string(),
        origin: crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: crate::LabeledPoint {
            id: "d".to_string(),
            lon: 6.003,
            lat: 53.0,
        },
        snap: SnapOptions {
            max_distance_m: 500.0,
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
    };

    let exact = engine
        .execute_route_with_mode(&request, EngineMode::Auto)
        .expect("exact route succeeds");
    let override_route = engine
        .execute_route_with_mode(&request, EngineMode::IgnoreMultiEdgeRestrictions)
        .expect("override route succeeds");
    let override_engine =
        engine.effective_engine_description(EngineMode::IgnoreMultiEdgeRestrictions);

    assert_eq!(exact.edge_path, vec![3, 2]);
    assert_eq!(override_route.edge_path, vec![0, 1, 2]);
    assert_eq!(
        override_engine.route_engine,
        "bidirectional_exact_pairwise_turns"
    );
}

#[test]
fn loads_od_pairs_from_csv() {
    let path = write_temp_file(
        "od_pairs.csv",
        "id,source_x,source_y,target_x,target_y\npair_1,6.1,53.1,6.2,53.2\n",
    );

    let document = load_od_pairs(&path).expect("CSV loads");

    assert_eq!(document.pairs.len(), 1);
    assert_eq!(document.pairs[0].pair_id, "pair_1");
    assert_eq!(document.pairs[0].origin.id, "pair_1:source");
    assert_eq!(document.pairs[0].destination.id, "pair_1:target");
    assert_eq!(document.pairs[0].origin.lon, 6.1);
    assert_eq!(document.pairs[0].destination.lat, 53.2);
    fs::remove_file(path).ok();
}

#[test]
fn loads_point_set_from_csv() {
    let path = write_temp_file("points.csv", "id,x,y\na,6.1,53.1\nb,6.2,53.2\n");

    let document = load_point_set(&path).expect("CSV loads");

    assert_eq!(document.points.len(), 2);
    assert_eq!(document.points[0].id, "a");
    assert_eq!(document.points[1].lon, 6.2);
    assert_eq!(document.points[1].lat, 53.2);
    fs::remove_file(path).ok();
}

#[test]
fn loads_experiment_with_route_and_matrix_scenarios() {
    let path = write_temp_file(
        "experiment.yml",
        r#"
experiment:
  id: baseline_sweep
  label: Baseline Sweep
  dataset: groningen_2026_03
scenarios:
  - id: baseline_route
    profile: ../profiles/car_research_v1.yml
    analysis: route
    request: ../requests/route.json
  - id: baseline_matrix
    profile: ../profiles/car_research_v1.yml
    analysis: matrix
    origins: ../requests/matrix_origins.csv
    destinations: ../requests/matrix_destinations.csv
    out: exports/matrix.csv
"#,
    );

    let document = load_experiment(&path).expect("experiment loads");

    assert_eq!(document.experiment.id, "baseline_sweep");
    assert_eq!(document.scenarios.len(), 2);
    assert_eq!(document.scenarios[0].analysis, AnalysisKind::Route);
    assert_eq!(
        document.scenarios[1].origins.as_deref(),
        Some(PathBuf::from("../requests/matrix_origins.csv").as_path())
    );
    assert_eq!(
        document.scenarios[1].destinations.as_deref(),
        Some(PathBuf::from("../requests/matrix_destinations.csv").as_path())
    );
    fs::remove_file(path).ok();
}
