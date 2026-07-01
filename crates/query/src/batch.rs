use std::collections::HashMap;

use anyhow::Result;
use netweevil_core::{CompiledProfileBundle, TopologyBundle};
use netweevil_profile::ReturnConfig;

use crate::*;

pub(crate) fn execute_od_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    document: &OdPairsDocument,
) -> Result<OdResult> {
    let mut snap_cache = HashMap::new();
    let origin_snaps = document
        .pairs
        .iter()
        .map(|pair| {
            cached_snap_candidates(
                &mut snap_cache,
                topology,
                routing_graph,
                &pair.origin,
                document.snap.max_distance_m,
                true,
            )
        })
        .collect::<Vec<_>>();
    let destination_snaps = document
        .pairs
        .iter()
        .map(|pair| {
            cached_snap_candidates(
                &mut snap_cache,
                topology,
                routing_graph,
                &pair.destination,
                document.snap.max_distance_m,
                false,
            )
        })
        .collect::<Vec<_>>();
    let (origin_refs, unique_origin_candidates) = intern_candidate_sets(origin_snaps);
    let (destination_refs, unique_destination_candidates) =
        intern_candidate_sets(destination_snaps);
    let batch_strategy = choose_batch_route_strategy(
        routing_graph,
        unique_origin_candidates.len(),
        count_unique_ok_pairs(&origin_refs, &destination_refs),
    );
    let mut route_cache = HashMap::new();
    let mut origin_tree_cache = HashMap::new();
    let mut pairs = Vec::with_capacity(document.pairs.len());
    let mut succeeded_count = 0_usize;
    let mut ignored_count = 0_usize;

    for ((pair, origin_ref), destination_ref) in document
        .pairs
        .iter()
        .zip(&origin_refs)
        .zip(&destination_refs)
    {
        let route = match (origin_ref, destination_ref) {
            (Ok(origin_set_id), Ok(destination_set_id)) => cached_batch_route_result(
                &mut route_cache,
                &mut origin_tree_cache,
                topology,
                metrics,
                routing_graph,
                batch_strategy,
                "",
                document.snap.max_distance_m,
                &document.connectivity,
                &document.fallback,
                &document.returns,
                &document.alternatives,
                &unique_origin_candidates,
                *origin_set_id,
                &unique_destination_candidates,
                *destination_set_id,
            ),
            (Err(error), _) => Err(anyhow::anyhow!(error.clone())),
            (_, Err(error)) => Err(anyhow::anyhow!(error.clone())),
        };
        match route {
            Ok(route) => {
                let status = batch_status_for_route(&route);
                if matches!(status, BatchItemStatus::Succeeded) {
                    succeeded_count += 1;
                } else {
                    ignored_count += 1;
                }
                let ignored = matches!(status, BatchItemStatus::Ignored);
                let error = batch_ignored_message(&route);
                let alternatives = batch_alternatives_from_route(&route, ignored);
                let geometry = route.geometry;
                let diagnostics = route.diagnostics;
                pairs.push(OdPairResult {
                    pair_id: pair.pair_id.clone(),
                    origin_id: pair.origin.id.clone(),
                    destination_id: pair.destination.id.clone(),
                    status,
                    outcome: route.outcome,
                    fallback_used: route.fallback_used,
                    origin_component_id: route.origin.component_id,
                    destination_component_id: route.destination.component_id,
                    origin_hop_distance_m: route.origin_hop_distance_m,
                    destination_hop_distance_m: route.destination_hop_distance_m,
                    origin_snap_distance_m: Some(route.origin.snap_distance_m),
                    destination_snap_distance_m: Some(route.destination.snap_distance_m),
                    total_distance_m: (!ignored).then_some(route.summary.total_distance_m),
                    total_travel_time_s: (!ignored).then_some(route.summary.total_travel_time_s),
                    total_generalized_cost: (!ignored)
                        .then_some(route.summary.total_generalized_cost),
                    illegal_movement_penalty_s: (!ignored)
                        .then_some(route.summary.illegal_movement_penalty_s),
                    illegal_movement_penalty_cost: (!ignored)
                        .then_some(route.summary.illegal_movement_penalty_cost),
                    violation_count: route.summary.violation_count,
                    violation_types: route.summary.violation_types.clone(),
                    geometry,
                    diagnostics,
                    error,
                    alternatives,
                });
            }
            Err(error) => {
                let (outcome, diagnostics) = failure_outcome_and_diagnostics(&error);
                pairs.push(OdPairResult {
                    pair_id: pair.pair_id.clone(),
                    origin_id: pair.origin.id.clone(),
                    destination_id: pair.destination.id.clone(),
                    status: BatchItemStatus::Failed,
                    outcome,
                    fallback_used: false,
                    origin_component_id: None,
                    destination_component_id: None,
                    origin_hop_distance_m: None,
                    destination_hop_distance_m: None,
                    origin_snap_distance_m: None,
                    destination_snap_distance_m: None,
                    total_distance_m: None,
                    total_travel_time_s: None,
                    total_generalized_cost: None,
                    illegal_movement_penalty_s: None,
                    illegal_movement_penalty_cost: None,
                    violation_count: 0,
                    violation_types: Vec::new(),
                    geometry: None,
                    diagnostics,
                    error: Some(error.to_string()),
                    alternatives: Vec::new(),
                });
            }
        }
    }

    Ok(OdResult {
        pair_count: pairs.len(),
        succeeded_count,
        failed_count: pairs.len() - succeeded_count - ignored_count,
        ignored_count,
        pairs,
        diagnostics: Vec::new(),
        warnings: {
            let mut warnings = vec![
                "Batch OD execution now reuses exact single-source search trees and duplicate snapped solves within the batch; multi-edge restriction cases still fall back to per-pair automaton search.".to_string(),
            ];
            if ignored_count > 0 {
                warnings.push(batch_ignored_unreachable_warning("OD", ignored_count));
            }
            warnings.extend(execution_warnings(metrics));
            warnings
        },
    })
}

