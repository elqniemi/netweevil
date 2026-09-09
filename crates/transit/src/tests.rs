use std::collections::BTreeMap;
use std::sync::Arc;

use crate::gtfs::{GtfsFiles, build_bundle_from_files};

use super::*;

#[test]
fn imports_one_week_and_routes_walk_transit_walk() {
    let bundle = build_bundle_from_files(
        fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 7,
        },
    )
    .expect("fixture imports");
    assert_eq!(bundle.connections.len(), 14);
    let request = TransitRouteRequest {
        route_id: "r1".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "dest".to_string(),
            lon: 6.02,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions {
            max_access_distance_m: 100.0,
            max_egress_distance_m: 100.0,
            ..TransitModeOptions::default()
        },
        returns: TransitReturnOptions {
            include_geometry: true,
            include_stops: true,
            include_stop_segments: true,
            ..TransitReturnOptions::default()
        },
        alternatives: TransitAlternativeOptions::default(),
    };
    let result = execute_transit_route(&bundle, &request).expect("route executes");
    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(result.summary.boarding_count, 1);
    assert_eq!(
        result
            .legs
            .iter()
            .filter(|leg| leg.leg_type == TransitLegType::Transit)
            .count(),
        1
    );
    assert_eq!(result.stops.len(), 3);
    assert_eq!(result.stop_segments.len(), 2);
    assert_eq!(result.stops[0].stop_id, "A");
    assert_eq!(result.stops[2].stop_id, "C");
    assert_eq!(result.stop_segments[0].from_stop_id, "A");
    assert_eq!(result.stop_segments[1].to_stop_id, "C");
}

#[test]
fn executes_transit_service_area_to_reachable_stops_and_segments() {
    let bundle = build_bundle_from_files(
        fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 7,
        },
    )
    .expect("fixture imports");
    let request = TransitServiceAreaRequest {
        analysis_id: "sa1".to_string(),
        origins: vec![TransitPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
        }],
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions {
            max_access_distance_m: 100.0,
            max_transfer_distance_m: 100.0,
            ..TransitModeOptions::default()
        },
        max_travel_time_s: 1800,
        returns: TransitServiceAreaReturnOptions::default(),
    };

    let result = execute_transit_service_area(&bundle, &request).expect("service area");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(result.processed_origin_count, 1);
    assert!(result.stops.iter().any(|stop| stop.stop_id == "C"));
    assert!(
        result
            .stop_segments
            .iter()
            .any(|segment| segment.from_stop_id == "B" && segment.to_stop_id == "C")
    );
    assert!(
        result
            .stops
            .iter()
            .all(|stop| stop.travel_time_s <= request.max_travel_time_s)
    );
}

#[test]
fn rejects_transit_service_area_above_stop_segment_limit() {
    let bundle = build_bundle_from_files(
        fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 7,
        },
    )
    .expect("fixture imports");
    let request = TransitServiceAreaRequest {
        analysis_id: "sa-output-limit".to_string(),
        origins: vec![TransitPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
        }],
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions {
            max_access_distance_m: 100.0,
            max_transfer_distance_m: 100.0,
            ..TransitModeOptions::default()
        },
        max_travel_time_s: 1800,
        returns: TransitServiceAreaReturnOptions {
            max_stop_segments: 0,
            ..TransitServiceAreaReturnOptions::default()
        },
    };

    let error = execute_transit_service_area(&bundle, &request)
        .expect_err("stop segment cap should reject large output");

    assert!(error.to_string().contains("max_stop_segments"));
}

#[test]
fn prepared_router_reuses_departures_and_spatial_index() {
    let bundle = Arc::new(
        build_bundle_from_files(
            fixture_files(),
            "abc".to_string(),
            TransitImportOptions {
                name: "fixture".to_string(),
                source_label: "fixture".to_string(),
                service_start_date: "2026-05-11".to_string(),
                service_days: 7,
            },
        )
        .expect("fixture imports"),
    );
    let router = PreparedTransitRouter::new(bundle);
    let request = TransitRouteRequest {
        route_id: "r1".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "dest".to_string(),
            lon: 6.02,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions {
            max_access_distance_m: 100.0,
            max_egress_distance_m: 100.0,
            ..TransitModeOptions::default()
        },
        returns: TransitReturnOptions::default(),
        alternatives: TransitAlternativeOptions::default(),
    };

    let result = router.execute_route(&request).expect("route executes");
    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(result.summary.total_travel_time_s, Some(1201));
    assert_eq!(result.summary.boarding_count, 1);
}

#[test]
fn filters_disallowed_transit_modes() {
    let bundle = build_bundle_from_files(
        fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 7,
        },
    )
    .expect("fixture imports");
    let request = TransitRouteRequest {
        route_id: "r1".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "dest".to_string(),
            lon: 6.02,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions {
            transit: vec![TransitMode::Rail],
            max_access_distance_m: 100.0,
            max_egress_distance_m: 100.0,
            ..TransitModeOptions::default()
        },
        returns: TransitReturnOptions {
            include_geometry: true,
            ..TransitReturnOptions::default()
        },
        alternatives: TransitAlternativeOptions::default(),
    };
    let result = execute_transit_route(&bundle, &request).expect("route executes");
    assert_eq!(result.outcome, TransitOutcome::Unreachable);
}

#[test]
fn does_not_walk_transfer_before_first_boarding() {
    let bundle = build_bundle_from_files(
        access_transfer_fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 1,
        },
    )
    .expect("fixture imports");
    let request = TransitRouteRequest {
        route_id: "no_preboard_transfer".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "dest".to_string(),
            lon: 6.01,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions {
            max_access_distance_m: 100.0,
            max_egress_distance_m: 100.0,
            max_transfer_distance_m: 1_000.0,
            ..TransitModeOptions::default()
        },
        returns: TransitReturnOptions::default(),
        alternatives: TransitAlternativeOptions::default(),
    };

    let result = execute_transit_route(&bundle, &request).expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Unreachable);
    assert!(result.legs.is_empty());
}

#[test]
fn does_not_finish_by_transferring_to_an_egress_stop() {
    let bundle = build_bundle_from_files(
        terminal_transfer_fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 1,
        },
    )
    .expect("fixture imports");
    let request = TransitRouteRequest {
        route_id: "no_terminal_transfer".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "dest".to_string(),
            lon: 6.01,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions {
            max_access_distance_m: 100.0,
            max_egress_distance_m: 100.0,
            max_transfer_distance_m: 1_000.0,
            ..TransitModeOptions::default()
        },
        returns: TransitReturnOptions::default(),
        alternatives: TransitAlternativeOptions::default(),
    };

    let result = execute_transit_route(&bundle, &request).expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Unreachable);
    assert!(result.legs.is_empty());
}

#[test]
fn rejects_routes_with_transit_legs_below_minimum_duration() {
    let bundle = build_bundle_from_files(
        fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 1,
        },
    )
    .expect("fixture imports");
    let request = TransitRouteRequest {
        route_id: "min_leg_duration".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "dest".to_string(),
            lon: 6.02,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions {
            max_access_distance_m: 100.0,
            max_egress_distance_m: 100.0,
            min_transit_leg_duration_s: 600,
            ..TransitModeOptions::default()
        },
        returns: TransitReturnOptions::default(),
        alternatives: TransitAlternativeOptions::default(),
    };

    let result = execute_transit_route(&bundle, &request).expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Unreachable);
    assert!(result.diagnostics[0].contains("minimum transit leg constraints"));
}

#[test]
fn accepts_routes_when_transit_leg_distance_minimum_passes() {
    let bundle = build_bundle_from_files(
        fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 1,
        },
    )
    .expect("fixture imports");
    let request = TransitRouteRequest {
        route_id: "min_leg_distance".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "dest".to_string(),
            lon: 6.02,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions {
            max_access_distance_m: 100.0,
            max_egress_distance_m: 100.0,
            min_transit_leg_duration_s: 600,
            min_transit_leg_distance_m: 1_000.0,
            ..TransitModeOptions::default()
        },
        returns: TransitReturnOptions::default(),
        alternatives: TransitAlternativeOptions::default(),
    };

    let result = execute_transit_route(&bundle, &request).expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(result.summary.boarding_count, 1);
}

