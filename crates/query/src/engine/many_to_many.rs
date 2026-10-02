//! Many-to-many CCH routing for matrices and OD batches.
//!
//! Every unique snapped endpoint pays for one complete upward search. Target
//! search spaces are inverted into per-state buckets, so one linear scan of
//! an origin's forward space yields the best meeting state for every target,
//! with the same tie-breaking as a pairwise merge-join.
//!
//! Summary-only requests never unpack winning paths edge by edge. Each
//! hierarchy arc is summarized once per worker (cost, time, distance,
//! components and a restriction window), following the exact decomposition
//! that explicit unpacking uses. A path is the first edge plus its arc chain,
//! so its summary is a fold over a few dozen arcs. Multi-edge restrictions
//! can only match inside a contiguous run of restriction edges, so each arc
//! keeps its leading and trailing runs (bounded by the longest sequence) and
//! whether a forbidden sequence lies wholly inside it. Only cells whose
//! hierarchy path is illegal fall back to explicit unpacking and the
//! restriction-aware search, exactly as pairwise routing does.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use netweevil_core::{CompiledProfileBundle, TopologyBundle};
use netweevil_profile::ReturnConfig;
use rayon::prelude::*;
use rustc_hash::FxHashMap;

use crate::*;

/// Whether a request can use arc summaries instead of explicit unpacking:
/// it must not ask for anything that needs the per-edge path.
pub(crate) fn summary_only_returns(returns: &ReturnConfig) -> bool {
    matches!(returns.geometry, netweevil_profile::ReturnGeometry::None)
        && !returns.segment_rows
        && returns.road_type_breakdown.is_empty()
        && returns.surface_breakdown.is_empty()
}

/// Restriction-edge runs at both ends of an edge sequence.
#[derive(Debug, Clone, Default, PartialEq)]
struct RunSummary {
    /// Every edge in the sequence is a restriction edge.
    all_relevant: bool,
    /// A forbidden sequence lies wholly inside the sequence.
    violation: bool,
    /// Leading restriction edges, at most `window` of them.
    prefix: Vec<u32>,
    /// Trailing restriction edges, at most `window` of them.
    suffix: Vec<u32>,
}

impl RunSummary {
    fn is_clean(&self) -> bool {
        !self.all_relevant && !self.violation && self.prefix.is_empty() && self.suffix.is_empty()
    }
}

struct RestrictionWindows<'a> {
    automaton: &'a RestrictionAutomaton,
    relevant: Vec<u64>,
    /// A forbidden sequence of length `k` crossing a junction uses at most
    /// `k - 1` edges from either side.
    window: usize,
}

impl<'a> RestrictionWindows<'a> {
    fn new(routing_graph: &'a RoutingGraph) -> Option<Self> {
        if !routing_graph.has_restriction_sequences() {
            return None;
        }
        let automaton = &routing_graph.automaton;
        let mut relevant = vec![0_u64; routing_graph.edge_costs.len().div_ceil(64)];
        for edge in automaton.sequence_edges() {
            if let Some(word) = relevant.get_mut(edge / 64) {
                *word |= 1 << (edge % 64);
            }
        }
        Some(Self {
            automaton,
            relevant,
            window: automaton.max_sequence_len().saturating_sub(1).max(1),
        })
    }

    fn is_relevant(&self, edge: u32) -> bool {
        let edge = edge as usize;
        self.relevant
            .get(edge / 64)
            .is_some_and(|word| word & (1 << (edge % 64)) != 0)
    }

    fn single(&self, edge: u32) -> RunSummary {
        if self.is_relevant(edge) {
            RunSummary {
                all_relevant: true,
                violation: false,
                prefix: vec![edge],
                suffix: vec![edge],
            }
        } else {
            RunSummary::default()
        }
    }

    fn violates(&self, edges: impl Iterator<Item = u32>) -> bool {
        let mut state = 0_usize;
        for edge in edges {
            let edge = edge as usize;
            if self
                .automaton
                .prohibited_sequence_len(state, edge)
                .is_some()
            {
                return true;
            }
            state = self.automaton.transition(state, edge);
        }
        false
    }

    /// Summary of the concatenation `left ++ right`.
    fn combine(&self, left: &RunSummary, right: &RunSummary) -> RunSummary {
        if left.is_clean() && right.is_clean() {
            return RunSummary::default();
        }
        let violation = left.violation
            || right.violation
            || (!left.suffix.is_empty()
                && !right.prefix.is_empty()
                && self.violates(left.suffix.iter().chain(&right.prefix).copied()));
        let prefix = if left.all_relevant {
            left.prefix
                .iter()
                .chain(&right.prefix)
                .take(self.window)
                .copied()
                .collect()
        } else {
            left.prefix.clone()
        };
        let suffix = if right.all_relevant {
            let joined = left.suffix.iter().chain(&right.suffix).copied();
            let total = left.suffix.len() + right.suffix.len();
            joined.skip(total.saturating_sub(self.window)).collect()
        } else {
            right.suffix.clone()
        };
        RunSummary {
            all_relevant: left.all_relevant && right.all_relevant,
            violation,
            prefix,
            suffix,
        }
    }
}

