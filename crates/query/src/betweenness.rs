use anyhow::{Result, bail};
use netweevil_core::{CompiledProfileBundle, TopologyBundle};
use netweevil_profile::{ReturnConfig, ReturnGeometry};
use serde::{Deserialize, Serialize};

use crate::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightedPoint {
    pub id: String,
    pub lon: f64,
    pub lat: f64,
    #[serde(default)]
    pub z: Option<f64>,
    #[serde(default = "default_demand_weight")]
    pub weight: f64,
}

fn default_demand_weight() -> f64 {
    1.0
}

impl WeightedPoint {
    fn labeled(&self) -> LabeledPoint {
        LabeledPoint {
            id: self.id.clone(),
            lon: self.lon,
            lat: self.lat,
            z: self.z,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BetweennessRequest {
    pub analysis_id: String,
    pub origins: Vec<WeightedPoint>,
    pub destinations: Vec<WeightedPoint>,
    #[serde(default)]
    pub snap: SnapOptions,
    #[serde(default, flatten)]
    pub temporal: TemporalRequestOptions,
    #[serde(default = "default_betweenness_pair_limit")]
    pub max_od_pairs: usize,
    #[serde(default)]
    pub include_zero: bool,
}

fn default_betweenness_pair_limit() -> usize {
    2_000_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BetweennessEdgeScore {
    pub edge_id: u32,
    pub source_way_id: i64,
    pub from_node_id: u32,
    pub to_node_id: u32,
    pub score: f64,
    pub normalized_score: f64,
    pub routed_pair_count: u64,
    pub geometry: Vec<[f64; 3]>,
}

/// Outcome for one positive-demand origin/destination pair. Indexes avoid
/// cloning point ids while remaining unambiguous when ids repeat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BetweennessPairResult {
    pub origin_index: usize,
    pub destination_index: usize,
    pub demand: f64,
    pub routed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BetweennessResult {
    pub analysis_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub departure_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrive_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    pub origin_count: usize,
    pub destination_count: usize,
    pub requested_pair_count: usize,
    pub routed_pair_count: usize,
    pub unreachable_pair_count: usize,
    pub routed_demand: f64,
    pub time_dependent: bool,
    /// Positive-demand pair outcomes used for exact scenario comparisons.
    /// Empty when the caller does not ask for per-pair results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pairs: Vec<BetweennessPairResult>,
    pub edges: Vec<BetweennessEdgeScore>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

pub(crate) fn execute_betweenness_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &BetweennessRequest,
    include_pair_results: bool,
) -> Result<BetweennessResult> {
    let temporal_routing_graph;
    let routing_graph = if request.temporal.is_temporal()
        && !routing_graph.includes_temporal_materialized_directions
    {
        temporal_routing_graph = build_temporal_routing_graph(topology, metrics)?;
        &temporal_routing_graph
    } else {
        routing_graph
    };
    if request.origins.is_empty() || request.destinations.is_empty() {
        bail!("betweenness requires at least one origin and one destination");
    }
    for point in request.origins.iter().chain(&request.destinations) {
        if !point.weight.is_finite() || point.weight < 0.0 {
            bail!(
                "demand weight for point '{}' must be finite and non-negative",
                point.id
            );
        }
    }
    let pair_count = request
        .origins
        .len()
        .saturating_mul(request.destinations.len());
    if pair_count > request.max_od_pairs {
        bail!(
            "betweenness request has {pair_count} OD pairs, above max_od_pairs {}",
            request.max_od_pairs
        );
    }

    let mut scores = vec![0.0_f64; topology.edge_count()];
    let mut routed_counts = vec![0_u64; topology.edge_count()];
    let mut routed_pair_count = 0_usize;
    let mut unreachable_pair_count = 0_usize;
    let mut routed_demand = 0.0;
    let mut pairs = if include_pair_results {
        Vec::with_capacity(pair_count)
    } else {
        Vec::new()
    };
    let use_pairwise =
        request.temporal.requires_exact_labels() || routing_graph.has_restriction_sequences();

    if use_pairwise {
        let snapped_origins = request
            .origins
            .iter()
            .map(|point| {
                nearest_selected_point(topology, routing_graph, point, &request.snap, true)
            })
            .collect::<Vec<_>>();
        let snapped_destinations = request
            .destinations
            .iter()
            .map(|point| {
                nearest_selected_point(topology, routing_graph, point, &request.snap, false)
            })
            .collect::<Vec<_>>();
        for (origin_index, origin) in request.origins.iter().enumerate() {
            for (destination_index, destination) in request.destinations.iter().enumerate() {
                let demand = origin.weight * destination.weight;
                if demand == 0.0 {
                    continue;
                }
                let (Ok(origin_point), Ok(destination_point)) = (
                    &snapped_origins[origin_index],
                    &snapped_destinations[destination_index],
                ) else {
                    unreachable_pair_count += 1;
                    push_pair_result(
                        &mut pairs,
                        include_pair_results,
                        origin_index,
                        destination_index,
                        demand,
                        false,
                    );
                    continue;
                };
                let route = RouteRequest {
                    route_id: format!("{}:{}", origin.id, destination.id),
                    origin: origin_point.clone(),
                    destination: destination_point.clone(),
                    snap: SnapOptions {
                        // The demand point was already snapped under the
                        // caller's policy. Re-snapping only recovers all
                        // directed candidates at that exact graph location;
                        // it must not shift demand to a cheaper nearby node.
                        max_distance_m: 0.05,
                        z_window_m: Some(0.05),
                        attribute_filters: request.snap.attribute_filters.clone(),
                        point_constraints: request.snap.point_constraints.clone(),
                    },
                    connectivity: ConnectivityPolicy::default(),
                    fallback: FallbackPolicy::default(),
                    returns: ReturnConfig {
                        geometry: ReturnGeometry::Full,
                        ..ReturnConfig::default()
                    },
                    alternatives: AlternativeRouteOptions::default(),
                    temporal: request.temporal.clone(),
                };
                match execute_route_with_graph(topology, metrics, routing_graph, &route, None) {
                    Ok(result) => {
                        routed_pair_count += 1;
                        routed_demand += demand;
                        push_pair_result(
                            &mut pairs,
                            include_pair_results,
                            origin_index,
                            destination_index,
                            demand,
                            true,
                        );
                        for edge_id in result.edge_path {
                            let edge_index = edge_id as usize;
                            if edge_index < scores.len() {
                                scores[edge_index] += demand;
                                routed_counts[edge_index] += 1;
                            }
                        }
                    }
                    Err(_) => {
                        unreachable_pair_count += 1;
                        push_pair_result(
                            &mut pairs,
                            include_pair_results,
                            origin_index,
                            destination_index,
                            demand,
                            false,
                        );
                    }
                }
            }
        }
    } else {
        let destination_points = request
            .destinations
            .iter()
            .map(WeightedPoint::labeled)
            .collect::<Vec<_>>();
        let destination_candidates = destination_points
            .iter()
            .map(|point| {
                nearest_snap_candidates(topology, routing_graph, point, &request.snap, false)
            })
            .collect::<Vec<_>>();

        for (origin_index, origin) in request.origins.iter().enumerate() {
            let origin_point = origin.labeled();
            let origin_candidates = match nearest_snap_candidates(
                topology,
                routing_graph,
                &origin_point,
                &request.snap,
                true,
            ) {
                Ok(candidates) => candidates,
                Err(_) => {
                    for (destination_index, destination) in request.destinations.iter().enumerate()
                    {
                        let demand = origin.weight * destination.weight;
                        if demand == 0.0 {
                            continue;
                        }
                        unreachable_pair_count += 1;
                        push_pair_result(
                            &mut pairs,
                            include_pair_results,
                            origin_index,
                            destination_index,
                            demand,
                            false,
                        );
                    }
                    continue;
                }
            };
            let trees = origin_candidates
                .iter()
                .filter_map(|candidate| {
                    build_single_source_edge_tree(topology, routing_graph, candidate)
                        .ok()
                        .map(|tree| (candidate, tree))
                })
                .collect::<Vec<_>>();

            for (destination_index, destination) in request.destinations.iter().enumerate() {
                let demand = origin.weight * destination.weight;
                if demand == 0.0 {
                    continue;
                }
                let Ok(destination_candidates) = &destination_candidates[destination_index] else {
                    unreachable_pair_count += 1;
                    push_pair_result(
                        &mut pairs,
                        include_pair_results,
                        origin_index,
                        destination_index,
                        demand,
                        false,
                    );
                    continue;
                };
                let mut best: Option<RoutePath> = None;
                for (origin_candidate, tree) in &trees {
                    for destination_candidate in destination_candidates {
                        let Some(path) = best_path_from_origin_tree(
                            routing_graph,
                            tree,
                            origin_candidate,
                            destination_candidate,
                        ) else {
                            continue;
                        };
                        if best.as_ref().is_none_or(|existing| {
                            path.total_generalized_cost < existing.total_generalized_cost
                        }) {
                            best = Some(path);
                        }
                    }
                }
                let Some(path) = best else {
                    unreachable_pair_count += 1;
                    push_pair_result(
                        &mut pairs,
                        include_pair_results,
                        origin_index,
                        destination_index,
                        demand,
                        false,
                    );
                    continue;
                };
                routed_pair_count += 1;
                routed_demand += demand;
                push_pair_result(
                    &mut pairs,
                    include_pair_results,
                    origin_index,
                    destination_index,
                    demand,
                    true,
                );
                for edge_index in path.edge_indexes {
                    scores[edge_index] += demand;
                    routed_counts[edge_index] += 1;
                }
            }
        }
    }

    let edges = (0..topology.edge_count())
        .filter(|edge_index| request.include_zero || scores[*edge_index] > 0.0)
        .map(|edge_index| {
            let edge = topology.routing_edge(edge_index);
            let from = &topology.nodes[edge.from.0 as usize];
            let to = &topology.nodes[edge.to.0 as usize];
            BetweennessEdgeScore {
                edge_id: edge.edge_id.0,
                source_way_id: edge.source_way_id,
                from_node_id: edge.from.0,
                to_node_id: edge.to.0,
                score: scores[edge_index],
                normalized_score: if routed_demand > 0.0 {
                    scores[edge_index] / routed_demand
                } else {
                    0.0
                },
                routed_pair_count: routed_counts[edge_index],
                geometry: vec![
                    [from.lon, from.lat, finite_z(from.z)],
                    [to.lon, to.lat, finite_z(to.z)],
                ],
            }
        })
        .collect();
    let mut warnings = Vec::new();
    if unreachable_pair_count > 0 {
        warnings.push(format!(
            "{unreachable_pair_count} OD pair(s) were unreachable or unsnappable"
        ));
    }
    if use_pairwise && routing_graph.has_restriction_sequences() && !request.temporal.is_temporal()
    {
        warnings.push(
            "restriction-aware pairwise routing was used instead of one-to-many trees".to_string(),
        );
    }

    let provenance = temporal_result_provenance(&request.temporal)?;
    Ok(BetweennessResult {
        analysis_id: request.analysis_id.clone(),
        departure_time: provenance.departure_time,
        arrive_by: provenance.arrive_by,
        scenario_id: provenance.scenario_id,
        origin_count: request.origins.len(),
        destination_count: request.destinations.len(),
        requested_pair_count: pair_count,
        routed_pair_count,
        unreachable_pair_count,
        routed_demand,
        time_dependent: request.temporal.is_temporal(),
        pairs,
        edges,
        warnings,
    })
}

fn push_pair_result(
    pairs: &mut Vec<BetweennessPairResult>,
    include_pair_results: bool,
    origin_index: usize,
    destination_index: usize,
    demand: f64,
    routed: bool,
) {
    if !include_pair_results {
        return;
    }
    pairs.push(BetweennessPairResult {
        origin_index,
        destination_index,
        demand,
        routed,
    });
}

fn nearest_snap_candidates(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    options: &SnapOptions,
    is_origin: bool,
) -> Result<Vec<SnappedPoint>> {
    let mut candidates =
        snap_candidates_with_options(topology, routing_graph, point, options, is_origin)?;
    let nearest_distance = candidates
        .first()
        .map(|candidate| candidate.snap_distance_m)
        .unwrap_or(f64::INFINITY);
    // Keep coincident directed-edge/node representations while excluding
    // farther alternatives whose lower network cost would spatially move the
    // demand point within a broad snap radius.
    candidates.retain(|candidate| candidate.snap_distance_m <= nearest_distance + 1.0e-6);
    Ok(candidates)
}

fn nearest_selected_point(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    point: &WeightedPoint,
    options: &SnapOptions,
    is_origin: bool,
) -> Result<LabeledPoint> {
    let candidate = nearest_snap_candidates(
        topology,
        routing_graph,
        &point.labeled(),
        options,
        is_origin,
    )?
    .into_iter()
    .next()
    .ok_or_else(|| anyhow::anyhow!("point '{}' has no snap candidate", point.id))?;
    Ok(LabeledPoint {
        id: point.id.clone(),
        lon: candidate.snapped_lon,
        lat: candidate.snapped_lat,
        z: Some(candidate.snapped_z),
    })
}

fn finite_z(z: f64) -> f64 {
    if z.is_finite() { z } else { 0.0 }
}
