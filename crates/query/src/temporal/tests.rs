use super::*;
use netweevil_core::{
    AccessMask, CacheBundleId, CompiledCostComponent, CompiledEdgeMetric, CompiledProfileBundle,
    CompiledTemporalProfile, DirectedEdge, EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION, EdgeId,
    MinuteInterval, NodeId, RoadClass, SmoothnessClass, SurfaceClass, TopologyBundle,
    TopologyEdgeLayers, TopologyNode, TravelMode,
};

#[test]
fn open_only_waits_across_a_direction_flip_boundary() {
    let timestamp = at("2026-07-10T09:59:30+08:00");
    let rules = TemporalRuleSet {
        rules: vec![every_day(10 * 60, 24 * 60, TemporalEffect::OpenOnly)],
    };
    let evaluate = |candidate| {
        evaluate_rules(
            Some(&rules),
            None,
            AccessMask::FOOT,
            1,
            candidate,
            None,
            &HashSet::new(),
        )
    };
    let (entry, _) = best_available_entry(
        timestamp,
        true,
        120.0,
        true,
        false,
        &evaluate,
        &|_, factor| 10.0 / f64::from(factor),
    )
    .unwrap();
    assert_eq!(entry, at("2026-07-10T10:00:00+08:00"));
}

#[test]
fn constant_edges_do_not_scan_the_voluntary_wait_horizon() {
    let timestamp = at("2026-07-10T09:59:30+08:00");
    let evaluations = std::cell::Cell::new(0_u32);
    let result = best_available_entry(
        timestamp,
        true,
        86_400.0,
        false,
        false,
        &|_| {
            evaluations.set(evaluations.get() + 1);
            RuleEvaluation::Available { speed_factor: 1.0 }
        },
        &|_, factor| 10.0 / f64::from(factor),
    )
    .expect("constant edge is immediately available");
    assert_eq!(result, (timestamp, 1.0));
    assert_eq!(evaluations.get(), 1);
}

#[test]
fn public_holiday_bit_replaces_weekday_bit() {
    let friday_noon = at("2026-07-10T12:00:00+08:00");
    let holiday = HashSet::from([friday_noon.date()]);
    let all_day = |day_mask, effect| TemporalRule::new(day_mask, vec![], effect);
    let friday_only = all_day(FRIDAY, TemporalEffect::Closed);
    assert!(rule_active(&friday_only, friday_noon, &HashSet::new()));
    assert!(!rule_active(&friday_only, friday_noon, &holiday));
    let every = all_day(EVERY_DAY, TemporalEffect::OpenOnly);
    assert!(rule_active(&every, friday_noon, &holiday));
}

#[test]
fn wrapping_rule_uses_the_day_on_which_the_interval_started() {
    let rule = TemporalRule {
        day_mask: FRIDAY,
        ..every_day(22 * 60, 6 * 60, TemporalEffect::OpenOnly)
    };
    for (timestamp, active) in [
        ("2026-07-10T05:00:00+08:00", false), // Friday morning: Thursday's interval
        ("2026-07-10T23:00:00+08:00", true),
        ("2026-07-11T05:00:00+08:00", true), // Saturday morning: Friday's interval
    ] {
        assert_eq!(
            rule_active(&rule, at(timestamp), &HashSet::new()),
            active,
            "{timestamp}"
        );
    }
}

/// `test_topology` with every node in `zone` and edge 0 open from 10:00 to
/// midnight.
fn opening_hours_topology(zone: Option<&str>) -> TopologyBundle {
    let mut topology = test_topology();
    set_rules(
        &mut topology,
        &[0],
        vec![every_day(10 * 60, 24 * 60, TemporalEffect::OpenOnly)],
    );
    if let Some(zone) = zone {
        topology.regions.timezones = vec![zone.to_string()];
        topology.regions.node_timezone = vec![0; topology.nodes.len()];
    }
    topology
}

