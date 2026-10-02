//! Historical (time-of-week) and live traffic speeds.
//!
//! Speeds act on edge travel time through a per-edge rate ratio
//! `phi = v_traffic / v_profile`: an edge whose profile time is `t` takes
//! `t / phi` under traffic, and its generalized cost changes by
//! `cost_time_weight * (t / phi - t)`, the same model the exact temporal
//! engine uses for `SpeedFactor` rules. Departure-bucket routing customizes
//! the CCH with the bucket's per-edge metric; arrival times are integrated
//! along the chosen path with the speed in force at every instant, which is
//! FIFO: entering an edge later never exits it earlier.

mod router;

use std::fmt;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use chrono::{Datelike, TimeZone, Timelike};
use chrono_tz::Tz;
use netweevil_core::{
    CompiledEdgeMetric, CompiledProfileBundle, RoadClass, TRAFFIC_SPEED_NO_DATA,
    TRAFFIC_SPEED_UNITS_PER_KPH, TopologyBundle, TrafficSpeedBundle, TravelMode,
};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub use router::{
    LiveSwapReport, TrafficBucketInfo, TrafficProfileStatus, TrafficRouter, TrafficSelection,
    TrafficService, TrafficStatus,
};

const NO_PATTERN: u32 = u32::MAX;

/// How observed speeds combine with the profile's own edge speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficSpeedPolicy {
    /// Observed speeds replace the profile speed (cars).
    Replace,
    /// Observed speeds can only slow the vehicle below its profile speed
    /// (heavy goods vehicles and transit vehicles keep their own limits).
    SlowDownOnly,
}

impl TrafficSpeedPolicy {
    /// Traffic applies to motorized modes only.
    pub fn for_mode(mode: TravelMode) -> Option<Self> {
        match mode {
            TravelMode::Car => Some(Self::Replace),
            TravelMode::Hgv | TravelMode::Transit => Some(Self::SlowDownOnly),
            TravelMode::Bicycle | TravelMode::Foot => None,
        }
    }
}

/// Historical speed patterns bound to the edges of one topology.
pub struct HistoricalTraffic {
    traffic_id: String,
    timezone_name: String,
    timezone: Tz,
    bucket_minutes: u16,
    buckets_per_day: u32,
    buckets_per_week: u32,
    pattern_speeds: Vec<u16>,
    edge_patterns: Vec<u32>,
    /// Buckets whose speed columns are identical across every pattern
    /// produce identical metrics and share one representative.
    bucket_class: Vec<u32>,
    pattern_count: usize,
    feature_count: usize,
    matched_edge_count: usize,
}

impl fmt::Debug for HistoricalTraffic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HistoricalTraffic")
            .field("traffic_id", &self.traffic_id)
            .field("timezone", &self.timezone_name)
            .field("bucket_minutes", &self.bucket_minutes)
            .field("pattern_count", &self.pattern_count)
            .field("matched_edge_count", &self.matched_edge_count)
            .finish()
    }
}

