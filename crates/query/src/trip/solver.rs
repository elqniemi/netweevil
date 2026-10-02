//! Directed tour optimization over a dense cost matrix.
//!
//! Node 0 is the depot: every tour leaves it first and returns to it last.
//! Fixed, free and shared endpoints are expressed by shaping the depot's row
//! and column (see `trip.rs`). Costs are asymmetric; `f64::INFINITY` marks a
//! pair without a route.
//!
//! Instances with at most `exact_max_stops` non-depot nodes are solved exactly
//! by Held–Karp dynamic programming, with Pareto labels over cost and
//! departure time when time windows apply. Larger instances start from the
//! best of several constructions (input order, nearest neighbor, greedy arc
//! matching and, with windows, earliest deadline first) and improve by
//! iterated local search: 2-opt priced with directed segment costs, Or-opt
//! moves of up to three consecutive stops in either orientation, and the
//! reversal-free or-3opt segment exchange. Moves are drawn from per-node
//! neighbor lists and driven by a work queue (don't-look bits). Each round
//! kicks the incumbent with a localized double bridge and keeps the result
//! unless it is worse. Runs are deterministic for a seed unless a wall-clock
//! budget ends them early.
//!
//! The objective is lexicographic: fewest legs without a route, then least
//! total lateness, then least generalized cost. Windows are hard in the exact
//! search; the heuristic minimizes lateness so infeasible instances still get
//! the least-late order.

use std::time::{Duration, Instant};

/// Largest exact instance, in non-depot nodes. Held–Karp state is
/// `2^k * k` entries, so 16 stays under a second and ~20 MB.
pub const DEFAULT_EXACT_MAX_STOPS: usize = 16;
/// Hard ceiling on exact instances regardless of options.
pub const MAX_EXACT_STOPS: usize = 18;
const NEIGHBORS: usize = 10;
const MAX_EXACT_LABELS: usize = 4_000_000;
const MAX_KICK_SEGMENT: usize = 30;
const COST_EPSILON: f64 = 1e-9;
const TIME_EPSILON: f64 = 1e-6;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeWindow {
    pub earliest_s: f64,
    pub latest_s: f64,
}

impl TimeWindow {
    pub const OPEN: Self = Self {
        earliest_s: f64::NEG_INFINITY,
        latest_s: f64::INFINITY,
    };
}

#[derive(Debug, Clone)]
struct Schedule {
    time: Vec<f64>,
    service: Vec<f64>,
    window: Vec<TimeWindow>,
    return_window: TimeWindow,
}

/// A directed tour instance. Node 0 departs within `window[0]` after its
/// service time and must return within `return_window`.
#[derive(Debug, Clone)]
pub struct TourProblem {
    size: usize,
    cost: Vec<f64>,
    schedule: Option<Schedule>,
}

impl TourProblem {
    /// `cost` is row-major `size * size`; diagonal entries are ignored.
    pub fn new(size: usize, cost: Vec<f64>) -> Self {
        assert!(size >= 1, "a tour needs a depot");
        assert_eq!(cost.len(), size * size, "cost matrix must be square");
        Self {
            size,
            cost,
            schedule: None,
        }
    }

