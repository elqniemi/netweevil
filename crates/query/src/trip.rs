//! Optimized multi-stop trips over directed static costs.
//!
//! Each stop is snapped once as an arrival and once as a departure, with the
//! same candidates `/v1/route` uses: every leg leaves from and arrives at the
//! nearest network position that has a route, in either travel direction.
//! One directed matrix (the parallel CCH many-to-many engine when the dataset
//! has a hierarchy) prices every ordered stop pair. [`solver`] orders the
//! stops for the endpoint mode, service times and time windows, and only the
//! chosen legs are routed again with the requested returns. Times are
//! seconds on the caller's clock (for example since midnight); temporal
//! routing is not supported.

pub mod solver;

use std::time::{Duration, Instant};

use anyhow::{Result, bail, ensure};
use netweevil_core::{CompiledProfileBundle, TopologyBundle};
use netweevil_profile::ReturnConfig;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use solver::{SolverOptions, TimeWindow, TourProblem, TourSolution};

use crate::*;

/// Largest trip; the matrix has `stops²` cells.
pub const MAX_TRIP_STOPS: usize = 2_000;
const LATE_EPSILON_S: f64 = 1e-6;

/// Which stops are fixed at the ends of the trip.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TripMode {
    /// Start at the first stop and return to it.
    #[default]
    Roundtrip,
    /// Start at the first stop and end at any stop.
    FixedStart,
    /// Start at any stop and end at the last stop.
    FixedEnd,
    /// Start at the first stop and end at the last stop.
    FixedEndpoints,
    /// Start and end at any stops.
    Open,
}