impl HistoricalTraffic {
    pub fn bind(bundle: &TrafficSpeedBundle, topology: &TopologyBundle) -> Result<Self> {
        bundle.validate().map_err(anyhow::Error::msg)?;
        let timezone: Tz = bundle
            .timezone
            .parse()
            .map_err(|error| anyhow::anyhow!("invalid traffic timezone: {error}"))?;
        let edge_patterns = (0..topology.edge_count())
            .into_par_iter()
            .map(|edge_index| {
                let edge = topology.routing_edge(edge_index);
                bundle
                    .lookup(edge.source_way_id, edge.source_direction)
                    .unwrap_or(NO_PATTERN)
            })
            .collect::<Vec<_>>();
        let matched_edge_count = edge_patterns
            .iter()
            .filter(|pattern| **pattern != NO_PATTERN)
            .count();
        let buckets_per_week = bundle.buckets_per_week() as u32;
        let pattern_count = bundle.pattern_count();
        let column = |bucket: u32| {
            (0..pattern_count).map(move |pattern| {
                bundle.pattern_speeds[pattern * buckets_per_week as usize + bucket as usize]
            })
        };
        let mut representatives = FxHashMap::<u64, Vec<u32>>::default();
        let mut bucket_class = Vec::with_capacity(buckets_per_week as usize);
        for bucket in 0..buckets_per_week {
            let hash = column(bucket).fold(0xcbf2_9ce4_8422_2325_u64, |hash, speed| {
                (hash ^ u64::from(speed)).wrapping_mul(0x0100_0000_01b3)
            });
            let candidates = representatives.entry(hash).or_default();
            let class = candidates
                .iter()
                .copied()
                .find(|&other| column(other).eq(column(bucket)))
                .unwrap_or_else(|| {
                    candidates.push(bucket);
                    bucket
                });
            bucket_class.push(class);
        }
        Ok(Self {
            traffic_id: bundle.traffic_id.clone(),
            timezone_name: bundle.timezone.clone(),
            timezone,
            bucket_minutes: bundle.bucket_minutes,
            buckets_per_day: bundle.buckets_per_day() as u32,
            buckets_per_week,
            pattern_speeds: bundle.pattern_speeds.clone(),
            edge_patterns,
            bucket_class,
            pattern_count,
            feature_count: bundle.feature_ids.len(),
            matched_edge_count,
        })
    }

    pub fn traffic_id(&self) -> &str {
        &self.traffic_id
    }

    pub fn matched_edge_count(&self) -> usize {
        self.matched_edge_count
    }

    pub fn distinct_bucket_metrics(&self) -> usize {
        self.bucket_class
            .iter()
            .enumerate()
            .filter(|(bucket, class)| *bucket as u32 == **class)
            .count()
    }

    fn bucket_seconds(&self) -> i64 {
        i64::from(self.bucket_minutes) * 60
    }

    /// Local time-of-week bucket and the seconds elapsed inside it.
    fn locate(&self, at: OffsetDateTime) -> (u32, f64) {
        let local = self
            .timezone
            .timestamp_opt(at.unix_timestamp(), 0)
            .single()
            .expect("a UTC instant maps to exactly one local time");
        let second_of_day = i64::from(local.num_seconds_from_midnight());
        let day = local.weekday().num_days_from_monday();
        let bucket = day * self.buckets_per_day + (second_of_day / self.bucket_seconds()) as u32;
        let into = (second_of_day % self.bucket_seconds()) as f64
            + f64::from(at.nanosecond()) / 1_000_000_000.0;
        (bucket, into)
    }

    pub fn bucket_at(&self, at: OffsetDateTime) -> u32 {
        self.locate(at).0
    }

    /// The next local bucket boundary strictly after `at`. Bucket widths
    /// divide an hour, so clock changes fall on boundaries.
    fn next_boundary(&self, at: OffsetDateTime) -> OffsetDateTime {
        let (_, into) = self.locate(at);
        at + time::Duration::seconds_f64(self.bucket_seconds() as f64 - into)
    }

    pub fn bucket_label(&self, bucket: u32) -> String {
        const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        let day = (bucket / self.buckets_per_day) as usize;
        let minute = (bucket % self.buckets_per_day) * u32::from(self.bucket_minutes);
        format!("{} {:02}:{:02}", DAYS[day % 7], minute / 60, minute % 60)
    }

    fn speed_kph(&self, edge_index: usize, bucket: u32) -> Option<f64> {
        let pattern = *self.edge_patterns.get(edge_index)?;
        if pattern == NO_PATTERN {
            return None;
        }
        let speed = self.pattern_speeds
            [pattern as usize * self.buckets_per_week as usize + bucket as usize];
        (speed != TRAFFIC_SPEED_NO_DATA).then(|| f64::from(speed) / TRAFFIC_SPEED_UNITS_PER_KPH)
    }

    fn has_pattern(&self, edge_index: usize) -> bool {
        self.edge_patterns
            .get(edge_index)
            .is_some_and(|pattern| *pattern != NO_PATTERN)
    }
}

/// Direction of a live update relative to the source geometry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveTrafficDirection {
    Forward,
    Backward,
    #[default]
    Both,
}

