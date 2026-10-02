use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, bail};
use netweevil_core::{CompiledProfileBundle, TopologyBundle};
use netweevil_profile::ReturnConfig;
use rayon::prelude::*;

use crate::*;

pub(crate) fn execute_od_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    document: &OdPairsDocument,
) -> Result<OdResult> {
    if document.temporal.requires_exact_labels() {
        let temporal_routing_graph;
        let routing_graph = if document.temporal.is_temporal()
            && !routing_graph.includes_temporal_materialized_directions
        {
            temporal_routing_graph = build_temporal_routing_graph(topology, metrics)?;
            &temporal_routing_graph
        } else {
            routing_graph
        };
        return execute_od_per_pair_exact(topology, metrics, routing_graph, document);
    }
    let origin_points = document
        .pairs
        .iter()
        .map(|pair| pair.origin.clone())
        .collect::<Vec<_>>();
    let destination_points = document
        .pairs
        .iter()
        .map(|pair| pair.destination.clone())
        .collect::<Vec<_>>();
    let origin_snaps = presnap_points_parallel(
        topology,
        routing_graph,
        &origin_points,
        &document.snap,
        true,
    );
    let destination_snaps = presnap_points_parallel(
        topology,
        routing_graph,
        &destination_points,
        &document.snap,
        false,
    );
    let (origin_refs, unique_origin_candidates) = intern_candidate_sets(origin_snaps);
    let (destination_refs, unique_destination_candidates) =
        intern_candidate_sets(destination_snaps);
    let context = BatchRouteContext {
        topology,
        metrics,
        routing_graph,
        strategy: choose_batch_route_strategy(
            routing_graph,
            unique_origin_candidates.len(),
            count_unique_ok_pairs(&origin_refs, &destination_refs),
        ),
        snap_max_distance_m: document.snap.max_distance_m,
        connectivity: &document.connectivity,
        fallback: &document.fallback,
        returns: &document.returns,
        alternatives: &document.alternatives,
        origin_candidates: &unique_origin_candidates,
        destination_candidates: &unique_destination_candidates,
    };
    let pairs = resolve_batch_routes(
        &context,
        document.pairs.len(),
        |index| (&origin_refs[index], &destination_refs[index]),
        |index, route| od_pair_result(&document.pairs[index], route),
    );
    let succeeded_count = pairs
        .iter()
        .filter(|pair| matches!(pair.status, BatchItemStatus::Succeeded))
        .count();
    let ignored_count = pairs
        .iter()
        .filter(|pair| matches!(pair.status, BatchItemStatus::Ignored))
        .count();

    let provenance = temporal_result_provenance(&document.temporal)?;
    Ok(OdResult {
        departure_time: provenance.departure_time,
        arrive_by: provenance.arrive_by,
        scenario_id: provenance.scenario_id,
        pair_count: pairs.len(),
        succeeded_count,
        failed_count: pairs.len() - succeeded_count - ignored_count,
        ignored_count,
        pairs,
        diagnostics: Vec::new(),
        warnings: {
            let mut warnings = Vec::new();
            if ignored_count > 0 {
                warnings.push(batch_ignored_unreachable_warning("OD", ignored_count));
            }
            warnings
        },
    })
}

