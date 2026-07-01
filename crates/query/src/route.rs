use std::collections::HashMap;

use anyhow::{Result, bail};
use netweevil_core::{CompiledEdgeMetric, CompiledProfileBundle, DirectedEdge, TopologyBundle};
use netweevil_profile::ReturnConfig;

use crate::*;

pub fn execute_route(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &RouteRequest,
) -> Result<RouteResult> {
    execute_route_with_optional_edge_names(topology, metrics, request, None)
}

pub fn execute_route_with_edge_names(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &RouteRequest,
    edge_names: &[String],
) -> Result<RouteResult> {
    execute_route_with_optional_edge_names(topology, metrics, request, Some(edge_names))
}

fn execute_route_with_optional_edge_names(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &RouteRequest,
    edge_names: Option<&[String]>,
) -> Result<RouteResult> {
    if has_failure_modes(&request.fallback) {
        validate_execution_inputs(topology, metrics)?;
        let degraded = cached_failure_mode_bundle(topology, metrics, &request.fallback, None)?;
        return execute_route_with_graph(
            &degraded.topology,
            degraded.metrics.as_ref(),
            &degraded.routing_graph,
            request,
            edge_names,
        );
    }
    validate_execution_inputs(topology, metrics)?;
    let routing_graph = build_routing_graph(topology, metrics)?;
    execute_route_with_graph(topology, metrics, &routing_graph, request, edge_names)
}

pub(crate) fn validate_execution_inputs(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
) -> Result<()> {
    if topology.edge_count() != metrics.edge_metrics.len() {
        bail!(
            "topology edge count ({}) does not match compiled metric count ({})",
            topology.edge_count(),
            metrics.edge_metrics.len()
        );
    }
    Ok(())
}

pub(crate) fn has_failure_modes(fallback: &FallbackPolicy) -> bool {
    fallback.allow_reverse_oneway
        || fallback.allow_illegal_turn
        || fallback.ignore_turn_restrictions
        || fallback.allow_uturn_where_normally_forbidden
}

pub(crate) fn auto_relaxation_requested(fallback: &FallbackPolicy) -> bool {
    fallback.auto_relax_unreachable
}

fn fallback_without_auto_relaxation(fallback: &FallbackPolicy) -> FallbackPolicy {
    let mut sanitized = fallback.clone();
    sanitized.auto_relax_unreachable = false;
    sanitized
}

fn auto_relaxed_seed_fallback(fallback: &FallbackPolicy) -> FallbackPolicy {
    let mut seed = fallback_without_auto_relaxation(fallback);
    seed.allow_reverse_oneway = true;
    seed.allow_illegal_turn = true;
    seed.ignore_turn_restrictions = true;
    seed.allow_uturn_where_normally_forbidden = true;
    seed.penalties.reverse_oneway_penalty_s =
        seed.penalties.reverse_oneway_penalty_s.or(Some(120.0));
    seed.penalties.illegal_turn_penalty_s = seed.penalties.illegal_turn_penalty_s.or(Some(90.0));
    seed.penalties.ignored_turn_restriction_penalty_s = seed
        .penalties
        .ignored_turn_restriction_penalty_s
        .or(Some(180.0));
    seed.penalties.forbidden_uturn_penalty_s =
        seed.penalties.forbidden_uturn_penalty_s.or(Some(60.0));
    seed
}

fn auto_selected_fallback(
    requested: &FallbackPolicy,
    seed: &FallbackPolicy,
    route: &RouteResult,
) -> FallbackPolicy {
    let mut selected = fallback_without_auto_relaxation(requested);
    for violation in &route.violations {
        match violation.violation_type {
            RouteViolationType::ReverseOneway => {
                selected.allow_reverse_oneway = true;
            }
            RouteViolationType::IllegalTurn => {
                selected.allow_illegal_turn = true;
            }
            RouteViolationType::IgnoredTurnRestriction => {
                selected.ignore_turn_restrictions = true;
            }
            RouteViolationType::ForbiddenUturn => {
                selected.allow_uturn_where_normally_forbidden = true;
            }
        }
    }

    if selected.allow_reverse_oneway {
        selected.penalties.reverse_oneway_penalty_s = seed.penalties.reverse_oneway_penalty_s;
    }
    if selected.allow_illegal_turn {
        selected.penalties.illegal_turn_penalty_s = seed.penalties.illegal_turn_penalty_s;
    }
    if selected.ignore_turn_restrictions {
        selected.penalties.ignored_turn_restriction_penalty_s =
            seed.penalties.ignored_turn_restriction_penalty_s;
    }
    if selected.allow_uturn_where_normally_forbidden {
        selected.penalties.forbidden_uturn_penalty_s = seed.penalties.forbidden_uturn_penalty_s;
    }

    selected
}

fn fallback_mode_labels(fallback: &FallbackPolicy) -> Vec<&'static str> {
    let mut labels = Vec::new();
    if fallback.allow_uturn_where_normally_forbidden {
        labels.push("allow_uturn_where_normally_forbidden");
    }
    if fallback.allow_illegal_turn {
        labels.push("allow_illegal_turn");
    }
    if fallback.allow_reverse_oneway {
        labels.push("allow_reverse_oneway");
    }
    if fallback.ignore_turn_restrictions {
        labels.push("ignore_turn_restrictions");
    }
    labels
}

