use super::fixtures::*;
use crate::*;

#[test]
fn imports_one_week_and_routes_walk_transit_walk() {
    let bundle = import(line_feed(), 7);
    assert_eq!(bundle.connections.len(), 14);
    let mut request = depart_at(hm(8, 0));
    request.returns = full_returns();

    let result = execute_transit_route(&bundle, &request).expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(result.summary.total_travel_time_s, Some(1200));
    assert_eq!(result.summary.boarding_count, 1);
    assert_eq!(legs_of(&result, TransitLegType::Transit).len(), 1);
    assert_eq!(result.stops.len(), 3);
    assert_eq!(result.stop_segments.len(), 2);
    assert_eq!(result.stops[0].stop_id, "A");
    assert_eq!(result.stops[2].stop_id, "C");
    assert_eq!(result.stop_segments[0].from_stop_id, "A");
    assert_eq!(result.stop_segments[1].to_stop_id, "C");
}

#[test]
fn filters_disallowed_transit_modes() {
    let bundle = import(line_feed(), 7);
    let mut request = depart_at(hm(8, 0));
    request.modes.transit = vec![TransitMode::Rail];
    let result = execute_transit_route(&bundle, &request).expect("route executes");
    assert_eq!(result.outcome, TransitOutcome::Unreachable);
}

#[test]
fn walking_transfers_cannot_precede_first_boarding_or_follow_last_alighting() {
    // Stops are ~335 m apart: outside the 100 m access/egress radius but inside
    // the transfer radius, so the trip is only usable via an illegal transfer.
    let stops = ["A,A,53.0,6.0", "B,B,53.0,6.005", "C,C,53.0,6.01"];
    for (case, trip, rows) in [
        (
            "walk A -> B before boarding",
            "R,WEEK,T1,C",
            ["T1,08:10:00,08:10:30,B,1", "T1,08:15:00,08:15:00,C,2"],
        ),
        (
            "walk B -> C after alighting",
            "R,WEEK,T1,B",
            ["T1,08:10:00,08:10:30,A,1", "T1,08:15:00,08:15:00,B,2"],
        ),
    ] {
        let bundle = import(feed(&stops, &["R,1,Line 1,3"], &[trip], &rows), 1);
        let mut request = depart_at(hm(8, 0));
        request.destination.lon = 6.01;
        request.modes.max_transfer_distance_m = 1_000.0;

        let result = execute_transit_route(&bundle, &request).expect("route executes");

        assert_eq!(result.outcome, TransitOutcome::Unreachable, "{case}");
        assert!(result.legs.is_empty(), "{case}");
    }
}

#[test]
fn minimum_transit_leg_is_satisfied_by_either_duration_or_distance() {
    // The only ride lasts 570 s and covers ~1.3 km.
    let bundle = import(line_feed(), 1);
    let mut request = depart_at(hm(8, 0));
    request.modes.min_transit_leg_duration_s = 600;

    let too_short = execute_transit_route(&bundle, &request).expect("route executes");
    assert_eq!(too_short.outcome, TransitOutcome::Unreachable);
    assert!(too_short.diagnostics[0].contains("minimum transit leg constraints"));

    request.modes.min_transit_leg_distance_m = 1_000.0;
    let long_enough = execute_transit_route(&bundle, &request).expect("route executes");
    assert_eq!(long_enough.outcome, TransitOutcome::Scheduled);
    assert_eq!(long_enough.summary.boarding_count, 1);
}

#[test]
fn rejected_short_leg_does_not_prune_a_valid_later_journey() {
    let mut files = line_feed();
    files.insert(
        "trips.txt",
        csv(
            "route_id,service_id,trip_id,trip_headsign",
            &["R,WEEK,SHORT,C", "R,WEEK,LONG,C"],
        ),
    );
    files.insert(
        "stop_times.txt",
        stop_times(&[
            "SHORT,08:05:00,08:05:00,A,1",
            "SHORT,08:06:00,08:06:00,C,2",
            "LONG,08:10:00,08:10:00,A,1",
            "LONG,08:25:00,08:25:00,C,2",
        ]),
    );
    let mut request = depart_at(hm(8, 0));
    request.modes.min_transit_leg_duration_s = 600;
    let route = router(import(files, 7)).execute_route(&request).unwrap();
    assert_eq!(route.outcome, TransitOutcome::Scheduled);
    assert_eq!(route.summary.arrival_s, Some(hm(8, 25)));
}

