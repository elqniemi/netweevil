use std::sync::Arc;

use super::fixtures::*;
use crate::*;

fn node_seed(
    topology: &netweevil_core::TopologyBundle,
    node: usize,
    initial_time_s: f64,
) -> ServiceAreaSeed {
    let node = &topology.nodes[node];
    ServiceAreaSeed {
        initial_time_s,
        point: serde_json::from_value(serde_json::json!({
            "point_id": "seed", "requested_lon": node.lon, "requested_lat": node.lat,
            "snapped_node_id": node.node_id.0, "snapped_lon": node.lon, "snapped_lat": node.lat,
            "snap_distance_m": 0
        }))
        .unwrap(),
    }
}

fn network(features: &[ServiceAreaFeature]) -> &serde_json::Value {
    &features
        .iter()
        .find(|f| f.geometry_type == ServiceAreaGeometryType::Network)
        .unwrap()
        .geometry
        .as_ref()
        .unwrap()["coordinates"]
}

#[test]
fn seeded_isochrone_spends_elapsed_time_and_clips_the_last_edge() {
    let topology = Arc::new(service_area_linear_topology());
    let engine = PreparedRoutingEngine::new(
        topology.clone(),
        Arc::new(service_area_linear_metrics()),
        None,
    )
    .unwrap();
    let seeds = [node_seed(&topology, 0, 50.0)];
    let features = engine
        .execute_seeded_service_area("origin", &seeds, 65.0, false, &Default::default())
        .unwrap();
    assert_eq!(features.len(), 2);
    assert_eq!(features[0].reachable_network_length_m, Some(150.0));
    let lines = network(&features);
    assert_eq!(lines.as_array().unwrap().len(), 2);
    assert!((lines[1][1][0].as_f64().unwrap() - 6.00125).abs() < 1e-9);
    assert!(
        features
            .iter()
            .all(|feature| feature.threshold_limit == 65.0)
    );

    let options = ServiceAreaReturnOptions {
        max_geometry_points: 1,
        ..Default::default()
    };
    assert!(
        engine
            .execute_seeded_service_area("origin", &seeds, 65.0, false, &options)
            .is_err()
    );
    let options = ServiceAreaReturnOptions {
        geometry: false,
        ..options
    };
    let features = engine
        .execute_seeded_service_area("origin", &seeds, 65.0, false, &options)
        .unwrap();
    assert!(features.iter().all(|feature| feature.geometry.is_none()));
}

#[test]
fn seeded_isochrone_preserves_disjoint_mid_edge_starts() {
    let topology = Arc::new(service_area_linear_topology());
    let engine = PreparedRoutingEngine::new(
        topology.clone(),
        Arc::new(service_area_linear_metrics()),
        None,
    )
    .unwrap();
    let mut partial = node_seed(&topology, 1, 14.0);
    partial.point.snapped_edge_id = Some(1);
    partial.point.snapped_edge_fraction = Some(0.8);
    let seeds = [
        node_seed(&topology, 0, 0.0),
        partial,
        node_seed(&topology, 1, 12.0),
    ];
    let features = engine
        .execute_seeded_service_area("origin", &seeds, 15.0, false, &Default::default())
        .unwrap();
    let lines = network(&features);
    assert_eq!(lines.as_array().unwrap().len(), 3);
    assert!((lines[1][1][0].as_f64().unwrap() - 6.00125).abs() < 1e-9);
    assert!((lines[2][0][0].as_f64().unwrap() - 6.0018).abs() < 1e-9);
    assert!((lines[2][1][0].as_f64().unwrap() - 6.00185).abs() < 1e-9);
}

#[test]
fn seeded_isochrone_arrive_by_follows_incoming_streets() {
    let topology = Arc::new(service_area_linear_topology());
    let engine = PreparedRoutingEngine::new(
        topology.clone(),
        Arc::new(service_area_linear_metrics()),
        None,
    )
    .unwrap();
    let seeds = [node_seed(&topology, 2, 4.0)];
    assert!(
        engine
            .execute_seeded_service_area("target", &seeds, 19.0, false, &Default::default())
            .unwrap()
            .is_empty()
    );
    let features = engine
        .execute_seeded_service_area("target", &seeds, 19.0, true, &Default::default())
        .unwrap();
    assert_eq!(features[0].reachable_network_length_m, Some(150.0));
    assert!((network(&features)[0][0][0].as_f64().unwrap() - 6.00125).abs() < 1e-9);
    assert_eq!(network(&features)[0][1][0], 6.002);
}

#[test]
fn seeded_isochrone_obeys_multi_edge_turn_restrictions_in_both_directions() {
    let topology = Arc::new(multi_edge_restricted_topology());
    let mut metrics = restricted_metrics();
    for metric in &mut metrics.edge_metrics[3..] {
        metric.travel_time_s = Some(100.0);
        metric.generalized_cost = Some(100.0);
    }
    let engine = PreparedRoutingEngine::new(topology.clone(), Arc::new(metrics), None).unwrap();
    for (node, reverse, excluded_edge) in [(0, false, 2), (3, true, 0)] {
        let features = engine
            .execute_seeded_service_area(
                "origin",
                &[node_seed(&topology, node, 0.0)],
                35.0,
                reverse,
                &Default::default(),
            )
            .unwrap();
        assert_eq!(features[0].reachable_edge_count, Some(4));
        let edge = topology.routing_edge(excluded_edge);
        let from = &topology.nodes[edge.from.0 as usize];
        let to = &topology.nodes[edge.to.0 as usize];
        for line in network(&features).as_array().unwrap() {
            assert!(
                !(line[0][0] == from.lon
                    && line[0][1] == from.lat
                    && line[1][0] == to.lon
                    && line[1][1] == to.lat)
            );
        }
    }
}
