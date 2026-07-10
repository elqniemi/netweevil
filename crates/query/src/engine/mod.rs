mod acceleration;
mod automaton;
mod exact;
mod fallback;
mod graph;

pub(crate) use acceleration::*;
pub(crate) use automaton::*;
pub(crate) use exact::*;
pub(crate) use fallback::*;
pub(crate) use graph::*;

use std::cmp::Ordering;
use std::sync::Arc;

use anyhow::Result;
use netweevil_core::{
    CompiledProfileBundle, DatasetAccelerationBundle, EDGE_FLAG_ROUNDABOUT,
    EDGE_FLAG_TARGET_TRAFFIC_SIGNAL, TopologyBundle,
};
use netweevil_profile::ReturnConfig;

use crate::*;

pub struct PreparedRoutingEngine {
    topology: Arc<TopologyBundle>,
    metrics: Arc<CompiledProfileBundle>,
    default_routing_graph: RoutingGraph,
    ignore_multi_edge_restrictions_graph: Option<RoutingGraph>,
}

impl PreparedRoutingEngine {
    pub fn new(
        topology: Arc<TopologyBundle>,
        metrics: Arc<CompiledProfileBundle>,
        acceleration: Option<Arc<DatasetAccelerationBundle>>,
    ) -> Result<Self> {
        validate_execution_inputs(topology.as_ref(), metrics.as_ref())?;
        let default_routing_graph = build_routing_graph_from_shared(
            topology.as_ref(),
            metrics.clone(),
            acceleration.clone(),
        )?;
        let ignore_multi_edge_restrictions_graph =
            if default_routing_graph.has_restriction_sequences() {
                Some(build_routing_graph_with_options_from_shared(
                    topology.as_ref(),
                    metrics.clone(),
                    acceleration.clone(),
                    RoutingGraphBuildOptions {
                        ignore_multi_edge_restriction_sequences: true,
                        search_time_turn_restrictions: false,
                        include_temporal_materialized_directions: false,
                    },
                )?)
            } else {
                None
            };
        Ok(Self {
            topology,
            metrics,
            default_routing_graph,
            ignore_multi_edge_restrictions_graph,
        })
    }

    pub fn topology(&self) -> &TopologyBundle {
        self.topology.as_ref()
    }

    pub fn topology_arc(&self) -> Arc<TopologyBundle> {
        self.topology.clone()
    }

    pub fn metrics(&self) -> &CompiledProfileBundle {
        self.metrics.as_ref()
    }

    pub fn metrics_arc(&self) -> Arc<CompiledProfileBundle> {
        self.metrics.clone()
    }

    pub fn execute_route(&self, request: &RouteRequest) -> Result<RouteResult> {
        self.execute_route_with_optional_edge_names(request, None, EngineMode::Auto)
    }

    pub fn execute_route_with_edge_names(
        &self,
        request: &RouteRequest,
        edge_names: &[String],
    ) -> Result<RouteResult> {
        self.execute_route_with_optional_edge_names(request, Some(edge_names), EngineMode::Auto)
    }

    pub fn execute_route_with_mode(
        &self,
        request: &RouteRequest,
        mode: EngineMode,
    ) -> Result<RouteResult> {
        self.execute_route_with_optional_edge_names(request, None, mode)
    }

    pub fn execute_route_with_edge_names_and_mode(
        &self,
        request: &RouteRequest,
        edge_names: &[String],
        mode: EngineMode,
    ) -> Result<RouteResult> {
        self.execute_route_with_optional_edge_names(request, Some(edge_names), mode)
    }

    /// Snap a point to routable candidates on the default routing graph.
    /// Lets batch callers (e.g. simulation dispatch) snap each unique
    /// endpoint once and reuse the candidates across many route executions.
    pub fn snap_route_candidates(
        &self,
        point: &LabeledPoint,
        max_distance_m: f64,
        is_origin: bool,
    ) -> Result<Vec<SnappedPoint>> {
        snap_candidates(
            self.topology.as_ref(),
            &self.default_routing_graph,
            point,
            max_distance_m,
            is_origin,
        )
    }

    /// Snap with the full 3D and semantic-filter contract used by route
    /// requests. This is useful for transit stop bindings and other callers
    /// that need reusable candidates without executing a route.
    pub fn snap_route_candidates_with_options(
        &self,
        point: &LabeledPoint,
        options: &SnapOptions,
        is_origin: bool,
    ) -> Result<Vec<SnappedPoint>> {
        snap_candidates_with_options(
            self.topology.as_ref(),
            &self.default_routing_graph,
            point,
            options,
            is_origin,
        )
    }