pub(crate) fn execute_matrix_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
) -> Result<MatrixResult> {
    let snap_max_distance_m = origins
        .snap
        .max_distance_m
        .max(destinations.snap.max_distance_m);
    let returns = merge_point_set_returns(&origins.returns, &destinations.returns);
    let connectivity =
        merge_point_set_connectivity_policy(&origins.connectivity, &destinations.connectivity);
    let fallback = merge_point_set_fallback_policy(&origins.fallback, &destinations.fallback);
    let alternatives =
        merge_point_set_alternatives(&origins.alternatives, &destinations.alternatives);
    let origin_snaps = presnap_point_set(
        topology,
        routing_graph,
        &origins.points,
        snap_max_distance_m,
        true,
    );
    let (origin_refs, unique_origin_candidates) = intern_candidate_sets(origin_snaps);
    let destination_snaps = presnap_point_set(
        topology,
        routing_graph,
        &destinations.points,
        snap_max_distance_m,
        false,
    );
    let (destination_refs, unique_destination_candidates) =
        intern_candidate_sets(destination_snaps);
    let batch_strategy = choose_batch_route_strategy(
        routing_graph,
        unique_origin_candidates.len(),
        unique_origin_candidates.len() * unique_destination_candidates.len(),
    );
    let mut route_cache = HashMap::new();
    let mut origin_tree_cache = HashMap::new();
    let mut cells = Vec::with_capacity(origins.points.len() * destinations.points.len());
    let mut succeeded_count = 0_usize;
    let mut ignored_count = 0_usize;

    for (origin, origin_ref) in origins.points.iter().zip(&origin_refs) {
        for (destination, destination_ref) in destinations.points.iter().zip(&destination_refs) {
            let route = match (origin_ref, destination_ref) {
                (Ok(origin_set_id), Ok(destination_set_id)) => cached_batch_route_result(
                    &mut route_cache,
                    &mut origin_tree_cache,
                    topology,
                    metrics,
                    routing_graph,
                    batch_strategy,
                    "",
                    snap_max_distance_m,
                    &connectivity,
                    &fallback,
                    &returns,
                    &alternatives,
                    &unique_origin_candidates,
                    *origin_set_id,
                    &unique_destination_candidates,
                    *destination_set_id,
                ),
                (Err(error), _) => Err(anyhow::anyhow!(error.clone())),
                (_, Err(error)) => Err(anyhow::anyhow!(error.clone())),
            };
            match route {
                Ok(route) => {
                    let status = batch_status_for_route(&route);
                    if matches!(status, BatchItemStatus::Succeeded) {
                        succeeded_count += 1;
                    } else {
                        ignored_count += 1;
                    }
                    let ignored = matches!(status, BatchItemStatus::Ignored);
                    let error = batch_ignored_message(&route);
                    let alternatives = batch_alternatives_from_route(&route, ignored);
                    let geometry = route.geometry;
                    let diagnostics = route.diagnostics;
                    cells.push(MatrixCellResult {
                        origin_id: origin.id.clone(),
                        destination_id: destination.id.clone(),
                        status,
                        outcome: route.outcome,
                        fallback_used: route.fallback_used,
                        origin_component_id: route.origin.component_id,
                        destination_component_id: route.destination.component_id,
                        origin_hop_distance_m: route.origin_hop_distance_m,
                        destination_hop_distance_m: route.destination_hop_distance_m,
                        origin_snap_distance_m: Some(route.origin.snap_distance_m),
                        destination_snap_distance_m: Some(route.destination.snap_distance_m),
                        total_distance_m: (!ignored).then_some(route.summary.total_distance_m),
                        total_travel_time_s: (!ignored)
                            .then_some(route.summary.total_travel_time_s),
                        total_generalized_cost: (!ignored)
                            .then_some(route.summary.total_generalized_cost),
                        illegal_movement_penalty_s: (!ignored)
                            .then_some(route.summary.illegal_movement_penalty_s),
                        illegal_movement_penalty_cost: (!ignored)
                            .then_some(route.summary.illegal_movement_penalty_cost),
                        violation_count: route.summary.violation_count,
                        violation_types: route.summary.violation_types.clone(),
                        geometry,
                        diagnostics,
                        error,
                        alternatives,
                    });
                }
                Err(error) => {
                    let (outcome, diagnostics) = failure_outcome_and_diagnostics(&error);
                    cells.push(MatrixCellResult {
                        origin_id: origin.id.clone(),
                        destination_id: destination.id.clone(),
                        status: BatchItemStatus::Failed,
                        outcome,
                        fallback_used: false,
                        origin_component_id: None,
                        destination_component_id: None,
                        origin_hop_distance_m: None,
                        destination_hop_distance_m: None,
                        origin_snap_distance_m: None,
                        destination_snap_distance_m: None,
                        total_distance_m: None,
                        total_travel_time_s: None,
                        total_generalized_cost: None,
                        illegal_movement_penalty_s: None,
                        illegal_movement_penalty_cost: None,
                        violation_count: 0,
                        violation_types: Vec::new(),
                        geometry: None,
                        diagnostics,
                        error: Some(error.to_string()),
                        alternatives: Vec::new(),
                    });
                }
            }
        }
    }

    Ok(MatrixResult {
        origin_count: origins.points.len(),
        destination_count: destinations.points.len(),
        cell_count: cells.len(),
        succeeded_count,
        failed_count: cells.len() - succeeded_count - ignored_count,
        ignored_count,
        cells,
        diagnostics: Vec::new(),
        warnings: {
            let mut warnings = vec![
                "Matrix execution now reuses exact single-source search trees and duplicate snapped solves within the batch; multi-edge restriction cases still fall back to per-cell automaton search.".to_string(),
            ];
            if ignored_count > 0 {
                warnings.push(batch_ignored_unreachable_warning("matrix", ignored_count));
            }
            warnings.extend(execution_warnings(metrics));
            warnings
        },
    })
}

