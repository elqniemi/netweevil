//! Ordered static routes and exact directed stop ordering.
//!
//! Break locations reset turn history and split legs. Through locations keep
//! turn history, prohibit a U-turn at the location, and remain in the same leg.
//! Stop ordering uses Held–Karp on directed, unquantized network costs, with
//! fixed endpoints and at most 16 intermediate break locations. Temporal and
//! component-constrained requests are rejected rather than priced statically.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use anyhow::{Result, bail, ensure};
use netweevil_core::{CompiledProfileBundle, TopologyBundle};
use netweevil_profile::{BreakdownMetric, ReturnConfig};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaypointKind {
    #[default]
    Break,
    Through,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Waypoint {
    pub point: LabeledPoint,
    #[serde(default)]
    pub kind: WaypointKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaypointRequest {
    pub route_id: String,
    pub waypoints: Vec<Waypoint>,
    #[serde(default)]
    pub snap: SnapOptions,
    #[serde(default)]
    pub returns: ReturnConfig,
    #[serde(default)]
    pub optimize_order: bool,
    #[serde(default, flatten)]
    pub temporal: TemporalRequestOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaypointResult {
    pub route_id: String,
    /// Original input indexes in visit order; first and last remain fixed.
    pub waypoint_order: Vec<usize>,
    /// Through locations do not split legs.
    pub legs: Vec<RouteResult>,
    pub total_distance_m: u64,
    pub total_travel_time_s: f64,
    pub total_generalized_cost: f64,
    /// `held_karp_exact` minimizes directed static costs for the snapped stops.
    /// `input_order` means no stop permutation was requested.
    pub optimization_method: String,
}

pub(crate) fn execute_waypoints_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    graph: &RoutingGraph,
    request: &WaypointRequest,
) -> Result<WaypointResult> {
    ensure!(
        request.waypoints.len() >= 2,
        "waypoint routes require at least two locations"
    );
    ensure!(
        request.waypoints.len() <= 128,
        "waypoint routes support at most 128 locations"
    );
    ensure!(
        !request.temporal.requires_exact_labels() && request.temporal.holiday_calendar.is_none(),
        "waypoint routing supports static costs only; temporal, scenario, and component-constrained requests are not supported"
    );
    if request.optimize_order {
        ensure!(
            request.waypoints.len() <= 18,
            "exact stop ordering supports at most 16 intermediate locations"
        );
        ensure!(
            request.waypoints[1..request.waypoints.len() - 1]
                .iter()
                .all(|point| point.kind == WaypointKind::Break),
            "stop ordering supports break locations only; through locations carry turn history across legs"
        );
    }
    let snaps = request
        .waypoints
        .iter()
        .enumerate()
        .map(|(index, waypoint)| {
            snap_waypoint(
                topology,
                graph,
                &waypoint.point,
                &request.snap,
                index,
                request.waypoints.len(),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let order = if request.optimize_order {
        let costs = stop_cost_matrix(topology, metrics, graph, &snaps)?;
        optimize_stop_order(&costs)?
    } else {
        (0..snaps.len()).collect()
    };
    let mut legs = Vec::new();
    let mut start = 0;
    for end in 1..order.len() {
        if end + 1 != order.len() && request.waypoints[order[end]].kind == WaypointKind::Through {
            continue;
        }
        let locations = order[start..=end]
            .iter()
            .map(|&index| snaps[index].as_slice())
            .collect::<Vec<_>>();
        let solution = solve_ordered(topology, graph, &locations)?.ok_or_else(|| {
            anyhow::anyhow!(
                "no legal route through waypoint indexes {:?}",
                &order[start..=end]
            )
        })?;
        legs.push(render_leg(
            topology,
            metrics,
            graph,
            request,
            legs.len(),
            solution,
        )?);
        start = end;
    }
    Ok(WaypointResult {
        route_id: request.route_id.clone(),
        waypoint_order: order,
        total_distance_m: legs.iter().map(|leg| leg.summary.total_distance_m).sum(),
        total_travel_time_s: legs.iter().map(|leg| leg.summary.total_travel_time_s).sum(),
        total_generalized_cost: legs
            .iter()
            .map(|leg| leg.summary.total_generalized_cost)
            .sum(),
        optimization_method: if request.optimize_order {
            "held_karp_exact"
        } else {
            "input_order"
        }
        .into(),
        legs,
    })
}

/// Fix each location to its nearest routable physical position. Keep both
/// directions at that position so breaks may reverse without moving the stop.
fn snap_waypoint(
    topology: &TopologyBundle,
    graph: &RoutingGraph,
    point: &LabeledPoint,
    options: &SnapOptions,
    index: usize,
    count: usize,
) -> Result<Vec<SnappedPoint>> {
    let mut candidates = Vec::new();
    for is_origin in [true, false] {
        if (index == 0 && !is_origin) || (index + 1 == count && is_origin) {
            continue;
        }
        if let Ok(mut snapped) =
            snap_candidates_with_options(topology, graph, point, options, is_origin)
        {
            candidates.append(&mut snapped);
        }
    }
    candidates.sort_by(|left, right| left.snap_distance_m.total_cmp(&right.snap_distance_m));
    let Some(nearest) = candidates.first().cloned() else {
        return Err(route_snap_failure(point, options.max_distance_m).into());
    };
    candidates.retain(|candidate| {
        same_position(candidate, &nearest)
            && match (candidate.snapped_edge_id, nearest.snapped_edge_id) {
                (Some(left), Some(right)) => {
                    topology.routing_edge(left as usize).source_way_id
                        == topology.routing_edge(right as usize).source_way_id
                }
                _ => true,
            }
    });
    candidates.sort_by_key(snap_cache_key);
    candidates.dedup_by_key(|candidate| snap_cache_key(candidate));
    Ok(candidates)
}

struct Solution {
    path: Vec<usize>,
    origin: SnappedPoint,
    destination: SnappedPoint,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Key {
    edge: usize,
    automaton: usize,
    next: usize,
    no_uturn: bool,
}

struct Label {
    cost: f64,
    previous: Option<Key>,
    origin: usize,
}

#[derive(Clone, Copy)]
struct QueueEntry {
    key: Key,
    cost: f64,
}
impl PartialEq for QueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.cost.to_bits() == other.cost.to_bits()
    }
}
impl Eq for QueueEntry {}
impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .total_cmp(&self.cost)
            .then_with(|| other.key.cmp(&self.key))
    }
}

fn same_position(left: &SnappedPoint, right: &SnappedPoint) -> bool {
    let same_network_location = match (left.snapped_edge_id, right.snapped_edge_id) {
        (None, None) => left.snapped_node_id == right.snapped_node_id,
        (Some(a), Some(b)) => {
            a == b
                || (left.snapped_from_node_id == right.snapped_to_node_id
                    && left.snapped_to_node_id == right.snapped_from_node_id)
        }
        _ => false,
    };
    same_network_location
        && (left.snapped_lon - right.snapped_lon).abs() <= 1e-10
        && (left.snapped_lat - right.snapped_lat).abs() <= 1e-10
        && (left.snapped_z - right.snapped_z).abs() <= 1e-6
}

/// Advance through locations encountered on this directed traversal. Fractions
/// must be nondecreasing; crossing a through point never changes edge direction.
fn advance(
    topology: &TopologyBundle,
    edge: usize,
    mut next: usize,
    mut fraction: f64,
    locations: &[&[SnappedPoint]],
) -> (usize, bool, Option<(usize, f64)>) {
    let head = topology.routing_edge(edge).to.0;
    let mut no_uturn = false;
    while next < locations.len() {
        let matched = locations[next]
            .iter()
            .enumerate()
            .filter_map(|(index, point)| {
                let position = if let Some(id) = point.snapped_edge_id {
                    if id as usize != edge {
                        return None;
                    }
                    point.snapped_edge_fraction.unwrap_or_default()
                } else {
                    if point.snapped_node_id != head {
                        return None;
                    }
                    1.0
                };
                (position + 1e-12 >= fraction).then_some((index, position))
            })
            .min_by(|left, right| left.1.total_cmp(&right.1));
        let Some((candidate, position)) = matched else {
            break;
        };
        if next + 1 == locations.len() {
            return (next + 1, no_uturn, Some((candidate, position)));
        }
        no_uturn |= locations[next][candidate].snapped_edge_id.is_none();
        fraction = position;
        next += 1;
    }
    (next, no_uturn, None)
}

fn solve_ordered(
    topology: &TopologyBundle,
    graph: &RoutingGraph,
    locations: &[&[SnappedPoint]],
) -> Result<Option<Solution>> {
    let mut labels = FxHashMap::<Key, Label>::default();
    let mut heap = BinaryHeap::new();
    let mut best = f64::INFINITY;
    let mut goal = None;
    let minimum_adjustment = locations
        .last()
        .unwrap()
        .iter()
        .flat_map(|point| destination_edge_seeds(graph, point))
        .map(|(_, cost)| cost)
        .fold(0.0, f64::min);
    for (origin_index, origin) in locations[0].iter().enumerate() {
        let mut first = 1;
        while first < locations.len()
            && locations[first]
                .iter()
                .any(|point| same_position(origin, point))
        {
            first += 1;
        }
        if first == locations.len() {
            return Ok(Some(Solution {
                path: Vec::new(),
                origin: origin.clone(),
                destination: locations.last().unwrap()[0].clone(),
            }));
        }
        for (edge, cost) in origin_edge_seeds(graph, origin) {
            let fraction = if origin.snapped_edge_id == Some(edge as u32) {
                origin.snapped_edge_fraction.unwrap_or_default()
            } else {
                0.0
            };
            let (next, no_uturn, reached) = advance(topology, edge, first, fraction, locations);
            let key = Key {
                edge,
                automaton: graph.automaton.transition(0, edge),
                next,
                no_uturn,
            };
            if labels.get(&key).is_some_and(|label| label.cost <= cost) {
                continue;
            }
            labels.insert(
                key,
                Label {
                    cost,
                    previous: None,
                    origin: origin_index,
                },
            );
            if let Some((candidate, fraction)) = reached {
                let total = cost - graph.edge_costs[edge] * (1.0 - fraction);
                if total < best {
                    best = total;
                    goal = Some((key, candidate));
                }
            } else {
                heap.push(QueueEntry { key, cost });
            }
        }
    }
    while let Some(QueueEntry { key, cost }) = heap.pop() {
        if cost + minimum_adjustment >= best {
            break;
        }
        if cost > labels[&key].cost {
            continue;
        }
        let origin = labels[&key].origin;
        for slot in graph.transition_range(key.edge) {
            let edge = graph.transition_edges[slot] as usize;
            if !graph.automaton.is_transition_allowed(key.automaton, edge) {
                continue;
            }
            if key.no_uturn
                && topology.routing_edge(key.edge).from == topology.routing_edge(edge).to
            {
                continue;
            }
            let next_cost = cost + graph.transition_costs[slot];
            let (next, no_uturn, reached) = advance(topology, edge, key.next, 0.0, locations);
            let next_key = Key {
                edge,
                automaton: graph.automaton.transition(key.automaton, edge),
                next,
                no_uturn,
            };
            if !next_cost.is_finite()
                || labels
                    .get(&next_key)
                    .is_some_and(|label| label.cost <= next_cost)
            {
                continue;
            }
            labels.insert(
                next_key,
                Label {
                    cost: next_cost,
                    previous: Some(key),
                    origin,
                },
            );
            if let Some((candidate, fraction)) = reached {
                let total = next_cost - graph.edge_costs[edge] * (1.0 - fraction);
                if total < best {
                    best = total;
                    goal = Some((next_key, candidate));
                }
            } else {
                heap.push(QueueEntry {
                    key: next_key,
                    cost: next_cost,
                });
            }
        }
    }
    let Some((mut cursor, destination)) = goal else {
        return Ok(None);
    };
    let origin = labels[&cursor].origin;
    let mut path = Vec::new();
    loop {
        path.push(cursor.edge);
        let Some(previous) = labels[&cursor].previous else {
            break;
        };
        cursor = previous;
    }
    path.reverse();
    Ok(Some(Solution {
        path,
        origin: locations[0][origin].clone(),
        destination: locations.last().unwrap()[destination].clone(),
    }))
}

fn stop_cost_matrix(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    graph: &RoutingGraph,
    snaps: &[Vec<SnappedPoint>],
) -> Result<Vec<Vec<f64>>> {
    let mut costs = vec![vec![f64::INFINITY; snaps.len()]; snaps.len()];
    let fallback = FallbackPolicy::default();
    let mut search = RestrictedSearchScratch::default();
    for (origin_index, origins) in snaps.iter().enumerate().take(snaps.len() - 1) {
        let seeds = origins
            .iter()
            .flat_map(|point| origin_edge_seeds(graph, point))
            .collect::<Vec<_>>();
        search.prepare(metrics, graph, &seeds, &fallback);
        for (destination_index, destinations) in snaps.iter().enumerate().skip(1) {
            if origin_index == destination_index {
                costs[origin_index][destination_index] = 0.0;
                continue;
            }
            let seeds = destinations
                .iter()
                .flat_map(|point| destination_edge_seeds(graph, point))
                .collect::<Vec<_>>();
            let direct = origins
                .iter()
                .flat_map(|origin| {
                    destinations.iter().filter_map(move |destination| {
                        direct_same_edge_path(graph, origin, destination)
                    })
                })
                .min_by(|left, right| {
                    left.total_generalized_cost
                        .total_cmp(&right.total_generalized_cost)
                });
            if let Some(path) =
                search.route_to(topology, metrics, graph, &seeds, direct, &fallback)?
            {
                costs[origin_index][destination_index] = path.total_generalized_cost;
            }
        }
    }
    Ok(costs)
}

fn optimize_stop_order(costs: &[Vec<f64>]) -> Result<Vec<usize>> {
    let count = costs.len();
    let intermediate = count - 2;
    if intermediate == 0 {
        ensure!(costs[0][1].is_finite(), "fixed endpoints are unreachable");
        return Ok(vec![0, 1]);
    }
    let states = 1_usize << intermediate;
    let mut best = vec![f64::INFINITY; states * intermediate];
    let mut previous = vec![usize::MAX; states * intermediate];
    for next in 0..intermediate {
        best[(1 << next) * intermediate + next] = costs[0][next + 1];
    }
    for mask in 1..states {
        for last in 0..intermediate {
            if mask & (1 << last) == 0 {
                continue;
            }
            let known = best[mask * intermediate + last];
            if !known.is_finite() {
                continue;
            }
            for next in 0..intermediate {
                if mask & (1 << next) != 0 {
                    continue;
                }
                let next_mask = mask | (1 << next);
                let cost = known + costs[last + 1][next + 1];
                let slot = next_mask * intermediate + next;
                if cost < best[slot] {
                    best[slot] = cost;
                    previous[slot] = last;
                }
            }
        }
    }
    let full = states - 1;
    let last = (0..intermediate)
        .filter(|&last| (best[full * intermediate + last] + costs[last + 1][count - 1]).is_finite())
        .min_by(|&left, &right| {
            (best[full * intermediate + left] + costs[left + 1][count - 1])
                .total_cmp(&(best[full * intermediate + right] + costs[right + 1][count - 1]))
        });
    let Some(mut last) = last else {
        bail!("no directed route can visit all stops between the fixed endpoints");
    };
    let mut order = vec![count - 1];
    let mut mask = full;
    loop {
        order.push(last + 1);
        let prior = previous[mask * intermediate + last];
        mask ^= 1 << last;
        if prior == usize::MAX {
            break;
        }
        last = prior;
    }
    order.push(0);
    order.reverse();
    Ok(order)
}

fn render_leg(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    graph: &RoutingGraph,
    request: &WaypointRequest,
    leg_index: usize,
    solution: Solution,
) -> Result<RouteResult> {
    let Solution {
        path,
        origin,
        destination,
    } = solution;
    let hop_info = hop_info_for_pair(
        &origin,
        &destination,
        request.snap.max_distance_m,
        &ConnectivityPolicy::default(),
    )
    .ok_or_else(|| anyhow::anyhow!("waypoint endpoints have incompatible connectivity"))?;
    let mut route = crate::batch::batch_route_result_for_path(
        topology,
        metrics,
        graph,
        &format!("{}:{leg_index}", request.route_id),
        &FallbackPolicy::default(),
        &request.returns,
        &origin,
        &destination,
        hop_info,
        RoutePath {
            edge_indexes: path.clone(),
            total_generalized_cost: 0.0,
        },
    )?;
    if request_returns_detailed_path(&request.returns) {
        route.edge_path = path.iter().map(|&edge| edge as u32).collect();
        if let Some(&first) = path.first() {
            route.node_path.push(topology.routing_edge(first).from.0);
            route
                .node_path
                .extend(path.iter().map(|&edge| topology.routing_edge(edge).to.0));
        }
    }
    if let Some(breakdowns) = route.breakdowns.as_mut() {
        *breakdowns = RouteBreakdowns::default();
        for (position, &edge_index) in path.iter().enumerate() {
            let factor = edge_traversal_factor(
                edge_index,
                position == 0,
                position + 1 == path.len(),
                &origin,
                &destination,
            );
            let edge = topology.edge(edge_index);
            for (values, key, wanted) in [
                (
                    &mut breakdowns.road_class,
                    format!("{:?}", edge.road_class).to_lowercase(),
                    &request.returns.road_type_breakdown,
                ),
                (
                    &mut breakdowns.surface,
                    format!("{:?}", edge.surface).to_lowercase(),
                    &request.returns.surface_breakdown,
                ),
            ] {
                if wanted.is_empty() {
                    continue;
                }
                let entry = values.entry(key).or_default();
                for metric in wanted {
                    match metric {
                        BreakdownMetric::DistanceM => {
                            *entry.distance_m.get_or_insert(0) +=
                                (f64::from(edge.length_m) * factor).round() as u64
                        }
                        BreakdownMetric::TimeS => {
                            *entry.time_s.get_or_insert(0.0) += metrics.edge_metrics[edge_index]
                                .travel_time_s
                                .unwrap_or_default()
                                * factor
                        }
                    }
                }
            }
        }
    }
    Ok(route)
}