#[test]
fn rejected_short_leg_does_not_prune_a_valid_earlier_arrive_by_journey() {
    let mut files = line_feed();
    files.insert(
        "trips.txt",
        csv(
            "route_id,service_id,trip_id,trip_headsign",
            &["R,WEEK,SHORT,C", "R,WEEK,LONG,C"],
        ),
    );
    files.insert(
        "stop_times.txt",
        stop_times(&[
            "LONG,08:05:00,08:05:00,A,1",
            "LONG,08:20:00,08:20:00,C,2",
            "SHORT,08:25:00,08:25:00,A,1",
            "SHORT,08:26:00,08:26:00,C,2",
        ]),
    );
    let mut request = arrive_by(hm(8, 30));
    request.modes.min_transit_leg_duration_s = 600;
    let route = router(import(files, 7)).execute_route(&request).unwrap();
    assert_eq!(route.outcome, TransitOutcome::Scheduled);
    assert_eq!(route.summary.departure_s, hm(8, 5) - 30);
}

#[test]
fn expands_frequency_based_trips() {
    let bundle = import(frequency_feed(), 1);
    assert_eq!(bundle.connections.len(), 8);
    let mut request = depart_at(hm(8, 5));
    request.time.search_window_s = 1800;
    request.modes.transit = vec![TransitMode::Subway];

    let result = execute_transit_route(&bundle, &request).expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(result.summary.boarding_count, 1);
    assert_eq!(
        first_leg(&result, TransitLegType::Transit)
            .route_short_name
            .as_deref(),
        Some("MTR")
    );
}

#[test]
fn changing_frequency_runs_counts_as_a_transfer_and_keeps_separate_legs() {
    let mut bundle = import(frequency_feed(), 7);
    // Model partial service: the 08:00 vehicle terminates at B and only the
    // 08:10 vehicle serves B to C. Their shared GTFS trip_id is not a vehicle.
    bundle.connections.retain(|connection| {
        (connection.from_stop_index == 0 && connection.departure_s == hm(8, 0))
            || (connection.from_stop_index == 1 && connection.departure_s == hm(8, 15))
    });
    let router = router(bundle);
    for arrive_by in [false, true] {
        let mut request = timed_request(arrive_by, hm(7, 55), hm(8, 30));
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
        let rides = legs_of(&result, TransitLegType::Transit);
        assert_eq!(rides.len(), 2);
        assert_eq!(rides[0].trip_id, rides[1].trip_id);
        assert_ne!(rides[0].run_index, rides[1].run_index);
    }
}

#[test]
fn departure_search_keeps_same_trip_among_unboardable_departures() {
    let mut bundle = import(line_feed(), 7);
    // T1 stays at B for zero seconds. Later departures from B need boarding
    // slack and must not make the binary search skip T1's continuation.
    bundle
        .connections
        .retain(|connection| connection.departure_s < 86_400);
    bundle.connections[1].departure_s = hm(8, 15);
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
            departure_s: hm(8, 15) + offset_s,
            arrival_s: hm(8, 16),
        });
    }
    bundle
        .connections
        .sort_by_key(|connection| connection.departure_s);
    let mut request = depart_at(hm(8, 0));
    request.modes.max_transfer_distance_m = 0.0;
    request.modes.max_transfers = 0;
    let router = router(bundle);

    let route = router.execute_route(&request).expect("route executes");
    assert_eq!(route.outcome, TransitOutcome::Scheduled);
    assert_eq!(route.summary.arrival_s, Some(hm(8, 20)));
    let area = router.execute_service_area(&area_for(&request)).unwrap();
    assert_eq!(area_stop(&area, "C").unwrap().arrival_s, hm(8, 20));
}