#[test]
fn rules_read_the_local_clock_of_the_edge() {
    let metrics = test_metrics(false);
    // 08:00 UTC is 10:00 in Amsterdam (CEST): the edge is open there, but a
    // dataset without zones evaluates the rule on the request's UTC clock.
    let request = depart_at("2026-07-10T08:00:00Z");
    let local = crate::execute_route(
        &opening_hours_topology(Some("Europe/Amsterdam")),
        &metrics,
        &request,
    )
    .unwrap();
    assert_eq!(local.edge_path, vec![0, 1]);
    let request_clock =
        crate::execute_route(&opening_hours_topology(None), &metrics, &request).unwrap();
    assert_eq!(request_clock.edge_path, vec![2]);

    let rules = TemporalRuleSet {
        rules: vec![every_day(10 * 60, 24 * 60, TemporalEffect::OpenOnly)],
    };
    let zone = Some(chrono_tz::Asia::Hong_Kong);
    let evaluate = |instant| {
        evaluate_rules(
            Some(&rules),
            None,
            AccessMask::FOOT,
            1,
            at(instant),
            zone,
            &HashSet::new(),
        )
    };
    assert_eq!(
        evaluate("2026-07-10T02:00:00Z"),
        RuleEvaluation::Available { speed_factor: 1.0 }
    );
    assert_eq!(
        evaluate("2026-07-10T01:59:00Z"),
        RuleEvaluation::Unavailable
    );
}

#[test]
fn offset_less_departures_use_the_origin_zone_and_reject_dst_gaps_and_overlaps() {
    let topology = opening_hours_topology(Some("Europe/Amsterdam"));
    let metrics = test_metrics(false);
    let route = crate::execute_route(&topology, &metrics, &depart_at("2026-07-10T10:00")).unwrap();
    assert_eq!(route.edge_path, vec![0, 1]);
    assert_eq!(
        route.summary.departure_time.as_deref(),
        Some("2026-07-10T10:00:00+02:00")
    );
    let winter =
        crate::execute_route(&topology, &metrics, &depart_at("2026-12-01T10:00:00")).unwrap();
    assert_eq!(
        winter.summary.departure_time.as_deref(),
        Some("2026-12-01T10:00:00+01:00")
    );

    for (raw, message) in [
        ("2026-03-29T02:30:00", "does not exist"),
        ("2026-10-25T02:30:00", "ambiguous"),
    ] {
        let error = crate::execute_route(&topology, &metrics, &depart_at(raw))
            .expect_err("DST transitions need an explicit offset");
        assert!(error.to_string().contains(message), "{error}");
    }
    // An explicit offset resolves the autumn overlap.
    let second =
        crate::execute_route(&topology, &metrics, &depart_at("2026-10-25T02:30:00+01:00")).unwrap();
    assert_eq!(
        second.summary.departure_time.as_deref(),
        Some("2026-10-25T02:30:00+01:00")
    );

    let error = crate::execute_route(
        &opening_hours_topology(None),
        &metrics,
        &depart_at("2026-07-10T10:00"),
    )
    .expect_err("a dataset without zones needs an offset");
    assert!(error.to_string().contains("no UTC offset"), "{error}");
}

#[test]
fn exact_route_changes_across_direction_reversal_boundary() {
    let mut topology = test_topology();
    set_rules(
        &mut topology,
        &[0],
        vec![
            every_day(0, 10 * 60, TemporalEffect::BackwardOnly),
            every_day(10 * 60, 24 * 60, TemporalEffect::ForwardOnly),
        ],
    );
    topology.edge_layers.routing[0].source_direction = 1;
    let metrics = test_metrics(false);

    let before = crate::execute_route(&topology, &metrics, &depart_at("2026-07-10T09:59:00+08:00"))
        .expect("pre-flip route succeeds");
    let after = crate::execute_route(&topology, &metrics, &depart_at("2026-07-10T10:01:00+08:00"))
        .expect("post-flip route succeeds");

    assert_eq!(before.edge_path, vec![2]);
    assert_eq!(before.summary.total_travel_time_s, 100.0);
    assert_eq!(after.edge_path, vec![0, 1]);
    assert_eq!(after.summary.total_travel_time_s, 30.0);
}

