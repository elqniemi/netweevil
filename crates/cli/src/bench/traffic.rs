//! `netweevil bench traffic`: bucket customization, live swap and route
//! latency for traffic-aware routing on a real dataset.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Args;
use netweevil_persist::{read_traffic_manifest, write_json};
use netweevil_query::{
    LiveTrafficDirection, LiveTrafficUpdate, PreparedRoutingEngine, RouteRequest, TrafficMode,
    TrafficRouter, parse_datetime,
};
use serde::Serialize;

use super::{CorpusRow, percentile, read_corpus, route_request};
use crate::traffic::load_traffic_service;

#[derive(Args, Debug)]
pub(crate) struct BenchTrafficArgs {
    /// Id of an imported dataset.
    #[arg(long)]
    dataset: String,
    /// Routing profile file (YAML or TOML) of a motorized mode.
    #[arg(long)]
    profile: PathBuf,
    /// Id of an imported historical traffic set.
    #[arg(long)]
    traffic: String,
    /// Route corpus (CSV) from `bench corpus`.
    #[arg(long)]
    corpus: PathBuf,
    /// Departures whose buckets are customized and routed.
    #[arg(long, value_delimiter = ',', default_values_t = [
        "2026-10-05T08:15:00+02:00".to_string(),
        "2026-10-05T12:30:00+02:00".to_string(),
        "2026-10-05T17:30:00+02:00".to_string(),
        "2026-10-04T03:00:00+02:00".to_string(),
    ])]
    departures: Vec<String>,
    /// Live batch sizes to publish (alternating closures and slowdowns).
    #[arg(long, value_delimiter = ',', default_values_t = [1_usize, 100, 1000])]
    live_batches: Vec<usize>,
    /// Compare bucket routes with the exact engine on the same metric.
    #[arg(long)]
    verify_exact: bool,
    /// Corpus routes to time with the exact time-dependent engine.
    #[arg(long, default_value_t = 0)]
    time_dependent_routes: usize,
    /// Number of passes over the corpus for each measurement.
    #[arg(long, default_value_t = 5)]
    passes: usize,
    /// Write the machine-readable report here.
    #[arg(long)]
    json: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct Latency {
    count: usize,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

fn latency(mut samples: Vec<f64>) -> Latency {
    samples.sort_by(f64::total_cmp);
    Latency {
        count: samples.len(),
        p50_ms: percentile(&samples, 0.5),
        p95_ms: percentile(&samples, 0.95),
        max_ms: samples.last().copied().unwrap_or_default(),
    }
}

#[derive(Debug, Serialize)]
struct BucketReport {
    departure: String,
    bucket: String,
    metric_bucket: u32,
    customize_ms: f64,
    cache: &'static str,
    routes: Latency,
    /// Mean |integrated ETA - departure-bucket metric time| / metric time.
    mean_eta_vs_bucket_metric: f64,
    routes_crossing_buckets: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    exact_mismatches: Option<usize>,
}

#[derive(Debug, Serialize)]
struct LiveReport {
    batch: usize,
    snapshot_ms: f64,
    customize_ms: f64,
    swap_ms: f64,
    /// Route latency of a concurrent reader while the swap ran.
    reader_during_swap: Latency,
    routes_after_swap: Latency,
}

#[derive(Debug, Serialize)]
struct TrafficBenchReport {
    dataset: String,
    profile_id: String,
    traffic_id: String,
    edges: usize,
    bundle_bytes: u64,
    pattern_count: usize,
    feature_count: usize,
    matched_edge_count: usize,
    distinct_bucket_metrics: usize,
    load_ms: f64,
    static_routes: Latency,
    buckets: Vec<BucketReport>,
    live: Vec<LiveReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_dependent_routes: Option<Latency>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rss_per_bucket_engine_bytes: Option<u64>,
}

fn timed_request(row: &CorpusRow, departure: &str) -> RouteRequest {
    let mut request = route_request(row, netweevil_profile::ReturnGeometry::None);
    request.temporal.departure_time = Some(departure.to_string());
    request
}

fn current_rss_bytes() -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let kilobytes: u64 = String::from_utf8(output.stdout).ok()?.trim().parse().ok()?;
    Some(kilobytes * 1024)
}

fn time_ms<T>(job: impl FnOnce() -> T) -> (T, f64) {
    let started = Instant::now();
    let value = job();
    (value, started.elapsed().as_secs_f64() * 1000.0)
}

fn route_corpus(
    router: &TrafficRouter,
    rows: &[CorpusRow],
    departure: &str,
    passes: usize,
) -> Result<Vec<f64>> {
    let mut samples = Vec::new();
    for _ in 0..passes {
        for row in rows {
            let request = timed_request(row, departure);
            let (result, ms) = time_ms(|| router.execute_route(&request, None));
            if result.is_ok() {
                samples.push(ms);
            }
        }
    }
    Ok(samples)
}

