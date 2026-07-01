use std::collections::HashSet;

use anyhow::Result;
use netweevil_core::{CompiledProfileBundle, TopologyBundle};
use netweevil_profile::ReturnConfig;

use crate::*;

pub(crate) fn build_route_alternatives(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    fallback: &FallbackPolicy,
    returns: &ReturnConfig,
    alternatives: &AlternativeRouteOptions,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    best_path: &RoutePath,
    best_summary: &RouteSummary,
    edge_names: Option<&[String]>,
) -> Result<Vec<RouteAlternative>> {
    if alternatives.max_routes <= 1 || best_path.edge_indexes.len() <= 1 {
        return Ok(Vec::new());
    }

    let mut accepted_paths = vec![best_path.edge_indexes.clone()];
    let mut accepted = Vec::new();
    let mut tried_bans = HashSet::new();
    let max_generalized_cost = alternative_max_generalized_cost(best_summary, alternatives);
    // Ban edges sampled evenly along the best path instead of scanning it
    // front-to-back: adjacent bans produce near-identical detours, and each
    // attempt is a full route search that must stay bounded.
    let max_attempts = alternatives.max_search_attempts.max(1);
    for banned_edge in evenly_spaced(&best_path.edge_indexes, max_attempts) {
        if accepted.len() + 1 >= alternatives.max_routes {
            break;
        }
        if !tried_bans.insert(banned_edge) {
            continue;
        }
        let mut banned_edges = HashSet::new();
        banned_edges.insert(banned_edge);
        let Some((origin, destination, path, hop_info)) =
            route_between_candidates_with_banned_edges(
                topology,
                metrics,
                routing_graph,
                snap_max_distance_m,
                connectivity,
                fallback,
                origin_candidates,
                destination_candidates,
                &banned_edges,
                max_generalized_cost,
            )?
        else {
            continue;
        };
        if path.edge_indexes == best_path.edge_indexes
            || accepted_paths.contains(&path.edge_indexes)
        {
            continue;
        }
        let analysis = analyze_route_path(
            topology,
            metrics,
            routing_graph,
            fallback,
            &path,
            &origin,
            &destination,
        )?;
        if !alternative_within_limits(&analysis.summary, best_summary, alternatives)
            || !alternative_is_diverse(
                &path.edge_indexes,
                &accepted_paths,
                alternatives.min_jaccard_distance,
            )
        {
            continue;
        }
        let rank = accepted.len() as u32 + 1;
        accepted_paths.push(path.edge_indexes.clone());
        accepted.push(materialize_route_alternative(
            topology,
            metrics,
            returns,
            edge_names,
            rank,
            &origin,
            &destination,
            path,
            hop_info,
            analysis,
        )?);
    }

    if accepted.len() + 1 < alternatives.max_routes {
        let mut expanded_bans = HashSet::new();
        for path in &accepted_paths {
            for &edge_index in path {
                expanded_bans.insert(edge_index);
            }
        }
        if !expanded_bans.is_empty()
            && let Some((origin, destination, path, hop_info)) =
                route_between_candidates_with_banned_edges(
                    topology,
                    metrics,
                    routing_graph,
                    snap_max_distance_m,
                    connectivity,
                    fallback,
                    origin_candidates,
                    destination_candidates,
                    &expanded_bans,
                    max_generalized_cost,
                )?
        {
            let duplicate = accepted_paths.contains(&path.edge_indexes);
            let analysis = analyze_route_path(
                topology,
                metrics,
                routing_graph,
                fallback,
                &path,
                &origin,
                &destination,
            )?;
            if !duplicate
                && alternative_within_limits(&analysis.summary, best_summary, alternatives)
                && alternative_is_diverse(
                    &path.edge_indexes,
                    &accepted_paths,
                    alternatives.min_jaccard_distance,
                )
            {
                let rank = accepted.len() as u32 + 1;
                accepted.push(materialize_route_alternative(
                    topology,
                    metrics,
                    returns,
                    edge_names,
                    rank,
                    &origin,
                    &destination,
                    path,
                    hop_info,
                    analysis,
                )?);
            }
        }
    }

    Ok(accepted)
}

/// Up to `count` elements sampled at even intervals across `items`,
/// preserving order and starting from the first element.
fn evenly_spaced(items: &[usize], count: usize) -> Vec<usize> {
    if items.len() <= count {
        return items.to_vec();
    }
    (0..count)
        .map(|slot| items[slot * items.len() / count])
        .collect()
}

fn alternative_max_generalized_cost(
    best: &RouteSummary,
    alternatives: &AlternativeRouteOptions,
) -> Option<f64> {
    let limit = best.total_generalized_cost.max(1.0) * alternatives.max_cost_ratio;
    limit.is_finite().then_some(limit)
}

