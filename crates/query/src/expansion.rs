//! Search expansion for debugging and visualisation: the edges an exact
//! edge-based Dijkstra settles, in settle order, with their labels, or the
//! states the contraction hierarchy query settles for the same route.
//!
//! Production routes run on the contraction hierarchy, whose search space
//! is shortcuts rather than streets. `route` records the exact reference
//! search over the same graph, turn costs and restriction automaton; it
//! reaches the same optimal cost. `hierarchy` records the hierarchy query
//! itself: each settled state is a street edge, joined to its predecessor
//! by a shortcut.

use std::collections::BinaryHeap;

use anyhow::{Result, bail};
use netweevil_core::{CompiledProfileBundle, TopologyBundle};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::*;

#[cfg(test)]
mod tests;

pub const DEFAULT_EXPANSION_MAX_EDGES: usize = 50_000;
pub const MAX_EXPANSION_EDGES: usize = 500_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpansionAction {
    /// Forward search from `origin` until `destination` is settled, on the
    /// profile's generalized cost.
    Route,
    /// Search bounded by `limit` on `metric`, from (`outbound`) or to
    /// (`inbound`) the origin.
    ServiceArea,
    /// The contraction hierarchy query from `origin` to `destination`: the
    /// upward forward search and the backward search, interleaved as they
    /// run. Needs a profile with a hierarchy.
    Hierarchy,
}

/// The search that settled an [`ExpandedEdge`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpansionSide {
    #[default]
    Forward,
    Backward,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpansionRequest {
    pub action: ExpansionAction,
    pub origin: LabeledPoint,
    #[serde(default)]
    pub destination: Option<LabeledPoint>,
    #[serde(default)]
    pub limit: Option<f64>,
    #[serde(default = "default_metric")]
    pub metric: ServiceAreaThresholdMetric,
    #[serde(default)]
    pub direction: ServiceAreaDirection,
    #[serde(default)]
    pub snap: SnapOptions,
    /// Settled edges to report; the search stops once this many are settled.
    #[serde(default = "default_max_edges")]
    pub max_edges: usize,
}

fn default_metric() -> ServiceAreaThresholdMetric {
    ServiceAreaThresholdMetric::TravelTimeS
}

fn default_max_edges() -> usize {
    DEFAULT_EXPANSION_MAX_EDGES
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpansionResult {
    pub action: ExpansionAction,
    pub direction: ServiceAreaDirection,
    /// Generalized cost (route) or threshold metric (service area).
    pub metric: String,
    pub edges: Vec<ExpandedEdge>,
    /// The search stopped at `max_edges` before finishing.
    pub truncated: bool,
    /// Optimal route cost; `None` for service areas or unreachable routes.
    pub route_cost: Option<f64>,
}

/// One settled edge. `cost` is the label at the edge's far end: from the
/// origin to the edge end (forward), or from the edge start to the origin
/// (inbound).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandedEdge {
    pub order: u32,
    pub edge_index: u32,
    pub cost: f64,
    /// Edge the label was relaxed from; `None` for seeds.
    pub previous_edge_index: Option<u32>,
    /// `backward` for hierarchy states settled from the destination.
    #[serde(default)]
    pub side: ExpansionSide,
}