    /// Execute a route between pre-snapped candidate sets produced by
    /// [`Self::snap_route_candidates`]. Requests with failure modes or exact
    /// temporal/component labels fall back to the full snap-and-route path so
    /// their runtime graph and label semantics stay identical to
    /// [`Self::execute_route`].
    pub fn execute_route_between_candidates(
        &self,
        request: &RouteRequest,
        origin_candidates: &[SnappedPoint],
        destination_candidates: &[SnappedPoint],
    ) -> Result<RouteResult> {
        if has_failure_modes(&request.fallback) || request.temporal.requires_exact_labels() {
            return self.execute_route(request);
        }
        execute_route_with_candidates(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            &self.default_routing_graph,
            &request.route_id,
            request.snap.max_distance_m,
            &request.connectivity,
            &request.fallback,
            &request.returns,
            &request.alternatives,
            origin_candidates,
            destination_candidates,
            None,
        )
    }

    fn execute_route_with_optional_edge_names(
        &self,
        request: &RouteRequest,
        edge_names: Option<&[String]>,
        mode: EngineMode,
    ) -> Result<RouteResult> {
        if has_failure_modes(&request.fallback) {
            let degraded = cached_failure_mode_bundle(
                self.topology.as_ref(),
                self.metrics.as_ref(),
                &request.fallback,
                Some(&self.default_routing_graph),
            )?;
            return execute_route_with_graph(
                &degraded.topology,
                degraded.metrics.as_ref(),
                &degraded.routing_graph,
                request,
                edge_names,
            );
        }
        let (routing_graph, _) = self.routing_graph_for_mode(mode);
        execute_route_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            routing_graph,
            request,
            edge_names,
        )
    }

    pub fn execute_od(&self, document: &OdPairsDocument) -> Result<OdResult> {
        self.execute_od_with_mode(document, EngineMode::Auto)
    }

    pub fn execute_od_with_mode(
        &self,
        document: &OdPairsDocument,
        mode: EngineMode,
    ) -> Result<OdResult> {
        if has_failure_modes(&document.fallback) {
            let degraded = cached_failure_mode_bundle(
                self.topology.as_ref(),
                self.metrics.as_ref(),
                &document.fallback,
                Some(&self.default_routing_graph),
            )?;
            return execute_od_with_graph(
                &degraded.topology,
                degraded.metrics.as_ref(),
                &degraded.routing_graph,
                document,
            );
        }
        let (routing_graph, _) = self.routing_graph_for_mode(mode);
        execute_od_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            routing_graph,
            document,
        )
    }

    pub fn execute_matrix(
        &self,
        origins: &PointSetDocument,
        destinations: &PointSetDocument,
    ) -> Result<MatrixResult> {
        self.execute_matrix_with_mode(origins, destinations, EngineMode::Auto)
    }

    pub fn execute_matrix_with_mode(
        &self,
        origins: &PointSetDocument,
        destinations: &PointSetDocument,
        mode: EngineMode,
    ) -> Result<MatrixResult> {
        let fallback = merge_point_set_fallback_policy(&origins.fallback, &destinations.fallback);
        if has_failure_modes(&fallback) {
            let degraded = cached_failure_mode_bundle(
                self.topology.as_ref(),
                self.metrics.as_ref(),
                &fallback,
                Some(&self.default_routing_graph),
            )?;
            return execute_matrix_with_graph(
                &degraded.topology,
                degraded.metrics.as_ref(),
                &degraded.routing_graph,
                origins,
                destinations,
            );
        }
        let (routing_graph, _) = self.routing_graph_for_mode(mode);
        execute_matrix_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            routing_graph,
            origins,
            destinations,
        )
    }

    pub fn execute_accessibility(
        &self,
        request: &AccessibilityRequest,
    ) -> Result<AccessibilityResult> {
        self.execute_accessibility_with_mode(request, EngineMode::Auto)
    }

    pub fn execute_accessibility_with_mode(
        &self,
        request: &AccessibilityRequest,
        mode: EngineMode,
    ) -> Result<AccessibilityResult> {
        if has_failure_modes(&request.origins.fallback) {
            let degraded = cached_failure_mode_bundle(
                self.topology.as_ref(),
                self.metrics.as_ref(),
                &request.origins.fallback,
                Some(&self.default_routing_graph),
            )?;
            return execute_accessibility_with_graph(
                &degraded.topology,
                degraded.metrics.as_ref(),
                &degraded.routing_graph,
                request,
            );
        }
        let (routing_graph, _) = self.routing_graph_for_mode(mode);
        execute_accessibility_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            routing_graph,
            request,
        )
    }

    pub fn execute_service_area(&self, request: &ServiceAreaRequest) -> Result<ServiceAreaResult> {
        self.execute_service_area_with_mode(request, EngineMode::Auto)
    }

    pub fn execute_service_area_sequence(
        &self,
        request: &ServiceAreaSequenceRequest,
    ) -> Result<ServiceAreaSequenceResult> {
        execute_service_area_sequence_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            &self.default_routing_graph,
            request,
        )
    }

    pub fn execute_betweenness(&self, request: &BetweennessRequest) -> Result<BetweennessResult> {
        execute_betweenness_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            &self.default_routing_graph,
            request,
            false,
        )
    }

    pub(crate) fn execute_betweenness_with_pair_results(
        &self,
        request: &BetweennessRequest,
    ) -> Result<BetweennessResult> {
        execute_betweenness_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            &self.default_routing_graph,
            request,
            true,
        )
    }

    pub fn execute_service_area_with_mode(
        &self,
        request: &ServiceAreaRequest,
        mode: EngineMode,
    ) -> Result<ServiceAreaResult> {
        let (routing_graph, _) = self.routing_graph_for_mode(mode);
        execute_service_area_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            routing_graph,
            request,
        )
    }

    pub fn effective_engine_description(&self, mode: EngineMode) -> EffectiveEngineDescription {
        self.routing_graph_for_mode(mode).1
    }

    /// Describe the engine that a particular route request will actually
    /// execute. Temporal and component-label requests intentionally bypass
    /// static CCH acceleration even when it is available on the prepared
    /// graph.
    pub fn effective_route_engine_description(
        &self,
        request: &RouteRequest,
        mode: EngineMode,
    ) -> EffectiveEngineDescription {
        let mut description = self.effective_engine_description(mode);
        if !request.temporal.constraints.is_empty() || request.temporal.pareto.is_some() {
            description.route_engine = "component_nondominated_label_setting";
            description.acceleration = "spatial_index+edge_phantoms+turn_automaton";
        } else if request.temporal.is_temporal() {
            description.route_engine = "time_dependent_nondominated_label_setting";
            description.acceleration = "spatial_index+edge_phantoms+turn_automaton";
        }
        description
    }

    fn routing_graph_for_mode(
        &self,
        mode: EngineMode,
    ) -> (&RoutingGraph, EffectiveEngineDescription) {
        match mode {
            EngineMode::Auto => (
                &self.default_routing_graph,
                effective_engine_description(&self.default_routing_graph),
            ),
            EngineMode::IgnoreMultiEdgeRestrictions => {
                let graph = self
                    .ignore_multi_edge_restrictions_graph
                    .as_ref()
                    .unwrap_or(&self.default_routing_graph);
                (graph, effective_engine_description(graph))
            }
        }
    }
}

