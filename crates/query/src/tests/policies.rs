use std::fs;

use crate::{
    AnalysisDiagnosticCode, AnalysisOutcome, BearingConstraint, ConnectivityPolicy,
    DisconnectedNetworkMode, FallbackPolicy, HopEndpoint, IllegalMovementPenaltyPolicy,
    LabeledPoint, PointSnapConstraint, RouteRequest, RouteViolationType, ServiceAreaBandMode,
    ServiceAreaBoundaryMode, ServiceAreaMultiOriginMode, ServiceAreaOutputMode, ServiceAreaRequest,
    ServiceAreaReturnOptions, ServiceAreaThreshold, ServiceAreaThresholdMetric, analysis_failure,
    execute_od, execute_route, execute_service_area, load_service_area_request,
};
use netweevil_core::CompiledProfileBundle;

use super::fixtures::*;

/// A route between points named "origin" and "destination", given as
/// `(lon, lat)`, snapping within `snap_m` metres.
fn route(route_id: &str, origin: (f64, f64), destination: (f64, f64), snap_m: f64) -> RouteRequest {
    RouteRequest {
        snap: snap_within(snap_m),
        ..route_request(
            route_id,
            point("origin", origin.0, origin.1),
            point("destination", destination.0, destination.1),
        )
    }
}

fn ignore_unreachable() -> ConnectivityPolicy {
    ConnectivityPolicy {
        disconnected: DisconnectedNetworkMode::IgnoreUnreachable,
        ..Default::default()
    }
}

fn hop(disconnected: DisconnectedNetworkMode) -> ConnectivityPolicy {
    ConnectivityPolicy {
        disconnected,
        max_hop_distance_m: Some(120.0),
        report_hop_distance_separately: true,
    }
}

fn travel_time_threshold(id: &str, limit: f64) -> ServiceAreaThreshold {
    ServiceAreaThreshold {
        id: Some(id.to_string()),
        limit,
        metric: ServiceAreaThresholdMetric::TravelTimeS,
    }
}

/// A cumulative, boundary-cutting network service area with default snap.
fn service_area(
    analysis_id: &str,
    origins: Vec<LabeledPoint>,
    thresholds: Vec<ServiceAreaThreshold>,
) -> ServiceAreaRequest {
    ServiceAreaRequest {
        analysis_id: analysis_id.to_string(),
        origins,
        thresholds,
        snap: Default::default(),
        connectivity: Default::default(),
        fallback: Default::default(),
        output_mode: ServiceAreaOutputMode::Network,
        band_mode: ServiceAreaBandMode::Cumulative,
        boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
        multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
        direction: Default::default(),
        polygon: Default::default(),
        returns: Default::default(),
        temporal: Default::default(),
    }
}

fn linear_service_area(request: &ServiceAreaRequest) -> crate::ServiceAreaResult {
    execute_service_area(
        &service_area_linear_topology(),
        &service_area_linear_metrics(),
        request,
    )
    .expect("service area succeeds")
}

#[test]
fn reports_structured_diagnostics_for_component_mismatch() {
    let request = route("disconnected", (6.0, 53.0), (6.01, 53.0), 100.0);

    let error = execute_route(&disconnected_topology(), &disconnected_metrics(), &request)
        .expect_err("route should fail");
    let failure = analysis_failure(&error).expect("failure should be structured");

    assert_eq!(failure.outcome, AnalysisOutcome::Unreachable);
    assert_eq!(failure.diagnostics.len(), 1);
    assert_eq!(
        failure.diagnostics[0].code,
        AnalysisDiagnosticCode::DisconnectedComponents
    );
    assert_eq!(failure.diagnostics[0].component_ids, vec![0, 1]);
}

