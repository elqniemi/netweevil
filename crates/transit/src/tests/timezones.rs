//! Europe/Amsterdam service days and query times across the 2026 DST changes
//! (spring forward on 29 March, fall back on 25 October).

use super::fixtures::*;
use crate::*;

const DAILY_MORNING: [&str; 2] = ["T1,08:00:00,08:00:00,A,1", "T1,08:30:00,08:30:00,C,2"];

/// `line_feed` running every day of 2026 with the given stop times.
fn year_round_feed(stop_time_rows: &[&str]) -> GtfsFiles {
    let mut files = line_feed();
    files.insert("calendar.txt", calendar("20260101", "20261231"));
    files.insert("stop_times.txt", stop_times(stop_time_rows));
    files
}

fn year_round_router(
    start_date: &str,
    days: u32,
    stop_time_rows: &[&str],
) -> PreparedTransitRouter {
    router(try_import(year_round_feed(stop_time_rows), start_date, days).unwrap())
}

#[test]
fn gtfs_service_days_use_dst_aware_noon_minus_twelve_hours() {
    for (start, expected_steps, expected_departures) in [
        (
            "2026-03-28",
            [23 * 3600, 24 * 3600],
            [
                "2026-03-28T08:00:00+01:00",
                "2026-03-29T08:00:00+02:00",
                "2026-03-30T08:00:00+02:00",
            ],
        ),
        (
            "2026-10-24",
            [25 * 3600, 24 * 3600],
            [
                "2026-10-24T08:00:00+02:00",
                "2026-10-25T08:00:00+01:00",
                "2026-10-26T08:00:00+01:00",
            ],
        ),
    ] {
        let bundle = try_import(year_round_feed(&DAILY_MORNING), start, 3).unwrap();
        assert_eq!(bundle.agency_timezone, "Europe/Amsterdam");
        assert_eq!(bundle.connections.len(), 3);
        for (pair, expected) in bundle.connections.windows(2).zip(expected_steps) {
            assert_eq!(pair[1].departure_s - pair[0].departure_s, expected);
        }
        for (connection, expected) in bundle.connections.iter().zip(expected_departures) {
            assert_eq!(
                bundle
                    .time_context()
                    .datetime(connection.departure_s)
                    .unwrap(),
                expected
            );
        }
    }
}

#[test]
fn query_offsets_and_naive_agency_times_resolve_to_the_same_instant() {
    let router = year_round_router("2026-03-28", 3, &DAILY_MORNING);
    for arrive_by in [false, true] {
        let datetimes = if arrive_by {
            [
                "2026-03-29T08:31:00+02:00",
                "2026-03-29T06:31:00Z",
                "2026-03-29T08:31:00",
            ]
        } else {
            [
                "2026-03-29T07:59:00+02:00",
                "2026-03-29T05:59:00Z",
                "2026-03-29T07:59:00",
            ]
        };
        let mut expected_summary = None;
        for datetime in datetimes {
            let request = route_request(datetime, arrive_by);
            let route = router.execute_route(&request).unwrap();
            assert_eq!(route.outcome, TransitOutcome::Scheduled);
            assert_eq!(
                route
                    .time_context
                    .datetime(route.summary.arrival_s.unwrap())
                    .unwrap(),
                "2026-03-29T08:30:00+02:00"
            );
            let summary = (route.summary.departure_s, route.summary.arrival_s);
            if let Some(expected) = expected_summary {
                assert_eq!(summary, expected);
            }
            expected_summary = Some(summary);
            let area = router.execute_service_area(&area_for(&request)).unwrap();
            let endpoint = area_stop(&area, far_stop(arrive_by)).unwrap();
            assert_eq!(
                area.time_context.datetime(endpoint.arrival_s).unwrap(),
                if arrive_by {
                    "2026-03-29T07:59:30+02:00"
                } else {
                    "2026-03-29T08:30:00+02:00"
                }
            );
        }
    }
}

#[test]
fn ambiguous_and_nonexistent_naive_datetimes_are_rejected() {
    for (start, datetime, message) in [
        ("2026-03-28", "2026-03-29T02:30:00", "does not exist"),
        ("2026-10-24", "2026-10-25T02:30:00", "ambiguous"),
    ] {
        let router = year_round_router(start, 3, &DAILY_MORNING);
        for arrive_by in [false, true] {
            let request = route_request(datetime, arrive_by);
            let route_error = router.execute_route(&request).unwrap_err();
            assert!(route_error.to_string().contains(message));
            let area_error = router
                .execute_service_area(&area_for(&request))
                .unwrap_err();
            assert!(area_error.to_string().contains(message));
        }
    }
}

