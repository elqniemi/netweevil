use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, bail};
use netweevil_core::{CompiledProfileBundle, TopologyBundle};

use crate::*;
use rayon::prelude::*;

pub(crate) fn execute_accessibility_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &AccessibilityRequest,
) -> Result<AccessibilityResult> {
    validate_execution_inputs(topology, metrics)?;
    if request.origins.points.is_empty() {
        bail!("accessibility origins must contain at least one point");
    }
    if request.categories.is_empty() {
        bail!("accessibility request must contain at least one destination category");
    }
    if request.max_travel_time_s <= 0.0 {
        bail!("accessibility max_travel_time_s must be positive");
    }

    let mut thresholds = request.thresholds_s.clone();
    thresholds.retain(|threshold| threshold.is_finite() && *threshold > 0.0);
    thresholds.sort_by(|left, right| left.total_cmp(right));
    thresholds.dedup_by(|left, right| (*left - *right).abs() <= f64::EPSILON);
    if thresholds.is_empty() {
        thresholds.push(request.max_travel_time_s);
    }

    let search_distance_m = request
        .origins
        .connectivity
        .max_hop_distance_m
        .unwrap_or(request.origins.snap.max_distance_m)
        .max(request.origins.snap.max_distance_m);
    let mut snap_cache = HashMap::new();
    let origin_candidate_sets = request
        .origins
        .points
        .iter()
        .map(|origin| {
            cached_snap_candidates(
                &mut snap_cache,
                topology,
                routing_graph,
                origin,
                search_distance_m,
                true,
            )
        })
        .collect::<Vec<_>>();
    let (origin_refs, unique_origin_candidates) = intern_candidate_sets(origin_candidate_sets);

    let destination_sets = request
        .categories
        .iter()
        .map(|category| {
            let snaps = presnap_point_set(
                topology,
                routing_graph,
                &category.destinations.points,
                category.destinations.snap.max_distance_m,
                false,
            );
            let snapped_count = snaps.iter().filter(|snap| snap.is_ok()).count();
            CategoryDestinationSnaps {
                category_id: category.category_id.clone(),
                points: category.destinations.points.clone(),
                snaps,
                snapped_count,
            }
        })
        .collect::<Vec<_>>();

    // Expansions are independent per unique snapped origin; build them in
    // parallel up front and share them afterwards instead of cloning the
    // per-edge cost arrays per origin.
    let mut expansion_jobs: Vec<(usize, &LabeledPoint)> = Vec::new();
    let mut seen_expansions = std::collections::HashSet::new();
    for (origin, origin_ref) in request.origins.points.iter().zip(&origin_refs) {
        let Ok(origin_set_id) = origin_ref else {
            continue;
        };
        if seen_expansions.insert(*origin_set_id) {
            expansion_jobs.push((*origin_set_id, origin));
        }
    }
    let expansion_cache: HashMap<
        usize,
        std::result::Result<std::sync::Arc<ServiceAreaOriginExpansion>, AnalysisFailure>,
    > = expansion_jobs
        .into_par_iter()
        .map(|(origin_set_id, origin)| {
            let expansion = build_service_area_expansion(
                topology,
                metrics,
                routing_graph,
                origin,
                &unique_origin_candidates[origin_set_id],
                request.origins.snap.max_distance_m,
                &request.origins.connectivity,
                ServiceAreaMetricKind::TravelTimeS,
                request.max_travel_time_s,
            )
            .map(std::sync::Arc::new)
            .map_err(|error| {
                analysis_failure(&error).cloned().unwrap_or_else(|| {
                    AnalysisFailure::new(
                        error.to_string(),
                        AnalysisOutcome::Unreachable,
                        Vec::new(),
                    )
                })
            });
            (origin_set_id, expansion)
        })
        .collect();
    let mut rows = Vec::new();
    let mut diagnostics = Vec::new();
    let mut warnings = execution_warnings(metrics);
    let mut skipped_origin_count = 0_usize;

    for (origin, origin_ref) in request.origins.points.iter().zip(&origin_refs) {
        let origin_set_id = match origin_ref {
            Ok(origin_set_id) => *origin_set_id,
            Err(error) => {
                skipped_origin_count += 1;
                diagnostics.extend(error.diagnostics.clone());
                for category in &destination_sets {
                    rows.push(failed_accessibility_row(
                        origin,
                        category,
                        &thresholds,
                        AnalysisOutcome::Unreachable,
                        Some(error.message.clone()),
                        error.diagnostics.clone(),
                    ));
                }
                continue;
            }
        };

        let expansion = expansion_cache
            .get(&origin_set_id)
            .expect("expansion precomputed for every snapped origin")
            .clone();

        let expansion = match expansion {
            Ok(expansion) => expansion,
            Err(error) => {
                skipped_origin_count += 1;
                diagnostics.extend(error.diagnostics.clone());
                for category in &destination_sets {
                    rows.push(failed_accessibility_row(
                        origin,
                        category,
                        &thresholds,
                        error.outcome,
                        Some(error.message.clone()),
                        error.diagnostics.clone(),
                    ));
                }
                continue;
            }
        };

        diagnostics.extend(expansion.diagnostics.clone());
        warnings.extend(expansion.warnings.clone());

        for category in &destination_sets {
            rows.push(accessibility_row_for_category(
                topology,
                metrics,
                routing_graph,
                expansion.as_ref(),
                category,
                &thresholds,
                request.max_travel_time_s,
            ));
        }
    }

    let succeeded_count = rows
        .iter()
        .filter(|row| matches!(row.status, BatchItemStatus::Succeeded))
        .count();
    let failed_count = rows
        .iter()
        .filter(|row| matches!(row.status, BatchItemStatus::Failed))
        .count();
    let destination_count = destination_sets
        .iter()
        .map(|category| category.points.len())
        .sum();

    Ok(AccessibilityResult {
        origin_count: request.origins.points.len(),
        category_count: request.categories.len(),
        destination_count,
        max_travel_time_s: request.max_travel_time_s,
        thresholds_s: thresholds,
        row_count: rows.len(),
        succeeded_count,
        failed_count,
        skipped_origin_count,
        rows,
        diagnostics,
        warnings,
    })
}