fn validate_matrix_point_constraints(
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
) -> Result<()> {
    let unrestricted = PointSnapConstraint::default();
    for (points, other) in [(origins, destinations), (destinations, origins)] {
        for point in &points.points {
            let own = points
                .snap
                .point_constraints
                .get(&point.id)
                .unwrap_or(&unrestricted);
            if other
                .snap
                .point_constraints
                .get(&point.id)
                .is_some_and(|constraint| constraint != own)
            {
                bail!(
                    "origin and destination point sets specify conflicting snap constraints for point ID '{}'",
                    point.id
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn execute_matrix_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
) -> Result<MatrixResult> {
    validate_matrix_point_constraints(origins, destinations)?;
    if origins.temporal.requires_exact_labels() || destinations.temporal.requires_exact_labels() {
        let is_temporal = origins.temporal.is_temporal() || destinations.temporal.is_temporal();
        let temporal_routing_graph;
        let routing_graph =
            if is_temporal && !routing_graph.includes_temporal_materialized_directions {
                temporal_routing_graph = build_temporal_routing_graph(topology, metrics)?;
                &temporal_routing_graph
            } else {
                routing_graph
            };
        return execute_matrix_per_pair_exact(
            topology,
            metrics,
            routing_graph,
            origins,
            destinations,
        );
    }
    // Cells are fully materialized in memory; refuse unbounded results
    // instead of exhausting memory on oversized requests.
    const MAX_MATRIX_CELLS: usize = 4_000_000;
    let requested_cells = origins
        .points
        .len()
        .saturating_mul(destinations.points.len());
    if requested_cells > MAX_MATRIX_CELLS {
        bail!(
            "matrix request would produce {requested_cells} cells (origins x destinations), above the {MAX_MATRIX_CELLS} safety limit; split the request into chunks"
        );
    }
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
    let origin_snaps = presnap_points_parallel(
        topology,
        routing_graph,
        &origins.points,
        &origins.snap,
        true,
    );
    let (origin_refs, unique_origin_candidates) = intern_candidate_sets(origin_snaps);
    let destination_snaps = presnap_points_parallel(
        topology,
        routing_graph,
        &destinations.points,
        &destinations.snap,
        false,
    );
    let (destination_refs, unique_destination_candidates) =
        intern_candidate_sets(destination_snaps);
    let context = BatchRouteContext {
        topology,
        metrics,
        routing_graph,
        strategy: choose_batch_route_strategy(
            routing_graph,
            unique_origin_candidates.len(),
            unique_origin_candidates.len() * unique_destination_candidates.len(),
        ),
        snap_max_distance_m,
        connectivity: &connectivity,
        fallback: &fallback,
        returns: &returns,
        alternatives: &alternatives,
        origin_candidates: &unique_origin_candidates,
        destination_candidates: &unique_destination_candidates,
    };
    let destination_count = destinations.points.len();
    let cells = resolve_batch_routes(
        &context,
        requested_cells,
        |index| {
            (
                &origin_refs[index / destination_count],
                &destination_refs[index % destination_count],
            )
        },
        |index, route| {
            matrix_cell_result(
                &origins.points[index / destination_count].id,
                &destinations.points[index % destination_count].id,
                route,
            )
        },
    );
    let succeeded_count = cells
        .iter()
        .filter(|cell| matches!(cell.status, BatchItemStatus::Succeeded))
        .count();
    let ignored_count = cells
        .iter()
        .filter(|cell| matches!(cell.status, BatchItemStatus::Ignored))
        .count();

    Ok(MatrixResult {
        departure_time: None,
        arrive_by: None,
        scenario_id: None,
        origin_count: origins.points.len(),
        destination_count: destinations.points.len(),
        cell_count: cells.len(),
        succeeded_count,
        failed_count: cells.len() - succeeded_count - ignored_count,
        ignored_count,
        cells,
        diagnostics: Vec::new(),
        warnings: {
            let mut warnings = Vec::new();
            if ignored_count > 0 {
                warnings.push(batch_ignored_unreachable_warning("matrix", ignored_count));
            }
            warnings
        },
    })
}

/// Snaps every point independently and in parallel; duplicate coordinates
/// are interned afterwards by candidate identity.
fn presnap_points_parallel(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    points: &[LabeledPoint],
    options: &SnapOptions,
    is_origin: bool,
) -> Vec<std::result::Result<Vec<SnappedPoint>, AnalysisFailure>> {
    points
        .par_iter()
        .map(|point| {
            snap_candidates_with_options(topology, routing_graph, point, options, is_origin)
                .map_err(|_| route_snap_failure(point, options.max_distance_m))
        })
        .collect()
}

fn od_pair_result(
    pair: &OdPair,
    route: std::result::Result<&RouteResult, &anyhow::Error>,
) -> OdPairResult {
    let cell = matrix_cell_result(&pair.origin.id, &pair.destination.id, route);
    OdPairResult {
        pair_id: pair.pair_id.clone(),
        origin_id: cell.origin_id,
        destination_id: cell.destination_id,
        status: cell.status,
        outcome: cell.outcome,
        fallback_used: cell.fallback_used,
        origin_component_id: cell.origin_component_id,
        destination_component_id: cell.destination_component_id,
        origin_hop_distance_m: cell.origin_hop_distance_m,
        destination_hop_distance_m: cell.destination_hop_distance_m,
        origin_snap_distance_m: cell.origin_snap_distance_m,
        destination_snap_distance_m: cell.destination_snap_distance_m,
        total_distance_m: cell.total_distance_m,
        total_travel_time_s: cell.total_travel_time_s,
        total_generalized_cost: cell.total_generalized_cost,
        components: cell.components,
        illegal_movement_penalty_s: cell.illegal_movement_penalty_s,
        illegal_movement_penalty_cost: cell.illegal_movement_penalty_cost,
        violation_count: cell.violation_count,
        violation_types: cell.violation_types,
        geometry: cell.geometry,
        diagnostics: cell.diagnostics,
        error: cell.error,
        alternatives: cell.alternatives,
    }
}

fn matrix_cell_result(
    origin_id: &str,
    destination_id: &str,
    route: std::result::Result<&RouteResult, &anyhow::Error>,
) -> MatrixCellResult {
    match route {
        Ok(route) => {
            let status = batch_status_for_route(route);
            let ignored = matches!(status, BatchItemStatus::Ignored);
            MatrixCellResult {
                origin_id: origin_id.to_string(),
                destination_id: destination_id.to_string(),
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
                total_generalized_cost: (!ignored).then_some(route.summary.total_generalized_cost),
                components: if ignored {
                    BTreeMap::new()
                } else {
                    route.summary.components.clone()
                },
                illegal_movement_penalty_s: (!ignored)
                    .then_some(route.summary.illegal_movement_penalty_s),
                illegal_movement_penalty_cost: (!ignored)
                    .then_some(route.summary.illegal_movement_penalty_cost),
                violation_count: route.summary.violation_count,
                violation_types: route.summary.violation_types.clone(),
                geometry: route.geometry.clone(),
                diagnostics: route.diagnostics.clone(),
                error: batch_ignored_message(route),
                alternatives: batch_alternatives_from_route(route, ignored),
            }
        }
        Err(error) => {
            let (outcome, diagnostics) = failure_outcome_and_diagnostics(error);
            MatrixCellResult {
                origin_id: origin_id.to_string(),
                destination_id: destination_id.to_string(),
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
                components: BTreeMap::new(),
                illegal_movement_penalty_s: None,
                illegal_movement_penalty_cost: None,
                violation_count: 0,
                violation_types: Vec::new(),
                geometry: None,
                diagnostics,
                error: Some(error.to_string()),
                alternatives: Vec::new(),
            }
        }
    }
}

fn execute_od_per_pair_exact(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    document: &OdPairsDocument,
) -> Result<OdResult> {
    let mut pairs = Vec::with_capacity(document.pairs.len());
    let mut succeeded_count = 0;
    let mut ignored_count = 0;
    for pair in &document.pairs {
        let request = RouteRequest {
            route_id: pair.pair_id.clone(),
            origin: pair.origin.clone(),
            destination: pair.destination.clone(),
            snap: document.snap.clone(),
            connectivity: document.connectivity.clone(),
            fallback: document.fallback.clone(),
            returns: document.returns.clone(),
            alternatives: document.alternatives.clone(),
            temporal: document.temporal.clone(),
        };
        match execute_route_with_graph(topology, metrics, routing_graph, &request, None) {
            Ok(route) => {
                let status = batch_status_for_route(&route);
                if matches!(status, BatchItemStatus::Succeeded) {
                    succeeded_count += 1;
                } else {
                    ignored_count += 1;
                }
                let ignored = matches!(status, BatchItemStatus::Ignored);
                let alternatives = batch_alternatives_from_route(&route, ignored);
                let error = batch_ignored_message(&route);
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
                    components: if ignored {
                        Default::default()
                    } else {
                        route.summary.components.clone()
                    },
                    illegal_movement_penalty_s: (!ignored)
                        .then_some(route.summary.illegal_movement_penalty_s),
                    illegal_movement_penalty_cost: (!ignored)
                        .then_some(route.summary.illegal_movement_penalty_cost),
                    violation_count: route.summary.violation_count,
                    violation_types: route.summary.violation_types.clone(),
                    geometry: route.geometry,
                    diagnostics: route.diagnostics,
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
                    components: BTreeMap::new(),
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
    let provenance = temporal_result_provenance(&document.temporal)?;
    Ok(OdResult {
        departure_time: provenance.departure_time,
        arrive_by: provenance.arrive_by,
        scenario_id: provenance.scenario_id,
        pair_count: pairs.len(),
        succeeded_count,
        failed_count: pairs.len() - succeeded_count - ignored_count,
        ignored_count,
        pairs,
        diagnostics: Vec::new(),
        warnings: if ignored_count > 0 {
            vec![batch_ignored_unreachable_warning("OD", ignored_count)]
        } else {
            Vec::new()
        },
    })
}

fn execute_matrix_per_pair_exact(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
) -> Result<MatrixResult> {
    const MAX_MATRIX_CELLS: usize = 4_000_000;
    let requested_cells = origins
        .points
        .len()
        .saturating_mul(destinations.points.len());
    if requested_cells > MAX_MATRIX_CELLS {
        bail!("matrix request exceeds the {MAX_MATRIX_CELLS} cell safety limit");
    }
    let temporal = match (
        origins.temporal.requires_exact_labels(),
        destinations.temporal.requires_exact_labels(),
    ) {
        (true, true) if origins.temporal != destinations.temporal => {
            bail!("origin and destination point sets specify conflicting temporal options")
        }
        (true, _) => origins.temporal.clone(),
        (_, true) => destinations.temporal.clone(),
        _ => TemporalRequestOptions::default(),
    };
    let snap = SnapOptions {
        max_distance_m: origins
            .snap
            .max_distance_m
            .max(destinations.snap.max_distance_m),
        z_window_m: origins.snap.z_window_m.or(destinations.snap.z_window_m),
        attribute_filters: if origins.snap.attribute_filters.is_empty() {
            destinations.snap.attribute_filters.clone()
        } else {
            origins.snap.attribute_filters.clone()
        },
        point_constraints: origins
            .snap
            .point_constraints
            .iter()
            .chain(destinations.snap.point_constraints.iter())
            .map(|(id, constraint)| (id.clone(), constraint.clone()))
            .collect(),
    };
    let connectivity =
        merge_point_set_connectivity_policy(&origins.connectivity, &destinations.connectivity);
    let fallback = merge_point_set_fallback_policy(&origins.fallback, &destinations.fallback);
    let returns = merge_point_set_returns(&origins.returns, &destinations.returns);
    let alternatives =
        merge_point_set_alternatives(&origins.alternatives, &destinations.alternatives);
    let mut cells = Vec::with_capacity(requested_cells);
    let mut succeeded_count = 0;
    let mut ignored_count = 0;
    for origin in &origins.points {
        for destination in &destinations.points {
            let request = RouteRequest {
                route_id: format!("{}__{}", origin.id, destination.id),
                origin: origin.clone(),
                destination: destination.clone(),
                snap: snap.clone(),
                connectivity: connectivity.clone(),
                fallback: fallback.clone(),
                returns: returns.clone(),
                alternatives: alternatives.clone(),
                temporal: temporal.clone(),
            };
            match execute_route_with_graph(topology, metrics, routing_graph, &request, None) {
                Ok(route) => {
                    let status = batch_status_for_route(&route);
                    if matches!(status, BatchItemStatus::Succeeded) {
                        succeeded_count += 1;
                    } else {
                        ignored_count += 1;
                    }
                    let ignored = matches!(status, BatchItemStatus::Ignored);
                    let alternatives = batch_alternatives_from_route(&route, ignored);
                    let error = batch_ignored_message(&route);
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
                        components: if ignored {
                            Default::default()
                        } else {
                            route.summary.components.clone()
                        },
                        illegal_movement_penalty_s: (!ignored)
                            .then_some(route.summary.illegal_movement_penalty_s),
                        illegal_movement_penalty_cost: (!ignored)
                            .then_some(route.summary.illegal_movement_penalty_cost),
                        violation_count: route.summary.violation_count,
                        violation_types: route.summary.violation_types.clone(),
                        geometry: route.geometry,
                        diagnostics: route.diagnostics,
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
                        components: BTreeMap::new(),
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
    let provenance = temporal_result_provenance(&temporal)?;
    Ok(MatrixResult {
        departure_time: provenance.departure_time,
        arrive_by: provenance.arrive_by,
        scenario_id: provenance.scenario_id,
        origin_count: origins.points.len(),
        destination_count: destinations.points.len(),
        cell_count: cells.len(),
        succeeded_count,
        failed_count: cells.len() - succeeded_count - ignored_count,
        ignored_count,
        cells,
        diagnostics: Vec::new(),
        warnings: if ignored_count > 0 {
            vec![batch_ignored_unreachable_warning("matrix", ignored_count)]
        } else {
            Vec::new()
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
        let degraded = cached_failure_mode_bundle(topology, metrics, &document.fallback, None)?;
        return execute_od_with_graph(
            &degraded.topology,
            degraded.metrics.as_ref(),
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
        let degraded = cached_failure_mode_bundle(topology, metrics, &fallback, None)?;
        return execute_matrix_with_graph(
            &degraded.topology,
            degraded.metrics.as_ref(),
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
            components: alternative.summary.components.clone(),
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

/// Request-wide inputs shared by every batch item.
struct BatchRouteContext<'a> {
    topology: &'a TopologyBundle,
    metrics: &'a CompiledProfileBundle,
    routing_graph: &'a RoutingGraph,
    strategy: BatchRouteStrategy,
    snap_max_distance_m: f64,
    connectivity: &'a ConnectivityPolicy,
    fallback: &'a FallbackPolicy,
    returns: &'a ReturnConfig,
    alternatives: &'a AlternativeRouteOptions,
    origin_candidates: &'a [Vec<SnappedPoint>],
    destination_candidates: &'a [Vec<SnappedPoint>],
}

type SnapRef = std::result::Result<usize, AnalysisFailure>;
type CellResult = std::result::Result<RouteResult, AnalysisFailure>;

/// Cells computed by the many-to-many engine, grouped by origin set with
/// ascending destination sets per row.
struct PrecomputedRoutes {
    row_of_origin: Vec<u32>,
    rows: Vec<(usize, Vec<usize>)>,
    results: Vec<Vec<CellResult>>,
}

impl PrecomputedRoutes {
    fn get(&self, origin: usize, destination: usize) -> &CellResult {
        let row = self.row_of_origin[origin] as usize;
        let column = self.rows[row]
            .1
            .binary_search(&destination)
            .expect("precomputed cell present");
        &self.results[row][column]
    }
}

/// Resolves one route per batch item, in item order. Accelerated
/// many-to-many requests compute every unique cell in parallel up front;
/// other strategies route items sequentially with per-request caches.
fn resolve_batch_routes<'a, R: Send>(
    context: &BatchRouteContext<'_>,
    count: usize,
    refs: impl Fn(usize) -> (&'a SnapRef, &'a SnapRef) + Sync,
    build: impl Fn(usize, std::result::Result<&RouteResult, &anyhow::Error>) -> R + Sync,
) -> Vec<R> {
    let use_many_to_many = batchable_request(context.fallback, context.alternatives)
        && matches!(context.strategy, BatchRouteStrategy::AcceleratedManyToMany)
        && context.routing_graph.acceleration.is_some();
    if use_many_to_many {
        let precomputed = precompute_many_to_many(context, count, &refs);
        return (0..count)
            .into_par_iter()
            .map(|index| {
                let route = match refs(index) {
                    (Ok(origin), Ok(destination)) => precomputed
                        .get(*origin, *destination)
                        .as_ref()
                        .map_err(|error| anyhow::Error::new(error.clone())),
                    (Err(error), _) | (_, Err(error)) => Err(anyhow::Error::new(error.clone())),
                };
                build(index, route.as_ref().map(|route| *route))
            })
            .collect();
    }

    let mut route_cache = HashMap::new();
    let mut origin_tree_cache = HashMap::new();
    (0..count)
        .map(|index| {
            let route = match refs(index) {
                (Ok(origin_set_id), Ok(destination_set_id)) => cached_batch_route_result(
                    &mut route_cache,
                    &mut origin_tree_cache,
                    context,
                    *origin_set_id,
                    *destination_set_id,
                ),
                (Err(error), _) | (_, Err(error)) => Err(anyhow::Error::new(error.clone())),
            };
            build(index, route.as_ref())
        })
        .collect()
}

fn precompute_many_to_many<'a>(
    context: &BatchRouteContext<'_>,
    count: usize,
    refs: &(impl Fn(usize) -> (&'a SnapRef, &'a SnapRef) + Sync),
) -> PrecomputedRoutes {
    let mut pairs = (0..count)
        .filter_map(|index| match refs(index) {
            (Ok(origin), Ok(destination)) => Some((*origin, *destination)),
            _ => None,
        })
        .collect::<Vec<_>>();
    pairs.par_sort_unstable();
    pairs.dedup();
    let mut row_of_origin = vec![u32::MAX; context.origin_candidates.len()];
    let mut rows = Vec::<(usize, Vec<usize>)>::new();
    for (origin, destination) in pairs {
        match rows.last_mut() {
            Some((row_origin, destinations)) if *row_origin == origin => {
                destinations.push(destination);
            }
            _ => {
                row_of_origin[origin] = rows.len() as u32;
                rows.push((origin, vec![destination]));
            }
        }
    }
    let results = many_to_many_routes(
        &ManyToManyRequest {
            topology: context.topology,
            metrics: context.metrics,
            routing_graph: context.routing_graph,
            route_id: "",
            snap_max_distance_m: context.snap_max_distance_m,
            connectivity: context.connectivity,
            fallback: context.fallback,
            returns: context.returns,
            origin_sets: context.origin_candidates,
            destination_sets: context.destination_candidates,
        },
        &rows,
    );
    PrecomputedRoutes {
        row_of_origin,
        rows,
        results,
    }
}

fn batchable_request(fallback: &FallbackPolicy, alternatives: &AlternativeRouteOptions) -> bool {
    !has_failure_modes(fallback)
        && !auto_relaxation_requested(fallback)
        && alternatives.max_routes <= 1
}

fn cached_batch_route_result(
    cache: &mut HashMap<(usize, usize), CellResult>,
    origin_tree_cache: &mut HashMap<(u32, u64, u64), Result<SingleSourceEdgeTree, String>>,
    context: &BatchRouteContext<'_>,
    origin_set_id: usize,
    destination_set_id: usize,
) -> Result<RouteResult> {
    let value = cache
        .entry((origin_set_id, destination_set_id))
        .or_insert_with(|| {
            execute_batched_route_with_candidates(
                origin_tree_cache,
                context.topology,
                context.metrics,
                context.routing_graph,
                context.strategy,
                "",
                context.snap_max_distance_m,
                context.connectivity,
                context.fallback,
                context.returns,
                context.alternatives,
                &context.origin_candidates[origin_set_id],
                &context.destination_candidates[destination_set_id],
            )
            .map_err(|error| {
                analysis_failure(&error).cloned().unwrap_or_else(|| {
                    AnalysisFailure::new(
                        error.to_string(),
                        AnalysisOutcome::Unreachable,
                        Vec::new(),
                    )
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
    // Accelerated many-to-many cells are precomputed by
    // `many_to_many_route_cache`; only exact single-source trees remain here.
    let use_single_source = batchable_request(fallback, alternatives)
        && !routing_graph.has_restriction_sequences()
        && matches!(strategy, BatchRouteStrategy::SingleSource);
    if !use_single_source {
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

    let mut attachment: Option<(usize, usize)> = None;
    let mut best: Option<RouteResult> = None;
    {
        for (origin_index, origin) in origin_candidates.iter().enumerate() {
            if let Some((first_origin, _)) = attachment
                && !same_physical_attachment(origin, &origin_candidates[first_origin])
            {
                continue;
            }
            let tree =
                cached_single_source_edge_tree(origin_tree_cache, topology, routing_graph, origin)?;
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
                let Some(hop_info) =
                    hop_info_for_pair(origin, destination, snap_max_distance_m, connectivity)
                else {
                    continue;
                };
                let path = best_path_from_origin_tree(routing_graph, tree, origin, destination);
                if let Some(path) = path {
                    let result = batch_route_result_for_path(
                        topology,
                        metrics,
                        routing_graph,
                        route_id,
                        fallback,
                        returns,
                        origin,
                        destination,
                        hop_info,
                        path,
                    )?;
                    attachment.get_or_insert((origin_index, destination_index));
                    if best.as_ref().is_none_or(|previous| {
                        result.summary.total_generalized_cost
                            < previous.summary.total_generalized_cost
                    }) {
                        best = Some(result);
                    }
                }
            }
        }
    }

    if let Some(result) = best {
        return Ok(result);
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
            Vec::new(),
        ));
    }

    Err(failure.into())
}

pub(crate) fn batch_route_result_for_path(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    route_id: &str,
    fallback: &FallbackPolicy,
    returns: &ReturnConfig,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
    hop_info: HopSelectionInfo,
    path: RoutePath,
) -> Result<RouteResult> {
    let path = finalize_route_path(topology, metrics, path.edge_indexes, origin, destination);
    let analysis = analyze_route_path(
        topology,
        metrics,
        routing_graph,
        fallback,
        &path,
        origin,
        destination,
    )?;
    Ok(RouteResult {
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
                    .enumerate()
                    .map(|(position, &edge_index)| {
                        let edge = topology.edge(edge_index);
                        let metric = &metrics.edge_metrics[edge_index];
                        let factor = edge_traversal_factor(
                            edge_index,
                            position == 0,
                            position + 1 == path.edge_indexes.len(),
                            origin,
                            destination,
                        );
                        RouteSegment {
                            edge_id: edge.edge_id.0,
                            from_node_id: edge.from.0,
                            to_node_id: edge.to.0,
                            source_way_id: edge.source_way_id,
                            length_m: (edge.length_m as f64 * factor).round() as u32,
                            travel_time_s: metric.travel_time_s.unwrap_or_default() * factor,
                            generalized_cost: metric.generalized_cost.unwrap_or_default() * factor,
                            components: static_edge_components(metrics, edge_index, factor),
                            waiting_time_s: 0.0,
                            entry_time: None,
                            exit_time: None,
                            road_class: edge.road_class,
                            surface: edge.surface,
                            name: edge
                                .name_index
                                .and_then(|index| topology.names.get(index as usize))
                                .cloned(),
                            violation_type: analysis
                                .segment_violation_types
                                .get(&edge_index)
                                .copied(),
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
        warnings: merge_warnings(analysis.warnings, hop_info.warnings),
        alternatives: Vec::new(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchRouteStrategy {
    Pairwise,
    SingleSource,
    AcceleratedManyToMany,
}

fn choose_batch_route_strategy(
    routing_graph: &RoutingGraph,
    unique_origin_count: usize,
    unique_pair_count: usize,
) -> BatchRouteStrategy {
    if unique_origin_count == 0 {
        return BatchRouteStrategy::Pairwise;
    }

    let average_destination_count =
        unique_pair_count.saturating_add(unique_origin_count - 1) / unique_origin_count;
    if routing_graph.acceleration.is_some() {
        // Complete forward/backward CCH search spaces cost about one pairwise
        // query per unique endpoint and answer every origin/destination
        // combination with a cheap merge-join, so they win as soon as
        // endpoints are reused a few times.
        if average_destination_count >= 4 {
            BatchRouteStrategy::AcceleratedManyToMany
        } else {
            BatchRouteStrategy::Pairwise
        }
    } else if !routing_graph.has_restriction_sequences() && average_destination_count >= 8 {
        // A single-source tree costs one exhaustive Dijkstra per origin while
        // a pairwise query costs one point-to-point search per pair; the tree
        // wins once each origin serves several destinations.
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

pub(crate) fn build_single_source_edge_tree(
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

pub(crate) fn best_path_from_origin_tree(
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

/// Upper bound on cells materialized at once by [`route_set_matrix`].
const SET_MATRIX_CHUNK_CELLS: usize = 200_000;

/// Routes every origin set to every destination set with default policies
/// and reduces each cell as soon as its chunk of rows is routed, so memory
/// follows the chunk rather than the full matrix. Accelerated graphs use the
/// many-to-many engine.
pub(crate) fn route_set_matrix<T: Send>(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origin_sets: &[Vec<SnappedPoint>],
    destination_sets: &[Vec<SnappedPoint>],
    snap_max_distance_m: f64,
    reduce: impl Fn(usize, usize, std::result::Result<&RouteResult, &anyhow::Error>) -> T + Sync,
) -> Vec<Vec<T>> {
    let columns = destination_sets.len();
    let rows_per_chunk = (SET_MATRIX_CHUNK_CELLS / columns.max(1)).max(1);
    let mut matrix = Vec::with_capacity(origin_sets.len());
    for first_row in (0..origin_sets.len()).step_by(rows_per_chunk) {
        let rows = first_row..(first_row + rows_per_chunk).min(origin_sets.len());
        let pairs = rows
            .clone()
            .flat_map(|row| (0..columns).map(move |column| (row, column)))
            .collect::<Vec<_>>();
        let mut cells = route_set_pairs(
            topology,
            metrics,
            routing_graph,
            origin_sets,
            destination_sets,
            &pairs,
            snap_max_distance_m,
            &ReturnConfig::default(),
            |index, route| reduce(pairs[index].0, pairs[index].1, route),
        )
        .into_iter();
        for _ in rows {
            matrix.push(cells.by_ref().take(columns).collect());
        }
    }
    matrix
}

/// Routes `(origin set, destination set)` index pairs with default policies.
/// Accelerated graphs always use the parallel many-to-many engine.
pub(crate) fn route_set_pairs<R: Send>(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origin_sets: &[Vec<SnappedPoint>],
    destination_sets: &[Vec<SnappedPoint>],
    pairs: &[(usize, usize)],
    snap_max_distance_m: f64,
    returns: &ReturnConfig,
    build: impl Fn(usize, std::result::Result<&RouteResult, &anyhow::Error>) -> R + Sync,
) -> Vec<R> {
    let origin_refs = (0..origin_sets.len()).map(Ok).collect::<Vec<SnapRef>>();
    let destination_refs = (0..destination_sets.len())
        .map(Ok)
        .collect::<Vec<SnapRef>>();
    let strategy = if routing_graph.acceleration.is_some() {
        BatchRouteStrategy::AcceleratedManyToMany
    } else {
        let origins = pairs
            .iter()
            .map(|&(origin, _)| origin)
            .collect::<std::collections::BTreeSet<_>>();
        choose_batch_route_strategy(routing_graph, origins.len(), pairs.len())
    };
    let connectivity = ConnectivityPolicy::default();
    let fallback = FallbackPolicy::default();
    let alternatives = AlternativeRouteOptions::default();
    let context = BatchRouteContext {
        topology,
        metrics,
        routing_graph,
        strategy,
        snap_max_distance_m,
        connectivity: &connectivity,
        fallback: &fallback,
        returns,
        alternatives: &alternatives,
        origin_candidates: origin_sets,
        destination_candidates: destination_sets,
    };
    resolve_batch_routes(
        &context,
        pairs.len(),
        |index| {
            let (origin, destination) = pairs[index];
            (&origin_refs[origin], &destination_refs[destination])
        },
        build,
    )
}
