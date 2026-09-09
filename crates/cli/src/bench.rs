//! `netweevil bench` — reproducible performance benchmarks for the routing
//! engine.
//!
//! Four subcommands share one corpus format and one statistics path:
//!
//! - `corpus` samples a deterministic set of snappable origin/destination
//!   pairs, stratified into short/medium/long straight-line buckets.
//! - `run` executes a workload in-process against one shared
//!   [`PreparedRoutingEngine`], optionally across several threads.
//! - `http` executes the same corpus against a running HTTP server, either the
//!   NetWeevil, OSRM, or Valhalla route service.
//! - `compare` diffs two JSON reports produced from the same corpus, reporting
//!   both latency and route-quality agreement.
//!
//! `run` and `http` emit the same [`BenchReport`] JSON so any two runs are
//! comparable: NetWeevil against OSRM, accelerated against exact, or one commit
//! against another.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use netweevil_core::{
    CompiledProfileBundle, DatasetAccelerationBundle, TopologyBounds, TopologyBundle,
};
use netweevil_manifest::now_rfc3339;
use netweevil_persist::{
    WorkspacePaths, read_acceleration_bundle, read_compiled_profile_bundle,
    read_compiled_profile_manifests, read_dataset_manifest, read_topology_bundle,
};
use netweevil_profile::{ReturnConfig, ReturnGeometry, load_profile};
use netweevil_query::{
    AlternativeRouteOptions, AnalysisOutcome, ConnectivityPolicy, FallbackPolicy, LabeledPoint,
    PointSetDocument, PreparedRoutingEngine, RouteRequest, ServiceAreaBandMode,
    ServiceAreaBoundaryMode, ServiceAreaMultiOriginMode, ServiceAreaOutputMode,
    ServiceAreaPolygonOptions, ServiceAreaRequest, ServiceAreaReturnOptions, ServiceAreaThreshold,
    ServiceAreaThresholdMetric, SnapOptions, TemporalRequestOptions,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Snap radius used by every generated request, matching the request schema
/// default so a corpus stays valid across all three subcommands.
const DEFAULT_SNAP_DISTANCE_M: f64 = 500.0;

/// Upper straight-line bound of the `short` bucket, in metres.
const SHORT_MAX_M: f64 = 5_000.0;

/// Upper straight-line bound of the `medium` bucket, in metres.
const MEDIUM_MAX_M: f64 = 30_000.0;

const EARTH_RADIUS_M: f64 = 6_371_008.8;

const REPORT_SCHEMA_VERSION: u32 = 2;

// ---------------------------------------------------------------------------
// CLI surface
// ---------------------------------------------------------------------------

#[derive(Subcommand, Debug)]
pub(crate) enum BenchCommand {
    /// Generate a deterministic, distance-stratified route corpus.
    Corpus(BenchCorpusArgs),
    /// Benchmark a workload in-process against a shared routing engine.
    Run(BenchRunArgs),
    /// Benchmark the same corpus against a running HTTP routing server.
    Http(BenchHttpArgs),
    /// Compare two benchmark JSON reports produced from the same corpus.
    Compare(BenchCompareArgs),
}

#[derive(Args, Debug)]
pub(crate) struct BenchCorpusArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long, default_value_t = 1000)]
    count: usize,
    #[arg(long, default_value_t = 42)]
    seed: u64,
    #[arg(long)]
    out: PathBuf,
    /// Restrict sampling to `min_lon,min_lat,max_lon,max_lat` instead of the
    /// full topology bounds.
    #[arg(long)]
    bbox: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum EngineChoice {
    /// Use the dataset's CCH acceleration bundle when the profile has one.
    Accelerated,
    /// Ignore the acceleration bundle and search the exact graph.
    Exact,
}

impl EngineChoice {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accelerated => "accelerated",
            Self::Exact => "exact",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Workload {
    /// Point-to-point routes returning summaries only.
    Route,
    /// Point-to-point routes returning full geometry.
    RouteGeometry,
    /// One NxN travel-time matrix over the first N corpus origins.
    Matrix,
    /// One service area per corpus row, seeded from its origin.
    ServiceArea,
}

impl Workload {
    fn as_str(self) -> &'static str {
        match self {
            Self::Route => "route",
            Self::RouteGeometry => "route-geometry",
            Self::Matrix => "matrix",
            Self::ServiceArea => "service-area",
        }
    }
}

#[derive(Args, Debug)]
pub(crate) struct BenchRunArgs {
    #[arg(long)]
    dataset: String,
    #[arg(long)]
    profile: PathBuf,
    #[arg(long)]
    corpus: PathBuf,
    #[arg(long, value_enum, default_value_t = EngineChoice::Accelerated)]
    engine: EngineChoice,
    #[arg(long, default_value_t = 1)]
    concurrency: usize,
    /// Untimed requests executed before the measured phase.
    #[arg(long, default_value_t = 0)]
    warmup: usize,
    /// Number of passes over the corpus in the measured phase.
    #[arg(long, default_value_t = 1)]
    iterations: usize,
    #[arg(long, value_enum, default_value_t = Workload::Route)]
    workload: Workload,
    /// Only use the first N corpus rows.
    #[arg(long)]
    limit: Option<usize>,
    /// Side length of the `matrix` workload.
    #[arg(long, default_value_t = 100)]
    matrix_size: usize,
    /// Travel-time cutoff of the `service-area` workload, in seconds.
    #[arg(long, default_value_t = 900.0)]
    threshold_s: f64,
    /// Write the machine-readable report here.
    #[arg(long)]
    json: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum HttpBackend {
    /// The NetWeevil HTTP API (`POST /v1/route`).
    Netweevil,
    /// An OSRM `route` service (`GET /route/v1/{profile}/...`).
    Osrm,
    /// A Valhalla route service (`POST /route`).
    Valhalla,
}

impl HttpBackend {
    fn as_str(self) -> &'static str {
        match self {
            Self::Netweevil => "netweevil",
            Self::Osrm => "osrm",
            Self::Valhalla => "valhalla",
        }
    }
}

#[derive(Args, Debug)]
pub(crate) struct BenchHttpArgs {
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    url: String,
    #[arg(long)]
    corpus: PathBuf,
    #[arg(long, value_enum, default_value_t = HttpBackend::Netweevil)]
    backend: HttpBackend,
    #[arg(long, default_value_t = 1)]
    concurrency: usize,
    #[arg(long, default_value_t = 0)]
    warmup: usize,
    #[arg(long, default_value_t = 1)]
    iterations: usize,
    #[arg(long)]
    limit: Option<usize>,
    /// `profile_id` sent to the NetWeevil API; omitted means the server default.
    #[arg(long)]
    profile_id: Option<String>,
    /// OSRM profile path segment.
    #[arg(long, default_value = "driving")]
    osrm_profile: String,
    /// Valhalla costing model, such as auto, bicycle, or pedestrian.
    #[arg(long, default_value = "auto")]
    valhalla_costing: String,
    /// Minimum interval between HTTP request starts; requires --concurrency 1.
    #[arg(long, default_value_t = 0)]
    request_interval_ms: u64,
    /// Per-request timeout in seconds.
    #[arg(long, default_value_t = 300)]
    timeout_s: u64,
    #[arg(long)]
    json: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(crate) struct BenchCompareArgs {
    /// Reference report.
    #[arg(long)]
    baseline: PathBuf,
    /// Report being judged against the baseline.
    #[arg(long)]
    candidate: PathBuf,
}

// ---------------------------------------------------------------------------
// Corpus
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bucket {
    Short,
    Medium,
    Long,
}

const BUCKETS: [Bucket; 3] = [Bucket::Short, Bucket::Medium, Bucket::Long];

impl Bucket {
    fn classify(straight_line_m: f64) -> Self {
        if straight_line_m < SHORT_MAX_M {
            Self::Short
        } else if straight_line_m <= MEDIUM_MAX_M {
            Self::Medium
        } else {
            Self::Long
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Short => "short",
            Self::Medium => "medium",
            Self::Long => "long",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "short" => Some(Self::Short),
            "medium" => Some(Self::Medium),
            "long" => Some(Self::Long),
            _ => None,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Short => 0,
            Self::Medium => 1,
            Self::Long => 2,
        }
    }

    /// Straight-line sampling range for this bucket. `max_long_m` bounds the
    /// open-ended `long` bucket so sampled destinations can still land inside
    /// the study area.
    fn sampling_range_m(self, max_long_m: f64) -> (f64, f64) {
        match self {
            Self::Short => (150.0, SHORT_MAX_M),
            Self::Medium => (SHORT_MAX_M, MEDIUM_MAX_M),
            Self::Long => (MEDIUM_MAX_M, max_long_m.max(MEDIUM_MAX_M * 1.2)),
        }
    }
}

#[derive(Debug, Clone)]
struct CorpusRow {
    id: String,
    source_lon: f64,
    source_lat: f64,
    target_lon: f64,
    target_lat: f64,
    bucket: Bucket,
}

/// SplitMix64. Small, seedable, and dependency-free, so a `--seed` reproduces
/// the same corpus on any machine.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    fn next_unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1_u64 << 53) as f64)
    }

    fn next_range(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.next_unit()
    }
}