/// Additive summary of the edges an arc unpacks to, excluding its tail.
#[derive(Debug, Clone)]
struct ArcSummary {
    cost: f64,
    time_s: f64,
    distance_m: u64,
    edges: u32,
    /// Restriction runs; `None` when the arc touches no restriction edge.
    run: Option<Arc<RunSummary>>,
    /// Component totals aligned with the profile's components.
    components: Option<Arc<[f64]>>,
}

struct SummaryContext<'a> {
    topology: &'a TopologyBundle,
    metrics: &'a CompiledProfileBundle,
    acceleration: &'a AccelerationGraph,
    restrictions: Option<RestrictionWindows<'a>>,
    component_multipliers: Vec<f64>,
}

impl<'a> SummaryContext<'a> {
    fn new(
        topology: &'a TopologyBundle,
        metrics: &'a CompiledProfileBundle,
        routing_graph: &'a RoutingGraph,
    ) -> Option<Self> {
        Some(Self {
            topology,
            metrics,
            acceleration: routing_graph.acceleration.as_ref()?,
            restrictions: RestrictionWindows::new(routing_graph),
            component_multipliers: metrics
                .components
                .iter()
                .map(|component| {
                    if component.overlay_name.is_some() && !component.invert_overlay {
                        0.0
                    } else {
                        1.0
                    }
                })
                .collect(),
        })
    }

    fn edge_component(&self, component_index: usize, edge: usize, factor: f64) -> f64 {
        self.metrics.components[component_index]
            .edge_values
            .get(edge)
            .map_or(0.0, |value| {
                f64::from(*value) * factor * self.component_multipliers[component_index]
            })
    }
}

const SUMMARY_SHARDS: usize = 512;

/// Arc summaries shared by all workers of one request. Arcs near the top of
/// the hierarchy recur in most paths and their decompositions overlap, so
/// each arc is summarized about once per request.
struct SharedArcSummaries {
    shards: Vec<Mutex<FxHashMap<u64, ArcSummary>>>,
}

impl SharedArcSummaries {
    fn new() -> Self {
        Self {
            shards: (0..SUMMARY_SHARDS)
                .map(|_| Mutex::new(FxHashMap::default()))
                .collect(),
        }
    }

    fn shard(&self, key: u64) -> &Mutex<FxHashMap<u64, ArcSummary>> {
        let mixed = (key ^ (key >> 29)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        &self.shards[(mixed >> 55) as usize % SUMMARY_SHARDS]
    }
}

/// One worker's view of the shared summaries, with a private front cache so
/// hot arcs are looked up without locking.
struct ArcSummaryCache<'a> {
    shared: &'a SharedArcSummaries,
    local: FxHashMap<u64, ArcSummary>,
    stack: Vec<(bool, u32, Option<(u32, u32)>)>,
}

impl<'a> ArcSummaryCache<'a> {
    fn new(shared: &'a SharedArcSummaries) -> Self {
        Self {
            shared,
            local: FxHashMap::default(),
            stack: Vec::new(),
        }
    }

    fn key(is_upward: bool, slot: u32) -> u64 {
        (u64::from(slot) << 1) | u64::from(is_upward)
    }

    fn lookup(&mut self, key: u64) -> Option<&ArcSummary> {
        if !self.local.contains_key(&key) {
            let found = self
                .shared
                .shard(key)
                .lock()
                .expect("arc summary shard poisoned")
                .get(&key)
                .cloned()?;
            self.local.insert(key, found);
        }
        self.local.get(&key)
    }

    fn summary(&mut self, context: &SummaryContext<'_>, is_upward: bool, slot: u32) -> &ArcSummary {
        let root = Self::key(is_upward, slot);
        if self.lookup(root).is_none() {
            self.compute(context, is_upward, slot);
        }
        &self.local[&root]
    }

