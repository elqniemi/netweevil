use std::sync::Arc;

use anyhow::{Context, Result, bail};
use netweevil_core::{CompiledProfileBundle, DatasetAccelerationBundle, TopologyBundle};

use crate::*;

#[derive(Clone)]
pub(crate) struct RoutePath {
    pub(crate) edge_indexes: Vec<usize>,
    pub(crate) total_generalized_cost: f64,
}

/// Search graph of one profile. The adjacency arrays, hierarchy, automata
/// and lazily built tables are reference counted, so graphs that differ only
/// in their restriction automata
/// ([`RoutingGraph::without_restriction_sequences`]) or only in edge costs
/// (traffic bucket engines, [`RoutingGraph::with_metrics`]) share them.
#[derive(Clone)]
pub(crate) struct RoutingGraph {
    pub(crate) edge_spatial_index: Arc<crate::edge_spatial_index::EdgeSpatialIndex>,
    first_out: Arc<[u32]>,
    edge_order: Arc<[u32]>,
    incoming_first_out: Arc<[u32]>,
    incoming_edge_order: Arc<[u32]>,
    pub(crate) edge_costs: Arc<[f64]>,
    transition_first_out: Arc<[u32]>,
    pub(crate) transition_edges: Arc<[u32]>,
    pub(crate) transition_costs: Arc<[f64]>,
    reverse_transition_first_out: Arc<[u32]>,
    pub(crate) reverse_transition_edges: Arc<[u32]>,
    pub(crate) reverse_transition_costs: Arc<[f64]>,
    pub(crate) acceleration: Option<Arc<AccelerationGraph>>,
    /// True when the attached acceleration weights were customized with the
    /// request's failure-mode penalties baked in (degraded graphs only);
    /// plain acceleration weights must never serve failure-mode searches.
    pub(crate) acceleration_includes_failure_penalties: bool,
    pub(crate) automaton: Arc<RestrictionAutomaton>,
    pub(crate) reverse_automaton: Arc<RestrictionAutomaton>,
    /// Original edge of each virtual reverse edge, indexed by edge. Empty
    /// for graphs without virtual edges.
    pub(crate) virtual_reverse_of: Arc<[Option<usize>]>,
    pub(crate) includes_temporal_materialized_directions: bool,
    /// Pairwise turn restrictions under a time condition. Time-dependent
    /// graphs keep these transitions and searches check the condition when
    /// the vehicle reaches the junction; static graphs apply such
    /// restrictions as their `applies_statically` flag says and leave this
    /// empty.
    pub(crate) conditional_turns: Arc<ConditionalTurns>,
    /// Turn penalty seconds per forward transition, computed on first use.
    turn_seconds: Arc<std::sync::OnceLock<Option<TransitionTurnSeconds>>>,
}

/// `(from edge, to edge, temporal rule set)` of time-conditional pairwise
/// turn restrictions, sorted. The restriction is in force while its rule
/// set evaluates unavailable, like an edge schedule.
#[derive(Debug, Default)]
pub(crate) struct ConditionalTurns {
    turns: Vec<(u32, u32, u32)>,
}

impl ConditionalTurns {
    /// The time-conditional pairwise restrictions of `topology` that bind
    /// `mode_bit`, whether or not static routing applies them.
    pub(crate) fn for_mode(topology: &TopologyBundle, mode_bit: u16) -> Self {
        let mut turns = topology
            .turn_restrictions
            .iter()
            .filter(|restriction| {
                restriction.mode_mask.contains(mode_bit) && restriction.edge_path.len() == 2
            })
            .filter_map(|restriction| {
                let condition = restriction.condition?;
                Some((
                    restriction.edge_path[0].0,
                    restriction.edge_path[1].0,
                    condition.rule_set_id,
                ))
            })
            .collect::<Vec<_>>();
        turns.sort_unstable();
        turns.dedup();
        Self { turns }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    /// Rule sets of the conditional restrictions on `from -> to`.
    pub(crate) fn rule_sets(&self, from: usize, to: usize) -> impl Iterator<Item = usize> + '_ {
        let key = (from as u32, to as u32);
        let start = self
            .turns
            .partition_point(|&(from, to, _)| (from, to) < key);
        self.turns[start..]
            .iter()
            .take_while(move |&&(from, to, _)| (from, to) == key)
            .map(|&(_, _, rule_set)| rule_set as usize)
    }
}

/// Turn penalty seconds per forward transition as an index into the few
/// distinct penalty values, so searches avoid recomputing turn geometry.
pub(crate) struct TransitionTurnSeconds {
    values: Vec<f64>,
    classes: Vec<u8>,
}

impl TransitionTurnSeconds {
    pub(crate) fn get(&self, transition_index: usize) -> f64 {
        self.values[self.classes[transition_index] as usize]
    }
}

impl TransitionTurnSeconds {
    fn build(
        graph: &RoutingGraph,
        topology: &TopologyBundle,
        metrics: &CompiledProfileBundle,
    ) -> Option<Self> {
        use rayon::prelude::*;
        let seconds = (0..graph.edge_costs.len())
            .into_par_iter()
            .flat_map_iter(|edge_index| {
                graph
                    .transition_range(edge_index)
                    .map(move |transition_index| {
                        let next_edge = graph.transition_edges[transition_index] as usize;
                        turn_penalty_seconds(topology, metrics, edge_index, next_edge)
                    })
            })
            .collect::<Vec<_>>();
        let mut values = Vec::<f64>::new();
        let mut classes = Vec::with_capacity(seconds.len());
        for seconds in seconds {
            let class = match values
                .iter()
                .position(|known| known.to_bits() == seconds.to_bits())
            {
                Some(class) => class,
                None => {
                    values.push(seconds);
                    values.len() - 1
                }
            };
            classes.push(u8::try_from(class).ok()?);
        }
        Some(Self { values, classes })
    }
}

