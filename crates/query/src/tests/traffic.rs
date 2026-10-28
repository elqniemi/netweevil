use std::sync::Arc;

use netweevil_core::{
    DirectedEdge, TRAFFIC_BUNDLE_SCHEMA_VERSION, TRAFFIC_SPEED_UNITS_PER_KPH, TopologyBundle,
    TrafficSpeedBundle,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{
    LiveTrafficDirection, LiveTrafficUpdate, PreparedRoutingEngine, RouteRequest, TrafficMode,
    TrafficService,
};

use super::fixtures::*;

fn at(value: &str) -> OffsetDateTime {
    OffsetDateTime::parse(value, &Rfc3339).unwrap()
}

/// A traffic bundle with hourly buckets. Each pattern is one day of 24
/// speeds repeated for the whole week; assignments are
/// `(feature_id, direction, pattern)`.
fn hourly_bundle(
    timezone: &str,
    days: &[[f64; 24]],
    assignments: &[(i64, i8, u32)],
) -> TrafficSpeedBundle {
    let pattern_speeds = days
        .iter()
        .flat_map(|day| {
            (0..7).flat_map(move |_| {
                day.iter()
                    .map(|kph| (kph * TRAFFIC_SPEED_UNITS_PER_KPH).round() as u16)
            })
        })
        .collect();
    let mut assignments = assignments.to_vec();
    assignments.sort_unstable();
    TrafficSpeedBundle {
        schema_version: TRAFFIC_BUNDLE_SCHEMA_VERSION,
        traffic_id: "test_traffic".into(),
        timezone: timezone.into(),
        bucket_minutes: 60,
        pattern_speeds,
        feature_ids: assignments.iter().map(|a| a.0).collect(),
        directions: assignments.iter().map(|a| a.1).collect(),
        feature_patterns: assignments.iter().map(|a| a.2).collect(),
    }
}

fn day_with(base: f64, slow_hours: &[(usize, f64)]) -> [f64; 24] {
    let mut day = [base; 24];
    for &(hour, kph) in slow_hours {
        day[hour] = kph;
    }
    day
}

fn timed_request(origin: (f64, f64), destination: (f64, f64), departure: &str) -> RouteRequest {
    let mut request = route_request(
        "traffic",
        point("o", origin.0, origin.1),
        point("d", destination.0, destination.1),
    );
    request.snap = snap_within(50.0);
    request.temporal.departure_time = Some(departure.to_string());
    request
}

/// A 5 x 5 grid of two-way streets (one source way per block side, both
/// geometry directions) with 10 m/s profile speeds and uneven lengths.
fn grid() -> (TopologyBundle, Vec<(f64, f64)>) {
    const N: usize = 5;
    let coordinate =
        |row: usize, column: usize| (6.0 + column as f64 * 0.002, 53.0 + row as f64 * 0.0012);
    let nodes = (0..N * N)
        .map(|index| coordinate(index / N, index % N))
        .collect::<Vec<_>>();
    let mut edges = Vec::<DirectedEdge>::new();
    let mut add_way = |way: i64, from: usize, to: usize, length: u32| {
        for (from, to, direction) in [(from, to, 1_i8), (to, from, -1)] {
            let id = edges.len() as u32;
            edges.push(DirectedEdge {
                source_direction: direction,
                ..edge(id, from as u32, to as u32, way, length)
            });
        }
    };
    for row in 0..N {
        for column in 0..N {
            let node = row * N + column;
            if column + 1 < N {
                add_way(
                    100 + node as i64,
                    node,
                    node + 1,
                    120 + (node as u32 * 37) % 60,
                );
            }
            if row + 1 < N {
                add_way(
                    200 + node as i64,
                    node,
                    node + N,
                    110 + (node as u32 * 23) % 70,
                );
            }
        }
    }
    (topology("traffic-grid", &nodes, &edges), nodes)
}

fn profile_metrics(topology: &TopologyBundle) -> netweevil_core::CompiledProfileBundle {
    let times = (0..topology.edge_count())
        .map(|edge| f64::from(topology.routing_edge(edge).length_m) / 10.0)
        .collect::<Vec<_>>();
    turn_weighted_metrics(&times)
}

/// The CCH customized for a departure bucket (historical speeds plus live
/// entries valid at departure) must find routes as cheap as the exact
/// engine running on the very same per-edge metric, in every bucket.
#[test]
fn bucket_customized_cch_matches_exact_engine_on_the_bucket_metric() {
    let (topology, nodes) = grid();
    let metrics = profile_metrics(&topology);
    let horizontal_jam = day_with(36.0, &[(8, 6.0), (9, 12.0)]);
    let vertical_jam = day_with(36.0, &[(17, 5.0)]);
    let fast_night = day_with(36.0, &[(3, 90.0)]);
    let mut assignments = Vec::new();
    for way in 100..125_i64 {
        assignments.push((way, 1, 0));
        assignments.push((way, -1, if way % 2 == 0 { 0 } else { 2 }));
    }
    for way in 200..220_i64 {
        assignments.push((way, 1, 1));
    }
    let bundle = hourly_bundle(
        "UTC",
        &[horizontal_jam, vertical_jam, fast_night],
        &assignments,
    );
    let base = Arc::new(cch_engine(topology.clone(), &metrics));
    let service = TrafficService::new(
        Arc::new(topology.clone()),
        Some(&bundle),
        [("car".to_string(), base.clone())],
        8,
    )
    .unwrap();
    let router = service.router("car").unwrap();
    let now = at("2026-10-05T12:00:00Z");
    service
        .apply_live(
            &[
                LiveTrafficUpdate {
                    feature_id: 207,
                    direction: LiveTrafficDirection::Both,
                    speed_kph: None,
                    closed: true,
                    clear: false,
                    expires_at: None,
                    ttl_s: Some(3600.0),
                },
                LiveTrafficUpdate {
                    feature_id: 112,
                    direction: LiveTrafficDirection::Forward,
                    speed_kph: Some(4.0),
                    closed: false,
                    clear: false,
                    expires_at: None,
                    ttl_s: Some(3600.0),
                },
            ],
            false,
            now,
        )
        .unwrap();

    let pairs = [
        (0, 24),
        (4, 20),
        (2, 22),
        (10, 14),
        (21, 3),
        (12, 0),
        (24, 6),
    ];
    let mut costs_by_departure = Vec::new();
    for departure in [
        "2026-10-05T08:30:00Z",
        "2026-10-05T17:30:00Z",
        "2026-10-05T03:10:00Z",
        "2026-10-05T12:10:00Z",
    ] {
        let departure = at(departure);
        let (engine, selection) = router.engine_at(departure, true).unwrap();
        assert!(
            engine
                .effective_engine_description(crate::EngineMode::Auto)
                .acceleration
                .contains("cch"),
            "bucket engines route on the customized hierarchy"
        );
        let exact = PreparedRoutingEngine::new(
            Arc::new(topology.clone()),
            Arc::new(router.bucket_metrics(departure, true)),
            None,
        )
        .unwrap();
        let mut costs = Vec::new();
        for &(from, to) in &pairs {
            let mut request = timed_request(nodes[from], nodes[to], "unused");
            request.temporal = Default::default();
            let accelerated = engine.execute_route(&request).unwrap();
            let reference = exact.execute_route(&request).unwrap();
            let (a, r) = (
                accelerated.summary.total_generalized_cost,
                reference.summary.total_generalized_cost,
            );
            assert!(
                (a - r).abs() <= 1e-6 * r.max(1.0) + 0.02,
                "{} {from}->{to}: cch {a} vs exact {r}",
                selection.departure_time.as_deref().unwrap()
            );
            costs.push(r);
        }
        costs_by_departure.push(costs);
    }
    // Buckets and live entries change the optimum.
    assert_ne!(costs_by_departure[0], costs_by_departure[2]);
    assert_ne!(costs_by_departure[1], costs_by_departure[2]);
    assert_ne!(costs_by_departure[3], costs_by_departure[2]);

    // 04:xx has the same speed column as 03:xx except the fast-night
    // pattern, and 05:xx equals 04:xx: those buckets share one engine.
    let (_, first) = router.engine_at(at("2026-10-05T04:10:00Z"), true).unwrap();
    let (_, second) = router.engine_at(at("2026-10-06T05:40:00Z"), true).unwrap();
    assert_eq!(
        first.bucket.as_ref().unwrap().metric_bucket,
        second.bucket.as_ref().unwrap().metric_bucket
    );
    assert_eq!(second.engine_cache, "hit");
}

/// Parallel one-way routes s -> a -> t (A) and s -> b -> t (B), 1000 m per
/// edge at a 36 km/h profile speed. Edge a -> t runs at 72 km/h until
/// 09:00 local time and at 6 km/h afterwards.
fn diamond() -> (TopologyBundle, (f64, f64), (f64, f64)) {
    let nodes = [(6.0, 53.0), (6.01, 53.005), (6.01, 52.995), (6.02, 53.0)];
    let topology = topology(
        "traffic-diamond",
        &nodes,
        &[
            edge(0, 0, 1, 1, 1000),
            edge(1, 1, 3, 2, 1000),
            edge(2, 0, 2, 3, 1000),
            edge(3, 2, 3, 4, 1000),
        ],
    );
    (topology, nodes[0], nodes[3])
}

fn diamond_service() -> (TrafficService, (f64, f64), (f64, f64)) {
    let (topology, origin, destination) = diamond();
    let metrics = turn_weighted_metrics(&[100.0, 100.0, 100.0, 100.0]);
    let rush = {
        let mut day = [6.0; 24];
        day[..9].fill(72.0);
        day
    };
    let bundle = hourly_bundle("Europe/Amsterdam", &[rush], &[(2, 1, 0)]);
    let base = Arc::new(cch_engine(topology.clone(), &metrics));
    let service = TrafficService::new(
        Arc::new(topology),
        Some(&bundle),
        [("car".to_string(), base)],
        4,
    )
    .unwrap();
    (service, origin, destination)
}

fn travel_seconds(result: &crate::RouteResult) -> f64 {
    let departure = at(result.summary.departure_time.as_deref().unwrap());
    let arrival = at(result.summary.arrival_time.as_deref().unwrap());
    (arrival - departure).as_seconds_f64()
}

/// The departure-bucket route is optimal for the departure metric; its
/// arrival time integrates the speed in force along the path (FIFO across
/// a bucket boundary). The exact time-dependent engine sees the later slow
/// regime and takes the other branch.
#[test]
fn departure_bucket_eta_integrates_speeds_across_bucket_boundaries() {
    let (service, origin, destination) = diamond_service();
    let router = service.router("car").unwrap();

    // Enter a -> t at 08:59:30 local: 30 s at 20 m/s cover 600 m, the last
    // 400 m take 240 s at 6 km/h.
    let request = timed_request(origin, destination, "2026-10-05T08:57:50+02:00");
    let (result, selection) = router.execute_route(&request, None).unwrap().unwrap();
    assert_eq!(selection.engine, "departure_bucket_cch");
    assert_eq!(selection.bucket.as_ref().unwrap().label, "Mon 08:00");
    assert_eq!(selection.bucket_boundaries_crossed, Some(1));
    assert_eq!(result.summary.segment_count, 2);
    assert!((travel_seconds(&result) - 370.0).abs() < 1e-6);
    assert!((result.summary.total_generalized_cost - 370.0).abs() < 1e-6);

    // Entering after the boundary is entirely slow: still FIFO.
    let later = timed_request(origin, destination, "2026-10-05T08:58:30+02:00");
    let (later, _) = router.execute_route(&later, None).unwrap().unwrap();
    assert!((travel_seconds(&later) - 700.0).abs() < 1e-6);

    let mut exact = later_request(origin, destination);
    exact.temporal.traffic = TrafficMode::TimeDependent;
    let (exact, selection) = router.execute_route(&exact, None).unwrap().unwrap();
    assert_eq!(selection.engine, "time_dependent_exact");
    assert!((travel_seconds(&exact) - 200.0).abs() < 1e-6);

    // Without traffic the request keeps the plain temporal engine.
    let mut off = later_request(origin, destination);
    off.temporal.traffic = TrafficMode::Off;
    assert!(router.execute_route(&off, None).unwrap().is_none());
}

fn later_request(origin: (f64, f64), destination: (f64, f64)) -> RouteRequest {
    timed_request(origin, destination, "2026-10-05T08:58:30+02:00")
}

/// A live closure is customized before it is published: engines handed out
/// earlier keep routing on the old metric, departures while it is valid
/// avoid the feature, and departures after its expiry use it again.
#[test]
fn live_closure_publishes_new_engine_and_expires() {
    let (service, origin, destination) = diamond_service();
    let router = service.router("car").unwrap();
    // 03:00 local: branch A is fast (72 km/h on a -> t).
    let departure = "2026-10-05T03:00:00+02:00";
    let before = router.engine_at(at(departure), true).unwrap().0;
    let route = |departure: &str| {
        router
            .execute_route(&timed_request(origin, destination, departure), None)
            .unwrap()
            .unwrap()
    };
    assert!((travel_seconds(&route(departure).0) - 150.0).abs() < 1e-6);

    let report = service
        .apply_live(
            &[LiveTrafficUpdate {
                feature_id: 1,
                direction: LiveTrafficDirection::Both,
                speed_kph: None,
                closed: true,
                clear: false,
                expires_at: Some("2026-10-05T03:30:00+02:00".into()),
                ttl_s: None,
            }],
            false,
            at("2026-10-05T02:59:00+02:00"),
        )
        .unwrap();
    assert_eq!(report.version, 1);
    assert_eq!(report.profiles[0].engine_cache, "miss");

    let (closed, selection) = route(departure);
    assert_eq!(
        selection.engine_cache, "hit",
        "the swap prepared this engine"
    );
    assert_eq!(selection.live_entries_applied, 1);
    assert!((travel_seconds(&closed) - 200.0).abs() < 1e-6);

    let mut stale = timed_request(origin, destination, departure);
    stale.temporal = Default::default();
    let old = before.execute_route(&stale).unwrap();
    assert_eq!(old.summary.total_generalized_cost, 150.0);

    let mut historical = timed_request(origin, destination, departure);
    historical.temporal.traffic = TrafficMode::Historical;
    let (historical, _) = router.execute_route(&historical, None).unwrap().unwrap();
    assert!((travel_seconds(&historical) - 150.0).abs() < 1e-6);

    let (reopened, selection) = route("2026-10-05T03:31:00+02:00");
    assert_eq!(selection.live_entries_applied, 0);
    assert!((travel_seconds(&reopened) - 150.0).abs() < 1e-6);
}