#[test]
fn gtfs_pickup_and_drop_off_restrictions_allow_riding_through_a_stop() {
    let mut files = line_feed();
    files.insert(
        "stop_times.txt",
        csv(
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type",
            &[
                "T1,08:05:00,08:05:00,A,1,0,0",
                "T1,08:10:00,08:10:00,B,2,1,1",
                "T1,08:20:00,08:20:00,C,3,0,0",
            ],
        ),
    );
    let bundle = import(files, 7);
    assert!(!bundle.connections[0].drop_off_allowed);
    assert!(!bundle.connections[1].pickup_allowed);
    let router = router(bundle);
    for arrive_by in [false, true] {
        let mut request = timed_request(arrive_by, hm(8, 0), hm(8, 30));
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
        let area = router.execute_service_area(&area_for(&request)).unwrap();
        assert!(area_stop(&area, "B").is_none());
        assert!(area_stop(&area, far_stop(arrive_by)).is_some());
    }
}

#[test]
fn arranged_pickup_and_drop_off_are_excluded_without_booking_support() {
    for restriction in [2, 3] {
        for (pickup, drop_off) in [(restriction, 0), (0, restriction)] {
            let mut files = line_feed();
            files.insert(
                "stop_times.txt",
                csv(
                    "trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type",
                    &[
                        &format!("T1,08:05:00,08:05:00,A,1,{pickup},0"),
                        &format!("T1,08:20:00,08:20:00,C,2,0,{drop_off}"),
                    ],
                ),
            );
            let router = router(import(files, 7));
            for arrive_by in [false, true] {
                let request = timed_request(arrive_by, hm(8, 0), hm(8, 30));
                assert_eq!(
                    router.execute_route(&request).unwrap().outcome,
                    TransitOutcome::Unreachable
                );
            }
        }
    }
}

#[test]
fn repeated_stop_vehicle_positions_preserve_pickup_and_full_loop_geometry() {
    let mut files = line_feed();
    files.insert(
        "stop_times.txt",
        csv(
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type",
            &[
                "T1,08:00:00,08:00:00,A,1,0,0",
                "T1,08:10:00,08:10:00,B,2,1,1",
                "T1,08:20:00,08:20:00,A,3,1,1",
                "T1,08:30:00,08:30:00,C,4,0,0",
            ],
        ),
    );
    let router = router(import(files, 7));
    for arrive_by in [false, true] {
        let mut request = timed_request(arrive_by, hm(7, 55), hm(8, 40));
        request.modes.max_transfers = 0;
        request.modes.max_transfer_distance_m = 0.0;
        request.returns = full_returns();
        let result = router.execute_route(&request).unwrap();
        assert_eq!(
            result.outcome,
            TransitOutcome::Scheduled,
            "arrive_by={arrive_by}"
        );
        let ride = first_leg(&result, TransitLegType::Transit);
        assert_eq!(ride.departure_s, hm(8, 0));
        assert_eq!(ride.arrival_s, hm(8, 30));
        assert_eq!(
            ride.geometry
                .iter()
                .map(|point| point[0])
                .collect::<Vec<_>>(),
            vec![6.0, 6.01, 6.0, 6.02]
        );
        assert_eq!(result.stop_segments.len(), 3);
        let area = router.execute_service_area(&area_for(&request)).unwrap();
        let endpoint = area_stop(&area, far_stop(arrive_by)).expect("endpoint is reachable");
        let expected = if arrive_by { hm(8, 0) - 30 } else { hm(8, 30) };
        assert_eq!(endpoint.arrival_s, expected);
    }
}

#[test]
fn uses_gtfs_shapes_for_transit_geometry() {
    let bundle = import(shaped_feed(), 1);
    let mut request = depart_at(hm(8, 0));
    request.returns = full_returns();

    let result = execute_transit_route(&bundle, &request).expect("route executes");

    assert!(first_leg(&result, TransitLegType::Transit).geometry.len() > 3);
    assert!(
        result
            .stop_segments
            .iter()
            .any(|segment| segment.geometry.len() > 2)
    );
}