/// Degraded topology/metrics/graph triple used to answer requests with
/// failure modes enabled. Structure depends only on the dataset, the
/// profile, and whether reverse-oneway virtual edges are materialized; the
/// per-request penalties are applied at search time.
pub(crate) struct FailureModeBundle {
    pub(crate) topology: TopologyBundle,
    pub(crate) metrics: std::sync::Arc<CompiledProfileBundle>,
    pub(crate) routing_graph: RoutingGraph,
}

/// Cache key: dataset bundle id, profile hash, the full failure-mode
/// configuration (flags and penalty values, which bake into the customized
/// acceleration weights), whether a base acceleration was available, plus
/// the base topology's structural counts. Production bundle ids are
/// content-derived, so the counts are redundant there; they guard against
/// hand-constructed bundles (e.g. tests) that reuse placeholder ids across
/// different topologies.
type FailureModeKey = (
    netweevil_core::CacheBundleId,
    String,
    [bool; 4],
    [u64; 4],
    bool,
    usize,
    usize,
    usize,
);

fn failure_mode_key(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    fallback: &FallbackPolicy,
    base_acceleration_available: bool,
) -> FailureModeKey {
    (
        metrics.source_topology_bundle_id.clone(),
        metrics.profile_hash.clone(),
        [
            fallback.allow_reverse_oneway,
            fallback.allow_illegal_turn,
            fallback.ignore_turn_restrictions,
            fallback.allow_uturn_where_normally_forbidden,
        ],
        [
            fallback
                .penalties
                .reverse_oneway_penalty_s
                .unwrap_or_default()
                .to_bits(),
            fallback
                .penalties
                .illegal_turn_penalty_s
                .unwrap_or_default()
                .to_bits(),
            fallback
                .penalties
                .ignored_turn_restriction_penalty_s
                .unwrap_or_default()
                .to_bits(),
            fallback
                .penalties
                .forbidden_uturn_penalty_s
                .unwrap_or_default()
                .to_bits(),
        ],
        base_acceleration_available,
        topology.edge_count(),
        topology.nodes.len(),
        topology.edge_based_topology.edge_transition_edges.len(),
    )
}

/// Degraded bundles are expensive (a full topology clone plus a CSR graph
/// rebuild) and were previously rebuilt on every failure-mode request. Keep
/// the few most recent variants; each (dataset, profile) pair has at most
/// two (with and without reverse-oneway edges).
const FAILURE_MODE_CACHE_CAPACITY: usize = 8;

static FAILURE_MODE_CACHE: std::sync::Mutex<
    Vec<(FailureModeKey, std::sync::Arc<FailureModeBundle>)>,
> = std::sync::Mutex::new(Vec::new());

pub(crate) fn cached_failure_mode_bundle(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    fallback: &FallbackPolicy,
    base_graph: Option<&RoutingGraph>,
) -> Result<std::sync::Arc<FailureModeBundle>> {
    // Reverse-oneway variants add virtual edges, so the base contraction no
    // longer covers the edge set; the other failure modes are weight-only
    // and can reuse the base CCH with penalty-customized weights.
    let base_acceleration = base_graph
        .filter(|_| !fallback.allow_reverse_oneway)
        .and_then(|graph| graph.acceleration.as_ref());
    let key = failure_mode_key(topology, metrics, fallback, base_acceleration.is_some());
    let mut cache = FAILURE_MODE_CACHE.lock().expect("failure-mode cache lock");
    if let Some(position) = cache.iter().position(|(existing, _)| existing == &key) {
        let entry = cache.remove(position);
        let bundle = entry.1.clone();
        // Most-recently-used entries live at the back.
        cache.push(entry);
        return Ok(bundle);
    }
    let (degraded_topology, mut degraded_metrics, mut routing_graph) =
        build_failure_mode_bundle(topology, metrics, fallback)?;

    if let Some(base_acceleration) = base_acceleration
        && let Some(customized) = customize_failure_mode_acceleration(
            &degraded_topology,
            &degraded_metrics,
            &routing_graph,
            base_acceleration,
            fallback,
        )
    {
        degraded_metrics.acceleration = Some(customized);
    }
    let degraded_metrics = std::sync::Arc::new(degraded_metrics);
    if degraded_metrics.acceleration.is_some()
        && let Some(base_acceleration) = base_acceleration
    {
        routing_graph.acceleration = build_acceleration_graph(
            degraded_metrics.clone(),
            Some(base_acceleration.source.clone()),
            degraded_topology.edge_count(),
        )?;
        routing_graph.acceleration_includes_failure_penalties =
            routing_graph.acceleration.is_some();
    }

    let bundle = std::sync::Arc::new(FailureModeBundle {
        topology: degraded_topology,
        metrics: degraded_metrics,
        routing_graph,
    });
    if cache.len() >= FAILURE_MODE_CACHE_CAPACITY {
        cache.remove(0);
    }
    cache.push((key, bundle.clone()));
    Ok(bundle)
}