#[test]
fn earliest_arrival_incumbent_does_not_replace_a_cheaper_slower_route() {
    let topology = test_topology();
    let mut metrics = test_metrics(false);
    metrics.edge_metrics[2].travel_time_s = Some(5.0);
    metrics.edge_metrics[2].generalized_cost = Some(100.0);

    let route = crate::execute_route(&topology, &metrics, &depart_at("2026-07-10T10:00:00+08:00"))
        .expect("generalized search improves the earliest-arrival incumbent");
    assert_eq!(route.edge_path, vec![0, 1]);
    assert_eq!(route.summary.total_generalized_cost, 30.0);
    assert_eq!(route.summary.total_travel_time_s, 30.0);
}

#[test]
fn exact_route_waits_for_an_open_only_edge_when_fifo_waiting_is_enabled() {
    let mut topology = test_topology();
    set_rules(
        &mut topology,
        &[0],
        vec![every_day(10 * 60, 24 * 60, TemporalEffect::OpenOnly)],
    );
    let route = crate::execute_route(
        &topology,
        &test_metrics(true),
        &depart_at("2026-07-10T09:59:30+08:00"),
    )
    .expect("waiting route succeeds");

    assert_eq!(route.edge_path, vec![0, 1]);
    assert_eq!(route.summary.waiting_time_s, 30.0);
    assert_eq!(route.summary.total_travel_time_s, 60.0);
    assert_eq!(
        route.summary.arrival_time.as_deref(),
        Some("2026-07-10T10:00:30+08:00")
    );
}

#[test]
fn exact_route_voluntarily_waits_for_a_faster_speed_factor_regime() {
    let mut topology = test_topology();
    set_rules(&mut topology, &[0], slow_until_ten_then_fast());
    let route = crate::execute_route(
        &topology,
        &test_metrics(true),
        &depart_at("2026-07-10T09:59:50+08:00"),
    )
    .expect("waiting for the faster regime produces the earliest route");

    assert_eq!(route.edge_path, vec![0, 1]);
    assert_eq!(route.summary.waiting_time_s, 10.0);
    assert_eq!(route.summary.total_travel_time_s, 31.0);
    assert_eq!(
        route.summary.arrival_time.as_deref(),
        Some("2026-07-10T10:00:21+08:00")
    );
}

#[test]
fn speed_factor_rules_without_a_wait_policy_keep_the_fifo_envelope() {
    let mut topology = test_topology();
    set_rules(&mut topology, &[0], slow_until_ten_then_fast());
    // Approaching the open edge more slowly to enter the faster regime is
    // available without a wait policy, so arrival stays FIFO.
    let route = crate::execute_route(
        &topology,
        &test_metrics(false),
        &depart_at("2026-07-10T09:59:50+08:00"),
    )
    .expect("the FIFO envelope routes without a wait policy");
    assert_eq!(route.edge_path, vec![0, 1]);
    assert_eq!(route.summary.waiting_time_s, 10.0);
    assert_eq!(
        route.summary.arrival_time.as_deref(),
        Some("2026-07-10T10:00:21+08:00")
    );

    // A faster regime starting after the immediate exit is not waited for.
    let requested = at("2026-07-10T09:58:00+08:00");
    let later = evaluate_edge_at(
        &topology,
        &test_metrics(false),
        &TemporalContext::default(),
        0,
        requested,
        1.0,
    )
    .expect("open edge");
    assert_eq!(later.entry_time, requested);
    assert!(((later.exit_time - requested).as_seconds_f64() - 100.0).abs() < 1e-3);

    // Closed edges are still never waited for.
    set_rules(
        &mut topology,
        &[0],
        vec![every_day(10 * 60, 24 * 60, TemporalEffect::OpenOnly)],
    );
    let route = crate::execute_route(
        &topology,
        &test_metrics(false),
        &depart_at("2026-07-10T09:59:50+08:00"),
    )
    .expect("the direct edge remains");
    assert_eq!(route.edge_path, vec![2]);
}