impl RoutingGraph {
    /// Exact `turn_penalty_seconds` for every forward transition, built on
    /// first use. `None` when more than 256 distinct values occur; callers
    /// then compute turns directly. `topology` and `metrics` must be the
    /// ones the graph was built from.
    pub(crate) fn transition_turn_seconds(
        &self,
        topology: &TopologyBundle,
        metrics: &CompiledProfileBundle,
    ) -> Option<&TransitionTurnSeconds> {
        self.turn_seconds
            .get_or_init(|| TransitionTurnSeconds::build(self, topology, metrics))
            .as_ref()
    }

    /// `turn_penalty_seconds(previous_edge, next_edge)`, read from the
    /// transition table when it has already been built and the transition
    /// belongs to this graph; computed from the turn geometry otherwise.
    /// The table holds the same values, so both paths agree bit for bit.
    /// `topology` and `metrics` must be the ones the graph was built from.
    pub(crate) fn turn_seconds_between(
        &self,
        topology: &TopologyBundle,
        metrics: &CompiledProfileBundle,
        previous_edge: usize,
        next_edge: usize,
    ) -> f64 {
        if let Some(Some(table)) = self.turn_seconds.get()
            && previous_edge < self.edge_costs.len()
            && let Some(transition_index) = self
                .transition_range(previous_edge)
                .find(|&index| self.transition_edges[index] as usize == next_edge)
        {
            return table.get(transition_index);
        }
        turn_penalty_seconds(topology, metrics, previous_edge, next_edge)
    }

    pub(crate) fn outgoing_edges(&self, node_index: usize) -> &[u32] {
        let start = self.first_out[node_index] as usize;
        let end = self.first_out[node_index + 1] as usize;
        &self.edge_order[start..end]
    }

    pub(crate) fn incoming_edges(&self, node_index: usize) -> &[u32] {
        let start = self.incoming_first_out[node_index] as usize;
        let end = self.incoming_first_out[node_index + 1] as usize;
        &self.incoming_edge_order[start..end]
    }

    pub(crate) fn has_restriction_sequences(&self) -> bool {
        !self.automaton.is_trivial()
    }

    pub(crate) fn transition_range(&self, edge_index: usize) -> std::ops::Range<usize> {
        self.transition_first_out[edge_index] as usize
            ..self.transition_first_out[edge_index + 1] as usize
    }

    pub(crate) fn reverse_transition_range(&self, edge_index: usize) -> std::ops::Range<usize> {
        self.reverse_transition_first_out[edge_index] as usize
            ..self.reverse_transition_first_out[edge_index + 1] as usize
    }

    #[cfg(test)]
    pub(crate) fn has_edge_transition(&self, from_edge: usize, to_edge: usize) -> bool {
        self.transition_range(from_edge)
            .any(|index| self.transition_edges[index] == to_edge as u32)
    }
}

pub(crate) struct AccelerationGraph {
    pub(crate) source: Arc<DatasetAccelerationBundle>,
    /// The customized profile. Its generalized-cost arc weights may be
    /// absent: [`Self::ranked`] holds them.
    pub(crate) metrics: Arc<CompiledProfileBundle>,
    /// The hierarchy renumbered by rank, with the cost weights.
    pub(crate) ranked: RankedCch,
    /// Service-area weight sets customized on first use when the compiled
    /// profile was loaded without them. `None` for graphs whose weights
    /// carry request-specific penalties (failure modes), which must never
    /// gain plain per-metric weights.
    pub(crate) deferred_metric_weights: Option<Arc<DeferredMetricWeights>>,
    pub(crate) unpack: UnpackTable,
    /// Lower-bound customization for restriction-aware A* potentials,
    /// built on first use.
    pub(crate) lower_bounds: std::sync::OnceLock<Option<LowerBoundWeights>>,
}

type ArcWeights = Option<(Vec<u32>, Vec<u32>)>;

/// Travel-time and distance CCH weights customized on first use and shared
/// by every routing graph of one prepared engine. Only service areas read
/// them, so route-only servers never hold them in memory.
#[derive(Default)]
pub(crate) struct DeferredMetricWeights {
    travel_time: std::sync::OnceLock<ArcWeights>,
    distance: std::sync::OnceLock<ArcWeights>,
}

impl AccelerationGraph {
    /// Bundle slot of each arc of the ranked downward rows, for searches
    /// that pair them with other slot-ordered weight sets; built on first
    /// use and shared by every metric on the hierarchy.
    /// `downward_slots()[p]` is the slot of the arc at position `p` of
    /// [`RankedCch::downward_range`].
    pub(crate) fn downward_slots(&self) -> &[u32] {
        self.ranked.downward_slots(&self.source)
    }

    fn compiled_metric_weights(
        &self,
        metric_kind: crate::service_area::ServiceAreaMetricKind,
    ) -> Option<(&[u32], &[u32])> {
        let compiled = self.metrics.acceleration.as_ref()?;
        let (upward, downward) = match metric_kind {
            crate::service_area::ServiceAreaMetricKind::TravelTimeS => {
                (&compiled.time_upward_weight, &compiled.time_downward_weight)
            }
            crate::service_area::ServiceAreaMetricKind::DistanceM => (
                &compiled.distance_upward_weight,
                &compiled.distance_downward_weight,
            ),
        };
        (upward.len() == self.source.upward_head.len()
            && downward.len() == self.source.downward_head.len())
        .then_some((upward.as_slice(), downward.as_slice()))
    }

