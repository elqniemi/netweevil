use std::sync::Arc;

use netweevil_core::{
    AccessMask, CompiledEdgeMetric, EdgeId, TurnRestriction, TurnRestrictionKind,
};
use netweevil_profile::ReturnConfig;

use crate::{PointSetDocument, PreparedRoutingEngine, RouteRequest, SnapOptions};

use super::fixtures::*;

/// Summary-only matrices fold arc summaries instead of unpacking paths and
/// run the restriction-aware A* when the hierarchy path is illegal. Both must
/// agree with independent pairwise routes.
#[test]
fn summary_matrix_with_illegal_hierarchy_paths_matches_pairwise_routes() {
    let topology = multi_edge_restricted_topology();
    let (bundle, metrics) = build_test_cch(&topology, &restricted_metrics());
    let engine =
        PreparedRoutingEngine::new(Arc::new(topology), Arc::new(metrics), Some(bundle)).unwrap();
    let point = |lon: f64| crate::LabeledPoint {
        id: lon.to_string(),
        lon,
        lat: 53.0,
        z: None,
    };
    let point_set = |points| PointSetDocument {
        points,
        snap: SnapOptions {
            max_distance_m: 10.0,
            ..Default::default()
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };
    let origins = point_set(vec![point(6.0), point(6.0005), point(6.0012)]);
    let destinations = point_set(vec![
        point(6.0008),
        point(6.0022),
        point(6.0024),
        point(6.0026),
        point(6.003),
    ]);
    let matrix = engine.execute_matrix(&origins, &destinations).unwrap();
    for (origin_index, origin) in origins.points.iter().enumerate() {
        for (destination_index, destination) in destinations.points.iter().enumerate() {
            let route = engine.execute_route(&RouteRequest {
                route_id: "independent".into(),
                origin: origin.clone(),
                destination: destination.clone(),
                snap: origins.snap.clone(),
                connectivity: Default::default(),
                fallback: Default::default(),
                returns: ReturnConfig::default(),
                alternatives: Default::default(),
                temporal: Default::default(),
            });
            let cell = &matrix.cells[origin_index * destinations.points.len() + destination_index];
            let Ok(route) = route else {
                assert_ne!(cell.status, crate::BatchItemStatus::Succeeded);
                continue;
            };
            assert_eq!(cell.total_distance_m, Some(route.summary.total_distance_m));
            for (matrix_value, route_value) in [
                (
                    cell.total_generalized_cost,
                    route.summary.total_generalized_cost,
                ),
                (cell.total_travel_time_s, route.summary.total_travel_time_s),
            ] {
                assert!((matrix_value.unwrap() - route_value).abs() <= 1e-9);
            }
            assert_eq!(cell.violation_count, 0);
        }
    }
}

/// The lower-bound potential must never cut off the exact restricted
/// optimum: compare A* against the restriction-aware Dijkstra on random
/// directed graphs with zero costs, via-way restrictions, several seeds and
/// negative destination offsets.
#[test]
fn restricted_astar_matches_restricted_dijkstra_on_random_graphs() {
    let mut random = 7_u64;
    let mut next = || {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        random >> 33
    };
    let fallback = crate::FallbackPolicy::default();
    for _ in 0..16 {
        let mut topology = test_topology();
        topology.nodes = (0..8)
            .map(|index| netweevil_core::TopologyNode {
                node_id: netweevil_core::NodeId(index),
                lon: 6.0 + f64::from(index) * 0.001,
                lat: 53.0 + f64::from(index % 3) * 0.001,
                z: 0.0,
            })
            .collect();
        let mut edges = Vec::new();
        let mut metrics = test_metrics();
        metrics.edge_metrics.clear();
        for edge_index in 0..28 {
            let mut edge = topology.edge(0);
            edge.edge_id = EdgeId(edge_index);
            edge.from = netweevil_core::NodeId((next() % 8) as u32);
            edge.to = netweevil_core::NodeId((edge.from.0 + 1 + (next() % 7) as u32) % 8);
            edges.push(edge);
            let cost = (next() % 30) as f64;
            metrics.edge_metrics.push(CompiledEdgeMetric {
                edge_id: EdgeId(edge_index),
                travel_time_s: Some(cost),
                generalized_cost: Some(cost),
            });
        }
        // Forbid a few consecutive three-edge sequences.
        let mut restrictions = Vec::new();
        for _ in 0..40 {
            let first = (next() % 28) as usize;
            let Some(second) =
                (0..28).find(|&e| edges[e].from == edges[first].to && next() % 2 == 0)
            else {
                continue;
            };
            let Some(third) =
                (0..28).find(|&e| edges[e].from == edges[second].to && next() % 2 == 0)
            else {
                continue;
            };
            restrictions.push(TurnRestriction {
                relation_id: restrictions.len() as i64,
                kind: TurnRestrictionKind::NoTurn,
                edge_path: vec![
                    EdgeId(first as u32),
                    EdgeId(second as u32),
                    EdgeId(third as u32),
                ],
                mode_mask: AccessMask::new(AccessMask::CAR),
                condition: None,
            });
            if restrictions.len() == 5 {
                break;
            }
        }
        topology.edge_layers = netweevil_core::TopologyEdgeLayers::from_directed_edges(&edges);
        topology.turn_restrictions = restrictions;
        let topology = with_edge_based_topology(topology);
        let (bundle, compiled) = build_test_cch(&topology, &metrics);
        let compiled = Arc::new(compiled);
        let graph =
            crate::build_routing_graph_from_shared(&topology, compiled.clone(), Some(bundle))
                .unwrap();
        let weights = crate::lower_bound_weights(&graph).expect("lower bounds customize");
        for _ in 0..60 {
            let origins = [
                ((next() % 28) as usize, (next() % 30) as f64),
                ((next() % 28) as usize, (next() % 30) as f64),
            ];
            let destinations = [
                ((next() % 28) as usize, -((next() % 30) as f64)),
                ((next() % 28) as usize, -((next() % 30) as f64)),
            ];
            let mut dijkstra = crate::RestrictedSearchScratch::default();
            dijkstra.prepare(&compiled, &graph, &origins, &fallback);
            let expected = dijkstra
                .route_to(&topology, &compiled, &graph, &destinations, None, &fallback)
                .unwrap()
                .map(|path| path.total_generalized_cost);
            let actual = crate::restricted_astar_route(
                &topology,
                &compiled,
                &graph,
                weights,
                &origins,
                &destinations,
                None,
                &fallback,
            )
            .unwrap();
            assert_eq!(
                actual.as_ref().map(|path| path.total_generalized_cost),
                expected
            );
            // The single-route fallback dispatches to the same A*.
            let single_route = crate::restricted_route_between_edge_seeds(
                &topology,
                &compiled,
                &graph,
                &origins,
                &destinations,
                None,
                &fallback,
            )
            .unwrap();
            assert_eq!(
                single_route.map(|path| path.total_generalized_cost),
                expected
            );
            if let Some(path) = actual {
                assert!(crate::path_respects_restriction_sequences(
                    &graph,
                    &path.edge_indexes
                ));
            }
        }
    }
}