#[test]
fn speed_factor_waiting_rejects_an_overlay_component_with_nondominated_choices() {
    let mut topology = test_topology();
    set_rules(&mut topology, &[0], slow_until_ten_then_fast());
    let mut metrics = test_metrics(true);
    metrics.components = vec![component("exposure", false, Some("exposure_factor"))];
    let boundary = at("2026-07-10T10:00:00+08:00");
    let exposure_on = |feature| {
        overlay_context(
            feature,
            boundary,
            boundary + Duration::hours(1),
            "exposure_factor",
            1.0,
        )
    };
    let context = exposure_on(1);

    let requested = at("2026-07-10T09:59:50+08:00");
    let waited = evaluate_edge_at(&topology, &metrics, &context, 0, requested, 1.0)
        .expect("voluntary wait option exists");
    let mut slow_only = topology.clone();
    set_rules(
        &mut slow_only,
        &[0],
        vec![every_day(0, 24 * 60, TemporalEffect::SpeedFactor(0.1))],
    );
    let immediate = evaluate_edge_at(&slow_only, &metrics, &context, 0, requested, 1.0)
        .expect("immediate slow traversal exists");
    assert!(waited.exit_time < immediate.exit_time);
    assert!(waited.generalized_cost > immediate.generalized_cost);
    assert!(waited.components["exposure"] > immediate.components["exposure"]);

    let error = validate_speed_factor_wait_policy(&topology, &metrics, &context)
        .expect_err("the early-arrival and low-exposure choices are nondominated");
    assert!(error.to_string().contains("'exposure'"), "{error}");

    validate_speed_factor_wait_policy(&topology, &metrics, &exposure_on(999))
        .expect("an overlay on an unrelated feature does not affect speed-edge exactness");

    let mut travel_scaled_metrics = test_metrics(true);
    travel_scaled_metrics.components = vec![component("uncovered_time", true, None)];
    validate_speed_factor_wait_policy(
        &topology,
        &travel_scaled_metrics,
        &TemporalContext::default(),
    )
    .expect("a faster earliest-exit choice also dominates a plain travel-scaled component");
}

#[test]
fn reverse_segments_match_forward_voluntary_speed_factor_waiting() {
    let mut topology = test_topology();
    set_rules(&mut topology, &[0], slow_until_ten_then_fast());
    for allow_wait in [true, false] {
        assert_reverse_segments_match_forward(&topology, &test_metrics(allow_wait));
    }
}

fn assert_reverse_segments_match_forward(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
) {
    let context = TemporalContext::default();
    let range = TimeWindow {
        start: at("2026-07-10T09:58:00+08:00"),
        end: at("2026-07-10T10:01:00+08:00"),
    };
    let segments = build_edge_arrival_segments(topology, metrics, &context, 0, 1.0, range);

    let mut requested = range.start;
    while requested <= range.end {
        let forward = evaluate_edge_at(topology, metrics, &context, 0, requested, 1.0)
            .expect("edge is traversable");
        let segment = segments
            .iter()
            .find(|segment| match segment {
                EdgeArrivalSegment::Affine { entries, .. }
                | EdgeArrivalSegment::Constant { entries, .. } => entries.contains(requested),
            })
            .expect("reverse model covers every forward entry");
        let (actual_entry, exit) = match segment {
            EdgeArrivalSegment::Affine { duration, .. } => (requested, requested + *duration),
            EdgeArrivalSegment::Constant {
                actual_entry, exit, ..
            } => (*actual_entry, *exit),
        };
        assert_eq!(actual_entry, forward.entry_time);
        assert_eq!(exit, forward.exit_time);
        requested += Duration::seconds(1);
    }
}

