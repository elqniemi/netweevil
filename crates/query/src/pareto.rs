use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use anyhow::{Result, bail};
use netweevil_core::{CompiledProfileBundle, TopologyBundle};
use netweevil_profile::ReturnGeometry;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::*;

#[derive(Debug, Clone)]
struct ComponentLabel {
    state: SearchStateKey,
    generalized_cost: f64,
    component_values: Vec<f64>,
    previous: Option<usize>,
    edge_index: usize,
    active: bool,
}

#[derive(Debug, Clone, Copy)]
struct HeapEntry {
    label_id: usize,
    cost: f64,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cost.to_bits() == other.cost.to_bits() && self.label_id == other.label_id
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .total_cmp(&self.cost)
            .then_with(|| other.label_id.cmp(&self.label_id))
    }
}

#[derive(Debug, Clone)]
struct ComponentPath {
    origin: SnappedPoint,
    destination: SnappedPoint,
    hop_info: HopSelectionInfo,
    edge_indexes: Vec<usize>,
    generalized_cost: f64,
    component_values: Vec<f64>,
}

#[derive(Debug, Clone)]
struct TemporalComponentTraversal {
    edge_index: usize,
    fraction: f64,
    turn_time_s: f64,
    cost: crate::temporal::EdgeTemporalCost,
}

#[derive(Debug, Clone)]
struct TemporalComponentLabel {
    state: SearchStateKey,
    arrival: OffsetDateTime,
    generalized_cost: f64,
    component_values: Vec<f64>,
    previous: Option<usize>,
    traversal: TemporalComponentTraversal,
    active: bool,
}

#[derive(Debug, Clone)]
struct TemporalComponentPath {
    origin: SnappedPoint,
    destination: SnappedPoint,
    hop_info: HopSelectionInfo,
    departure: OffsetDateTime,
    arrival: OffsetDateTime,
    generalized_cost: f64,
    component_values: Vec<f64>,
    traversals: Vec<TemporalComponentTraversal>,
}

pub(crate) fn execute_component_route_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &RouteRequest,
    edge_names: Option<&[String]>,
) -> Result<RouteResult> {
    if request.fallback != FallbackPolicy::default() {
        bail!("component constraints/Pareto currently require the strict fallback policy");
    }
    let constraints = request
        .temporal
        .constraints
        .iter()
        .map(|constraint| {
            if !constraint.max_value.is_finite() || constraint.max_value < 0.0 {
                bail!(
                    "component constraint '{}' must have a finite non-negative max_value",
                    constraint.component
                );
            }
            let index = component_index(metrics, &constraint.component)?;
            Ok((index, constraint.max_value))
        })
        .collect::<Result<Vec<_>>>()?;
    let pareto_index = request
        .temporal
        .pareto
        .as_ref()
        .map(|pareto| component_index(metrics, &pareto.component))
        .transpose()?;
    let max_labels = request
        .temporal
        .pareto
        .as_ref()
        .map(|pareto| pareto.max_labels_per_state)
        .unwrap_or(request.temporal.max_labels_per_state);
    if max_labels == 0 {
        bail!("component label limit must be greater than zero");
    }
    let dominance_dimensions = {
        let mut dimensions = constraints
            .iter()
            .map(|(index, _)| *index)
            .collect::<Vec<_>>();
        if let Some(index) = pareto_index {
            dimensions.push(index);
        }
        dimensions.sort_unstable();
        dimensions.dedup();
        dimensions
    };

    if request.temporal.is_temporal() {
        return execute_temporal_component_route(
            topology,
            metrics,
            routing_graph,
            request,
            edge_names,
            &constraints,
            &dominance_dimensions,
            pareto_index,
            max_labels,
        );
    }

    let search_distance_m = request
        .connectivity
        .max_hop_distance_m
        .unwrap_or(request.snap.max_distance_m)
        .max(request.snap.max_distance_m);
    let search_snap = SnapOptions {
        max_distance_m: search_distance_m,
        ..request.snap.clone()
    };
    let origin_candidates =
        snap_candidates_with_options(topology, routing_graph, &request.origin, &search_snap, true)?;
    let destination_candidates = snap_candidates_with_options(
        topology,
        routing_graph,
        &request.destination,
        &search_snap,
        false,
    )?;
    let mut paths = Vec::new();
    for origin in &origin_candidates {
        for destination in &destination_candidates {
            let Some(hop_info) = hop_info_for_pair(
                origin,
                destination,
                request.snap.max_distance_m,
                &request.connectivity,
            ) else {
                continue;
            };
            for (edge_indexes, generalized_cost, component_values) in component_paths_for_pair(
                topology,
                metrics,
                routing_graph,
                origin,
                destination,
                &constraints,
                &dominance_dimensions,
                pareto_index,
                max_labels,
            )? {
                paths.push(ComponentPath {
                    origin: origin.clone(),
                    destination: destination.clone(),
                    hop_info: hop_info.clone(),
                    edge_indexes,
                    generalized_cost,
                    component_values,
                });
            }
        }
    }
    retain_global_frontier(&mut paths, pareto_index);
    if paths.is_empty() {
        return Err(anyhow::Error::new(no_route_failure(
            topology,
            &origin_candidates,
            &destination_candidates,
        )));
    }
    paths.sort_by(|left, right| left.generalized_cost.total_cmp(&right.generalized_cost));
    let max_routes = request
        .temporal
        .pareto
        .as_ref()
        .map(|pareto| pareto.max_routes.max(1))
        .unwrap_or(1);
    paths.truncate(max_routes);
    build_component_route_result(topology, metrics, routing_graph, request, paths, edge_names)
}