#[test]
fn explicit_offsets_distinguish_both_occurrences_of_the_autumn_hour() {
    let mut files = year_round_feed(&[
        "EARLY,01:45:00,01:45:00,A,1",
        "EARLY,02:15:00,02:15:00,C,2",
        "LATE,02:45:00,02:45:00,A,1",
        "LATE,03:15:00,03:15:00,C,2",
    ]);
    files.insert(
        "trips.txt",
        csv(
            "route_id,service_id,trip_id,trip_headsign",
            &["R,WEEK,EARLY,C", "R,WEEK,LATE,C"],
        ),
    );
    let bundle = try_import(files, "2026-10-25", 1).unwrap();
    // This day's GTFS zero is 01:00 summer time; origin still covers midnight.
    assert_eq!(
        bundle.time_context().datetime(0).unwrap(),
        "2026-10-25T00:00:00+02:00"
    );
    let router = router(bundle);
    let mut departures = Vec::new();
    for (datetime, trip_id, arrival) in [
        (
            "2026-10-25T02:30:00+02:00",
            "EARLY",
            "2026-10-25T02:15:00+01:00",
        ),
        (
            "2026-10-25T02:30:00+01:00",
            "LATE",
            "2026-10-25T03:15:00+01:00",
        ),
    ] {
        let result = router
            .execute_route(&route_request(datetime, false))
            .unwrap();
        assert_eq!(result.outcome, TransitOutcome::Scheduled);
        assert_eq!(
            result
                .time_context
                .datetime(result.summary.arrival_s.unwrap())
                .unwrap(),
            arrival
        );
        assert_eq!(
            result.legs.iter().find_map(|leg| leg.trip_id.as_deref()),
            Some(trip_id)
        );
        departures.push(result.summary.departure_s);
    }
    assert_eq!(departures[1] - departures[0], 3600);
    let result = router
        .execute_route(&route_request("2026-10-25T02:20:00+01:00", true))
        .unwrap();
    assert_eq!(
        result
            .time_context
            .datetime(result.summary.departure_s)
            .unwrap(),
        "2026-10-25T02:44:30+02:00"
    );
}

#[test]
fn overnight_service_routes_beyond_last_imported_date_across_both_dst_changes() {
    for (service_date, rows, departure, expected_boarding, expected_arrival) in [
        (
            "2026-03-28",
            ["T1,25:30:00,25:30:00,A,1", "T1,26:30:00,26:30:00,C,2"],
            "2026-03-29T01:20:00+01:00",
            "2026-03-29T01:30:00+01:00",
            "2026-03-29T03:30:00+02:00",
        ),
        (
            "2026-10-24",
            ["T1,26:15:00,26:15:00,A,1", "T1,27:15:00,27:15:00,C,2"],
            "2026-10-25T02:00:00+02:00",
            "2026-10-25T02:15:00+02:00",
            "2026-10-25T02:15:00+01:00",
        ),
    ] {
        let router = year_round_router(service_date, 1, &rows);
        let mut request = route_request(departure, false);
        request.time.search_window_s = 7200;
        let result = router.execute_route(&request).unwrap();
        assert_eq!(result.outcome, TransitOutcome::Scheduled);
        let ride = first_leg(&result, TransitLegType::Transit);
        assert_eq!(ride.arrival_s - ride.departure_s, 3600);
        assert_eq!(
            result.time_context.datetime(ride.departure_s).unwrap(),
            expected_boarding
        );
        assert_eq!(
            result
                .time_context
                .datetime(result.summary.arrival_s.unwrap())
                .unwrap(),
            expected_arrival
        );
        request.time.arrive_by = true;
        request.time.datetime = expected_arrival.to_string();
        let reverse = router.execute_route(&request).unwrap();
        assert_eq!(reverse.outcome, TransitOutcome::Scheduled);
        assert_eq!(reverse.summary.arrival_s, result.summary.arrival_s);
        assert_eq!(reverse.summary.boarding_count, 1);
    }
}