#[test]
fn arrive_by_finds_a_late_feasible_departure_when_lookback_start_is_closed() {
    let mut topology = test_topology();
    set_rules(
        &mut topology,
        &[0, 1, 2],
        vec![every_day(10 * 60, 24 * 60, TemporalEffect::OpenOnly)],
    );
    let request = arrive_by("2026-07-10T10:01:00+08:00", 3_600.0);

    let route = crate::execute_route(&topology, &test_metrics(false), &request)
        .expect("a departure after the closed lookback probe must be found");
    let departure = at(route.summary.departure_time.as_deref().unwrap());
    let arrival = at(route.summary.arrival_time.as_deref().unwrap());
    assert!(departure >= at("2026-07-10T10:00:00+08:00"));
    assert!(arrival <= at("2026-07-10T10:01:00+08:00"));
    assert_eq!(route.edge_path, vec![0, 1]);
}

#[test]
fn arrive_by_does_not_skip_a_narrow_later_opening_window() {
    let mut topology = test_topology();
    set_rules(
        &mut topology,
        &[0, 1, 2],
        vec![
            every_day(9 * 60, 10 * 60, TemporalEffect::OpenOnly),
            every_day(11 * 60, 11 * 60 + 1, TemporalEffect::OpenOnly),
        ],
    );
    let request = arrive_by("2026-07-10T12:00:00+08:00", 4.0 * 3_600.0);

    let route = crate::execute_route(&topology, &test_metrics(false), &request)
        .expect("later one-minute opening must not be skipped");
    let departure = at(route.summary.departure_time.as_deref().unwrap());
    assert!(departure >= at("2026-07-10T11:00:00+08:00"));
    assert!(departure < at("2026-07-10T11:01:00+08:00"));
}

#[test]
fn arrive_by_breaks_equal_latest_departures_by_generalized_cost() {
    let mut topology = test_topology();
    topology.push_edge(DirectedEdge {
        edge_id: EdgeId(3),
        source_way_id: 4,
        ..topology.edge(1)
    });
    topology.edge_based_topology = crate::build_edge_based_topology_fallback(&topology);

    let mut metrics = test_metrics(false);
    // Exclude the unrelated direct edge. Both shared-prefix paths take 30
    // seconds, but the edge inserted first has a much higher scalar cost.
    metrics.edge_metrics[1].generalized_cost = Some(200.0);
    metrics.edge_metrics[2].travel_time_s = None;
    metrics.edge_metrics[2].generalized_cost = None;
    metrics.edge_metrics.push(edge_metric(3, 20.0));

    let route = crate::execute_route(
        &topology,
        &metrics,
        &arrive_by("2026-07-10T12:00:00+08:00", 3_600.0),
    )
    .expect("equal latest departures retain the cheaper suffix");
    assert_eq!(route.edge_path, vec![0, 3]);
    assert_eq!(route.summary.total_generalized_cost, 30.0);
    assert_eq!(
        route.summary.departure_time.as_deref(),
        Some("2026-07-10T11:59:30+08:00")
    );
}

#[test]
fn temporal_scenario_can_enable_a_materialized_direction_hidden_from_static_routing() {
    let mut topology = test_topology();
    topology.push_edge(DirectedEdge {
        source_way_id: 1,
        source_direction: -1,
        flags: EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION,
        ..test_edge(3, 1, 0, 100)
    });
    topology.edge_based_topology = crate::build_edge_based_topology_fallback(&topology);
    let mut metrics = test_metrics(false);
    metrics.edge_metrics.push(edge_metric(3, 10.0));
    let mut request = depart_at("2026-07-10T12:00:00+08:00");
    request.origin = point("upper", 6.001);
    request.destination = point("lower", 6.0);

    let mut static_request = request.clone();
    static_request.temporal = Default::default();
    assert!(crate::execute_route(&topology, &metrics, &static_request).is_err());

    let static_graph = crate::build_routing_graph(&topology, &metrics).unwrap();
    assert!(!static_graph.edge_costs[3].is_finite());
    let context = TemporalContext::new(
        Some(ScenarioOverlay {
            id: Some("reverse".to_string()),
            features: vec![ScenarioFeatureOverride {
                source_feature_id: 1,
                force_closed: false,
                force_open: false,
                replace_rules: true,
                rules: vec![TemporalRule::new(
                    EVERY_DAY,
                    vec![],
                    TemporalEffect::BackwardOnly,
                )],
                speed_factor: None,
            }],
        }),
        HashSet::new(),
        Vec::new(),
    );
    let route = execute_temporal_route_with_graph(
        &topology,
        &metrics,
        &static_graph,
        &request,
        &context,
        None,
    )
    .expect("scenario direction rule enables materialized reverse edge");
    assert_eq!(route.edge_path, vec![3]);
}