fn haversine_m(from: (f64, f64), to: (f64, f64)) -> f64 {
    let (lon_a, lat_a) = from;
    let (lon_b, lat_b) = to;
    let phi_a = lat_a.to_radians();
    let phi_b = lat_b.to_radians();
    let delta_phi = (lat_b - lat_a).to_radians();
    let delta_lambda = (lon_b - lon_a).to_radians();
    let h = (delta_phi / 2.0).sin().powi(2)
        + phi_a.cos() * phi_b.cos() * (delta_lambda / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().clamp(0.0, 1.0).asin()
}

/// Spherical forward geodesic: the point `distance_m` away from `origin` along
/// `bearing_rad` (clockwise from north).
fn offset_point(origin: (f64, f64), bearing_rad: f64, distance_m: f64) -> (f64, f64) {
    let (lon, lat) = origin;
    let angular = distance_m / EARTH_RADIUS_M;
    let phi = lat.to_radians();
    let lambda = lon.to_radians();
    let sin_phi_2 = phi.sin() * angular.cos() + phi.cos() * angular.sin() * bearing_rad.cos();
    let phi_2 = sin_phi_2.clamp(-1.0, 1.0).asin();
    let lambda_2 = lambda
        + (bearing_rad.sin() * angular.sin() * phi.cos())
            .atan2(angular.cos() - phi.sin() * sin_phi_2);
    let lon_2 = lambda_2.to_degrees();
    let lon_2 = ((lon_2 + 540.0) % 360.0) - 180.0;
    (lon_2, phi_2.to_degrees())
}

fn bounds_contain(bounds: &TopologyBounds, point: (f64, f64)) -> bool {
    point.0 >= bounds.min_lon
        && point.0 <= bounds.max_lon
        && point.1 >= bounds.min_lat
        && point.1 <= bounds.max_lat
}

fn parse_bbox(value: &str) -> Result<TopologyBounds> {
    let parts: Vec<f64> = value
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<f64>()
                .with_context(|| format!("parsing bbox component '{part}'"))
        })
        .collect::<Result<_>>()?;
    if parts.len() != 4 {
        bail!("--bbox expects min_lon,min_lat,max_lon,max_lat");
    }
    let bounds = TopologyBounds {
        min_lon: parts[0],
        min_lat: parts[1],
        max_lon: parts[2],
        max_lat: parts[3],
    };
    if bounds.min_lon >= bounds.max_lon || bounds.min_lat >= bounds.max_lat {
        bail!("--bbox min values must be smaller than max values");
    }
    Ok(bounds)
}

fn topology_bounds(topology: &TopologyBundle) -> Result<TopologyBounds> {
    if let Some(index) = topology.spatial_index.as_ref() {
        return Ok(index.bounds);
    }
    let mut bounds = TopologyBounds {
        min_lon: f64::INFINITY,
        min_lat: f64::INFINITY,
        max_lon: f64::NEG_INFINITY,
        max_lat: f64::NEG_INFINITY,
    };
    for node in &topology.nodes {
        bounds.min_lon = bounds.min_lon.min(node.lon);
        bounds.min_lat = bounds.min_lat.min(node.lat);
        bounds.max_lon = bounds.max_lon.max(node.lon);
        bounds.max_lat = bounds.max_lat.max(node.lat);
    }
    if !bounds.min_lon.is_finite() || bounds.min_lon >= bounds.max_lon {
        bail!("topology has no usable coordinate bounds");
    }
    Ok(bounds)
}

/// Component ids reachable from `point`, or `None` when nothing snaps.
fn snap_components(
    engine: &PreparedRoutingEngine,
    point: (f64, f64),
    is_origin: bool,
) -> Option<Vec<u32>> {
    let labeled = LabeledPoint {
        id: "probe".to_string(),
        lon: point.0,
        lat: point.1,
        z: None,
    };
    let candidates = engine
        .snap_route_candidates(&labeled, DEFAULT_SNAP_DISTANCE_M, is_origin)
        .ok()?;
    if candidates.is_empty() {
        return None;
    }
    Some(
        candidates
            .iter()
            .filter_map(|candidate| candidate.component_id)
            .collect(),
    )
}

/// A pair is usable when both ends snap, share a weak component, and a route
/// actually exists between them. The two cheap checks reject most candidates
/// before the expensive one, and requiring a real route keeps unreachable
/// searches — which explore the whole graph — out of the latency tail.
fn pair_is_routable(
    engine: &PreparedRoutingEngine,
    candidate: &CorpusRow,
    source: (f64, f64),
    target: (f64, f64),
) -> bool {
    let Some(source_components) = snap_components(engine, source, true) else {
        return false;
    };
    let Some(target_components) = snap_components(engine, target, false) else {
        return false;
    };
    if !source_components.is_empty()
        && !target_components.is_empty()
        && !source_components
            .iter()
            .any(|component| target_components.contains(component))
    {
        return false;
    }
    matches!(
        engine.execute_route(&route_request(candidate, ReturnGeometry::None)),
        Ok(result) if result.outcome != AnalysisOutcome::Unreachable
    )
}

/// Sample `count` routable pairs, filling the three distance buckets as evenly
/// as the study area allows. Buckets that cannot be filled within their attempt
/// budget are abandoned and their quota goes to the remaining ones.
fn sample_corpus(
    engine: &PreparedRoutingEngine,
    bounds: &TopologyBounds,
    count: usize,
    seed: u64,
) -> Vec<CorpusRow> {
    let diagonal_m = haversine_m(
        (bounds.min_lon, bounds.min_lat),
        (bounds.max_lon, bounds.max_lat),
    );
    let max_long_m = diagonal_m * 0.6;
    let attempt_budget = 400 * count.div_ceil(BUCKETS.len()).max(1);

    let mut rng = SplitMix64::new(seed);
    let mut rows: Vec<CorpusRow> = Vec::with_capacity(count);
    let mut accepted = [0_usize; BUCKETS.len()];
    let mut attempts = [0_usize; BUCKETS.len()];
    let mut active = [true; BUCKETS.len()];

    while rows.len() < count {
        let Some(bucket) = BUCKETS
            .iter()
            .copied()
            .filter(|bucket| active[bucket.index()])
            .min_by_key(|bucket| accepted[bucket.index()])
        else {
            break;
        };
        let slot = bucket.index();
        attempts[slot] += 1;
        if attempts[slot] > attempt_budget {
            active[slot] = false;
            eprintln!(
                "[bench corpus] giving up on the '{}' bucket after {attempt_budget} attempts",
                bucket.as_str()
            );
            continue;
        }

        let source = (
            rng.next_range(bounds.min_lon, bounds.max_lon),
            rng.next_range(bounds.min_lat, bounds.max_lat),
        );
        let (low, high) = bucket.sampling_range_m(max_long_m);
        let bearing = rng.next_range(0.0, std::f64::consts::TAU);
        let target = offset_point(source, bearing, rng.next_range(low, high));
        if !bounds_contain(bounds, target) {
            continue;
        }
        if Bucket::classify(haversine_m(source, target)) != bucket {
            continue;
        }
        let candidate = CorpusRow {
            id: format!("{}_{:04}", bucket.as_str(), accepted[slot] + 1),
            source_lon: source.0,
            source_lat: source.1,
            target_lon: target.0,
            target_lat: target.1,
            bucket,
        };
        if !pair_is_routable(engine, &candidate, source, target) {
            continue;
        }

        accepted[slot] += 1;
        rows.push(candidate);
        if rows.len().is_multiple_of(50) || rows.len() == count {
            eprintln!("[bench corpus] accepted {}/{count} pairs", rows.len());
        }
    }

    rows
}