fn effective_engine_description(routing_graph: &RoutingGraph) -> EffectiveEngineDescription {
    if routing_graph.has_restriction_sequences() {
        if routing_graph.acceleration.is_some() {
            EffectiveEngineDescription {
                route_engine: "cch_with_restriction_sequence_validation",
                batch_engine: "cch_with_restriction_sequence_validation_batch_reuse",
                acceleration: "spatial_index+edge_phantoms+cch+turn_automaton_fallback",
            }
        } else {
            EffectiveEngineDescription {
                route_engine: "astar_exact_multi_edge_turns",
                batch_engine: "astar_exact_multi_edge_turns_batch_reuse",
                acceleration: "spatial_index+a_star+turn_automaton",
            }
        }
    } else if routing_graph.acceleration.is_some() {
        EffectiveEngineDescription {
            route_engine: "accelerated_pairwise_turns",
            batch_engine: "accelerated_pairwise_turns_batch_reuse",
            acceleration: "spatial_index+edge_phantoms+cch",
        }
    } else {
        EffectiveEngineDescription {
            route_engine: "bidirectional_exact_pairwise_turns",
            batch_engine: "bidirectional_exact_pairwise_turns_batch_reuse",
            acceleration: "spatial_index+edge_phantoms",
        }
    }
}