#[test]
fn expands_frequency_based_trips() {
    let bundle = build_bundle_from_files(
        frequency_fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 1,
        },
    )
    .expect("fixture imports");
    assert_eq!(bundle.connections.len(), 8);

    let request = TransitRouteRequest {
        route_id: "freq".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "dest".to_string(),
            lon: 6.02,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: "2026-05-11T08:05:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 1800,
        },
        modes: TransitModeOptions {
            transit: vec![TransitMode::Subway],
            max_access_distance_m: 100.0,
            max_egress_distance_m: 100.0,
            ..TransitModeOptions::default()
        },
        returns: TransitReturnOptions::default(),
        alternatives: TransitAlternativeOptions::default(),
    };

    let result = execute_transit_route(&bundle, &request).expect("route executes");
    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(result.summary.boarding_count, 1);
    assert!(
        result
            .legs
            .iter()
            .any(|leg| leg.leg_type == TransitLegType::Transit
                && leg.route_short_name.as_deref() == Some("MTR"))
    );
}

#[test]
fn uses_gtfs_shapes_for_transit_geometry() {
    let bundle = build_bundle_from_files(
        shape_fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 1,
        },
    )
    .expect("fixture imports");

    let request = TransitRouteRequest {
        route_id: "shape".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "dest".to_string(),
            lon: 6.02,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions {
            max_access_distance_m: 100.0,
            max_egress_distance_m: 100.0,
            ..TransitModeOptions::default()
        },
        returns: TransitReturnOptions {
            include_geometry: true,
            include_stops: true,
            include_stop_segments: true,
            ..TransitReturnOptions::default()
        },
        alternatives: TransitAlternativeOptions::default(),
    };

    let result = execute_transit_route(&bundle, &request).expect("route executes");
    let transit_leg = result
        .legs
        .iter()
        .find(|leg| leg.leg_type == TransitLegType::Transit)
        .expect("transit leg");
    assert!(transit_leg.geometry.len() > 3);
    assert!(
        result
            .stop_segments
            .iter()
            .any(|segment| segment.geometry.len() > 2)
    );
}

#[test]
fn routes_bicycle_access_with_walk_egress_when_mixed_modes_enabled() {
    let bundle = build_bundle_from_files(
        fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 1,
        },
    )
    .expect("fixture imports");
    // Origin is ~2 km from stop A: outside the walking access limit but
    // well inside the default bicycle access limit.
    let request = TransitRouteRequest {
        route_id: "bike_first_mile".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 5.97,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "dest".to_string(),
            lon: 6.02,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions {
            access: vec![AccessMode::Bicycle],
            egress: vec![AccessMode::Walk],
            mixed_access_egress: true,
            max_access_distance_m: 100.0,
            max_egress_distance_m: 100.0,
            ..TransitModeOptions::default()
        },
        returns: TransitReturnOptions::default(),
        alternatives: TransitAlternativeOptions::default(),
    };

    let result = execute_transit_route(&bundle, &request).expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    let access_leg = result
        .legs
        .iter()
        .find(|leg| leg.leg_type == TransitLegType::Access)
        .expect("access leg");
    assert_eq!(access_leg.street_mode, Some(AccessMode::Bicycle));
    let egress_leg = result
        .legs
        .iter()
        .find(|leg| leg.leg_type == TransitLegType::Egress)
        .expect("egress leg");
    assert_eq!(egress_leg.street_mode, Some(AccessMode::Walk));
}

#[test]
fn rejects_mixed_access_egress_modes_without_opt_in() {
    let bundle = build_bundle_from_files(
        fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 1,
        },
    )
    .expect("fixture imports");
    let request = TransitRouteRequest {
        route_id: "mixed_without_flag".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 5.97,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "dest".to_string(),
            lon: 6.02,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions {
            access: vec![AccessMode::Bicycle],
            egress: vec![AccessMode::Walk],
            ..TransitModeOptions::default()
        },
        returns: TransitReturnOptions::default(),
        alternatives: TransitAlternativeOptions::default(),
    };

    let error = execute_transit_route(&bundle, &request).expect_err("mixed modes rejected");
    assert!(error.to_string().contains("mixed_access_egress"));
}

#[test]
fn routes_with_matching_bicycle_access_and_egress_without_opt_in() {
    let bundle = build_bundle_from_files(
        fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 1,
        },
    )
    .expect("fixture imports");
    let request = TransitRouteRequest {
        route_id: "bike_both_miles".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 5.97,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "dest".to_string(),
            lon: 6.05,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions {
            access: vec![AccessMode::Bicycle],
            egress: vec![AccessMode::Bicycle],
            max_access_distance_m: 100.0,
            max_egress_distance_m: 100.0,
            ..TransitModeOptions::default()
        },
        returns: TransitReturnOptions::default(),
        alternatives: TransitAlternativeOptions::default(),
    };

    let result = execute_transit_route(&bundle, &request).expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert!(
        result
            .legs
            .iter()
            .filter(|leg| matches!(
                leg.leg_type,
                TransitLegType::Access | TransitLegType::Egress
            ))
            .all(|leg| leg.street_mode == Some(AccessMode::Bicycle))
    );
}

#[test]
fn car_access_extends_transit_service_area_reach() {
    let bundle = build_bundle_from_files(
        fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 1,
        },
    )
    .expect("fixture imports");
    // Origin is ~10 km from stop A: only car access can bridge the first mile.
    let origin = TransitPoint {
        id: "park_and_ride".to_string(),
        lon: 5.85,
        lat: 53.0,
    };
    let walk_request = TransitServiceAreaRequest {
        analysis_id: "sa_walk".to_string(),
        origins: vec![origin.clone()],
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions::default(),
        max_travel_time_s: 3600,
        returns: TransitServiceAreaReturnOptions::default(),
    };
    let car_request = TransitServiceAreaRequest {
        analysis_id: "sa_car".to_string(),
        modes: TransitModeOptions {
            access: vec![AccessMode::Car],
            egress: vec![AccessMode::Car],
            ..TransitModeOptions::default()
        },
        ..walk_request.clone()
    };

    let walk_result = execute_transit_service_area(&bundle, &walk_request).expect("walk run");
    assert_eq!(walk_result.outcome, TransitOutcome::Unreachable);
    assert_eq!(walk_result.skipped_origin_count, 1);

    let car_result = execute_transit_service_area(&bundle, &car_request).expect("car run");
    assert_eq!(car_result.outcome, TransitOutcome::Scheduled);
    assert_eq!(car_result.processed_origin_count, 1);
    let stop_a = car_result
        .stops
        .iter()
        .find(|stop| stop.stop_id == "A")
        .expect("stop A reachable by car access");
    assert_eq!(stop_a.access_mode, Some(AccessMode::Car));
}

#[test]
fn hardens_bom_blank_stop_times_sentinel_calendar_and_after_midnight_times() {
    let bundle = build_bundle_from_files(
        hardened_gtfs_fixture_files(),
        "hardened".to_string(),
        TransitImportOptions {
            name: "hardened".to_string(),
            source_label: "hardened".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 2,
        },
    )
    .expect("hardened fixture imports");

    // The sentinel calendar activates Mondays, but calendar_dates removes the
    // Monday and adds Tuesday. Only the Tuesday copy should be materialized.
    assert_eq!(bundle.connections.len(), 3);
    assert_eq!(bundle.connections[0].departure_s, 86_400 + 25 * 3_600);
    assert_eq!(bundle.connections[0].arrival_s, 86_400 + 25 * 3_600 + 1_200);
    assert_eq!(
        bundle.connections[1].departure_s,
        86_400 + 25 * 3_600 + 1_200
    );
    assert_eq!(bundle.connections[2].arrival_s, 86_400 + 26 * 3_600);
}

#[test]
fn rejects_unbounded_blank_stop_times() {
    let mut files = hardened_gtfs_fixture_files();
    files.insert(
        "stop_times.txt",
        "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,,,A,1\nT1,,,B,2\nT1,26:00:00,26:00:00,D,3\n"
            .to_string(),
    );
    let error = build_bundle_from_files(
        files,
        "hardened".to_string(),
        TransitImportOptions {
            name: "hardened".to_string(),
            source_label: "hardened".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 2,
        },
    )
    .expect_err("unbounded blanks must be rejected");
    assert!(error.to_string().contains("only intermediate blanks"));
}