    fn deferred_cell(
        &self,
        metric_kind: crate::service_area::ServiceAreaMetricKind,
    ) -> Option<(
        &std::sync::OnceLock<ArcWeights>,
        netweevil_profile::ServiceAreaMetric,
    )> {
        let deferred = self.deferred_metric_weights.as_deref()?;
        Some(match metric_kind {
            crate::service_area::ServiceAreaMetricKind::TravelTimeS => (
                &deferred.travel_time,
                netweevil_profile::ServiceAreaMetric::TravelTime,
            ),
            crate::service_area::ServiceAreaMetricKind::DistanceM => (
                &deferred.distance,
                netweevil_profile::ServiceAreaMetric::Distance,
            ),
        })
    }

    /// Customized weight sets for a service-area metric over the hierarchy
    /// arcs, or `None` when no arc-aligned weights exist for that metric.
    /// Deferred weights are customized on the first call (blocking
    /// concurrent callers until done); `topology` must be the topology this
    /// graph was built from. Callers fall back to Dijkstra on `None`.
    pub(crate) fn metric_weights(
        &self,
        metric_kind: crate::service_area::ServiceAreaMetricKind,
        topology: &TopologyBundle,
    ) -> Option<(&[u32], &[u32])> {
        if let Some(weights) = self.compiled_metric_weights(metric_kind) {
            return Some(weights);
        }
        let (cell, metric) = self.deferred_cell(metric_kind)?;
        cell.get_or_init(|| {
            netweevil_profile::customize_service_area_metric(
                topology,
                &self.metrics,
                &self.source,
                metric,
            )
        })
        .as_ref()
        .map(|(upward, downward)| (upward.as_slice(), downward.as_slice()))
    }

    /// Whether [`Self::metric_weights`] can return weights, without
    /// customizing deferred ones.
    pub(crate) fn has_metric_weights(
        &self,
        metric_kind: crate::service_area::ServiceAreaMetricKind,
    ) -> bool {
        self.compiled_metric_weights(metric_kind).is_some()
            || self
                .deferred_cell(metric_kind)
                .is_some_and(|(cell, _)| cell.get().is_none_or(Option::is_some))
    }

    /// Binary-searches the head-sorted CSR row of `tail` for an arc to `head`.
    pub(crate) fn upward_arc_slot(&self, tail: usize, head: u32) -> Option<usize> {
        let start = self.source.upward_first_out[tail] as usize;
        let end = self.source.upward_first_out[tail + 1] as usize;
        self.source.upward_head[start..end]
            .binary_search(&head)
            .ok()
            .map(|offset| start + offset)
    }

    pub(crate) fn downward_arc_slot(&self, tail: usize, head: u32) -> Option<usize> {
        let start = self.source.downward_first_out[tail] as usize;
        let end = self.source.downward_first_out[tail + 1] as usize;
        self.source.downward_head[start..end]
            .binary_search(&head)
            .ok()
            .map(|offset| start + offset)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SearchStateKey {
    pub(crate) edge_index: usize,
    pub(crate) automaton_state: usize,
}

pub(crate) const NO_PREVIOUS_EDGE: u32 = u32::MAX;
pub(crate) const NO_PREVIOUS_ARC: u32 = u32::MAX;

struct EdgeBasedTopologyView<'a> {
    node_first_out: &'a [u32],
    node_edge_order: &'a [u32],
    edge_transition_first_out: &'a [u32],
    edge_transition_edges: &'a [u32],
}

fn edge_based_topology_view(topology: &TopologyBundle) -> Option<EdgeBasedTopologyView<'_>> {
    let edge_topology = &topology.edge_based_topology;
    let edge_count = topology.edge_count();
    if edge_topology.node_first_out.len() != topology.nodes.len() + 1
        || edge_topology.node_edge_order.len() != edge_count
        || edge_topology.edge_transition_first_out.len() != edge_count + 1
    {
        return None;
    }
    let expected_transition_len = edge_topology
        .edge_transition_first_out
        .last()
        .copied()
        .unwrap_or_default() as usize;
    if edge_topology.edge_transition_edges.len() != expected_transition_len {
        return None;
    }
    Some(EdgeBasedTopologyView {
        node_first_out: &edge_topology.node_first_out,
        node_edge_order: &edge_topology.node_edge_order,
        edge_transition_first_out: &edge_topology.edge_transition_first_out,
        edge_transition_edges: &edge_topology.edge_transition_edges,
    })
}