pub(crate) fn finalize_route_path(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    edge_indexes: Vec<usize>,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> RoutePath {
    let mut total_generalized_cost = 0.0;
    let mut previous_edge_index = None;
    let first_edge = edge_indexes.first().copied();
    let last_edge = edge_indexes.last().copied();
    for &edge_index in &edge_indexes {
        let metric = &metrics.edge_metrics[edge_index];
        let factor = edge_traversal_factor(edge_index, first_edge, last_edge, origin, destination);
        total_generalized_cost += metric.generalized_cost.unwrap_or_default() * factor;
        if let Some(previous_edge_index) = previous_edge_index {
            total_generalized_cost +=
                turn_penalty_seconds(topology, metrics, previous_edge_index, edge_index)
                    * metrics.turn_costs.cost_time_weight;
        }
        previous_edge_index = Some(edge_index);
    }

    RoutePath {
        edge_indexes,
        total_generalized_cost,
    }
}

pub(crate) fn edge_traversal_factor(
    edge_index: usize,
    first_edge: Option<usize>,
    last_edge: Option<usize>,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> f64 {
    let mut start_factor = 1.0;
    let mut end_factor = 1.0;
    if first_edge == Some(edge_index)
        && origin.snapped_edge_id == Some(edge_index as u32)
        && origin.snapped_edge_fraction.is_some()
    {
        start_factor = 1.0 - origin.snapped_edge_fraction.unwrap_or_default();
    }
    if last_edge == Some(edge_index)
        && destination.snapped_edge_id == Some(edge_index as u32)
        && destination.snapped_edge_fraction.is_some()
    {
        end_factor = destination.snapped_edge_fraction.unwrap_or(1.0);
    }
    if first_edge == Some(edge_index)
        && last_edge == Some(edge_index)
        && origin.snapped_edge_id == Some(edge_index as u32)
        && destination.snapped_edge_id == Some(edge_index as u32)
    {
        return (destination.snapped_edge_fraction.unwrap_or(1.0)
            - origin.snapped_edge_fraction.unwrap_or_default())
        .clamp(0.0, 1.0);
    }
    start_factor.min(end_factor)
}

pub(crate) fn build_route_geometry(
    topology: &TopologyBundle,
    edge_indexes: &[usize],
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> Vec<[f64; 3]> {
    let mut geometry = Vec::with_capacity(edge_indexes.len() + 2);
    geometry.push([origin.snapped_lon, origin.snapped_lat, origin.snapped_z]);
    for &edge_index in edge_indexes {
        let node = &topology.nodes[topology.routing_edge(edge_index).to.0 as usize];
        geometry.push([
            node.lon,
            node.lat,
            if node.z.is_finite() { node.z } else { 0.0 },
        ]);
    }
    if geometry.last().is_none_or(|point| {
        point[0] != destination.snapped_lon || point[1] != destination.snapped_lat
    }) {
        geometry.push([
            destination.snapped_lon,
            destination.snapped_lat,
            destination.snapped_z,
        ]);
    }
    geometry
}

pub(crate) fn execution_warnings(metrics: &CompiledProfileBundle) -> Vec<String> {
    let _ = metrics;
    Vec::new()
}

pub(crate) fn turn_penalty_cost(
    metrics: &CompiledProfileBundle,
    topology: &TopologyBundle,
    previous_edge_index: usize,
    next_edge_index: usize,
) -> f64 {
    turn_penalty_seconds(topology, metrics, previous_edge_index, next_edge_index)
        * metrics.turn_costs.cost_time_weight
}

pub(crate) fn turn_penalty_seconds(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    previous_edge_index: usize,
    next_edge_index: usize,
) -> f64 {
    let previous = topology.routing_edge(previous_edge_index);
    let next = topology.routing_edge(next_edge_index);

    if previous.to != next.from {
        return 0.0;
    }

    let mut penalty_s = 0.0;
    if previous.flags & EDGE_FLAG_TARGET_TRAFFIC_SIGNAL != 0 {
        penalty_s += metrics.turn_costs.traffic_signal_penalty_s;
    }
    if previous.flags & EDGE_FLAG_ROUNDABOUT == 0 && next.flags & EDGE_FLAG_ROUNDABOUT != 0 {
        penalty_s += metrics.turn_costs.roundabout_entry_penalty_s;
    }

    if previous.from == next.to {
        return penalty_s + metrics.turn_costs.uturn_penalty_s;
    }

    if previous.source_way_id == next.source_way_id {
        return penalty_s;
    }

    penalty_s
        + match classify_turn(topology, previous_edge_index, next_edge_index) {
            TurnDirection::Straight => 0.0,
            TurnDirection::Left => metrics.turn_costs.left_penalty_s,
            TurnDirection::Right => metrics.turn_costs.right_penalty_s,
            TurnDirection::Uturn => metrics.turn_costs.uturn_penalty_s,
        }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnDirection {
    Straight,
    Left,
    Right,
    Uturn,
}

pub(crate) fn classify_turn(
    topology: &TopologyBundle,
    previous_edge_index: usize,
    next_edge_index: usize,
) -> TurnDirection {
    const STRAIGHT_THRESHOLD_RAD: f64 = 30.0_f64.to_radians();
    const UTURN_THRESHOLD_RAD: f64 = 150.0_f64.to_radians();

    let previous = topology.routing_edge(previous_edge_index);
    let next = topology.routing_edge(next_edge_index);
    let from = &topology.nodes[previous.from.0 as usize];
    let via = &topology.nodes[previous.to.0 as usize];
    let to = &topology.nodes[next.to.0 as usize];

    let in_x = projected_delta_x(from.lon, via.lat, via.lon);
    let in_y = projected_delta_y(from.lat, via.lat);
    let out_x = projected_delta_x(via.lon, via.lat, to.lon);
    let out_y = projected_delta_y(via.lat, to.lat);

    let in_norm = (in_x * in_x + in_y * in_y).sqrt();
    let out_norm = (out_x * out_x + out_y * out_y).sqrt();
    if in_norm <= f64::EPSILON || out_norm <= f64::EPSILON {
        return TurnDirection::Straight;
    }

    let dot = ((in_x * out_x + in_y * out_y) / (in_norm * out_norm)).clamp(-1.0, 1.0);
    let cross = in_x * out_y - in_y * out_x;
    let angle = cross.atan2(dot);
    let abs_angle = angle.abs();

    if abs_angle <= STRAIGHT_THRESHOLD_RAD {
        TurnDirection::Straight
    } else if abs_angle >= UTURN_THRESHOLD_RAD {
        TurnDirection::Uturn
    } else if angle > 0.0 {
        TurnDirection::Left
    } else {
        TurnDirection::Right
    }
}

pub(crate) fn build_breakdowns(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    edge_indexes: &[usize],
    returns: &ReturnConfig,
) -> Option<RouteBreakdowns> {
    let want_road = !returns.road_type_breakdown.is_empty();
    let want_surface = !returns.surface_breakdown.is_empty();
    if !want_road && !want_surface {
        return None;
    }

    let mut breakdowns = RouteBreakdowns::default();
    for &edge_index in edge_indexes {
        let edge = topology.edge(edge_index);
        let metric = &metrics.edge_metrics[edge_index];

        if want_road {
            let entry = breakdowns
                .road_class
                .entry(format!("{:?}", edge.road_class).to_lowercase())
                .or_default();
            fill_metric_breakdown(
                entry,
                edge.length_m as u64,
                metric.travel_time_s.unwrap_or_default(),
                returns,
                true,
            );
        }
        if want_surface {
            let entry = breakdowns
                .surface
                .entry(format!("{:?}", edge.surface).to_lowercase())
                .or_default();
            fill_metric_breakdown(
                entry,
                edge.length_m as u64,
                metric.travel_time_s.unwrap_or_default(),
                returns,
                false,
            );
        }
    }

    Some(breakdowns)
}

fn fill_metric_breakdown(
    entry: &mut MetricBreakdown,
    distance_m: u64,
    time_s: f64,
    returns: &ReturnConfig,
    road: bool,
) {
    let metrics = if road {
        &returns.road_type_breakdown
    } else {
        &returns.surface_breakdown
    };
    for metric in metrics {
        match metric {
            netweevil_profile::BreakdownMetric::DistanceM => {
                *entry.distance_m.get_or_insert(0) += distance_m;
            }
            netweevil_profile::BreakdownMetric::TimeS => {
                *entry.time_s.get_or_insert(0.0) += time_s;
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct State {
    pub(crate) edge_index: usize,
    pub(crate) automaton_state: usize,
    pub(crate) cost: f64,
    pub(crate) score: f64,
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        self.edge_index == other.edge_index
            && self.automaton_state == other.automaton_state
            && self.score.to_bits() == other.score.to_bits()
    }
}

impl Eq for State {}

impl PartialOrd for State {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| self.cost.total_cmp(&other.cost))
            .then_with(|| self.edge_index.cmp(&other.edge_index))
            .then_with(|| self.automaton_state.cmp(&other.automaton_state))
    }
}