    fn compute(&mut self, context: &SummaryContext<'_>, is_upward: bool, slot: u32) {
        let component_count = context.metrics.components.len();
        debug_assert!(self.stack.is_empty());
        self.stack.push((
            is_upward,
            slot,
            unpack_witness(context.acceleration, is_upward, slot),
        ));
        while let Some(&(is_upward, slot, witness)) = self.stack.last() {
            let key = Self::key(is_upward, slot);
            if self.lookup(key).is_some() {
                self.stack.pop();
                continue;
            }
            let summary = match witness {
                None => {
                    let tail = arc_tail(context.acceleration, is_upward, slot) as usize;
                    let head = arc_head(context.acceleration, is_upward, slot) as usize;
                    let turn_s =
                        turn_penalty_seconds(context.topology, context.metrics, tail, head);
                    let metric = &context.metrics.edge_metrics[head];
                    let run = context
                        .restrictions
                        .as_ref()
                        .map(|restrictions| restrictions.single(head as u32))
                        .filter(|run| !run.is_clean())
                        .map(Arc::new);
                    let components = (component_count > 0).then(|| {
                        (0..component_count)
                            .map(|index| context.edge_component(index, head, 1.0))
                            .collect::<Arc<[f64]>>()
                    });
                    ArcSummary {
                        cost: metric.generalized_cost.unwrap_or_default()
                            + turn_s * context.metrics.turn_costs.cost_time_weight,
                        time_s: metric.travel_time_s.unwrap_or_default() + turn_s,
                        distance_m: u64::from(context.topology.routing_edge(head).length_m),
                        edges: 1,
                        run,
                        components,
                    }
                }
                Some((left, right)) => {
                    let left_key = Self::key(false, left);
                    let right_key = Self::key(true, right);
                    let left_known = self.lookup(left_key).is_some();
                    let right_known = self.lookup(right_key).is_some();
                    if !left_known || !right_known {
                        if !left_known {
                            self.stack.push((
                                false,
                                left,
                                unpack_witness(context.acceleration, false, left),
                            ));
                        }
                        if !right_known {
                            self.stack.push((
                                true,
                                right,
                                unpack_witness(context.acceleration, true, right),
                            ));
                        }
                        continue;
                    }
                    let left = &self.local[&left_key];
                    let right = &self.local[&right_key];
                    let run = match (&context.restrictions, &left.run, &right.run) {
                        (Some(restrictions), left_run, right_run)
                            if left_run.is_some() || right_run.is_some() =>
                        {
                            let clean = RunSummary::default();
                            let run = restrictions.combine(
                                left_run.as_deref().unwrap_or(&clean),
                                right_run.as_deref().unwrap_or(&clean),
                            );
                            (!run.is_clean()).then(|| Arc::new(run))
                        }
                        _ => None,
                    };
                    let components = match (&left.components, &right.components) {
                        (Some(left), Some(right)) => Some(
                            left.iter()
                                .zip(right.iter())
                                .map(|(left, right)| left + right)
                                .collect::<Arc<[f64]>>(),
                        ),
                        _ => None,
                    };
                    ArcSummary {
                        cost: left.cost + right.cost,
                        time_s: left.time_s + right.time_s,
                        distance_m: left.distance_m + right.distance_m,
                        edges: left.edges + right.edges,
                        run,
                        components,
                    }
                }
            };
            self.shared
                .shard(key)
                .lock()
                .expect("arc summary shard poisoned")
                .insert(key, summary.clone());
            self.local.insert(key, summary);
            self.stack.pop();
        }
    }
}

/// Summary of a hierarchy path through a meeting state, or `None` when the
/// path is a single edge or violates a multi-edge restriction; callers then
/// use explicit unpacking.
struct PathSummary {
    cost: f64,
    time_s: f64,
    distance_m: u64,
    edges: usize,
    components: Vec<f64>,
}