pub(crate) fn bench_traffic(
    paths: &netweevil_persist::WorkspacePaths,
    args: BenchTrafficArgs,
) -> Result<()> {
    let rows = read_corpus(&args.corpus)?;
    let (loaded, load_ms) = time_ms(|| {
        load_traffic_service(
            paths,
            &args.dataset,
            &args.profile,
            Some(&args.traffic),
            args.departures.len() + 4,
        )
    });
    let (service, bundle, profile_id) = loaded?;
    let bundle = bundle.expect("traffic set requested");
    let manifest = read_traffic_manifest(paths, &args.traffic)?;
    let bundle_bytes = std::fs::metadata(&manifest.bundle.path)?.len();
    let router = service
        .router(&profile_id)
        .context("profile has no traffic router")?;
    let base = router.base_engine().clone();
    let historical = service.historical().expect("historical traffic loaded");

    let mut static_samples = Vec::new();
    let mut features = BTreeSet::new();
    for pass in 0..args.passes {
        for row in &rows {
            let mut request = route_request(row, netweevil_profile::ReturnGeometry::None);
            request.returns.segment_rows = pass == 0;
            let (result, ms) = time_ms(|| base.execute_route(&request));
            if let Ok(result) = result {
                static_samples.push(ms);
                for segment in result.segments.iter().flatten() {
                    features.insert(segment.source_way_id);
                }
            }
        }
    }

    let rss_before = current_rss_bytes();
    let mut buckets = Vec::new();
    for departure in &args.departures {
        let at = parse_datetime(departure)?;
        let ((engine, selection), customize_ms) = {
            let (value, ms) = time_ms(|| router.engine_at(at, true));
            (value?, ms)
        };
        let samples = route_corpus(&router, &rows, departure, args.passes)?;
        let mut relative = Vec::new();
        let mut crossing = 0;
        let mut exact_mismatches = None;
        let exact = args
            .verify_exact
            .then(|| {
                PreparedRoutingEngine::new(
                    Arc::new(base.topology().clone()),
                    Arc::new(router.bucket_metrics(at, true)),
                    None,
                )
            })
            .transpose()?;
        let mut mismatches = 0;
        for row in &rows {
            let request = timed_request(row, departure);
            let Some((result, selection)) = router.execute_route(&request, None)? else {
                continue;
            };
            let mut static_request = request.clone();
            static_request.temporal = Default::default();
            let bucket_route = engine.execute_route(&static_request)?;
            relative.push(
                (result.summary.total_travel_time_s - bucket_route.summary.total_travel_time_s)
                    .abs()
                    / bucket_route.summary.total_travel_time_s.max(1e-9),
            );
            crossing += usize::from(selection.bucket_boundaries_crossed.unwrap_or(0) > 0);
            if let Some(exact) = &exact {
                let reference = exact.execute_route(&static_request)?;
                let (a, r) = (
                    bucket_route.summary.total_generalized_cost,
                    reference.summary.total_generalized_cost,
                );
                if (a - r).abs() > 1e-6 * r.max(1.0) + 0.05 {
                    mismatches += 1;
                    println!("  mismatch {}: cch {a:.3} exact {r:.3}", row.id);
                }
            }
        }
        if exact.is_some() {
            exact_mismatches = Some(mismatches);
        }
        let bucket = selection.bucket.expect("historical bucket");
        buckets.push(BucketReport {
            departure: departure.clone(),
            bucket: bucket.label,
            metric_bucket: bucket.metric_bucket,
            customize_ms,
            cache: selection.engine_cache,
            routes: latency(samples),
            mean_eta_vs_bucket_metric: relative.iter().sum::<f64>() / relative.len().max(1) as f64,
            routes_crossing_buckets: crossing,
            exact_mismatches,
        });
    }
    let rss_after = current_rss_bytes();
    let built = buckets
        .iter()
        .filter(|bucket| bucket.cache == "miss")
        .count();
    let rss_per_bucket_engine_bytes = rss_before
        .zip(rss_after)
        .filter(|_| built > 0)
        .map(|(before, after)| after.saturating_sub(before) / built as u64);

    // Live swaps at the first departure while a reader keeps routing.
    let departure = args.departures.first().context("no departures")?.clone();
    let now = parse_datetime(&departure)?;
    // Features on the corpus routes first, then any other source features.
    let mut features = features.into_iter().collect::<Vec<_>>();
    let topology = base.topology();
    let mut seen = features.iter().copied().collect::<BTreeSet<_>>();
    let largest_batch = args.live_batches.iter().copied().max().unwrap_or(0);
    for edge in 0..topology.edge_count() {
        if features.len() >= largest_batch {
            break;
        }
        let feature = topology.routing_edge(edge).source_way_id;
        if seen.insert(feature) {
            features.push(feature);
        }
    }
    let mut live = Vec::new();
    for &batch in &args.live_batches {
        let updates = (0..batch)
            .map(|index| LiveTrafficUpdate {
                feature_id: features[index % features.len()],
                direction: if index % 2 == 0 {
                    LiveTrafficDirection::Both
                } else {
                    LiveTrafficDirection::Forward
                },
                speed_kph: (index % 3 != 0).then_some(12.0),
                closed: index % 3 == 0,
                clear: false,
                expires_at: None,
                ttl_s: Some(1800.0),
            })
            .collect::<Vec<_>>();
        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let router = router.clone();
            let rows = rows.clone();
            let stop = stop.clone();
            let departure = departure.clone();
            std::thread::spawn(move || {
                let mut samples = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    for row in &rows {
                        let request = timed_request(row, &departure);
                        let (_, ms) = time_ms(|| router.execute_route(&request, None));
                        samples.push(ms);
                    }
                }
                samples
            })
        };
        // Let the reader warm up on the current engine first.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let report = service.apply_live(&updates, true, now)?;
        stop.store(true, Ordering::Relaxed);
        let reader_samples = reader.join().expect("reader thread");
        let after = route_corpus(&router, &rows, &departure, args.passes)?;
        live.push(LiveReport {
            batch,
            snapshot_ms: report.snapshot_ms,
            customize_ms: report.profiles.iter().map(|p| p.customize_ms).sum(),
            swap_ms: report.swap_ms,
            reader_during_swap: latency(reader_samples),
            routes_after_swap: latency(after),
        });
    }
    service.apply_live(&[], true, now)?;

    let time_dependent_routes = (args.time_dependent_routes > 0)
        .then(|| {
            let mut samples = Vec::new();
            for row in rows.iter().take(args.time_dependent_routes) {
                let mut request = timed_request(row, &departure);
                request.temporal.traffic = TrafficMode::TimeDependent;
                let (result, ms) = time_ms(|| router.execute_route(&request, None));
                result?;
                samples.push(ms);
            }
            Ok::<_, anyhow::Error>(latency(samples))
        })
        .transpose()?;

    let report = TrafficBenchReport {
        dataset: args.dataset.clone(),
        profile_id,
        traffic_id: args.traffic.clone(),
        edges: base.topology().edge_count(),
        bundle_bytes,
        pattern_count: bundle.pattern_count(),
        feature_count: bundle.feature_ids.len(),
        matched_edge_count: historical.matched_edge_count(),
        distinct_bucket_metrics: historical.distinct_bucket_metrics(),
        load_ms,
        static_routes: latency(static_samples),
        buckets,
        live,
        time_dependent_routes,
        rss_per_bucket_engine_bytes,
    };
    print_report(&report);
    if let Some(path) = &args.json {
        write_report(path, &report)?;
    }
    Ok(())
}