#[test]
fn temporal_overlay_updates_component_vector_and_scalar_cost() {
    let topology = test_topology();
    let mut metrics = test_metrics(false);
    metrics.edge_metrics[0].generalized_cost = Some(30.0);
    metrics.components = vec![CompiledCostComponent {
        weight: 2.0,
        edge_values: vec![10.0, 20.0, 100.0],
        invert_overlay: true,
        ..component("sun_time", true, Some("shade_fraction"))
    }];
    let timestamp = at("2026-07-10T12:00:00+08:00");
    let context = overlay_context(
        1,
        timestamp - Duration::minutes(1),
        timestamp + Duration::minutes(1),
        "shade_fraction",
        0.75,
    );
    let evaluated = evaluate_edge_at(&topology, &metrics, &context, 0, timestamp, 1.0)
        .expect("edge is available");
    assert_eq!(evaluated.components["sun_time"], 2.5);
    assert_eq!(evaluated.generalized_cost, 15.0);
}

fn at(value: &str) -> OffsetDateTime {
    parse_datetime(value).unwrap()
}

fn every_day(start_minute: u16, end_minute: u16, effect: TemporalEffect) -> TemporalRule {
    TemporalRule::new(
        EVERY_DAY,
        vec![MinuteInterval {
            start_minute,
            end_minute,
        }],
        effect,
    )
}

/// Crawling before 10:00 and fast after it, so entering just before the
/// boundary is slower than waiting for it.
fn slow_until_ten_then_fast() -> Vec<TemporalRule> {
    vec![
        every_day(0, 10 * 60, TemporalEffect::SpeedFactor(0.1)),
        every_day(10 * 60, 24 * 60, TemporalEffect::SpeedFactor(10.0)),
    ]
}

fn set_rules(topology: &mut TopologyBundle, edges: &[usize], rules: Vec<TemporalRule>) {
    topology.temporal_rule_sets = vec![TemporalRuleSet { rules }];
    for &edge in edges {
        topology.edge_layers.routing[edge].temporal_rule_id = Some(0);
    }
}

/// An overlay value on one source feature over `[start, end)`.
fn overlay_context(
    feature: i64,
    start: OffsetDateTime,
    end: OffsetDateTime,
    name: &str,
    value: f64,
) -> TemporalContext {
    TemporalContext::new(
        None,
        HashSet::new(),
        vec![TemporalOverlaySeries {
            by_feature: HashMap::from([(
                feature,
                vec![TemporalOverlayInterval {
                    start_unix_s: start.unix_timestamp(),
                    end_unix_s: end.unix_timestamp(),
                    values: BTreeMap::from([(name.to_string(), value)]),
                }],
            )]),
        }],
    )
}

/// A 200-unit component concentrated on edge 0, the temporally ruled edge.
fn component(
    name: &str,
    scales_with_travel_time: bool,
    overlay_name: Option<&str>,
) -> CompiledCostComponent {
    CompiledCostComponent {
        name: name.to_string(),
        weight: 1.0,
        edge_values: vec![200.0, 0.0, 0.0],
        scales_with_travel_time,
        overlay_name: overlay_name.map(str::to_string),
        invert_overlay: false,
    }
}

fn point(id: &str, lon: f64) -> crate::LabeledPoint {
    crate::LabeledPoint {
        id: id.to_string(),
        lon,
        lat: 53.0,
        z: None,
    }
}