fn summarize_space_path(
    context: &SummaryContext<'_>,
    cache: &mut ArcSummaryCache<'_>,
    arcs: &mut Vec<(bool, u32)>,
    forward: &CchSearchSpace,
    backward: &CchSearchSpace,
    meeting_edge: u32,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> Option<PathSummary> {
    let acceleration = context.acceleration;
    arcs.clear();
    let mut position = forward.position(meeting_edge)?;
    loop {
        let entry = forward.entries[position];
        if entry.arc == NO_PREVIOUS_ARC {
            break;
        }
        arcs.push((true, entry.arc));
        position = entry.parent as usize;
    }
    let first_edge = forward.entries[position].edge_index;
    arcs.reverse();
    let mut position = backward.position(meeting_edge)?;
    loop {
        let entry = backward.entries[position];
        if entry.arc == NO_PREVIOUS_ARC {
            break;
        }
        arcs.push((false, entry.arc));
        position = entry.parent as usize;
    }
    let last_edge = backward.entries[position].edge_index;
    if arcs.is_empty() {
        return None;
    }
    debug_assert_eq!(
        arc_head(acceleration, arcs[arcs.len() - 1].0, arcs[arcs.len() - 1].1),
        last_edge
    );

    let metrics = context.metrics;
    let first = first_edge as usize;
    let last = last_edge as usize;
    let first_factor = edge_traversal_factor(first, true, false, origin, destination);
    let last_factor = edge_traversal_factor(last, false, true, origin, destination);
    let first_metric = &metrics.edge_metrics[first];
    let last_metric = &metrics.edge_metrics[last];
    let first_length = f64::from(context.topology.routing_edge(first).length_m);
    let last_length = context.topology.routing_edge(last).length_m;

    let mut cost = first_metric.generalized_cost.unwrap_or_default() * first_factor;
    let mut time_s = first_metric.travel_time_s.unwrap_or_default() * first_factor;
    let mut distance_m = (first_length * first_factor).round() as u64;
    let mut edges = 1_usize;
    let component_count = metrics.components.len();
    let mut components = (0..component_count)
        .map(|index| context.edge_component(index, first, first_factor))
        .collect::<Vec<_>>();
    let mut run = context
        .restrictions
        .as_ref()
        .map(|restrictions| restrictions.single(first_edge))
        .unwrap_or_default();
    let clean = RunSummary::default();
    for &(is_upward, slot) in arcs.iter() {
        let summary = cache.summary(context, is_upward, slot);
        cost += summary.cost;
        time_s += summary.time_s;
        distance_m += summary.distance_m;
        edges += summary.edges as usize;
        if let Some(values) = &summary.components {
            for (total, value) in components.iter_mut().zip(values.iter()) {
                *total += value;
            }
        }
        if let Some(restrictions) = &context.restrictions
            && (summary.run.is_some() || !run.is_clean())
        {
            run = restrictions.combine(&run, summary.run.as_deref().unwrap_or(&clean));
        }
    }
    if run.violation {
        return None;
    }
    // Arc summaries count the final edge in full; keep only the part up to
    // the destination, rounding its distance as route analysis does.
    if last_factor != 1.0 {
        cost += last_metric.generalized_cost.unwrap_or_default() * (last_factor - 1.0);
        time_s += last_metric.travel_time_s.unwrap_or_default() * (last_factor - 1.0);
        distance_m = distance_m - u64::from(last_length)
            + (f64::from(last_length) * last_factor).round() as u64;
        for (index, total) in components.iter_mut().enumerate() {
            *total += context.edge_component(index, last, last_factor)
                - context.edge_component(index, last, 1.0);
        }
    }
    Some(PathSummary {
        cost,
        time_s,
        distance_m,
        edges,
        components,
    })
}

fn route_result_from_summary(
    metrics: &CompiledProfileBundle,
    route_id: &str,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
    hop_info: HopSelectionInfo,
    summary: PathSummary,
) -> RouteResult {
    RouteResult {
        route_id: route_id.to_string(),
        origin: origin.clone(),
        destination: destination.clone(),
        outcome: if hop_info.fallback_used {
            AnalysisOutcome::Degraded
        } else {
            AnalysisOutcome::Legal
        },
        fallback_used: hop_info.fallback_used,
        origin_hop_distance_m: hop_info.origin_hop_distance_m,
        destination_hop_distance_m: hop_info.destination_hop_distance_m,
        summary: RouteSummary {
            network_distance_m: summary.distance_m,
            network_travel_time_s: summary.time_s,
            network_generalized_cost: summary.cost,
            components: metrics
                .components
                .iter()
                .zip(&summary.components)
                .map(|(component, value)| (component.name.clone(), *value))
                .collect(),
            waiting_time_s: 0.0,
            departure_time: None,
            arrival_time: None,
            scenario_id: None,
            illegal_movement_penalty_s: 0.0,
            illegal_movement_penalty_cost: 0.0,
            violation_count: 0,
            violation_types: Vec::new(),
            total_distance_m: summary.distance_m,
            total_travel_time_s: summary.time_s,
            total_generalized_cost: summary.cost,
            segment_count: summary.edges,
        },
        node_path: Vec::new(),
        edge_path: Vec::new(),
        geometry: None,
        hop_segments: hop_info.hop_segments,
        segments: None,
        breakdowns: None,
        violations: Vec::new(),
        diagnostics: hop_info.diagnostics,
        warnings: hop_info.warnings,
        alternatives: Vec::new(),
    }
}

/// Target search spaces inverted by hierarchy state: `bucket(v)` lists
/// `(backward cost at v, target space)` in ascending target order.
struct BackwardBuckets {
    /// Dense map from state to bucket id plus one; zero means no bucket.
    state_bucket: Vec<u32>,
    first: Vec<u32>,
    entries: Vec<(f64, u32)>,
}

impl BackwardBuckets {
    fn build(edge_count: usize, spaces: &[(usize, &CchSearchSpace)]) -> Self {
        let mut state_bucket = vec![0_u32; edge_count];
        let mut counts = Vec::<u32>::new();
        for (_, space) in spaces {
            for entry in &space.entries {
                let bucket = &mut state_bucket[entry.edge_index as usize];
                if *bucket == 0 {
                    counts.push(0);
                    *bucket = counts.len() as u32;
                }
                counts[*bucket as usize - 1] += 1;
            }
        }
        let mut first = Vec::with_capacity(counts.len() + 1);
        first.push(0_u32);
        for count in &counts {
            first.push(first[first.len() - 1] + count);
        }
        let mut write = first[..counts.len()].to_vec();
        let mut entries = vec![(0.0, 0_u32); first[counts.len()] as usize];
        for &(target, space) in spaces {
            for entry in &space.entries {
                let bucket = state_bucket[entry.edge_index as usize] as usize - 1;
                entries[write[bucket] as usize] = (entry.cost, target as u32);
                write[bucket] += 1;
            }
        }
        Self {
            state_bucket,
            first,
            entries,
        }
    }

    /// Best `(cost, meeting state)` per target for one forward space. States
    /// are visited in ascending order with a strict improvement test, so ties
    /// resolve exactly as in `join_cch_spaces`.
    fn scan(&self, forward: &CchSearchSpace, best: &mut [(f64, u32)]) {
        best.fill((f64::INFINITY, NO_PREVIOUS_ARC));
        for entry in &forward.entries {
            let bucket = self.state_bucket[entry.edge_index as usize];
            if bucket == 0 {
                continue;
            }
            let bucket = bucket as usize - 1;
            for &(backward_cost, target) in
                &self.entries[self.first[bucket] as usize..self.first[bucket + 1] as usize]
            {
                let cost = entry.cost + backward_cost;
                let slot = &mut best[target as usize];
                if cost < slot.0 {
                    *slot = (cost, entry.edge_index);
                }
            }
        }
    }
}

pub(crate) struct ManyToManyRequest<'a> {
    pub(crate) topology: &'a TopologyBundle,
    pub(crate) metrics: &'a CompiledProfileBundle,
    pub(crate) routing_graph: &'a RoutingGraph,
    pub(crate) route_id: &'a str,
    pub(crate) snap_max_distance_m: f64,
    pub(crate) connectivity: &'a ConnectivityPolicy,
    pub(crate) fallback: &'a FallbackPolicy,
    pub(crate) returns: &'a ReturnConfig,
    pub(crate) origin_sets: &'a [Vec<SnappedPoint>],
    pub(crate) destination_sets: &'a [Vec<SnappedPoint>],
}