#[allow(clippy::needless_range_loop)]
pub(crate) fn build_edge_based_topology_fallback(
    topology: &TopologyBundle,
) -> netweevil_core::EdgeBasedTopology {
    let mut out_degree = vec![0_u32; topology.nodes.len()];
    let edge_count = topology.edge_count();
    let mut head = vec![0_u32; edge_count];
    for edge_index in 0..edge_count {
        let edge = topology.routing_edge(edge_index);
        out_degree[edge.from.0 as usize] += 1;
        head[edge_index] = edge.to.0;
    }

    let mut node_first_out = vec![0_u32; topology.nodes.len() + 1];
    for (node_index, degree) in out_degree.iter().enumerate() {
        node_first_out[node_index + 1] = node_first_out[node_index] + degree;
    }

    let mut node_edge_order = vec![0_u32; edge_count];
    let mut write_positions = node_first_out[..topology.nodes.len()].to_vec();
    for edge_index in 0..edge_count {
        let edge = topology.routing_edge(edge_index);
        let write_index = &mut write_positions[edge.from.0 as usize];
        node_edge_order[*write_index as usize] = edge_index as u32;
        *write_index += 1;
    }

    let mut edge_transition_first_out = vec![0_u32; edge_count + 1];
    for edge_index in 0..edge_count {
        let head_node = head[edge_index] as usize;
        edge_transition_first_out[edge_index + 1] = edge_transition_first_out[edge_index]
            + (node_first_out[head_node + 1] - node_first_out[head_node]);
    }

    let mut edge_transition_edges = vec![0_u32; edge_transition_first_out[edge_count] as usize];
    let mut transition_write_positions = edge_transition_first_out[..edge_count].to_vec();
    for edge_index in 0..edge_count {
        let head_node = head[edge_index] as usize;
        for &next_edge in &node_edge_order
            [node_first_out[head_node] as usize..node_first_out[head_node + 1] as usize]
        {
            let write_index = &mut transition_write_positions[edge_index];
            edge_transition_edges[*write_index as usize] = next_edge;
            *write_index += 1;
        }
    }

    netweevil_core::EdgeBasedTopology {
        node_first_out,
        node_edge_order,
        edge_transition_first_out,
        edge_transition_edges,
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RoutingGraphBuildOptions {
    pub(crate) ignore_multi_edge_restriction_sequences: bool,
    pub(crate) search_time_turn_restrictions: bool,
    pub(crate) include_temporal_materialized_directions: bool,
}

pub(crate) fn build_routing_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
) -> Result<RoutingGraph> {
    build_routing_graph_with_options(topology, metrics, RoutingGraphBuildOptions::default())
}

/// Builds the exact-search graph used when request-owned temporal direction
/// rules may enable reverse edges that are intentionally absent from the
/// static graph and its CCH weights.
pub(crate) fn build_temporal_routing_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
) -> Result<RoutingGraph> {
    build_routing_graph_with_options(
        topology,
        metrics,
        RoutingGraphBuildOptions {
            include_temporal_materialized_directions: true,
            ..RoutingGraphBuildOptions::default()
        },
    )
}

pub(crate) fn build_routing_graph_with_options(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    options: RoutingGraphBuildOptions,
) -> Result<RoutingGraph> {
    build_routing_graph_with_options_from_shared(topology, Arc::new(metrics.clone()), None, options)
}

#[cfg(test)]
pub(crate) fn build_routing_graph_from_shared(
    topology: &TopologyBundle,
    metrics: Arc<CompiledProfileBundle>,
    acceleration: Option<Arc<DatasetAccelerationBundle>>,
) -> Result<RoutingGraph> {
    build_routing_graph_with_options_from_shared(
        topology,
        metrics,
        acceleration,
        RoutingGraphBuildOptions::default(),
    )
}

pub(crate) fn build_routing_graph_with_options_from_shared(
    topology: &TopologyBundle,
    metrics: Arc<CompiledProfileBundle>,
    acceleration: Option<Arc<DatasetAccelerationBundle>>,
    options: RoutingGraphBuildOptions,
) -> Result<RoutingGraph> {
    build_routing_graph_with_weights(topology, metrics, None, acceleration, options)
}