fn depart_at(departure_time: &str) -> crate::RouteRequest {
    crate::RouteRequest {
        route_id: "temporal".to_string(),
        origin: point("a", 6.0),
        destination: point("c", 6.002),
        snap: crate::SnapOptions {
            max_distance_m: 20.0,
            ..Default::default()
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: netweevil_profile::ReturnConfig {
            geometry: ReturnGeometry::Full,
            segment_rows: true,
            ..Default::default()
        },
        alternatives: Default::default(),
        temporal: crate::TemporalRequestOptions {
            departure_time: Some(departure_time.to_string()),
            ..Default::default()
        },
    }
}

fn arrive_by(arrive_by: &str, lookback_s: f64) -> crate::RouteRequest {
    let mut request = depart_at(arrive_by);
    request.temporal.departure_time = None;
    request.temporal.arrive_by = Some(arrive_by.to_string());
    request.temporal.arrive_by_lookback_s = lookback_s;
    request
}

fn edge_metric(edge_id: u32, seconds: f64) -> CompiledEdgeMetric {
    CompiledEdgeMetric {
        edge_id: EdgeId(edge_id),
        travel_time_s: Some(seconds),
        generalized_cost: Some(seconds),
    }
}

/// Edges 0 (a->b, 10 s), 1 (b->c, 20 s) and 2 (a->c direct, 100 s).
fn test_metrics(allow_wait: bool) -> CompiledProfileBundle {
    CompiledProfileBundle {
        schema_version: 4,
        profile_id: "foot".to_string(),
        profile_hash: "abc".to_string(),
        mode: TravelMode::Foot,
        turn_costs: Default::default(),
        components: Vec::new(),
        temporal: CompiledTemporalProfile {
            allow_wait,
            max_wait_s: 120.0,
            ..Default::default()
        },
        source_topology_bundle_id: CacheBundleId::new("topology-test"),
        acceleration: None,
        edge_metrics: vec![
            edge_metric(0, 10.0),
            edge_metric(1, 20.0),
            edge_metric(2, 100.0),
        ],
        vehicle: Default::default(),
    }
}

fn test_edge(edge_id: u32, from: u32, to: u32, length_m: u32) -> DirectedEdge {
    DirectedEdge {
        edge_id: EdgeId(edge_id),
        from: NodeId(from),
        to: NodeId(to),
        source_way_id: i64::from(edge_id) + 1,
        length_m,
        ascent_m: 0.0,
        descent_m: 0.0,
        feature_row: netweevil_core::NO_FEATURE_ROW,
        source_direction: 1,
        temporal_rule_id: None,
        duration_s: None,
        road_class: RoadClass::Path,
        surface: SurfaceClass::Paved,
        smoothness: SmoothnessClass::Good,
        access_mask: AccessMask::new(AccessMask::FOOT),
        is_toll: false,
        max_speed_kph: None,
        lanes: None,
        name_index: None,
        geometry_offset: 0,
        geometry_len: 0,
        flags: 0,
    }
}

/// Three nodes 100 m apart along latitude 53; see `test_metrics` for edges.
fn test_topology() -> TopologyBundle {
    let node = |id, lon| TopologyNode {
        node_id: NodeId(id),
        lon,
        lat: 53.0,
        z: 0.0,
    };
    let mut topology = TopologyBundle {
        schema_version: 1,
        source_path: "test".to_string(),
        source_sha256: "abc".to_string(),
        nodes: vec![node(0, 6.0), node(1, 6.001), node(2, 6.002)],
        edge_layers: TopologyEdgeLayers::from_directed_edges(&[
            test_edge(0, 0, 1, 100),
            test_edge(1, 1, 2, 100),
            test_edge(2, 0, 2, 300),
        ]),
        turn_restrictions: Vec::new(),
        names: Vec::new(),
        edge_based_topology: Default::default(),
        spatial_index: None,
        node_component_ids: vec![0, 0, 0],
        edge_component_ids: vec![0, 0, 0],
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
        regions: Default::default(),
        elevation: Default::default(),
    };
    topology.edge_based_topology = crate::build_edge_based_topology_fallback(&topology);
    topology
}