#[test]
fn applies_explicit_stop_bindings_with_3d_attribute_filters() {
    let mut bundle = build_bundle_from_files(
        fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 1,
        },
    )
    .expect("fixture imports");
    let mut attribute_filter = BTreeMap::new();
    attribute_filter.insert("indoor_location".to_string(), "platform".to_string());
    let table = TransitStopBindingTable {
        schema_version: TRANSIT_STOP_BINDING_SCHEMA_VERSION,
        feed_id: Some("fixture".to_string()),
        bindings: vec![
            TransitStopBinding {
                stop_id: "A".to_string(),
                target: TransitStopBindingTarget::Node { node_id: 42 },
            },
            TransitStopBinding {
                stop_id: "C".to_string(),
                target: TransitStopBindingTarget::Coordinate {
                    lon: 114.17,
                    lat: 22.3,
                    z: Some(-8.5),
                    z_window_m: Some(1.5),
                    attribute_filter: attribute_filter.clone(),
                },
            },
        ],
    };
    let json = serde_json::to_string(&table).expect("binding table serializes as JSON");
    assert!(json.contains("\"node\""));
    assert!(json.contains("\"coordinate\""));
    let json_roundtrip: TransitStopBindingTable =
        serde_json::from_str(&json).expect("binding table deserializes from JSON");
    assert_eq!(json_roundtrip, table);

    let summary = apply_transit_stop_bindings(&mut bundle, &table).expect("bindings apply");
    assert_eq!(summary.bound_stop_count, 2);
    assert_eq!(summary.unbound_stop_count, 1);
    assert_eq!(
        bundle.stops[0].binding,
        Some(TransitStopBindingTarget::Node { node_id: 42 })
    );
    assert_eq!(
        bundle.stops[2].binding,
        Some(TransitStopBindingTarget::Coordinate {
            lon: 114.17,
            lat: 22.3,
            z: Some(-8.5),
            z_window_m: Some(1.5),
            attribute_filter,
        })
    );
    let encoded = bincode::serialize(&bundle).expect("bound bundle serializes");
    let decoded: TransitBundle = bincode::deserialize(&encoded).expect("bound bundle deserializes");
    assert_eq!(decoded.stop_binding_sha256, bundle.stop_binding_sha256);
    assert_eq!(decoded.stops[2].binding, bundle.stops[2].binding);
}

#[test]
fn transfer_builder_routes_bound_stops_directionally() {
    struct BindingAwareEstimator;
    impl StreetTimeEstimator for BindingAwareEstimator {
        fn street_time_s(
            &self,
            _mode: AccessMode,
            _egress: bool,
            _from_lon: f64,
            _from_lat: f64,
            _to_lon: f64,
            _to_lat: f64,
        ) -> Option<u32> {
            None
        }

        fn stop_to_stop_path(
            &self,
            _mode: AccessMode,
            from: &TransitStop,
            to: &TransitStop,
        ) -> Option<TransitStreetPath> {
            let TransitStopBindingTarget::Node { node_id: from_node } = from.binding.as_ref()?
            else {
                return None;
            };
            let TransitStopBindingTarget::Node { node_id: to_node } = to.binding.as_ref()? else {
                return None;
            };
            Some(TransitStreetPath {
                travel_time_s: from_node.abs_diff(*to_node) + 10,
                distance_m: Some(from_node.abs_diff(*to_node) as f64),
                edge_path: vec![*from_node, *to_node],
                geometry: Vec::new(),
                components: BTreeMap::new(),
            })
        }
    }

    let mut bundle = build_bundle_from_files(
        fixture_files(),
        "abc".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 1,
        },
    )
    .expect("fixture imports");
    for (index, stop) in bundle.stops.iter_mut().enumerate() {
        stop.binding = Some(TransitStopBindingTarget::Node {
            node_id: index as u32 + 100,
        });
    }
    let table = build_transit_transfer_table(
        &bundle,
        &TransitTransferBuildOptions {
            dataset_id: "station-network".to_string(),
            profile_id: "step_free".to_string(),
            profile_hash: "profile-hash".to_string(),
            max_transfer_distance_m: 2_000.0,
            max_candidates_per_stop: 1,
        },
        &BindingAwareEstimator,
    )
    .expect("transfer table builds");

    assert_eq!(table.profile_id, "step_free");
    assert_eq!(table.transfers.len(), 3);
    assert!(
        table
            .transfers
            .iter()
            .all(|transfer| transfer.path.edge_path.len() == 2)
    );

    struct CoordinateOnlyEstimator;
    impl StreetTimeEstimator for CoordinateOnlyEstimator {
        fn street_time_s(
            &self,
            _mode: AccessMode,
            _egress: bool,
            _from_lon: f64,
            _from_lat: f64,
            _to_lon: f64,
            _to_lat: f64,
        ) -> Option<u32> {
            Some(1)
        }
    }
    let coordinate_only = build_transit_transfer_table(
        &bundle,
        &TransitTransferBuildOptions {
            dataset_id: "station-network".to_string(),
            profile_id: "step_free".to_string(),
            profile_hash: "profile-hash".to_string(),
            max_transfer_distance_m: 2_000.0,
            max_candidates_per_stop: 1,
        },
        &CoordinateOnlyEstimator,
    )
    .expect("coordinate-only transfer build completes");
    assert!(
        coordinate_only.transfers.is_empty(),
        "coordinate-only fallback must not bypass explicit stop bindings"
    );
    assert!(
        table
            .transfers
            .iter()
            .any(|transfer| transfer.from_stop_id == "B" && transfer.to_stop_id == "A")
    );
}

#[test]
fn csa_consumes_network_transfer_table_and_exposes_station_path() {
    let bundle = Arc::new(
        build_bundle_from_files(
            two_line_transfer_fixture_files(),
            "station".to_string(),
            TransitImportOptions {
                name: "station".to_string(),
                source_label: "station".to_string(),
                service_start_date: "2026-05-11".to_string(),
                service_days: 1,
            },
        )
        .expect("station fixture imports"),
    );
    let mut components = BTreeMap::new();
    components.insert("ascent_m".to_string(), 12.0);
    components.insert("lift_wait_s".to_string(), 35.0);
    let table = TransitTransferTable {
        schema_version: TRANSIT_TRANSFER_TABLE_SCHEMA_VERSION,
        feed_id: bundle.feed_id.clone(),
        source_sha256: bundle.source_sha256.clone(),
        stop_binding_sha256: bundle.stop_binding_sha256.clone(),
        dataset_id: "station-network".to_string(),
        profile_id: "step_free".to_string(),
        profile_hash: "profile-hash".to_string(),
        max_transfer_distance_m: 100.0,
        transfers: vec![network_transfer(
            "B1",
            "B2",
            7.0,
            TransitStreetPath {
                travel_time_s: 180,
                distance_m: Some(95.0),
                edge_path: vec![41, 42, 43],
                geometry: vec![[6.01, 53.0, -12.0], [6.0101, 53.0, -2.0]],
                components: components.clone(),
            },
        )],
    };
    let encoded = bincode::serialize(&table).expect("transfer table serializes");
    let table: TransitTransferTable =
        bincode::deserialize(&encoded).expect("transfer table deserializes");
    let router = PreparedTransitRouter::new_with_transfer_tables(Arc::clone(&bundle), vec![table])
        .expect("prepared router loads table");
    let request = TransitRouteRequest {
        route_id: "station-transfer".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "destination".to_string(),
            lon: 6.02,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3_600,
        },
        modes: TransitModeOptions {
            max_access_distance_m: 50.0,
            max_egress_distance_m: 50.0,
            max_transfer_distance_m: 100.0,
            transfer_profile_id: Some("step_free".to_string()),
            transfer_slack_s: 0,
            ..TransitModeOptions::default()
        },
        returns: TransitReturnOptions {
            include_geometry: true,
            walking_geometry: TransitWalkingGeometry::Network,
            ..TransitReturnOptions::default()
        },
        alternatives: TransitAlternativeOptions::default(),
    };

    let result = router
        .execute_route(&request)
        .expect("network-transfer route executes");
    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(result.summary.boarding_count, 2);
    let transfer = result
        .legs
        .iter()
        .find(|leg| leg.leg_type == TransitLegType::Transfer)
        .expect("transfer leg");
    assert_eq!(transfer.arrival_s - transfer.departure_s, 180);
    let path = transfer
        .network_path
        .as_ref()
        .expect("network path exposed");
    assert_eq!(path.edge_path, vec![41, 42, 43]);
    assert_eq!(path.components, components);
    assert_eq!(path.geometry[0][2], -12.0);
    assert_eq!(transfer.geometry, path.geometry);
}