type CellResult = std::result::Result<RouteResult, AnalysisFailure>;

/// Lazily built search spaces for unique snapped candidates. Primary
/// candidates (the first physical attachment of each set) are prebuilt in
/// parallel; others are built on first use.
struct SpaceTable {
    seeds: Vec<Vec<(usize, f64)>>,
    spaces: Vec<OnceLock<CchSearchSpace>>,
    forward: bool,
}

impl SpaceTable {
    fn new(
        routing_graph: &RoutingGraph,
        sets: &[Vec<SnappedPoint>],
        used_sets: impl Iterator<Item = usize>,
        forward: bool,
    ) -> (Self, Vec<Vec<usize>>, Vec<usize>) {
        let mut keys = HashMap::new();
        let mut seeds = Vec::new();
        let mut set_ids = vec![Vec::new(); sets.len()];
        let mut primary = Vec::new();
        let mut used = vec![false; sets.len()];
        for set_index in used_sets {
            if std::mem::replace(&mut used[set_index], true) {
                continue;
            }
            let set = &sets[set_index];
            for (candidate_index, candidate) in set.iter().enumerate() {
                let id = *keys.entry(snap_cache_key(candidate)).or_insert_with(|| {
                    seeds.push(if forward {
                        origin_edge_seeds(routing_graph, candidate)
                    } else {
                        destination_edge_seeds(routing_graph, candidate)
                    });
                    seeds.len() - 1
                });
                set_ids[set_index].push(id);
                if candidate_index == 0 || same_physical_attachment(candidate, &set[0]) {
                    primary.push(id);
                }
            }
        }
        primary.sort_unstable();
        primary.dedup();
        let spaces = (0..seeds.len()).map(|_| OnceLock::new()).collect();
        (
            Self {
                seeds,
                spaces,
                forward,
            },
            set_ids,
            primary,
        )
    }

    fn get(&self, routing_graph: &RoutingGraph, id: usize) -> &CchSearchSpace {
        self.spaces[id].get_or_init(|| {
            if self.forward {
                build_forward_cch_space(routing_graph, &self.seeds[id])
            } else {
                build_backward_cch_space(routing_graph, &self.seeds[id])
            }
        })
    }

    fn prebuild(&self, routing_graph: &RoutingGraph, ids: &[usize]) {
        ids.par_iter().for_each(|&id| {
            self.get(routing_graph, id);
        });
    }
}

