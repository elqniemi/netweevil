use clap::Parser;

use super::*;

fn parse_analyze(args: &[&str]) -> AnalyzeCommand {
    let cli = crate::Cli::try_parse_from(["netweevil", "analyze"].iter().chain(args))
        .unwrap_or_else(|error| panic!("{args:?} should parse: {error}"));
    let crate::Command::Analyze { command } = cli.command else {
        panic!("expected analyze command");
    };
    command
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn parses_repeatable_temporal_analyze_overrides() {
    let AnalyzeCommand::Route(args) = parse_analyze(&[
        "route",
        "--dataset",
        "hk-pedestrian",
        "--profile",
        "pedestrian.yml",
        "--request",
        "route.json",
        "--departure-time",
        "2026-07-10T09:55:00+08:00",
        "--scenario",
        "central.yml",
        "--holiday-calendar",
        "holidays.yml",
        "--overlay",
        "shade.csv",
        "--overlay",
        "heat.csv",
    ]) else {
        panic!("expected analyze route command");
    };
    let temporal = args.temporal;
    assert_eq!(
        temporal.departure_time.as_deref(),
        Some("2026-07-10T09:55:00+08:00")
    );
    assert_eq!(temporal.scenario.as_deref(), Some(Path::new("central.yml")));
    assert_eq!(
        temporal.holiday_calendar.as_deref(),
        Some(Path::new("holidays.yml"))
    );
    assert_eq!(
        temporal.overlays,
        [PathBuf::from("shade.csv"), PathBuf::from("heat.csv")]
    );
}

#[test]
fn temporal_overrides_preserve_unmentioned_request_fields() {
    let mut temporal = TemporalRequestOptions {
        departure_time: Some("2026-07-10T08:00:00+08:00".to_string()),
        scenario: Some(PathBuf::from("request-scenario.yml")),
        holiday_calendar: Some(PathBuf::from("request-holidays.yml")),
        overlays: vec![PathBuf::from("request-overlay.csv")],
        ..TemporalRequestOptions::default()
    };
    TemporalOverrideArgs {
        scenario: Some(PathBuf::from("cli-scenario.yml")),
        overlays: vec![
            PathBuf::from("cli-overlay-a.csv"),
            PathBuf::from("cli-overlay-b.csv"),
        ],
        ..TemporalOverrideArgs::default()
    }
    .apply_to(&mut temporal);

    assert_eq!(
        temporal.departure_time.as_deref(),
        Some("2026-07-10T08:00:00+08:00")
    );
    assert_eq!(
        temporal.scenario.as_deref(),
        Some(Path::new("cli-scenario.yml"))
    );
    assert_eq!(
        temporal.holiday_calendar.as_deref(),
        Some(Path::new("request-holidays.yml"))
    );
    assert_eq!(
        temporal.overlays,
        [
            PathBuf::from("request-overlay.csv"),
            PathBuf::from("cli-overlay-a.csv"),
            PathBuf::from("cli-overlay-b.csv"),
        ]
    );
}

#[test]
fn parses_transit_network_street_inputs() {
    for (subcommand, request_flag) in [
        ("transit-route", "--request"),
        ("transit-batch", "--requests"),
    ] {
        let (dataset, profile) = match parse_analyze(&[
            subcommand,
            "--feed",
            "hk-mtr",
            request_flag,
            "transit.json",
            "--street-dataset",
            "hk-pedestrian",
            "--street-profile",
            "step-free.yml",
        ]) {
            AnalyzeCommand::TransitRoute(args) => (args.street_dataset, args.street_profile),
            AnalyzeCommand::TransitBatch(args) => (args.street_dataset, args.street_profile),
            _ => panic!("expected {subcommand} command"),
        };
        assert_eq!(dataset.as_deref(), Some("hk-pedestrian"), "{subcommand}");
        assert_eq!(
            profile.as_deref(),
            Some(Path::new("step-free.yml")),
            "{subcommand}"
        );
    }
}

#[test]
fn cli_network_street_estimator_rejects_non_walk_modes() {
    let mut request =
        load_transit_request(repository().join("examples/requests/transit_openov_groningen.json"))
            .expect("transit request loads");
    request.modes.street_access = TransitStreetAccessModel::Network;
    assert!(
        validate_network_street_access_modes(&request).expect("walk-only network modes are valid")
    );

    request.modes.access = vec![AccessMode::Bicycle];
    request.modes.egress = vec![AccessMode::Bicycle];
    assert!(validate_network_street_access_modes(&request).is_err());
}

#[test]
fn hong_kong_escalator_fixture_uses_materialized_source_feature() {
    let mapping = netweevil_ingest::load_gpkg_mapping(
        repository().join("examples/ingest/hong_kong_pedestrian_mapping.yml"),
    )
    .expect("Hong Kong mapping loads");
    assert_eq!(mapping.materialize_both_directions_for.len(), 33);
    assert!(
        mapping
            .materialize_both_directions_for
            .contains(&250_009_423)
    );

    let scenario = netweevil_query::load_scenario_overlay(
        repository().join("examples/scenarios/hong_kong_central_escalator_example.yml"),
    )
    .expect("Central-Mid-Levels scenario loads");
    assert_eq!(scenario.features.len(), 1);
    assert_eq!(scenario.features[0].source_feature_id, 250_009_423);
    assert!(scenario.features[0].replace_rules);
}
