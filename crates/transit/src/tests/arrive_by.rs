use super::fixtures::*;
use crate::*;

#[test]
fn arrive_by_route_takes_the_latest_departure_that_meets_the_deadline() {
    // Departures every ten minutes from 08:00; the run leaving A at 08:20
    // reaches C at 08:30, the last one that clears an 08:35 deadline.
    let bundle = import(frequency_feed(), 7);
    let deadline_s = hm(8, 35);

    let result = execute_transit_route(&bundle, &arrive_by(deadline_s)).expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    let arrival_s = result.summary.arrival_s.expect("journey arrives");
    assert!(
        arrival_s <= deadline_s,
        "arrival {arrival_s} must not pass the deadline {deadline_s}"
    );
    assert_eq!(result.summary.boarding_count, 1);
    let ride = first_leg(&result, TransitLegType::Transit);
    assert_eq!(ride.departure_s, hm(8, 20));
    assert_eq!(ride.arrival_s, hm(8, 30));
    // One second of access walking plus the boarding slack in front of the
    // 08:20 departure.
    assert_eq!(result.summary.departure_s, hm(8, 20) - 30);
    let access = result.legs.first().expect("journey starts on foot");
    assert_eq!(access.leg_type, TransitLegType::Access);
    assert_eq!(access.departure_s, result.summary.departure_s);
    assert_eq!(access.arrival_s, hm(8, 20) - 30);
}

#[test]
fn arrive_by_and_depart_after_agree_on_the_same_journey() {
    let bundle = import(frequency_feed(), 7);
    let arrive_by =
        execute_transit_route(&bundle, &arrive_by(hm(8, 35))).expect("arrive-by route executes");
    assert_eq!(arrive_by.outcome, TransitOutcome::Scheduled);

    let depart_after = execute_transit_route(&bundle, &depart_at(arrive_by.summary.departure_s))
        .expect("route executes");

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
        legs_of(result, TransitLegType::Transit)
            .into_iter()
            .map(|leg| (leg.from_id.clone(), leg.to_id.clone(), leg.departure_s))
            .collect::<Vec<_>>()
    };
    assert_eq!(ridden(&depart_after), ridden(&arrive_by));
}

#[test]
fn arrive_by_transfer_respects_the_minimum_transfer_time() {
    // T1 reaches platform B1 at 08:10; T2 leaves platform B2 at 08:15, so the
    // transfer only works while the slack plus the walk fits in five minutes.
    let deadline_s = hm(8, 40);
    // Both requests share one prepared router and its cached reverse index.
    let router = router(import(two_line_feed(), 7));
    let result = router
        .execute_route(&arrive_by(deadline_s))
        .expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(result.summary.boarding_count, 2);
    let transfer = first_leg(&result, TransitLegType::Transfer);
    assert_eq!(transfer.from_id, "B1");
    assert_eq!(transfer.to_id, "B2");
    // The walk starts a transfer slack after alighting and has to finish a
    // boarding slack before the second departure.
    assert_eq!(transfer.departure_s, hm(8, 10) + 120);
    assert!(transfer.arrival_s <= hm(8, 15) - 30);
    assert!(result.summary.arrival_s.expect("journey arrives") <= deadline_s);

    let mut tight = arrive_by(deadline_s);
    tight.modes.transfer_slack_s = 400;
    let tight = router.execute_route(&tight).expect("route executes");
    assert_eq!(tight.outcome, TransitOutcome::Unreachable);
}

#[test]
fn arrive_by_reports_unreachable_when_no_service_meets_the_deadline() {
    let bundle = import(line_feed(), 7);
    let result = execute_transit_route(&bundle, &arrive_by(hm(7, 0))).expect("route executes");

    assert_eq!(result.outcome, TransitOutcome::Unreachable);
    assert!(result.legs.is_empty());
    assert!(!result.diagnostics.is_empty());
}

#[test]
fn arrive_by_service_area_reports_latest_departures_towards_the_target() {
    let bundle = import(line_feed(), 7);
    let deadline_s = hm(8, 30);

    let result = execute_transit_service_area(&bundle, &area_for(&arrive_by(deadline_s)))
        .expect("service area executes");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(result.processed_origin_count, 1);
    let stop_a = area_stop(&result, "A").expect("stop A reaches the target");
    // T1 leaves A at 08:10:30, so a traveller has to stand there by 08:10:00.
    assert_eq!(stop_a.arrival_s, hm(8, 10));
    assert_eq!(stop_a.travel_time_s, deadline_s - stop_a.arrival_s);
    assert_eq!(stop_a.boarding_count, 1);
    let stop_c = area_stop(&result, "C").expect("the target stop is reachable on foot");
    assert_eq!(stop_c.travel_time_s, deadline_s - stop_c.arrival_s);
    assert!(stop_c.arrival_s > stop_a.arrival_s);
    assert!(
        result
            .stop_segments
            .iter()
            .any(|segment| segment.from_stop_id == "A" && segment.to_stop_id == "B")
    );
}