fn write_report(path: &Path, report: &TrafficBenchReport) -> Result<()> {
    write_json(path, report)?;
    println!("report written to {}", path.display());
    Ok(())
}

fn print_report(report: &TrafficBenchReport) {
    println!(
        "traffic '{}' on '{}' ({} edges): bundle {} bytes, {} patterns, {} directed features, {} matched edges, {} distinct bucket metrics",
        report.traffic_id,
        report.dataset,
        report.edges,
        report.bundle_bytes,
        report.pattern_count,
        report.feature_count,
        report.matched_edge_count,
        report.distinct_bucket_metrics,
    );
    println!(
        "static routes: p50 {:.2} ms p95 {:.2} ms ({} samples)",
        report.static_routes.p50_ms, report.static_routes.p95_ms, report.static_routes.count
    );
    for bucket in &report.buckets {
        println!(
            "bucket {} ({}): customize {:.0} ms ({}), routes p50 {:.2} ms p95 {:.2} ms, ETA vs bucket metric {:.2}%, {} routes cross buckets{}",
            bucket.bucket,
            bucket.departure,
            bucket.customize_ms,
            bucket.cache,
            bucket.routes.p50_ms,
            bucket.routes.p95_ms,
            bucket.mean_eta_vs_bucket_metric * 100.0,
            bucket.routes_crossing_buckets,
            bucket
                .exact_mismatches
                .map_or(String::new(), |count| format!(", {count} exact mismatches")),
        );
    }
    for live in &report.live {
        println!(
            "live batch {}: snapshot {:.1} ms, customize {:.0} ms, swap {:.0} ms; reader during swap p50 {:.2} ms max {:.2} ms; routes after p50 {:.2} ms",
            live.batch,
            live.snapshot_ms,
            live.customize_ms,
            live.swap_ms,
            live.reader_during_swap.p50_ms,
            live.reader_during_swap.max_ms,
            live.routes_after_swap.p50_ms,
        );
    }
    if let Some(td) = &report.time_dependent_routes {
        println!(
            "time-dependent exact routes: p50 {:.0} ms max {:.0} ms ({} routes)",
            td.p50_ms, td.max_ms, td.count
        );
    }
    if let Some(bytes) = report.rss_per_bucket_engine_bytes {
        println!(
            "RSS per customized bucket engine: {:.1} MiB",
            bytes as f64 / 1048576.0
        );
    }
}