pub fn execute_od(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    document: &OdPairsDocument,
) -> Result<OdResult> {
    if has_failure_modes(&document.fallback) {
        validate_execution_inputs(topology, metrics)?;
        let degraded = cached_failure_mode_bundle(topology, metrics, &document.fallback)?;
        return execute_od_with_graph(
            &degraded.topology,
            &degraded.metrics,
            &degraded.routing_graph,
            document,
        );
    }
    validate_execution_inputs(topology, metrics)?;
    let routing_graph = build_routing_graph(topology, metrics)?;
    execute_od_with_graph(topology, metrics, &routing_graph, document)
}

pub fn execute_matrix(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
) -> Result<MatrixResult> {
    let fallback = merge_point_set_fallback_policy(&origins.fallback, &destinations.fallback);
    if has_failure_modes(&fallback) {
        validate_execution_inputs(topology, metrics)?;
        let degraded = cached_failure_mode_bundle(topology, metrics, &fallback)?;
        return execute_matrix_with_graph(
            &degraded.topology,
            &degraded.metrics,
            &degraded.routing_graph,
            origins,
            destinations,
        );
    }
    validate_execution_inputs(topology, metrics)?;
    let routing_graph = build_routing_graph(topology, metrics)?;
    execute_matrix_with_graph(topology, metrics, &routing_graph, origins, destinations)
}