/// Routes every `(origin set, destination set)` pair of `rows` and returns
/// results aligned with each row's destination list.
pub(crate) fn many_to_many_routes(
    request: &ManyToManyRequest<'_>,
    rows: &[(usize, Vec<usize>)],
) -> Vec<Vec<CellResult>> {
    let routing_graph = request.routing_graph;
    let (forward_table, origin_ids, forward_primary) = SpaceTable::new(
        routing_graph,
        request.origin_sets,
        rows.iter().map(|(origin, _)| *origin),
        true,
    );
    let (backward_table, destination_ids, backward_primary) = SpaceTable::new(
        routing_graph,
        request.destination_sets,
        rows.iter()
            .flat_map(|(_, destinations)| destinations.iter().copied()),
        false,
    );
    forward_table.prebuild(routing_graph, &forward_primary);
    backward_table.prebuild(routing_graph, &backward_primary);

    // Bucket scans cost one pass over a forward space per origin candidate;
    // they pay off when a row needs a sizeable share of the targets.
    let bucket_ids = backward_primary;
    let mut bucket_slot = vec![u32::MAX; backward_table.seeds.len()];
    for (slot, &id) in bucket_ids.iter().enumerate() {
        bucket_slot[id] = slot as u32;
    }
    let row_uses_buckets = rows
        .iter()
        .map(|(_, destinations)| destinations.len() * 4 >= bucket_ids.len())
        .collect::<Vec<_>>();
    let buckets = row_uses_buckets.iter().any(|&uses| uses).then(|| {
        let spaces = bucket_ids
            .iter()
            .enumerate()
            .map(|(slot, &id)| (slot, backward_table.get(routing_graph, id)))
            .collect::<Vec<_>>();
        BackwardBuckets::build(routing_graph.edge_costs.len(), &spaces)
    });

    let summaries = summary_only_returns(request.returns)
        .then(|| SummaryContext::new(request.topology, request.metrics, routing_graph))
        .flatten();
    let shared_summaries = SharedArcSummaries::new();
    let shared = CellShared {
        request,
        forward_table: &forward_table,
        backward_table: &backward_table,
        origin_ids: &origin_ids,
        destination_ids: &destination_ids,
        buckets: buckets.as_ref(),
        bucket_slot: &bucket_slot,
        bucket_count: bucket_ids.len(),
        scans: (0..forward_table.seeds.len())
            .map(|_| OnceLock::new())
            .collect(),
        forward_reach: (0..forward_table.seeds.len())
            .map(|_| OnceLock::new())
            .collect(),
        summaries: summaries.as_ref(),
    };

    // Cells, not rows, are the unit of parallel work: restriction fallbacks
    // cluster on a few origins and would otherwise serialize on one worker.
    // Row-major chunks keep each chunk on few origins, so per-chunk caches
    // and the resumable restricted frontier stay effective.
    let cells = rows
        .iter()
        .enumerate()
        .flat_map(|(row, (_, destinations))| (0..destinations.len()).map(move |col| (row, col)))
        .collect::<Vec<_>>();
    let route = |worker: &mut CellWorker<'_>, (row, col): (usize, usize)| {
        let (origin_set, destinations) = &rows[row];
        shared.route_cell(
            worker,
            row_uses_buckets[row],
            *origin_set,
            destinations[col],
        )
    };
    let into_failure = |error: anyhow::Error| {
        analysis_failure(&error).cloned().unwrap_or_else(|| {
            AnalysisFailure::new(error.to_string(), AnalysisOutcome::Unreachable, Vec::new())
        })
    };
    // Pass 1 answers every cell whose hierarchy path is legal, in large
    // row-major chunks so worker caches stay warm. Cells needing the
    // restriction-aware search are deferred: they cluster on a few origins
    // and would otherwise serialize one chunk.
    let threads = rayon::current_num_threads().max(1);
    let chunk = cells.len().div_ceil(threads * 2).max(1);
    let mut results = cells
        .par_chunks(chunk)
        .flat_map_iter(|chunk| {
            let mut worker = CellWorker::new(&shared_summaries, true);
            chunk
                .iter()
                .map(|&cell| match route(&mut worker, cell) {
                    Err(error) if error.is::<RestrictedCellDeferred>() => None,
                    result => Some(result.map_err(into_failure)),
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    // Pass 2 resolves deferred cells one by one across all workers.
    let deferred = results
        .iter()
        .enumerate()
        .filter_map(|(index, result)| result.is_none().then_some(index))
        .collect::<Vec<_>>();
    let resolved = deferred
        .par_iter()
        .map_init(
            || CellWorker::new(&shared_summaries, false),
            |worker, &index| route(worker, cells[index]).map_err(into_failure),
        )
        .collect::<Vec<_>>();
    for (index, result) in deferred.into_iter().zip(resolved) {
        results[index] = Some(result);
    }
    let mut results = results
        .into_iter()
        .map(|result| result.expect("every cell resolved"));
    rows.iter()
        .map(|(_, destinations)| results.by_ref().take(destinations.len()).collect())
        .collect()
}

/// Edge states reachable from `origin`, sorted, when there are at most
/// `TRAPPED_REACH_LIMIT` of them. Restrictions only remove paths, so an edge
/// outside this set is unreachable under every restriction policy.
fn trapped_reach_set(routing_graph: &RoutingGraph, origin: &SnappedPoint) -> Option<Vec<u32>> {
    const TRAPPED_REACH_LIMIT: usize = 1024;
    let mut reached = origin_edge_seeds(routing_graph, origin)
        .into_iter()
        .map(|(edge, _)| edge as u32)
        .collect::<Vec<_>>();
    reached.sort_unstable();
    reached.dedup();
    let mut seen = reached
        .iter()
        .copied()
        .collect::<rustc_hash::FxHashSet<_>>();
    let mut cursor = 0;
    while cursor < reached.len() {
        let edge = reached[cursor] as usize;
        cursor += 1;
        for transition in routing_graph.transition_range(edge) {
            let next = routing_graph.transition_edges[transition];
            if seen.insert(next) {
                if seen.len() > TRAPPED_REACH_LIMIT {
                    return None;
                }
                reached.push(next);
            }
        }
    }
    reached.sort_unstable();
    Some(reached)
}

/// Marks a cell whose hierarchy path is illegal during the first pass.
#[derive(Debug)]
struct RestrictedCellDeferred;

impl std::fmt::Display for RestrictedCellDeferred {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("restricted cell deferred")
    }
}

impl std::error::Error for RestrictedCellDeferred {}

struct CellShared<'a, 'b> {
    request: &'a ManyToManyRequest<'b>,
    forward_table: &'a SpaceTable,
    backward_table: &'a SpaceTable,
    origin_ids: &'a [Vec<usize>],
    destination_ids: &'a [Vec<usize>],
    buckets: Option<&'a BackwardBuckets>,
    bucket_slot: &'a [u32],
    bucket_count: usize,
    /// Bucket-scan results per forward space, computed on first use.
    scans: Vec<OnceLock<Vec<(f64, u32)>>>,
    /// Complete reach set per forward space when the origin is trapped in a
    /// small part of the graph; `None` when the reach is large.
    forward_reach: Vec<OnceLock<Option<Vec<u32>>>>,
    summaries: Option<&'a SummaryContext<'a>>,
}

struct CellWorker<'a> {
    cache: ArcSummaryCache<'a>,
    arcs: Vec<(bool, u32)>,
    /// Defer cells that need the restriction-aware search instead of
    /// computing them inline.
    defer_restricted: bool,
    /// The current origin's restriction-aware Dijkstra frontier, used only
    /// when no lower-bound hierarchy is available.
    restricted: Option<((u32, u64, u64), RestrictedSearchScratch)>,
}

impl<'a> CellWorker<'a> {
    fn new(shared: &'a SharedArcSummaries, defer_restricted: bool) -> Self {
        Self {
            cache: ArcSummaryCache::new(shared),
            arcs: Vec::new(),
            defer_restricted,
            restricted: None,
        }
    }
}

impl CellShared<'_, '_> {
    fn joined(
        &self,
        use_buckets: bool,
        forward_id: usize,
        backward_id: usize,
    ) -> Option<(f64, u32)> {
        let routing_graph = self.request.routing_graph;
        let slot = self.bucket_slot[backward_id];
        if use_buckets && slot != u32::MAX {
            let buckets = self.buckets.expect("bucket scan requested");
            let scan = self.scans[forward_id].get_or_init(|| {
                let mut best = vec![(f64::INFINITY, NO_PREVIOUS_ARC); self.bucket_count];
                buckets.scan(self.forward_table.get(routing_graph, forward_id), &mut best);
                best
            });
            let (cost, meeting) = scan[slot as usize];
            return (meeting != NO_PREVIOUS_ARC).then_some((cost, meeting));
        }
        join_cch_spaces(
            self.forward_table.get(routing_graph, forward_id),
            self.backward_table.get(routing_graph, backward_id),
        )
    }

    fn route_cell(
        &self,
        worker: &mut CellWorker<'_>,
        use_buckets: bool,
        origin_set: usize,
        destination_set: usize,
    ) -> anyhow::Result<RouteResult> {
        let request = self.request;
        let routing_graph = request.routing_graph;
        let origin_candidates = &request.origin_sets[origin_set];
        let destination_candidates = &request.destination_sets[destination_set];
        let mut attachment: Option<(usize, usize)> = None;
        let mut best: Option<RouteResult> = None;
        for (origin_index, origin) in origin_candidates.iter().enumerate() {
            if let Some((first_origin, _)) = attachment
                && !same_physical_attachment(origin, &origin_candidates[first_origin])
            {
                continue;
            }
            let forward_id = self.origin_ids[origin_set][origin_index];
            for (destination_index, destination) in destination_candidates.iter().enumerate() {
                if let Some((_, first_destination)) = attachment
                    && !same_physical_attachment(
                        destination,
                        &destination_candidates[first_destination],
                    )
                {
                    continue;
                }
                if same_edge_reverse_pair(origin, destination) {
                    continue;
                }
                let Some(hop_info) = hop_info_for_pair(
                    origin,
                    destination,
                    request.snap_max_distance_m,
                    request.connectivity,
                ) else {
                    continue;
                };
                let backward_id = self.destination_ids[destination_set][destination_index];
                let direct = direct_same_edge_path(routing_graph, origin, destination);
                // Weak components and a trapped origin's exhaustive reach set
                // prove unreachability without building the target's space.
                let reachable = !origin
                    .component_id
                    .zip(destination.component_id)
                    .is_some_and(|(origin_component, destination_component)| {
                        origin_component != destination_component
                    })
                    && self.forward_reach[forward_id]
                        .get_or_init(|| trapped_reach_set(routing_graph, origin))
                        .as_ref()
                        .is_none_or(|reach| {
                            destination_edge_seeds(routing_graph, destination)
                                .iter()
                                .any(|(edge, _)| reach.binary_search(&(*edge as u32)).is_ok())
                        });
                let joined = if reachable {
                    self.joined(use_buckets, forward_id, backward_id)
                } else {
                    None
                };
                let choice = match (direct, joined) {
                    (Some(direct), Some((cost, meeting)))
                        if cost < direct.total_generalized_cost =>
                    {
                        Some(Err((cost, meeting)))
                    }
                    (Some(direct), _) => Some(Ok(direct)),
                    (None, Some(joined)) => Some(Err(joined)),
                    (None, None) => None,
                };
                let result = match choice {
                    None => continue,
                    Some(Ok(direct)) => {
                        self.explicit_result(worker, origin, destination, hop_info, Some(direct))?
                    }
                    Some(Err((cost, meeting))) => {
                        let forward = self.forward_table.get(routing_graph, forward_id);
                        let backward = self.backward_table.get(routing_graph, backward_id);
                        let summary = self.summaries.and_then(|context| {
                            summarize_space_path(
                                context,
                                &mut worker.cache,
                                &mut worker.arcs,
                                forward,
                                backward,
                                meeting,
                                origin,
                                destination,
                            )
                        });
                        match summary {
                            Some(summary) => Some(route_result_from_summary(
                                request.metrics,
                                request.route_id,
                                origin,
                                destination,
                                hop_info,
                                summary,
                            )),
                            None => {
                                let path = reconstruct_cch_space_path(
                                    routing_graph,
                                    forward,
                                    backward,
                                    meeting,
                                    cost,
                                );
                                self.explicit_result(
                                    worker,
                                    origin,
                                    destination,
                                    hop_info,
                                    Some(path),
                                )?
                            }
                        }
                    }
                };
                let Some(result) = result else {
                    continue;
                };
                attachment.get_or_insert((origin_index, destination_index));
                if best.as_ref().is_none_or(|previous| {
                    result.summary.total_generalized_cost < previous.summary.total_generalized_cost
                }) {
                    best = Some(result);
                }
            }
        }

        if let Some(result) = best {
            return Ok(result);
        }
        let failure = no_route_failure(request.topology, origin_candidates, destination_candidates);
        if matches!(
            request.connectivity.disconnected,
            DisconnectedNetworkMode::IgnoreUnreachable
        ) && matches!(failure.outcome, AnalysisOutcome::Unreachable)
        {
            return Ok(ignored_unreachable_route_result(
                request.route_id,
                origin_candidates,
                destination_candidates,
                failure,
                Vec::new(),
            ));
        }
        Err(failure.into())
    }

    /// Exact restriction-aware path for a cell whose hierarchy candidate is
    /// illegal: A* with lower-bound hierarchy potentials, or the origin's
    /// resumable Dijkstra frontier when no lower-bound hierarchy exists.
    fn restricted_path(
        &self,
        worker: &mut CellWorker<'_>,
        origin: &SnappedPoint,
        destination: &SnappedPoint,
    ) -> anyhow::Result<Option<RoutePath>> {
        let request = self.request;
        let routing_graph = request.routing_graph;
        let origin_seeds = origin_edge_seeds(routing_graph, origin);
        let destination_seeds = destination_edge_seeds(routing_graph, destination);
        let direct = direct_same_edge_path(routing_graph, origin, destination);
        if let Some(weights) = lower_bound_weights(routing_graph) {
            return restricted_astar_route(
                request.topology,
                request.metrics,
                routing_graph,
                weights,
                &origin_seeds,
                &destination_seeds,
                direct,
                request.fallback,
            );
        }
        let key = snap_cache_key(origin);
        if worker
            .restricted
            .as_ref()
            .is_none_or(|(known, _)| *known != key)
        {
            let mut search = RestrictedSearchScratch::default();
            search.prepare(
                request.metrics,
                routing_graph,
                &origin_seeds,
                request.fallback,
            );
            worker.restricted = Some((key, search));
        }
        let (_, restricted) = worker
            .restricted
            .as_mut()
            .expect("restricted search prepared");
        restricted.route_to(
            request.topology,
            request.metrics,
            routing_graph,
            &destination_seeds,
            direct,
            request.fallback,
        )
    }

    /// Validates an explicit candidate path, replacing an illegal one with
    /// the exact restriction-aware path, and analyzes the result.
    fn explicit_result(
        &self,
        worker: &mut CellWorker<'_>,
        origin: &SnappedPoint,
        destination: &SnappedPoint,
        hop_info: HopSelectionInfo,
        path: Option<RoutePath>,
    ) -> anyhow::Result<Option<RouteResult>> {
        let request = self.request;
        let routing_graph = request.routing_graph;
        let path = if path.as_ref().is_some_and(|path| {
            !path_respects_restriction_sequences(routing_graph, &path.edge_indexes)
        }) {
            if worker.defer_restricted {
                return Err(RestrictedCellDeferred.into());
            }
            self.restricted_path(worker, origin, destination)?
        } else {
            path
        };
        path.map(|path| {
            batch_route_result_for_path(
                request.topology,
                request.metrics,
                routing_graph,
                request.route_id,
                request.fallback,
                request.returns,
                origin,
                destination,
                hop_info,
                path,
            )
        })
        .transpose()
    }
}