fn hardened_gtfs_fixture_files() -> GtfsFiles {
    let mut files = GtfsFiles::default();
    files.insert(
        "stops.txt",
        "\u{feff}stop_id,stop_name,stop_lat,stop_lon\nA,A,53.0,6.0\nB,B,53.0,6.01\nC,C,53.0,6.02\nD,D,53.0,6.03\n"
            .to_string(),
    );
    files.insert(
        "routes.txt",
        "\u{feff}route_id,route_short_name,route_long_name,route_type\nR,1,Night,3\n".to_string(),
    );
    files.insert(
        "trips.txt",
        "\u{feff}route_id,service_id,trip_id,trip_headsign\nR,WEEK,T1,D\n".to_string(),
    );
    files.insert(
        "calendar.txt",
        "\u{feff}service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nWEEK,1,0,0,0,0,0,0,20200101,20991231\n"
            .to_string(),
    );
    files.insert(
        "calendar_dates.txt",
        "\u{feff}service_id,date,exception_type\nWEEK,20260511,2\nWEEK,20260512,1\n".to_string(),
    );
    files.insert(
        "stop_times.txt",
        "\u{feff}trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,25:00:00,25:00:00,A,1\nT1,,,B,2\nT1,,,C,3\nT1,26:00:00,26:00:00,D,4\n"
            .to_string(),
    );
    files
}

fn two_line_transfer_fixture_files() -> GtfsFiles {
    let mut files = GtfsFiles::default();
    files.insert(
        "stops.txt",
        "stop_id,stop_name,stop_lat,stop_lon\nA,A,53.0,6.0\nB1,Platform 1,53.0,6.01\nB2,Platform 2,53.0,6.0101\nC,C,53.0,6.02\n"
            .to_string(),
    );
    files.insert(
        "routes.txt",
        "route_id,route_short_name,route_long_name,route_type\nR1,1,Line 1,1\nR2,2,Line 2,1\n"
            .to_string(),
    );
    files.insert(
        "trips.txt",
        "route_id,service_id,trip_id,trip_headsign\nR1,WEEK,T1,B1\nR2,WEEK,T2,C\n".to_string(),
    );
    files.insert(
        "calendar.txt",
        "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nWEEK,1,1,1,1,1,1,1,20260501,20260531\n"
            .to_string(),
    );
    files.insert(
        "calendar_dates.txt",
        "service_id,date,exception_type\n".to_string(),
    );
    files.insert(
        "stop_times.txt",
        "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,08:05:00,08:05:00,A,1\nT1,08:10:00,08:10:00,B1,2\nT2,08:15:00,08:15:00,B2,1\nT2,08:25:00,08:25:00,C,2\n"
            .to_string(),
    );
    files
}

fn fixture_files() -> GtfsFiles {
    let mut files = GtfsFiles::default();
    files.insert(
        "stops.txt",
        "stop_id,stop_name,stop_lat,stop_lon\nA,A,53.0,6.0\nB,B,53.0,6.01\nC,C,53.0,6.02\n"
            .to_string(),
    );
    files.insert(
        "routes.txt",
        "route_id,route_short_name,route_long_name,route_type\nR,1,Line 1,3\n".to_string(),
    );
    files.insert(
        "trips.txt",
        "route_id,service_id,trip_id,trip_headsign\nR,WEEK,T1,C\n".to_string(),
    );
    files.insert(
            "calendar.txt",
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nWEEK,1,1,1,1,1,1,1,20260501,20260531\n".to_string(),
        );
    files.insert(
        "calendar_dates.txt",
        "service_id,date,exception_type\n".to_string(),
    );
    files.insert(
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,08:10:00,08:10:30,A,1\nT1,08:15:00,08:15:30,B,2\nT1,08:20:00,08:20:00,C,3\n".to_string(),
        );
    files
}

fn frequency_fixture_files() -> GtfsFiles {
    let mut files = GtfsFiles::default();
    files.insert(
        "stops.txt",
        "stop_id,stop_name,stop_lat,stop_lon\nA,A,53.0,6.0\nB,B,53.0,6.01\nC,C,53.0,6.02\n"
            .to_string(),
    );
    files.insert(
        "routes.txt",
        "route_id,route_short_name,route_long_name,route_type\nM,MTR,Metro,1\n".to_string(),
    );
    files.insert(
        "trips.txt",
        "route_id,service_id,trip_id,trip_headsign\nM,WEEK,T1,C\n".to_string(),
    );
    files.insert(
            "calendar.txt",
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nWEEK,1,1,1,1,1,1,1,20260501,20260531\n".to_string(),
        );
    files.insert(
        "calendar_dates.txt",
        "service_id,date,exception_type\n".to_string(),
    );
    files.insert(
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,00:00:00,00:00:00,A,1\nT1,00:05:00,00:05:00,B,2\nT1,00:10:00,00:10:00,C,3\n".to_string(),
        );
    files.insert(
        "frequencies.txt",
        "trip_id,start_time,end_time,headway_secs\nT1,08:00:00,08:31:00,600\n".to_string(),
    );
    files
}

fn access_transfer_fixture_files() -> GtfsFiles {
    let mut files = GtfsFiles::default();
    files.insert(
        "stops.txt",
        "stop_id,stop_name,stop_lat,stop_lon\nA,A,53.0,6.0\nB,B,53.0,6.005\nC,C,53.0,6.01\n"
            .to_string(),
    );
    files.insert(
        "routes.txt",
        "route_id,route_short_name,route_long_name,route_type\nR,1,Line 1,3\n".to_string(),
    );
    files.insert(
        "trips.txt",
        "route_id,service_id,trip_id,trip_headsign\nR,WEEK,T1,C\n".to_string(),
    );
    files.insert(
            "calendar.txt",
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nWEEK,1,1,1,1,1,1,1,20260501,20260531\n".to_string(),
        );
    files.insert(
        "calendar_dates.txt",
        "service_id,date,exception_type\n".to_string(),
    );
    files.insert(
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,08:10:00,08:10:30,B,1\nT1,08:15:00,08:15:00,C,2\n".to_string(),
        );
    files
}

fn terminal_transfer_fixture_files() -> GtfsFiles {
    let mut files = GtfsFiles::default();
    files.insert(
        "stops.txt",
        "stop_id,stop_name,stop_lat,stop_lon\nA,A,53.0,6.0\nB,B,53.0,6.005\nC,C,53.0,6.01\n"
            .to_string(),
    );
    files.insert(
        "routes.txt",
        "route_id,route_short_name,route_long_name,route_type\nR,1,Line 1,3\n".to_string(),
    );
    files.insert(
        "trips.txt",
        "route_id,service_id,trip_id,trip_headsign\nR,WEEK,T1,B\n".to_string(),
    );
    files.insert(
            "calendar.txt",
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nWEEK,1,1,1,1,1,1,1,20260501,20260531\n".to_string(),
        );
    files.insert(
        "calendar_dates.txt",
        "service_id,date,exception_type\n".to_string(),
    );
    files.insert(
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,08:10:00,08:10:30,A,1\nT1,08:15:00,08:15:00,B,2\n".to_string(),
        );
    files
}

fn shape_fixture_files() -> GtfsFiles {
    let mut files = fixture_files();
    files.insert(
        "trips.txt",
        "route_id,service_id,trip_id,trip_headsign,shape_id\nR,WEEK,T1,C,S1\n".to_string(),
    );
    files.insert(
            "shapes.txt",
            "shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence\nS1,53.0,6.0,1\nS1,53.001,6.005,2\nS1,53.0,6.01,3\nS1,52.999,6.015,4\nS1,53.0,6.02,5\n".to_string(),
        );
    files
}

