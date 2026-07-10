use crate::{
    AnalysisDiagnosticCode, ConnectivityPolicy, DisconnectedNetworkMode, FallbackPolicy,
    IllegalMovementPenaltyPolicy, OdPair, OdPairsDocument, RouteRequest, ServiceAreaBandMode,
    ServiceAreaBoundaryMode, ServiceAreaMultiOriginMode, ServiceAreaOutputMode,
    ServiceAreaReturnOptions, ServiceAreaThreshold, ServiceAreaThresholdMetric, SnapOptions,
    analysis_failure, execute_od, execute_route, execute_service_area, load_service_area_request,
};

use netweevil_core::CompiledCostComponent;
use netweevil_profile::{ReturnConfig, ReturnGeometry};
use std::fs;

use super::fixtures::*;
use super::fixtures_disconnected::*;

#[test]
fn reports_structured_diagnostics_for_component_mismatch() {
    let topology = disconnected_topology();
    let metrics = disconnected_metrics();
    let request = RouteRequest {
        route_id: "disconnected".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.01,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 100.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let error = execute_route(&topology, &metrics, &request).expect_err("route should fail");
    let failure = analysis_failure(&error).expect("failure should be structured");

    assert_eq!(failure.outcome, crate::AnalysisOutcome::Unreachable);
    assert_eq!(failure.diagnostics.len(), 1);
    assert_eq!(
        failure.diagnostics[0].code,
        AnalysisDiagnosticCode::DisconnectedComponents
    );
    assert_eq!(failure.diagnostics[0].component_ids, vec![0, 1]);
}

#[test]
fn returns_partial_route_when_ignore_unreachable_is_enabled() {
    let topology = disconnected_topology();
    let metrics = disconnected_metrics();
    let request = RouteRequest {
        route_id: "disconnected-ignore".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.01,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 100.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: ConnectivityPolicy {
            disconnected: DisconnectedNetworkMode::IgnoreUnreachable,
            ..ConnectivityPolicy::default()
        },
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("route returns partial");

    assert_eq!(result.outcome, crate::AnalysisOutcome::Partial);
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
    let topology = disconnected_topology();
    let metrics = disconnected_metrics();
    let document = OdPairsDocument {
        pairs: vec![
            OdPair {
                pair_id: "connected".to_string(),
                origin: crate::LabeledPoint {
                    id: "origin_a".to_string(),
                    lon: 6.0,
                    lat: 53.0,
                    z: None,
                },
                destination: crate::LabeledPoint {
                    id: "destination_a".to_string(),
                    lon: 6.001,
                    lat: 53.0,
                    z: None,
                },
            },
            OdPair {
                pair_id: "ignored".to_string(),
                origin: crate::LabeledPoint {
                    id: "origin_b".to_string(),
                    lon: 6.0,
                    lat: 53.0,
                    z: None,
                },
                destination: crate::LabeledPoint {
                    id: "destination_b".to_string(),
                    lon: 6.01,
                    lat: 53.0,
                    z: None,
                },
            },
        ],
        snap: SnapOptions {
            max_distance_m: 100.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: ConnectivityPolicy {
            disconnected: DisconnectedNetworkMode::IgnoreUnreachable,
            ..ConnectivityPolicy::default()
        },
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_od(&topology, &metrics, &document).expect("OD succeeds");

    assert_eq!(result.succeeded_count, 1);
    assert_eq!(result.ignored_count, 1);
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.pairs[0].status, crate::BatchItemStatus::Succeeded);
    assert_eq!(result.pairs[1].status, crate::BatchItemStatus::Ignored);
    assert_eq!(result.pairs[1].outcome, crate::AnalysisOutcome::Partial);
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
    let topology = service_area_linear_topology();
    let metrics = service_area_linear_metrics();
    let request = crate::ServiceAreaRequest {
        analysis_id: "service-area-ignore".to_string(),
        origins: vec![
            crate::LabeledPoint {
                id: "reachable".to_string(),
                lon: 6.0,
                lat: 53.0,
                z: None,
            },
            crate::LabeledPoint {
                id: "too_far".to_string(),
                lon: 6.5,
                lat: 53.5,
                z: None,
            },
        ],
        thresholds: vec![ServiceAreaThreshold {
            id: Some("band".to_string()),
            limit: 15.0,
            metric: ServiceAreaThresholdMetric::TravelTimeS,
        }],
        snap: SnapOptions {
            max_distance_m: 50.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: ConnectivityPolicy {
            disconnected: DisconnectedNetworkMode::IgnoreUnreachable,
            ..ConnectivityPolicy::default()
        },
        fallback: Default::default(),
        output_mode: ServiceAreaOutputMode::Network,
        band_mode: ServiceAreaBandMode::Cumulative,
        boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
        multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
        polygon: Default::default(),
        returns: Default::default(),
        temporal: Default::default(),
    };

    let result =
        execute_service_area(&topology, &metrics, &request).expect("service area succeeds");

    assert_eq!(result.outcome, crate::AnalysisOutcome::Partial);
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
    let request = crate::ServiceAreaRequest {
        analysis_id: "service-area-fallback".to_string(),
        origins: vec![crate::LabeledPoint {
            id: "a".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        }],
        thresholds: vec![ServiceAreaThreshold {
            id: Some("band".to_string()),
            limit: 10.0,
            metric: ServiceAreaThresholdMetric::DistanceM,
        }],
        snap: Default::default(),
        connectivity: Default::default(),
        fallback: FallbackPolicy {
            allow_reverse_oneway: true,
            ..FallbackPolicy::default()
        },
        output_mode: ServiceAreaOutputMode::Network,
        band_mode: ServiceAreaBandMode::Cumulative,
        boundary_mode: ServiceAreaBoundaryMode::Overlap,
        multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
        polygon: Default::default(),
        returns: Default::default(),
        temporal: Default::default(),
    };

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
    let request = RouteRequest {
        route_id: "reverse-oneway".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 2.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: FallbackPolicy {
            allow_reverse_oneway: true,
            penalties: IllegalMovementPenaltyPolicy {
                reverse_oneway_penalty_s: Some(30.0),
                ..IllegalMovementPenaltyPolicy::default()
            },
            ..FallbackPolicy::default()
        },
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("degraded route succeeds");
    assert_eq!(result.outcome, crate::AnalysisOutcome::Degraded);
    assert_eq!(result.summary.violation_count, 2);
    assert!(
        result
            .summary
            .violation_types
            .contains(&crate::RouteViolationType::ReverseOneway)
    );
}

#[test]
fn strict_route_still_fails_on_oneway_dead_end() {
    let topology = one_way_dead_end_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let request = RouteRequest {
        route_id: "reverse-oneway-strict".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.001,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 2.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    assert!(execute_route(&topology, &metrics, &request).is_err());
}

#[test]
fn auto_relaxes_unreachable_route_with_minimal_reverse_oneway_policy() {
    let topology = one_way_dead_end_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let request = RouteRequest {
        route_id: "reverse-oneway-auto".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0019,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.001,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 40.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: FallbackPolicy {
            auto_relax_unreachable: true,
            ..FallbackPolicy::default()
        },
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("auto route succeeds");
    assert_eq!(result.outcome, crate::AnalysisOutcome::Degraded);
    assert_eq!(
        result.summary.violation_types,
        vec![crate::RouteViolationType::ReverseOneway]
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("allow_reverse_oneway"))
    );
}

#[test]
fn allows_illegal_turn_when_requested() {
    let topology = illegal_turn_only_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let request = RouteRequest {
        route_id: "illegal-turn".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 40.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: FallbackPolicy {
            allow_illegal_turn: true,
            penalties: IllegalMovementPenaltyPolicy {
                illegal_turn_penalty_s: Some(45.0),
                ..IllegalMovementPenaltyPolicy::default()
            },
            ..FallbackPolicy::default()
        },
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("illegal turn route succeeds");
    assert_eq!(result.outcome, crate::AnalysisOutcome::Degraded);
    assert!(
        result
            .summary
            .violation_types
            .contains(&crate::RouteViolationType::IllegalTurn)
    );
}

#[test]
fn ignores_multi_edge_restriction_only_when_explicitly_requested() {
    let topology = ignored_restriction_only_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let strict_request = RouteRequest {
        route_id: "ignored-restriction-strict".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.003,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 40.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };
    assert!(execute_route(&topology, &metrics, &strict_request).is_err());

    let degraded_request = RouteRequest {
        fallback: FallbackPolicy {
            ignore_turn_restrictions: true,
            penalties: IllegalMovementPenaltyPolicy {
                ignored_turn_restriction_penalty_s: Some(60.0),
                ..IllegalMovementPenaltyPolicy::default()
            },
            ..FallbackPolicy::default()
        },
        ..strict_request
    };
    let result = execute_route(&topology, &metrics, &degraded_request)
        .expect("ignored restriction route succeeds");
    assert_eq!(result.outcome, crate::AnalysisOutcome::Degraded);
    assert!(
        result
            .summary
            .violation_types
            .contains(&crate::RouteViolationType::IgnoredTurnRestriction)
    );
}

#[test]
fn allows_forbidden_uturn_when_requested() {
    let topology = forbidden_uturn_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let strict_request = RouteRequest {
        route_id: "forbidden-uturn-strict".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0009,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.0,
            lat: 53.001,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 40.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let degraded_request = RouteRequest {
        fallback: FallbackPolicy {
            allow_uturn_where_normally_forbidden: true,
            penalties: IllegalMovementPenaltyPolicy {
                forbidden_uturn_penalty_s: Some(15.0),
                ..IllegalMovementPenaltyPolicy::default()
            },
            ..FallbackPolicy::default()
        },
        ..strict_request
    };
    let result =
        execute_route(&topology, &metrics, &degraded_request).expect("uturn route succeeds");
    assert_eq!(result.outcome, crate::AnalysisOutcome::Degraded);
    assert!(
        result
            .summary
            .violation_types
            .contains(&crate::RouteViolationType::ForbiddenUturn)
    );
}

#[test]
fn detects_ferry_only_component_fixture() {
    let topology = ferry_only_subnetwork_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let request = RouteRequest {
        route_id: "ferry-component".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.011,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 40.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

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
        crate::ServiceAreaThresholdMetric::TravelTimeS
    ));
}

#[test]
fn executes_service_area_with_partial_edge_frontier() {
    let request = crate::ServiceAreaRequest {
        analysis_id: "service-area-cut".to_string(),
        origins: vec![crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        }],
        thresholds: vec![ServiceAreaThreshold {
            id: Some("fifteen_s".to_string()),
            limit: 15.0,
            metric: ServiceAreaThresholdMetric::TravelTimeS,
        }],
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        output_mode: ServiceAreaOutputMode::Network,
        band_mode: ServiceAreaBandMode::Cumulative,
        boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
        multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
        polygon: Default::default(),
        returns: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_service_area(
        &service_area_linear_topology(),
        &service_area_linear_metrics(),
        &request,
    )
    .expect("service area succeeds");

    assert_eq!(result.outcome, crate::AnalysisOutcome::Legal);
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

#[test]
fn executes_unbanded_service_area_with_segment_costs() {
    let request = crate::ServiceAreaRequest {
        analysis_id: "service-area-segments".to_string(),
        origins: vec![crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        }],
        thresholds: vec![ServiceAreaThreshold {
            id: Some("fifteen_s".to_string()),
            limit: 15.0,
            metric: ServiceAreaThresholdMetric::TravelTimeS,
        }],
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        output_mode: ServiceAreaOutputMode::Network,
        band_mode: ServiceAreaBandMode::Unbanded,
        boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
        multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
        polygon: Default::default(),
        returns: ServiceAreaReturnOptions {
            segments: true,
            ..ServiceAreaReturnOptions::default()
        },
        temporal: Default::default(),
    };

    let result = execute_service_area(
        &service_area_linear_topology(),
        &service_area_linear_metrics(),
        &request,
    )
    .expect("service area succeeds");

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

fn service_area_component_metrics() -> netweevil_core::CompiledProfileBundle {
    let mut metrics = service_area_linear_metrics();
    metrics.components = vec![CompiledCostComponent {
        name: "Exposure".to_string(),
        weight: 1.0,
        edge_values: vec![4.0, 8.0],
        scales_with_travel_time: false,
        overlay_name: None,
        invert_overlay: false,
    }];
    metrics
}

fn component_service_area_request() -> crate::ServiceAreaRequest {
    crate::ServiceAreaRequest {
        analysis_id: "service-area-components".to_string(),
        origins: vec![crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        }],
        thresholds: vec![ServiceAreaThreshold {
            id: Some("fifteen_s".to_string()),
            limit: 15.0,
            metric: ServiceAreaThresholdMetric::TravelTimeS,
        }],
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        output_mode: ServiceAreaOutputMode::Network,
        band_mode: ServiceAreaBandMode::Unbanded,
        boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
        multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
        polygon: Default::default(),
        returns: ServiceAreaReturnOptions {
            segments: true,
            ..ServiceAreaReturnOptions::default()
        },
        temporal: Default::default(),
    }
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
        &component_service_area_request(),
    )
    .expect("static component service area succeeds");

    assert_component_service_area(&result);
}

#[test]
fn reports_exact_temporal_service_area_component_labels() {
    let mut request = component_service_area_request();
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
    let request = crate::ServiceAreaRequest {
        analysis_id: "service-area-output-limit".to_string(),
        origins: vec![crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        }],
        thresholds: vec![ServiceAreaThreshold {
            id: Some("thirty_s".to_string()),
            limit: 30.0,
            metric: ServiceAreaThresholdMetric::TravelTimeS,
        }],
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        output_mode: ServiceAreaOutputMode::Network,
        band_mode: ServiceAreaBandMode::Cumulative,
        boundary_mode: ServiceAreaBoundaryMode::Overlap,
        multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
        polygon: Default::default(),
        returns: ServiceAreaReturnOptions {
            max_geometry_points: 1,
            ..ServiceAreaReturnOptions::default()
        },
        temporal: Default::default(),
    };

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
    let request = crate::ServiceAreaRequest {
        analysis_id: "service-area-ring".to_string(),
        origins: vec![crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        }],
        thresholds: vec![
            ServiceAreaThreshold {
                id: Some("ten_s".to_string()),
                limit: 10.0,
                metric: ServiceAreaThresholdMetric::TravelTimeS,
            },
            ServiceAreaThreshold {
                id: Some("thirty_s".to_string()),
                limit: 30.0,
                metric: ServiceAreaThresholdMetric::TravelTimeS,
            },
        ],
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        output_mode: ServiceAreaOutputMode::Network,
        band_mode: ServiceAreaBandMode::Ring,
        boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
        multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
        polygon: Default::default(),
        returns: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_service_area(
        &service_area_linear_topology(),
        &service_area_linear_metrics(),
        &request,
    )
    .expect("service area succeeds");

    assert_eq!(result.features.len(), 2);
    assert_eq!(result.features[0].reachable_network_length_m, Some(100.0));
    assert_eq!(result.features[1].band_start_limit, Some(10.0));
    assert_eq!(result.features[1].reachable_network_length_m, Some(200.0));
}

#[test]
fn merges_multi_origin_service_areas() {
    let request = crate::ServiceAreaRequest {
        analysis_id: "service-area-merge".to_string(),
        origins: vec![
            crate::LabeledPoint {
                id: "origin_a".to_string(),
                lon: 6.0,
                lat: 53.0,
                z: None,
            },
            crate::LabeledPoint {
                id: "origin_b".to_string(),
                lon: 6.001,
                lat: 53.0,
                z: None,
            },
        ],
        thresholds: vec![ServiceAreaThreshold {
            id: Some("thirty_s".to_string()),
            limit: 30.0,
            metric: ServiceAreaThresholdMetric::TravelTimeS,
        }],
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        output_mode: ServiceAreaOutputMode::Network,
        band_mode: ServiceAreaBandMode::Cumulative,
        boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
        multi_origin_mode: ServiceAreaMultiOriginMode::Merge,
        polygon: Default::default(),
        returns: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_service_area(
        &service_area_linear_topology(),
        &service_area_linear_metrics(),
        &request,
    )
    .expect("service area succeeds");

    assert_eq!(result.features.len(), 1);
    assert_eq!(result.features[0].origin_id, None);
    assert_eq!(result.summaries.len(), 2);
    assert_eq!(result.processed_origin_count, 2);
}

#[test]
fn emits_service_area_polygon_output() {
    let request = crate::ServiceAreaRequest {
        analysis_id: "service-area-polygon".to_string(),
        origins: vec![crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        }],
        thresholds: vec![ServiceAreaThreshold {
            id: Some("three_hundred_m".to_string()),
            limit: 300.0,
            metric: ServiceAreaThresholdMetric::DistanceM,
        }],
        snap: SnapOptions {
            max_distance_m: 500.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        output_mode: ServiceAreaOutputMode::Both,
        band_mode: ServiceAreaBandMode::Cumulative,
        boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
        multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
        polygon: Default::default(),
        returns: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_service_area(
        &service_area_linear_topology(),
        &service_area_linear_metrics(),
        &request,
    )
    .expect("service area succeeds");

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
fn allows_origin_hop_fallback_between_components() {
    let topology = hop_disconnected_topology();
    let metrics = hop_disconnected_metrics();
    let request = RouteRequest {
        route_id: "origin-hop".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0002,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.0015,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 40.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: ConnectivityPolicy {
            disconnected: DisconnectedNetworkMode::HopOriginToNearestReachableComponent,
            max_hop_distance_m: Some(120.0),
            report_hop_distance_separately: true,
        },
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::Full,
            ..ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
    assert_eq!(result.outcome, crate::AnalysisOutcome::Degraded);
    assert!(result.fallback_used);
    assert!(result.origin_hop_distance_m.is_some());
    assert_eq!(result.destination_hop_distance_m, None);
    assert_eq!(result.hop_segments.len(), 1);
    assert_eq!(result.hop_segments[0].endpoint, crate::HopEndpoint::Origin);
}

#[test]
fn allows_destination_hop_fallback_between_components() {
    let topology = hop_disconnected_topology();
    let metrics = hop_disconnected_metrics();
    let request = RouteRequest {
        route_id: "destination-hop".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0010,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.0003,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 40.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: ConnectivityPolicy {
            disconnected: DisconnectedNetworkMode::HopDestinationToNearestReachableComponent,
            max_hop_distance_m: Some(120.0),
            report_hop_distance_separately: true,
        },
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
    assert_eq!(result.outcome, crate::AnalysisOutcome::Degraded);
    assert!(result.fallback_used);
    assert_eq!(result.origin_hop_distance_m, None);
    assert!(result.destination_hop_distance_m.is_some());
    assert_eq!(result.hop_segments.len(), 1);
    assert_eq!(
        result.hop_segments[0].endpoint,
        crate::HopEndpoint::Destination
    );
}

#[test]
fn allows_either_end_hop_fallback_between_components() {
    let topology = hop_disconnected_topology();
    let metrics = hop_disconnected_metrics();
    let request = RouteRequest {
        route_id: "either-hop".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0002,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.0015,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 40.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: ConnectivityPolicy {
            disconnected: DisconnectedNetworkMode::HopEitherEnd,
            max_hop_distance_m: Some(120.0),
            report_hop_distance_separately: true,
        },
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
    assert!(result.fallback_used);
    assert_eq!(result.outcome, crate::AnalysisOutcome::Degraded);
}

#[test]
fn hop_fallback_keeps_legal_network_cost_unchanged() {
    let topology = hop_disconnected_topology();
    let metrics = hop_disconnected_metrics();
    let legal_request = RouteRequest {
        route_id: "legal".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0010,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.0015,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 40.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };
    let hopped_request = RouteRequest {
        route_id: "hopped".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0002,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.0015,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 40.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: ConnectivityPolicy {
            disconnected: DisconnectedNetworkMode::HopOriginToNearestReachableComponent,
            max_hop_distance_m: Some(120.0),
            report_hop_distance_separately: true,
        },
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

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
    let request = RouteRequest {
        route_id: "dead-node-snap".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.0002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 30.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

    let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
    assert_ne!(result.origin.snapped_node_id, 0);
    assert_eq!(result.origin.snapped_node_id, 1);
    assert_eq!(result.destination.snapped_node_id, 2);
}

#[test]
fn accelerated_failure_mode_route_matches_astar_route() {
    // Pairwise-only restriction: the failure penalties bake into customized
    // CCH weights, so the accelerated engine must return the same degraded
    // route as the unaccelerated A* path.
    let topology = illegal_turn_only_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let (bundle, accelerated_metrics) = build_test_cch(&topology, &metrics);
    let plain_engine = crate::PreparedRoutingEngine::new(
        std::sync::Arc::new(topology.clone()),
        std::sync::Arc::new(metrics),
        None,
    )
    .expect("plain engine builds");
    let accelerated_engine = crate::PreparedRoutingEngine::new(
        std::sync::Arc::new(topology),
        std::sync::Arc::new(accelerated_metrics),
        Some(bundle),
    )
    .expect("accelerated engine builds");

    let request = RouteRequest {
        route_id: "illegal-turn-accelerated".to_string(),
        origin: crate::LabeledPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
            z: None,
        },
        destination: crate::LabeledPoint {
            id: "destination".to_string(),
            lon: 6.002,
            lat: 53.0,
            z: None,
        },
        snap: SnapOptions {
            max_distance_m: 40.0,
            z_window_m: None,
            attribute_filters: Default::default(),
        },
        connectivity: Default::default(),
        fallback: FallbackPolicy {
            allow_illegal_turn: true,
            penalties: IllegalMovementPenaltyPolicy {
                illegal_turn_penalty_s: Some(45.0),
                ..IllegalMovementPenaltyPolicy::default()
            },
            ..FallbackPolicy::default()
        },
        returns: ReturnConfig::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };

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