fn build_failure_mode_bundle(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    fallback: &FallbackPolicy,
) -> Result<(TopologyBundle, CompiledProfileBundle, RoutingGraph)> {
    let mut degraded_topology = topology.clone();
    let mut degraded_metrics = metrics.clone();
    degraded_metrics.acceleration = None;
    let mut virtual_reverse_of = vec![None; degraded_topology.edge_count()];

    if fallback.allow_reverse_oneway {
        let existing_edges = (0..degraded_topology.edge_count())
            .filter(|&edge_index| {
                degraded_metrics
                    .edge_metrics
                    .get(edge_index)
                    .and_then(|metric| metric.generalized_cost)
                    .is_some()
            })
            .map(|edge_index| {
                let edge = degraded_topology.routing_edge(edge_index);
                (edge.from.0, edge.to.0, edge.source_way_id)
            })
            .collect::<std::collections::BTreeSet<_>>();
        let original_edge_count = degraded_topology.edge_count();
        for edge_index in 0..original_edge_count {
            let edge = degraded_topology.edge(edge_index);
            let metric = &degraded_metrics.edge_metrics[edge_index];
            if metric.generalized_cost.is_none() || metric.travel_time_s.is_none() {
                continue;
            }
            if existing_edges.contains(&(edge.to.0, edge.from.0, edge.source_way_id)) {
                continue;
            }
            degraded_topology.push_edge(DirectedEdge {
                edge_id: edge.edge_id,
                from: edge.to,
                to: edge.from,
                source_way_id: edge.source_way_id,
                length_m: edge.length_m,
                duration_s: edge.duration_s,
                road_class: edge.road_class,
                surface: edge.surface,
                smoothness: edge.smoothness,
                access_mask: edge.access_mask,
                is_toll: edge.is_toll,
                name_index: edge.name_index,
                geometry_offset: edge.geometry_offset,
                geometry_len: edge.geometry_len,
                flags: 0,
            });
            degraded_metrics.edge_metrics.push(CompiledEdgeMetric {
                edge_id: metric.edge_id,
                travel_time_s: metric.travel_time_s,
                generalized_cost: metric.generalized_cost,
            });
            degraded_topology.edge_component_ids.push(
                topology
                    .edge_component_id(edge_index as u32)
                    .unwrap_or_default(),
            );
            virtual_reverse_of.push(Some(edge_index));
        }
    }

    degraded_topology.edge_based_topology = build_edge_based_topology_fallback(&degraded_topology);
    let mut routing_graph = build_routing_graph_with_options(
        &degraded_topology,
        &degraded_metrics,
        RoutingGraphBuildOptions {
            search_time_turn_restrictions: true,
            ..RoutingGraphBuildOptions::default()
        },
    )?;
    routing_graph.acceleration = None;
    routing_graph.virtual_reverse_of = virtual_reverse_of;
    Ok((degraded_topology, degraded_metrics, routing_graph))
}

pub(crate) fn execute_route_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &RouteRequest,
    edge_names: Option<&[String]>,
) -> Result<RouteResult> {
    let search_distance_m = request
        .connectivity
        .max_hop_distance_m
        .unwrap_or(request.snap.max_distance_m)
        .max(request.snap.max_distance_m);
    let origin_candidates = snap_candidates(
        topology,
        routing_graph,
        &request.origin,
        search_distance_m,
        true,
    )?;
    let destination_candidates = snap_candidates(
        topology,
        routing_graph,
        &request.destination,
        search_distance_m,
        false,
    )?;
    execute_route_with_candidates(
        topology,
        metrics,
        routing_graph,
        &request.route_id,
        request.snap.max_distance_m,
        &request.connectivity,
        &request.fallback,
        &request.returns,
        &request.alternatives,
        &origin_candidates,
        &destination_candidates,
        edge_names,
    )
}