#[test]
fn routes_bicycle_access_with_walk_egress_when_mixed_modes_enabled() {
    let bundle = import(line_feed(), 1);
    // Origin is ~2 km from stop A: outside the walking access limit but
    // well inside the default bicycle access limit.
    let mut request = depart_at(hm(8, 0));
    request.origin.lon = 5.97;
    request.modes.access = vec![AccessMode::Bicycle];
    request.modes.egress = vec![AccessMode::Walk];
    request.modes.mixed_access_egress = true;

    let result = execute_transit_route(&bundle, &request).expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(
        first_leg(&result, TransitLegType::Access).street_mode,
        Some(AccessMode::Bicycle)
    );
    assert_eq!(
        first_leg(&result, TransitLegType::Egress).street_mode,
        Some(AccessMode::Walk)
    );
}

#[test]
fn rejects_mixed_access_egress_modes_without_opt_in() {
    let bundle = import(line_feed(), 1);
    let mut request = depart_at(hm(8, 0));
    request.origin.lon = 5.97;
    request.modes.access = vec![AccessMode::Bicycle];
    request.modes.egress = vec![AccessMode::Walk];

    let error = execute_transit_route(&bundle, &request).expect_err("mixed modes rejected");
    assert!(error.to_string().contains("mixed_access_egress"));
}

#[test]
fn routes_with_matching_bicycle_access_and_egress_without_opt_in() {
    let bundle = import(line_feed(), 1);
    let mut request = depart_at(hm(8, 0));
    request.origin.lon = 5.97;
    request.destination.lon = 6.05;
    request.modes.access = vec![AccessMode::Bicycle];
    request.modes.egress = vec![AccessMode::Bicycle];

    let result = execute_transit_route(&bundle, &request).expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert!(
        access_and_egress_legs(&result).all(|leg| leg.street_mode == Some(AccessMode::Bicycle))
    );
}

#[test]
fn zero_distance_access_and_egress_preserve_exact_timetable_boundaries() {
    let router = router(import(line_feed(), 7));
    let departure_s = hm(8, 10) + 30;
    let arrival_s = hm(8, 20);
    for arrive_by in [false, true] {
        let mut request = timed_request(arrive_by, departure_s, arrival_s);
        request.time.search_window_s = arrival_s - departure_s;
        request.modes.board_slack_s = 0;
        request.modes.transfer_slack_s = 0;
        let result = router.execute_route(&request).unwrap();
        assert_eq!(
            result.outcome,
            TransitOutcome::Scheduled,
            "arrive_by={arrive_by}"
        );
        assert_eq!(result.summary.departure_s, departure_s);
        assert_eq!(result.summary.arrival_s, Some(arrival_s));
        for leg in access_and_egress_legs(&result) {
            assert_eq!(leg.departure_s, leg.arrival_s);
        }
        let mut area = area_for(&request);
        area.max_travel_time_s = arrival_s - departure_s;
        let area = router.execute_service_area(&area).unwrap();
        let reached = area_stop(&area, far_stop(arrive_by)).expect("boundary stop is reachable");
        assert_eq!(reached.travel_time_s, arrival_s - departure_s);
        assert_eq!(
            reached.arrival_s,
            if arrive_by { departure_s } else { arrival_s }
        );
    }
}

#[test]
fn zero_distance_time_preserves_positive_rounding_and_invalid_speed_handling() {
    use crate::legs::seconds_for_distance;
    assert_eq!(seconds_for_distance(0.0, 4.8), 0);
    assert_eq!(seconds_for_distance(0.001, 4.8), 1);
    assert_eq!(seconds_for_distance(10.0, 3.6), 10);
    assert_eq!(seconds_for_distance(0.0, 0.0), u32::MAX / 4);
    assert_eq!(seconds_for_distance(0.0, -1.0), u32::MAX / 4);
    assert_eq!(seconds_for_distance(0.0, f64::NAN), 1);
    assert_eq!(seconds_for_distance(0.0, f64::INFINITY), 1);
}

#[test]
fn stop_spatial_index_matches_exact_distance_at_boundaries_poles_and_dateline() {
    use crate::legs::haversine_m;
    use crate::runtime::StopSpatialIndex;
    let mut bundle = import(line_feed(), 1);
    bundle.stops = [
        (0.01, 0.0),
        (-179.999, 0.0),
        (179.999, 0.0),
        (90.0, 89.999),
        (-90.0, 89.999),
    ]
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
