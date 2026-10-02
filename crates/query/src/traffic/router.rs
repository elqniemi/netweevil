//! Traffic-aware execution: per-bucket customized engines in an LRU cache,
//! departure-bucket routing with integrated arrival times, the exact
//! time-dependent engine, and atomically published live updates.

use std::collections::{BTreeMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;

use anyhow::{Result, bail};
use netweevil_core::{
    CompiledAcceleration, CompiledProfileBundle, TopologyBundle, TrafficSpeedBundle,
};
use netweevil_profile::ReturnGeometry;
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::{
    FeatureEdgeIndex, HistoricalTraffic, LiveTrafficSnapshot, LiveTrafficUpdate, TemporalTraffic,
    TrafficSpeedPolicy, bucket_edge_metrics, next_live_snapshot,
};
use crate::temporal::{
    EXACT_TEMPORAL_ROUTE_WARNING, TemporalCandidatePath, build_temporal_route_result,
    context_from_request, execute_temporal_route_with_graph, replay_temporal_edge_path,
};
use crate::{
    AnalysisOutcome, ConditionalTurns, EngineMode, FallbackPolicy, HopSelectionInfo, MatrixResult,
    OdPairsDocument, OdResult, PointSetDocument, PreparedRoutingEngine, RouteRequest, RouteResult,
    ServiceAreaRequest, ServiceAreaResult, TemporalRequestOptions, TrafficMode,
    edge_traversal_factor, parse_datetime, request_returns_detailed_path,
};

const BUCKET_ROUTE_WARNING: &str = "Traffic route chosen on the CCH customized for the departure time-of-week bucket (and live updates valid at departure); arrival times integrate the traffic speed in force along the path. Use traffic: time_dependent for exact time-dependent optimization.";
const BUCKET_BATCH_WARNING: &str = "Costs and times use the CCH customized for the departure time-of-week bucket (and live updates valid at departure); they are not integrated over the trip.";
const CLOSED_PATH_WARNING: &str = "The departure-bucket path was closed at a later entry time; the exact time-dependent engine was used.";

/// How a traffic-aware request was answered.
#[derive(Debug, Clone, Serialize)]
pub struct TrafficSelection {
    pub profile_id: String,
    /// `departure_bucket_cch` or `time_dependent_exact`.
    pub engine: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traffic_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub departure_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket: Option<TrafficBucketInfo>,
    pub live_version: u64,
    /// Live entries baked into the departure-bucket metric.
    pub live_entries_applied: usize,
    /// `hit` when a customized engine was reused, `miss` when this request
    /// customized it, `none` for the exact engine.
    pub engine_cache: &'static str,
    pub prepare_ms: f64,
    /// Bucket boundaries between departure and arrival: zero means the
    /// departure-bucket metric held for the whole trip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket_boundaries_crossed: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrafficBucketInfo {
    pub index: u32,
    pub label: String,
    /// Representative bucket whose metric this bucket shares.
    pub metric_bucket: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileSwapTiming {
    pub profile_id: String,
    pub customize_ms: f64,
    pub engine_cache: &'static str,
}

/// Result of publishing a live update batch.
#[derive(Debug, Clone, Serialize)]
pub struct LiveSwapReport {
    pub version: u64,
    pub update_count: usize,
    pub entry_count: usize,
    pub active_entry_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unmatched_feature_ids: Vec<i64>,
    /// Building the new snapshot (validation and edge resolution).
    pub snapshot_ms: f64,
    /// CCH customization and engine preparation per profile.
    pub profiles: Vec<ProfileSwapTiming>,
    /// From receipt to publication; readers keep the old engines until then.
    pub swap_ms: f64,
    pub published_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrafficProfileStatus {
    pub profile_id: String,
    pub policy: TrafficSpeedPolicy,
    pub cached_engines: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrafficStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traffic_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket_minutes: Option<u16>,
    pub pattern_count: usize,
    pub feature_count: usize,
    pub matched_edge_count: usize,
    pub distinct_bucket_metrics: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_bucket: Option<TrafficBucketInfo>,
    pub live_version: u64,
    pub live_entry_count: usize,
    pub live_active_entry_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_swap: Option<LiveSwapReport>,
    pub profiles: Vec<TrafficProfileStatus>,
}

/// State shared by the routers of one dataset runtime.
struct TrafficShared {
    topology: Arc<TopologyBundle>,
    historical: Option<Arc<HistoricalTraffic>>,
    feature_index: OnceLock<FeatureEdgeIndex>,
    live: RwLock<Arc<LiveTrafficSnapshot>>,
    /// Serializes live updates and keeps the last swap report.
    writer: Mutex<Option<LiveSwapReport>>,
}

impl TrafficShared {
    fn live_snapshot(&self) -> Arc<LiveTrafficSnapshot> {
        self.live.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Traffic for every motorized profile of one dataset.
pub struct TrafficService {
    shared: Arc<TrafficShared>,
    routers: BTreeMap<String, Arc<TrafficRouter>>,
}

impl TrafficService {
    /// Binds `historical` (if any) to `topology` and creates a router for
    /// each motorized profile. Each router keeps up to `cache_entries`
    /// customized engines.
    pub fn new(
        topology: Arc<TopologyBundle>,
        historical: Option<&TrafficSpeedBundle>,
        profiles: impl IntoIterator<Item = (String, Arc<PreparedRoutingEngine>)>,
        cache_entries: usize,
    ) -> Result<Self> {
        if cache_entries == 0 {
            bail!("the traffic engine cache needs at least one entry");
        }
        let historical = historical
            .map(|bundle| HistoricalTraffic::bind(bundle, topology.as_ref()).map(Arc::new))
            .transpose()?;
        let shared = Arc::new(TrafficShared {
            topology,
            historical,
            feature_index: OnceLock::new(),
            live: RwLock::new(Arc::new(LiveTrafficSnapshot::default())),
            writer: Mutex::new(None),
        });
        let routers = profiles
            .into_iter()
            .filter_map(|(profile_id, base)| {
                let policy = TrafficSpeedPolicy::for_mode(base.metrics().mode)?;
                Some((
                    profile_id.clone(),
                    Arc::new(TrafficRouter {
                        profile_id,
                        base,
                        policy,
                        shared: shared.clone(),
                        cache: EngineCache::new(cache_entries),
                        conditional_turns: OnceLock::new(),
                    }),
                ))
            })
            .collect();
        Ok(Self { shared, routers })
    }

    pub fn historical(&self) -> Option<&HistoricalTraffic> {
        self.shared.historical.as_deref()
    }

    pub fn router(&self, profile_id: &str) -> Option<Arc<TrafficRouter>> {
        self.routers.get(profile_id).cloned()
    }

    pub fn routers(&self) -> impl Iterator<Item = &Arc<TrafficRouter>> {
        self.routers.values()
    }

    pub fn live_snapshot(&self) -> Arc<LiveTrafficSnapshot> {
        self.shared.live_snapshot()
    }

    /// Validates and merges a live batch (or replaces all live state), then
    /// customizes every router's engine for the current bucket with the new
    /// state and only then publishes it. Requests that started earlier keep
    /// the previous snapshot and engines; none of them waits for this call.
    pub fn apply_live(
        &self,
        updates: &[LiveTrafficUpdate],
        replace: bool,
        now: OffsetDateTime,
    ) -> Result<LiveSwapReport> {
        let mut last = self.shared.writer.lock().unwrap_or_else(|e| e.into_inner());
        let started = Instant::now();
        let topology = self.shared.topology.as_ref();
        let index = self
            .shared
            .feature_index
            .get_or_init(|| FeatureEdgeIndex::build(topology));
        let current = self.shared.live_snapshot();
        let (next, unmatched_feature_ids) =
            next_live_snapshot(&current, index, topology, updates, replace, now)?;
        let next = Arc::new(next);
        let snapshot_ms = elapsed_ms(started);
        let mut profiles = Vec::new();
        for router in self.routers.values() {
            let customize_started = Instant::now();
            let (_, hit) = router.prepare_with(now, &next)?;
            profiles.push(ProfileSwapTiming {
                profile_id: router.profile_id.clone(),
                customize_ms: elapsed_ms(customize_started),
                engine_cache: if hit { "hit" } else { "miss" },
            });
        }
        *self.shared.live.write().unwrap_or_else(|e| e.into_inner()) = next.clone();
        let report = LiveSwapReport {
            version: next.version,
            update_count: updates.len(),
            entry_count: next.entries.len(),
            active_entry_count: next.active_count(now),
            unmatched_feature_ids,
            snapshot_ms,
            profiles,
            swap_ms: elapsed_ms(started),
            published_at: format_time(now),
        };
        *last = Some(report.clone());
        Ok(report)
    }

    /// Re-applies the unexpired live entries of `previous` (a service over
    /// an earlier load of the same dataset) so a runtime reload keeps the
    /// live state. Entries are re-resolved by source feature id.
    pub fn carry_live_from(
        &self,
        previous: &TrafficService,
        now: OffsetDateTime,
    ) -> Result<Option<LiveSwapReport>> {
        let previous = previous.live_snapshot();
        let updates = previous
            .entries
            .iter()
            .filter(|entry| entry.expires_at > now.unix_timestamp())
            .map(|entry| LiveTrafficUpdate {
                feature_id: entry.feature_id,
                direction: if entry.direction < 0 {
                    super::LiveTrafficDirection::Backward
                } else {
                    super::LiveTrafficDirection::Forward
                },
                speed_kph: entry.speed_kph,
                closed: entry.closed,
                clear: false,
                expires_at: OffsetDateTime::from_unix_timestamp(entry.expires_at)
                    .ok()
                    .map(format_time),
                ttl_s: None,
            })
            .collect::<Vec<_>>();
        if updates.is_empty() {
            return Ok(None);
        }
        self.apply_live(&updates, true, now).map(Some)
    }

    /// Customizes, ahead of time, the engines that requests departing at
    /// `at` will select (for example just before a bucket boundary or a
    /// live expiry). Returns how many engines were built.
    pub fn prewarm(&self, at: OffsetDateTime) -> Result<usize> {
        let snapshot = self.shared.live_snapshot();
        let mut built = 0;
        for router in self.routers.values() {
            built += usize::from(!router.prepare_with(at, &snapshot)?.1);
        }
        Ok(built)
    }

    /// The next instant after `at` at which the metric for a departure can
    /// change: a bucket boundary or a live entry starting or expiring.
    pub fn next_event_after(&self, at: OffsetDateTime) -> Option<OffsetDateTime> {
        let boundary = self
            .shared
            .historical
            .as_ref()
            .map(|historical| historical.next_boundary(at));
        super::min_instant(boundary, self.shared.live_snapshot().next_change_after(at))
    }

    pub fn status(&self, now: OffsetDateTime) -> TrafficStatus {
        let historical = self.shared.historical.as_deref();
        let live = self.shared.live_snapshot();
        TrafficStatus {
            traffic_id: historical.map(|h| h.traffic_id.clone()),
            timezone: historical.map(|h| h.timezone_name.clone()),
            bucket_minutes: historical.map(|h| h.bucket_minutes),
            pattern_count: historical.map_or(0, |h| h.pattern_count),
            feature_count: historical.map_or(0, |h| h.feature_count),
            matched_edge_count: historical.map_or(0, |h| h.matched_edge_count),
            distinct_bucket_metrics: historical
                .map_or(0, HistoricalTraffic::distinct_bucket_metrics),
            current_bucket: historical.map(|h| bucket_info(h, h.bucket_at(now))),
            live_version: live.version,
            live_entry_count: live.entries.len(),
            live_active_entry_count: live.active_count(now),
            last_swap: self
                .shared
                .writer
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            profiles: self
                .routers
                .values()
                .map(|router| TrafficProfileStatus {
                    profile_id: router.profile_id.clone(),
                    policy: router.policy,
                    cached_engines: router.cache.len(),
                })
                .collect(),
        }
    }
}

/// Traffic-aware execution for one motorized profile.
pub struct TrafficRouter {
    profile_id: String,
    base: Arc<PreparedRoutingEngine>,
    policy: TrafficSpeedPolicy,
    shared: Arc<TrafficShared>,
    cache: EngineCache,
    /// Time-conditional turn restrictions of this profile's mode, for
    /// replaying departure-bucket paths; built on first use.
    conditional_turns: OnceLock<ConditionalTurns>,
}

enum TrafficPlan {
    DepartureBucket {
        departure: OffsetDateTime,
        use_live: bool,
    },
    TimeDependent {
        use_live: bool,
    },
}

/// The engine selected for one departure and the state it was built from.
struct Prepared {
    engine: Arc<PreparedRoutingEngine>,
    snapshot: Arc<LiveTrafficSnapshot>,
    selection: TrafficSelection,
}

impl TrafficRouter {
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    pub fn base_engine(&self) -> &Arc<PreparedRoutingEngine> {
        &self.base
    }

    pub fn cached_engine_count(&self) -> usize {
        self.cache.len()
    }

    /// Whether traffic applies to a request with these temporal options.
    pub fn applies_to(&self, temporal: &TemporalRequestOptions) -> bool {
        temporal.traffic != TrafficMode::Off
            && (temporal.departure_time.is_some() || temporal.arrive_by.is_some())
    }

    fn plan(&self, temporal: &TemporalRequestOptions) -> Result<Option<TrafficPlan>> {
        if !self.applies_to(temporal) {
            return Ok(None);
        }
        if !temporal.constraints.is_empty() || temporal.pareto.is_some() {
            bail!(
                "component-constrained and Pareto routes do not apply traffic speeds; set traffic: off"
            );
        }
        let use_live = temporal.traffic != TrafficMode::Historical;
        let exact = temporal.traffic == TrafficMode::TimeDependent
            || temporal.arrive_by.is_some()
            || temporal.scenario.is_some()
            || !temporal.overlays.is_empty();
        Ok(Some(if exact {
            TrafficPlan::TimeDependent { use_live }
        } else {
            let departure = temporal
                .departure_time
                .as_deref()
                .map(parse_datetime)
                .transpose()?
                .expect("checked by applies_to");
            TrafficPlan::DepartureBucket {
                departure,
                use_live,
            }
        }))
    }

    /// The engine customized for the departure bucket of `departure` plus
    /// (with `use_live`) the live entries valid at that instant.
    pub fn engine_at(
        &self,
        departure: OffsetDateTime,
        use_live: bool,
    ) -> Result<(Arc<PreparedRoutingEngine>, TrafficSelection)> {
        let prepared = self.prepare(departure, use_live)?;
        Ok((prepared.engine, prepared.selection))
    }

    fn prepare(&self, departure: OffsetDateTime, use_live: bool) -> Result<Prepared> {
        let snapshot = if use_live {
            self.shared.live_snapshot()
        } else {
            Arc::new(LiveTrafficSnapshot::default())
        };
        let started = Instant::now();
        let (engine, hit) = self.prepare_with(departure, &snapshot)?;
        let historical = self.shared.historical.as_deref();
        let active = snapshot.active_count(departure);
        Ok(Prepared {
            engine,
            selection: TrafficSelection {
                profile_id: self.profile_id.clone(),
                engine: "departure_bucket_cch",
                traffic_id: historical.map(|h| h.traffic_id.clone()),
                departure_time: Some(format_time(departure)),
                bucket: historical.map(|h| bucket_info(h, h.bucket_at(departure))),
                live_version: snapshot.version,
                live_entries_applied: active,
                engine_cache: if hit { "hit" } else { "miss" },
                prepare_ms: elapsed_ms(started),
                bucket_boundaries_crossed: None,
            },
            snapshot,
        })
    }

    fn metric_key(&self, departure: OffsetDateTime, snapshot: &LiveTrafficSnapshot) -> MetricKey {
        let bucket_class =
            self.shared.historical.as_ref().map(|historical| {
                historical.bucket_class[historical.bucket_at(departure) as usize]
            });
        let active = snapshot.active_indices(departure);
        MetricKey {
            bucket_class,
            live_version: if active.is_empty() {
                0
            } else {
                snapshot.version
            },
            active,
        }
    }

    fn prepare_with(
        &self,
        departure: OffsetDateTime,
        snapshot: &LiveTrafficSnapshot,
    ) -> Result<(Arc<PreparedRoutingEngine>, bool)> {
        let key = self.metric_key(departure, snapshot);
        let build_key = key.clone();
        self.cache.get_or_build(key, || {
            self.base
                .with_edge_metrics(Arc::new(self.bucket_metrics_for(&build_key, snapshot)))
        })
    }

    fn bucket_metrics_for(
        &self,
        key: &MetricKey,
        snapshot: &LiveTrafficSnapshot,
    ) -> CompiledProfileBundle {
        let base = self.base.metrics();
        // Edges the profile's vehicle excludes get traffic speeds too, so a
        // request vehicle that re-enables them sees the bucket's speeds.
        let unrestricted;
        let base = if base.vehicle.is_empty() {
            base
        } else {
            let mut metrics = base.clone();
            crate::vehicle::restore_vehicle_exclusions(&mut metrics);
            unrestricted = metrics;
            &unrestricted
        };
        let edge_metrics = bucket_edge_metrics(
            self.shared.topology.as_ref(),
            base,
            self.shared.historical.as_deref(),
            key.bucket_class,
            snapshot,
            &key.active,
            self.policy,
        );
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        let mut metrics = CompiledProfileBundle {
            schema_version: base.schema_version,
            profile_id: base.profile_id.clone(),
            // Distinct per metric so no cache keyed by profile hash can mix
            // traffic engines.
            profile_hash: format!("{}+traffic-{:016x}", base.profile_hash, hasher.finish()),
            mode: base.mode,
            turn_costs: base.turn_costs,
            components: base.components.clone(),
            temporal: base.temporal.clone(),
            source_topology_bundle_id: base.source_topology_bundle_id.clone(),
            acceleration: base
                .acceleration
                .as_ref()
                .map(|acceleration| CompiledAcceleration {
                    schema_version: acceleration.schema_version,
                    source_acceleration_bundle_id: acceleration
                        .source_acceleration_bundle_id
                        .clone(),
                    algorithm: acceleration.algorithm.clone(),
                    upward_weight: Vec::new(),
                    downward_weight: Vec::new(),
                    time_upward_weight: Vec::new(),
                    time_downward_weight: Vec::new(),
                    distance_upward_weight: Vec::new(),
                    distance_downward_weight: Vec::new(),
                }),
            edge_metrics,
            vehicle: base.vehicle.clone(),
        };
        let excluded = &self.base.metrics().vehicle.edges;
        if !excluded.is_empty() {
            crate::vehicle::exclude_vehicle_edges(&mut metrics, excluded);
        }
        metrics
    }

    /// The per-edge metric of the engine `engine_at(departure, use_live)`
    /// routes on, for differential checks against the exact engine.
    pub fn bucket_metrics(
        &self,
        departure: OffsetDateTime,
        use_live: bool,
    ) -> CompiledProfileBundle {
        let snapshot = if use_live {
            self.shared.live_snapshot()
        } else {
            Arc::new(LiveTrafficSnapshot::default())
        };
        let key = self.metric_key(departure, &snapshot);
        self.bucket_metrics_for(&key, &snapshot)
    }

    fn temporal_traffic(&self, snapshot: Option<Arc<LiveTrafficSnapshot>>) -> TemporalTraffic {
        TemporalTraffic {
            historical: self.shared.historical.clone(),
            live: snapshot,
            policy: self.policy,
        }
    }

    /// Executes a traffic-aware route, or returns `None` when traffic does
    /// not apply (no departure/arrival time, or `traffic: off`).
    pub fn execute_route(
        &self,
        request: &RouteRequest,
        edge_names: Option<&[String]>,
    ) -> Result<Option<(RouteResult, TrafficSelection)>> {
        let Some(plan) = self.plan(&request.temporal)? else {
            return Ok(None);
        };
        if request.fallback != FallbackPolicy::default() {
            bail!("traffic-aware routing requires the strict fallback policy");
        }
        Ok(Some(match plan {
            TrafficPlan::DepartureBucket {
                departure,
                use_live,
            } => self.route_on_bucket(request, departure, use_live, edge_names)?,
            TrafficPlan::TimeDependent { use_live } => {
                let snapshot = use_live.then(|| self.shared.live_snapshot());
                self.route_time_dependent(request, snapshot, edge_names)?
            }
        }))
    }

    fn route_on_bucket(
        &self,
        request: &RouteRequest,
        departure: OffsetDateTime,
        use_live: bool,
        edge_names: Option<&[String]>,
    ) -> Result<(RouteResult, TrafficSelection)> {
        let Prepared {
            engine,
            snapshot,
            mut selection,
        } = self.prepare(departure, use_live)?;
        let mut static_request = request.clone();
        static_request.temporal = static_options(&request.temporal);
        if !request_returns_detailed_path(&static_request.returns) {
            static_request.returns.geometry = ReturnGeometry::Full;
        }
        let static_result = engine.execute_route(&static_request)?;
        if static_result.outcome != AnalysisOutcome::Legal {
            let mut result = static_result;
            result.summary.departure_time = selection.departure_time.clone();
            return Ok((result, selection));
        }

        let edge_count = static_result.edge_path.len();
        let path = static_result
            .edge_path
            .iter()
            .enumerate()
            .map(|(position, &edge_id)| {
                let edge_index = edge_id as usize;
                let fraction = edge_traversal_factor(
                    edge_index,
                    position == 0,
                    position + 1 == edge_count,
                    &static_result.origin,
                    &static_result.destination,
                );
                (edge_index, fraction)
            })
            .collect::<Vec<_>>();
        let live = use_live.then_some(snapshot);
        let context = context_from_request(&request.temporal)?
            .with_traffic(self.temporal_traffic(live.clone()));
        // Replay on the profile metric of the request's vehicle when it
        // re-enables edges, so they carry their static times and costs.
        // Vehicles that only exclude edges leave the path's metric as is.
        let vehicle_engine = match self.base.request_adjustment(
            &crate::AvoidOptions::default(),
            request.temporal.vehicle.as_ref(),
        )? {
            crate::RequestAdjustment::Engine(engine) => Some(engine),
            _ => None,
        };
        let replay_engine = vehicle_engine.as_ref().unwrap_or(&self.base);
        let topology = replay_engine.topology();
        let metrics = replay_engine.metrics();
        let deadline = departure + time::Duration::days(366);
        // The bucket path follows static turn rules; time-conditional turn
        // restrictions are checked at the times the replay reaches them.
        let turns = self
            .conditional_turns
            .get_or_init(|| ConditionalTurns::for_mode(topology, metrics.mode.access_bit()));
        let Some((departure, arrival, generalized_cost, traversals)) = replay_temporal_edge_path(
            topology, metrics, &context, turns, &path, departure, deadline,
        ) else {
            let (mut result, mut selection) =
                self.route_time_dependent(request, live, edge_names)?;
            result.warnings.push(CLOSED_PATH_WARNING.to_string());
            selection.engine = "time_dependent_exact";
            return Ok((result, selection));
        };
        let candidate = TemporalCandidatePath {
            origin: static_result.origin.clone(),
            destination: static_result.destination.clone(),
            departure,
            arrival,
            generalized_cost,
            traversals,
            hop_info: HopSelectionInfo {
                fallback_used: static_result.fallback_used,
                origin_hop_distance_m: static_result.origin_hop_distance_m,
                destination_hop_distance_m: static_result.destination_hop_distance_m,
                hop_segments: static_result.hop_segments.clone(),
                diagnostics: static_result.diagnostics.clone(),
                warnings: Vec::new(),
            },
        };
        let mut result = build_temporal_route_result(
            topology,
            metrics,
            request,
            &context,
            candidate,
            edge_names,
            BUCKET_ROUTE_WARNING,
        )?;
        if !static_result.alternatives.is_empty() {
            result.alternatives = static_result.alternatives;
            result.warnings.push(
                "Alternative summaries use the departure-bucket metric without arrival-time integration."
                    .to_string(),
            );
        }
        selection.bucket_boundaries_crossed = self
            .shared
            .historical
            .as_deref()
            .map(|historical| boundaries_between(historical, departure, arrival));
        Ok((result, selection))
    }

    fn route_time_dependent(
        &self,
        request: &RouteRequest,
        snapshot: Option<Arc<LiveTrafficSnapshot>>,
        edge_names: Option<&[String]>,
    ) -> Result<(RouteResult, TrafficSelection)> {
        let started = Instant::now();
        let live_version = snapshot.as_ref().map_or(0, |live| live.version);
        let context =
            context_from_request(&request.temporal)?.with_traffic(self.temporal_traffic(snapshot));
        // Avoid areas and request vehicles change the metric the exact
        // search uses.
        let target = self
            .base
            .temporal_target(&request.temporal, EngineMode::Auto)?;
        let engine = target.as_ref().unwrap_or(&self.base);
        let (graph, _) = engine.routing_graph_for_mode(EngineMode::Auto);
        let mut result = execute_temporal_route_with_graph(
            engine.topology(),
            engine.metrics(),
            graph,
            request,
            &context,
            edge_names,
        )?;
        if let Some(warning) = result
            .warnings
            .iter_mut()
            .find(|warning| warning.as_str() == EXACT_TEMPORAL_ROUTE_WARNING)
        {
            *warning = "Time-dependent route with traffic speeds integrated along every edge used the exact multi-label edge search; static CCH acceleration was intentionally bypassed.".to_string();
        }
        let historical = self.shared.historical.as_deref();
        let departure = result
            .summary
            .departure_time
            .as_deref()
            .and_then(|value| parse_datetime(value).ok());
        let arrival = result
            .summary
            .arrival_time
            .as_deref()
            .and_then(|value| parse_datetime(value).ok());
        let selection = TrafficSelection {
            profile_id: self.profile_id.clone(),
            engine: "time_dependent_exact",
            traffic_id: historical.map(|h| h.traffic_id.clone()),
            departure_time: result.summary.departure_time.clone(),
            bucket: historical
                .zip(departure)
                .map(|(h, departure)| bucket_info(h, h.bucket_at(departure))),
            live_version,
            live_entries_applied: 0,
            engine_cache: "none",
            prepare_ms: elapsed_ms(started),
            bucket_boundaries_crossed: historical
                .zip(departure.zip(arrival))
                .map(|(h, (departure, arrival))| boundaries_between(h, departure, arrival)),
        };
        Ok((result, selection))
    }

    fn bucket_engine_for_batch(
        &self,
        temporal: &TemporalRequestOptions,
        analysis: &str,
    ) -> Result<Option<Prepared>> {
        match self.plan(temporal)? {
            None => Ok(None),
            Some(TrafficPlan::DepartureBucket {
                departure,
                use_live,
            }) => self.prepare(departure, use_live).map(Some),
            Some(TrafficPlan::TimeDependent { .. }) => bail!(
                "traffic-aware {analysis} use the departure-bucket metric and need departure_time without arrive_by, scenarios, overlays or traffic: time_dependent; set traffic: off for the exact temporal engine without traffic"
            ),
        }
    }

    pub fn execute_od(
        &self,
        document: &OdPairsDocument,
    ) -> Result<Option<(OdResult, TrafficSelection)>> {
        let Some(prepared) = self.bucket_engine_for_batch(&document.temporal, "OD batches")? else {
            return Ok(None);
        };
        let mut document = document.clone();
        document.temporal = static_options(&document.temporal);
        let mut result = prepared.engine.execute_od(&document)?;
        result.departure_time = prepared.selection.departure_time.clone();
        Ok(Some((result, prepared.selection)))
    }

    pub fn execute_matrix(
        &self,
        origins: &PointSetDocument,
        destinations: &PointSetDocument,
    ) -> Result<Option<(MatrixResult, TrafficSelection)>> {
        let temporal =
            if origins.temporal.departure_time.is_some() || origins.temporal.arrive_by.is_some() {
                &origins.temporal
            } else {
                &destinations.temporal
            };
        let Some(prepared) = self.bucket_engine_for_batch(temporal, "matrices")? else {
            return Ok(None);
        };
        let (mut origins, mut destinations) = (origins.clone(), destinations.clone());
        origins.temporal = static_options(&origins.temporal);
        destinations.temporal = static_options(&destinations.temporal);
        let mut result = prepared.engine.execute_matrix(&origins, &destinations)?;
        result.departure_time = prepared.selection.departure_time.clone();
        result.warnings.push(BUCKET_BATCH_WARNING.to_string());
        Ok(Some((result, prepared.selection)))
    }

    pub fn execute_service_area(
        &self,
        request: &ServiceAreaRequest,
    ) -> Result<Option<(ServiceAreaResult, TrafficSelection)>> {
        let Some(prepared) = self.bucket_engine_for_batch(&request.temporal, "service areas")?
        else {
            return Ok(None);
        };
        let mut request = request.clone();
        request.temporal = static_options(&request.temporal);
        let mut result = prepared.engine.execute_service_area(&request)?;
        result.departure_time = prepared.selection.departure_time.clone();
        result.warnings.push(BUCKET_BATCH_WARNING.to_string());
        Ok(Some((result, prepared.selection)))
    }
}

/// Static options for a departure-bucket engine: timing is baked into the
/// metric, request-owned avoid areas and vehicles still apply.
fn static_options(temporal: &TemporalRequestOptions) -> TemporalRequestOptions {
    TemporalRequestOptions {
        avoid: temporal.avoid.clone(),
        vehicle: temporal.vehicle,
        ..TemporalRequestOptions::default()
    }
}

fn bucket_info(historical: &HistoricalTraffic, bucket: u32) -> TrafficBucketInfo {
    TrafficBucketInfo {
        index: bucket,
        label: historical.bucket_label(bucket),
        metric_bucket: historical.bucket_class[bucket as usize],
    }
}

fn boundaries_between(
    historical: &HistoricalTraffic,
    departure: OffsetDateTime,
    arrival: OffsetDateTime,
) -> usize {
    let mut count = 0;
    let mut at = departure;
    while count < 100_000 {
        at = historical.next_boundary(at);
        if at > arrival {
            break;
        }
        count += 1;
    }
    count
}

fn format_time(at: OffsetDateTime) -> String {
    at.format(&Rfc3339)
        .expect("OffsetDateTime always formats as RFC 3339")
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// Identifies one per-edge metric: the representative bucket and the live
/// entries baked in. Equal keys produce identical engines.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MetricKey {
    bucket_class: Option<u32>,
    live_version: u64,
    active: Vec<u32>,
}

type EngineSlot = Arc<OnceLock<Result<Arc<PreparedRoutingEngine>, String>>>;

/// Least-recently-used customized engines. Concurrent requests for the same
/// key wait for one customization; evicted engines stay alive while
/// requests still hold them.
struct EngineCache {
    capacity: usize,
    entries: Mutex<VecDeque<(MetricKey, EngineSlot)>>,
}

impl EngineCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Mutex::new(VecDeque::new()),
        }
    }

    fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// The engine for `key` and whether it already existed.
    fn get_or_build(
        &self,
        key: MetricKey,
        build: impl FnOnce() -> Result<PreparedRoutingEngine>,
    ) -> Result<(Arc<PreparedRoutingEngine>, bool)> {
        let (slot, evicted) = {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(position) = entries.iter().position(|(other, _)| *other == key) {
                let entry = entries.remove(position).expect("position is in range");
                let slot = entry.1.clone();
                entries.push_back(entry);
                (slot, None)
            } else {
                let evicted = (entries.len() >= self.capacity)
                    .then(|| entries.pop_front())
                    .flatten();
                let slot = EngineSlot::default();
                entries.push_back((key, slot.clone()));
                (slot, evicted)
            }
        };
        // Free a possibly large engine outside the lock.
        drop(evicted);
        let mut built = false;
        let result = slot.get_or_init(|| {
            built = true;
            build().map(Arc::new).map_err(|error| format!("{error:#}"))
        });
        match result {
            Ok(engine) => Ok((engine.clone(), !built)),
            Err(error) => {
                let error = error.clone();
                self.entries
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|(_, other)| !Arc::ptr_eq(other, &slot));
                bail!(error)
            }
        }
    }
}