pub(crate) fn execute_route_with_candidates(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    route_id: &str,
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    fallback: &FallbackPolicy,
    returns: &ReturnConfig,
    alternatives: &AlternativeRouteOptions,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    edge_names: Option<&[String]>,
) -> Result<RouteResult> {
    if auto_relaxation_requested(fallback) && !has_failure_modes(fallback) {
        let strict_fallback = fallback_without_auto_relaxation(fallback);
        if let Err(error) = route_between_candidates(
            topology,
            metrics,
            routing_graph,
            snap_max_distance_m,
            connectivity,
            &strict_fallback,
            origin_candidates,
            destination_candidates,
        ) {
            if let Some(result) = try_auto_relaxed_route_with_candidates(
                topology,
                metrics,
                route_id,
                snap_max_distance_m,
                connectivity,
                fallback,
                returns,
                alternatives,
                origin_candidates,
                destination_candidates,
                edge_names,
            )? {
                return Ok(result);
            }
            if matches!(
                connectivity.disconnected,
                DisconnectedNetworkMode::IgnoreUnreachable
            ) && let Some(failure) = analysis_failure(&error)
                && matches!(failure.outcome, AnalysisOutcome::Unreachable)
            {
                return Ok(ignored_unreachable_route_result(
                    route_id,
                    origin_candidates,
                    destination_candidates,
                    failure.clone(),
                    execution_warnings(metrics),
                ));
            }
            return Err(error);
        }
    }

    let (origin, destination, path, hop_info) = match route_between_candidates(
        topology,
        metrics,
        routing_graph,
        snap_max_distance_m,
        connectivity,
        fallback,
        origin_candidates,
        destination_candidates,
    ) {
        Ok(result) => result,
        Err(error)
            if matches!(
                connectivity.disconnected,
                DisconnectedNetworkMode::IgnoreUnreachable
            ) =>
        {
            if let Some(failure) = analysis_failure(&error)
                && matches!(failure.outcome, AnalysisOutcome::Unreachable)
            {
                return Ok(ignored_unreachable_route_result(
                    route_id,
                    origin_candidates,
                    destination_candidates,
                    failure.clone(),
                    execution_warnings(metrics),
                ));
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };

    let include_detailed_paths = request_returns_detailed_path(returns);
    let needs_node_path = include_detailed_paths
        || !matches!(returns.geometry, netweevil_profile::ReturnGeometry::None);
    let node_path = if needs_node_path {
        let mut node_path = Vec::with_capacity(path.edge_indexes.len() + 1);
        if let Some(&first_edge) = path.edge_indexes.first() {
            node_path.push(topology.routing_edge(first_edge).from.0);
            for &edge_index in &path.edge_indexes {
                node_path.push(topology.routing_edge(edge_index).to.0);
            }
        } else if origin.snapped_edge_id.is_none() {
            node_path.push(origin.snapped_node_id);
        }
        node_path
    } else {
        Vec::new()
    };

    let geometry = match returns.geometry {
        netweevil_profile::ReturnGeometry::None => None,
        _ => Some(build_route_geometry(
            topology,
            &path.edge_indexes,
            &origin,
            &destination,
        )),
    };

    let segments = if returns.segment_rows {
        let edge_names = edge_names.unwrap_or(&topology.names);
        Some(
            path.edge_indexes
                .iter()
                .map(|&edge_index| {
                    let edge = topology.edge(edge_index);
                    let metric = &metrics.edge_metrics[edge_index];
                    let factor = edge_traversal_factor(
                        edge_index,
                        path.edge_indexes.first().copied(),
                        path.edge_indexes.last().copied(),
                        &origin,
                        &destination,
                    );
                    RouteSegment {
                        edge_id: edge.edge_id.0,
                        from_node_id: edge.from.0,
                        to_node_id: edge.to.0,
                        source_way_id: edge.source_way_id,
                        length_m: (edge.length_m as f64 * factor).round() as u32,
                        travel_time_s: metric.travel_time_s.unwrap_or_default() * factor,
                        generalized_cost: metric.generalized_cost.unwrap_or_default() * factor,
                        road_class: edge.road_class,
                        surface: edge.surface,
                        name: edge
                            .name_index
                            .and_then(|index| edge_names.get(index as usize))
                            .cloned(),
                        violation_type: None,
                    }
                })
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };

    let analysis = analyze_route_path(
        topology,
        metrics,
        routing_graph,
        fallback,
        &path,
        &origin,
        &destination,
    )?;
    let mut segments = segments;
    if let Some(segment_rows) = segments.as_mut() {
        for (segment, edge_index) in segment_rows.iter_mut().zip(&path.edge_indexes) {
            segment.violation_type = analysis
                .segment_violation_types
                .get(edge_index)
                .copied()
                .flatten();
        }
    }
    let breakdowns = build_breakdowns(topology, metrics, &path.edge_indexes, returns);
    let warnings = execution_warnings(metrics);

    let mut result = RouteResult {
        route_id: route_id.to_string(),
        origin: origin.clone(),
        destination: destination.clone(),
        outcome: if !analysis.violations.is_empty() || hop_info.fallback_used {
            AnalysisOutcome::Degraded
        } else {
            AnalysisOutcome::Legal
        },
        fallback_used: hop_info.fallback_used || !analysis.violations.is_empty(),
        origin_hop_distance_m: hop_info.origin_hop_distance_m,
        destination_hop_distance_m: hop_info.destination_hop_distance_m,
        summary: analysis.summary,
        node_path,
        edge_path: if include_detailed_paths {
            path.edge_indexes
                .iter()
                .map(|&edge_index| topology.routing_edge(edge_index).edge_id.0)
                .collect()
        } else {
            Vec::new()
        },
        geometry,
        hop_segments: hop_info.hop_segments,
        segments,
        breakdowns,
        violations: analysis.violations,
        diagnostics: hop_info.diagnostics,
        warnings: merge_warnings(
            merge_warnings(warnings, analysis.warnings),
            hop_info.warnings,
        ),
        alternatives: Vec::new(),
    };
    result.alternatives = build_route_alternatives(
        topology,
        metrics,
        routing_graph,
        snap_max_distance_m,
        connectivity,
        fallback,
        returns,
        alternatives,
        std::slice::from_ref(&origin),
        std::slice::from_ref(&destination),
        &path,
        &result.summary,
        edge_names,
    )?;
    Ok(result)
}

fn try_auto_relaxed_route_with_candidates(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    route_id: &str,
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    fallback: &FallbackPolicy,
    returns: &ReturnConfig,
    alternatives: &AlternativeRouteOptions,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    edge_names: Option<&[String]>,
) -> Result<Option<RouteResult>> {
    let seed_fallback = auto_relaxed_seed_fallback(fallback);
    let degraded = cached_failure_mode_bundle(topology, metrics, &seed_fallback, None)?;
    let seed_result = match execute_route_with_candidates(
        &degraded.topology,
        degraded.metrics.as_ref(),
        &degraded.routing_graph,
        route_id,
        snap_max_distance_m,
        connectivity,
        &seed_fallback,
        returns,
        alternatives,
        origin_candidates,
        destination_candidates,
        edge_names,
    ) {
        Ok(result) => result,
        Err(error) => {
            if analysis_failure(&error)
                .is_some_and(|failure| matches!(failure.outcome, AnalysisOutcome::Unreachable))
            {
                return Ok(None);
            }
            return Err(error);
        }
    };

    let selected_fallback = auto_selected_fallback(fallback, &seed_fallback, &seed_result);
    let mut result = if selected_fallback == seed_fallback {
        seed_result
    } else {
        let selected = cached_failure_mode_bundle(topology, metrics, &selected_fallback, None)?;
        execute_route_with_candidates(
            &selected.topology,
            selected.metrics.as_ref(),
            &selected.routing_graph,
            route_id,
            snap_max_distance_m,
            connectivity,
            &selected_fallback,
            returns,
            alternatives,
            origin_candidates,
            destination_candidates,
            edge_names,
        )?
    };

    let selected_labels = fallback_mode_labels(&selected_fallback);
    if !selected_labels.is_empty() {
        result.diagnostics.push(AnalysisDiagnostic {
            code: AnalysisDiagnosticCode::FallbackUsed,
            severity: AnalysisDiagnosticSeverity::Warning,
            message: format!(
                "Auto fallback resolved an unreachable strict route by enabling {}.",
                selected_labels.join(", ")
            ),
            point_ids: vec![result.origin.point_id.clone(), result.destination.point_id.clone()],
            component_ids: Vec::new(),
            suggested_actions: vec![
                "Review the returned violations and fallback_used fields before using this route for strict legal-network analysis.".to_string(),
            ],
        });
        result.warnings.push(format!(
            "Auto fallback enabled {} after the strict route was unreachable.",
            selected_labels.join(", ")
        ));
    }

    Ok(Some(result))
}

pub(crate) fn request_returns_detailed_path(returns: &ReturnConfig) -> bool {
    !matches!(returns.geometry, netweevil_profile::ReturnGeometry::None)
        || returns.segment_rows
        || !returns.road_type_breakdown.is_empty()
        || !returns.surface_breakdown.is_empty()
        || returns.penalty_breakdown
        || returns.explain_cost_derivation
}

#[derive(Debug, Clone)]
pub(crate) struct HopSelectionInfo {
    pub(crate) fallback_used: bool,
    pub(crate) origin_hop_distance_m: Option<f64>,
    pub(crate) destination_hop_distance_m: Option<f64>,
    pub(crate) hop_segments: Vec<RouteHopSegment>,
    pub(crate) diagnostics: Vec<AnalysisDiagnostic>,
    pub(crate) warnings: Vec<String>,
}

pub(crate) struct RoutePathAnalysis {
    pub(crate) summary: RouteSummary,
    pub(crate) violations: Vec<RouteViolation>,
    pub(crate) segment_violation_types: HashMap<usize, Option<RouteViolationType>>,
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn merge_warnings(mut warnings: Vec<String>, extra: Vec<String>) -> Vec<String> {
    warnings.extend(extra);
    warnings
}

pub(crate) fn analyze_route_path(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    fallback: &FallbackPolicy,
    path: &RoutePath,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> Result<RoutePathAnalysis> {
    let mut network_distance_m = 0_u64;
    let mut network_travel_time_s = 0.0;
    let mut network_generalized_cost = 0.0;
    let mut penalty_s = 0.0;
    let mut penalty_cost = 0.0;
    let mut previous_edge_index = None;
    let mut automaton_state = 0_usize;
    let mut violations = Vec::new();
    let mut violation_types = std::collections::BTreeSet::<RouteViolationType>::new();
    let mut segment_violation_types = HashMap::<usize, Option<RouteViolationType>>::new();
    let first_edge = path.edge_indexes.first().copied();
    let last_edge = path.edge_indexes.last().copied();

    for &edge_index in &path.edge_indexes {
        let edge = topology.routing_edge(edge_index);
        let metric = &metrics.edge_metrics[edge_index];
        let factor = edge_traversal_factor(edge_index, first_edge, last_edge, origin, destination);
        network_distance_m += (edge.length_m as f64 * factor).round() as u64;
        network_travel_time_s += metric.travel_time_s.unwrap_or_default() * factor;
        network_generalized_cost += metric.generalized_cost.unwrap_or_default() * factor;

        if let Some(original_edge_index) = routing_graph
            .virtual_reverse_of
            .get(edge_index)
            .and_then(|value| *value)
        {
            let route_violation_type = RouteViolationType::ReverseOneway;
            let edge_penalty_s = fallback
                .penalties
                .reverse_oneway_penalty_s
                .unwrap_or_default();
            let edge_penalty_cost = edge_penalty_s * metrics.turn_costs.cost_time_weight;
            penalty_s += edge_penalty_s;
            penalty_cost += edge_penalty_cost;
            violations.push(RouteViolation {
                violation_type: route_violation_type,
                edge_id: Some(topology.routing_edge(original_edge_index).edge_id.0),
                from_edge_id: None,
                to_edge_id: None,
                distance_m: Some(edge.length_m as f64 * factor),
                penalty_s: edge_penalty_s,
                penalty_generalized_cost: edge_penalty_cost,
            });
            violation_types.insert(route_violation_type);
            segment_violation_types.insert(edge_index, Some(route_violation_type));
        } else {
            segment_violation_types.entry(edge_index).or_insert(None);
        }

        if let Some(previous_edge_index) = previous_edge_index {
            let turn_penalty_s =
                turn_penalty_seconds(topology, metrics, previous_edge_index, edge_index);
            network_travel_time_s += turn_penalty_s;
            network_generalized_cost += turn_penalty_s * metrics.turn_costs.cost_time_weight;
            if let Some(sequence_len) = routing_graph
                .automaton
                .prohibited_sequence_len(automaton_state, edge_index)
            {
                let (violation_type, local_penalty_s) = if fallback.ignore_turn_restrictions {
                    (
                        RouteViolationType::IgnoredTurnRestriction,
                        fallback
                            .penalties
                            .ignored_turn_restriction_penalty_s
                            .unwrap_or_default(),
                    )
                } else if sequence_len == 2
                    && classify_turn(topology, previous_edge_index, edge_index)
                        == TurnDirection::Uturn
                    && fallback.allow_uturn_where_normally_forbidden
                {
                    (
                        RouteViolationType::ForbiddenUturn,
                        fallback
                            .penalties
                            .forbidden_uturn_penalty_s
                            .or(fallback.penalties.illegal_turn_penalty_s)
                            .unwrap_or_default(),
                    )
                } else if sequence_len == 2 && fallback.allow_illegal_turn {
                    (
                        RouteViolationType::IllegalTurn,
                        fallback
                            .penalties
                            .illegal_turn_penalty_s
                            .unwrap_or_default(),
                    )
                } else {
                    (RouteViolationType::IgnoredTurnRestriction, 0.0)
                };
                let local_penalty_cost = local_penalty_s * metrics.turn_costs.cost_time_weight;
                penalty_s += local_penalty_s;
                penalty_cost += local_penalty_cost;
                violations.push(RouteViolation {
                    violation_type,
                    edge_id: None,
                    from_edge_id: Some(topology.routing_edge(previous_edge_index).edge_id.0),
                    to_edge_id: Some(topology.routing_edge(edge_index).edge_id.0),
                    distance_m: None,
                    penalty_s: local_penalty_s,
                    penalty_generalized_cost: local_penalty_cost,
                });
                violation_types.insert(violation_type);
            }
        }
        automaton_state = routing_graph
            .automaton
            .transition(automaton_state, edge_index);
        previous_edge_index = Some(edge_index);
    }

    let reverse_distance_m = violations
        .iter()
        .filter(|violation| violation.violation_type == RouteViolationType::ReverseOneway)
        .filter_map(|violation| violation.distance_m)
        .sum::<f64>();
    if let Some(max_illegal_distance_m) = fallback.max_illegal_distance_m
        && reverse_distance_m > max_illegal_distance_m + f64::EPSILON
    {
        return Err(AnalysisFailure::new(
                format!(
                    "degraded route exceeds fallback.max_illegal_distance_m ({:.1} m > {:.1} m)",
                    reverse_distance_m, max_illegal_distance_m
                ),
                AnalysisOutcome::Unreachable,
                vec![AnalysisDiagnostic {
                    code: AnalysisDiagnosticCode::FallbackUsed,
                    severity: AnalysisDiagnosticSeverity::Error,
                    message: format!(
                        "The selected degraded route required {:.1} m of reverse-oneway travel, exceeding fallback.max_illegal_distance_m={:.1}.",
                        reverse_distance_m, max_illegal_distance_m
                    ),
                    point_ids: vec![origin.point_id.clone(), destination.point_id.clone()],
                    component_ids: Vec::new(),
                    suggested_actions: vec![
                        "Increase fallback.max_illegal_distance_m only if this degraded behavior is still acceptable.".to_string(),
                        "Disable allow_reverse_oneway to keep the route strictly legal.".to_string(),
                    ],
                }],
            )
            .into());
    }
    if let Some(max_illegal_turns) = fallback.max_illegal_turns {
        let illegal_turn_count = violations
            .iter()
            .filter(|violation| violation.violation_type != RouteViolationType::ReverseOneway)
            .count() as u32;
        if illegal_turn_count > max_illegal_turns {
            return Err(AnalysisFailure::new(
                format!(
                    "degraded route exceeds fallback.max_illegal_turns ({} > {})",
                    illegal_turn_count, max_illegal_turns
                ),
                AnalysisOutcome::Unreachable,
                vec![AnalysisDiagnostic {
                    code: AnalysisDiagnosticCode::FallbackUsed,
                    severity: AnalysisDiagnosticSeverity::Error,
                    message: format!(
                        "The selected degraded route required {} illegal turn or restriction override(s), exceeding fallback.max_illegal_turns={}.",
                        illegal_turn_count, max_illegal_turns
                    ),
                    point_ids: vec![origin.point_id.clone(), destination.point_id.clone()],
                    component_ids: Vec::new(),
                    suggested_actions: vec![
                        "Increase fallback.max_illegal_turns only if this degraded behavior is still acceptable.".to_string(),
                        "Disable turn-related fallback flags to keep the route strictly legal.".to_string(),
                    ],
                }],
            )
            .into());
        }
    }

    let mut warnings = Vec::new();
    if has_failure_modes(fallback) {
        if violations.is_empty() {
            warnings.push(
                "Unsafe failure-mode flags were enabled for this request, but the returned route did not require illegal movements."
                    .to_string(),
            );
        } else {
            warnings.push(
                "Unsafe failure-mode fallback was used; this route is degraded and noncompliant for strict legal routing analysis."
                    .to_string(),
            );
        }
    }

    Ok(RoutePathAnalysis {
        summary: RouteSummary {
            network_distance_m,
            network_travel_time_s,
            network_generalized_cost,
            illegal_movement_penalty_s: penalty_s,
            illegal_movement_penalty_cost: penalty_cost,
            violation_count: violations.len(),
            violation_types: violation_types.into_iter().collect(),
            total_distance_m: network_distance_m,
            total_travel_time_s: network_travel_time_s + penalty_s,
            total_generalized_cost: network_generalized_cost + penalty_cost,
            segment_count: path.edge_indexes.len(),
        },
        violations,
        segment_violation_types,
        warnings,
    })
}

fn ignored_unreachable_warning() -> String {
    "Connectivity policy ignored an unreachable pair; no legal path geometry or network cost was returned.".to_string()
}

pub(crate) fn ignored_unreachable_route_result(
    route_id: &str,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    failure: AnalysisFailure,
    warnings: Vec<String>,
) -> RouteResult {
    let origin = origin_candidates
        .first()
        .cloned()
        .expect("successful snapping must produce at least one origin candidate");
    let destination = destination_candidates
        .first()
        .cloned()
        .expect("successful snapping must produce at least one destination candidate");
    RouteResult {
        route_id: route_id.to_string(),
        origin,
        destination,
        outcome: AnalysisOutcome::Partial,
        fallback_used: false,
        origin_hop_distance_m: None,
        destination_hop_distance_m: None,
        summary: RouteSummary {
            network_distance_m: 0,
            network_travel_time_s: 0.0,
            network_generalized_cost: 0.0,
            illegal_movement_penalty_s: 0.0,
            illegal_movement_penalty_cost: 0.0,
            violation_count: 0,
            violation_types: Vec::new(),
            total_distance_m: 0,
            total_travel_time_s: 0.0,
            total_generalized_cost: 0.0,
            segment_count: 0,
        },
        node_path: Vec::new(),
        edge_path: Vec::new(),
        geometry: None,
        hop_segments: Vec::new(),
        segments: None,
        breakdowns: None,
        violations: Vec::new(),
        diagnostics: failure.diagnostics,
        warnings: merge_warnings(warnings, vec![ignored_unreachable_warning()]),
        alternatives: Vec::new(),
    }
}

pub(crate) fn hop_info_for_pair(
    origin: &SnappedPoint,
    destination: &SnappedPoint,
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
) -> Option<HopSelectionInfo> {
    let origin_requires_hop = origin.snap_distance_m > snap_max_distance_m;
    let destination_requires_hop = destination.snap_distance_m > snap_max_distance_m;
    let max_hop_distance_m = connectivity
        .max_hop_distance_m
        .unwrap_or(snap_max_distance_m);

    if (origin_requires_hop && origin.snap_distance_m > max_hop_distance_m)
        || (destination_requires_hop && destination.snap_distance_m > max_hop_distance_m)
    {
        return None;
    }

    let allowed = match connectivity.disconnected {
        DisconnectedNetworkMode::Strict | DisconnectedNetworkMode::IgnoreUnreachable => {
            !origin_requires_hop && !destination_requires_hop
        }
        DisconnectedNetworkMode::HopOriginToNearestReachableComponent => !destination_requires_hop,
        DisconnectedNetworkMode::HopDestinationToNearestReachableComponent => !origin_requires_hop,
        DisconnectedNetworkMode::HopEitherEnd => !(origin_requires_hop && destination_requires_hop),
    };
    if !allowed {
        return None;
    }

    let mut hop_segments = Vec::new();
    let mut diagnostics = Vec::new();
    let mut warnings = Vec::new();
    if origin_requires_hop {
        hop_segments.push(RouteHopSegment {
            endpoint: HopEndpoint::Origin,
            distance_m: origin.snap_distance_m,
            geometry: vec![
                [origin.requested_lon, origin.requested_lat],
                [origin.snapped_lon, origin.snapped_lat],
            ],
        });
        diagnostics.push(AnalysisDiagnostic {
            code: AnalysisDiagnosticCode::FallbackUsed,
            severity: AnalysisDiagnosticSeverity::Warning,
            message: format!(
                "Connectivity fallback hopped the origin {:.1} m to reach component {}.",
                origin.snap_distance_m,
                origin.component_id.unwrap_or_default()
            ),
            point_ids: vec![origin.point_id.clone()],
            component_ids: origin.component_id.into_iter().collect(),
            suggested_actions: vec![],
        });
        warnings.push(format!(
            "Connectivity fallback used a non-network origin hop of {:.1} m.",
            origin.snap_distance_m
        ));
    }
    if destination_requires_hop {
        hop_segments.push(RouteHopSegment {
            endpoint: HopEndpoint::Destination,
            distance_m: destination.snap_distance_m,
            geometry: vec![
                [destination.snapped_lon, destination.snapped_lat],
                [destination.requested_lon, destination.requested_lat],
            ],
        });
        diagnostics.push(AnalysisDiagnostic {
            code: AnalysisDiagnosticCode::FallbackUsed,
            severity: AnalysisDiagnosticSeverity::Warning,
            message: format!(
                "Connectivity fallback hopped the destination {:.1} m to reach component {}.",
                destination.snap_distance_m,
                destination.component_id.unwrap_or_default()
            ),
            point_ids: vec![destination.point_id.clone()],
            component_ids: destination.component_id.into_iter().collect(),
            suggested_actions: vec![],
        });
        warnings.push(format!(
            "Connectivity fallback used a non-network destination hop of {:.1} m.",
            destination.snap_distance_m
        ));
    }

    Some(HopSelectionInfo {
        fallback_used: origin_requires_hop || destination_requires_hop,
        origin_hop_distance_m: origin_requires_hop.then_some(origin.snap_distance_m),
        destination_hop_distance_m: destination_requires_hop.then_some(destination.snap_distance_m),
        hop_segments,
        diagnostics,
        warnings,
    })
}

pub(crate) fn no_route_failure(
    topology: &TopologyBundle,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
) -> AnalysisFailure {
    let origin_components = candidate_component_ids(origin_candidates);
    let destination_components = candidate_component_ids(destination_candidates);
    if !origin_components.is_empty()
        && !destination_components.is_empty()
        && origin_components.is_disjoint(&destination_components)
    {
        let component_ids = origin_components
            .union(&destination_components)
            .copied()
            .collect::<Vec<_>>();
        let point_ids = vec![
            origin_candidates
                .first()
                .map(|candidate| candidate.point_id.clone())
                .unwrap_or_else(|| "origin".to_string()),
            destination_candidates
                .first()
                .map(|candidate| candidate.point_id.clone())
                .unwrap_or_else(|| "destination".to_string()),
        ];
        return AnalysisFailure::new(
            "origin and destination snapped to different weakly connected components",
            AnalysisOutcome::Unreachable,
            vec![AnalysisDiagnostic {
                code: AnalysisDiagnosticCode::DisconnectedComponents,
                severity: AnalysisDiagnosticSeverity::Error,
                message: format!(
                    "Origin and destination snapped to different weak components in the legal graph for dataset '{}'.",
                    topology.source_path
                ),
                point_ids,
                component_ids,
                suggested_actions: vec![
                    "Choose points in the same connected subnetwork.".to_string(),
                    "Set connectivity.disconnected to ignore_unreachable or a hop_* mode for exploratory analysis.".to_string(),
                ],
            }],
        );
    }

    AnalysisFailure::new(
        "no route found between the snapped origin and destination",
        AnalysisOutcome::Unreachable,
        vec![AnalysisDiagnostic {
            code: AnalysisDiagnosticCode::LegalRouteUnreachable,
            severity: AnalysisDiagnosticSeverity::Error,
            message: "Snapping succeeded, but no legal route was found between the snapped origin and destination candidates.".to_string(),
            point_ids: vec![
                origin_candidates
                    .first()
                    .map(|candidate| candidate.point_id.clone())
                    .unwrap_or_else(|| "origin".to_string()),
                destination_candidates
                    .first()
                    .map(|candidate| candidate.point_id.clone())
                    .unwrap_or_else(|| "destination".to_string()),
            ],
            component_ids: Vec::new(),
            suggested_actions: vec![
                "Inspect one-way and turn-restriction constraints near the endpoints.".to_string(),
                "If exploratory degraded analysis is acceptable, enable explicit fallback flags in fallback.".to_string(),
            ],
        }],
    )
}

pub(crate) fn candidate_component_ids(
    candidates: &[SnappedPoint],
) -> std::collections::BTreeSet<u32> {
    candidates
        .iter()
        .filter_map(|candidate| candidate.component_id)
        .collect()
}

pub(crate) fn failure_outcome_and_diagnostics(
    error: &anyhow::Error,
) -> (AnalysisOutcome, Vec<AnalysisDiagnostic>) {
    if let Some(failure) = analysis_failure(error) {
        (failure.outcome, failure.diagnostics.clone())
    } else {
        (AnalysisOutcome::Unreachable, Vec::new())
    }
}
