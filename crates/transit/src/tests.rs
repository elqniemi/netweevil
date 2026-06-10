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
        returns: TransitReturnOptions::default(),
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