#[derive(Debug, Clone)]
struct CategoryDestinationSnaps {
    category_id: String,
    points: Vec<LabeledPoint>,
    snaps: Vec<std::result::Result<Vec<SnappedPoint>, AnalysisFailure>>,
    snapped_count: usize,
}

fn failed_accessibility_row(
    origin: &LabeledPoint,
    category: &CategoryDestinationSnaps,
    thresholds: &[f64],
    outcome: AnalysisOutcome,
    error: Option<String>,
    diagnostics: Vec<AnalysisDiagnostic>,
) -> AccessibilityCategoryResult {
    AccessibilityCategoryResult {
        origin_id: origin.id.clone(),
        category_id: category.category_id.clone(),
        status: BatchItemStatus::Failed,
        outcome,
        fallback_used: false,
        origin_component_id: None,
        origin_hop_distance_m: None,
        origin_snap_distance_m: None,
        destination_count: category.points.len(),
        snapped_destination_count: category.snapped_count,
        nearest_destination_id: None,
        nearest_travel_time_s: None,
        nearest_destination_snap_distance_m: None,
        counts_within_threshold_s: empty_accessibility_counts(thresholds),
        diagnostics,
        error,
    }
}

fn accessibility_row_for_category(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    expansion: &ServiceAreaOriginExpansion,
    category: &CategoryDestinationSnaps,
    thresholds: &[f64],
    max_travel_time_s: f64,
) -> AccessibilityCategoryResult {
    let mut counts = empty_accessibility_counts(thresholds);
    let mut nearest_destination_id = None;
    let mut nearest_travel_time_s = f64::INFINITY;
    let mut nearest_snap_distance_m = None;

    for (destination, snap_result) in category.points.iter().zip(&category.snaps) {
        let Ok(candidates) = snap_result else {
            continue;
        };
        let mut best_destination_time = f64::INFINITY;
        let mut best_destination_snap_distance = None;
        for candidate in candidates {
            let Some(travel_time_s) =
                travel_time_to_destination(topology, metrics, routing_graph, expansion, candidate)
            else {
                continue;
            };
            if travel_time_s + f64::EPSILON < best_destination_time {
                best_destination_time = travel_time_s;
                best_destination_snap_distance = Some(candidate.snap_distance_m);
            }
        }
        if !best_destination_time.is_finite() || best_destination_time > max_travel_time_s {
            continue;
        }
        for threshold in thresholds {
            if best_destination_time <= *threshold + f64::EPSILON {
                let key = accessibility_threshold_key(*threshold);
                *counts.entry(key).or_insert(0) += 1;
            }
        }
        if best_destination_time + f64::EPSILON < nearest_travel_time_s {
            nearest_destination_id = Some(destination.id.clone());
            nearest_travel_time_s = best_destination_time;
            nearest_snap_distance_m = best_destination_snap_distance;
        }
    }

    let found = nearest_destination_id.is_some();
    AccessibilityCategoryResult {
        origin_id: expansion.origin_id.clone(),
        category_id: category.category_id.clone(),
        status: if found {
            BatchItemStatus::Succeeded
        } else {
            BatchItemStatus::Failed
        },
        outcome: if found {
            AnalysisOutcome::Legal
        } else {
            AnalysisOutcome::Unreachable
        },
        fallback_used: expansion.fallback_used,
        origin_component_id: expansion.representative_origin.component_id,
        origin_hop_distance_m: expansion.origin_hop_distance_m,
        origin_snap_distance_m: Some(expansion.representative_origin.snap_distance_m),
        destination_count: category.points.len(),
        snapped_destination_count: category.snapped_count,
        nearest_destination_id,
        nearest_travel_time_s: found.then_some(nearest_travel_time_s),
        nearest_destination_snap_distance_m: nearest_snap_distance_m,
        counts_within_threshold_s: counts,
        diagnostics: Vec::new(),
        error: (!found).then(|| {
            format!(
                "No '{}' destination was reachable within {:.0} seconds.",
                category.category_id, max_travel_time_s
            )
        }),
    }
}