fn batch_ignored_unreachable_warning(label: &str, ignored_count: usize) -> String {
    format!(
        "Connectivity policy ignored {} unreachable {} item(s); see per-item status='ignored' and outcome='partial'.",
        ignored_count, label
    )
}

fn batch_status_for_route(route: &RouteResult) -> BatchItemStatus {
    if matches!(route.outcome, AnalysisOutcome::Partial) {
        BatchItemStatus::Ignored
    } else {
        BatchItemStatus::Succeeded
    }
}

fn batch_ignored_message(route: &RouteResult) -> Option<String> {
    matches!(route.outcome, AnalysisOutcome::Partial).then(|| {
        route
            .diagnostics
            .first()
            .map(|diagnostic| diagnostic.message.clone())
            .unwrap_or_else(|| "Connectivity policy ignored an unreachable pair.".to_string())
    })
}

fn batch_alternatives_from_route(
    route: &RouteResult,
    ignored: bool,
) -> Vec<BatchAlternativeResult> {
    if ignored {
        return Vec::new();
    }
    route
        .alternatives
        .iter()
        .map(|alternative| BatchAlternativeResult {
            alternative_index: alternative.alternative_index,
            rank: alternative.rank,
            total_distance_m: Some(alternative.summary.total_distance_m),
            total_travel_time_s: Some(alternative.summary.total_travel_time_s),
            total_generalized_cost: Some(alternative.summary.total_generalized_cost),
            geometry: alternative.geometry.clone(),
            violation_count: alternative.summary.violation_count,
            violation_types: alternative.summary.violation_types.clone(),
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct BatchSnapCandidateKey {
    snapped_edge_id: u32,
    snapped_edge_fraction_bits: u64,
    snapped_node_id: u32,
    snap_distance_bits: u64,
}

fn batch_snap_candidate_key(candidate: &SnappedPoint) -> BatchSnapCandidateKey {
    BatchSnapCandidateKey {
        snapped_edge_id: candidate.snapped_edge_id.unwrap_or(u32::MAX),
        snapped_edge_fraction_bits: candidate
            .snapped_edge_fraction
            .unwrap_or_default()
            .to_bits(),
        snapped_node_id: candidate.snapped_node_id,
        snap_distance_bits: candidate.snap_distance_m.to_bits(),
    }
}

fn batch_candidate_set_key(candidates: &[SnappedPoint]) -> Vec<BatchSnapCandidateKey> {
    candidates.iter().map(batch_snap_candidate_key).collect()
}

pub(crate) fn intern_candidate_sets(
    candidate_sets: Vec<std::result::Result<Vec<SnappedPoint>, AnalysisFailure>>,
) -> (
    Vec<std::result::Result<usize, AnalysisFailure>>,
    Vec<Vec<SnappedPoint>>,
) {
    let mut unique = Vec::new();
    let mut interned = HashMap::new();
    let mut refs = Vec::with_capacity(candidate_sets.len());

    for candidates in candidate_sets {
        match candidates {
            Ok(candidates) => {
                let key = batch_candidate_set_key(&candidates);
                if let Some(&set_id) = interned.get(&key) {
                    refs.push(Ok(set_id));
                    continue;
                }
                let set_id = unique.len();
                interned.insert(key, set_id);
                unique.push(candidates);
                refs.push(Ok(set_id));
            }
            Err(error) => refs.push(Err(error)),
        }
    }

    (refs, unique)
}

fn cached_batch_route_result(
    cache: &mut HashMap<(usize, usize), std::result::Result<RouteResult, AnalysisFailure>>,
    origin_tree_cache: &mut HashMap<(u32, u64, u64), Result<SingleSourceEdgeTree, String>>,
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    strategy: BatchRouteStrategy,
    route_id: &str,
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    fallback: &FallbackPolicy,
    returns: &ReturnConfig,
    alternatives: &AlternativeRouteOptions,
    origin_candidates: &[Vec<SnappedPoint>],
    origin_set_id: usize,
    destination_candidates: &[Vec<SnappedPoint>],
    destination_set_id: usize,
) -> Result<RouteResult> {
    let key = (origin_set_id, destination_set_id);
    let value = cache.entry(key).or_insert_with(|| {
        execute_batched_route_with_candidates(
            origin_tree_cache,
            topology,
            metrics,
            routing_graph,
            strategy,
            route_id,
            snap_max_distance_m,
            connectivity,
            fallback,
            returns,
            alternatives,
            &origin_candidates[origin_set_id],
            &destination_candidates[destination_set_id],
        )
        .map_err(|error| {
            analysis_failure(&error).cloned().unwrap_or_else(|| {
                AnalysisFailure::new(error.to_string(), AnalysisOutcome::Unreachable, Vec::new())
            })
        })
    });
    value.clone().map_err(anyhow::Error::new)
}

fn execute_batched_route_with_candidates(
    origin_tree_cache: &mut HashMap<(u32, u64, u64), Result<SingleSourceEdgeTree, String>>,
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    strategy: BatchRouteStrategy,
    route_id: &str,
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    fallback: &FallbackPolicy,
    returns: &ReturnConfig,
    alternatives: &AlternativeRouteOptions,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
) -> Result<RouteResult> {
    if !matches!(strategy, BatchRouteStrategy::SingleSource)
        || routing_graph.has_restriction_sequences()
        || has_failure_modes(fallback)
        || auto_relaxation_requested(fallback)
        || alternatives.max_routes > 1
    {
        return execute_route_with_candidates(
            topology,
            metrics,
            routing_graph,
            route_id,
            snap_max_distance_m,
            connectivity,
            fallback,
            returns,
            alternatives,
            origin_candidates,
            destination_candidates,
            None,
        );
    }

    for origin in origin_candidates {
        let tree =
            cached_single_source_edge_tree(origin_tree_cache, topology, routing_graph, origin)?;
        for destination in destination_candidates {
            if same_edge_reverse_pair(origin, destination) {
                continue;
            }
            let Some(hop_info) =
                hop_info_for_pair(origin, destination, snap_max_distance_m, connectivity)
            else {
                continue;
            };
            let path = best_path_from_origin_tree(routing_graph, tree, origin, destination);
            if let Some(path) = path {
                let path =
                    finalize_route_path(topology, metrics, path.edge_indexes, origin, destination);
                let analysis = analyze_route_path(
                    topology,
                    metrics,
                    routing_graph,
                    fallback,
                    &path,
                    origin,
                    destination,
                )?;
                return Ok(RouteResult {
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
                    node_path: Vec::new(),
                    edge_path: Vec::new(),
                    geometry: match returns.geometry {
                        netweevil_profile::ReturnGeometry::None => None,
                        _ => Some(build_route_geometry(
                            topology,
                            &path.edge_indexes,
                            origin,
                            destination,
                        )),
                    },
                    hop_segments: hop_info.hop_segments,
                    segments: if returns.segment_rows {
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
                                        origin,
                                        destination,
                                    );
                                    RouteSegment {
                                        edge_id: edge.edge_id.0,
                                        from_node_id: edge.from.0,
                                        to_node_id: edge.to.0,
                                        source_way_id: edge.source_way_id,
                                        length_m: (edge.length_m as f64 * factor).round() as u32,
                                        travel_time_s: metric.travel_time_s.unwrap_or_default()
                                            * factor,
                                        generalized_cost: metric
                                            .generalized_cost
                                            .unwrap_or_default()
                                            * factor,
                                        road_class: edge.road_class,
                                        surface: edge.surface,
                                        name: edge
                                            .name_index
                                            .and_then(|index| topology.names.get(index as usize))
                                            .cloned(),
                                        violation_type: analysis
                                            .segment_violation_types
                                            .get(&edge_index)
                                            .copied()
                                            .flatten(),
                                    }
                                })
                                .collect(),
                        )
                    } else {
                        None
                    },
                    breakdowns: build_breakdowns(topology, metrics, &path.edge_indexes, returns),
                    violations: analysis.violations,
                    diagnostics: hop_info.diagnostics,
                    warnings: merge_warnings(
                        merge_warnings(execution_warnings(metrics), analysis.warnings),
                        hop_info.warnings,
                    ),
                    alternatives: Vec::new(),
                });
            }
        }
    }

    let failure = no_route_failure(topology, origin_candidates, destination_candidates);
    if matches!(
        connectivity.disconnected,
        DisconnectedNetworkMode::IgnoreUnreachable
    ) && matches!(failure.outcome, AnalysisOutcome::Unreachable)
    {
        return Ok(ignored_unreachable_route_result(
            route_id,
            origin_candidates,
            destination_candidates,
            failure,
            execution_warnings(metrics),
        ));
    }

    Err(failure.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchRouteStrategy {
    Pairwise,
    SingleSource,
}

fn choose_batch_route_strategy(
    routing_graph: &RoutingGraph,
    unique_origin_count: usize,
    unique_pair_count: usize,
) -> BatchRouteStrategy {
    if routing_graph.has_restriction_sequences() || unique_origin_count == 0 {
        return BatchRouteStrategy::Pairwise;
    }

    let average_destination_count =
        unique_pair_count.saturating_add(unique_origin_count - 1) / unique_origin_count;
    // A single-source tree costs one exhaustive Dijkstra per origin while a
    // pairwise query costs one point-to-point search per pair. Both are exact
    // on the same weights, so this is purely a cost crossover: CCH pairwise
    // searches are roughly 45x faster than plain ones, so with acceleration
    // available the tree only wins on much wider destination fan-outs.
    let single_source_threshold = if routing_graph.acceleration.is_some() {
        48
    } else {
        8
    };
    if average_destination_count >= single_source_threshold {
        BatchRouteStrategy::SingleSource
    } else {
        BatchRouteStrategy::Pairwise
    }
}

fn count_unique_ok_pairs(
    origin_refs: &[std::result::Result<usize, AnalysisFailure>],
    destination_refs: &[std::result::Result<usize, AnalysisFailure>],
) -> usize {
    let mut unique = HashMap::<(usize, usize), ()>::new();
    for (origin_ref, destination_ref) in origin_refs.iter().zip(destination_refs) {
        if let (Ok(origin_set_id), Ok(destination_set_id)) = (origin_ref, destination_ref) {
            unique.insert((*origin_set_id, *destination_set_id), ());
        }
    }
    unique.len()
}

fn cached_single_source_edge_tree<'a>(
    cache: &'a mut HashMap<(u32, u64, u64), Result<SingleSourceEdgeTree, String>>,
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    origin: &SnappedPoint,
) -> Result<&'a SingleSourceEdgeTree> {
    let key = snap_cache_key(origin);
    let value = cache.entry(key).or_insert_with(|| {
        build_single_source_edge_tree(topology, routing_graph, origin)
            .map_err(|error| error.to_string())
    });
    match value {
        Ok(tree) => Ok(tree),
        Err(error) => Err(anyhow::Error::msg(error.clone())),
    }
}

