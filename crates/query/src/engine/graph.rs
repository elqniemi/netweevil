use std::sync::Arc;

use anyhow::{Context, Result, bail};
use netweevil_core::{
    CompiledProfileBundle, DatasetAccelerationBundle, EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION,
    TopologyBundle,
};

use crate::*;

#[derive(Clone)]
pub(crate) struct RoutePath {
    pub(crate) edge_indexes: Vec<usize>,
    pub(crate) total_generalized_cost: f64,
}

pub(crate) struct RoutingGraph {
    first_out: Vec<u32>,
    edge_order: Vec<u32>,
    incoming_first_out: Vec<u32>,
    incoming_edge_order: Vec<u32>,
    pub(crate) edge_costs: Vec<f64>,
    transition_first_out: Vec<u32>,
    pub(crate) transition_edges: Vec<u32>,
    pub(crate) transition_costs: Vec<f64>,
    reverse_transition_first_out: Vec<u32>,
    pub(crate) reverse_transition_edges: Vec<u32>,
    pub(crate) reverse_transition_costs: Vec<f64>,
    pub(crate) acceleration: Option<AccelerationGraph>,
    /// True when the attached acceleration weights were customized with the
    /// request's failure-mode penalties baked in (degraded graphs only);
    /// plain acceleration weights must never serve failure-mode searches.
    pub(crate) acceleration_includes_failure_penalties: bool,
    pub(crate) automaton: RestrictionAutomaton,
    pub(crate) reverse_automaton: RestrictionAutomaton,
    pub(crate) virtual_reverse_of: Vec<Option<usize>>,
    pub(crate) includes_temporal_materialized_directions: bool,
}

impl RoutingGraph {
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
    pub(crate) metrics: Arc<CompiledProfileBundle>,
    pub(crate) upward_tail: Vec<u32>,
    pub(crate) downward_tail: Vec<u32>,
    pub(crate) reverse_downward_first_out: Vec<u32>,
    pub(crate) reverse_downward_edge: Vec<u32>,
    pub(crate) reverse_downward_arc: Vec<u32>,
}

impl AccelerationGraph {
    /// Customized weight sets for a service-area metric over the hierarchy
    /// arcs, or `None` when the compiled profile carries no arc-aligned
    /// weights for that metric. Callers fall back to Dijkstra on `None`.
    pub(crate) fn metric_weights(
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

#[allow(clippy::needless_range_loop)]
pub(crate) fn build_routing_graph_with_options_from_shared(
    topology: &TopologyBundle,
    metrics: Arc<CompiledProfileBundle>,
    acceleration: Option<Arc<DatasetAccelerationBundle>>,
    options: RoutingGraphBuildOptions,
) -> Result<RoutingGraph> {
    let edge_count = topology.edge_count();
    let mut out_degree = vec![0_u32; topology.nodes.len()];
    let mut in_degree = vec![0_u32; topology.nodes.len()];
    let mut edge_costs = vec![f64::INFINITY; edge_count];

    for edge_index in 0..edge_count {
        let edge = topology.routing_edge(edge_index);
        let metric = &metrics.edge_metrics[edge_index];
        if edge.flags & EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION != 0
            && !options.include_temporal_materialized_directions
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

    let mode_bit = metrics.mode.access_bit();
    let mut pairwise_forbidden = Vec::<(u32, u32)>::new();
    let mut restricted_sequences = Vec::new();
    for restriction in topology
        .turn_restrictions
        .iter()
        .filter(|restriction| restriction.mode_mask.contains(mode_bit))
    {
        if restriction.edge_path.len() < 2 {
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

    Ok(RoutingGraph {
        first_out,
        edge_order,
        incoming_first_out,
        incoming_edge_order,
        edge_costs,
        transition_first_out,
        transition_edges,
        transition_costs,
        reverse_transition_first_out,
        reverse_transition_edges,
        reverse_transition_costs,
        // Temporal-inclusive graphs intentionally run exact searches: their
        // scenario-only directions are absent from the static CCH weights.
        // Do not invoke the acceleration loader here, because a missing
        // dataset bundle is expected and warning that the user must re-import
        // would be misleading.
        acceleration: if options.include_temporal_materialized_directions {
            None
        } else {
            build_acceleration_graph(metrics, acceleration, edge_count)?
        },
        acceleration_includes_failure_penalties: false,
        automaton: RestrictionAutomaton::build(&restricted_sequences),
        reverse_automaton: RestrictionAutomaton::build(&reverse_restricted_sequences),
        virtual_reverse_of: vec![None; edge_count],
        includes_temporal_materialized_directions: options.include_temporal_materialized_directions,
    })
}

#[allow(clippy::needless_range_loop)]
pub(crate) fn build_acceleration_graph(
    metrics: Arc<CompiledProfileBundle>,
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
    if acceleration
        .upward_weight
        .contains(&netweevil_core::CCH_WEIGHT_OVERFLOW)
        || acceleration
            .downward_weight
            .contains(&netweevil_core::CCH_WEIGHT_OVERFLOW)
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
    if topology_source.upward_head.len() != upward_len
        || acceleration.upward_weight.len() != upward_len
    {
        bail!(
            "acceleration upward arrays are inconsistent; re-import the dataset and recompile the profile"
        );
    }
    if topology_source.downward_head.len() != downward_len
        || acceleration.downward_weight.len() != downward_len
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

    let mut reverse_downward_first_out = vec![0_u32; edge_count + 1];
    for &next_edge in &topology_source.downward_head {
        reverse_downward_first_out[next_edge as usize + 1] += 1;
    }
    for edge_index in 0..edge_count {
        reverse_downward_first_out[edge_index + 1] += reverse_downward_first_out[edge_index];
    }
    let mut reverse_downward_edge = vec![0_u32; downward_len];
    let mut reverse_downward_arc = vec![0_u32; downward_len];
    let mut downward_tail = vec![0_u32; downward_len];
    let mut write_positions = reverse_downward_first_out[..edge_count].to_vec();
    for edge_index in 0..edge_count {
        for slot in topology_source.downward_first_out[edge_index] as usize
            ..topology_source.downward_first_out[edge_index + 1] as usize
        {
            downward_tail[slot] = edge_index as u32;
            let next_edge = topology_source.downward_head[slot] as usize;
            let write_index = &mut write_positions[next_edge];
            let target_slot = *write_index as usize;
            reverse_downward_edge[target_slot] = edge_index as u32;
            reverse_downward_arc[target_slot] = slot as u32;
            *write_index += 1;
        }
    }

    let mut upward_tail = vec![0_u32; upward_len];
    for edge_index in 0..edge_count {
        for slot in topology_source.upward_first_out[edge_index] as usize
            ..topology_source.upward_first_out[edge_index + 1] as usize
        {
            upward_tail[slot] = edge_index as u32;
        }
    }
    Ok(Some(AccelerationGraph {
        source: topology_source,
        metrics,
        upward_tail,
        downward_tail,
        reverse_downward_first_out,
        reverse_downward_edge,
        reverse_downward_arc,
    }))
}