/// One live observation for a source feature, as pushed by a feed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveTrafficUpdate {
    #[serde(alias = "way_id")]
    pub feature_id: i64,
    #[serde(default)]
    pub direction: LiveTrafficDirection,
    /// Observed speed; replaces the historical and profile speed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_kph: Option<f64>,
    /// The feature is impassable while the update is valid.
    #[serde(default)]
    pub closed: bool,
    /// Removes any live state of the feature instead of setting one.
    #[serde(default)]
    pub clear: bool,
    /// RFC 3339 expiry. Defaults to `ttl_s` after receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Validity after receipt in seconds when `expires_at` is absent
    /// (default 900).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_s: Option<f64>,
}

const DEFAULT_LIVE_TTL_S: f64 = 900.0;

/// A validated live entry for one directed source feature.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveTrafficEntry {
    pub feature_id: i64,
    /// `1` forward, `-1` backward (geometry-relative).
    pub direction: i8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_kph: Option<f64>,
    pub closed: bool,
    /// Unix seconds; the entry applies from receipt.
    pub valid_from: i64,
    /// Unix seconds, rounded up to a whole minute so that waiting for a
    /// reopening can be evaluated exactly at minute resolution.
    pub expires_at: i64,
    pub edge_count: usize,
}

impl LiveTrafficEntry {
    fn active_at(&self, unix_s: f64) -> bool {
        self.valid_from as f64 <= unix_s && unix_s < self.expires_at as f64
    }
}

/// An immutable set of live entries. Readers hold an `Arc` to the snapshot
/// they started with; updates publish a new snapshot.
#[derive(Debug, Default)]
pub struct LiveTrafficSnapshot {
    pub version: u64,
    entries: Vec<LiveTrafficEntry>,
    /// `(edge, entry)` pairs sorted by edge.
    edge_entries: Vec<(u32, u32)>,
}

impl LiveTrafficSnapshot {
    pub fn entries(&self) -> &[LiveTrafficEntry] {
        &self.entries
    }

    pub fn active_count(&self, at: OffsetDateTime) -> usize {
        let unix_s = unix_seconds(at);
        self.entries
            .iter()
            .filter(|entry| entry.active_at(unix_s))
            .count()
    }

    fn active_indices(&self, at: OffsetDateTime) -> Vec<u32> {
        let unix_s = unix_seconds(at);
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.active_at(unix_s))
            .map(|(index, _)| index as u32)
            .collect()
    }

    fn edge_entries(&self, edge_index: usize) -> impl Iterator<Item = &LiveTrafficEntry> {
        let edge = edge_index as u32;
        let start = self
            .edge_entries
            .partition_point(|(other, _)| *other < edge);
        self.edge_entries[start..]
            .iter()
            .take_while(move |(other, _)| *other == edge)
            .map(|(_, entry)| &self.entries[*entry as usize])
    }

    /// Instants after `at` at which some entry starts or stops applying.
    fn next_change_after(&self, at: OffsetDateTime) -> Option<OffsetDateTime> {
        let unix_s = unix_seconds(at);
        self.entries
            .iter()
            .flat_map(|entry| [entry.valid_from, entry.expires_at])
            .filter(|instant| *instant as f64 > unix_s)
            .min()
            .and_then(|instant| OffsetDateTime::from_unix_timestamp(instant).ok())
    }
}

fn unix_seconds(at: OffsetDateTime) -> f64 {
    at.unix_timestamp() as f64 + f64::from(at.nanosecond()) / 1_000_000_000.0
}

/// Sorted `(source feature, edge)` pairs for resolving live updates.
struct FeatureEdgeIndex {
    pairs: Vec<(i64, u32)>,
}

impl FeatureEdgeIndex {
    fn build(topology: &TopologyBundle) -> Self {
        let mut pairs = (0..topology.edge_count())
            .map(|edge_index| {
                (
                    topology.routing_edge(edge_index).source_way_id,
                    edge_index as u32,
                )
            })
            .collect::<Vec<_>>();
        pairs.par_sort_unstable();
        Self { pairs }
    }