fn alternative_within_limits(
    summary: &RouteSummary,
    best: &RouteSummary,
    alternatives: &AlternativeRouteOptions,
) -> bool {
    let best_cost = best.total_generalized_cost.max(1.0);
    if summary.total_generalized_cost > best_cost * alternatives.max_cost_ratio {
        return false;
    }
    if let Some(max_extra_time_s) = alternatives.max_extra_time_s
        && summary.total_travel_time_s > best.total_travel_time_s + max_extra_time_s
    {
        return false;
    }
    if let Some(max_extra_distance_m) = alternatives.max_extra_distance_m
        && summary.total_distance_m > best.total_distance_m.saturating_add(max_extra_distance_m)
    {
        return false;
    }
    true
}

fn alternative_is_diverse(
    candidate: &[usize],
    accepted_paths: &[Vec<usize>],
    min_jaccard_distance: f64,
) -> bool {
    accepted_paths.iter().all(|accepted| {
        edge_jaccard_distance(candidate, accepted) + f64::EPSILON >= min_jaccard_distance
    })
}

fn edge_jaccard_distance(left: &[usize], right: &[usize]) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 0.0;
    }
    let left_set = left.iter().copied().collect::<HashSet<_>>();
    let right_set = right.iter().copied().collect::<HashSet<_>>();
    let intersection = left_set.intersection(&right_set).count() as f64;
    let union = left_set.union(&right_set).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        1.0 - intersection / union
    }
}

fn materialize_route_alternative(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    returns: &ReturnConfig,
    edge_names: Option<&[String]>,
    rank: u32,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
    path: RoutePath,
    hop_info: HopSelectionInfo,
    analysis: RoutePathAnalysis,
) -> Result<RouteAlternative> {
    let include_detailed_paths = request_returns_detailed_path(returns);
    let needs_node_path = include_detailed_paths
        || !matches!(returns.geometry, netweevil_profile::ReturnGeometry::None);
    let node_path = if needs_node_path {
        node_path_for_route(topology, origin, &path.edge_indexes)
    } else {
        Vec::new()
    };
    let geometry = match returns.geometry {
        netweevil_profile::ReturnGeometry::None => None,
        _ => Some(build_route_geometry(
            topology,
            &path.edge_indexes,
            origin,
            destination,
        )),
    };
    let mut segments = if returns.segment_rows {
        Some(route_segments_for_path(
            topology,
            metrics,
            edge_names.unwrap_or(&topology.names),
            &path.edge_indexes,
            origin,
            destination,
        ))
    } else {
        None
    };
    if let Some(segment_rows) = segments.as_mut() {
        for (segment, edge_index) in segment_rows.iter_mut().zip(&path.edge_indexes) {
            segment.violation_type = analysis
                .segment_violation_types
                .get(edge_index)
                .copied()
                .flatten();
        }
    }
    let warnings = merge_warnings(
        merge_warnings(analysis.warnings, hop_info.warnings),
        execution_warnings(metrics),
    );
    let diagnostics = hop_info.diagnostics;
    Ok(RouteAlternative {
        alternative_index: rank,
        rank,
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
        segments,
        breakdowns: build_breakdowns(topology, metrics, &path.edge_indexes, returns),
        violations: analysis.violations,
        diagnostics,
        warnings,
    })
}

fn node_path_for_route(
    topology: &TopologyBundle,
    origin: &SnappedPoint,
    edge_indexes: &[usize],
) -> Vec<u32> {
    let mut node_path = Vec::with_capacity(edge_indexes.len() + 1);
    if let Some(&first_edge) = edge_indexes.first() {
        node_path.push(topology.routing_edge(first_edge).from.0);
        for &edge_index in edge_indexes {
            node_path.push(topology.routing_edge(edge_index).to.0);
        }
    } else if origin.snapped_edge_id.is_none() {
        node_path.push(origin.snapped_node_id);
    }
    node_path
}

fn route_segments_for_path(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    edge_names: &[String],
    edge_indexes: &[usize],
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> Vec<RouteSegment> {
    edge_indexes
        .iter()
        .map(|&edge_index| {
            let edge = topology.edge(edge_index);
            let metric = &metrics.edge_metrics[edge_index];
            let factor = edge_traversal_factor(
                edge_index,
                edge_indexes.first().copied(),
                edge_indexes.last().copied(),
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
                road_class: edge.road_class,
                surface: edge.surface,
                name: edge
                    .name_index
                    .and_then(|index| edge_names.get(index as usize))
                    .cloned(),
                violation_type: None,
            }
        })
        .collect()
}
