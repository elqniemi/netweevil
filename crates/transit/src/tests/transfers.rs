//! Transfer slack, transfer search radius, and GTFS `transfers.txt` semantics.

use super::fixtures::*;
use crate::*;

/// `two_line_feed` with T2 departing from B1 itself, so the change needs no walk.
const SAME_PLATFORM_STOP_TIMES: [&str; 4] = [
    "T1,08:05:00,08:05:00,A,1",
    "T1,08:10:00,08:10:00,B1,2",
    "T2,08:15:00,08:15:00,B1,1",
    "T2,08:25:00,08:25:00,C,2",
];

fn with_transfers(mut files: GtfsFiles, header: &str, rows: &[&str]) -> TransitBundle {
    files.insert("transfers.txt", csv(header, rows));
    import(files, 7)
}

/// Checks that origin -> dest is (un)reachable as both a route and a service
/// area, depart-after and arrive-by, with zero slack and a 100 m transfer radius.
fn assert_transfer_reachability(bundle: TransitBundle, expected: bool) {
    let router = router(bundle);
    for arrive_by in [false, true] {
        let mut request = timed_request(arrive_by, hm(8, 0), hm(8, 40));
        request.modes.max_transfer_distance_m = 100.0;
        request.modes.board_slack_s = 0;
        request.modes.transfer_slack_s = 0;
        let result = router.execute_route(&request).unwrap();
        assert_eq!(
            result.outcome == TransitOutcome::Scheduled,
            expected,
            "route arrive_by={arrive_by}"
        );
        let area = router.execute_service_area(&area_for(&request)).unwrap();
        assert_eq!(
            area_stop(&area, far_stop(arrive_by)).is_some(),
            expected,
            "service area arrive_by={arrive_by}"
        );
    }
}

#[test]
fn same_stop_changes_pay_transfer_slack_in_both_directions_and_service_areas() {
    let mut files = two_line_feed();
    files.insert("stop_times.txt", stop_times(&SAME_PLATFORM_STOP_TIMES));
    let router = router(import(files, 7));
    for arrive_by in [false, true] {
        // T1 arrives at 08:10 and T2 leaves at 08:15 after a 30 s boarding slack.
        for (transfer_slack_s, expected) in [
            (270, TransitOutcome::Scheduled),
            (271, TransitOutcome::Unreachable),
        ] {
            let mut request = timed_request(arrive_by, hm(8, 0), hm(8, 40));
            request.modes.max_transfer_distance_m = 0.0;
            request.modes.transfer_slack_s = transfer_slack_s;
            let result = router.execute_route(&request).expect("route executes");
            assert_eq!(
                result.outcome, expected,
                "arrive_by={arrive_by}, slack={transfer_slack_s}"
            );
            let area = router.execute_service_area(&area_for(&request)).unwrap();
            assert_eq!(
                area_stop(&area, far_stop(arrive_by)).is_some(),
                expected == TransitOutcome::Scheduled,
                "service area arrive_by={arrive_by}, slack={transfer_slack_s}"
            );
        }
    }
}