    fn edges(
        &self,
        topology: &TopologyBundle,
        feature_id: i64,
        direction: i8,
    ) -> impl Iterator<Item = u32> {
        let start = self.pairs.partition_point(|(id, _)| *id < feature_id);
        self.pairs[start..]
            .iter()
            .take_while(move |(id, _)| *id == feature_id)
            .map(|(_, edge)| *edge)
            .filter(move |edge| {
                let source_direction = topology.routing_edge(*edge as usize).source_direction;
                (source_direction < 0) == (direction < 0)
            })
    }
}

/// Builds the next live snapshot from `current` and a batch of updates.
/// Returns the snapshot and the feature ids that matched no edge.
fn next_live_snapshot(
    current: &LiveTrafficSnapshot,
    index: &FeatureEdgeIndex,
    topology: &TopologyBundle,
    updates: &[LiveTrafficUpdate],
    replace: bool,
    now: OffsetDateTime,
) -> Result<(LiveTrafficSnapshot, Vec<i64>)> {
    let now_s = now.unix_timestamp();
    let mut entries = std::collections::BTreeMap::<(i64, i8), LiveTrafficEntry>::new();
    if !replace {
        for entry in &current.entries {
            if entry.expires_at > now_s {
                entries.insert((entry.feature_id, entry.direction), entry.clone());
            }
        }
    }
    let mut unmatched = Vec::new();
    for update in updates {
        let directions: &[i8] = match update.direction {
            LiveTrafficDirection::Forward => &[1],
            LiveTrafficDirection::Backward => &[-1],
            LiveTrafficDirection::Both => &[-1, 1],
        };
        let context = || format!("live update for feature {}", update.feature_id);
        if update.clear {
            if update.closed || update.speed_kph.is_some() {
                bail!(
                    "{}: clear cannot be combined with closed or speed_kph",
                    context()
                );
            }
            for direction in directions {
                entries.remove(&(update.feature_id, *direction));
            }
            continue;
        }
        match (update.closed, update.speed_kph) {
            (true, None) => {}
            (false, Some(speed)) if speed.is_finite() && speed > 0.0 && speed <= 1000.0 => {}
            (false, Some(_)) => bail!("{}: speed_kph must be in (0, 1000]", context()),
            _ => bail!("{}: set exactly one of closed=true or speed_kph", context()),
        }
        let expires_at = match (&update.expires_at, update.ttl_s) {
            (Some(_), Some(_)) => bail!("{}: set only one of expires_at or ttl_s", context()),
            (Some(raw), None) => crate::parse_datetime(raw).with_context(context)?,
            (None, ttl) => {
                let ttl = ttl.unwrap_or(DEFAULT_LIVE_TTL_S);
                if !ttl.is_finite() || ttl <= 0.0 {
                    bail!("{}: ttl_s must be positive", context());
                }
                now + time::Duration::seconds_f64(ttl)
            }
        };
        if expires_at <= now {
            bail!("{}: expiry is not in the future", context());
        }
        let expires_at = expires_at.unix_timestamp() + i64::from(expires_at.nanosecond() > 0);
        let expires_at = expires_at + (60 - expires_at.rem_euclid(60)) % 60;
        let mut matched = false;
        for &direction in directions {
            let edge_count = index.edges(topology, update.feature_id, direction).count();
            // A direction without edges (one-way features) carries no state.
            if edge_count == 0 {
                entries.remove(&(update.feature_id, direction));
                continue;
            }
            matched = true;
            entries.insert(
                (update.feature_id, direction),
                LiveTrafficEntry {
                    feature_id: update.feature_id,
                    direction,
                    speed_kph: update.speed_kph,
                    closed: update.closed,
                    valid_from: now_s,
                    expires_at,
                    edge_count,
                },
            );
        }
        if !matched {
            unmatched.push(update.feature_id);
        }
    }
    let entries = entries.into_values().collect::<Vec<_>>();
    let mut edge_entries = Vec::new();
    for (entry_index, entry) in entries.iter().enumerate() {
        for edge in index.edges(topology, entry.feature_id, entry.direction) {
            edge_entries.push((edge, entry_index as u32));
        }
    }
    edge_entries.sort_unstable();
    Ok((
        LiveTrafficSnapshot {
            version: current.version + 1,
            entries,
            edge_entries,
        },
        unmatched,
    ))
}