pub(crate) fn execute_expansion_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    graph: &RoutingGraph,
    request: &ExpansionRequest,
) -> Result<ExpansionResult> {
    if !(1..=MAX_EXPANSION_EDGES).contains(&request.max_edges) {
        bail!("expansion max_edges must be in [1, {MAX_EXPANSION_EDGES}]");
    }
    let inbound = matches!(request.direction, ServiceAreaDirection::Inbound);
    match request.action {
        ExpansionAction::Route | ExpansionAction::Hierarchy => {
            if inbound {
                bail!("route expansion is forward; direction=inbound applies to service_area");
            }
            let Some(destination) = request.destination.as_ref() else {
                bail!("route expansion requires a destination");
            };
            let origins = nearest_attachment(snap_candidates_with_options(
                topology,
                graph,
                &request.origin,
                &request.snap,
                true,
            )?);
            let destinations = nearest_attachment(snap_candidates_with_options(
                topology,
                graph,
                destination,
                &request.snap,
                false,
            )?);
            let seeds = origins
                .iter()
                .flat_map(|origin| origin_edge_seeds(graph, origin))
                .collect::<Vec<_>>();
            let targets = destinations
                .iter()
                .flat_map(|destination| destination_edge_seeds(graph, destination))
                .collect::<Vec<_>>();
            if request.action == ExpansionAction::Hierarchy {
                let Some((settles, truncated, route_cost)) =
                    recorded_hierarchy_search(graph, &seeds, &targets, request.max_edges)
                else {
                    bail!("hierarchy expansion needs a profile with a contraction hierarchy");
                };
                let edges = settles
                    .into_iter()
                    .enumerate()
                    .map(|(order, settle)| ExpandedEdge {
                        order: order as u32,
                        edge_index: settle.edge_index,
                        cost: settle.cost,
                        previous_edge_index: settle.previous_edge_index,
                        side: if settle.backward {
                            ExpansionSide::Backward
                        } else {
                            ExpansionSide::Forward
                        },
                    })
                    .collect();
                return Ok(ExpansionResult {
                    action: request.action,
                    direction: request.direction,
                    metric: "generalized_cost".to_string(),
                    edges,
                    truncated,
                    route_cost,
                });
            }
            let costs = SearchCosts::Generalized;
            let (edges, truncated, route_cost) = recorded_search(
                topology,
                metrics,
                graph,
                &seeds,
                &targets,
                costs,
                f64::INFINITY,
                false,
                request.max_edges,
            );
            Ok(ExpansionResult {
                action: request.action,
                direction: request.direction,
                metric: "generalized_cost".to_string(),
                edges,
                truncated,
                route_cost,
            })
        }
        ExpansionAction::ServiceArea => {
            let Some(limit) = request
                .limit
                .filter(|limit| limit.is_finite() && *limit >= 0.0)
            else {
                bail!("service-area expansion requires a finite non-negative limit");
            };
            let candidates = nearest_attachment(snap_candidates_with_options(
                topology,
                graph,
                &request.origin,
                &request.snap,
                !inbound,
            )?);
            let metric_kind = ServiceAreaMetricKind::from(request.metric);
            let seeds = candidates
                .iter()
                .flat_map(|candidate| {
                    service_area_seeds(topology, metrics, graph, candidate, metric_kind, inbound)
                })
                .collect::<Vec<_>>();
            let (edges, truncated, _) = recorded_search(
                topology,
                metrics,
                graph,
                &seeds,
                &[],
                SearchCosts::Metric(metric_kind),
                limit,
                inbound,
                request.max_edges,
            );
            Ok(ExpansionResult {
                action: request.action,
                direction: request.direction,
                metric: match request.metric {
                    ServiceAreaThresholdMetric::DistanceM => "distance_m",
                    ServiceAreaThresholdMetric::TravelTimeS => "travel_time_s",
                }
                .to_string(),
                edges,
                truncated,
                route_cost: None,
            })
        }
    }
}

/// The candidates at the nearest snap distance (the orientations of one
/// attachment), as routing and service areas seed from.
fn nearest_attachment(candidates: Vec<SnappedPoint>) -> Vec<SnappedPoint> {
    let nearest = candidates
        .iter()
        .map(|candidate| candidate.snap_distance_m)
        .fold(f64::INFINITY, f64::min);
    candidates
        .into_iter()
        .filter(|candidate| candidate.snap_distance_m <= nearest + 1e-6)
        .collect()
}

#[derive(Clone, Copy)]
enum SearchCosts {
    /// The routing graph's transition costs (generalized cost with turns).
    Generalized,
    Metric(ServiceAreaMetricKind),
}

/// Seeds labelled at the far end of their edge: the remaining part ahead
/// of an outbound origin, or the part behind an inbound anchor.
fn service_area_seeds(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    graph: &RoutingGraph,
    candidate: &SnappedPoint,
    metric_kind: ServiceAreaMetricKind,
    inbound: bool,
) -> Vec<(usize, f64)> {
    let cost = |edge| service_area_edge_cost(topology, metrics, edge, metric_kind);
    if let (Some(edge), Some(fraction)) =
        (candidate.snapped_edge_id, candidate.snapped_edge_fraction)
    {
        let edge = edge as usize;
        let share = if inbound { fraction } else { 1.0 - fraction };
        return cost(edge)
            .filter(|_| graph.edge_costs[edge].is_finite())
            .map(|cost| (edge, cost * share))
            .into_iter()
            .collect();
    }
    let node = candidate.snapped_node_id as usize;
    let edges = if inbound {
        graph.incoming_edges(node)
    } else {
        graph.outgoing_edges(node)
    };
    edges
        .iter()
        .filter_map(|&edge| cost(edge as usize).map(|cost| (edge as usize, cost)))
        .collect()
}