fn empty_accessibility_counts(thresholds: &[f64]) -> BTreeMap<String, usize> {
    thresholds
        .iter()
        .map(|threshold| (accessibility_threshold_key(*threshold), 0_usize))
        .collect()
}

fn accessibility_threshold_key(threshold_s: f64) -> String {
    if (threshold_s.fract()).abs() <= f64::EPSILON {
        format!("{threshold_s:.0}")
    } else {
        threshold_s.to_string()
    }
}

fn travel_time_to_destination(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    expansion: &ServiceAreaOriginExpansion,
    destination: &SnappedPoint,
) -> Option<f64> {
    if let (Some(edge_id), Some(fraction)) = (
        destination.snapped_edge_id,
        destination.snapped_edge_fraction,
    ) {
        let edge_index = edge_id as usize;
        let start_fraction = expansion.edge_start_fractions[edge_index];
        if fraction + f64::EPSILON < start_fraction {
            return None;
        }
        let before_cost = expansion.edge_before_costs[edge_index];
        if !before_cost.is_finite() {
            return None;
        }
        let edge_cost = service_area_edge_cost(
            topology,
            metrics,
            edge_index,
            ServiceAreaMetricKind::TravelTimeS,
        )?;
        return Some(before_cost + edge_cost * (fraction - start_fraction));
    }

    routing_graph
        .incoming_edges(destination.snapped_node_id as usize)
        .iter()
        .filter_map(|edge_index| expansion.edge_end_costs.get(*edge_index as usize).copied())
        .filter(|cost| cost.is_finite())
        .min_by(|left, right| left.total_cmp(right))
}

pub fn execute_accessibility(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &AccessibilityRequest,
) -> Result<AccessibilityResult> {
    if has_failure_modes(&request.origins.fallback) {
        validate_execution_inputs(topology, metrics)?;
        let degraded = cached_failure_mode_bundle(topology, metrics, &request.origins.fallback)?;
        return execute_accessibility_with_graph(
            &degraded.topology,
            &degraded.metrics,
            &degraded.routing_graph,
            request,
        );
    }
    validate_execution_inputs(topology, metrics)?;
    let routing_graph = build_routing_graph(topology, metrics)?;
    execute_accessibility_with_graph(topology, metrics, &routing_graph, request)
}