/// Traffic state used to evaluate one edge over time: historical patterns,
/// live entries and the profile's speed policy.
#[derive(Clone)]
pub(crate) struct TemporalTraffic {
    pub(crate) historical: Option<Arc<HistoricalTraffic>>,
    pub(crate) live: Option<Arc<LiveTrafficSnapshot>>,
    pub(crate) policy: TrafficSpeedPolicy,
}

impl fmt::Debug for TemporalTraffic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TemporalTraffic")
            .field("historical", &self.historical)
            .field("live_version", &self.live.as_ref().map(|live| live.version))
            .field("policy", &self.policy)
            .finish()
    }
}

/// The traffic speed on an edge at one instant and when it can next change.
struct EdgeRate {
    phi: f64,
    next_change: Option<OffsetDateTime>,
}

impl TemporalTraffic {
    /// Whether any traffic data can affect `edge_index`.
    pub(crate) fn affects_edge(&self, edge_index: usize) -> bool {
        self.historical
            .as_ref()
            .is_some_and(|historical| historical.has_pattern(edge_index))
            || self
                .live
                .as_ref()
                .is_some_and(|live| live.edge_entries(edge_index).next().is_some())
    }

    /// A live closure on `edge_index` at `at`.
    pub(crate) fn closed_at(&self, edge_index: usize, at: OffsetDateTime) -> bool {
        let unix_s = unix_seconds(at);
        self.live.as_ref().is_some_and(|live| {
            live.edge_entries(edge_index)
                .any(|entry| entry.closed && entry.active_at(unix_s))
        })
    }

    /// Whether a live closure ever applies to `edge_index`.
    pub(crate) fn has_closure(&self, edge_index: usize) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.edge_entries(edge_index).any(|entry| entry.closed))
    }

    fn rate_at(
        &self,
        topology: &TopologyBundle,
        metric: &CompiledEdgeMetric,
        edge_index: usize,
        at: OffsetDateTime,
    ) -> EdgeRate {
        let unix_s = unix_seconds(at);
        let mut next_change = None::<OffsetDateTime>;
        let mut speed = None;
        if let Some(live) = &self.live {
            for entry in live.edge_entries(edge_index) {
                if entry.active_at(unix_s) {
                    speed = speed.or(entry.speed_kph);
                    next_change = min_instant(
                        next_change,
                        OffsetDateTime::from_unix_timestamp(entry.expires_at).ok(),
                    );
                } else if entry.valid_from as f64 > unix_s {
                    next_change = min_instant(
                        next_change,
                        OffsetDateTime::from_unix_timestamp(entry.valid_from).ok(),
                    );
                }
            }
        }
        if speed.is_none()
            && let Some(historical) = &self.historical
            && historical.has_pattern(edge_index)
        {
            speed = historical.speed_kph(edge_index, historical.bucket_at(at));
            next_change = min_instant(next_change, Some(historical.next_boundary(at)));
        }
        EdgeRate {
            phi: speed.map_or(1.0, |speed| {
                speed_ratio(topology, metric, edge_index, speed, self.policy)
            }),
            next_change,
        }
    }

    /// Seconds needed to cover `profile_seconds` of profile-speed travel on
    /// `edge_index` entered at `entry`, with `rule_factor` applied on top of
    /// the traffic speed. Integrating the piecewise-constant speed keeps the
    /// traversal FIFO.
    pub(crate) fn traversal_seconds(
        &self,
        topology: &TopologyBundle,
        metric: &CompiledEdgeMetric,
        edge_index: usize,
        entry: OffsetDateTime,
        profile_seconds: f64,
        rule_factor: f64,
    ) -> f64 {
        let mut at = entry;
        let mut remaining = profile_seconds;
        let mut elapsed = 0.0;
        // Each step crosses one bucket or live boundary; a traversal longer
        // than several weeks finishes at its last rate.
        for _ in 0..8 * 7 * 24 * 60 {
            let rate = self.rate_at(topology, metric, edge_index, at);
            let speed = rule_factor * rate.phi;
            let needed = remaining / speed;
            match rate.next_change {
                Some(change) if at + time::Duration::seconds_f64(needed) > change => {
                    let step = (change - at).as_seconds_f64();
                    remaining -= step * speed;
                    elapsed += step;
                    at = change;
                }
                _ => return elapsed + needed,
            }
        }
        elapsed + remaining / (rule_factor * self.rate_at(topology, metric, edge_index, at).phi)
    }
}