#[test]
fn returns_partial_route_when_ignore_unreachable_is_enabled() {
    let mut request = route("disconnected-ignore", (6.0, 53.0), (6.01, 53.0), 100.0);
    request.connectivity = ignore_unreachable();
    request.returns = full_geometry();

    let result = execute_route(&disconnected_topology(), &disconnected_metrics(), &request)
        .expect("route returns partial");

    assert_eq!(result.outcome, AnalysisOutcome::Partial);
    assert!(!result.fallback_used);
    assert_eq!(result.summary.total_distance_m, 0);
    assert_eq!(result.summary.total_travel_time_s, 0.0);
    assert!(result.geometry.is_none());
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(
        result.diagnostics[0].code,
        AnalysisDiagnosticCode::DisconnectedComponents
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("ignored an unreachable pair"))
    );
}

#[test]
fn marks_ignored_unreachable_pairs_explicitly_in_od_batches() {
    let document = crate::OdPairsDocument {
        snap: snap_within(100.0),
        connectivity: ignore_unreachable(),
        returns: full_geometry(),
        ..od_pairs(vec![
            od_pair(
                "connected",
                point("origin_a", 6.0, 53.0),
                point("destination_a", 6.001, 53.0),
            ),
            od_pair(
                "ignored",
                point("origin_b", 6.0, 53.0),
                point("destination_b", 6.01, 53.0),
            ),
        ])
    };

    let result = execute_od(&disconnected_topology(), &disconnected_metrics(), &document)
        .expect("OD succeeds");

    assert_eq!(result.succeeded_count, 1);
    assert_eq!(result.ignored_count, 1);
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.pairs[0].status, crate::BatchItemStatus::Succeeded);
    assert_eq!(result.pairs[1].status, crate::BatchItemStatus::Ignored);
    assert_eq!(result.pairs[1].outcome, AnalysisOutcome::Partial);
    assert_eq!(result.pairs[1].total_distance_m, None);
    assert!(result.pairs[1].geometry.is_none());
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("status='ignored'"))
    );
}

#[test]
fn service_area_ignores_unreachable_origins_under_ignore_policy() {
    let mut request = service_area(
        "service-area-ignore",
        vec![point("reachable", 6.0, 53.0), point("too_far", 6.5, 53.5)],
        vec![travel_time_threshold("band", 15.0)],
    );
    request.snap = snap_within(50.0);
    request.connectivity = ignore_unreachable();

    let result = linear_service_area(&request);

    assert_eq!(result.outcome, AnalysisOutcome::Partial);
    assert_eq!(result.processed_origin_count, 1);
    assert_eq!(result.skipped_origin_count, 1);
    assert!(
        result
            .features
            .iter()
            .all(|feature| feature.origin_component_id.is_some())
    );
}

#[test]
fn rejects_service_area_failure_modes_until_supported() {
    let mut request = service_area(
        "service-area-fallback",
        vec![point("a", 6.0, 53.0)],
        vec![ServiceAreaThreshold {
            id: Some("band".to_string()),
            limit: 10.0,
            metric: ServiceAreaThresholdMetric::DistanceM,
        }],
    );
    request.boundary_mode = ServiceAreaBoundaryMode::Overlap;
    request.fallback.allow_reverse_oneway = true;

    let path = write_temp_file(
        "service_area_failure_mode.json",
        &serde_json::to_string_pretty(&request).expect("request serializes"),
    );
    let error = load_service_area_request(&path).expect_err("request should fail validation");
    assert!(
        error
            .to_string()
            .contains("does not support fallback failure modes")
    );
    fs::remove_file(path).ok();
}

#[test]
fn allows_reverse_oneway_when_requested() {
    let topology = one_way_dead_end_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let mut request = route("reverse-oneway", (6.002, 53.0), (6.0, 53.0), 2.0);
    request.fallback = FallbackPolicy {
        allow_reverse_oneway: true,
        penalties: IllegalMovementPenaltyPolicy {
            reverse_oneway_penalty_s: Some(30.0),
            ..Default::default()
        },
        ..Default::default()
    };

    let result = execute_route(&topology, &metrics, &request).expect("degraded route succeeds");
    assert_eq!(result.outcome, AnalysisOutcome::Degraded);
    assert_eq!(result.summary.violation_count, 2);
    assert!(
        result
            .summary
            .violation_types
            .contains(&RouteViolationType::ReverseOneway)
    );
}