#[test]
fn busy_station_transfer_search_keeps_platforms_beyond_the_nearest_32() {
    let mut bundle = import(two_line_feed(), 7);
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
    let router = router(bundle);
    for arrive_by in [false, true] {
        // Returning to 250 m after 100 m checks the router's per-radius
        // transfer cache is replaced rather than reused.
        for (radius, expected) in [
            (250.0, TransitOutcome::Scheduled),
            (100.0, TransitOutcome::Unreachable),
            (250.0, TransitOutcome::Scheduled),
        ] {
            let mut request = timed_request(arrive_by, hm(8, 0), hm(8, 40));
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
fn coincident_platforms_allow_zero_time_transfers_without_walking_cycles() {
    let mut files = two_line_feed();
    files.insert(
        "stops.txt",
        csv(
            "stop_id,stop_name,stop_lat,stop_lon",
            &[
                "A,A,53,6",
                "B1,Platform1,53,6.01",
                "MID,Intermediate,53,6.01",
                "B2,Platform2,53,6.01",
                "C,C,53,6.02",
            ],
        ),
    );
    files.insert(
        "stop_times.txt",
        stop_times(&[
            "T1,08:05:00,08:05:00,A,1",
            "T1,08:10:00,08:10:00,B1,2",
            "T2,08:10:00,08:10:00,B2,1",
            "T2,08:25:00,08:25:00,C,2",
        ]),
    );
    assert_transfer_reachability(import(files, 7), true);
}

#[test]
fn forbidden_and_minimum_transfers_apply_to_same_stop_and_platform_walks() {
    for same_stop in [false, true] {
        for (rule_type, minimum, expected) in [
            (0, "", true),
            (3, "", false),
            (2, "300", true),
            (2, "301", false),
        ] {
            let mut files = two_line_feed();
            if same_stop {
                files.insert("stop_times.txt", stop_times(&SAME_PLATFORM_STOP_TIMES));
            }
            let to_stop = if same_stop { "B1" } else { "B2" };
            let bundle = with_transfers(
                files,
                "from_stop_id,to_stop_id,transfer_type,min_transfer_time",
                &[&format!("B1,{to_stop},{rule_type},{minimum}")],
            );
            assert_transfer_reachability(bundle, expected);
        }
    }
}

#[test]
fn forbidden_transfer_does_not_require_alighting_from_continuing_vehicle() {
    let bundle = with_transfers(
        line_feed(),
        "from_stop_id,to_stop_id,transfer_type",
        &["B,B,3"],
    );
    assert_transfer_reachability(bundle, true);
}

#[test]
fn station_transfer_rules_expand_to_children_and_trip_pair_overrides_broad_ban() {
    for override_rule in [false, true] {
        let mut files = two_line_feed();
        files.insert(
            "stops.txt",
            csv(
                "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station",
                &[
                    "A,A,53,6,0,",
                    "S,Station,53,6.01,1,",
                    "B1,Platform1,53,6.01,0,S",
                    "B2,Platform2,53,6.0101,0,S",
                    "C,C,53,6.02,0,",
                ],
            ),
        );
        let mut rows = vec!["S,S,3,,"];
        if override_rule {
            rows.push("B1,B2,0,T1,T2");
        }
        let bundle = with_transfers(
            files,
            "from_stop_id,to_stop_id,transfer_type,from_trip_id,to_trip_id",
            &rows,
        );
        assert_eq!(
            bundle.transfer_rules.len(),
            if override_rule { 5 } else { 4 }
        );
        assert_transfer_reachability(bundle, override_rule);
    }
}

#[test]
fn trip_specific_transfer_walk_states_preserve_legal_incoming_and_outgoing_trips() {
    // The earlier incoming trip and later outgoing trip are both forbidden.
    // Merging all on-foot labels at each stop loses the valid middle pair.
    let mut files = two_line_feed();
    files.insert(
        "trips.txt",
        csv(
            "route_id,service_id,trip_id",
            &["R1,WEEK,EARLY", "R1,WEEK,T1", "R2,WEEK,T2", "R2,WEEK,LATE"],
        ),
    );
    files.insert(
        "stop_times.txt",
        stop_times(&[
            "EARLY,08:02:00,08:02:00,A,1",
            "EARLY,08:08:00,08:08:00,B1,2",
            "T1,08:05:00,08:05:00,A,1",
            "T1,08:10:00,08:10:00,B1,2",
            "T2,08:15:00,08:15:00,B2,1",
            "T2,08:25:00,08:25:00,C,2",
            "LATE,08:18:00,08:18:00,B2,1",
            "LATE,08:28:00,08:28:00,C,2",
        ]),
    );
    let bundle = with_transfers(
        files,
        "from_stop_id,to_stop_id,transfer_type,from_trip_id,to_trip_id",
        &["B1,B2,3,,", "B1,B2,0,T1,T2"],
    );
    assert_transfer_reachability(bundle, true);
}

#[test]
fn transfer_specificity_follows_all_six_gtfs_ranks_in_both_directions() {
    // Selector columns: from_route_id, to_route_id, from_trip_id, to_trip_id,
    // ordered from least to most specific.
    let selectors = [",,,", "R1,,,", "R1,R2,,", ",,T1,", ",R2,T1,", ",,T1,T2"];
    for winning_rank in 1..selectors.len() {
        for reverse_order in [false, true] {
            let forbid = format!("B1,B2,3,{}", selectors[winning_rank - 1]);
            let allow = format!("B1,B2,0,{}", selectors[winning_rank]);
            let mut rows = [forbid.as_str(), allow.as_str()];
            if reverse_order {
                rows.reverse();
            }
            let bundle = with_transfers(
                two_line_feed(),
                "from_stop_id,to_stop_id,transfer_type,from_route_id,to_route_id,from_trip_id,to_trip_id",
                &rows,
            );
            assert_transfer_reachability(bundle, true);
        }
    }
}

#[test]
fn transfer_trip_selectors_override_matching_route_selectors() {
    let bundle = with_transfers(
        two_line_feed(),
        "from_stop_id,to_stop_id,transfer_type,from_route_id,to_route_id,from_trip_id,to_trip_id",
        &["B1,B2,3,R1,R2,,", "B1,B2,0,R1,R2,T1,T2"],
    );
    assert_transfer_reachability(bundle, true);
}

#[test]
fn equally_specific_overlapping_transfer_rules_fail_independent_of_row_order() {
    for rows in [
        ["B1,B2,0,T1,", "B1,B2,3,,T2"],
        ["B1,B2,3,,T2", "B1,B2,0,T1,"],
    ] {
        let router = router(with_transfers(
            two_line_feed(),
            "from_stop_id,to_stop_id,transfer_type,from_trip_id,to_trip_id",
            &rows,
        ));
        for arrive_by in [false, true] {
            let request = timed_request(arrive_by, hm(8, 0), hm(8, 40));
            let error = router.execute_route(&request).unwrap_err();
            assert!(error.to_string().contains("equally specific"), "{error:#}");
        }
    }
}

#[test]
fn transfer_walk_hops_cannot_bypass_forbidden_or_minimum_rules() {
    for row in ["B1,B2,3,", "B1,B2,2,301"] {
        let mut files = two_line_feed();
        // MID sits between the platforms, offering a two-hop walk around the rule.
        files.insert(
            "stops.txt",
            csv(
                "stop_id,stop_name,stop_lat,stop_lon",
                &[
                    "A,A,53,6",
                    "B1,Platform1,53,6.01",
                    "MID,Intermediate,53,6.01005",
                    "B2,Platform2,53,6.0101",
                    "C,C,53,6.02",
                ],
            ),
        );
        let bundle = with_transfers(
            files,
            "from_stop_id,to_stop_id,transfer_type,min_transfer_time",
            &[row],
        );
        assert_transfer_reachability(bundle, false);
    }
}

#[test]
fn timed_transfers_report_static_semantics_and_do_not_hold_departed_vehicles() {
    for minimum in ["0", "301"] {
        let bundle = with_transfers(
            two_line_feed(),
            "from_stop_id,to_stop_id,transfer_type,min_transfer_time",
            &[&format!("B1,B2,1,{minimum}")],
        );
        let expected = crate::transfer_rules::TIMED_TRANSFER_DIAGNOSTIC;
        assert_eq!(transit_import_summary(&bundle).diagnostics, vec![expected]);
        let router = router(bundle.clone());
        let request = arrive_by(hm(8, 40));
        let route = router.execute_route(&request).unwrap();
        assert!(route.diagnostics.iter().any(|message| message == expected));
        let area = router.execute_service_area(&area_for(&request)).unwrap();
        assert!(area.diagnostics.iter().any(|message| message == expected));
        assert_transfer_reachability(bundle, minimum == "0");
    }
}

#[test]
fn timed_transfer_cannot_board_a_vehicle_before_the_incoming_arrival() {
    let mut files = two_line_feed();
    files.insert(
        "stop_times.txt",
        stop_times(&[
            "T1,08:05:00,08:05:00,A,1",
            "T1,08:10:00,08:10:00,B1,2",
            "T2,08:09:00,08:09:00,B2,1",
            "T2,08:25:00,08:25:00,C,2",
        ]),
    );
    let bundle = with_transfers(files, "from_stop_id,to_stop_id,transfer_type", &["B1,B2,1"]);
    assert_transfer_reachability(bundle, false);
}

#[test]
fn inactive_trip_transfer_rules_do_not_become_general_restrictions() {
    let mut files = two_line_feed();
    files.insert(
        "trips.txt",
        csv(
            "route_id,service_id,trip_id",
            &["R1,WEEK,T1", "R2,WEEK,T2", "R1,INACTIVE,OFF"],
        ),
    );
    let bundle = with_transfers(
        files,
        "from_stop_id,to_stop_id,transfer_type,from_trip_id",
        &["B1,B2,3,OFF"],
    );
    assert!(bundle.transfer_rules.is_empty());
    assert_transfer_reachability(bundle, true);
}

#[test]
fn invalid_and_unsupported_transfer_rules_are_rejected() {
    for raw in [
        "from_trip_id,to_trip_id,transfer_type\nT1,T2,4\n",
        "from_trip_id,to_trip_id,transfer_type\nT1,T2,5\n",
        "from_stop_id,to_stop_id,transfer_type\nB1,B2,2\n",
        "from_stop_id,to_stop_id,transfer_type\nB1,B2,6\n",
        "from_stop_id,to_stop_id,transfer_type\nB1,B2,0\nB1,B2,3\n",
        "from_stop_id,to_stop_id,transfer_type,from_trip_id\nB1,B2,0,MISSING\n",
        "from_stop_id,to_stop_id,transfer_type,from_route_id,from_trip_id\nB1,B2,0,R2,T1\n",
    ] {
        let mut files = two_line_feed();
        files.insert("transfers.txt", raw.to_string());
        assert!(
            try_import(files, SERVICE_START, 1).is_err(),
            "should reject {raw}"
        );
    }
}