fn min_instant(
    left: Option<OffsetDateTime>,
    right: Option<OffsetDateTime>,
) -> Option<OffsetDateTime> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

/// `v_traffic / v_profile` for one edge. Ferries, zero-length edges and
/// edges without a profile time keep their profile speed.
fn speed_ratio(
    topology: &TopologyBundle,
    metric: &CompiledEdgeMetric,
    edge_index: usize,
    speed_kph: f64,
    policy: TrafficSpeedPolicy,
) -> f64 {
    let length_m = f64::from(topology.routing_edge(edge_index).length_m);
    let Some(profile_time_s) = metric.travel_time_s else {
        return 1.0;
    };
    if length_m <= 0.0
        || profile_time_s <= 0.0
        || topology.edge_profile(edge_index).road_class == RoadClass::Ferry
    {
        return 1.0;
    }
    let phi = speed_kph / 3.6 * profile_time_s / length_m;
    match policy {
        TrafficSpeedPolicy::Replace => phi,
        TrafficSpeedPolicy::SlowDownOnly => phi.min(1.0),
    }
}

/// Generalized cost of an edge (or a fraction of it) whose profile time
/// `profile_time_s` became `traffic_time_s`. Floored at zero: an observed
/// speed far above the profile speed cannot make an edge pay back cost.
pub(crate) fn traffic_cost(
    profile_cost: f64,
    profile_time_s: f64,
    traffic_time_s: f64,
    cost_time_weight: f64,
) -> f64 {
    (profile_cost + (traffic_time_s - profile_time_s) * cost_time_weight).max(0.0)
}

/// Per-edge metrics for a departure bucket plus the live entries active at
/// the departure instant.
fn bucket_edge_metrics(
    topology: &TopologyBundle,
    base: &CompiledProfileBundle,
    historical: Option<&HistoricalTraffic>,
    bucket: Option<u32>,
    live: &LiveTrafficSnapshot,
    active: &[u32],
    policy: TrafficSpeedPolicy,
) -> Vec<CompiledEdgeMetric> {
    let weight = base.turn_costs.cost_time_weight;
    let mut metrics = base.edge_metrics.clone();
    metrics
        .par_iter_mut()
        .enumerate()
        .for_each(|(edge_index, metric)| {
            let (Some(time_s), Some(cost)) = (metric.travel_time_s, metric.generalized_cost) else {
                return;
            };
            let speed = historical
                .zip(bucket)
                .and_then(|(historical, bucket)| historical.speed_kph(edge_index, bucket));
            if let Some(speed) = speed {
                let phi = speed_ratio(topology, metric, edge_index, speed, policy);
                let traffic_time = time_s / phi;
                metric.travel_time_s = Some(traffic_time);
                metric.generalized_cost = Some(traffic_cost(cost, time_s, traffic_time, weight));
            }
        });
    // `active` is sorted; live entries override the historical speed.
    for &(edge, entry_index) in &live.edge_entries {
        if active.binary_search(&entry_index).is_err() {
            continue;
        }
        let entry = &live.entries[entry_index as usize];
        let edge_index = edge as usize;
        let base_metric = &base.edge_metrics[edge_index];
        let metric = &mut metrics[edge_index];
        let (Some(time_s), Some(cost)) = (base_metric.travel_time_s, base_metric.generalized_cost)
        else {
            continue;
        };
        if entry.closed {
            metric.travel_time_s = None;
            metric.generalized_cost = None;
        } else if let Some(speed) = entry.speed_kph {
            let phi = speed_ratio(topology, base_metric, edge_index, speed, policy);
            let traffic_time = time_s / phi;
            metric.travel_time_s = Some(traffic_time);
            metric.generalized_cost = Some(traffic_cost(cost, time_s, traffic_time, weight));
        }
    }
    metrics
}
