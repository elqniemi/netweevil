use super::*;

fn english() -> Phrasebook {
    Phrasebook::for_language(None).0
}

fn two_ride_journey() -> TransitRouteResult {
    serde_json::from_value(json!({
        "route_id":"station-transfer", "time_context":{"agency_timezone":"Asia/Hong_Kong","time_origin_unix_s":1788969600},
        "outcome":"scheduled", "summary":{"departure_s":0,"arrival_s":120,"total_travel_time_s":120,"transit_time_s":70,"access_egress_time_s":20,"transfer_time_s":15,"wait_time_s":15,"boarding_count":2},
        "diagnostics":[], "legs":[
            {"leg_type":"access","from_id":"O","to_id":"P1","from_name":"Origin","to_name":"Platform 1","departure_s":0,"arrival_s":10,"street_mode":"walk","geometry":[[114.1,22.3,10],[114.1,22.3,-12]]},
            {"leg_type":"transit","from_id":"P1","to_id":"P2","from_name":"Platform 1","to_name":"Platform 2","departure_s":20,"arrival_s":50,"mode":"subway","route_id":"R1","route_short_name":"Island","trip_id":"T1","headsign":"Harbour","geometry":[[114.1,22.3,-12],[114.2,22.3,-12]]},
            {"leg_type":"transfer","from_id":"P2","to_id":"P3","from_name":"Platform 2","to_name":"Tram platform","departure_s":50,"arrival_s":65,"street_mode":"walk","geometry":[[114.2,22.3,-12],[114.2,22.3,4]]},
            {"leg_type":"transit","from_id":"P3","to_id":"P4","from_name":"Tram platform","to_name":"Pier","departure_s":70,"arrival_s":110,"mode":"tram","route_id":"R2","route_short_name":"T2","trip_id":"T2","headsign":"Pier","geometry":[[114.2,22.3,4],[114.3,22.3,4]]},
            {"leg_type":"egress","from_id":"P4","to_id":"D","from_name":"Pier","to_name":"Destination","departure_s":110,"arrival_s":120,"street_mode":"walk","geometry":[[114.3,22.3,4],[114.3,22.31,4]]}
        ]
    })).unwrap()
}

#[test]
fn orders_two_rides_station_transfer_and_source_walking_instructions() {
    let result = two_ride_journey();
    let stops = BTreeMap::from([(
        "P1".into(),
        GtfsStopContext {
            stop_name: "Platform 1".into(),
            station_name: Some("Central".into()),
            platform_code: Some("1".into()),
        },
    )]);
    let mut walking = vec![Vec::new(); result.legs.len()];
    walking[2] = vec![
        json!({"kind":"stairs","instruction":"Take the stairs up.","source":"network_facility_attribute"}),
        json!({"kind":"lift","instruction":"Take the lift up.","source":"network_facility_attribute"}),
    ];
    let steps = build_directions(&result, &stops, &walking, english()).unwrap();
    assert_eq!(
        steps
            .iter()
            .map(|step| step.kind.as_str())
            .collect::<Vec<_>>(),
        [
            "access", "wait", "board", "ride", "alight", "transfer", "wait", "board", "ride",
            "alight", "egress"
        ]
    );
    assert_eq!(steps.iter().map(|step| step.duration_s).sum::<u32>(), 120);
    assert!(
        steps[2]
            .instruction
            .contains("Island toward Harbour at Central, Platform 1")
    );
    assert_eq!(steps[2].platform.as_deref(), Some("1"));
    assert_eq!(steps[2].location, Some([114.1, 22.3, -12.0]));
    assert_eq!(steps[5].walking_maneuvers, walking[2]);
    assert_eq!(steps[7].route_short_name.as_deref(), Some("T2"));
    assert_eq!(steps[10].arrival_s, 120);
    assert!(steps[10].arrival_datetime.ends_with("00:02:00+08:00"));
    for (index, step) in steps.iter().enumerate() {
        assert_eq!(step.sequence, index + 1);
    }
}

#[test]
fn unreachable_journey_has_no_instructions_and_filtered_legs_are_not_invented() {
    let mut result = two_ride_journey();
    result.outcome = TransitOutcome::Unreachable;
    assert!(
        build_directions(&result, &BTreeMap::new(), &[], english())
            .unwrap()
            .is_empty()
    );
    result.outcome = TransitOutcome::Scheduled;
    result
        .legs
        .retain(|leg| leg.mode != Some(netweevil_transit::TransitMode::Tram));
    let steps = build_directions(&result, &BTreeMap::new(), &[], english()).unwrap();
    assert_eq!(steps.iter().filter(|step| step.kind == "board").count(), 1);
    assert!(
        steps
            .iter()
            .all(|step| step.route_id.as_deref() != Some("R2"))
    );
}

#[test]
fn journey_steps_follow_the_requested_language() {
    let result = two_ride_journey();
    let (dutch, warning) = Phrasebook::for_language(Some("nl"));
    assert_eq!(warning, None);
    let steps = build_directions(&result, &BTreeMap::new(), &[], dutch).unwrap();
    let instructions = steps
        .iter()
        .map(|step| step.instruction.as_str())
        .collect::<Vec<_>>();
    assert_eq!(instructions[0], "Loop van Origin naar Platform 1.");
    assert_eq!(
        instructions[2],
        "Stap in Island richting Harbour bij Platform 1."
    );
    assert_eq!(instructions[4], "Stap uit bij Platform 2.");
}