/// [`build_routing_graph_with_options_from_shared`] with the hierarchy's
/// cost weights passed separately (see [`build_acceleration_graph`]).
#[allow(clippy::needless_range_loop)]
pub(crate) fn build_routing_graph_with_weights(
    topology: &TopologyBundle,
    metrics: Arc<CompiledProfileBundle>,
    weights: Option<CchArcWeights>,
    acceleration: Option<Arc<DatasetAccelerationBundle>>,
    options: RoutingGraphBuildOptions,
) -> Result<RoutingGraph> {
    let edge_count = topology.edge_count();
    let mut out_degree = vec![0_u32; topology.nodes.len()];
    let mut in_degree = vec![0_u32; topology.nodes.len()];
    let mut edge_costs = vec![f64::INFINITY; edge_count];

    let mode_bit = metrics.mode.access_bit();
    for edge_index in 0..edge_count {
        let edge = topology.routing_edge(edge_index);
        let metric = &metrics.edge_metrics[edge_index];
        // Scenario-materialized directions and edges the mode may use only
        // under a time condition belong to time-dependent searches alone.
        if !options.include_temporal_materialized_directions
            && !netweevil_profile::is_statically_usable(topology, edge_index, mode_bit)
        {
            continue;
        }
        if let (Some(cost), Some(_)) = (metric.generalized_cost, metric.travel_time_s) {
            edge_costs[edge_index] = cost;
            in_degree[edge.to.0 as usize] += 1;
        }
    }

    let edge_topology = edge_based_topology_view(topology).context(
        "topology bundle is missing persisted edge-based adjacency; re-import the dataset with the current format",
    )?;

    for node_index in 0..topology.nodes.len() {
        for &edge_index in &edge_topology.node_edge_order[edge_topology.node_first_out[node_index]
            as usize
            ..edge_topology.node_first_out[node_index + 1] as usize]
        {
            if edge_costs[edge_index as usize].is_finite() {
                out_degree[node_index] += 1;
            }
        }
    }

    let mut first_out = vec![0_u32; topology.nodes.len() + 1];
    for (node_index, degree) in out_degree.iter().enumerate() {
        first_out[node_index + 1] = first_out[node_index] + degree;
    }
    let mut edge_order = vec![0_u32; first_out[topology.nodes.len()] as usize];
    let mut write_positions = first_out[..topology.nodes.len()].to_vec();
    let mut incoming_first_out = vec![0_u32; topology.nodes.len() + 1];
    for (node_index, degree) in in_degree.iter().enumerate() {
        incoming_first_out[node_index + 1] = incoming_first_out[node_index] + degree;
    }
    let mut incoming_edge_order = vec![0_u32; incoming_first_out[topology.nodes.len()] as usize];
    let mut incoming_write_positions = incoming_first_out[..topology.nodes.len()].to_vec();
    for node_index in 0..topology.nodes.len() {
        for &edge_index in &edge_topology.node_edge_order[edge_topology.node_first_out[node_index]
            as usize
            ..edge_topology.node_first_out[node_index + 1] as usize]
        {
            if !edge_costs[edge_index as usize].is_finite() {
                continue;
            }
            let write_index = &mut write_positions[node_index];
            edge_order[*write_index as usize] = edge_index;
            *write_index += 1;
            let head_node = topology.routing_edge(edge_index as usize).to.0 as usize;
            let incoming_write_index = &mut incoming_write_positions[head_node];
            incoming_edge_order[*incoming_write_index as usize] = edge_index;
            *incoming_write_index += 1;
        }
    }

    let mut pairwise_forbidden = Vec::<(u32, u32)>::new();
    let mut restricted_sequences = Vec::new();
    // Time-dependent graphs keep conditional pairwise turns as transitions
    // and check them at the junction (see `ConditionalTurns`); conditional
    // sequences keep their static treatment there too.
    let time_dependent = options.include_temporal_materialized_directions;
    for restriction in topology.turn_restrictions.iter().filter(|restriction| {
        restriction.mode_mask.contains(mode_bit) && restriction.applies_statically()
    }) {
        if restriction.edge_path.len() < 2
            || (time_dependent
                && restriction.condition.is_some()
                && restriction.edge_path.len() == 2)
        {
            continue;
        }
        if options.search_time_turn_restrictions {
            if restriction.edge_path.len() > 2 && options.ignore_multi_edge_restriction_sequences {
                continue;
            }
            restricted_sequences.push(
                restriction
                    .edge_path
                    .iter()
                    .map(|edge| edge.0 as usize)
                    .collect::<Vec<_>>(),
            );
            continue;
        }
        if restriction.edge_path.len() == 2 {
            pairwise_forbidden.push((restriction.edge_path[0].0, restriction.edge_path[1].0));
            continue;
        }
        if options.ignore_multi_edge_restriction_sequences {
            continue;
        }
        restricted_sequences.push(
            restriction
                .edge_path
                .iter()
                .map(|edge| edge.0 as usize)
                .collect::<Vec<_>>(),
        );
    }

    pairwise_forbidden.sort_unstable();
    pairwise_forbidden.dedup();
    let mut forbidden_turn_first_out = vec![0_u32; edge_count + 1];
    for &(from_edge, _) in &pairwise_forbidden {
        forbidden_turn_first_out[from_edge as usize + 1] += 1;
    }
    for edge_index in 0..edge_count {
        forbidden_turn_first_out[edge_index + 1] += forbidden_turn_first_out[edge_index];
    }
    let mut forbidden_turns = vec![0_u32; pairwise_forbidden.len()];
    let mut write_positions = forbidden_turn_first_out[..edge_count].to_vec();
    for (from_edge, to_edge) in pairwise_forbidden {
        let write_index = &mut write_positions[from_edge as usize];
        forbidden_turns[*write_index as usize] = to_edge;
        *write_index += 1;
    }

    let mut transition_degree = vec![0_u32; edge_count];
    for edge_index in 0..edge_count {
        if !edge_costs[edge_index].is_finite() {
            continue;
        }
        for &next_edge in &edge_topology.edge_transition_edges[edge_topology
            .edge_transition_first_out[edge_index]
            as usize
            ..edge_topology.edge_transition_first_out[edge_index + 1] as usize]
        {
            if !edge_costs[next_edge as usize].is_finite() {
                continue;
            }
            if options.search_time_turn_restrictions
                || forbidden_turns[forbidden_turn_first_out[edge_index] as usize
                    ..forbidden_turn_first_out[edge_index + 1] as usize]
                    .binary_search(&next_edge)
                    .is_err()
            {
                transition_degree[edge_index] += 1;
            }
        }
    }
    let mut transition_first_out = vec![0_u32; edge_count + 1];
    for edge_index in 0..edge_count {
        transition_first_out[edge_index + 1] =
            transition_first_out[edge_index] + transition_degree[edge_index];
    }
    let transition_count = transition_first_out[edge_count] as usize;
    let mut transition_edges = vec![0_u32; transition_count];
    let mut transition_costs = vec![0.0_f64; transition_count];
    let mut transition_write_positions = transition_first_out[..edge_count].to_vec();
    for edge_index in 0..edge_count {
        if !edge_costs[edge_index].is_finite() {
            continue;
        }
        for &next_edge in &edge_topology.edge_transition_edges[edge_topology
            .edge_transition_first_out[edge_index]
            as usize
            ..edge_topology.edge_transition_first_out[edge_index + 1] as usize]
        {
            let next_edge = next_edge as usize;
            if !edge_costs[next_edge].is_finite() {
                continue;
            }
            let write_index = &mut transition_write_positions[edge_index];
            if !options.search_time_turn_restrictions
                && forbidden_turns[forbidden_turn_first_out[edge_index] as usize
                    ..forbidden_turn_first_out[edge_index + 1] as usize]
                    .binary_search(&(next_edge as u32))
                    .is_ok()
            {
                continue;
            }
            let slot = *write_index as usize;
            transition_edges[slot] = next_edge as u32;
            transition_costs[slot] = edge_costs[next_edge]
                + turn_penalty_cost(metrics.as_ref(), topology, edge_index, next_edge);
            *write_index += 1;
        }
    }
    let mut reverse_transition_first_out = vec![0_u32; edge_count + 1];
    for &next_edge in &transition_edges {
        reverse_transition_first_out[next_edge as usize + 1] += 1;
    }
    for edge_index in 0..edge_count {
        reverse_transition_first_out[edge_index + 1] += reverse_transition_first_out[edge_index];
    }
    let mut reverse_transition_edges = vec![0_u32; transition_count];
    let mut reverse_transition_costs = vec![0.0_f64; transition_count];
    let mut reverse_write_positions = reverse_transition_first_out[..edge_count].to_vec();
    for edge_index in 0..edge_count {
        for transition_index in
            transition_first_out[edge_index] as usize..transition_first_out[edge_index + 1] as usize
        {
            let next_edge = transition_edges[transition_index] as usize;
            let write_index = &mut reverse_write_positions[next_edge];
            let slot = *write_index as usize;
            reverse_transition_edges[slot] = edge_index as u32;
            reverse_transition_costs[slot] = transition_costs[transition_index];
            *write_index += 1;
        }
    }

    let reverse_restricted_sequences = restricted_sequences
        .iter()
        .map(|sequence| sequence.iter().rev().copied().collect::<Vec<_>>())
        .collect::<Vec<_>>();

    let edge_spatial_index =
        crate::edge_spatial_index::EdgeSpatialIndex::build(topology, &edge_costs);
    Ok(RoutingGraph {
        edge_spatial_index: Arc::new(edge_spatial_index),
        first_out: first_out.into(),
        edge_order: edge_order.into(),
        incoming_first_out: incoming_first_out.into(),
        incoming_edge_order: incoming_edge_order.into(),
        edge_costs: edge_costs.into(),
        transition_first_out: transition_first_out.into(),
        transition_edges: transition_edges.into(),
        transition_costs: transition_costs.into(),
        reverse_transition_first_out: reverse_transition_first_out.into(),
        reverse_transition_edges: reverse_transition_edges.into(),
        reverse_transition_costs: reverse_transition_costs.into(),
        // Temporal-inclusive graphs intentionally run exact searches: their
        // scenario-only directions are absent from the static CCH weights.
        // Do not invoke the acceleration loader here, because a missing
        // dataset bundle is expected and warning that the user must re-import
        // would be misleading.
        acceleration: if options.include_temporal_materialized_directions {
            None
        } else {
            build_acceleration_graph(metrics, weights, acceleration, edge_count)?.map(Arc::new)
        },
        acceleration_includes_failure_penalties: false,
        automaton: Arc::new(RestrictionAutomaton::build(&restricted_sequences)),
        reverse_automaton: Arc::new(RestrictionAutomaton::build(&reverse_restricted_sequences)),
        virtual_reverse_of: Arc::default(),
        includes_temporal_materialized_directions: options.include_temporal_materialized_directions,
        conditional_turns: Arc::new(if time_dependent {
            ConditionalTurns::for_mode(topology, mode_bit)
        } else {
            ConditionalTurns::default()
        }),
        turn_seconds: Arc::default(),
    })
}