fn component_index(metrics: &CompiledProfileBundle, name: &str) -> Result<usize> {
    metrics
        .components
        .iter()
        .position(|component| component.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown cost component '{}'; compiled components are {}",
                name,
                metrics
                    .components
                    .iter()
                    .map(|component| component.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

#[allow(clippy::too_many_arguments)]
fn execute_temporal_component_route(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &RouteRequest,
    edge_names: Option<&[String]>,
    constraints: &[(usize, f64)],
    dominance_dimensions: &[usize],
    pareto_index: Option<usize>,
    max_labels: usize,
) -> Result<RouteResult> {
    if request.temporal.departure_time.is_some() && request.temporal.arrive_by.is_some() {
        bail!("route request must set only one of departure_time or arrive_by");
    }
    let context = crate::temporal::context_from_request(&request.temporal)?;
    crate::temporal::validate_speed_factor_wait_policy(topology, metrics, &context)?;
    let temporal_routing_graph;
    let routing_graph = if routing_graph.includes_temporal_materialized_directions {
        routing_graph
    } else {
        temporal_routing_graph = crate::build_temporal_routing_graph(topology, metrics)?;
        &temporal_routing_graph
    };
    let search_distance_m = request
        .connectivity
        .max_hop_distance_m
        .unwrap_or(request.snap.max_distance_m)
        .max(request.snap.max_distance_m);
    let search_snap = SnapOptions {
        max_distance_m: search_distance_m,
        ..request.snap.clone()
    };
    let origin_candidates =
        snap_candidates_with_options(topology, routing_graph, &request.origin, &search_snap, true)?;
    let destination_candidates = snap_candidates_with_options(
        topology,
        routing_graph,
        &request.destination,
        &search_snap,
        false,
    )?;
    let mut paths = if let Some(departure) = request.temporal.departure_time.as_deref() {
        temporal_component_paths_between_candidates(
            topology,
            metrics,
            routing_graph,
            request,
            &origin_candidates,
            &destination_candidates,
            crate::parse_datetime(departure)?,
            constraints,
            dominance_dimensions,
            pareto_index,
            max_labels,
            &context,
        )?
    } else if let Some(arrive_by) = request.temporal.arrive_by.as_deref() {
        arrive_by_component_paths(
            topology,
            metrics,
            routing_graph,
            request,
            &origin_candidates,
            &destination_candidates,
            crate::parse_datetime(arrive_by)?,
            constraints,
            dominance_dimensions,
            pareto_index,
            max_labels,
            &context,
        )?
    } else {
        bail!("a scenario or temporal overlay requires departure_time or arrive_by");
    };
    retain_temporal_global_frontier(&mut paths, pareto_index);
    if paths.is_empty() {
        return Err(anyhow::Error::new(no_route_failure(
            topology,
            &origin_candidates,
            &destination_candidates,
        )));
    }
    paths.sort_by(|left, right| left.generalized_cost.total_cmp(&right.generalized_cost));
    let max_routes = request
        .temporal
        .pareto
        .as_ref()
        .map(|pareto| pareto.max_routes.max(1))
        .unwrap_or(1);
    paths.truncate(max_routes);
    build_temporal_component_route_result(topology, metrics, request, &context, paths, edge_names)
}

#[allow(clippy::too_many_arguments)]
fn arrive_by_component_paths(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &RouteRequest,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    deadline: OffsetDateTime,
    constraints: &[(usize, f64)],
    dominance_dimensions: &[usize],
    pareto_index: Option<usize>,
    max_labels: usize,
    context: &TemporalContext,
) -> Result<Vec<TemporalComponentPath>> {
    let lookback_s = request.temporal.arrive_by_lookback_s;
    if !lookback_s.is_finite() || lookback_s <= 0.0 {
        bail!("arrive_by_lookback_s must be finite and greater than zero");
    }
    let departures = crate::temporal::exact_arrive_by_departure_candidates(
        topology,
        metrics,
        routing_graph,
        &request.connectivity,
        request.snap.max_distance_m,
        origin_candidates,
        destination_candidates,
        deadline,
        lookback_s,
        max_labels,
        context,
    )?;
    for departure in departures {
        let paths = temporal_component_paths_arriving_by(
            topology,
            metrics,
            routing_graph,
            request,
            origin_candidates,
            destination_candidates,
            departure,
            deadline,
            constraints,
            dominance_dimensions,
            pareto_index,
            max_labels,
            context,
        )?;
        if !paths.is_empty() {
            return Ok(paths);
        }
    }
    Ok(Vec::new())
}

#[allow(clippy::too_many_arguments)]
fn temporal_component_paths_arriving_by(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &RouteRequest,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    departure: OffsetDateTime,
    deadline: OffsetDateTime,
    constraints: &[(usize, f64)],
    dominance_dimensions: &[usize],
    pareto_index: Option<usize>,
    max_labels: usize,
    context: &TemporalContext,
) -> Result<Vec<TemporalComponentPath>> {
    let mut paths = temporal_component_paths_between_candidates(
        topology,
        metrics,
        routing_graph,
        request,
        origin_candidates,
        destination_candidates,
        departure,
        constraints,
        dominance_dimensions,
        pareto_index,
        max_labels,
        context,
    )?;
    paths.retain(|path| path.arrival <= deadline);
    Ok(paths)
}

#[allow(clippy::too_many_arguments)]
fn temporal_component_paths_between_candidates(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &RouteRequest,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    departure: OffsetDateTime,
    constraints: &[(usize, f64)],
    dominance_dimensions: &[usize],
    pareto_index: Option<usize>,
    max_labels: usize,
    context: &TemporalContext,
) -> Result<Vec<TemporalComponentPath>> {
    let mut paths = Vec::new();
    for origin in origin_candidates {
        for destination in destination_candidates {
            let Some(hop_info) = hop_info_for_pair(
                origin,
                destination,
                request.snap.max_distance_m,
                &request.connectivity,
            ) else {
                continue;
            };
            for (generalized_cost, arrival, component_values, traversals) in
                temporal_component_paths_for_pair(
                    topology,
                    metrics,
                    routing_graph,
                    origin,
                    destination,
                    departure,
                    constraints,
                    dominance_dimensions,
                    pareto_index,
                    max_labels,
                    context,
                )?
            {
                paths.push(TemporalComponentPath {
                    origin: origin.clone(),
                    destination: destination.clone(),
                    hop_info: hop_info.clone(),
                    departure,
                    arrival,
                    generalized_cost,
                    component_values,
                    traversals,
                });
            }
        }
    }
    Ok(paths)
}

#[allow(clippy::too_many_arguments)]
fn temporal_component_paths_for_pair(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
    departure: OffsetDateTime,
    constraints: &[(usize, f64)],
    dominance_dimensions: &[usize],
    pareto_index: Option<usize>,
    max_labels: usize,
    context: &TemporalContext,
) -> Result<
    Vec<(
        f64,
        OffsetDateTime,
        Vec<f64>,
        Vec<TemporalComponentTraversal>,
    )>,
> {
    if origin.snapped_edge_id.is_none()
        && destination.snapped_edge_id.is_none()
        && origin.snapped_node_id == destination.snapped_node_id
    {
        return Ok(vec![(
            0.0,
            departure,
            vec![0.0; metrics.components.len()],
            Vec::new(),
        )]);
    }
    if let (
        Some(origin_edge),
        Some(origin_fraction),
        Some(destination_edge),
        Some(destination_fraction),
    ) = (
        origin.snapped_edge_id,
        origin.snapped_edge_fraction,
        destination.snapped_edge_id,
        destination.snapped_edge_fraction,
    ) && origin_edge == destination_edge
        && origin_fraction <= destination_fraction
    {
        let edge_index = origin_edge as usize;
        let fraction = destination_fraction - origin_fraction;
        let Some(cost) = crate::temporal::evaluate_edge_at(
            topology, metrics, context, edge_index, departure, fraction,
        ) else {
            return Ok(Vec::new());
        };
        let component_values = temporal_component_vector(metrics, &cost);
        if !within_constraints(&component_values, constraints) {
            return Ok(Vec::new());
        }
        return Ok(vec![(
            cost.generalized_cost,
            cost.exit_time,
            component_values,
            vec![TemporalComponentTraversal {
                edge_index,
                fraction,
                turn_time_s: 0.0,
                cost,
            }],
        )]);
    }

    let mut labels = Vec::<TemporalComponentLabel>::new();
    let mut state_labels = HashMap::<SearchStateKey, Vec<usize>>::new();
    let mut heap = BinaryHeap::new();
    let mut targets = Vec::new();
    let seeds = if let (Some(edge_id), Some(fraction)) =
        (origin.snapped_edge_id, origin.snapped_edge_fraction)
    {
        vec![(edge_id as usize, 1.0 - fraction)]
    } else {
        routing_graph
            .outgoing_edges(origin.snapped_node_id as usize)
            .iter()
            .map(|edge_index| (*edge_index as usize, 1.0))
            .collect()
    };
    for (edge_index, full_fraction) in seeds {
        let target_fraction = destination_fraction_for_edge(topology, destination, edge_index);
        let fraction = target_fraction.unwrap_or(full_fraction);
        let Some(cost) = crate::temporal::evaluate_edge_at(
            topology, metrics, context, edge_index, departure, fraction,
        ) else {
            continue;
        };
        let component_values = temporal_component_vector(metrics, &cost);
        if !within_constraints(&component_values, constraints) {
            continue;
        }
        let generalized_cost = cost.generalized_cost;
        let label_id = labels.len();
        labels.push(TemporalComponentLabel {
            state: SearchStateKey {
                edge_index,
                automaton_state: routing_graph.automaton.transition(0, edge_index),
            },
            arrival: cost.exit_time,
            generalized_cost,
            component_values,
            previous: None,
            traversal: TemporalComponentTraversal {
                edge_index,
                fraction,
                turn_time_s: 0.0,
                cost,
            },
            active: true,
        });
        if target_fraction.is_some() {
            targets.push(label_id);
        } else if insert_temporal_component_label(
            &mut labels,
            &mut state_labels,
            label_id,
            dominance_dimensions,
            max_labels,
        )? {
            heap.push(HeapEntry {
                label_id,
                cost: generalized_cost,
            });
        }
    }

    while let Some(entry) = heap.pop() {
        if !labels[entry.label_id].active || entry.cost > labels[entry.label_id].generalized_cost {
            continue;
        }
        let current = labels[entry.label_id].state;
        let current_arrival = labels[entry.label_id].arrival;
        for transition_index in routing_graph.transition_range(current.edge_index) {
            let next_edge = routing_graph.transition_edges[transition_index] as usize;
            if routing_graph
                .automaton
                .prohibited_sequence_len(current.automaton_state, next_edge)
                .is_some()
            {
                continue;
            }
            let turn_time_s =
                turn_penalty_seconds(topology, metrics, current.edge_index, next_edge);
            let requested_entry = current_arrival + Duration::seconds_f64(turn_time_s);
            let target_fraction = destination_fraction_for_edge(topology, destination, next_edge);
            let fraction = target_fraction.unwrap_or(1.0);
            let Some(cost) = crate::temporal::evaluate_edge_at(
                topology,
                metrics,
                context,
                next_edge,
                requested_entry,
                fraction,
            ) else {
                continue;
            };
            let mut component_values = labels[entry.label_id].component_values.clone();
            for (slot, value) in temporal_component_vector(metrics, &cost)
                .into_iter()
                .enumerate()
            {
                component_values[slot] += value;
            }
            if !within_constraints(&component_values, constraints) {
                continue;
            }
            let generalized_cost = labels[entry.label_id].generalized_cost
                + turn_time_s * metrics.turn_costs.cost_time_weight
                + cost.generalized_cost;
            let label_id = labels.len();
            labels.push(TemporalComponentLabel {
                state: SearchStateKey {
                    edge_index: next_edge,
                    automaton_state: routing_graph
                        .automaton
                        .transition(current.automaton_state, next_edge),
                },
                arrival: cost.exit_time,
                generalized_cost,
                component_values,
                previous: Some(entry.label_id),
                traversal: TemporalComponentTraversal {
                    edge_index: next_edge,
                    fraction,
                    turn_time_s,
                    cost,
                },
                active: true,
            });
            if target_fraction.is_some() {
                targets.push(label_id);
            } else if insert_temporal_component_label(
                &mut labels,
                &mut state_labels,
                label_id,
                dominance_dimensions,
                max_labels,
            )? {
                heap.push(HeapEntry {
                    label_id,
                    cost: generalized_cost,
                });
            }
        }
    }

    retain_temporal_target_frontier(&mut targets, &labels, pareto_index);
    Ok(targets
        .into_iter()
        .map(|target| {
            let mut traversals = Vec::new();
            let mut cursor = Some(target);
            while let Some(label_id) = cursor {
                traversals.push(labels[label_id].traversal.clone());
                cursor = labels[label_id].previous;
            }
            traversals.reverse();
            (
                labels[target].generalized_cost,
                labels[target].arrival,
                labels[target].component_values.clone(),
                traversals,
            )
        })
        .collect())
}

fn temporal_component_vector(
    metrics: &CompiledProfileBundle,
    cost: &crate::temporal::EdgeTemporalCost,
) -> Vec<f64> {
    metrics
        .components
        .iter()
        .map(|component| {
            cost.components
                .get(&component.name)
                .copied()
                .unwrap_or_default()
        })
        .collect()
}

fn component_paths_for_pair(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
    constraints: &[(usize, f64)],
    dominance_dimensions: &[usize],
    pareto_index: Option<usize>,
    max_labels: usize,
) -> Result<Vec<(Vec<usize>, f64, Vec<f64>)>> {
    if origin.snapped_edge_id.is_none()
        && destination.snapped_edge_id.is_none()
        && origin.snapped_node_id == destination.snapped_node_id
    {
        return Ok(vec![(Vec::new(), 0.0, vec![0.0; metrics.components.len()])]);
    }
    if let (
        Some(origin_edge),
        Some(origin_fraction),
        Some(destination_edge),
        Some(destination_fraction),
    ) = (
        origin.snapped_edge_id,
        origin.snapped_edge_fraction,
        destination.snapped_edge_id,
        destination.snapped_edge_fraction,
    ) && origin_edge == destination_edge
        && origin_fraction <= destination_fraction
    {
        let fraction = destination_fraction - origin_fraction;
        let components = edge_component_vector(metrics, origin_edge as usize, fraction);
        if within_constraints(&components, constraints) {
            return Ok(vec![(
                vec![origin_edge as usize],
                routing_graph.edge_costs[origin_edge as usize] * fraction,
                components,
            )]);
        }
        return Ok(Vec::new());
    }

    let mut labels = Vec::<ComponentLabel>::new();
    let mut state_labels = HashMap::<SearchStateKey, Vec<usize>>::new();
    let mut heap = BinaryHeap::new();
    let mut targets = Vec::new();
    let seeds = if let (Some(edge_id), Some(fraction)) =
        (origin.snapped_edge_id, origin.snapped_edge_fraction)
    {
        vec![(edge_id as usize, 1.0 - fraction)]
    } else {
        routing_graph
            .outgoing_edges(origin.snapped_node_id as usize)
            .iter()
            .map(|edge_index| (*edge_index as usize, 1.0))
            .collect()
    };
    for (edge_index, ordinary_fraction) in seeds {
        let target_fraction = destination_fraction_for_edge(topology, destination, edge_index);
        let fraction = target_fraction.unwrap_or(ordinary_fraction);
        let component_values = edge_component_vector(metrics, edge_index, fraction);
        if !within_constraints(&component_values, constraints) {
            continue;
        }
        let generalized_cost = routing_graph.edge_costs[edge_index] * fraction;
        if !generalized_cost.is_finite() {
            continue;
        }
        let label_id = labels.len();
        labels.push(ComponentLabel {
            state: SearchStateKey {
                edge_index,
                automaton_state: routing_graph.automaton.transition(0, edge_index),
            },
            generalized_cost,
            component_values,
            previous: None,
            edge_index,
            active: true,
        });
        if target_fraction.is_some() {
            targets.push(label_id);
        } else if insert_label(
            &mut labels,
            &mut state_labels,
            label_id,
            dominance_dimensions,
            max_labels,
        )? {
            heap.push(HeapEntry {
                label_id,
                cost: generalized_cost,
            });
        }
    }

    while let Some(entry) = heap.pop() {
        if !labels[entry.label_id].active || entry.cost > labels[entry.label_id].generalized_cost {
            continue;
        }
        let current = labels[entry.label_id].state;
        for transition_index in routing_graph.transition_range(current.edge_index) {
            let next_edge = routing_graph.transition_edges[transition_index] as usize;
            if routing_graph
                .automaton
                .prohibited_sequence_len(current.automaton_state, next_edge)
                .is_some()
            {
                continue;
            }
            let target_fraction = destination_fraction_for_edge(topology, destination, next_edge);
            let fraction = target_fraction.unwrap_or(1.0);
            let mut component_values = labels[entry.label_id].component_values.clone();
            for (slot, value) in edge_component_vector(metrics, next_edge, fraction)
                .into_iter()
                .enumerate()
            {
                component_values[slot] += value;
            }
            if !within_constraints(&component_values, constraints) {
                continue;
            }
            let turn_cost = turn_penalty_seconds(topology, metrics, current.edge_index, next_edge)
                * metrics.turn_costs.cost_time_weight;
            let generalized_cost = labels[entry.label_id].generalized_cost
                + routing_graph.edge_costs[next_edge] * fraction
                + turn_cost;
            let label_id = labels.len();
            labels.push(ComponentLabel {
                state: SearchStateKey {
                    edge_index: next_edge,
                    automaton_state: routing_graph
                        .automaton
                        .transition(current.automaton_state, next_edge),
                },
                generalized_cost,
                component_values,
                previous: Some(entry.label_id),
                edge_index: next_edge,
                active: true,
            });
            if target_fraction.is_some() {
                targets.push(label_id);
            } else if insert_label(
                &mut labels,
                &mut state_labels,
                label_id,
                dominance_dimensions,
                max_labels,
            )? {
                heap.push(HeapEntry {
                    label_id,
                    cost: generalized_cost,
                });
            }
        }
    }

    retain_target_frontier(&mut targets, &labels, pareto_index);
    Ok(targets
        .into_iter()
        .map(|target| {
            let mut edge_indexes = Vec::new();
            let mut cursor = Some(target);
            while let Some(label_id) = cursor {
                edge_indexes.push(labels[label_id].edge_index);
                cursor = labels[label_id].previous;
            }
            edge_indexes.reverse();
            (
                edge_indexes,
                labels[target].generalized_cost,
                labels[target].component_values.clone(),
            )
        })
        .collect())
}

fn edge_component_vector(
    metrics: &CompiledProfileBundle,
    edge_index: usize,
    fraction: f64,
) -> Vec<f64> {
    metrics
        .components
        .iter()
        .map(|component| {
            let overlay_multiplier =
                if component.overlay_name.is_some() && !component.invert_overlay {
                    0.0
                } else {
                    1.0
                };
            f64::from(*component.edge_values.get(edge_index).unwrap_or(&0.0))
                * fraction
                * overlay_multiplier
        })
        .collect()
}

fn within_constraints(values: &[f64], constraints: &[(usize, f64)]) -> bool {
    constraints
        .iter()
        .all(|(index, max)| values[*index] <= *max + f64::EPSILON)
}

fn insert_label(
    labels: &mut [ComponentLabel],
    state_labels: &mut HashMap<SearchStateKey, Vec<usize>>,
    new_id: usize,
    dimensions: &[usize],
    max_labels: usize,
) -> Result<bool> {
    let state = labels[new_id].state;
    let existing = state_labels.entry(state).or_default();
    existing.retain(|id| labels[*id].active);
    if existing
        .iter()
        .any(|id| dominates(&labels[*id], &labels[new_id], dimensions))
    {
        labels[new_id].active = false;
        return Ok(false);
    }
    for &id in existing.iter() {
        if dominates(&labels[new_id], &labels[id], dimensions) {
            labels[id].active = false;
        }
    }
    existing.retain(|id| labels[*id].active);
    if existing.len() >= max_labels {
        bail!(
            "component search exceeded max_labels_per_state={max_labels} at edge {} (automaton state {}); increase the label guard to preserve the exact Pareto frontier",
            state.edge_index,
            state.automaton_state
        );
    }
    existing.push(new_id);
    Ok(true)
}

fn dominates(left: &ComponentLabel, right: &ComponentLabel, dimensions: &[usize]) -> bool {
    left.generalized_cost <= right.generalized_cost
        && dimensions
            .iter()
            .all(|index| left.component_values[*index] <= right.component_values[*index])
}

fn insert_temporal_component_label(
    labels: &mut [TemporalComponentLabel],
    state_labels: &mut HashMap<SearchStateKey, Vec<usize>>,
    new_id: usize,
    dimensions: &[usize],
    max_labels: usize,
) -> Result<bool> {
    let state = labels[new_id].state;
    let existing = state_labels.entry(state).or_default();
    existing.retain(|id| labels[*id].active);
    if existing
        .iter()
        .any(|id| temporal_component_dominates(&labels[*id], &labels[new_id], dimensions))
    {
        labels[new_id].active = false;
        return Ok(false);
    }
    for &id in existing.iter() {
        if temporal_component_dominates(&labels[new_id], &labels[id], dimensions) {
            labels[id].active = false;
        }
    }
    existing.retain(|id| labels[*id].active);
    if existing.len() >= max_labels {
        bail!(
            "temporal component search exceeded max_labels_per_state={max_labels} at edge {} (automaton state {}); increase the label guard to preserve the exact Pareto frontier",
            state.edge_index,
            state.automaton_state
        );
    }
    existing.push(new_id);
    Ok(true)
}

fn temporal_component_dominates(
    left: &TemporalComponentLabel,
    right: &TemporalComponentLabel,
    dimensions: &[usize],
) -> bool {
    left.generalized_cost <= right.generalized_cost
        && left.arrival <= right.arrival
        && dimensions
            .iter()
            .all(|index| left.component_values[*index] <= right.component_values[*index])
}

fn retain_temporal_target_frontier(
    targets: &mut Vec<usize>,
    labels: &[TemporalComponentLabel],
    pareto_index: Option<usize>,
) {
    let snapshot = targets.clone();
    targets.retain(|candidate| {
        !snapshot.iter().any(|other| {
            other != candidate
                && labels[*other].generalized_cost <= labels[*candidate].generalized_cost
                && pareto_index.is_none_or(|index| {
                    labels[*other].component_values[index]
                        <= labels[*candidate].component_values[index]
                })
                && (labels[*other].generalized_cost < labels[*candidate].generalized_cost
                    || pareto_index.is_some_and(|index| {
                        labels[*other].component_values[index]
                            < labels[*candidate].component_values[index]
                    }))
        })
    });
}

fn retain_temporal_global_frontier(
    paths: &mut Vec<TemporalComponentPath>,
    pareto_index: Option<usize>,
) {
    let snapshot = paths.clone();
    paths.retain(|candidate| {
        !snapshot.iter().any(|other| {
            other.generalized_cost <= candidate.generalized_cost
                && pareto_index.is_none_or(|index| {
                    other.component_values[index] <= candidate.component_values[index]
                })
                && (other.generalized_cost < candidate.generalized_cost
                    || pareto_index.is_some_and(|index| {
                        other.component_values[index] < candidate.component_values[index]
                    }))
        })
    });
}

fn retain_target_frontier(
    targets: &mut Vec<usize>,
    labels: &[ComponentLabel],
    pareto_index: Option<usize>,
) {
    let snapshot = targets.clone();
    targets.retain(|candidate| {
        !snapshot.iter().any(|other| {
            other != candidate
                && labels[*other].generalized_cost <= labels[*candidate].generalized_cost
                && pareto_index.is_none_or(|index| {
                    labels[*other].component_values[index]
                        <= labels[*candidate].component_values[index]
                })
                && (labels[*other].generalized_cost < labels[*candidate].generalized_cost
                    || pareto_index.is_some_and(|index| {
                        labels[*other].component_values[index]
                            < labels[*candidate].component_values[index]
                    }))
        })
    });
}

fn retain_global_frontier(paths: &mut Vec<ComponentPath>, pareto_index: Option<usize>) {
    let snapshot = paths.clone();
    paths.retain(|candidate| {
        !snapshot.iter().any(|other| {
            other.generalized_cost <= candidate.generalized_cost
                && pareto_index.is_none_or(|index| {
                    other.component_values[index] <= candidate.component_values[index]
                })
                && (other.generalized_cost < candidate.generalized_cost
                    || pareto_index.is_some_and(|index| {
                        other.component_values[index] < candidate.component_values[index]
                    }))
        })
    });
}

fn destination_fraction_for_edge(
    topology: &TopologyBundle,
    destination: &SnappedPoint,
    edge_index: usize,
) -> Option<f64> {
    if destination.snapped_edge_id == Some(edge_index as u32) {
        return Some(destination.snapped_edge_fraction.unwrap_or(1.0));
    }
    (destination.snapped_edge_id.is_none()
        && topology.routing_edge(edge_index).to.0 == destination.snapped_node_id)
        .then_some(1.0)
}

fn build_component_route_result(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &RouteRequest,
    mut paths: Vec<ComponentPath>,
    edge_names: Option<&[String]>,
) -> Result<RouteResult> {
    let primary = paths.remove(0);
    let path = RoutePath {
        edge_indexes: primary.edge_indexes.clone(),
        total_generalized_cost: primary.generalized_cost,
    };
    let analysis = analyze_route_path(
        topology,
        metrics,
        routing_graph,
        &FallbackPolicy::default(),
        &path,
        &primary.origin,
        &primary.destination,
    )?;
    let detailed = request_returns_detailed_path(&request.returns);
    let names = edge_names.unwrap_or(&topology.names);
    let segments = request.returns.segment_rows.then(|| {
        primary
            .edge_indexes
            .iter()
            .enumerate()
            .map(|(position, edge_index)| {
                let edge = topology.edge(*edge_index);
                let factor = edge_traversal_factor(
                    *edge_index,
                    position == 0,
                    position + 1 == primary.edge_indexes.len(),
                    &primary.origin,
                    &primary.destination,
                );
                RouteSegment {
                    edge_id: edge.edge_id.0,
                    from_node_id: edge.from.0,
                    to_node_id: edge.to.0,
                    source_way_id: edge.source_way_id,
                    length_m: (f64::from(edge.length_m) * factor).round() as u32,
                    travel_time_s: metrics.edge_metrics[*edge_index]
                        .travel_time_s
                        .unwrap_or_default()
                        * factor,
                    generalized_cost: metrics.edge_metrics[*edge_index]
                        .generalized_cost
                        .unwrap_or_default()
                        * factor,
                    components: static_edge_components(metrics, *edge_index, factor),
                    waiting_time_s: 0.0,
                    entry_time: None,
                    exit_time: None,
                    road_class: edge.road_class,
                    surface: edge.surface,
                    name: edge
                        .name_index
                        .and_then(|index| names.get(index as usize))
                        .cloned(),
                    violation_type: None,
                }
            })
            .collect()
    });
    let alternatives = paths
        .into_iter()
        .enumerate()
        .map(|(index, alternative)| {
            let path = RoutePath {
                edge_indexes: alternative.edge_indexes.clone(),
                total_generalized_cost: alternative.generalized_cost,
            };
            let analysis = analyze_route_path(
                topology,
                metrics,
                routing_graph,
                &FallbackPolicy::default(),
                &path,
                &alternative.origin,
                &alternative.destination,
            )?;
            Ok(RouteAlternative {
                alternative_index: index as u32 + 1,
                rank: index as u32 + 2,
                summary: analysis.summary,
                node_path: Vec::new(),
                edge_path: alternative
                    .edge_indexes
                    .iter()
                    .map(|edge_index| topology.routing_edge(*edge_index).edge_id.0)
                    .collect(),
                geometry: (!matches!(request.returns.geometry, ReturnGeometry::None)).then(|| {
                    build_route_geometry(
                        topology,
                        &alternative.edge_indexes,
                        &alternative.origin,
                        &alternative.destination,
                    )
                }),
                segments: None,
                breakdowns: None,
                violations: Vec::new(),
                diagnostics: Vec::new(),
                warnings: Vec::new(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut warnings = Vec::new();
    warnings.push(
        "Component budget/Pareto route used exact nondominated label-setting; static CCH acceleration was intentionally bypassed."
            .to_string(),
    );
    Ok(RouteResult {
        route_id: request.route_id.clone(),
        origin: primary.origin.clone(),
        destination: primary.destination.clone(),
        outcome: if primary.hop_info.fallback_used {
            AnalysisOutcome::Degraded
        } else {
            AnalysisOutcome::Legal
        },
        fallback_used: primary.hop_info.fallback_used,
        origin_hop_distance_m: primary.hop_info.origin_hop_distance_m,
        destination_hop_distance_m: primary.hop_info.destination_hop_distance_m,
        summary: analysis.summary,
        node_path: Vec::new(),
        edge_path: if detailed {
            primary
                .edge_indexes
                .iter()
                .map(|edge_index| topology.routing_edge(*edge_index).edge_id.0)
                .collect()
        } else {
            Vec::new()
        },
        geometry: (!matches!(request.returns.geometry, ReturnGeometry::None)).then(|| {
            build_route_geometry(
                topology,
                &primary.edge_indexes,
                &primary.origin,
                &primary.destination,
            )
        }),
        hop_segments: primary.hop_info.hop_segments,
        segments,
        breakdowns: build_breakdowns(topology, metrics, &primary.edge_indexes, &request.returns),
        violations: analysis.violations,
        diagnostics: primary.hop_info.diagnostics,
        warnings,
        alternatives,
    })
}

fn build_temporal_component_route_result(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &RouteRequest,
    context: &TemporalContext,
    mut paths: Vec<TemporalComponentPath>,
    edge_names: Option<&[String]>,
) -> Result<RouteResult> {
    let primary = paths.remove(0);
    let detailed = request_returns_detailed_path(&request.returns);
    let primary_edges = temporal_component_edge_indexes(&primary);
    let primary_summary = temporal_component_summary(topology, metrics, context, &primary);
    let names = edge_names.unwrap_or(&topology.names);
    let segments = request
        .returns
        .segment_rows
        .then(|| temporal_component_segments(topology, &primary, names));
    let alternatives = paths
        .into_iter()
        .enumerate()
        .map(|(index, alternative)| {
            let edge_indexes = temporal_component_edge_indexes(&alternative);
            Ok(RouteAlternative {
                alternative_index: index as u32 + 1,
                rank: index as u32 + 2,
                summary: temporal_component_summary(topology, metrics, context, &alternative),
                node_path: if detailed {
                    route_nodes(topology, &edge_indexes)
                } else {
                    Vec::new()
                },
                edge_path: if detailed {
                    edge_indexes
                        .iter()
                        .map(|edge_index| topology.routing_edge(*edge_index).edge_id.0)
                        .collect()
                } else {
                    Vec::new()
                },
                geometry: (!matches!(request.returns.geometry, ReturnGeometry::None)).then(|| {
                    build_route_geometry(
                        topology,
                        &edge_indexes,
                        &alternative.origin,
                        &alternative.destination,
                    )
                }),
                segments: request
                    .returns
                    .segment_rows
                    .then(|| temporal_component_segments(topology, &alternative, names)),
                breakdowns: build_breakdowns(topology, metrics, &edge_indexes, &request.returns),
                violations: Vec::new(),
                diagnostics: alternative.hop_info.diagnostics,
                warnings: Vec::new(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut warnings = Vec::new();
    warnings.push(
        "Temporal component budget/Pareto route used exact nondominated label-setting with edge-entry evaluation; static CCH acceleration was intentionally bypassed."
            .to_string(),
    );
    Ok(RouteResult {
        route_id: request.route_id.clone(),
        origin: primary.origin.clone(),
        destination: primary.destination.clone(),
        outcome: if primary.hop_info.fallback_used {
            AnalysisOutcome::Degraded
        } else {
            AnalysisOutcome::Legal
        },
        fallback_used: primary.hop_info.fallback_used,
        origin_hop_distance_m: primary.hop_info.origin_hop_distance_m,
        destination_hop_distance_m: primary.hop_info.destination_hop_distance_m,
        summary: primary_summary,
        node_path: if detailed {
            route_nodes(topology, &primary_edges)
        } else {
            Vec::new()
        },
        edge_path: if detailed {
            primary_edges
                .iter()
                .map(|edge_index| topology.routing_edge(*edge_index).edge_id.0)
                .collect()
        } else {
            Vec::new()
        },
        geometry: (!matches!(request.returns.geometry, ReturnGeometry::None)).then(|| {
            build_route_geometry(
                topology,
                &primary_edges,
                &primary.origin,
                &primary.destination,
            )
        }),
        hop_segments: primary.hop_info.hop_segments,
        segments,
        breakdowns: build_breakdowns(topology, metrics, &primary_edges, &request.returns),
        violations: Vec::new(),
        diagnostics: primary.hop_info.diagnostics,
        warnings: {
            warnings.extend(primary.hop_info.warnings);
            warnings
        },
        alternatives,
    })
}

fn temporal_component_edge_indexes(path: &TemporalComponentPath) -> Vec<usize> {
    path.traversals
        .iter()
        .map(|traversal| traversal.edge_index)
        .collect()
}

fn route_nodes(topology: &TopologyBundle, edge_indexes: &[usize]) -> Vec<u32> {
    let mut nodes = Vec::with_capacity(edge_indexes.len() + 1);
    if let Some(first_edge) = edge_indexes.first() {
        nodes.push(topology.routing_edge(*first_edge).from.0);
        nodes.extend(
            edge_indexes
                .iter()
                .map(|edge_index| topology.routing_edge(*edge_index).to.0),
        );
    }
    nodes
}

fn temporal_component_summary(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    context: &TemporalContext,
    path: &TemporalComponentPath,
) -> RouteSummary {
    let mut distance_m = 0_u64;
    let mut waiting_time_s = 0.0;
    let mut travel_time_s = 0.0;
    let mut components = metrics
        .components
        .iter()
        .map(|component| (component.name.clone(), 0.0))
        .collect::<std::collections::BTreeMap<_, _>>();
    for traversal in &path.traversals {
        distance_m += (f64::from(topology.routing_edge(traversal.edge_index).length_m)
            * traversal.fraction)
            .round() as u64;
        waiting_time_s += traversal.cost.wait_s;
        travel_time_s +=
            traversal.turn_time_s + traversal.cost.wait_s + traversal.cost.travel_time_s;
        for (name, value) in &traversal.cost.components {
            *components.entry(name.clone()).or_default() += value;
        }
    }
    RouteSummary {
        network_distance_m: distance_m,
        network_travel_time_s: travel_time_s,
        network_generalized_cost: path.generalized_cost,
        components,
        waiting_time_s,
        departure_time: Some(format_temporal_datetime(path.departure)),
        arrival_time: Some(format_temporal_datetime(path.arrival)),
        scenario_id: context.scenario_id.clone(),
        illegal_movement_penalty_s: 0.0,
        illegal_movement_penalty_cost: 0.0,
        violation_count: 0,
        violation_types: Vec::new(),
        total_distance_m: distance_m,
        total_travel_time_s: travel_time_s,
        total_generalized_cost: path.generalized_cost,
        segment_count: path.traversals.len(),
    }
}

fn temporal_component_segments(
    topology: &TopologyBundle,
    path: &TemporalComponentPath,
    names: &[String],
) -> Vec<RouteSegment> {
    path.traversals
        .iter()
        .map(|traversal| {
            let edge = topology.edge(traversal.edge_index);
            RouteSegment {
                edge_id: edge.edge_id.0,
                from_node_id: edge.from.0,
                to_node_id: edge.to.0,
                source_way_id: edge.source_way_id,
                length_m: (f64::from(edge.length_m) * traversal.fraction).round() as u32,
                travel_time_s: traversal.cost.travel_time_s + traversal.cost.wait_s,
                generalized_cost: traversal.cost.generalized_cost,
                components: traversal.cost.components.clone(),
                waiting_time_s: traversal.cost.wait_s,
                entry_time: Some(format_temporal_datetime(traversal.cost.entry_time)),
                exit_time: Some(format_temporal_datetime(traversal.cost.exit_time)),
                road_class: edge.road_class,
                surface: edge.surface,
                name: edge
                    .name_index
                    .and_then(|index| names.get(index as usize))
                    .cloned(),
                violation_type: None,
            }
        })
        .collect()
}

fn format_temporal_datetime(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .expect("OffsetDateTime always formats as RFC 3339")
}