    /// Adds travel times, service times and time windows. Window 0 is the
    /// depot's departure window; `return_window` bounds the final arrival.
    pub fn with_schedule(
        mut self,
        time: Vec<f64>,
        service: Vec<f64>,
        window: Vec<TimeWindow>,
        return_window: TimeWindow,
    ) -> Self {
        assert_eq!(time.len(), self.size * self.size);
        assert_eq!(service.len(), self.size);
        assert_eq!(window.len(), self.size);
        self.schedule = Some(Schedule {
            time,
            service,
            window,
            return_window,
        });
        self
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn cost(&self, from: usize, to: usize) -> f64 {
        self.cost[from * self.size + to]
    }

    /// Whether any deadline can make an order late. Opening times alone
    /// only delay the schedule and never change which order is best.
    fn has_windows(&self) -> bool {
        self.schedule.as_ref().is_some_and(|schedule| {
            schedule.return_window.latest_s.is_finite()
                || schedule
                    .window
                    .iter()
                    .any(|window| window.latest_s.is_finite())
        })
    }

    /// Prices a tour given as the depot followed by every other node once.
    pub fn evaluate(&self, order: &[usize]) -> TourValue {
        debug_assert!(is_tour(order, self.size));
        let mut value = TourValue::default();
        let closed = order.iter().copied().chain(std::iter::once(0));
        let mut previous = order[0];
        for node in closed.skip(1) {
            let cost = self.cost(previous, node);
            if cost.is_finite() {
                value.cost += cost;
            } else {
                value.missing_legs += 1;
            }
            previous = node;
        }
        if let Some(schedule) = &self.schedule {
            value.lateness_s = schedule_lateness(schedule, self.size, order);
        }
        value
    }
}

fn is_tour(order: &[usize], size: usize) -> bool {
    let mut seen = vec![false; size];
    order.len() == size
        && order.first() == Some(&0)
        && order
            .iter()
            .all(|&node| node < size && !std::mem::replace(&mut seen[node], true))
}

fn schedule_lateness(schedule: &Schedule, size: usize, order: &[usize]) -> f64 {
    let mut time = schedule.window[0].earliest_s.max(0.0) + schedule.service[0];
    let mut lateness = 0.0;
    let mut previous = 0;
    for &node in order.iter().skip(1).chain(std::iter::once(&0)) {
        let arrival = time + schedule.time[previous * size + node];
        let window = if node == 0 {
            schedule.return_window
        } else {
            schedule.window[node]
        };
        lateness += (arrival - window.latest_s).max(0.0);
        time = arrival.max(window.earliest_s) + schedule.service[node];
        previous = node;
    }
    lateness
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TourValue {
    /// Generalized cost of the legs that have a route.
    pub cost: f64,
    /// Total time by which service starts after window closings.
    pub lateness_s: f64,
    /// Legs without any route.
    pub missing_legs: usize,
}

impl TourValue {
    /// Lexicographic: missing legs, lateness, cost.
    pub fn is_better_than(&self, other: &Self) -> bool {
        if self.missing_legs != other.missing_legs {
            return self.missing_legs < other.missing_legs;
        }
        if (self.lateness_s - other.lateness_s).abs() > TIME_EPSILON {
            return self.lateness_s < other.lateness_s;
        }
        self.cost < other.cost - COST_EPSILON * other.cost.abs().max(1.0)
    }
}

#[derive(Debug, Clone)]
pub struct SolverOptions {
    pub seed: u64,
    /// Instances with at most this many non-depot nodes are solved exactly.
    pub exact_max_stops: usize,
    /// Local search kicks; defaults to a size-dependent count.
    pub max_iterations: Option<usize>,
    /// Wall-clock cap. Results stay deterministic only when the iteration
    /// limit is reached first.
    pub time_budget: Option<Duration>,
}

impl Default for SolverOptions {
    fn default() -> Self {
        Self {
            seed: 0,
            exact_max_stops: DEFAULT_EXACT_MAX_STOPS,
            max_iterations: None,
            time_budget: None,
        }
    }
}

/// Default kick count: enough for the local optimum to settle on a few
/// thousand stops in about a second.
pub fn default_iterations(size: usize) -> usize {
    (size * 20).clamp(2_000, 40_000)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TourMethod {
    HeldKarp,
    HeldKarpTimeWindows,
    LocalSearch,
}

impl TourMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HeldKarp => "held_karp_exact",
            Self::HeldKarpTimeWindows => "held_karp_time_windows_exact",
            Self::LocalSearch => "iterated_local_search",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TourSolution {
    /// Depot first, then every other node once; the return is implied.
    pub order: Vec<usize>,
    pub value: TourValue,
    pub method: TourMethod,
    /// The exact search proved this order optimal.
    pub proven_optimal: bool,
    /// The exact search proved that no order meets every window.
    pub proven_infeasible: bool,
    /// Local search kicks performed.
    pub iterations: usize,
    /// Value of nodes visited in index order.
    pub input_order_value: TourValue,
    /// Best construction before local search.
    pub construction_value: TourValue,
}

pub fn solve(problem: &TourProblem, options: &SolverOptions) -> TourSolution {
    let size = problem.size;
    let input_order = (0..size).collect::<Vec<_>>();
    let input_order_value = problem.evaluate(&input_order);
    let solution = |order: Vec<usize>, method, proven_optimal| TourSolution {
        value: problem.evaluate(&order),
        order,
        method,
        proven_optimal,
        proven_infeasible: false,
        iterations: 0,
        input_order_value,
        construction_value: input_order_value,
    };
    let windows = problem.has_windows();
    let exact_method = if windows {
        TourMethod::HeldKarpTimeWindows
    } else {
        TourMethod::HeldKarp
    };
    if size <= 3 {
        // At most two orders: compare them directly.
        let mut best = input_order.clone();
        if size == 3
            && problem
                .evaluate(&[0, 2, 1])
                .is_better_than(&input_order_value)
        {
            best = vec![0, 2, 1];
        }
        let mut result = solution(best, exact_method, true);
        result.proven_infeasible = result.value.missing_legs == 0 && result.value.lateness_s > 0.0;
        result.proven_optimal = result.value.missing_legs == 0 && !result.proven_infeasible;
        return result;
    }
    let stops = size - 1;
    let mut proven_infeasible = false;
    if stops <= options.exact_max_stops.min(MAX_EXACT_STOPS) {
        if windows {
            match held_karp_time_windows(problem) {
                ExactOutcome::Optimal(order) => return solution(order, exact_method, true),
                ExactOutcome::Infeasible => proven_infeasible = true,
                ExactOutcome::LabelLimit => {}
            }
        } else if let Some(order) = held_karp(problem) {
            return solution(order, exact_method, true);
        }
    }
    let mut result = local_search(problem, options, windows);
    result.input_order_value = input_order_value;
    result.proven_infeasible = proven_infeasible;
    result
}

// ---------------------------------------------------------------------------
// Exact search
// ---------------------------------------------------------------------------

/// Held–Karp over directed costs; `None` when no tour has a route on every
/// leg.
fn held_karp(problem: &TourProblem) -> Option<Vec<usize>> {
    let stops = problem.size - 1;
    let states = 1_usize << stops;
    let mut best = vec![f64::INFINITY; states * stops];
    let mut previous = vec![u8::MAX; states * stops];
    for next in 0..stops {
        best[(1 << next) * stops + next] = problem.cost(0, next + 1);
    }
    for mask in 1..states {
        for last in 0..stops {
            if mask & (1 << last) == 0 {
                continue;
            }
            let known = best[mask * stops + last];
            if !known.is_finite() {
                continue;
            }
            let row = (last + 1) * problem.size + 1;
            for next in 0..stops {
                if mask & (1 << next) != 0 {
                    continue;
                }
                let cost = known + problem.cost[row + next];
                let slot = (mask | (1 << next)) * stops + next;
                if cost < best[slot] {
                    best[slot] = cost;
                    previous[slot] = last as u8;
                }
            }
        }
    }
    let full = states - 1;
    let total = |last: usize| best[full * stops + last] + problem.cost(last + 1, 0);
    let mut last = (0..stops)
        .filter(|&last| total(last).is_finite())
        .min_by(|&left, &right| total(left).total_cmp(&total(right)))?;
    let mut order = Vec::with_capacity(problem.size);
    let mut mask = full;
    loop {
        order.push(last + 1);
        let prior = previous[mask * stops + last];
        mask ^= 1 << last;
        if prior == u8::MAX {
            break;
        }
        last = prior as usize;
    }
    order.push(0);
    order.reverse();
    Some(order)
}

enum ExactOutcome {
    Optimal(Vec<usize>),
    Infeasible,
    LabelLimit,
}

#[derive(Clone, Copy)]
struct Label {
    cost: f64,
    departure: f64,
    previous: u32,
    next_in_bucket: u32,
    node: u8,
    dominated: bool,
}

/// Held–Karp with hard windows. Each `(visited set, last node)` keeps the
/// Pareto set of (cost, departure time) labels: waiting is allowed, so an
/// earlier departure at no greater cost is never worse.
fn held_karp_time_windows(problem: &TourProblem) -> ExactOutcome {
    let schedule = problem.schedule.as_ref().expect("windows imply a schedule");
    let size = problem.size;
    let stops = size - 1;
    let states = 1_usize << stops;
    let mut heads = vec![u32::MAX; states * stops];
    let mut labels = Vec::<Label>::new();
    let start = schedule.window[0].earliest_s.max(0.0) + schedule.service[0];

    let extend = |labels: &mut Vec<Label>,
                  heads: &mut [u32],
                  bucket: usize,
                  node: usize,
                  cost: f64,
                  time: f64,
                  previous: u32| {
        let window = schedule.window[node];
        if !cost.is_finite() || time > window.latest_s + TIME_EPSILON {
            return;
        }
        let departure = time.max(window.earliest_s) + schedule.service[node];
        let mut cursor = heads[bucket];
        while cursor != u32::MAX {
            let label = labels[cursor as usize];
            if !label.dominated {
                if label.cost <= cost && label.departure <= departure {
                    return;
                }
                if cost <= label.cost && departure <= label.departure {
                    labels[cursor as usize].dominated = true;
                }
            }
            cursor = label.next_in_bucket;
        }
        labels.push(Label {
            cost,
            departure,
            previous,
            next_in_bucket: heads[bucket],
            node: node as u8,
            dominated: false,
        });
        heads[bucket] = (labels.len() - 1) as u32;
    };

    for next in 0..stops {
        extend(
            &mut labels,
            &mut heads,
            (1 << next) * stops + next,
            next + 1,
            problem.cost(0, next + 1),
            start + schedule.time[next + 1],
            u32::MAX,
        );
    }
    for mask in 1..states {
        for last in 0..stops {
            if mask & (1 << last) == 0 {
                continue;
            }
            let mut cursor = heads[mask * stops + last];
            while cursor != u32::MAX {
                let slot = cursor;
                let label = labels[slot as usize];
                cursor = label.next_in_bucket;
                if label.dominated {
                    continue;
                }
                for next in 0..stops {
                    if mask & (1 << next) != 0 {
                        continue;
                    }
                    let from = (last + 1) * size + next + 1;
                    extend(
                        &mut labels,
                        &mut heads,
                        (mask | (1 << next)) * stops + next,
                        next + 1,
                        label.cost + problem.cost[from],
                        label.departure + schedule.time[from],
                        slot,
                    );
                }
                if labels.len() > MAX_EXACT_LABELS {
                    return ExactOutcome::LabelLimit;
                }
            }
        }
    }
    let full = states - 1;
    let mut best: Option<(f64, u32)> = None;
    for last in 0..stops {
        let mut cursor = heads[full * stops + last];
        while cursor != u32::MAX {
            let label = labels[cursor as usize];
            let leg = (last + 1) * size;
            let arrival = label.departure + schedule.time[leg];
            let cost = label.cost + problem.cost[leg];
            if !label.dominated
                && cost.is_finite()
                && arrival <= schedule.return_window.latest_s + TIME_EPSILON
                && best.is_none_or(|(known, _)| cost < known)
            {
                best = Some((cost, cursor));
            }
            cursor = label.next_in_bucket;
        }
    }
    let Some((_, mut cursor)) = best else {
        return ExactOutcome::Infeasible;
    };
    let mut order = Vec::with_capacity(size);
    while cursor != u32::MAX {
        order.push(labels[cursor as usize].node as usize);
        cursor = labels[cursor as usize].previous;
    }
    order.push(0);
    order.reverse();
    ExactOutcome::Optimal(order)
}

// ---------------------------------------------------------------------------
// Local search
// ---------------------------------------------------------------------------

/// Consecutive positions of the current tour, traversed from `from` to `to`
/// (reversed when `from > to`).
#[derive(Debug, Clone, Copy, Default)]
struct Segment {
    from: usize,
    to: usize,
}

/// A neighboring tour as up to five runs of the current one. Runs cover
/// positions `0..=m` once; the first starts at 0 and the last ends at `m`,
/// both forward, so the depot stays at both ends.
#[derive(Debug, Clone, Copy)]
struct Candidate {
    segments: [Segment; 5],
    len: usize,
}

impl Candidate {
    fn new(parts: &[(usize, usize)]) -> Self {
        let mut segments = [Segment::default(); 5];
        for (slot, &(from, to)) in segments.iter_mut().zip(parts) {
            *slot = Segment { from, to };
        }
        Self {
            segments,
            len: parts.len(),
        }
    }

    fn segments(&self) -> &[Segment] {
        &self.segments[..self.len]
    }
}

struct SearchSchedule {
    time: Vec<f64>,
    service: Vec<f64>,
    window: Vec<TimeWindow>,
    return_window: TimeWindow,
    start: f64,
}

/// Search-side copy of a problem. A missing route costs more than any
/// routable tour, so cost comparison counts missing legs first.
struct Instance {
    size: usize,
    cost: Vec<f64>,
    big: f64,
    schedule: Option<SearchSchedule>,
    out_neighbors: Vec<Vec<u32>>,
    in_neighbors: Vec<Vec<u32>>,
}

fn finite_ceiling(values: &[f64], size: usize) -> f64 {
    let largest = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .fold(0.0_f64, |largest, value| largest.max(value.abs()));
    4.0 * (size as f64 + 1.0) * (largest + 1.0)
}

impl Instance {
    fn new(problem: &TourProblem, windows: bool) -> Self {
        let size = problem.size;
        let big = finite_ceiling(&problem.cost, size);
        let cost = problem
            .cost
            .iter()
            .map(|&value| if value.is_finite() { value } else { big })
            .collect::<Vec<_>>();
        let schedule = problem
            .schedule
            .as_ref()
            .filter(|_| windows)
            .map(|schedule| {
                let ceiling =
                    finite_ceiling(&schedule.time, size) + schedule.service.iter().sum::<f64>();
                SearchSchedule {
                    time: schedule
                        .time
                        .iter()
                        .map(|&value| if value.is_finite() { value } else { ceiling })
                        .collect(),
                    service: schedule.service.clone(),
                    window: schedule.window.clone(),
                    return_window: schedule.return_window,
                    start: schedule.window[0].earliest_s.max(0.0) + schedule.service[0],
                }
            });
        let neighbors = |cost_of: &dyn Fn(usize, usize) -> f64| {
            (0..size)
                .map(|node| {
                    let mut others = (0..size).filter(|&other| other != node).collect::<Vec<_>>();
                    let order = |left: &usize, right: &usize| {
                        cost_of(node, *left)
                            .total_cmp(&cost_of(node, *right))
                            .then(left.cmp(right))
                    };
                    let keep = NEIGHBORS.min(others.len());
                    if keep < others.len() {
                        others.select_nth_unstable_by(keep, order);
                        others.truncate(keep);
                    }
                    others.sort_unstable_by(order);
                    others.into_iter().map(|other| other as u32).collect()
                })
                .collect::<Vec<_>>()
        };
        let out_neighbors = neighbors(&|from, to| cost[from * size + to]);
        let in_neighbors = neighbors(&|to, from| cost[from * size + to]);
        Self {
            size,
            cost,
            big,
            schedule,
            out_neighbors,
            in_neighbors,
        }
    }

    fn cost(&self, from: usize, to: usize) -> f64 {
        self.cost[from * self.size + to]
    }

    fn value(&self, cost: f64, lateness_s: f64) -> TourValue {
        TourValue {
            cost,
            lateness_s,
            missing_legs: (cost / self.big).floor() as usize,
        }
    }
}

/// A tour with prefix sums for O(1) segment costs and, with windows, the
/// schedule at every position.
#[derive(Clone)]
struct Tour {
    /// `nodes[0] == nodes[m] == 0`.
    nodes: Vec<usize>,
    position: Vec<usize>,
    /// `forward[i]`: cost of the first `i` arcs.
    forward: Vec<f64>,
    /// `backward[i]`: cost of the first `i` arcs traversed against the tour.
    backward: Vec<f64>,
    departure: Vec<f64>,
    /// Lateness accumulated through each position.
    late: Vec<f64>,
    value: TourValue,
}

impl Tour {
    fn new(instance: &Instance, order: &[usize]) -> Self {
        let mut nodes = order.to_vec();
        nodes.push(0);
        let mut tour = Self {
            nodes,
            position: vec![0; instance.size],
            forward: vec![0.0; instance.size + 1],
            backward: vec![0.0; instance.size + 1],
            departure: vec![0.0; instance.size + 1],
            late: vec![0.0; instance.size + 1],
            value: TourValue::default(),
        };
        tour.rebuild(instance);
        tour
    }

    fn last(&self) -> usize {
        self.nodes.len() - 1
    }

    /// Position of `node`, with the depot at the end of the tour.
    fn end_position(&self, node: usize) -> usize {
        if node == 0 {
            self.last()
        } else {
            self.position[node]
        }
    }

    fn rebuild(&mut self, instance: &Instance) {
        let last = self.last();
        for (position, &node) in self.nodes[..last].iter().enumerate() {
            self.position[node] = position;
        }
        for index in 0..last {
            let (from, to) = (self.nodes[index], self.nodes[index + 1]);
            self.forward[index + 1] = self.forward[index] + instance.cost(from, to);
            self.backward[index + 1] = self.backward[index] + instance.cost(to, from);
        }
        let mut lateness = 0.0;
        if let Some(schedule) = &instance.schedule {
            self.departure[0] = schedule.start;
            for index in 1..=last {
                let (from, node) = (self.nodes[index - 1], self.nodes[index]);
                let arrival =
                    self.departure[index - 1] + schedule.time[from * instance.size + node];
                let (window, service) = if index == last {
                    (schedule.return_window, 0.0)
                } else {
                    (schedule.window[node], schedule.service[node])
                };
                lateness += (arrival - window.latest_s).max(0.0);
                self.late[index] = lateness;
                self.departure[index] = arrival.max(window.earliest_s) + service;
            }
        }
        self.value = instance.value(self.forward[last], lateness);
    }

    fn candidate_cost(&self, instance: &Instance, candidate: &Candidate) -> f64 {
        let mut total = 0.0;
        let mut previous: Option<usize> = None;
        for segment in candidate.segments() {
            total += if segment.from <= segment.to {
                self.forward[segment.to] - self.forward[segment.from]
            } else {
                self.backward[segment.from] - self.backward[segment.to]
            };
            if let Some(previous) = previous {
                total += instance.cost(self.nodes[previous], self.nodes[segment.from]);
            }
            previous = Some(segment.to);
        }
        total
    }

    /// Lateness of `candidate`, or `None` once it exceeds `bound`. The walk
    /// starts after the unchanged prefix and stops once the unchanged tail
    /// is reached no later than in the current tour.
    fn candidate_lateness(
        &self,
        instance: &Instance,
        candidate: &Candidate,
        bound: f64,
    ) -> Option<f64> {
        let Some(schedule) = &instance.schedule else {
            return Some(0.0);
        };
        let last = self.last();
        let head = candidate.segments[0].to;
        let mut previous = self.nodes[head];
        let mut time = self.departure[head];
        let mut lateness = self.late[head];
        let mut position = head + 1;
        let segments = candidate.segments();
        for (index, segment) in segments.iter().enumerate().skip(1) {
            let tail = index + 1 == segments.len();
            let forward = segment.from <= segment.to;
            for step in 0..=segment.from.abs_diff(segment.to) {
                let source = if forward {
                    segment.from + step
                } else {
                    segment.from - step
                };
                let node = self.nodes[source];
                let arrival = time + schedule.time[previous * instance.size + node];
                let (window, service) = if position == last {
                    (schedule.return_window, 0.0)
                } else {
                    (schedule.window[node], schedule.service[node])
                };
                lateness += (arrival - window.latest_s).max(0.0);
                if lateness > bound + TIME_EPSILON {
                    return None;
                }
                time = arrival.max(window.earliest_s) + service;
                if tail {
                    // Departures are monotone in the previous departure, so
                    // an unchanged tail reached no later stays no later.
                    let remaining = self.late[last] - self.late[position];
                    if time == self.departure[position] {
                        return Some(lateness + remaining);
                    }
                    if time < self.departure[position] && remaining <= 0.0 {
                        return Some(lateness);
                    }
                }
                previous = node;
                position += 1;
            }
        }
        Some(lateness)
    }

    /// The candidate's value when it beats `best`.
    fn evaluate(
        &self,
        instance: &Instance,
        candidate: &Candidate,
        best: &TourValue,
    ) -> Option<TourValue> {
        let cost = self.candidate_cost(instance, candidate);
        if instance.schedule.is_none() {
            let value = instance.value(cost, 0.0);
            return value.is_better_than(best).then_some(value);
        }
        let missing_legs = instance.value(cost, 0.0).missing_legs;
        if missing_legs > best.missing_legs
            || (missing_legs == best.missing_legs
                && best.lateness_s <= TIME_EPSILON
                && cost >= best.cost - COST_EPSILON * best.cost.abs().max(1.0))
        {
            return None;
        }
        let bound = if missing_legs < best.missing_legs {
            f64::INFINITY
        } else {
            best.lateness_s
        };
        let lateness = self.candidate_lateness(instance, candidate, bound)?;
        let value = instance.value(cost, lateness);
        value.is_better_than(best).then_some(value)
    }

    fn apply(&mut self, instance: &Instance, candidate: &Candidate, scratch: &mut Vec<usize>) {
        scratch.clear();
        for segment in candidate.segments() {
            if segment.from <= segment.to {
                scratch.extend_from_slice(&self.nodes[segment.from..=segment.to]);
            } else {
                scratch.extend(self.nodes[segment.to..=segment.from].iter().rev());
            }
        }
        debug_assert_eq!(scratch.len(), self.nodes.len());
        std::mem::swap(&mut self.nodes, scratch);
        self.rebuild(instance);
    }

    /// Best improving neighbor among moves anchored at `node`.
    fn best_move(&self, instance: &Instance, node: usize) -> Option<Candidate> {
        let last = self.last();
        let mut best = self.value;
        let mut chosen = None;
        let mut consider = |parts: &[(usize, usize)]| {
            let candidate = Candidate::new(parts);
            if let Some(value) = self.evaluate(instance, &candidate, &best) {
                best = value;
                chosen = Some(candidate);
            }
        };
        let anchor = self.position[node];

        // 2-opt: reverse positions i+1..=j, adding arcs (t_i, t_j) and
        // (t_{i+1}, t_{j+1}); the anchor is the tail of either new arc.
        for &next in &instance.out_neighbors[node] {
            let next = next as usize;
            if next != 0 && self.position[next] >= anchor + 2 {
                let (i, j) = (anchor, self.position[next]);
                consider(&[(0, i), (j, i + 1), (j + 1, last)]);
            }
            if node != 0 && self.end_position(next) >= anchor + 2 {
                let (i, j) = (anchor - 1, self.end_position(next) - 1);
                consider(&[(0, i), (j, i + 1), (j + 1, last)]);
            }
        }

        // Or-opt: move one to three stops starting or ending at the anchor
        // between positions p and p + 1, in either orientation.
        if node != 0 {
            for length in 1..=3_usize {
                for start in [Some(anchor), anchor.checked_sub(length - 1)]
                    .into_iter()
                    .take(if length == 1 { 1 } else { 2 })
                    .flatten()
                {
                    if start == 0 || start + length > last {
                        continue;
                    }
                    let end = start + length - 1;
                    let (first, final_node) = (self.nodes[start], self.nodes[end]);
                    let mut insert = |after: usize, reversed: bool| {
                        if (start - 1..=end).contains(&after) || after >= last {
                            return;
                        }
                        let moved = if reversed { (end, start) } else { (start, end) };
                        if after < start {
                            consider(&[(0, after), moved, (after + 1, start - 1), (end + 1, last)]);
                        } else {
                            consider(&[(0, start - 1), (end + 1, after), moved, (after + 1, last)]);
                        }
                    };
                    for &before in &instance.in_neighbors[first] {
                        insert(self.position[before as usize], false);
                    }
                    for &after in &instance.out_neighbors[final_node] {
                        insert(self.end_position(after as usize) - 1, false);
                    }
                    if length > 1 {
                        for &before in &instance.in_neighbors[final_node] {
                            insert(self.position[before as usize], true);
                        }
                        for &after in &instance.out_neighbors[first] {
                            insert(self.end_position(after as usize) - 1, true);
                        }
                    }
                }
            }
        }

        // Or-3opt: exchange adjacent runs i+1..=j and j+1..=k without
        // reversing either, adding (t_i, t_{j+1}), (t_k, t_{i+1}) and
        // (t_j, t_{k+1}).
        if anchor + 2 < last {
            let first = self.nodes[anchor + 1];
            for &next in &instance.out_neighbors[node] {
                let next = next as usize;
                if next == 0 || self.position[next] < anchor + 2 {
                    continue;
                }
                let j = self.position[next] - 1;
                for &closing in &instance.in_neighbors[first] {
                    let closing = closing as usize;
                    if closing == 0 || self.position[closing] <= j {
                        continue;
                    }
                    let k = self.position[closing];
                    consider(&[(0, anchor), (j + 1, k), (anchor + 1, j), (k + 1, last)]);
                }
            }
        }
        chosen
    }
}

struct WorkQueue {
    queue: std::collections::VecDeque<usize>,
    queued: Vec<bool>,
}

impl WorkQueue {
    fn new(size: usize) -> Self {
        Self {
            queue: std::collections::VecDeque::with_capacity(size),
            queued: vec![false; size],
        }
    }

    fn push(&mut self, node: usize) {
        if !std::mem::replace(&mut self.queued[node], true) {
            self.queue.push_back(node);
        }
    }

    fn pop(&mut self) -> Option<usize> {
        let node = self.queue.pop_front()?;
        self.queued[node] = false;
        Some(node)
    }

    fn push_boundaries(&mut self, tour: &Tour, candidate: &Candidate) {
        for segment in candidate.segments() {
            self.push(tour.nodes[segment.from]);
            self.push(tour.nodes[segment.to]);
        }
    }
}

struct Deadline(Option<Instant>);

impl Deadline {
    fn passed(&self) -> bool {
        self.0.is_some_and(|deadline| Instant::now() >= deadline)
    }
}

fn descend(
    instance: &Instance,
    tour: &mut Tour,
    queue: &mut WorkQueue,
    scratch: &mut Vec<usize>,
    deadline: &Deadline,
) {
    let mut steps = 0_usize;
    while let Some(node) = queue.pop() {
        if let Some(candidate) = tour.best_move(instance, node) {
            queue.push_boundaries(tour, &candidate);
            queue.push(node);
            tour.apply(instance, &candidate, scratch);
        }
        steps += 1;
        if steps.is_multiple_of(256) && deadline.passed() {
            return;
        }
    }
}

fn nearest_neighbor_order(instance: &Instance) -> Vec<usize> {
    let size = instance.size;
    let mut visited = vec![false; size];
    visited[0] = true;
    let mut order = vec![0];
    let mut current = 0;
    for _ in 1..size {
        let next = (1..size)
            .filter(|&node| !visited[node])
            .min_by(|&left, &right| {
                instance
                    .cost(current, left)
                    .total_cmp(&instance.cost(current, right))
            })
            .expect("an unvisited node remains");
        visited[next] = true;
        order.push(next);
        current = next;
    }
    order
}

/// Greedy arc matching: cheapest neighbor arcs first, keeping in- and
/// out-degree at most one and closing no cycle early, then fragments chained
/// by nearest head.
fn greedy_arc_order(instance: &Instance) -> Vec<usize> {
    let size = instance.size;
    let mut arcs = instance
        .out_neighbors
        .iter()
        .enumerate()
        .flat_map(|(from, targets)| targets.iter().map(move |&to| (from, to as usize)))
        .collect::<Vec<_>>();
    arcs.sort_unstable_by(|left, right| {
        instance
            .cost(left.0, left.1)
            .total_cmp(&instance.cost(right.0, right.1))
            .then(left.cmp(right))
    });
    let mut successor = vec![usize::MAX; size];
    let mut predecessor = vec![usize::MAX; size];
    // Endpoints of each fragment point at each other.
    let mut other_end = (0..size).collect::<Vec<_>>();
    for (from, to) in arcs {
        if successor[from] != usize::MAX || predecessor[to] != usize::MAX || other_end[from] == to {
            continue;
        }
        let (head, tail) = (other_end[from], other_end[to]);
        successor[from] = to;
        predecessor[to] = from;
        other_end[head] = tail;
        other_end[tail] = head;
    }
    let mut heads = (0..size)
        .filter(|&node| predecessor[node] == usize::MAX)
        .collect::<Vec<_>>();
    let start = heads.remove(0);
    let mut tail = other_end[start];
    while !heads.is_empty() {
        let index = (0..heads.len())
            .min_by(|&left, &right| {
                instance
                    .cost(tail, heads[left])
                    .total_cmp(&instance.cost(tail, heads[right]))
            })
            .expect("a fragment remains");
        let head = heads.remove(index);
        successor[tail] = head;
        tail = other_end[head];
    }
    successor[tail] = start;
    let mut order = Vec::with_capacity(size);
    let mut node = 0;
    for _ in 0..size {
        order.push(node);
        node = successor[node];
    }
    order
}

fn earliest_deadline_order(instance: &Instance) -> Option<Vec<usize>> {
    let schedule = instance.schedule.as_ref()?;
    let mut order = (1..instance.size).collect::<Vec<_>>();
    order.sort_by(|&left, &right| {
        let (left_window, right_window) = (schedule.window[left], schedule.window[right]);
        left_window
            .latest_s
            .total_cmp(&right_window.latest_s)
            .then(left_window.earliest_s.total_cmp(&right_window.earliest_s))
            .then(left.cmp(&right))
    });
    order.insert(0, 0);
    Some(order)
}

/// SplitMix64: small, seedable and identical on every platform.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `low..=high`.
    fn between(&mut self, low: usize, high: usize) -> usize {
        low + (self.next() % (high - low + 1) as u64) as usize
    }
}

/// A localized double bridge: swap two adjacent runs of at most
/// `MAX_KICK_SEGMENT` stops each.
fn double_bridge(last: usize, random: &mut Random) -> Option<Candidate> {
    let movable = last - 1;
    if movable < 2 {
        return None;
    }
    let longest = MAX_KICK_SEGMENT.min(movable - 1);
    let first = random.between(1, longest);
    let second = random.between(1, longest.min(movable - first));
    let i = random.between(0, movable - first - second);
    let (j, k) = (i + first, i + first + second);
    Some(Candidate::new(&[
        (0, i),
        (j + 1, k),
        (i + 1, j),
        (k + 1, last),
    ]))
}

fn local_search(problem: &TourProblem, options: &SolverOptions, windows: bool) -> TourSolution {
    let started = Instant::now();
    let deadline = Deadline(options.time_budget.map(|budget| started + budget));
    let instance = Instance::new(problem, windows);
    let mut constructions = vec![
        (0..problem.size).collect::<Vec<_>>(),
        nearest_neighbor_order(&instance),
        greedy_arc_order(&instance),
    ];
    constructions.extend(earliest_deadline_order(&instance));
    let mut current = constructions
        .iter()
        .map(|order| Tour::new(&instance, order))
        .reduce(|best, tour| {
            if tour.value.is_better_than(&best.value) {
                tour
            } else {
                best
            }
        })
        .expect("constructions exist");
    let construction_value = problem.evaluate(&current.nodes[..problem.size]);

    let mut scratch = Vec::with_capacity(problem.size + 1);
    let mut queue = WorkQueue::new(problem.size);
    for &node in &current.nodes[..problem.size] {
        queue.push(node);
    }
    descend(&instance, &mut current, &mut queue, &mut scratch, &deadline);
    let mut best = current.clone();
    let mut saved = current.nodes.clone();
    let mut random = Random(options.seed);
    let limit = options
        .max_iterations
        .unwrap_or_else(|| default_iterations(problem.size));
    let mut iterations = 0;
    while iterations < limit && !deadline.passed() {
        let Some(kick) = double_bridge(current.last(), &mut random) else {
            break;
        };
        iterations += 1;
        saved.clone_from(&current.nodes);
        let saved_value = current.value;
        queue.push_boundaries(&current, &kick);
        current.apply(&instance, &kick, &mut scratch);
        descend(&instance, &mut current, &mut queue, &mut scratch, &deadline);
        if saved_value.is_better_than(&current.value) {
            current.nodes.clone_from(&saved);
            current.rebuild(&instance);
        } else if current.value.is_better_than(&best.value) {
            best.clone_from(&current);
        }
    }
    let order = best.nodes[..problem.size].to_vec();
    TourSolution {
        value: problem.evaluate(&order),
        order,
        method: TourMethod::LocalSearch,
        proven_optimal: false,
        proven_infeasible: false,
        iterations,
        input_order_value: TourValue::default(),
        construction_value,
    }
}

// ---------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------

/// Assignment lower bound on every tour (Hungarian algorithm, O(n³)): a tour
/// assigns each node one successor, so it costs at least the cheapest
/// assignment. Missing routes count at the search's prohibitive cost.
pub fn assignment_lower_bound(problem: &TourProblem) -> f64 {
    let size = problem.size;
    if size < 2 {
        return 0.0;
    }
    let big = finite_ceiling(&problem.cost, size);
    let cost = |row: usize, column: usize| {
        let value = problem.cost(row, column);
        if row == column || !value.is_finite() {
            big
        } else {
            value
        }
    };
    // Rows and columns are 1-based; column 0 is the augmenting start.
    let mut row_potential = vec![0.0; size + 1];
    let mut column_potential = vec![0.0; size + 1];
    let mut matched_row = vec![0_usize; size + 1];
    let mut way = vec![0_usize; size + 1];
    let mut slack = vec![0.0; size + 1];
    let mut used = vec![false; size + 1];
    for row in 1..=size {
        matched_row[0] = row;
        let mut column = 0;
        slack.fill(f64::INFINITY);
        used.fill(false);
        loop {
            used[column] = true;
            let current_row = matched_row[column];
            let mut delta = f64::INFINITY;
            let mut next_column = 0;
            for candidate in 1..=size {
                if used[candidate] {
                    continue;
                }
                let reduced = cost(current_row - 1, candidate - 1)
                    - row_potential[current_row]
                    - column_potential[candidate];
                if reduced < slack[candidate] {
                    slack[candidate] = reduced;
                    way[candidate] = column;
                }
                if slack[candidate] < delta {
                    delta = slack[candidate];
                    next_column = candidate;
                }
            }
            for candidate in 0..=size {
                if used[candidate] {
                    row_potential[matched_row[candidate]] += delta;
                    column_potential[candidate] -= delta;
                } else {
                    slack[candidate] -= delta;
                }
            }
            column = next_column;
            if matched_row[column] == 0 {
                break;
            }
        }
        loop {
            let previous = way[column];
            matched_row[column] = matched_row[previous];
            column = previous;
            if column == 0 {
                break;
            }
        }
    }
    (1..=size)
        .map(|column| cost(matched_row[column] - 1, column - 1))
        .sum()
}

#[cfg(test)]
mod tests;