/// Service may start from `earliest_s` (the vehicle waits) and must start
/// by `latest_s`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TripTimeWindow {
    #[serde(default)]
    pub earliest_s: f64,
    #[serde(default)]
    pub latest_s: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TripStop {
    pub point: LabeledPoint,
    #[serde(default)]
    pub service_s: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_window: Option<TripTimeWindow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TripOptimizerOptions {
    /// Seed for the local search kicks; equal seeds give equal orders.
    #[serde(default)]
    pub seed: u64,
    /// Local search kicks; defaults to a size-dependent count.
    #[serde(default)]
    pub max_iterations: Option<usize>,
    /// Wall-clock cap on the search. Orders are reproducible only when the
    /// iteration limit ends the search first.
    #[serde(default)]
    pub time_budget_ms: Option<u64>,
    /// Orders with at most this many free stops are solved exactly
    /// (default 16, at most 18).
    #[serde(default)]
    pub exact_max_stops: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TripRequest {
    pub trip_id: String,
    pub stops: Vec<TripStop>,
    #[serde(default)]
    pub mode: TripMode,
    /// The trip may not start earlier than this.
    #[serde(default)]
    pub departure_s: f64,
    /// Return the least-late order instead of failing when no order meets
    /// every time window.
    #[serde(default)]
    pub allow_late: bool,
    #[serde(default)]
    pub snap: SnapOptions,
    #[serde(default)]
    pub returns: ReturnConfig,
    #[serde(default)]
    pub optimizer: TripOptimizerOptions,
}

/// Arrival and service at one stop of the trip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TripVisit {
    pub stop_index: usize,
    pub point_id: String,
    pub arrival_s: f64,
    pub wait_s: f64,
    pub service_start_s: f64,
    pub departure_s: f64,
    pub lateness_s: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TripOptimization {
    /// `held_karp_exact`, `held_karp_time_windows_exact` or
    /// `iterated_local_search`.
    pub method: String,
    pub proven_optimal: bool,
    /// The exact search proved that no order meets every time window.
    pub proven_infeasible: bool,
    pub seed: u64,
    pub iterations: usize,
    /// Generalized cost of visiting the stops in request order, when every
    /// leg of that order has a route.
    pub input_order_cost: Option<f64>,
    /// Best construction before local search.
    pub construction_cost: Option<f64>,
    pub cost: f64,
    pub matrix_ms: f64,
    pub solve_ms: f64,
    pub legs_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TripResult {
    pub trip_id: String,
    pub mode: TripMode,
    /// Request stop indexes in visit order; a roundtrip ends with its start.
    pub stop_order: Vec<usize>,
    /// `legs[k]` runs from `stop_order[k]` to `stop_order[k + 1]`.
    pub legs: Vec<RouteResult>,
    /// One visit per `stop_order` entry.
    pub visits: Vec<TripVisit>,
    pub total_distance_m: u64,
    pub total_travel_time_s: f64,
    pub total_generalized_cost: f64,
    pub total_service_s: f64,
    pub total_wait_s: f64,
    pub start_s: f64,
    pub end_s: f64,
    pub time_windows_met: bool,
    pub total_lateness_s: f64,
    /// Stops whose service starts after their window closes.
    pub late_stops: Vec<usize>,
    pub optimization: TripOptimization,
}

pub(crate) fn execute_trip_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    graph: &RoutingGraph,
    request: &TripRequest,
) -> Result<TripResult> {
    validate_trip_request(request)?;
    let matrix_started = Instant::now();
    let matrix = trip_stop_matrix(topology, metrics, graph, request)?;
    let matrix_ms = elapsed_ms(matrix_started);

    let solve_started = Instant::now();
    let shaped = shape_problem(request.mode, &matrix, Some(&trip_timing(request)));
    let options = solver_options(&request.optimizer);
    let solution = solver::solve(&shaped.problem, &options);
    let solve_ms = elapsed_ms(solve_started);
    let stop_order = shaped.stop_order(&solution.order);
    if solution.value.missing_legs > 0 {
        bail!(
            "{}",
            unroutable_message(request, &matrix, &stop_order, &solution)
        );
    }

    let legs_started = Instant::now();
    let legs = route_trip_legs(topology, metrics, graph, request, &matrix, &stop_order)?;
    let legs_ms = elapsed_ms(legs_started);
    let visits = trip_visits(request, &stop_order, &legs);
    let late_stops = visits
        .iter()
        .filter(|visit| visit.lateness_s > LATE_EPSILON_S)
        .map(|visit| visit.stop_index)
        .collect::<Vec<_>>();
    if !late_stops.is_empty() && !request.allow_late {
        bail!(
            "{}",
            infeasible_message(request, &matrix, &visits, &solution)
        );
    }
    let total_generalized_cost = legs
        .iter()
        .map(|leg| leg.summary.total_generalized_cost)
        .sum();
    let finite_cost = |value: solver::TourValue| (value.missing_legs == 0).then_some(value.cost);
    Ok(TripResult {
        trip_id: request.trip_id.clone(),
        mode: request.mode,
        total_distance_m: legs.iter().map(|leg| leg.summary.total_distance_m).sum(),
        total_travel_time_s: legs.iter().map(|leg| leg.summary.total_travel_time_s).sum(),
        total_generalized_cost,
        total_service_s: visits
            .iter()
            .map(|visit| visit.departure_s - visit.service_start_s)
            .sum(),
        total_wait_s: visits.iter().map(|visit| visit.wait_s).sum(),
        start_s: visits.first().map_or(0.0, |visit| visit.arrival_s),
        end_s: visits.last().map_or(0.0, |visit| visit.departure_s),
        time_windows_met: late_stops.is_empty(),
        total_lateness_s: visits.iter().map(|visit| visit.lateness_s).sum(),
        late_stops,
        optimization: TripOptimization {
            method: solution.method.as_str().into(),
            proven_optimal: solution.proven_optimal,
            proven_infeasible: solution.proven_infeasible,
            seed: options.seed,
            iterations: solution.iterations,
            input_order_cost: finite_cost(solution.input_order_value),
            construction_cost: finite_cost(solution.construction_value),
            cost: solution.value.cost,
            matrix_ms,
            solve_ms,
            legs_ms,
        },
        stop_order,
        legs,
        visits,
    })
}

/// The directed tour instance a trip request reduces to, for benchmarking
/// the solver on real matrices.
pub(crate) fn trip_tour_problem_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    graph: &RoutingGraph,
    request: &TripRequest,
) -> Result<TourProblem> {
    validate_trip_request(request)?;
    let matrix = trip_stop_matrix(topology, metrics, graph, request)?;
    Ok(shape_problem(request.mode, &matrix, Some(&trip_timing(request))).problem)
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn validate_trip_request(request: &TripRequest) -> Result<()> {
    ensure!(
        (2..=MAX_TRIP_STOPS).contains(&request.stops.len()),
        "trips need 2 to {MAX_TRIP_STOPS} stops, got {}",
        request.stops.len()
    );
    ensure!(
        request.departure_s.is_finite() && request.departure_s >= 0.0,
        "departure_s must be a finite, non-negative number of seconds"
    );
    for (index, stop) in request.stops.iter().enumerate() {
        let id = &stop.point.id;
        ensure!(
            stop.service_s.is_finite() && stop.service_s >= 0.0,
            "stop {index} ('{id}') service_s must be finite and non-negative"
        );
        if let Some(window) = stop.time_window {
            ensure!(
                window.earliest_s.is_finite() && window.earliest_s >= 0.0,
                "stop {index} ('{id}') time_window.earliest_s must be finite and non-negative"
            );
            if let Some(latest) = window.latest_s {
                ensure!(
                    latest.is_finite() && latest >= window.earliest_s,
                    "stop {index} ('{id}') time_window.latest_s must be finite and not before earliest_s"
                );
            }
        }
    }
    let optimizer = &request.optimizer;
    if let Some(exact) = optimizer.exact_max_stops {
        ensure!(
            exact <= solver::MAX_EXACT_STOPS,
            "optimizer.exact_max_stops must be at most {}",
            solver::MAX_EXACT_STOPS
        );
    }
    ensure!(
        optimizer.time_budget_ms != Some(0),
        "optimizer.time_budget_ms must be positive"
    );
    Ok(())
}

fn solver_options(options: &TripOptimizerOptions) -> SolverOptions {
    SolverOptions {
        seed: options.seed,
        exact_max_stops: options
            .exact_max_stops
            .unwrap_or(solver::DEFAULT_EXACT_MAX_STOPS),
        max_iterations: options.max_iterations,
        time_budget: options.time_budget_ms.map(Duration::from_millis),
    }
}

/// Whether stop `index` is ever left or reached in `mode`.
fn stop_roles(mode: TripMode, index: usize, count: usize) -> (bool, bool) {
    let fixed_start = matches!(mode, TripMode::FixedStart | TripMode::FixedEndpoints);
    let fixed_end = matches!(mode, TripMode::FixedEnd | TripMode::FixedEndpoints);
    let departs = !(fixed_end && index + 1 == count);
    let arrives = !(fixed_start && index == 0);
    (departs, arrives)
}

fn trip_stop_matrix(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    graph: &RoutingGraph,
    request: &TripRequest,
) -> Result<StopMatrix> {
    let count = request.stops.len();
    let snaps = request
        .stops
        .par_iter()
        .enumerate()
        .map(|(index, stop)| {
            let (departs, arrives) = stop_roles(request.mode, index, count);
            snap_stop(
                topology,
                graph,
                index,
                &stop.point,
                &request.snap,
                departs,
                arrives,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let (origins, destinations): (Vec<_>, Vec<_>) = snaps.into_iter().unzip();
    Ok(stop_matrix(
        topology,
        metrics,
        graph,
        origins,
        destinations,
        request.snap.max_distance_m,
    ))
}

/// Departure and arrival candidates of a stop, as route requests snap them.
fn snap_stop(
    topology: &TopologyBundle,
    graph: &RoutingGraph,
    index: usize,
    point: &LabeledPoint,
    options: &SnapOptions,
    departs: bool,
    arrives: bool,
) -> Result<(Vec<SnappedPoint>, Vec<SnappedPoint>)> {
    let snap = |wanted: bool, is_origin: bool| -> Result<Vec<SnappedPoint>> {
        if !wanted {
            return Ok(Vec::new());
        }
        snap_candidates_with_options(topology, graph, point, options, is_origin)
            .map_err(|error| anyhow::anyhow!("stop {index}: {error:#}"))
    };
    Ok((snap(departs, true)?, snap(arrives, false)?))
}

/// Directed costs and travel times between stops; `INFINITY` where no route
/// exists or a role is unused.
pub(crate) struct StopMatrix {
    count: usize,
    cost: Vec<f64>,
    time: Vec<f64>,
    origins: Vec<Vec<SnappedPoint>>,
    destinations: Vec<Vec<SnappedPoint>>,
    snap_max_distance_m: f64,
}

impl StopMatrix {
    fn cost(&self, from: usize, to: usize) -> f64 {
        self.cost[from * self.count + to]
    }

    fn time(&self, from: usize, to: usize) -> f64 {
        self.time[from * self.count + to]
    }
}

/// Prices every ordered pair of stops with candidate sets on both sides.
/// Empty sets mark roles a stop never takes.
pub(crate) fn stop_matrix(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    graph: &RoutingGraph,
    origins: Vec<Vec<SnappedPoint>>,
    destinations: Vec<Vec<SnappedPoint>>,
    snap_max_distance_m: f64,
) -> StopMatrix {
    let count = origins.len();
    let rows = (0..count)
        .filter(|&stop| !origins[stop].is_empty())
        .collect::<Vec<_>>();
    let columns = (0..count)
        .filter(|&stop| !destinations[stop].is_empty())
        .collect::<Vec<_>>();
    let row_sets = rows
        .iter()
        .map(|&stop| origins[stop].clone())
        .collect::<Vec<_>>();
    let column_sets = columns
        .iter()
        .map(|&stop| destinations[stop].clone())
        .collect::<Vec<_>>();
    let cells = route_set_matrix(
        topology,
        metrics,
        graph,
        &row_sets,
        &column_sets,
        snap_max_distance_m,
        |row, column, route| {
            if rows[row] == columns[column] {
                return (0.0, 0.0);
            }
            route.map_or((f64::INFINITY, f64::INFINITY), |route| {
                (
                    route.summary.total_generalized_cost,
                    route.summary.total_travel_time_s,
                )
            })
        },
    );
    let mut cost = vec![f64::INFINITY; count * count];
    let mut time = vec![f64::INFINITY; count * count];
    for stop in 0..count {
        cost[stop * count + stop] = 0.0;
        time[stop * count + stop] = 0.0;
    }
    for (row, values) in rows.iter().zip(cells) {
        for (column, (cell_cost, cell_time)) in columns.iter().zip(values) {
            cost[row * count + column] = cell_cost;
            time[row * count + column] = cell_time;
        }
    }
    StopMatrix {
        count,
        cost,
        time,
        origins,
        destinations,
        snap_max_distance_m,
    }
}

/// Service times, windows and earliest start, per request stop.
pub(crate) struct StopTiming {
    service_s: Vec<f64>,
    window: Vec<TimeWindow>,
    departure_s: f64,
}

fn trip_timing(request: &TripRequest) -> StopTiming {
    StopTiming {
        service_s: request.stops.iter().map(|stop| stop.service_s).collect(),
        window: request.stops.iter().map(stop_window).collect(),
        departure_s: request.departure_s,
    }
}

fn stop_window(stop: &TripStop) -> TimeWindow {
    stop.time_window
        .map_or(TimeWindow::OPEN, |window| TimeWindow {
            earliest_s: window.earliest_s,
            latest_s: window.latest_s.unwrap_or(f64::INFINITY),
        })
}

/// A tour instance whose depot (node 0) encodes the endpoint mode: it leaves
/// from the start stop or a free dummy, and returns to the end stop or a
/// free dummy.
pub(crate) struct ShapedProblem {
    pub(crate) problem: TourProblem,
    mode: TripMode,
    count: usize,
    /// Request stop of each non-depot node.
    node_stop: Vec<usize>,
}

impl ShapedProblem {
    /// Request stop indexes in visit order for a solver order.
    pub(crate) fn stop_order(&self, order: &[usize]) -> Vec<usize> {
        let last = self.count - 1;
        let visits = order[1..].iter().map(|&node| self.node_stop[node]);
        match self.mode {
            TripMode::Roundtrip => std::iter::once(0).chain(visits).chain([0]).collect(),
            TripMode::FixedStart => std::iter::once(0).chain(visits).collect(),
            TripMode::FixedEnd => visits.chain([last]).collect(),
            TripMode::FixedEndpoints => std::iter::once(0).chain(visits).chain([last]).collect(),
            TripMode::Open => visits.collect(),
        }
    }
}

pub(crate) fn shape_problem(
    mode: TripMode,
    matrix: &StopMatrix,
    timing: Option<&StopTiming>,
) -> ShapedProblem {
    let count = matrix.count;
    let last = count - 1;
    // Stop the depot departs from and arrives at; `None` is a free end.
    let (depot_departure, depot_arrival, free_stops): (_, _, Vec<usize>) = match mode {
        TripMode::Roundtrip => (Some(0), Some(0), (1..count).collect()),
        TripMode::FixedStart => (Some(0), None, (1..count).collect()),
        TripMode::FixedEnd => (None, Some(last), (0..last).collect()),
        TripMode::FixedEndpoints => (Some(0), Some(last), (1..last).collect()),
        TripMode::Open => (None, None, (0..count).collect()),
    };
    let mut node_stop = vec![usize::MAX];
    node_stop.extend(&free_stops);
    let size = node_stop.len();
    let mut cost = vec![0.0; size * size];
    let mut time = vec![0.0; size * size];
    for from in 0..size {
        let origin = if from == 0 {
            depot_departure
        } else {
            Some(node_stop[from])
        };
        for to in 0..size {
            let destination = if to == 0 {
                depot_arrival
            } else {
                Some(node_stop[to])
            };
            if let (Some(origin), Some(destination)) = (origin, destination)
                && origin != destination
            {
                cost[from * size + to] = matrix.cost(origin, destination);
                time[from * size + to] = matrix.time(origin, destination);
            }
        }
    }
    let mut problem = TourProblem::new(size, cost);
    if let Some(timing) = timing {
        let mut service = vec![0.0; size];
        let mut window = vec![TimeWindow::OPEN; size];
        for node in 1..size {
            service[node] = timing.service_s[node_stop[node]];
            window[node] = timing.window[node_stop[node]];
        }
        let start = depot_departure.map(|stop| (timing.service_s[stop], timing.window[stop]));
        service[0] = start.map_or(0.0, |(service, _)| service);
        window[0] = TimeWindow {
            earliest_s: start.map_or(timing.departure_s, |(_, window)| {
                window.earliest_s.max(timing.departure_s)
            }),
            latest_s: f64::INFINITY,
        };
        let return_window = depot_arrival.map_or(TimeWindow::OPEN, |stop| TimeWindow {
            earliest_s: f64::NEG_INFINITY,
            latest_s: timing.window[stop].latest_s,
        });
        problem = problem.with_schedule(time, service, window, return_window);
    }
    ShapedProblem {
        problem,
        mode,
        count,
        node_stop,
    }
}

fn route_trip_legs(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    graph: &RoutingGraph,
    request: &TripRequest,
    matrix: &StopMatrix,
    stop_order: &[usize],
) -> Result<Vec<RouteResult>> {
    let pairs = stop_order
        .windows(2)
        .map(|pair| (pair[0], pair[1]))
        .collect::<Vec<_>>();
    route_set_pairs(
        topology,
        metrics,
        graph,
        &matrix.origins,
        &matrix.destinations,
        &pairs,
        matrix.snap_max_distance_m,
        &request.returns,
        |index, route| {
            let (from, to) = pairs[index];
            let mut route = route.cloned().map_err(|error| {
                anyhow::anyhow!(
                    "trip leg {index} from stop {from} ('{}') to stop {to} ('{}') failed: {error:#}",
                    request.stops[from].point.id,
                    request.stops[to].point.id
                )
            })?;
            route.route_id = format!("{}:{index}", request.trip_id);
            Ok(route)
        },
    )
    .into_iter()
    .collect()
}

/// Arrival, waiting, service and lateness along the routed legs. The first
/// stop starts at the trip departure (or its window opening); a roundtrip's
/// return to the start neither waits nor serves.
fn trip_visits(
    request: &TripRequest,
    stop_order: &[usize],
    legs: &[RouteResult],
) -> Vec<TripVisit> {
    let mut visits = Vec::with_capacity(stop_order.len());
    let mut clock = request.departure_s;
    for (position, &stop) in stop_order.iter().enumerate() {
        let window = stop_window(&request.stops[stop]);
        let arrival = if position == 0 {
            clock.max(window.earliest_s)
        } else {
            clock + legs[position - 1].summary.total_travel_time_s
        };
        let returning = request.mode == TripMode::Roundtrip && position + 1 == stop_order.len();
        let service_start = if returning {
            arrival
        } else {
            arrival.max(window.earliest_s)
        };
        let service = if returning {
            0.0
        } else {
            request.stops[stop].service_s
        };
        clock = service_start + service;
        visits.push(TripVisit {
            stop_index: stop,
            point_id: request.stops[stop].point.id.clone(),
            arrival_s: arrival,
            wait_s: service_start - arrival,
            service_start_s: service_start,
            departure_s: clock,
            lateness_s: (arrival - window.latest_s).max(0.0),
        });
    }
    visits
}

fn stop_label(request: &TripRequest, stop: usize) -> String {
    format!("stop {stop} ('{}')", request.stops[stop].point.id)
}

fn unroutable_message(
    request: &TripRequest,
    matrix: &StopMatrix,
    stop_order: &[usize],
    solution: &TourSolution,
) -> String {
    let count = matrix.count;
    let mut problems = Vec::new();
    for stop in 0..count {
        let (departs, arrives) = stop_roles(request.mode, stop, count);
        let others = (0..count).filter(|&other| other != stop);
        if departs
            && others
                .clone()
                .all(|other| !matrix.cost(stop, other).is_finite())
        {
            problems.push(format!(
                "{} has no route to any other stop",
                stop_label(request, stop)
            ));
        }
        if arrives
            && others
                .clone()
                .all(|other| !matrix.cost(other, stop).is_finite())
        {
            problems.push(format!(
                "no other stop has a route to {}",
                stop_label(request, stop)
            ));
        }
    }
    if problems.is_empty() {
        problems = stop_order
            .windows(2)
            .filter(|pair| !matrix.cost(pair[0], pair[1]).is_finite())
            .map(|pair| {
                format!(
                    "no route from {} to {}",
                    stop_label(request, pair[0]),
                    stop_label(request, pair[1])
                )
            })
            .collect();
    }
    format!(
        "no visiting order connects every stop ({} without a route in the best order found by {}): {}",
        solution.value.missing_legs,
        solution.method.as_str(),
        summarize(problems)
    )
}

fn infeasible_message(
    request: &TripRequest,
    matrix: &StopMatrix,
    visits: &[TripVisit],
    solution: &TourSolution,
) -> String {
    let proof = if solution.proven_infeasible {
        "the exact search proved that no order meets every time window"
    } else {
        "the best order found by local search misses some time windows"
    };
    let late = visits
        .iter()
        .filter(|visit| visit.lateness_s > LATE_EPSILON_S)
        .map(|visit| {
            format!(
                "{} arrives at {:.0} s, {:.0} s after its window closes",
                stop_label(request, visit.stop_index),
                visit.arrival_s,
                visit.lateness_s
            )
        })
        .collect::<Vec<_>>();
    let mut message = format!("{proof}: {}", summarize(late));
    if matches!(
        request.mode,
        TripMode::Roundtrip | TripMode::FixedStart | TripMode::FixedEndpoints
    ) {
        let start = &request.stops[0];
        let leave = request.departure_s.max(stop_window(start).earliest_s) + start.service_s;
        let hopeless = (1..request.stops.len())
            .filter(|&stop| {
                leave + matrix.time(0, stop)
                    > stop_window(&request.stops[stop]).latest_s + LATE_EPSILON_S
            })
            .map(|stop| stop_label(request, stop))
            .collect::<Vec<_>>();
        if !hopeless.is_empty() {
            message.push_str(&format!(
                "; even driving straight from the start cannot reach {} in time",
                summarize(hopeless)
            ));
        }
    }
    message.push_str("; set allow_late to return the least-late order");
    message
}

fn summarize(items: Vec<String>) -> String {
    const SHOWN: usize = 8;
    let hidden = items.len().saturating_sub(SHOWN);
    let mut text = items.into_iter().take(SHOWN).collect::<Vec<_>>().join("; ");
    if hidden > 0 {
        text.push_str(&format!("; and {hidden} more"));
    }
    text
}
