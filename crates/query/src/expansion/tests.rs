use super::*;
use crate::tests::fixtures::*;

fn request(action: ExpansionAction) -> ExpansionRequest {
    ExpansionRequest {
        action,
        origin: point("origin", 6.0, 53.0),
        destination: None,
        limit: None,
        metric: ServiceAreaThresholdMetric::TravelTimeS,
        direction: ServiceAreaDirection::Outbound,
        snap: snap_within(500.0),
        max_edges: DEFAULT_EXPANSION_MAX_EDGES,
    }
}

/// The recorded search reaches the routed optimum, settles in
/// nondecreasing cost order, and every non-seed edge names a settled
/// predecessor.
#[test]
fn route_expansion_settles_in_order_and_reaches_the_route_cost() {
    let topology = restricted_topology();
    let metrics = restricted_metrics();
    let graph = build_routing_graph(&topology, &metrics).unwrap();
    let mut expansion_request = request(ExpansionAction::Route);
    expansion_request.destination = Some(point("destination", 6.003, 53.0));
    let expansion =
        execute_expansion_with_graph(&topology, &metrics, &graph, &expansion_request).unwrap();

    let route = crate::execute_route(
        &topology,
        &metrics,
        &RouteRequest {
            snap: snap_within(500.0),
            ..route_request(
                "route",
                point("origin", 6.0, 53.0),
                point("destination", 6.003, 53.0),
            )
        },
    )
    .unwrap();
    let route_cost = expansion.route_cost.expect("destination reached");
    assert!(
        (route_cost - route.summary.total_generalized_cost).abs() < 1e-9,
        "expansion {route_cost} vs route {}",
        route.summary.total_generalized_cost
    );
    assert!(!expansion.truncated);
    assert!(
        expansion
            .edges
            .windows(2)
            .all(|pair| pair[0].cost <= pair[1].cost + 1e-12)
    );
    let settled = expansion
        .edges
        .iter()
        .map(|edge| edge.edge_index)
        .collect::<Vec<_>>();
    for edge in &expansion.edges {
        if let Some(previous) = edge.previous_edge_index {
            assert!(settled[..edge.order as usize].contains(&previous));
        }
    }

    expansion_request.max_edges = 1;
    let truncated =
        execute_expansion_with_graph(&topology, &metrics, &graph, &expansion_request).unwrap();
    assert!(truncated.truncated);
    assert_eq!(truncated.edges.len(), 1);
}

/// Inbound expansion labels are the inbound service-area labels.
#[test]
fn inbound_service_area_expansion_matches_inbound_labels() {
    let topology = turn_penalty_topology();
    let metrics = turn_penalty_metrics();
    let graph = build_routing_graph(&topology, &metrics).unwrap();
    let mut expansion_request = request(ExpansionAction::ServiceArea);
    expansion_request.origin = point("anchor", 6.002, 53.001);
    expansion_request.direction = ServiceAreaDirection::Inbound;
    expansion_request.limit = Some(1.0e9);
    let expansion =
        execute_expansion_with_graph(&topology, &metrics, &graph, &expansion_request).unwrap();

    let service_area: ServiceAreaRequest = serde_json::from_value(serde_json::json!({
        "analysis_id": "inbound",
        "origins": [{"id": "anchor", "lon": 6.002, "lat": 53.001}],
        "thresholds": [{"limit": 1.0e9, "metric": "travel_time_s"}],
        "output_mode": "network",
        "band_mode": "none",
        "boundary_mode": "overlap",
        "multi_origin_mode": "overlap",
        "direction": "inbound",
        "returns": {"segments": true}
    }))
    .unwrap();
    let result = crate::execute_service_area(&topology, &metrics, &service_area).unwrap();
    assert!(!expansion.edges.is_empty());
    for expanded in &expansion.edges {
        let segment = result
            .segments
            .iter()
            .find(|segment| segment.edge_index == expanded.edge_index)
            .expect("expanded edge is in the inbound service area");
        assert!(
            (segment.start_cost - expanded.cost).abs() < 1e-9,
            "edge {}: service area {} vs expansion {}",
            expanded.edge_index,
            segment.start_cost,
            expanded.cost
        );
    }
}