impl RoutingGraph {
    /// This graph without its multi-edge restriction sequences, sharing
    /// every array, the hierarchy and the lazily built tables. For graphs
    /// built without search-time turn restrictions, whose pairwise
    /// restrictions are removed transitions and so still apply; the result
    /// matches building with `ignore_multi_edge_restriction_sequences`.
    pub(crate) fn without_restriction_sequences(&self) -> Self {
        Self {
            edge_spatial_index: self.edge_spatial_index.clone(),
            first_out: self.first_out.clone(),
            edge_order: self.edge_order.clone(),
            incoming_first_out: self.incoming_first_out.clone(),
            incoming_edge_order: self.incoming_edge_order.clone(),
            edge_costs: self.edge_costs.clone(),
            transition_first_out: self.transition_first_out.clone(),
            transition_edges: self.transition_edges.clone(),
            transition_costs: self.transition_costs.clone(),
            reverse_transition_first_out: self.reverse_transition_first_out.clone(),
            reverse_transition_edges: self.reverse_transition_edges.clone(),
            reverse_transition_costs: self.reverse_transition_costs.clone(),
            acceleration: self.acceleration.clone(),
            acceleration_includes_failure_penalties: self.acceleration_includes_failure_penalties,
            automaton: Arc::new(RestrictionAutomaton::build(&[])),
            reverse_automaton: Arc::new(RestrictionAutomaton::build(&[])),
            virtual_reverse_of: self.virtual_reverse_of.clone(),
            includes_temporal_materialized_directions: self
                .includes_temporal_materialized_directions,
            conditional_turns: self.conditional_turns.clone(),
            turn_seconds: self.turn_seconds.clone(),
        }
    }