#[test]
fn network_street_access_prices_legs_with_the_estimator() {
    struct FixedEstimator;
    impl crate::StreetTimeEstimator for FixedEstimator {
        fn street_time_s(
            &self,
            _mode: AccessMode,
            _egress: bool,
            _from_lon: f64,
            _from_lat: f64,
            _to_lon: f64,
            _to_lat: f64,
        ) -> Option<u32> {
            Some(300)
        }
    }

    struct DirectionalEstimator {
        access: bool,
        egress: bool,
    }
    impl crate::StreetTimeEstimator for DirectionalEstimator {
        fn street_time_s(
            &self,
            _mode: AccessMode,
            egress: bool,
            _from_lon: f64,
            _from_lat: f64,
            _to_lon: f64,
            _to_lat: f64,
        ) -> Option<u32> {
            if (egress && self.egress) || (!egress && self.access) {
                Some(300)
            } else {
                None
            }
        }
    }

    let bundle = Arc::new(
        build_bundle_from_files(
            fixture_files(),
            "abc".to_string(),
            TransitImportOptions {
                name: "fixture".to_string(),
                source_label: "fixture".to_string(),
                service_start_date: "2026-05-11".to_string(),
                service_days: 7,
            },
        )
        .expect("fixture imports"),
    );
    let router = PreparedTransitRouter::new(bundle);
    let request = |street_access: TransitStreetAccessModel| TransitRouteRequest {
        route_id: "r1".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "dest".to_string(),
            lon: 6.02,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: "2026-05-11T08:00:00+02:00".to_string(),
            arrive_by: false,
            search_window_s: 3600,
        },
        modes: TransitModeOptions {
            max_access_distance_m: 100.0,
            max_egress_distance_m: 100.0,
            street_access,
            ..TransitModeOptions::default()
        },
        returns: TransitReturnOptions::default(),
        alternatives: TransitAlternativeOptions::default(),
    };

    // Straight-line pricing is unchanged even when an estimator is present.
    let straight = router
        .execute_route_with_street_estimator(
            &request(TransitStreetAccessModel::StraightLine),
            Some(&FixedEstimator),
        )
        .expect("straight-line route executes");
    assert_eq!(straight.summary.total_travel_time_s, Some(1201));

    // Network pricing uses the estimator's times for access and egress.
    let network = router
        .execute_route_with_street_estimator(
            &request(TransitStreetAccessModel::Network),
            Some(&FixedEstimator),
        )
        .expect("network route executes");
    assert_eq!(network.outcome, TransitOutcome::Scheduled);
    let access_leg = network
        .legs
        .iter()
        .find(|leg| leg.leg_type == TransitLegType::Access)
        .expect("access leg present");
    assert_eq!(access_leg.arrival_s - access_leg.departure_s, 300);
    let egress_leg = network
        .legs
        .iter()
        .find(|leg| leg.leg_type == TransitLegType::Egress)
        .expect("egress leg present");
    assert_eq!(egress_leg.arrival_s - egress_leg.departure_s, 300);

    // Network mode never teleports when the host has no estimator.
    let missing_estimator = router
        .execute_route(&request(TransitStreetAccessModel::Network))
        .expect("unreachable result is returned");
    assert_eq!(missing_estimator.outcome, TransitOutcome::Unreachable);
    assert_eq!(missing_estimator.summary.total_travel_time_s, None);

    // A missing access path omits the stop candidate even when egress is
    // routable. The fixture stops are intentionally unbound, exercising the
    // coordinate-only estimator path.
    let missing_access = router
        .execute_route_with_street_estimator(
            &request(TransitStreetAccessModel::Network),
            Some(&DirectionalEstimator {
                access: false,
                egress: true,
            }),
        )
        .expect("missing-access result is returned");
    assert_eq!(missing_access.outcome, TransitOutcome::Unreachable);

    // The reverse direction is equally strict: reachable access cannot hide
    // a missing egress path.
    let missing_egress = router
        .execute_route_with_street_estimator(
            &request(TransitStreetAccessModel::Network),
            Some(&DirectionalEstimator {
                access: true,
                egress: false,
            }),
        )
        .expect("missing-egress result is returned");
    assert_eq!(missing_egress.outcome, TransitOutcome::Unreachable);

    // A coordinate-only estimator cannot bypass an explicit platform/node
    // binding. Rich binding-aware path resolution is required for bound stops.
    let mut bound_bundle = build_bundle_from_files(
        fixture_files(),
        "abc-bound".to_string(),
        TransitImportOptions {
            name: "fixture-bound".to_string(),
            source_label: "fixture-bound".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 7,
        },
    )
    .expect("bound fixture imports");
    for (index, stop) in bound_bundle.stops.iter_mut().enumerate() {
        stop.binding = Some(TransitStopBindingTarget::Node {
            node_id: index as u32,
        });
    }
    let bound_router = PreparedTransitRouter::new(Arc::new(bound_bundle));
    let bound_fallback = bound_router
        .execute_route_with_street_estimator(
            &request(TransitStreetAccessModel::Network),
            Some(&FixedEstimator),
        )
        .expect("bound fallback result is returned");
    assert_eq!(bound_fallback.outcome, TransitOutcome::Unreachable);
}