/// Dijkstra over (edge, automaton state) recording each edge the first time
/// it is settled. `targets` are destination edges with cost adjustments;
/// the search stops once no queued label can improve the best target.
#[allow(clippy::too_many_arguments)]
fn recorded_search(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    graph: &RoutingGraph,
    seeds: &[(usize, f64)],
    targets: &[(usize, f64)],
    costs: SearchCosts,
    limit: f64,
    reverse: bool,
    max_edges: usize,
) -> (Vec<ExpandedEdge>, bool, Option<f64>) {
    let automaton = if reverse {
        &graph.reverse_automaton
    } else {
        &graph.automaton
    };
    let mut dist = FxHashMap::<(u32, u32), f64>::default();
    let mut previous = FxHashMap::<(u32, u32), u32>::default();
    let mut recorded = FxHashSet::<u32>::default();
    let mut heap = BinaryHeap::new();
    for &(edge, cost) in seeds {
        let state = automaton.transition(0, edge);
        if cost
            < dist
                .get(&(edge as u32, state as u32))
                .copied()
                .unwrap_or(f64::INFINITY)
        {
            dist.insert((edge as u32, state as u32), cost);
            heap.push(State {
                edge_index: edge,
                automaton_state: state,
                cost,
                score: cost,
            });
        }
    }
    let minimum_adjustment = targets
        .iter()
        .map(|&(_, adjustment)| adjustment)
        .fold(0.0, f64::min);
    let mut best_target = f64::INFINITY;
    let mut edges = Vec::new();
    let mut truncated = false;
    while let Some(State {
        edge_index,
        automaton_state,
        cost,
        ..
    }) = heap.pop()
    {
        let key = (edge_index as u32, automaton_state as u32);
        if cost > dist[&key] || cost > limit {
            continue;
        }
        if !targets.is_empty() && cost + minimum_adjustment >= best_target {
            break;
        }
        if recorded.insert(edge_index as u32) {
            if edges.len() >= max_edges {
                truncated = true;
                break;
            }
            edges.push(ExpandedEdge {
                order: edges.len() as u32,
                edge_index: edge_index as u32,
                cost,
                previous_edge_index: previous.get(&key).copied(),
                side: ExpansionSide::Forward,
            });
        }
        for &(target, adjustment) in targets {
            if target == edge_index && cost + adjustment >= 0.0 {
                best_target = best_target.min(cost + adjustment);
            }
        }
        let range = if reverse {
            graph.reverse_transition_range(edge_index)
        } else {
            graph.transition_range(edge_index)
        };
        for slot in range {
            let next = if reverse {
                graph.reverse_transition_edges[slot]
            } else {
                graph.transition_edges[slot]
            } as usize;
            if !automaton.is_transition_allowed(automaton_state, next) {
                continue;
            }
            let step = match costs {
                SearchCosts::Generalized => graph.transition_costs[slot],
                SearchCosts::Metric(metric_kind) => {
                    let Some(edge_cost) =
                        service_area_edge_cost(topology, metrics, next, metric_kind)
                    else {
                        continue;
                    };
                    let (from, to) = if reverse {
                        (next, edge_index)
                    } else {
                        (edge_index, next)
                    };
                    edge_cost + service_area_turn_cost(topology, metrics, from, to, metric_kind)
                }
            };
            let next_cost = cost + step;
            let next_state = automaton.transition(automaton_state, next);
            let next_key = (next as u32, next_state as u32);
            if next_cost < dist.get(&next_key).copied().unwrap_or(f64::INFINITY) {
                dist.insert(next_key, next_cost);
                previous.insert(next_key, edge_index as u32);
                heap.push(State {
                    edge_index: next,
                    automaton_state: next_state,
                    cost: next_cost,
                    score: next_cost,
                });
            }
        }
    }
    let route_cost = best_target.is_finite().then_some(best_target);
    (edges, truncated, route_cost)
}
