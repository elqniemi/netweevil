use super::*;

fn stop(stop_id: &str, name: &str, lon: f64, lat: f64) -> ScenarioStop {
    ScenarioStop {
        stop_id: stop_id.into(),
        name: name.into(),
        lon,
        lat,
    }
}

/// A line stop timed from `average_speed_kph` and `default_dwell_s`.
fn line_stop(stop_id: &str) -> ScenarioLineStop {
    ScenarioLineStop {
        stop_id: stop_id.into(),
        travel_s: None,
        dwell_s: None,
    }
}

fn build_options(
    feed_id: &str,
    service_start_date: &str,
    service_days: u32,
) -> ScenarioBuildOptions {
    ScenarioBuildOptions {
        feed_id: feed_id.into(),
        source_label: "scenario".into(),
        service_start_date: service_start_date.into(),
        service_days,
    }
}

/// One bidirectional rail line Groningen - Heerenveen - Lelystad on weekdays:
/// half-hourly 06:00-07:00 plus an explicit 23:30 departure.
fn scenario() -> GtfsScenario {
    GtfsScenario {
        schema_version: GTFS_SCENARIO_SCHEMA_VERSION,
        scenario_id: "lelylijn".to_string(),
        label: "Lelylijn".to_string(),
        base_feed_id: None,
        output_feed_id: None,
        agency: ScenarioAgency::default(),
        service_start_date: Some("2026-05-11".to_string()),
        service_days: Some(7),
        stops: vec![
            stop("gn", "Groningen", 6.5665, 53.2107),
            stop("hv", "Heerenveen", 5.9167, 52.9600),
            stop("ly", "Lelystad", 5.4744, 52.5079),
        ],
        lines: vec![ScenarioLine {
            line_id: "LL".into(),
            short_name: "LL".into(),
            long_name: "Lelylijn".into(),
            mode: TransitMode::Rail,
            color: Some("0E7C86".into()),
            headsign: None,
            stops: vec![
                line_stop("gn"),
                ScenarioLineStop {
                    stop_id: "hv".into(),
                    travel_s: Some(1200),
                    dwell_s: Some(60),
                },
                line_stop("ly"),
            ],
            average_speed_kph: 160.0,
            default_dwell_s: 30,
            bidirectional: true,
            services: vec![ScenarioService {
                days: [true, true, true, true, true, false, false],
                windows: vec![ScenarioHeadwayWindow {
                    start: "06:00".into(),
                    end: "07:00".into(),
                    headway_min: 30,
                }],
                departures: vec!["23:30".into()],
            }],
        }],
        removed_route_ids: Vec::new(),
        created_at: String::new(),
        updated_at: String::new(),
    }
}

#[test]
fn expands_headways_and_both_directions() {
    let scenario = scenario();
    let files = scenario.gtfs_files(None).unwrap();
    let file = |name: &str| &files.iter().find(|(file, _)| *file == name).unwrap().1;
    // 3 headway departures + 1 explicit, times two directions.
    assert_eq!(file("trips.txt").lines().count() - 1, 8);
    let stop_times = file("stop_times.txt");
    assert!(stop_times.contains("LL_s1_d0_0001,06:00:00,06:00:00,gn,1"));
    assert!(stop_times.contains("LL_s1_d0_0001,06:20:00,06:21:00,hv,2"));
    // Reverse direction reuses the forward segment times in reverse order.
    assert!(stop_times.contains("LL_s1_d1_0002,06:30:00,06:30:00,ly,1"));
    let summary = scenario.summary(None).unwrap();
    assert_eq!(summary.trip_count, 8);
    assert_eq!(summary.stop_count, 3);
}

#[test]
fn builds_a_stand_alone_bundle() {
    let bundle = build_scenario_bundle(
        &scenario(),
        None,
        &build_options("lelylijn", "2026-05-11", 7),
    )
    .unwrap();
    assert_eq!(bundle.stops.len(), 3);
    assert_eq!(bundle.routes.len(), 1);
    assert_eq!(bundle.trips.len(), 8);
    // 5 weekdays x 8 trips x 2 connections.
    assert_eq!(bundle.connections.len(), 80);
    let patterns = route_patterns(&bundle, "LL", 10);
    assert_eq!(patterns.len(), 2);
    assert_eq!(patterns[0].stops.len(), 3);
}

#[test]
fn overlays_on_a_base_feed_and_removes_routes() {
    let base =
        build_scenario_bundle(&scenario(), None, &build_options("base", "2026-05-11", 7)).unwrap();
    let mut overlay = scenario();
    overlay.scenario_id = "express".into();
    overlay.base_feed_id = Some("base".into());
    // Reuses the base feed's gn and ly stops and adds a new one.
    overlay.stops = vec![stop("dr", "Drachten", 6.09, 53.11)];
    overlay.lines[0].line_id = "LLX".into();
    overlay.lines[0].stops = vec![line_stop("gn"), line_stop("dr"), line_stop("ly")];
    overlay.removed_route_ids = vec!["LL".into()];
    // The service window comes from the base feed, so the options leave it empty.
    let merged =
        build_scenario_bundle(&overlay, Some(&base), &build_options("base_express", "", 0))
            .unwrap();

    assert_eq!(merged.stops.len(), 4);
    assert_eq!(merged.routes.len(), 2);
    assert!(
        merged
            .connections
            .iter()
            .all(|connection| connection.route_index == 1)
    );
    assert_eq!(merged.connections.len(), 80);
    let gn = merged
        .stops
        .iter()
        .position(|stop| stop.stop_id == "gn")
        .unwrap() as u32;
    assert!(
        merged
            .connections
            .iter()
            .any(|connection| connection.from_stop_index == gn)
    );
    assert_eq!(merged.time_origin_unix_s, base.time_origin_unix_s);
}

#[test]
fn rejects_unknown_stop_references() {
    let mut scenario = scenario();
    scenario.lines[0].stops[1].stop_id = "missing".into();
    let error = scenario.validate(None).unwrap_err().to_string();
    assert!(error.contains("unknown stop 'missing'"));
}