fn build_single_source_edge_tree(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    origin: &SnappedPoint,
) -> Result<SingleSourceEdgeTree> {
    let _ = topology;
    let origin_seeds = origin_edge_seeds(routing_graph, origin);
    SINGLE_SOURCE_SEARCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.prepare(routing_graph.edge_costs.len());

        for (edge_index, cost) in origin_seeds {
            if !scratch.update(edge_index, cost, NO_PREVIOUS_EDGE) {
                continue;
            }
            scratch.heap.push(State {
                edge_index,
                automaton_state: 0,
                cost,
                score: cost,
            });
        }

        while let Some(State {
            edge_index,
            automaton_state: _,
            cost,
            score: _,
        }) = scratch.heap.pop()
        {
            if cost > scratch.dist[edge_index] {
                continue;
            }
            for transition_index in routing_graph.transition_range(edge_index) {
                let next_edge = routing_graph.transition_edges[transition_index] as usize;
                let next_cost = cost + routing_graph.transition_costs[transition_index];
                if !scratch.update(next_edge, next_cost, edge_index as u32) {
                    continue;
                }
                scratch.heap.push(State {
                    edge_index: next_edge,
                    automaton_state: 0,
                    cost: next_cost,
                    score: next_cost,
                });
            }
        }

        Ok(SingleSourceEdgeTree {
            dist: scratch.dist.clone(),
            previous: scratch.previous.clone(),
        })
    })
}