#[test]
fn strict_route_still_fails_on_oneway_dead_end() {
    let topology = one_way_dead_end_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let request = route("reverse-oneway-strict", (6.002, 53.0), (6.001, 53.0), 2.0);

    assert!(execute_route(&topology, &metrics, &request).is_err());
}

#[test]
fn auto_relaxes_unreachable_route_with_minimal_reverse_oneway_policy() {
    let topology = one_way_dead_end_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let mut request = route("reverse-oneway-auto", (6.0019, 53.0), (6.001, 53.0), 40.0);
    request.fallback.auto_relax_unreachable = true;

    let result = execute_route(&topology, &metrics, &request).expect("auto route succeeds");
    assert_eq!(result.outcome, AnalysisOutcome::Degraded);
    assert_eq!(
        result.summary.violation_types,
        vec![RouteViolationType::ReverseOneway]
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("allow_reverse_oneway"))
    );
}

fn allow_illegal_turn() -> FallbackPolicy {
    FallbackPolicy {
        allow_illegal_turn: true,
        penalties: IllegalMovementPenaltyPolicy {
            illegal_turn_penalty_s: Some(45.0),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn allows_illegal_turn_when_requested() {
    let topology = illegal_turn_only_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let mut request = route("illegal-turn", (6.0, 53.0), (6.002, 53.0), 40.0);
    request.fallback = allow_illegal_turn();

    let result = execute_route(&topology, &metrics, &request).expect("illegal turn route succeeds");
    assert_eq!(result.outcome, AnalysisOutcome::Degraded);
    assert!(
        result
            .summary
            .violation_types
            .contains(&RouteViolationType::IllegalTurn)
    );
}

#[test]
fn ignores_multi_edge_restriction_only_when_explicitly_requested() {
    let topology = ignored_restriction_only_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let mut request = route(
        "ignored-restriction-strict",
        (6.0, 53.0),
        (6.003, 53.0),
        40.0,
    );
    assert!(execute_route(&topology, &metrics, &request).is_err());

    request.fallback = FallbackPolicy {
        ignore_turn_restrictions: true,
        penalties: IllegalMovementPenaltyPolicy {
            ignored_turn_restriction_penalty_s: Some(60.0),
            ..Default::default()
        },
        ..Default::default()
    };
    let result =
        execute_route(&topology, &metrics, &request).expect("ignored restriction route succeeds");
    assert_eq!(result.outcome, AnalysisOutcome::Degraded);
    assert!(
        result
            .summary
            .violation_types
            .contains(&RouteViolationType::IgnoredTurnRestriction)
    );
}

fn allow_forbidden_uturn() -> FallbackPolicy {
    FallbackPolicy {
        allow_uturn_where_normally_forbidden: true,
        penalties: IllegalMovementPenaltyPolicy {
            forbidden_uturn_penalty_s: Some(15.0),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Failure modes are penalized relaxations: with the u-turn allowed, the
/// legal orientation of the origin edge (19 s) still beats the u-turn route
/// (1 + 10 + 10 s plus the 15 s penalty).
#[test]
fn forbidden_uturn_fallback_keeps_cheaper_legal_route() {
    let topology = forbidden_uturn_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let strict = route("forbidden-uturn", (6.0009, 53.0), (6.0, 53.001), 40.0);
    let legal = execute_route(&topology, &metrics, &strict).expect("legal route succeeds");
    assert_eq!(legal.outcome, AnalysisOutcome::Legal);

    let mut relaxed = strict.clone();
    relaxed.fallback = allow_forbidden_uturn();
    let result = execute_route(&topology, &metrics, &relaxed).expect("route succeeds");
    assert_eq!(result.outcome, AnalysisOutcome::Legal);
    assert!(result.violations.is_empty());
    assert!((result.summary.total_generalized_cost - 19.0).abs() < 1e-6);
    assert!(
        (result.summary.total_generalized_cost - legal.summary.total_generalized_cost).abs() < 1e-9
    );
}

/// A bearing pins the origin to edge 0, so node 2 is only reachable through
/// the forbidden u-turn: strict routing fails, the relaxation degrades.
#[test]
fn forbidden_uturn_fallback_degrades_only_without_legal_route() {
    let topology = forbidden_uturn_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let mut strict = route("forbidden-uturn", (6.0009, 53.0), (6.0, 53.001), 40.0);
    strict.snap.point_constraints.insert(
        "origin".to_string(),
        PointSnapConstraint {
            bearing: Some(BearingConstraint {
                degrees: 90.0,
                tolerance_degrees: 10.0,
            }),
            ..Default::default()
        },
    );
    assert!(execute_route(&topology, &metrics, &strict).is_err());

    let mut relaxed = strict;
    relaxed.fallback = allow_forbidden_uturn();
    let result = execute_route(&topology, &metrics, &relaxed).expect("u-turn route succeeds");
    assert_eq!(result.outcome, AnalysisOutcome::Degraded);
    assert_eq!(
        result.summary.violation_types,
        vec![RouteViolationType::ForbiddenUturn]
    );
    assert!((result.summary.total_generalized_cost - 36.0).abs() < 1e-6);
}

/// The same rule for reverse-oneway travel: a short legal detour beats a
/// penalized reverse traversal, and a cheap enough penalty flips the choice.
#[test]
fn reverse_oneway_fallback_competes_on_penalized_cost() {
    let topology = oneway_with_detour_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let mut request = route("reverse-or-detour", (6.001, 53.0), (6.0, 53.0), 5.0);
    let legal = execute_route(&topology, &metrics, &request).expect("detour is legal");
    assert_eq!(legal.outcome, AnalysisOutcome::Legal);

    for (penalty_s, expected) in [
        (30.0, AnalysisOutcome::Legal),
        (5.0, AnalysisOutcome::Degraded),
    ] {
        request.fallback = FallbackPolicy {
            allow_reverse_oneway: true,
            penalties: IllegalMovementPenaltyPolicy {
                reverse_oneway_penalty_s: Some(penalty_s),
                ..Default::default()
            },
            ..Default::default()
        };
        let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
        assert_eq!(result.outcome, expected, "penalty {penalty_s}");
        let expected_cost = if expected == AnalysisOutcome::Legal {
            legal.summary.total_generalized_cost
        } else {
            10.0 + penalty_s
        };
        assert!((result.summary.total_generalized_cost - expected_cost).abs() < 1e-6);
    }
}

#[test]
fn detects_ferry_only_component_fixture() {
    let topology = ferry_only_subnetwork_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let request = route("ferry-component", (6.0, 53.0), (6.011, 53.0), 40.0);

    let error = execute_route(&topology, &metrics, &request).expect_err("route should fail");
    let failure = analysis_failure(&error).expect("failure should be structured");
    assert_eq!(
        failure.diagnostics[0].code,
        AnalysisDiagnosticCode::DisconnectedComponents
    );
}

#[test]
fn loads_service_area_request_schema() {
    let path = write_temp_file(
        "service-area.json",
        r#"{
  "analysis_id": "sa_demo",
  "origins": [{"id":"o1","lon":6.56,"lat":53.22}],
  "thresholds": [{"id":"five_min","limit":300.0,"metric":"travel_time_s"}],
  "output_mode": "both",
  "band_mode": "cumulative",
  "boundary_mode": "overlap",
  "multi_origin_mode": "merge"
}"#,
    );

    let request = load_service_area_request(&path).expect("service-area request parses");
    fs::remove_file(path).expect("fixture removed");

    assert_eq!(request.analysis_id, "sa_demo");
    assert_eq!(request.origins.len(), 1);
    assert_eq!(request.thresholds.len(), 1);
    assert!(matches!(
        request.thresholds[0].metric,
        ServiceAreaThresholdMetric::TravelTimeS
    ));
}

#[test]
fn executes_service_area_with_partial_edge_frontier() {
    let result = linear_service_area(&service_area(
        "service-area-cut",
        vec![point("origin", 6.0, 53.0)],
        vec![travel_time_threshold("fifteen_s", 15.0)],
    ));

    assert_eq!(result.outcome, AnalysisOutcome::Legal);
    assert_eq!(result.features.len(), 1);
    assert_eq!(result.summaries.len(), 1);
    assert_eq!(result.summaries[0].reachable_edge_count, Some(2));
    assert_eq!(result.summaries[0].reachable_network_length_m, Some(150.0));
    assert_eq!(
        result.features[0].geometry_type,
        crate::ServiceAreaGeometryType::Network
    );
    assert_eq!(
        result.features[0]
            .geometry
            .as_ref()
            .and_then(|value| value.get("type"))
            .and_then(|value| value.as_str()),
        Some("MultiLineString")
    );
}

/// Unbanded 15 s service area from node 0 that returns per-edge segments.
fn unbanded_segment_service_area(analysis_id: &str) -> ServiceAreaRequest {
    let mut request = service_area(
        analysis_id,
        vec![point("origin", 6.0, 53.0)],
        vec![travel_time_threshold("fifteen_s", 15.0)],
    );
    request.band_mode = ServiceAreaBandMode::Unbanded;
    request.returns = ServiceAreaReturnOptions {
        segments: true,
        ..Default::default()
    };
    request
}

#[test]
fn executes_unbanded_service_area_with_segment_costs() {
    let result = linear_service_area(&unbanded_segment_service_area("service-area-segments"));

    assert_eq!(result.features.len(), 2);
    assert_eq!(result.segments.len(), 2);
    assert!(
        result
            .features
            .iter()
            .all(|feature| feature.geometry_type == crate::ServiceAreaGeometryType::Segment)
    );
    assert_eq!(result.segments[0].edge_id, 0);
    assert_eq!(result.segments[0].start_cost, 0.0);
    assert_eq!(result.segments[0].end_cost, 10.0);
    assert_eq!(result.segments[1].edge_id, 1);
    assert_eq!(result.segments[1].end_fraction, 0.25);
    assert_eq!(result.segments[1].end_cost, 15.0);
    assert_eq!(result.segments[1].segment_distance_m, 50.0);
}

fn service_area_component_metrics() -> CompiledProfileBundle {
    let mut metrics = service_area_linear_metrics();
    metrics.components = vec![netweevil_core::CompiledCostComponent {
        weight: 1.0,
        ..cost_component("Exposure", &[4.0, 8.0])
    }];
    metrics
}

fn assert_component_service_area(result: &crate::ServiceAreaResult) {
    let first = result
        .segments
        .iter()
        .find(|segment| segment.edge_id == 0)
        .expect("first edge is emitted");
    assert_eq!(first.start_components.get("Exposure"), Some(&0.0));
    assert_eq!(first.end_components.get("Exposure"), Some(&4.0));

    let frontier = result
        .segments
        .iter()
        .find(|segment| segment.edge_id == 1)
        .expect("frontier edge is emitted");
    assert_eq!(frontier.start_fraction, 0.0);
    assert_eq!(frontier.end_fraction, 0.25);
    assert_eq!(frontier.start_components.get("Exposure"), Some(&4.0));
    assert_eq!(frontier.end_components.get("Exposure"), Some(&6.0));

    let feature = result
        .features
        .iter()
        .find(|feature| feature.edge_id == Some(1))
        .expect("frontier segment feature is emitted");
    assert_eq!(feature.start_components, frontier.start_components);
    assert_eq!(feature.end_components, frontier.end_components);
}

#[test]
fn reports_exact_static_service_area_component_labels() {
    let result = execute_service_area(
        &service_area_linear_topology(),
        &service_area_component_metrics(),
        &unbanded_segment_service_area("service-area-components"),
    )
    .expect("static component service area succeeds");

    assert_component_service_area(&result);
}

#[test]
fn reports_exact_temporal_service_area_component_labels() {
    let mut request = unbanded_segment_service_area("service-area-components");
    request.temporal.departure_time = Some("2026-07-10T08:00:00Z".to_string());
    let result = execute_service_area(
        &service_area_linear_topology(),
        &service_area_component_metrics(),
        &request,
    )
    .expect("temporal component service area succeeds");

    assert_component_service_area(&result);
    assert_eq!(result.departure_time, request.temporal.departure_time);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("exact FIFO Dijkstra"))
    );
}

#[test]
fn rejects_service_area_output_above_geometry_point_limit() {
    let mut request = service_area(
        "service-area-output-limit",
        vec![point("origin", 6.0, 53.0)],
        vec![travel_time_threshold("thirty_s", 30.0)],
    );
    request.boundary_mode = ServiceAreaBoundaryMode::Overlap;
    request.returns.max_geometry_points = 1;

    let error = execute_service_area(
        &service_area_linear_topology(),
        &service_area_linear_metrics(),
        &request,
    )
    .expect_err("geometry point cap should reject large output");

    assert!(error.to_string().contains("max_geometry_points"));
}

#[test]
fn executes_service_area_ring_bands() {
    let mut request = service_area(
        "service-area-ring",
        vec![point("origin", 6.0, 53.0)],
        vec![
            travel_time_threshold("ten_s", 10.0),
            travel_time_threshold("thirty_s", 30.0),
        ],
    );
    request.band_mode = ServiceAreaBandMode::Ring;

    let result = linear_service_area(&request);

    assert_eq!(result.features.len(), 2);
    assert_eq!(result.features[0].reachable_network_length_m, Some(100.0));
    assert_eq!(result.features[1].band_start_limit, Some(10.0));
    assert_eq!(result.features[1].reachable_network_length_m, Some(200.0));
}

#[test]
fn merges_multi_origin_service_areas() {
    let mut request = service_area(
        "service-area-merge",
        vec![point("origin_a", 6.0, 53.0), point("origin_b", 6.001, 53.0)],
        vec![travel_time_threshold("thirty_s", 30.0)],
    );
    request.multi_origin_mode = ServiceAreaMultiOriginMode::Merge;

    let result = linear_service_area(&request);

    assert_eq!(result.features.len(), 1);
    assert_eq!(result.features[0].origin_id, None);
    assert_eq!(result.summaries.len(), 2);
    assert_eq!(result.processed_origin_count, 2);
}

#[test]
fn emits_service_area_polygon_output() {
    let mut request = service_area(
        "service-area-polygon",
        vec![point("origin", 6.0, 53.0)],
        vec![ServiceAreaThreshold {
            id: Some("three_hundred_m".to_string()),
            limit: 300.0,
            metric: ServiceAreaThresholdMetric::DistanceM,
        }],
    );
    request.output_mode = ServiceAreaOutputMode::Both;

    let result = linear_service_area(&request);

    assert_eq!(result.features.len(), 2);
    assert!(result.features.iter().any(|feature| {
        feature.geometry_type == crate::ServiceAreaGeometryType::Polygon
            && feature
                .geometry
                .as_ref()
                .and_then(|value| value.get("type"))
                .and_then(|value| value.as_str())
                .is_some()
    }));
}

#[test]
fn hop_fallback_bridges_components_from_the_requested_end() {
    let topology = hop_disconnected_topology();
    let metrics = disconnected_metrics();
    for (mode, origin_lon, destination_lon, expected_endpoint) in [
        (
            DisconnectedNetworkMode::HopOriginToNearestReachableComponent,
            6.0002,
            6.0015,
            Some(HopEndpoint::Origin),
        ),
        (
            DisconnectedNetworkMode::HopDestinationToNearestReachableComponent,
            6.0010,
            6.0003,
            Some(HopEndpoint::Destination),
        ),
        (DisconnectedNetworkMode::HopEitherEnd, 6.0002, 6.0015, None),
    ] {
        let mut request = route("hop", (origin_lon, 53.0), (destination_lon, 53.0), 40.0);
        request.connectivity = hop(mode);
        request.returns = full_geometry();

        let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
        assert_eq!(result.outcome, AnalysisOutcome::Degraded, "{mode:?}");
        assert!(result.fallback_used, "{mode:?}");
        let Some(endpoint) = expected_endpoint else {
            continue;
        };
        assert_eq!(result.hop_segments.len(), 1);
        assert_eq!(result.hop_segments[0].endpoint, endpoint);
        assert_eq!(
            result.origin_hop_distance_m.is_some(),
            endpoint == HopEndpoint::Origin
        );
        assert_eq!(
            result.destination_hop_distance_m.is_some(),
            endpoint == HopEndpoint::Destination
        );
    }
}

#[test]
fn hop_fallback_keeps_legal_network_cost_unchanged() {
    let topology = hop_disconnected_topology();
    let metrics = disconnected_metrics();
    let legal_request = route("legal", (6.0010, 53.0), (6.0015, 53.0), 40.0);
    let mut hopped_request = route("hopped", (6.0002, 53.0), (6.0015, 53.0), 40.0);
    hopped_request.connectivity =
        hop(DisconnectedNetworkMode::HopOriginToNearestReachableComponent);

    let legal = execute_route(&topology, &metrics, &legal_request).expect("legal route");
    let hopped = execute_route(&topology, &metrics, &hopped_request).expect("hopped route");

    assert_eq!(
        hopped.summary.total_distance_m,
        legal.summary.total_distance_m
    );
    assert_eq!(
        hopped.summary.total_travel_time_s,
        legal.summary.total_travel_time_s
    );
    assert_eq!(
        hopped.summary.total_generalized_cost,
        legal.summary.total_generalized_cost
    );
    assert!(hopped.origin_hop_distance_m.is_some());
}

#[test]
fn skips_non_traversable_nodes_when_snapping_route_endpoints() {
    let topology = dead_node_snap_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let request = route("dead-node-snap", (6.0, 53.0), (6.0002, 53.0), 30.0);

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
    assert_eq!(result.origin.snapped_node_id, 1);
    assert_eq!(result.destination.snapped_node_id, 2);
}

#[test]
fn accelerated_failure_mode_route_matches_dijkstra_route() {
    // Pairwise-only restriction: the failure penalties bake into customized
    // CCH weights, so the accelerated engine must return the same degraded
    // route as the unaccelerated Dijkstra path.
    let topology = illegal_turn_only_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let accelerated_engine = cch_engine(topology.clone(), &metrics);
    let plain_engine = exact_engine(topology, metrics);
    let mut request = route("illegal-turn-accelerated", (6.0, 53.0), (6.002, 53.0), 40.0);
    request.fallback = allow_illegal_turn();

    let plain = plain_engine
        .execute_route(&request)
        .expect("plain degraded route succeeds");
    let accelerated = accelerated_engine
        .execute_route(&request)
        .expect("accelerated degraded route succeeds");

    assert_eq!(plain.outcome, accelerated.outcome);
    assert_eq!(
        plain.summary.violation_types,
        accelerated.summary.violation_types
    );
    assert!(
        (plain.summary.total_generalized_cost - accelerated.summary.total_generalized_cost).abs()
            <= 1e-9,
        "degraded cost differs: {} vs {}",
        plain.summary.total_generalized_cost,
        accelerated.summary.total_generalized_cost
    );
    assert!(
        (plain.summary.total_travel_time_s - accelerated.summary.total_travel_time_s).abs() <= 1e-9
    );
    assert_eq!(
        plain.summary.total_distance_m,
        accelerated.summary.total_distance_m
    );
}