    /// This graph re-costed for `metrics`, sharing every metric-independent
    /// array. Returns `None` when `metrics` changes which edges are
    /// traversable (a closure), the turn costs or the mode, all of which
    /// change the adjacency; callers then build a fresh graph. `base_metrics`
    /// must be the metrics this graph was built from, and the hierarchy's
    /// customized weights for `metrics` come from `weights` or, when that is
    /// `None`, from `metrics` itself.
    pub(crate) fn with_metrics(
        &self,
        topology: &TopologyBundle,
        base_metrics: &CompiledProfileBundle,
        metrics: Arc<CompiledProfileBundle>,
        weights: Option<&CchArcWeights>,
    ) -> Option<Self> {
        let turns = |metrics: &CompiledProfileBundle| {
            let turns = metrics.turn_costs;
            [
                turns.left_penalty_s,
                turns.right_penalty_s,
                turns.uturn_penalty_s,
                turns.traffic_signal_penalty_s,
                turns.roundabout_entry_penalty_s,
                turns.cost_time_weight,
                turns.destination_only_penalty_s,
            ]
            .map(f64::to_bits)
        };
        if self.includes_temporal_materialized_directions
            || self.acceleration_includes_failure_penalties
            || metrics.mode != base_metrics.mode
            || turns(&metrics) != turns(base_metrics)
            || metrics.edge_metrics.len() != self.edge_costs.len()
        {
            return None;
        }
        let mode_bit = metrics.mode.access_bit();
        let mut edge_costs = vec![f64::INFINITY; self.edge_costs.len()];
        for (edge_index, metric) in metrics.edge_metrics.iter().enumerate() {
            // The builder's static usability: no scenario-materialized
            // direction and no access that holds only under a time condition.
            let traversable =
                netweevil_profile::is_statically_usable(topology, edge_index, mode_bit)
                    && metric.travel_time_s.is_some();
            let cost = metric.generalized_cost.filter(|_| traversable);
            if cost.is_some() != self.edge_costs[edge_index].is_finite() {
                return None;
            }
            if let Some(cost) = cost {
                edge_costs[edge_index] = cost;
            }
        }
        // The builder's arithmetic: head cost plus turn seconds times the
        // time weight plus the destination-only entry cost, so the costs
        // equal those of a fresh build bit for bit.
        let turn_seconds = self.transition_turn_seconds(topology, base_metrics);
        let weight = metrics.turn_costs.cost_time_weight;
        let mut transition_costs = vec![0.0_f64; self.transition_edges.len()];
        for edge_index in 0..edge_costs.len() {
            for transition_index in self.transition_range(edge_index) {
                let next_edge = self.transition_edges[transition_index] as usize;
                let seconds = match turn_seconds {
                    Some(turn_seconds) => turn_seconds.get(transition_index),
                    None => turn_penalty_seconds(topology, base_metrics, edge_index, next_edge),
                };
                transition_costs[transition_index] = edge_costs[next_edge]
                    + (seconds * weight
                        + transition_access_cost(topology, base_metrics, edge_index, next_edge));
            }
        }
        let mut reverse_transition_costs = vec![0.0_f64; transition_costs.len()];
        let mut reverse_write_positions =
            self.reverse_transition_first_out[..edge_costs.len()].to_vec();
        for edge_index in 0..edge_costs.len() {
            for transition_index in self.transition_range(edge_index) {
                let next_edge = self.transition_edges[transition_index] as usize;
                let slot = &mut reverse_write_positions[next_edge];
                reverse_transition_costs[*slot as usize] = transition_costs[transition_index];
                *slot += 1;
            }
        }
        let acceleration = match &self.acceleration {
            Some(base) => Some(Arc::new(base.with_metrics(metrics, weights)?)),
            None => None,
        };
        Some(Self {
            edge_spatial_index: self.edge_spatial_index.clone(),
            first_out: self.first_out.clone(),
            edge_order: self.edge_order.clone(),
            incoming_first_out: self.incoming_first_out.clone(),
            incoming_edge_order: self.incoming_edge_order.clone(),
            edge_costs: edge_costs.into(),
            transition_first_out: self.transition_first_out.clone(),
            transition_edges: self.transition_edges.clone(),
            transition_costs: transition_costs.into(),
            reverse_transition_first_out: self.reverse_transition_first_out.clone(),
            reverse_transition_edges: self.reverse_transition_edges.clone(),
            reverse_transition_costs: reverse_transition_costs.into(),
            acceleration,
            acceleration_includes_failure_penalties: false,
            automaton: self.automaton.clone(),
            reverse_automaton: self.reverse_automaton.clone(),
            virtual_reverse_of: self.virtual_reverse_of.clone(),
            includes_temporal_materialized_directions: false,
            conditional_turns: self.conditional_turns.clone(),
            // Turn seconds depend only on the topology and the turn costs,
            // which `with_metrics` requires to be unchanged.
            turn_seconds: self.turn_seconds.clone(),
        })
    }
}

impl AccelerationGraph {
    /// The same hierarchy with the customized weights of `metrics`, taken
    /// from `weights` when given and from the bundle otherwise. Shares the
    /// ranked topology; only the weights are new. `None` when the weights do
    /// not fit the hierarchy or overflow, in which case a fresh build falls
    /// back to exact search.
    fn with_metrics(
        &self,
        metrics: Arc<CompiledProfileBundle>,
        weights: Option<&CchArcWeights>,
    ) -> Option<Self> {
        let compiled = metrics.acceleration.as_ref()?;
        let (upward_weight, downward_weight) = match weights {
            Some((upward, downward)) => (upward.as_slice(), downward.as_slice()),
            None => (
                compiled.upward_weight.as_slice(),
                compiled.downward_weight.as_slice(),
            ),
        };
        if upward_weight.len() != self.source.upward_head.len()
            || downward_weight.len() != self.source.downward_head.len()
            || upward_weight
                .iter()
                .chain(downward_weight)
                .any(|weight| *weight == netweevil_core::CCH_WEIGHT_OVERFLOW)
        {
            return None;
        }
        let ranked = self
            .ranked
            .with_weights(&self.source, upward_weight, downward_weight);
        Some(Self {
            source: self.source.clone(),
            metrics,
            ranked,
            deferred_metric_weights: None,
            unpack: UnpackTable::new(
                self.source.upward_head.len(),
                self.source.downward_head.len(),
            ),
            lower_bounds: std::sync::OnceLock::new(),
        })
    }
}

/// Slot-ordered `(upward, downward)` generalized-cost weights of a
/// hierarchy, as customization emits them.
pub(crate) type CchArcWeights = (Vec<u32>, Vec<u32>);