fn best_path_from_origin_tree(
    routing_graph: &RoutingGraph,
    tree: &SingleSourceEdgeTree,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> Option<RoutePath> {
    let mut best_path = direct_same_edge_path(routing_graph, origin, destination);
    let mut best_cost = best_path
        .as_ref()
        .map(|path| path.total_generalized_cost)
        .unwrap_or(f64::INFINITY);
    let mut best_edge = None;

    for (edge_index, adjustment) in destination_edge_seeds(routing_graph, destination) {
        let base_cost = tree.dist.get(edge_index).copied().unwrap_or(f64::INFINITY);
        if !base_cost.is_finite() {
            continue;
        }
        let total_cost = base_cost + adjustment;
        if total_cost < best_cost {
            best_cost = total_cost;
            best_edge = Some(edge_index);
        }
    }

    if let Some(edge_index) = best_edge {
        best_path = Some(reconstruct_single_source_route_path(
            tree, edge_index, best_cost,
        ));
    }

    best_path
}

fn reconstruct_single_source_route_path(
    tree: &SingleSourceEdgeTree,
    target_edge: usize,
    total_generalized_cost: f64,
) -> RoutePath {
    let mut edge_indexes = Vec::new();
    let mut cursor = target_edge;
    loop {
        edge_indexes.push(cursor);
        let previous_edge = tree.previous[cursor];
        if previous_edge == NO_PREVIOUS_EDGE {
            break;
        }
        cursor = previous_edge as usize;
    }
    edge_indexes.reverse();

    RoutePath {
        edge_indexes,
        total_generalized_cost,
    }
}