/// Bundle-relative seconds back to a request datetime; fixtures stay inside
/// the first service day.
fn fixture_datetime(seconds: u32) -> String {
    format!(
        "2026-05-11T{:02}:{:02}:{:02}+02:00",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

fn import_fixture(files: GtfsFiles, feed: &str) -> TransitBundle {
    build_bundle_from_files(
        files,
        format!("{feed}-hash"),
        TransitImportOptions {
            name: feed.to_string(),
            source_label: feed.to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 7,
        },
    )
    .expect("fixture imports")
}

fn arrive_by_request(deadline: &str, modes: TransitModeOptions) -> TransitRouteRequest {
    TransitRouteRequest {
        route_id: "arrive-by".to_string(),
        origin: TransitPoint {
            id: "origin".to_string(),
            lon: 6.0,
            lat: 53.0,
        },
        destination: TransitPoint {
            id: "dest".to_string(),
            lon: 6.02,
            lat: 53.0,
        },
        time: TransitQueryTime {
            datetime: deadline.to_string(),
            arrive_by: true,
            search_window_s: 3600,
        },
        modes,
        returns: TransitReturnOptions::default(),
        alternatives: TransitAlternativeOptions::default(),
    }
}

fn short_access_modes() -> TransitModeOptions {
    TransitModeOptions {
        max_access_distance_m: 100.0,
        max_egress_distance_m: 100.0,
        ..TransitModeOptions::default()
    }
}

#[test]
fn arrive_by_route_takes_the_latest_departure_that_meets_the_deadline() {
    // Departures every ten minutes from 08:00; the run leaving A at 08:20
    // reaches C at 08:30, the last one that clears an 08:35 deadline.
    let bundle = import_fixture(frequency_fixture_files(), "arrive-by-frequency");
    let deadline_s = 8 * 3600 + 35 * 60;
    let request = arrive_by_request(&fixture_datetime(deadline_s), short_access_modes());

    let result = execute_transit_route(&bundle, &request).expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    let arrival_s = result.summary.arrival_s.expect("journey arrives");
    assert!(
        arrival_s <= deadline_s,
        "arrival {arrival_s} must not pass the deadline {deadline_s}"
    );
    assert_eq!(result.summary.boarding_count, 1);
    let boarding = result
        .legs
        .iter()
        .find(|leg| leg.leg_type == TransitLegType::Transit)
        .expect("journey rides transit");
    assert_eq!(boarding.departure_s, 8 * 3600 + 20 * 60);
    assert_eq!(boarding.arrival_s, 8 * 3600 + 30 * 60);
    // One second of access walking plus the boarding slack in front of the
    // 08:20 departure.
    assert_eq!(result.summary.departure_s, 8 * 3600 + 20 * 60 - 30 - 1);
    let access = result.legs.first().expect("journey starts on foot");
    assert_eq!(access.leg_type, TransitLegType::Access);
    assert_eq!(access.departure_s, result.summary.departure_s);
    assert_eq!(access.arrival_s, 8 * 3600 + 20 * 60 - 30);
}

#[test]
fn arrive_by_and_depart_after_agree_on_the_same_journey() {
    let bundle = import_fixture(frequency_fixture_files(), "arrive-by-symmetry");
    let deadline_s = 8 * 3600 + 35 * 60;
    let arrive_by = execute_transit_route(
        &bundle,
        &arrive_by_request(&fixture_datetime(deadline_s), short_access_modes()),
    )
    .expect("arrive-by route executes");
    assert_eq!(arrive_by.outcome, TransitOutcome::Scheduled);

    let mut depart_after = arrive_by_request(
        &fixture_datetime(arrive_by.summary.departure_s),
        short_access_modes(),
    );
    depart_after.time.arrive_by = false;
    let depart_after = execute_transit_route(&bundle, &depart_after).expect("route executes");

    assert_eq!(depart_after.outcome, TransitOutcome::Scheduled);
    assert_eq!(depart_after.summary.arrival_s, arrive_by.summary.arrival_s);
    assert_eq!(
        depart_after.summary.departure_s,
        arrive_by.summary.departure_s
    );
    assert_eq!(
        depart_after.summary.boarding_count,
        arrive_by.summary.boarding_count
    );
    let ridden = |result: &TransitRouteResult| {
        result
            .legs
            .iter()
            .filter(|leg| leg.leg_type == TransitLegType::Transit)
            .map(|leg| (leg.from_id.clone(), leg.to_id.clone(), leg.departure_s))
            .collect::<Vec<_>>()
    };
    assert_eq!(ridden(&depart_after), ridden(&arrive_by));
}

#[test]
fn arrive_by_transfer_respects_the_minimum_transfer_time() {
    // T1 reaches platform B1 at 08:10; T2 leaves platform B2 at 08:15, so the
    // transfer only works while the slack plus the walk fits in five minutes.
    let bundle = import_fixture(two_line_transfer_fixture_files(), "arrive-by-transfer");
    let deadline_s = 8 * 3600 + 40 * 60;
    // The prepared router caches the reverse index across requests.
    let router = PreparedTransitRouter::new(Arc::new(bundle));
    let result = router
        .execute_route(&arrive_by_request(
            &fixture_datetime(deadline_s),
            short_access_modes(),
        ))
        .expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(result.summary.boarding_count, 2);
    let transfer = result
        .legs
        .iter()
        .find(|leg| leg.leg_type == TransitLegType::Transfer)
        .expect("journey transfers on foot");
    assert_eq!(transfer.from_id, "B1");
    assert_eq!(transfer.to_id, "B2");
    // The walk starts a transfer slack after alighting and has to finish a
    // boarding slack before the second departure.
    assert_eq!(transfer.departure_s, 8 * 3600 + 10 * 60 + 120);
    assert!(transfer.arrival_s <= 8 * 3600 + 15 * 60 - 30);
    assert!(result.summary.arrival_s.expect("journey arrives") <= deadline_s);

    let mut tight = arrive_by_request(&fixture_datetime(deadline_s), short_access_modes());
    tight.modes.transfer_slack_s = 400;
    let tight = router.execute_route(&tight).expect("route executes");
    assert_eq!(tight.outcome, TransitOutcome::Unreachable);
}

#[test]
fn arrive_by_reports_unreachable_when_no_service_meets_the_deadline() {
    let bundle = import_fixture(fixture_files(), "arrive-by-unreachable");
    let result = execute_transit_route(
        &bundle,
        &arrive_by_request(&fixture_datetime(7 * 3600), short_access_modes()),
    )
    .expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Unreachable);
    assert!(result.legs.is_empty());
    assert!(!result.diagnostics.is_empty());
}

#[test]
fn arrive_by_service_area_reports_latest_departures_towards_the_target() {
    let bundle = import_fixture(fixture_files(), "arrive-by-service-area");
    let deadline_s = 8 * 3600 + 30 * 60;
    let request = TransitServiceAreaRequest {
        analysis_id: "arrive-by-service-area".to_string(),
        origins: vec![TransitPoint {
            id: "target".to_string(),
            lon: 6.02,
            lat: 53.0,
        }],
        time: TransitQueryTime {
            datetime: fixture_datetime(deadline_s),
            arrive_by: true,
            search_window_s: 3600,
        },
        modes: short_access_modes(),
        max_travel_time_s: 3600,
        returns: TransitServiceAreaReturnOptions::default(),
    };

    let result = execute_transit_service_area(&bundle, &request).expect("service area executes");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(result.processed_origin_count, 1);
    let stop_a = result
        .stops
        .iter()
        .find(|stop| stop.stop_id == "A")
        .expect("stop A reaches the target");
    // T1 leaves A at 08:10:30, so a traveller has to stand there by 08:10:00.
    assert_eq!(stop_a.arrival_s, 8 * 3600 + 10 * 60);
    assert_eq!(stop_a.travel_time_s, deadline_s - stop_a.arrival_s);
    assert_eq!(stop_a.boarding_count, 1);
    let stop_c = result
        .stops
        .iter()
        .find(|stop| stop.stop_id == "C")
        .expect("the target stop is reachable on foot");
    assert_eq!(stop_c.travel_time_s, deadline_s - stop_c.arrival_s);
    assert!(stop_c.arrival_s > stop_a.arrival_s);
    assert!(
        result
            .stop_segments
            .iter()
            .any(|segment| segment.from_stop_id == "A" && segment.to_stop_id == "B")
    );
}

#[test]
fn same_stop_changes_pay_transfer_slack_in_both_directions_and_service_areas() {
    let mut files = two_line_transfer_fixture_files();
    files.insert("stop_times.txt", "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,08:05:00,08:05:00,A,1\nT1,08:10:00,08:10:00,B1,2\nT2,08:15:00,08:15:00,B1,1\nT2,08:25:00,08:25:00,C,2\n".to_string());
    let router = PreparedTransitRouter::new(Arc::new(import_fixture(files, "same-stop-transfer")));
    for arrive_by in [false, true] {
        for (transfer_slack_s, expected) in [
            (270, TransitOutcome::Scheduled),
            (271, TransitOutcome::Unreachable),
        ] {
            let mut modes = short_access_modes();
            modes.max_transfer_distance_m = 0.0;
            modes.transfer_slack_s = transfer_slack_s;
            let mut request = arrive_by_request(
                &fixture_datetime(if arrive_by {
                    8 * 3600 + 40 * 60
                } else {
                    8 * 3600
                }),
                modes,
            );
            request.time.arrive_by = arrive_by;
            let result = router.execute_route(&request).expect("route executes");
            assert_eq!(
                result.outcome, expected,
                "arrive_by={arrive_by}, slack={transfer_slack_s}"
            );
            let area = router
                .execute_service_area(&TransitServiceAreaRequest {
                    analysis_id: "same-stop-transfer".to_string(),
                    origins: vec![if arrive_by {
                        request.destination.clone()
                    } else {
                        request.origin.clone()
                    }],
                    time: request.time.clone(),
                    modes: request.modes.clone(),
                    max_travel_time_s: 3600,
                    returns: TransitServiceAreaReturnOptions::default(),
                })
                .expect("service area executes");
            let reached = area
                .stops
                .iter()
                .any(|stop| stop.stop_id == if arrive_by { "A" } else { "C" });
            assert_eq!(
                reached,
                expected == TransitOutcome::Scheduled,
                "service area arrive_by={arrive_by}, slack={transfer_slack_s}"
            );
        }
    }
}

#[test]
fn departure_search_keeps_same_trip_among_unboardable_departures() {
    let mut bundle = import_fixture(fixture_files(), "interleaved-departures");
    // T1 stays at B for zero seconds. Later departures from B need boarding
    // slack and must not make the binary search skip T1's continuation.
    bundle
        .connections
        .retain(|connection| connection.departure_s < 86_400);
    bundle.connections[1].departure_s = 8 * 3600 + 15 * 60;
    for offset_s in [10, 20] {
        let mut trip = bundle.trips[0].clone();
        trip.trip_id = format!("distractor-{offset_s}");
        let trip_index = bundle.trips.len() as u32;
        bundle.trips.push(trip);
        bundle.connections.push(TransitConnection {
            trip_index,
            run_index: 100 + trip_index,
            pickup_allowed: true,
            drop_off_allowed: true,
            route_index: 0,
            from_stop_index: 1,
            to_stop_index: 2,
            departure_s: 8 * 3600 + 15 * 60 + offset_s,
            arrival_s: 8 * 3600 + 16 * 60,
        });
    }
    bundle
        .connections
        .sort_by_key(|connection| connection.departure_s);
    let mut request = arrive_by_request(&fixture_datetime(8 * 3600), short_access_modes());
    request.time.arrive_by = false;
    request.modes.max_transfer_distance_m = 0.0;
    request.modes.max_transfers = 0;
    let router = PreparedTransitRouter::new(Arc::new(bundle));
    let route = router.execute_route(&request).expect("route executes");
    assert_eq!(route.outcome, TransitOutcome::Scheduled);
    assert_eq!(route.summary.arrival_s, Some(8 * 3600 + 20 * 60 + 1));
    let area = router
        .execute_service_area(&TransitServiceAreaRequest {
            analysis_id: "interleaved".to_string(),
            origins: vec![request.origin.clone()],
            time: request.time,
            modes: request.modes,
            max_travel_time_s: 3600,
            returns: TransitServiceAreaReturnOptions::default(),
        })
        .expect("service area executes");
    let target = area
        .stops
        .iter()
        .find(|stop| stop.stop_id == "C")
        .expect("C is reachable");
    assert_eq!(target.arrival_s, 8 * 3600 + 20 * 60);
}

#[test]
fn busy_station_transfer_search_keeps_platforms_beyond_the_nearest_32() {
    let mut bundle = import_fixture(two_line_transfer_fixture_files(), "busy-station");
    bundle.stops[2].lon = 6.013;
    for index in 0..40 {
        bundle.stops.push(TransitStop {
            stop_id: format!("unused-platform-{index}"),
            name: format!("Unused platform {index}"),
            lon: 6.01 + f64::from(index) * 0.00001,
            lat: 53.0,
            binding: None,
        });
    }
    // Give enough time for the 201 m walk and both slacks.
    for connection in &mut bundle.connections {
        if connection.trip_index == 1 {
            connection.departure_s += 60;
            connection.arrival_s += 60;
        }
    }
    let router = PreparedTransitRouter::new(Arc::new(bundle));
    for arrive_by in [false, true] {
        for (radius, expected) in [
            (250.0, TransitOutcome::Scheduled),
            (100.0, TransitOutcome::Unreachable),
            (250.0, TransitOutcome::Scheduled),
        ] {
            let mut request = arrive_by_request(
                &fixture_datetime(if arrive_by {
                    8 * 3600 + 40 * 60
                } else {
                    8 * 3600
                }),
                short_access_modes(),
            );
            request.time.arrive_by = arrive_by;
            request.modes.max_transfer_distance_m = radius;
            let route = router.execute_route(&request).expect("route executes");
            assert_eq!(
                route.outcome, expected,
                "radius={radius}, arrive_by={arrive_by}"
            );
        }
    }
}

#[test]
fn rejected_short_leg_does_not_prune_a_valid_later_journey() {
    let mut files = fixture_files();
    files.insert(
        "trips.txt",
        "route_id,service_id,trip_id,trip_headsign\nR,WEEK,SHORT,C\nR,WEEK,LONG,C\n".to_string(),
    );
    files.insert("stop_times.txt", "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nSHORT,08:05:00,08:05:00,A,1\nSHORT,08:06:00,08:06:00,C,2\nLONG,08:10:00,08:10:00,A,1\nLONG,08:25:00,08:25:00,C,2\n".to_string());
    let router = PreparedTransitRouter::new(Arc::new(import_fixture(files, "minimum-pruning")));
    let mut request = arrive_by_request(&fixture_datetime(8 * 3600), short_access_modes());
    request.time.arrive_by = false;
    request.modes.min_transit_leg_duration_s = 600;
    let route = router.execute_route(&request).expect("route executes");
    assert_eq!(route.outcome, TransitOutcome::Scheduled);
    assert_eq!(route.summary.arrival_s, Some(8 * 3600 + 25 * 60 + 1));
}

#[test]
fn rejected_short_leg_does_not_prune_a_valid_earlier_arrive_by_journey() {
    let mut files = fixture_files();
    files.insert(
        "trips.txt",
        "route_id,service_id,trip_id,trip_headsign\nR,WEEK,SHORT,C\nR,WEEK,LONG,C\n".to_string(),
    );
    files.insert("stop_times.txt", "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nLONG,08:05:00,08:05:00,A,1\nLONG,08:20:00,08:20:00,C,2\nSHORT,08:25:00,08:25:00,A,1\nSHORT,08:26:00,08:26:00,C,2\n".to_string());
    let router =
        PreparedTransitRouter::new(Arc::new(import_fixture(files, "minimum-pruning-reverse")));
    let mut request =
        arrive_by_request(&fixture_datetime(8 * 3600 + 30 * 60), short_access_modes());
    request.modes.min_transit_leg_duration_s = 600;
    let route = router.execute_route(&request).expect("route executes");
    assert_eq!(route.outcome, TransitOutcome::Scheduled);
    assert_eq!(route.summary.departure_s, 8 * 3600 + 5 * 60 - 31);
}

#[test]
fn gtfs_pickup_and_drop_off_restrictions_allow_riding_through_a_stop() {
    let mut files = fixture_files();
    files.insert("stop_times.txt", "trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type\nT1,08:05:00,08:05:00,A,1,0,0\nT1,08:10:00,08:10:00,B,2,1,1\nT1,08:20:00,08:20:00,C,3,0,0\n".to_string());
    let bundle = import_fixture(files, "restricted-stop");
    assert!(!bundle.connections[0].drop_off_allowed);
    assert!(!bundle.connections[1].pickup_allowed);
    let router = PreparedTransitRouter::new(Arc::new(bundle));
    for arrive_by in [false, true] {
        let mut request = arrive_by_request(
            &fixture_datetime(if arrive_by {
                8 * 3600 + 30 * 60
            } else {
                8 * 3600
            }),
            short_access_modes(),
        );
        request.time.arrive_by = arrive_by;
        request.modes.max_transfer_distance_m = 0.0;
        assert_eq!(
            router.execute_route(&request).unwrap().outcome,
            TransitOutcome::Scheduled
        );
        let mut alight = request.clone();
        alight.destination.lon = 6.01;
        assert_eq!(
            router.execute_route(&alight).unwrap().outcome,
            TransitOutcome::Unreachable
        );
        let mut board = request.clone();
        board.origin.lon = 6.01;
        assert_eq!(
            router.execute_route(&board).unwrap().outcome,
            TransitOutcome::Unreachable
        );
        let area = router
            .execute_service_area(&TransitServiceAreaRequest {
                analysis_id: "restricted-stop".to_string(),
                origins: vec![if arrive_by {
                    request.destination.clone()
                } else {
                    request.origin.clone()
                }],
                time: request.time,
                modes: request.modes,
                max_travel_time_s: 3600,
                returns: TransitServiceAreaReturnOptions::default(),
            })
            .unwrap();
        assert!(!area.stops.iter().any(|stop| stop.stop_id == "B"));
        assert!(
            area.stops
                .iter()
                .any(|stop| stop.stop_id == if arrive_by { "A" } else { "C" })
        );
    }
}

#[test]
fn frequency_departures_and_service_days_have_distinct_vehicle_runs() {
    let bundle = import_fixture(frequency_fixture_files(), "run-identities");
    let mut runs = BTreeMap::<u32, Vec<&TransitConnection>>::new();
    for connection in &bundle.connections {
        runs.entry(connection.run_index)
            .or_default()
            .push(connection);
    }
    assert_eq!(runs.len(), 4 * 7);
    for connections in runs.values() {
        assert_eq!(connections.len(), 2);
        assert_eq!(connections[0].trip_index, connections[1].trip_index);
        assert_eq!(connections[0].arrival_s, connections[1].departure_s);
    }
    let imported_again = import_fixture(frequency_fixture_files(), "run-identities");
    assert_eq!(
        bincode::serialize(&bundle).unwrap(),
        bincode::serialize(&imported_again).unwrap()
    );
}

#[test]
fn changing_frequency_runs_counts_as_a_transfer_and_keeps_separate_legs() {
    let mut bundle = import_fixture(frequency_fixture_files(), "run-change");
    // Model partial service: the 08:00 vehicle terminates at B and only the
    // 08:10 vehicle serves B to C. Their shared GTFS trip_id is not a vehicle.
    bundle.connections.retain(|connection| {
        (connection.from_stop_index == 0 && connection.departure_s == 8 * 3600)
            || (connection.from_stop_index == 1 && connection.departure_s == 8 * 3600 + 15 * 60)
    });
    let router = PreparedTransitRouter::new(Arc::new(bundle));
    for arrive_by in [false, true] {
        let mut request = arrive_by_request(
            &fixture_datetime(if arrive_by {
                8 * 3600 + 30 * 60
            } else {
                7 * 3600 + 55 * 60
            }),
            short_access_modes(),
        );
        request.time.arrive_by = arrive_by;
        request.modes.max_transfer_distance_m = 0.0;
        request.modes.max_transfers = 0;
        assert_eq!(
            router.execute_route(&request).unwrap().outcome,
            TransitOutcome::Unreachable
        );
        request.modes.max_transfers = 1;
        let result = router.execute_route(&request).unwrap();
        assert_eq!(result.outcome, TransitOutcome::Scheduled);
        assert_eq!(result.summary.boarding_count, 2);
        let legs = result
            .legs
            .iter()
            .filter(|leg| leg.leg_type == TransitLegType::Transit)
            .collect::<Vec<_>>();
        assert_eq!(legs.len(), 2);
        assert_eq!(legs[0].trip_id, legs[1].trip_id);
        assert_ne!(legs[0].run_index, legs[1].run_index);
    }
}

#[test]
fn stop_spatial_index_matches_exact_distance_at_boundaries_poles_and_dateline() {
    use crate::legs::haversine_m;
    use crate::runtime::StopSpatialIndex;
    let mut bundle = import_fixture(fixture_files(), "spatial-extremes");
    let points = [
        (0.01, 0.0),
        (-179.999, 0.0),
        (179.999, 0.0),
        (90.0, 89.999),
        (-90.0, 89.999),
    ];
    bundle.stops = points
        .iter()
        .enumerate()
        .map(|(index, &(lon, lat))| TransitStop {
            stop_id: index.to_string(),
            name: index.to_string(),
            lon,
            lat,
            binding: None,
        })
        .collect();
    let index = StopSpatialIndex::new(&bundle.stops);
    for (lon, lat, radius) in [
        (0.0, 0.0, 1111.95),
        (179.999, 0.0, 300.0),
        (-179.999, 0.0, 300.0),
        (0.0, 89.999, 300.0),
    ] {
        let mut found = index
            .nearby_stops(&bundle, lon, lat, radius)
            .iter()
            .map(|stop| stop.stop_index)
            .collect::<Vec<_>>();
        found.sort_unstable();
        let expected = bundle
            .stops
            .iter()
            .enumerate()
            .filter(|(_, stop)| haversine_m(lon, lat, stop.lon, stop.lat) <= radius)
            .map(|(index, _)| index as u32)
            .collect::<Vec<_>>();
        assert_eq!(found, expected, "search at {lon}, {lat}, radius {radius}");
    }
}

#[test]
fn arranged_pickup_and_drop_off_are_excluded_without_booking_support() {
    for restriction in [2, 3] {
        for (pickup, drop_off) in [(restriction, 0), (0, restriction)] {
            let mut files = fixture_files();
            files.insert("stop_times.txt", format!("trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type\nT1,08:05:00,08:05:00,A,1,{pickup},0\nT1,08:20:00,08:20:00,C,2,0,{drop_off}\n"));
            let router =
                PreparedTransitRouter::new(Arc::new(import_fixture(files, "arranged-service")));
            for arrive_by in [false, true] {
                let mut request = arrive_by_request(
                    &fixture_datetime(if arrive_by {
                        8 * 3600 + 30 * 60
                    } else {
                        8 * 3600
                    }),
                    short_access_modes(),
                );
                request.time.arrive_by = arrive_by;
                assert_eq!(
                    router.execute_route(&request).unwrap().outcome,
                    TransitOutcome::Unreachable
                );
            }
        }
    }
}

#[test]
#[ignore = "timing probe; run with --release --ignored --nocapture"]
fn benchmark_sparse_feed_preparation_and_repeated_routes() {
    use std::time::Instant;
    let mut bundle = import_fixture(fixture_files(), "sparse-preparation");
    // Distant stops expose preparation work that should depend on the
    // requested transfer radius, not a search for 32 remote neighbours.
    for index in 0..1_000 {
        bundle.stops.push(TransitStop {
            stop_id: format!("distant-{index}"),
            name: format!("Distant {index}"),
            lon: -150.0 + f64::from(index % 100) * 3.0,
            lat: -60.0 + f64::from(index / 100) * 10.0,
            binding: None,
        });
    }
    let start = Instant::now();
    let router = PreparedTransitRouter::new(Arc::new(bundle));
    let preparation = start.elapsed();
    let mut request = arrive_by_request(&fixture_datetime(8 * 3600), short_access_modes());
    request.time.arrive_by = false;
    let start = Instant::now();
    for _ in 0..100 {
        let result = router
            .execute_route(std::hint::black_box(&request))
            .unwrap();
        assert_eq!(result.outcome, TransitOutcome::Scheduled);
        std::hint::black_box(result);
    }
    eprintln!(
        "sparse feed: 1003 stops; preparation={preparation:?}; 100 routes including cold transfer cache={:?}",
        start.elapsed()
    );
}

#[test]
fn repeated_stop_vehicle_positions_preserve_pickup_and_full_loop_geometry() {
    let mut files = fixture_files();
    files.insert("stop_times.txt", "trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type\nT1,08:00:00,08:00:00,A,1,0,0\nT1,08:10:00,08:10:00,B,2,1,1\nT1,08:20:00,08:20:00,A,3,1,1\nT1,08:30:00,08:30:00,C,4,0,0\n".to_string());
    let router = PreparedTransitRouter::new(Arc::new(import_fixture(files, "repeated-stop")));
    for arrive_by in [false, true] {
        let mut request = arrive_by_request(
            &fixture_datetime(if arrive_by {
                8 * 3600 + 40 * 60
            } else {
                7 * 3600 + 55 * 60
            }),
            short_access_modes(),
        );
        request.time.arrive_by = arrive_by;
        request.modes.max_transfers = 0;
        request.modes.max_transfer_distance_m = 0.0;
        request.returns.include_stops = true;
        request.returns.include_stop_segments = true;
        request.returns.include_geometry = true;
        let result = router.execute_route(&request).unwrap();
        assert_eq!(
            result.outcome,
            TransitOutcome::Scheduled,
            "arrive_by={arrive_by}"
        );
        let transit = result
            .legs
            .iter()
            .find(|leg| leg.leg_type == TransitLegType::Transit)
            .unwrap();
        assert_eq!(transit.departure_s, 8 * 3600);
        assert_eq!(transit.arrival_s, 8 * 3600 + 30 * 60);
        assert_eq!(
            transit
                .geometry
                .iter()
                .map(|point| point[0])
                .collect::<Vec<_>>(),
            vec![6.0, 6.01, 6.0, 6.02]
        );
        assert_eq!(result.stop_segments.len(), 3);
        let area = router
            .execute_service_area(&TransitServiceAreaRequest {
                analysis_id: "repeated-stop".to_string(),
                origins: vec![if arrive_by {
                    request.destination.clone()
                } else {
                    request.origin.clone()
                }],
                time: request.time,
                modes: request.modes,
                max_travel_time_s: 3600,
                returns: TransitServiceAreaReturnOptions::default(),
            })
            .unwrap();
        let endpoint = area
            .stops
            .iter()
            .find(|stop| stop.stop_id == if arrive_by { "A" } else { "C" })
            .expect("endpoint is reachable");
        assert_eq!(
            endpoint.arrival_s,
            if arrive_by {
                8 * 3600 - 30
            } else {
                8 * 3600 + 30 * 60
            }
        );
    }
}