/// Compiled profile bundles persist per-edge metrics only. When `metrics`
/// carries an acceleration header for `dataset_acceleration` but no arc
/// weights yet, customize the generalized-cost weights now and return them
/// for [`build_routing_graph_with_weights`]; the bundle itself stays
/// weightless, so the ranked hierarchy holds the only copy. Service-area
/// weights are then deferred (see [`DeferredMetricWeights`]). Returns `None`
/// for bundles that carry weights already and for bundles that do not belong
/// together, which [`build_acceleration_graph`] reports.
pub(crate) fn customize_loaded_metrics(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    dataset_acceleration: Option<&DatasetAccelerationBundle>,
) -> Option<CchArcWeights> {
    let (Some(source), Some(compiled)) = (dataset_acceleration, metrics.acceleration.as_ref())
    else {
        return None;
    };
    let customized = compiled.upward_weight.len() == source.upward_head.len()
        && compiled.downward_weight.len() == source.downward_head.len();
    if customized
        || source.source_topology_bundle_id != metrics.source_topology_bundle_id
        || !source.is_current_format()
        || compiled.algorithm != netweevil_core::CCH_ALGORITHM
        || compiled.schema_version != netweevil_core::COMPILED_ACCELERATION_SCHEMA_VERSION
        || metrics.edge_metrics.len() != topology.edge_count()
    {
        return None;
    }
    let weights = netweevil_profile::customize_acceleration(
        topology,
        metrics,
        source,
        compiled.source_acceleration_bundle_id.clone(),
    );
    Some((weights.upward_weight, weights.downward_weight))
}

/// The ranked hierarchy of `metrics` over `dataset_acceleration`. The cost
/// weights come from `weights` when given (customized at load) and from the
/// bundle's acceleration otherwise.
pub(crate) fn build_acceleration_graph(
    metrics: Arc<CompiledProfileBundle>,
    weights: Option<CchArcWeights>,
    dataset_acceleration: Option<Arc<DatasetAccelerationBundle>>,
    edge_count: usize,
) -> Result<Option<AccelerationGraph>> {
    let Some(acceleration) = metrics.acceleration.as_ref() else {
        if dataset_acceleration.is_some() {
            tracing::warn!(
                profile = %metrics.profile_id,
                "compiled profile has no CCH weights although the dataset has an acceleration \
                 bundle; queries fall back to the exact engine. Recompile the profile to restore \
                 accelerated routing"
            );
        }
        return Ok(None);
    };
    let (upward_weight, downward_weight) = match &weights {
        Some((upward, downward)) => (upward.as_slice(), downward.as_slice()),
        None => (
            acceleration.upward_weight.as_slice(),
            acceleration.downward_weight.as_slice(),
        ),
    };
    if upward_weight.contains(&netweevil_core::CCH_WEIGHT_OVERFLOW)
        || downward_weight.contains(&netweevil_core::CCH_WEIGHT_OVERFLOW)
    {
        return Ok(None);
    }
    let Some(topology_source) = dataset_acceleration else {
        // Profiles compiled without a dataset CCH bundle in reach run on the
        // exact engine only.
        tracing::warn!(
            profile = %metrics.profile_id,
            "dataset has no acceleration bundle; queries fall back to the exact engine. \
             Re-import the dataset to build the CCH"
        );
        return Ok(None);
    };
    if topology_source.source_topology_bundle_id != metrics.source_topology_bundle_id {
        bail!(
            "dataset acceleration bundle does not match the compiled topology bundle; re-import the dataset and recompile the profile"
        );
    }
    if !topology_source.is_current_format() {
        bail!(
            "dataset acceleration bundle uses an outdated format (algorithm '{}', schema {}); re-import the dataset",
            topology_source.algorithm,
            topology_source.schema_version
        );
    }
    if acceleration.algorithm != netweevil_core::CCH_ALGORITHM
        || acceleration.schema_version != netweevil_core::COMPILED_ACCELERATION_SCHEMA_VERSION
    {
        bail!(
            "compiled profile acceleration uses an outdated format; recompile the profile against the current dataset"
        );
    }
    if topology_source.edge_order.len() != edge_count
        || topology_source.edge_rank.len() != edge_count
        || topology_source.upward_first_out.len() != edge_count + 1
        || topology_source.downward_first_out.len() != edge_count + 1
    {
        bail!(
            "acceleration topology does not match topology edge count; re-import the dataset and recompile the profile"
        );
    }
    let upward_len = topology_source
        .upward_first_out
        .last()
        .copied()
        .unwrap_or_default() as usize;
    let downward_len = topology_source
        .downward_first_out
        .last()
        .copied()
        .unwrap_or_default() as usize;
    if topology_source.upward_head.len() != upward_len || upward_weight.len() != upward_len {
        bail!(
            "acceleration upward arrays are inconsistent; re-import the dataset and recompile the profile"
        );
    }
    if topology_source.downward_head.len() != downward_len || downward_weight.len() != downward_len
    {
        bail!(
            "acceleration downward arrays are inconsistent; re-import the dataset and recompile the profile"
        );
    }
    for (upward, downward) in [
        (
            &acceleration.time_upward_weight,
            &acceleration.time_downward_weight,
        ),
        (
            &acceleration.distance_upward_weight,
            &acceleration.distance_downward_weight,
        ),
    ] {
        let present = !upward.is_empty() || !downward.is_empty();
        if present && (upward.len() != upward_len || downward.len() != downward_len) {
            bail!("acceleration per-metric weight arrays are inconsistent; recompile the profile");
        }
    }

    let ranked = RankedCch::build(&topology_source, upward_weight, downward_weight);
    drop(weights);
    Ok(Some(AccelerationGraph {
        source: topology_source,
        metrics,
        ranked,
        deferred_metric_weights: None,
        unpack: UnpackTable::new(upward_len, downward_len),
        lower_bounds: std::sync::OnceLock::new(),
    }))
}