fn write_corpus(path: &Path, rows: &[CorpusRow]) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut writer = csv::Writer::from_path(path)
        .with_context(|| format!("writing corpus {}", path.display()))?;
    writer.write_record([
        "id",
        "source_lon",
        "source_lat",
        "target_lon",
        "target_lat",
        "bucket",
    ])?;
    for row in rows {
        writer.write_record([
            row.id.as_str(),
            &format!("{:.6}", row.source_lon),
            &format!("{:.6}", row.source_lat),
            &format!("{:.6}", row.target_lon),
            &format!("{:.6}", row.target_lat),
            row.bucket.as_str(),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

/// Read longitude/latitude corpus coordinates. A missing `bucket` column is
/// derived from the straight-line distance.
fn read_corpus(path: &Path) -> Result<Vec<CorpusRow>> {
    let mut reader = csv::Reader::from_path(path)
        .with_context(|| format!("reading corpus {}", path.display()))?;
    let headers = reader
        .headers()
        .context("reading corpus header row")?
        .clone();
    let column = |name: &str| -> Result<usize> {
        headers
            .iter()
            .position(|header| header.trim() == name)
            .with_context(|| format!("corpus is missing a '{name}' column"))
    };
    let id_index = column("id")?;
    let source_lon_index = column("source_lon")?;
    let source_lat_index = column("source_lat")?;
    let target_lon_index = column("target_lon")?;
    let target_lat_index = column("target_lat")?;
    let bucket_index = headers.iter().position(|header| header.trim() == "bucket");

    let mut rows = Vec::new();
    let mut ids = BTreeSet::new();
    for (line, record) in reader.records().enumerate() {
        let record = record.with_context(|| format!("reading corpus row {}", line + 2))?;
        let field = |index: usize| -> Result<f64> {
            record
                .get(index)
                .unwrap_or_default()
                .trim()
                .parse::<f64>()
                .with_context(|| format!("parsing corpus row {}", line + 2))
        };
        let source_lon = field(source_lon_index)?;
        let source_lat = field(source_lat_index)?;
        let target_lon = field(target_lon_index)?;
        let target_lat = field(target_lat_index)?;
        let id = record.get(id_index).unwrap_or_default().trim().to_string();
        if id.is_empty() || !ids.insert(id.clone()) {
            bail!(
                "corpus row {} has an empty or duplicate id '{id}'",
                line + 2
            );
        }
        if !(-180.0..=180.0).contains(&source_lon)
            || !(-180.0..=180.0).contains(&target_lon)
            || !(-90.0..=90.0).contains(&source_lat)
            || !(-90.0..=90.0).contains(&target_lat)
        {
            bail!("corpus row {} has invalid WGS84 coordinates", line + 2);
        }
        let bucket = match bucket_index.and_then(|index| record.get(index)) {
            Some(value) => Bucket::parse(value.trim())
                .with_context(|| format!("invalid bucket in corpus row {}", line + 2))?,
            None => Bucket::classify(haversine_m(
                (source_lon, source_lat),
                (target_lon, target_lat),
            )),
        };
        rows.push(CorpusRow {
            id,
            source_lon,
            source_lat,
            target_lon,
            target_lat,
            bucket,
        });
    }
    if rows.is_empty() {
        bail!("corpus {} has no rows", path.display());
    }
    Ok(rows)
}

/// Hash the actual selected coordinates and ids, independent of file location.
fn corpus_fingerprint(rows: &[CorpusRow]) -> String {
    let mut hash = Sha256::new();
    for row in rows {
        hash.update((row.id.len() as u64).to_le_bytes());
        hash.update(row.id.as_bytes());
        for value in [
            row.source_lon,
            row.source_lat,
            row.target_lon,
            row.target_lat,
        ] {
            hash.update(value.to_le_bytes());
        }
        hash.update([row.bucket.index() as u8]);
    }
    format!("{:x}", hash.finalize())
}

// ---------------------------------------------------------------------------
// Report model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BenchReport {
    schema_version: u32,
    mode: String,
    generated_at: String,
    netweevil_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    git_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dataset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile_id: Option<String>,
    engine: String,
    settings: BenchSettings,
    summary: BenchSummary,
    buckets: Vec<BucketStats>,
    requests: Vec<RequestRecord>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BenchSettings {
    corpus_path: String,
    corpus_sha256: String,
    corpus_rows: usize,
    workload: String,
    concurrency: usize,
    warmup: usize,
    iterations: usize,
    snap_distance_m: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    matrix_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    threshold_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    osrm_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    valhalla_costing: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_interval_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BenchSummary {
    request_count: usize,
    ok_count: usize,
    routing_failure_count: usize,
    status_failure_count: usize,
    wall_time_s: f64,
    throughput_rps: f64,
    latency: LatencyStats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    engine_load_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    peak_rss_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    matrix_cells: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LatencyStats {
    count: usize,
    mean_ms: f64,
    p50_ms: f64,
    p90_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
}

impl LatencyStats {
    fn from_samples(mut values: Vec<f64>) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        values.sort_by(f64::total_cmp);
        let sum: f64 = values.iter().sum();
        Self {
            count: values.len(),
            mean_ms: sum / values.len() as f64,
            p50_ms: percentile(&values, 0.50),
            p90_ms: percentile(&values, 0.90),
            p95_ms: percentile(&values, 0.95),
            p99_ms: percentile(&values, 0.99),
            max_ms: *values.last().unwrap_or(&0.0),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BucketStats {
    bucket: String,
    count: usize,
    ok_count: usize,
    latency: LatencyStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MatrixCellCounts {
    succeeded: usize,
    failed: usize,
    ignored: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RequestRecord {
    index: usize,
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bucket: Option<String>,
    ok: bool,
    latency_ms: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    distance_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generalized_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    matrix_cell_counts: Option<MatrixCellCounts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Linear-interpolated percentile over an ascending slice.
fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = quantile.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let low = rank.floor() as usize;
    let high = rank.ceil() as usize;
    if low == high {
        return sorted[low];
    }
    let weight = rank - low as f64;
    sorted[low] * (1.0 - weight) + sorted[high] * weight
}

/// Peak resident set size of this process, from `VmHWM` in `/proc/self/status`.
fn peak_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kilobytes: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kilobytes * 1024)
}

/// Outcome of one benchmarked request.
struct Outcome {
    ok: bool,
    duration_s: Option<f64>,
    distance_m: Option<f64>,
    generalized_cost: Option<f64>,
    matrix_cell_counts: Option<MatrixCellCounts>,
    error: Option<String>,
    /// Transport or non-2xx status failure, as opposed to a server that
    /// answered but could not route.
    status_failure: bool,
}

impl Outcome {
    fn ok(duration_s: f64, distance_m: f64) -> Self {
        Self {
            ok: true,
            duration_s: Some(duration_s),
            distance_m: Some(distance_m),
            generalized_cost: None,
            matrix_cell_counts: None,
            error: None,
            status_failure: false,
        }
    }

    fn with_generalized_cost(mut self, cost: Option<f64>) -> Self {
        self.generalized_cost = cost;
        self
    }

    fn ok_without_quality() -> Self {
        Self {
            ok: true,
            duration_s: None,
            distance_m: None,
            generalized_cost: None,
            matrix_cell_counts: None,
            error: None,
            status_failure: false,
        }
    }

    fn routing_failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            duration_s: None,
            distance_m: None,
            generalized_cost: None,
            matrix_cell_counts: None,
            error: Some(error.into()),
            status_failure: false,
        }
    }

    fn status_failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            duration_s: None,
            distance_m: None,
            generalized_cost: None,
            matrix_cell_counts: None,
            error: Some(error.into()),
            status_failure: true,
        }
    }
}

/// One measured sample, carrying enough context to rebuild per-bucket stats.
struct Sample {
    index: usize,
    id: String,
    bucket: Option<Bucket>,
    latency_ms: f64,
    outcome: Outcome,
}

/// Drive `total` executions of `tasks` across `concurrency` scoped threads.
/// Returns the samples and the wall-clock duration of the measured phase.
fn drive<T, F>(
    tasks: &[T],
    iterations: usize,
    concurrency: usize,
    request_interval: Duration,
    execute: F,
) -> (Vec<Sample>, Duration)
where
    T: Sync,
    F: Fn(&T) -> (String, Option<Bucket>, Outcome) + Sync,
{
    let total = tasks.len() * iterations;
    let cursor = AtomicUsize::new(0);
    let progress_step = (total / 20).max(1);
    let started = Instant::now();
    let collected = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..concurrency.max(1))
            .map(|_| {
                let cursor = &cursor;
                let execute = &execute;
                scope.spawn(move || {
                    let mut local = Vec::new();
                    let mut next_start = Instant::now();
                    loop {
                        let index = cursor.fetch_add(1, Ordering::Relaxed);
                        if index >= total {
                            break;
                        }
                        let task = &tasks[index % tasks.len()];
                        std::thread::sleep(next_start.saturating_duration_since(Instant::now()));
                        let start = Instant::now();
                        next_start = start + request_interval;
                        let (id, bucket, outcome) = execute(task);
                        local.push(Sample {
                            index,
                            id,
                            bucket,
                            latency_ms: start.elapsed().as_secs_f64() * 1_000.0,
                            outcome,
                        });
                        if (index + 1).is_multiple_of(progress_step) {
                            eprintln!("[bench] {}/{total} requests", index + 1);
                        }
                    }
                    local
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .flatten()
            .collect::<Vec<_>>()
    });
    (collected, started.elapsed())
}

fn summarize(
    samples: &mut [Sample],
    wall: Duration,
) -> (BenchSummary, Vec<BucketStats>, Vec<RequestRecord>) {
    samples.sort_by_key(|sample| sample.index);
    let wall_time_s = wall.as_secs_f64();
    let ok_count = samples.iter().filter(|sample| sample.outcome.ok).count();
    let status_failure_count = samples
        .iter()
        .filter(|sample| sample.outcome.status_failure)
        .count();
    let summary = BenchSummary {
        request_count: samples.len(),
        ok_count,
        routing_failure_count: samples.len() - ok_count - status_failure_count,
        status_failure_count,
        wall_time_s,
        throughput_rps: if wall_time_s > 0.0 {
            samples.len() as f64 / wall_time_s
        } else {
            0.0
        },
        latency: LatencyStats::from_samples(
            samples.iter().map(|sample| sample.latency_ms).collect(),
        ),
        engine_load_s: None,
        peak_rss_bytes: peak_rss_bytes(),
        matrix_cells: None,
    };

    let buckets = BUCKETS
        .iter()
        .filter_map(|bucket| {
            let matching: Vec<&Sample> = samples
                .iter()
                .filter(|sample| sample.bucket == Some(*bucket))
                .collect();
            if matching.is_empty() {
                return None;
            }
            Some(BucketStats {
                bucket: bucket.as_str().to_string(),
                count: matching.len(),
                ok_count: matching.iter().filter(|sample| sample.outcome.ok).count(),
                latency: LatencyStats::from_samples(
                    matching.iter().map(|sample| sample.latency_ms).collect(),
                ),
            })
        })
        .collect();

    let requests = samples
        .iter()
        .map(|sample| RequestRecord {
            index: sample.index,
            id: sample.id.clone(),
            bucket: sample.bucket.map(|bucket| bucket.as_str().to_string()),
            ok: sample.outcome.ok,
            latency_ms: sample.latency_ms,
            duration_s: sample.outcome.duration_s,
            distance_m: sample.outcome.distance_m,
            generalized_cost: sample.outcome.generalized_cost,
            matrix_cell_counts: sample.outcome.matrix_cell_counts.clone(),
            error: sample.outcome.error.clone(),
        })
        .collect();

    (summary, buckets, requests)
}

fn write_report(path: &Path, report: &BenchReport) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(report).context("serializing benchmark report")?;
    std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Engine loading
// ---------------------------------------------------------------------------

struct LoadedEngine {
    engine: PreparedRoutingEngine,
    profile_id: String,
    load: Duration,
    acceleration_used: bool,
}

/// Load the dataset topology and the already-compiled profile bundle. This
/// never compiles or writes anything, so a benchmark cannot mutate the
/// workspace it measures.
fn load_engine(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile_path: &Path,
    choice: EngineChoice,
) -> Result<LoadedEngine> {
    let started = Instant::now();
    let profile = load_profile(profile_path)
        .with_context(|| format!("loading {}", profile_path.display()))?;
    profile.validate()?;
    let profile_hash = profile.fingerprint()?;

    let dataset_manifest = read_dataset_manifest(paths, dataset_id)
        .with_context(|| format!("reading dataset manifest for '{dataset_id}'"))?;

    // Resolve the compiled bundle before reading any large binary, so a stale
    // or uncompiled profile fails immediately instead of after a long load.
    let compiled_manifest = read_compiled_profile_manifests(paths)?
        .into_iter()
        .find(|manifest| {
            manifest.dataset_id.0 == dataset_id && manifest.profile_hash == profile_hash
        })
        .with_context(|| {
            format!(
                "no compiled bundle for profile '{}' (hash {profile_hash}) on dataset \
                 '{dataset_id}'; run `netweevil profile compile` first",
                profile.profile.id
            )
        })?;

    let topology_ref = dataset_manifest
        .topology_bundle
        .clone()
        .context("dataset is missing a topology bundle; run `netweevil dataset import` first")?;
    let topology: TopologyBundle = read_topology_bundle(&topology_ref.path)
        .with_context(|| format!("reading topology bundle {}", topology_ref.path))?;

    let acceleration: Option<DatasetAccelerationBundle> = match choice {
        EngineChoice::Accelerated => dataset_manifest
            .acceleration_bundle
            .as_ref()
            .map(|bundle_ref| {
                read_acceleration_bundle(&bundle_ref.path)
                    .with_context(|| format!("reading acceleration bundle {}", bundle_ref.path))
            })
            .transpose()?,
        EngineChoice::Exact => None,
    };
    let acceleration_used = acceleration.is_some();

    let compiled_bundle: CompiledProfileBundle =
        read_compiled_profile_bundle(&compiled_manifest.bundle.path).with_context(|| {
            format!(
                "reading compiled profile bundle {}",
                compiled_manifest.bundle.path
            )
        })?;

    let engine = PreparedRoutingEngine::new(
        Arc::new(topology),
        Arc::new(compiled_bundle),
        acceleration.map(Arc::new),
    )
    .context("preparing routing engine")?;

    Ok(LoadedEngine {
        engine,
        profile_id: profile.profile.id.clone(),
        load: started.elapsed(),
        acceleration_used,
    })
}

// ---------------------------------------------------------------------------
// Request builders
// ---------------------------------------------------------------------------

fn snap_options() -> SnapOptions {
    SnapOptions {
        max_distance_m: DEFAULT_SNAP_DISTANCE_M,
        ..SnapOptions::default()
    }
}

fn route_request(row: &CorpusRow, geometry: ReturnGeometry) -> RouteRequest {
    RouteRequest {
        route_id: row.id.clone(),
        origin: LabeledPoint {
            id: format!("{}_o", row.id),
            lon: row.source_lon,
            lat: row.source_lat,
            z: None,
        },
        destination: LabeledPoint {
            id: format!("{}_d", row.id),
            lon: row.target_lon,
            lat: row.target_lat,
            z: None,
        },
        snap: snap_options(),
        connectivity: ConnectivityPolicy::default(),
        fallback: FallbackPolicy::default(),
        returns: ReturnConfig {
            geometry,
            ..ReturnConfig::default()
        },
        alternatives: AlternativeRouteOptions::default(),
        temporal: TemporalRequestOptions::default(),
    }
}

fn point_set(points: Vec<LabeledPoint>) -> PointSetDocument {
    PointSetDocument {
        points,
        snap: snap_options(),
        connectivity: ConnectivityPolicy::default(),
        fallback: FallbackPolicy::default(),
        returns: ReturnConfig::default(),
        alternatives: AlternativeRouteOptions::default(),
        temporal: TemporalRequestOptions::default(),
    }
}

fn service_area_request(row: &CorpusRow, threshold_s: f64) -> ServiceAreaRequest {
    ServiceAreaRequest {
        analysis_id: row.id.clone(),
        origins: vec![LabeledPoint {
            id: format!("{}_o", row.id),
            lon: row.source_lon,
            lat: row.source_lat,
            z: None,
        }],
        thresholds: vec![ServiceAreaThreshold {
            id: None,
            limit: threshold_s,
            metric: ServiceAreaThresholdMetric::TravelTimeS,
        }],
        snap: snap_options(),
        connectivity: ConnectivityPolicy::default(),
        fallback: FallbackPolicy::default(),
        output_mode: ServiceAreaOutputMode::Network,
        band_mode: ServiceAreaBandMode::default(),
        boundary_mode: ServiceAreaBoundaryMode::default(),
        multi_origin_mode: ServiceAreaMultiOriginMode::default(),
        polygon: ServiceAreaPolygonOptions::default(),
        returns: ServiceAreaReturnOptions {
            geometry: false,
            attributes: false,
            per_threshold_summary: true,
            diagnostics: false,
            segments: false,
            ..ServiceAreaReturnOptions::default()
        },
        temporal: TemporalRequestOptions::default(),
    }
}

/// A single unit of benchmarked work.
enum Task {
    Route {
        id: String,
        bucket: Bucket,
        request: RouteRequest,
    },
    Matrix {
        origins: PointSetDocument,
        destinations: PointSetDocument,
    },
    ServiceArea {
        id: String,
        bucket: Bucket,
        request: ServiceAreaRequest,
    },
}

fn execute_task(engine: &PreparedRoutingEngine, task: &Task) -> (String, Option<Bucket>, Outcome) {
    match task {
        Task::Route {
            id,
            bucket,
            request,
        } => {
            let outcome = match engine.execute_route(request) {
                Ok(result) if result.outcome == AnalysisOutcome::Unreachable => {
                    Outcome::routing_failure("unreachable")
                }
                Ok(result) => Outcome::ok(
                    result.summary.total_travel_time_s,
                    result.summary.total_distance_m as f64,
                )
                .with_generalized_cost(Some(result.summary.total_generalized_cost)),
                Err(error) => Outcome::routing_failure(error.to_string()),
            };
            (id.clone(), Some(*bucket), outcome)
        }
        Task::Matrix {
            origins,
            destinations,
        } => {
            let outcome = match engine.execute_matrix(origins, destinations) {
                Ok(result) => {
                    let mut outcome = if result.succeeded_count == 0 {
                        Outcome::routing_failure("no matrix cell succeeded")
                    } else {
                        Outcome::ok_without_quality()
                    };
                    outcome.matrix_cell_counts = Some(MatrixCellCounts {
                        succeeded: result.succeeded_count,
                        failed: result.failed_count,
                        ignored: result.ignored_count,
                    });
                    outcome
                }
                Err(error) => Outcome::routing_failure(error.to_string()),
            };
            ("matrix".to_string(), None, outcome)
        }
        Task::ServiceArea {
            id,
            bucket,
            request,
        } => {
            let outcome = match engine.execute_service_area(request) {
                Ok(result) if result.processed_origin_count == 0 => {
                    Outcome::routing_failure("no origin produced a service area")
                }
                Ok(_) => Outcome::ok_without_quality(),
                Err(error) => Outcome::routing_failure(error.to_string()),
            };
            (id.clone(), Some(*bucket), outcome)
        }
    }
}

fn build_tasks(rows: &[CorpusRow], args: &BenchRunArgs) -> Result<Vec<Task>> {
    match args.workload {
        Workload::Route | Workload::RouteGeometry => {
            let geometry = if args.workload == Workload::RouteGeometry {
                ReturnGeometry::Full
            } else {
                ReturnGeometry::None
            };
            Ok(rows
                .iter()
                .map(|row| Task::Route {
                    id: row.id.clone(),
                    bucket: row.bucket,
                    request: route_request(row, geometry),
                })
                .collect())
        }
        Workload::Matrix => {
            let size = args.matrix_size.min(rows.len());
            if size == 0 {
                bail!("--matrix-size must be at least 1");
            }
            let points: Vec<LabeledPoint> = rows[..size]
                .iter()
                .enumerate()
                .map(|(index, row)| LabeledPoint {
                    id: format!("p{index}"),
                    lon: row.source_lon,
                    lat: row.source_lat,
                    z: None,
                })
                .collect();
            Ok(vec![Task::Matrix {
                origins: point_set(points.clone()),
                destinations: point_set(points),
            }])
        }
        Workload::ServiceArea => Ok(rows
            .iter()
            .map(|row| Task::ServiceArea {
                id: row.id.clone(),
                bucket: row.bucket,
                request: service_area_request(row, args.threshold_s),
            })
            .collect()),
    }
}

// ---------------------------------------------------------------------------
// HTTP clients
// ---------------------------------------------------------------------------

fn netweevil_route_url(base_url: &str) -> String {
    format!("{}/v1/route", base_url.trim_end_matches('/'))
}

fn osrm_route_url(base_url: &str, profile: &str, row: &CorpusRow) -> String {
    format!(
        "{}/route/v1/{}/{},{};{},{}?overview=false&radiuses=500;500",
        base_url.trim_end_matches('/'),
        profile,
        row.source_lon,
        row.source_lat,
        row.target_lon,
        row.target_lat
    )
}

fn netweevil_route_body(row: &CorpusRow, profile_id: Option<&str>) -> serde_json::Value {
    let mut body = serde_json::json!({
        "request": route_request(row, ReturnGeometry::None),
    });
    if let Some(profile_id) = profile_id {
        body["profile_id"] = serde_json::Value::String(profile_id.to_string());
    }
    body
}

fn valhalla_route_body(row: &CorpusRow, costing: &str) -> serde_json::Value {
    serde_json::json!({
        "id": row.id,
        "locations": [
            {"lon": row.source_lon, "lat": row.source_lat, "search_cutoff": DEFAULT_SNAP_DISTANCE_M},
            {"lon": row.target_lon, "lat": row.target_lat, "search_cutoff": DEFAULT_SNAP_DISTANCE_M},
        ],
        "costing": costing,
        "format": "osrm",
        "directions_type": "none",
        "shape_format": "no_shape",
    })
}

/// Read `{service, result}` from the NetWeevil API into a benchmark outcome.
fn parse_netweevil_route(body: &serde_json::Value) -> Outcome {
    let Some(result) = body.get("result") else {
        return Outcome::routing_failure("response has no 'result' object");
    };
    if result.get("outcome").and_then(serde_json::Value::as_str) == Some("unreachable") {
        return Outcome::routing_failure("unreachable");
    }
    let Some(summary) = result.get("summary") else {
        return Outcome::routing_failure("route result has no 'summary' object");
    };
    let duration_s = summary
        .get("total_travel_time_s")
        .and_then(serde_json::Value::as_f64);
    let distance_m = summary
        .get("total_distance_m")
        .and_then(serde_json::Value::as_f64);
    match (duration_s, distance_m) {
        (Some(duration_s), Some(distance_m)) => Outcome::ok(duration_s, distance_m)
            .with_generalized_cost(
                summary
                    .get("total_generalized_cost")
                    .and_then(serde_json::Value::as_f64),
            ),
        _ => Outcome::routing_failure("route summary has no travel time or distance"),
    }
}

/// Read an OSRM `route` response into a benchmark outcome.
fn parse_osrm_route(body: &serde_json::Value) -> Outcome {
    match body.get("code").and_then(serde_json::Value::as_str) {
        Some("Ok") => {}
        Some(code) => return Outcome::routing_failure(format!("osrm code '{code}'")),
        None => return Outcome::routing_failure("response has no 'code' field"),
    }
    let route = body
        .get("routes")
        .and_then(serde_json::Value::as_array)
        .and_then(|routes| routes.first());
    let Some(route) = route else {
        return Outcome::routing_failure("response has no routes");
    };
    let duration_s = route.get("duration").and_then(serde_json::Value::as_f64);
    let distance_m = route.get("distance").and_then(serde_json::Value::as_f64);
    match (duration_s, distance_m) {
        (Some(duration_s), Some(distance_m)) => Outcome::ok(duration_s, distance_m),
        _ => Outcome::routing_failure("route has no duration or distance"),
    }
}

fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(concat!(
            "NetWeevil/",
            env!("CARGO_PKG_VERSION"),
            " routing-research"
        ))
        .http_status_as_error(false)
        .build()
        .into()
}

fn execute_http(
    agent: &ureq::Agent,
    args: &BenchHttpArgs,
    row: &CorpusRow,
) -> (String, Option<Bucket>, Outcome) {
    let response = match args.backend {
        HttpBackend::Netweevil => agent
            .post(netweevil_route_url(&args.url))
            .send_json(netweevil_route_body(row, args.profile_id.as_deref())),
        HttpBackend::Osrm => agent
            .get(osrm_route_url(&args.url, &args.osrm_profile, row))
            .call(),
        HttpBackend::Valhalla => agent
            .post(format!("{}/route", args.url.trim_end_matches('/')))
            .send_json(valhalla_route_body(row, &args.valhalla_costing)),
    };
    let outcome = match response {
        Err(error) => Outcome::status_failure(error.to_string()),
        Ok(mut response) => {
            let status = response.status().as_u16();
            if !(200..300).contains(&status) {
                Outcome::status_failure(format!("HTTP {status}"))
            } else {
                match response.body_mut().read_json::<serde_json::Value>() {
                    Err(error) => Outcome::status_failure(format!("decoding body: {error}")),
                    Ok(body) => match args.backend {
                        HttpBackend::Netweevil => parse_netweevil_route(&body),
                        HttpBackend::Osrm => parse_osrm_route(&body),
                        HttpBackend::Valhalla => parse_osrm_route(&body),
                    },
                }
            }
        }
    };
    (row.id.clone(), Some(row.bucket), outcome)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn format_bytes(bytes: u64) -> String {
    let mib = bytes as f64 / (1024.0 * 1024.0);
    if mib >= 1024.0 {
        format!("{:.2} GiB", mib / 1024.0)
    } else {
        format!("{mib:.1} MiB")
    }
}

fn render_report(report: &BenchReport) {
    println!("netweevil bench {}", report.mode);
    println!(
        "  commit         {}",
        report.git_commit.as_deref().unwrap_or("unknown")
    );
    if let Some(dataset_id) = report.dataset_id.as_deref() {
        println!("  dataset        {dataset_id}");
    }
    if let Some(profile_id) = report.profile_id.as_deref() {
        println!("  profile        {profile_id}");
    }
    println!("  engine         {}", report.engine);
    println!("  workload       {}", report.settings.workload);
    println!(
        "  corpus         {} ({} rows)",
        report.settings.corpus_path, report.settings.corpus_rows
    );
    if let Some(url) = report.settings.url.as_deref() {
        println!("  url            {url}");
    }
    println!(
        "  concurrency    {}   warmup {}   iterations {}",
        report.settings.concurrency, report.settings.warmup, report.settings.iterations
    );
    if let Some(matrix_size) = report.settings.matrix_size {
        println!("  matrix size    {matrix_size}x{matrix_size}");
    }
    if let Some(threshold_s) = report.settings.threshold_s {
        println!("  threshold      {threshold_s} s");
    }

    let summary = &report.summary;
    println!();
    if let Some(engine_load_s) = summary.engine_load_s {
        println!("  engine load    {engine_load_s:.3} s");
    }
    if let Some(peak_rss) = summary.peak_rss_bytes {
        println!("  peak RSS       {}", format_bytes(peak_rss));
    }
    println!("  requests       {}", summary.request_count);
    println!("  ok             {}", summary.ok_count);
    println!("  routing fails  {}", summary.routing_failure_count);
    println!("  status fails   {}", summary.status_failure_count);
    if let Some(cells) = summary.matrix_cells {
        println!("  matrix cells   {cells}");
    }
    println!("  wall time      {:.3} s", summary.wall_time_s);
    println!("  throughput     {:.2} req/s", summary.throughput_rps);
    println!(
        "  latency ms     p50 {:.2}   p90 {:.2}   p95 {:.2}   p99 {:.2}   max {:.2}   mean {:.2}",
        summary.latency.p50_ms,
        summary.latency.p90_ms,
        summary.latency.p95_ms,
        summary.latency.p99_ms,
        summary.latency.max_ms,
        summary.latency.mean_ms
    );

    if !report.buckets.is_empty() {
        println!();
        println!(
            "  {:<8} {:>7} {:>7} {:>10} {:>10} {:>10}",
            "bucket", "n", "ok", "p50 ms", "p95 ms", "max ms"
        );
        for bucket in &report.buckets {
            println!(
                "  {:<8} {:>7} {:>7} {:>10.2} {:>10.2} {:>10.2}",
                bucket.bucket,
                bucket.count,
                bucket.ok_count,
                bucket.latency.p50_ms,
                bucket.latency.p95_ms,
                bucket.latency.max_ms
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Subcommand entry points
// ---------------------------------------------------------------------------

pub(crate) fn bench_corpus(paths: &WorkspacePaths, args: BenchCorpusArgs) -> Result<()> {
    if args.count == 0 {
        bail!("--count must be at least 1");
    }
    // Sampling routes every candidate pair, so it uses the accelerated engine.
    let loaded = load_engine(
        paths,
        &args.dataset,
        &args.profile,
        EngineChoice::Accelerated,
    )?;
    let bounds = match args.bbox.as_deref() {
        Some(bbox) => parse_bbox(bbox)?,
        None => topology_bounds(loaded.engine.topology())?,
    };
    eprintln!(
        "[bench corpus] sampling in {:.4},{:.4},{:.4},{:.4}",
        bounds.min_lon, bounds.min_lat, bounds.max_lon, bounds.max_lat
    );

    let rows = sample_corpus(&loaded.engine, &bounds, args.count, args.seed);
    if rows.is_empty() {
        bail!("no routable pairs were found in the requested bounds");
    }
    write_corpus(&args.out, &rows)?;

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for row in &rows {
        *counts.entry(row.bucket.as_str()).or_default() += 1;
    }
    println!(
        "wrote {} pairs to {} (seed {})",
        rows.len(),
        args.out.display(),
        args.seed
    );
    for (bucket, count) in counts {
        println!("  {bucket:<8} {count}");
    }
    Ok(())
}

pub(crate) fn bench_run(paths: &WorkspacePaths, args: BenchRunArgs) -> Result<()> {
    if args.iterations == 0 {
        bail!("--iterations must be at least 1");
    }
    let mut rows = read_corpus(&args.corpus)?;
    let corpus_rows = rows.len();
    if let Some(limit) = args.limit {
        rows.truncate(limit);
    }

    if rows.is_empty() {
        bail!("benchmark corpus is empty after applying --limit");
    }
    let loaded = load_engine(paths, &args.dataset, &args.profile, args.engine)?;
    let effective = loaded
        .engine
        .effective_engine_description(netweevil_query::EngineMode::Auto);
    let tasks = build_tasks(&rows, &args)?;

    for index in 0..args.warmup {
        let task = &tasks[index % tasks.len()];
        let _ = execute_task(&loaded.engine, task);
    }

    let (mut samples, wall) = drive(
        &tasks,
        args.iterations,
        args.concurrency,
        Duration::ZERO,
        |task| execute_task(&loaded.engine, task),
    );
    let (mut summary, buckets, requests) = summarize(&mut samples, wall);
    summary.engine_load_s = Some(loaded.load.as_secs_f64());
    if args.workload == Workload::Matrix {
        let size = args.matrix_size.min(rows.len());
        summary.matrix_cells = Some(size * size);
    }

    let report = BenchReport {
        schema_version: REPORT_SCHEMA_VERSION,
        mode: "run".to_string(),
        generated_at: now_rfc3339()?,
        netweevil_version: env!("CARGO_PKG_VERSION").to_string(),
        git_commit: option_env!("NETWEEVIL_GIT_COMMIT").map(ToString::to_string),
        dataset_id: Some(args.dataset.clone()),
        profile_id: Some(loaded.profile_id.clone()),
        engine: format!(
            "{} ({}{})",
            args.engine.as_str(),
            effective.route_engine,
            if loaded.acceleration_used {
                ""
            } else {
                ", no acceleration bundle"
            }
        ),
        settings: BenchSettings {
            corpus_path: args.corpus.display().to_string(),
            corpus_sha256: corpus_fingerprint(&rows),
            corpus_rows,
            workload: args.workload.as_str().to_string(),
            concurrency: args.concurrency.max(1),
            warmup: args.warmup,
            iterations: args.iterations,
            snap_distance_m: DEFAULT_SNAP_DISTANCE_M,
            limit: args.limit,
            matrix_size: (args.workload == Workload::Matrix)
                .then_some(args.matrix_size.min(rows.len())),
            threshold_s: (args.workload == Workload::ServiceArea).then_some(args.threshold_s),
            ..BenchSettings::default()
        },
        summary,
        buckets,
        requests,
    };

    render_report(&report);
    if let Some(path) = args.json.as_deref() {
        write_report(path, &report)?;
        println!();
        println!("report written to {}", path.display());
    }
    Ok(())
}

pub(crate) fn bench_http(args: BenchHttpArgs) -> Result<()> {
    if args.request_interval_ms > 0 && args.concurrency != 1 {
        bail!("--request-interval-ms requires --concurrency 1");
    }
    if args.iterations == 0 {
        bail!("--iterations must be at least 1");
    }
    if !args.url.starts_with("http://") {
        bail!("--url must be a plain http:// endpoint; this client is built without TLS");
    }
    let mut rows = read_corpus(&args.corpus)?;
    let corpus_rows = rows.len();
    if let Some(limit) = args.limit {
        rows.truncate(limit);
    }

    if rows.is_empty() {
        bail!("benchmark corpus is empty after applying --limit");
    }
    let request_interval = Duration::from_millis(args.request_interval_ms);
    let timeout = Duration::from_secs(args.timeout_s);
    let warmup_agent = http_agent(timeout);
    for index in 0..args.warmup {
        let started = Instant::now();
        let _ = execute_http(&warmup_agent, &args, &rows[index % rows.len()]);
        std::thread::sleep(request_interval.saturating_sub(started.elapsed()));
    }
    drop(warmup_agent);

    // One agent per worker so each concurrent request holds its own keep-alive
    // connection rather than queueing on a shared pool.
    let agents: Vec<ureq::Agent> = (0..args.concurrency.max(1))
        .map(|_| http_agent(timeout))
        .collect();
    let next_agent = AtomicUsize::new(0);
    let worker_agent = || {
        let index = next_agent.fetch_add(1, Ordering::Relaxed);
        agents[index % agents.len()].clone()
    };

    let (mut samples, wall) = drive(
        &rows,
        args.iterations,
        args.concurrency,
        request_interval,
        {
            let args = &args;
            move |row| {
                let agent = worker_agent();
                execute_http(&agent, args, row)
            }
        },
    );
    let (summary, buckets, requests) = summarize(&mut samples, wall);

    let report = BenchReport {
        schema_version: REPORT_SCHEMA_VERSION,
        mode: "http".to_string(),
        generated_at: now_rfc3339()?,
        netweevil_version: env!("CARGO_PKG_VERSION").to_string(),
        git_commit: option_env!("NETWEEVIL_GIT_COMMIT").map(ToString::to_string),
        dataset_id: None,
        profile_id: args.profile_id.clone(),
        engine: format!("http:{}", args.backend.as_str()),
        settings: BenchSettings {
            corpus_path: args.corpus.display().to_string(),
            corpus_sha256: corpus_fingerprint(&rows),
            corpus_rows,
            workload: "route".to_string(),
            concurrency: args.concurrency.max(1),
            warmup: args.warmup,
            iterations: args.iterations,
            snap_distance_m: DEFAULT_SNAP_DISTANCE_M,
            limit: args.limit,
            url: Some(args.url.clone()),
            backend: Some(args.backend.as_str().to_string()),
            osrm_profile: (args.backend == HttpBackend::Osrm).then(|| args.osrm_profile.clone()),
            valhalla_costing: (args.backend == HttpBackend::Valhalla)
                .then(|| args.valhalla_costing.clone()),
            request_interval_ms: Some(args.request_interval_ms),
            ..BenchSettings::default()
        },
        summary,
        buckets,
        requests,
    };

    render_report(&report);
    if let Some(path) = args.json.as_deref() {
        write_report(path, &report)?;
        println!();
        println!("report written to {}", path.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Compare
// ---------------------------------------------------------------------------

/// One request id folded across however many iterations recorded it.
#[derive(Debug, Clone)]
struct MergedRequest {
    bucket: Option<String>,
    ok: bool,
    latency_ms: f64,
    duration_s: Option<f64>,
    distance_m: Option<f64>,
    generalized_cost: Option<f64>,
}

/// Fold per-request records by id: median latency across iterations, and the
/// route quality of the first successful record.
fn merge_by_id(records: &[RequestRecord]) -> BTreeMap<String, MergedRequest> {
    let mut latencies: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut merged: BTreeMap<String, MergedRequest> = BTreeMap::new();
    for record in records {
        latencies
            .entry(record.id.clone())
            .or_default()
            .push(record.latency_ms);
        let entry = merged.entry(record.id.clone()).or_insert(MergedRequest {
            bucket: record.bucket.clone(),
            ok: false,
            latency_ms: 0.0,
            duration_s: None,
            distance_m: None,
            generalized_cost: None,
        });
        if record.ok && !entry.ok {
            entry.ok = true;
            entry.duration_s = record.duration_s;
            entry.distance_m = record.distance_m;
            entry.generalized_cost = record.generalized_cost;
        }
    }
    for (id, mut values) in latencies {
        values.sort_by(f64::total_cmp);
        if let Some(entry) = merged.get_mut(&id) {
            entry.latency_ms = percentile(&values, 0.50);
        }
    }
    merged
}

fn relative_difference(baseline: f64, candidate: f64) -> Option<f64> {
    if !baseline.is_finite() || !candidate.is_finite() || baseline.abs() < f64::EPSILON {
        return None;
    }
    Some((candidate - baseline).abs() / baseline.abs())
}

fn describe_report(path: &Path, report: &BenchReport) -> String {
    format!(
        "{} [{} / {} / {}]",
        path.display(),
        report.mode,
        report.engine,
        report.settings.workload
    )
}

pub(crate) fn bench_compare(args: BenchCompareArgs) -> Result<()> {
    let baseline: BenchReport = read_report(&args.baseline)?;
    let candidate: BenchReport = read_report(&args.candidate)?;
    validate_comparison(&baseline.settings, &candidate.settings)?;

    println!("netweevil bench compare");
    println!(
        "  baseline   {}",
        describe_report(&args.baseline, &baseline)
    );
    println!(
        "  candidate  {}",
        describe_report(&args.candidate, &candidate)
    );

    let baseline_requests = merge_by_id(&baseline.requests);
    let candidate_requests = merge_by_id(&candidate.requests);

    println!();
    println!(
        "  {:<8} {:>7} {:>11} {:>11} {:>11} {:>11} {:>9}",
        "bucket", "n", "base p50", "cand p50", "base p95", "cand p95", "p50 gain"
    );
    let mut bucket_labels: Vec<String> = BUCKETS
        .iter()
        .map(|bucket| bucket.as_str().to_string())
        .collect();
    bucket_labels.push("all".to_string());
    for label in &bucket_labels {
        let mut base_values = Vec::new();
        let mut cand_values = Vec::new();
        for (id, base) in &baseline_requests {
            let Some(cand) = candidate_requests.get(id) else {
                continue;
            };
            if label != "all" && base.bucket.as_deref() != Some(label.as_str()) {
                continue;
            }
            base_values.push(base.latency_ms);
            cand_values.push(cand.latency_ms);
        }
        if base_values.is_empty() {
            continue;
        }
        base_values.sort_by(f64::total_cmp);
        cand_values.sort_by(f64::total_cmp);
        let base_p50 = percentile(&base_values, 0.50);
        let cand_p50 = percentile(&cand_values, 0.50);
        let gain = if cand_p50 > 0.0 {
            base_p50 / cand_p50
        } else {
            f64::NAN
        };
        println!(
            "  {:<8} {:>7} {:>11.2} {:>11.2} {:>11.2} {:>11.2} {:>8.2}x",
            label,
            base_values.len(),
            base_p50,
            cand_p50,
            percentile(&base_values, 0.95),
            percentile(&cand_values, 0.95),
            gain
        );
    }

    let mut matched = 0_usize;
    let mut both_ok = 0_usize;
    let mut baseline_only = 0_usize;
    let mut candidate_only = 0_usize;
    let mut duration_diffs = Vec::new();
    let mut distance_diffs = Vec::new();
    let mut cost_diffs = Vec::new();
    for (id, base) in &baseline_requests {
        let Some(cand) = candidate_requests.get(id) else {
            continue;
        };
        matched += 1;
        match (base.ok, cand.ok) {
            (true, true) => both_ok += 1,
            (true, false) => baseline_only += 1,
            (false, true) => candidate_only += 1,
            (false, false) => {}
        }
        if !(base.ok && cand.ok) {
            continue;
        }
        if let (Some(a), Some(b)) = (base.duration_s, cand.duration_s)
            && let Some(diff) = relative_difference(a, b)
        {
            duration_diffs.push(diff);
        }
        if let (Some(a), Some(b)) = (base.distance_m, cand.distance_m)
            && let Some(diff) = relative_difference(a, b)
        {
            distance_diffs.push(diff);
        }
        if let (Some(a), Some(b)) = (base.generalized_cost, cand.generalized_cost)
            && let Some(diff) = relative_difference(a, b)
        {
            cost_diffs.push(diff);
        }
    }

    println!();
    println!("  route agreement");
    println!("  matched ids       {matched}");
    if matched > 0 {
        println!(
            "  both succeeded    {both_ok} ({:.1}%)",
            100.0 * both_ok as f64 / matched as f64
        );
    }
    println!("  baseline only     {baseline_only}");
    println!("  candidate only    {candidate_only}");
    for (label, mut diffs) in [
        ("duration", duration_diffs),
        ("distance", distance_diffs),
        ("cost", cost_diffs),
    ] {
        if diffs.is_empty() {
            println!("  {label:<9} rel diff  no comparable values");
            continue;
        }
        diffs.sort_by(f64::total_cmp);
        println!(
            "  {label:<9} rel diff  median {:.2}%   p95 {:.2}%   n {}",
            100.0 * percentile(&diffs, 0.50),
            100.0 * percentile(&diffs, 0.95),
            diffs.len()
        );
    }
    Ok(())
}

fn read_report(path: &Path) -> Result<BenchReport> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading benchmark report {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("parsing benchmark report {}", path.display()))?;
    if value["schema_version"].as_u64() != Some(u64::from(REPORT_SCHEMA_VERSION)) {
        bail!("unsupported benchmark report schema; rerun the benchmark with the current binary");
    }
    serde_json::from_value(value)
        .with_context(|| format!("parsing benchmark report {}", path.display()))
}

fn validate_comparison(baseline: &BenchSettings, candidate: &BenchSettings) -> Result<()> {
    if baseline.corpus_sha256.is_empty() || baseline.corpus_sha256 != candidate.corpus_sha256 {
        bail!("reports must use the same selected corpus coordinates, ids, and buckets");
    }
    if baseline.workload != candidate.workload
        || baseline.matrix_size != candidate.matrix_size
        || baseline.threshold_s != candidate.threshold_s
        || baseline.snap_distance_m != candidate.snap_distance_m
    {
        bail!("reports must use the same workload, matrix size, threshold, and snap distance");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paced_requests_wait_between_measurements() {
        let interval = Duration::from_millis(10);
        let (samples, wall) = drive(&[0, 1, 2], 1, 1, interval, |id| {
            (id.to_string(), None, Outcome::ok_without_quality())
        });
        assert_eq!(samples.len(), 3);
        assert!(wall >= interval * 2);
    }

    fn row(id: &str) -> CorpusRow {
        CorpusRow {
            id: id.to_string(),
            source_lon: 4.9,
            source_lat: 52.3,
            target_lon: 5.1,
            target_lat: 52.4,
            bucket: Bucket::Medium,
        }
    }

    #[test]
    fn classifies_distance_buckets() {
        assert_eq!(Bucket::classify(900.0), Bucket::Short);
        assert_eq!(Bucket::classify(4_999.0), Bucket::Short);
        assert_eq!(Bucket::classify(5_000.0), Bucket::Medium);
        assert_eq!(Bucket::classify(30_000.0), Bucket::Medium);
        assert_eq!(Bucket::classify(30_001.0), Bucket::Long);
    }

    #[test]
    fn interpolates_percentiles() {
        let sorted = vec![1.0, 2.0, 3.0, 4.0];
        assert!((percentile(&sorted, 0.0) - 1.0).abs() < 1e-9);
        assert!((percentile(&sorted, 0.5) - 2.5).abs() < 1e-9);
        assert!((percentile(&sorted, 1.0) - 4.0).abs() < 1e-9);
        assert_eq!(percentile(&[], 0.5), 0.0);
    }

    #[test]
    fn offsets_points_by_distance_and_bearing() {
        let origin = (6.5665, 53.2194);
        let north = offset_point(origin, 0.0, 10_000.0);
        assert!(north.1 > origin.1);
        assert!((haversine_m(origin, north) - 10_000.0).abs() < 5.0);
        let east = offset_point(origin, std::f64::consts::FRAC_PI_2, 25_000.0);
        assert!(east.0 > origin.0);
        assert!((haversine_m(origin, east) - 25_000.0).abs() < 5.0);
    }

    #[test]
    fn samples_reproducibly_from_a_seed() {
        let mut first = SplitMix64::new(42);
        let mut second = SplitMix64::new(42);
        let a: Vec<f64> = (0..8).map(|_| first.next_unit()).collect();
        let b: Vec<f64> = (0..8).map(|_| second.next_unit()).collect();
        assert_eq!(a, b);
        assert!(a.iter().all(|value| (0.0..1.0).contains(value)));
    }

    #[test]
    fn reads_canonical_corpus_and_rejects_invalid_rows() {
        let dir =
            std::env::temp_dir().join(format!("netweevil-bench-corpus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let current = dir.join("current.csv");
        std::fs::write(
            &current,
            "id,source_lon,source_lat,target_lon,target_lat,bucket\nb,6.5,53.2,6.9,53.6,long\n",
        )
        .unwrap();
        let rows = read_corpus(&current).unwrap();
        assert_eq!(rows[0].bucket, Bucket::Long);

        let invalid = dir.join("invalid.csv");
        for content in [
            "id,source_lon,source_lat,target_lon,target_lat\na,NaN,53.2,6.9,53.6\n",
            "id,source_lon,source_lat,target_lon,target_lat\na,6.5,91,6.9,53.6\n",
            "id,source_lon,source_lat,target_lon,target_lat\na,6.5,53.2,6.9,53.6\na,6.5,53.2,6.9,53.6\n",
            "id,source_lon,source_lat,target_lon,target_lat,bucket\na,6.5,53.2,6.9,53.6,invalid\n",
        ] {
            std::fs::write(&invalid, content).unwrap();
            assert!(read_corpus(&invalid).is_err(), "{content}");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn builds_osrm_route_url() {
        let url = osrm_route_url("http://127.0.0.1:5000/", "driving", &row("r1"));
        assert_eq!(
            url,
            "http://127.0.0.1:5000/route/v1/driving/4.9,52.3;5.1,52.4?overview=false&radiuses=500;500"
        );
    }

    #[test]
    fn builds_netweevil_route_url_and_body() {
        assert_eq!(
            netweevil_route_url("http://127.0.0.1:8080/"),
            "http://127.0.0.1:8080/v1/route"
        );
        let body = netweevil_route_body(&row("r1"), Some("car_v1"));
        assert_eq!(body["profile_id"], "car_v1");
        assert_eq!(body["request"]["route_id"], "r1");
        assert_eq!(body["request"]["returns"]["geometry"], "none");
        assert_eq!(body["request"]["origin"]["lon"], 4.9);

        let anonymous = netweevil_route_body(&row("r1"), None);
        assert!(anonymous.get("profile_id").is_none());
    }

    #[test]
    fn valhalla_requests_summary_in_osrm_units() {
        let body = valhalla_route_body(&row("v"), "bicycle");
        assert_eq!(body["costing"], "bicycle");
        assert_eq!(body["locations"][0]["lon"], 4.9);
        assert_eq!(body["locations"][1]["search_cutoff"], 500.0);
        assert_eq!(body["format"], "osrm");
        assert_eq!(body["shape_format"], "no_shape");
        assert_eq!(body["directions_type"], "none");
        let outcome = parse_osrm_route(&serde_json::json!({
            "code": "Ok", "routes": [{"duration": 60.0, "distance": 1200.0}]
        }));
        assert_eq!(outcome.distance_m, Some(1200.0));
    }

    #[test]
    fn comparison_rejects_changed_coordinates_or_workloads() {
        let original = row("same-id");
        let mut changed = original.clone();
        changed.target_lon += 0.00000001;
        let baseline = BenchSettings {
            corpus_sha256: corpus_fingerprint(&[original]),
            workload: "route".to_string(),
            ..BenchSettings::default()
        };
        let mut candidate = baseline.clone();
        candidate.corpus_path = "a-copy-at-another-path.csv".to_string();
        assert!(validate_comparison(&baseline, &candidate).is_ok());
        candidate.corpus_sha256 = corpus_fingerprint(&[changed]);
        assert!(validate_comparison(&baseline, &candidate).is_err());
        candidate = baseline.clone();
        candidate.workload = "route-geometry".to_string();
        assert!(validate_comparison(&baseline, &candidate).is_err());
    }

    #[test]
    fn parses_osrm_route_response() {
        let body = serde_json::json!({
            "code": "Ok",
            "routes": [{ "duration": 1234.5, "distance": 20345.0 }],
        });
        let outcome = parse_osrm_route(&body);
        assert!(outcome.ok);
        assert_eq!(outcome.duration_s, Some(1234.5));
        assert_eq!(outcome.distance_m, Some(20345.0));
    }

    #[test]
    fn reports_osrm_routing_failures() {
        let no_route = parse_osrm_route(&serde_json::json!({ "code": "NoRoute" }));
        assert!(!no_route.ok);
        assert!(!no_route.status_failure);
        assert!(no_route.error.unwrap().contains("NoRoute"));

        let empty = parse_osrm_route(&serde_json::json!({ "code": "Ok", "routes": [] }));
        assert!(!empty.ok);

        let malformed = parse_osrm_route(&serde_json::json!({ "routes": [] }));
        assert!(!malformed.ok);
    }

    #[test]
    fn parses_netweevil_route_response() {
        let body = serde_json::json!({
            "service": {},
            "result": {
                "outcome": "legal",
                "summary": { "total_travel_time_s": 900.5,
                    "total_generalized_cost": 912.25, "total_distance_m": 15000 },
            },
        });
        let outcome = parse_netweevil_route(&body);
        assert!(outcome.ok);
        assert_eq!(outcome.duration_s, Some(900.5));
        assert_eq!(outcome.generalized_cost, Some(912.25));
        assert_eq!(outcome.distance_m, Some(15000.0));

        let unreachable = parse_netweevil_route(&serde_json::json!({
            "result": { "outcome": "unreachable", "summary": {} },
        }));
        assert!(!unreachable.ok);
        assert!(!unreachable.status_failure);
    }

    #[test]
    fn merges_repeated_iterations_by_id() {
        let records = vec![
            RequestRecord {
                index: 0,
                id: "a".to_string(),
                bucket: Some("short".to_string()),
                ok: false,
                latency_ms: 30.0,
                duration_s: None,
                distance_m: None,
                generalized_cost: None,
                matrix_cell_counts: None,
                error: Some("unreachable".to_string()),
            },
            RequestRecord {
                index: 1,
                id: "a".to_string(),
                bucket: Some("short".to_string()),
                ok: true,
                latency_ms: 10.0,
                duration_s: Some(60.0),
                distance_m: Some(1000.0),
                generalized_cost: Some(60.0),
                matrix_cell_counts: None,
                error: None,
            },
        ];
        let merged = merge_by_id(&records);
        let entry = &merged["a"];
        assert!(entry.ok);
        assert_eq!(entry.duration_s, Some(60.0));
        assert!((entry.latency_ms - 20.0).abs() < 1e-9);
    }

    #[test]
    fn computes_relative_difference() {
        assert_eq!(relative_difference(100.0, 110.0), Some(0.1));
        assert_eq!(relative_difference(100.0, 90.0), Some(0.1));
        assert_eq!(relative_difference(0.0, 5.0), None);
    }

    #[test]
    fn rejects_malformed_bbox() {
        assert!(parse_bbox("1,2,3").is_err());
        assert!(parse_bbox("5,52,4,53").is_err());
        let bounds = parse_bbox("4.0,52.0,5.0,53.0").unwrap();
        assert!(bounds_contain(&bounds, (4.5, 52.5)));
        assert!(!bounds_contain(&bounds, (6.0, 52.5)));
    }
}
