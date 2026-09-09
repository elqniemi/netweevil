//! Static trace matching with Gaussian emissions and network-distance transitions.
//!
//! Viterbi states include the turn-restriction automaton state, so observations
//! never reset turn history. Transition distances are shortest legal distances,
//! measured in metres, within the requested distance bound. Confidence is an
//! exponential mean model-fit score, not a calibrated probability of correctness.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use anyhow::{Result, ensure};
use netweevil_core::TopologyBundle;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceObservation {
    pub point: LabeledPoint,
    /// Seconds on one common time axis. Supply for every observation or none.
    #[serde(default)]
    pub timestamp_s: Option<f64>,
    /// Standard deviation of the positional error, in metres.
    #[serde(default)]
    pub accuracy_m: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceMatchRequest {
    pub trace_id: String,
    pub observations: Vec<TraceObservation>,
    #[serde(default = "default_trace_snap")]
    pub snap: SnapOptions,
    #[serde(default = "default_match_candidates")]
    pub max_candidates: usize,
    #[serde(default = "default_gps_accuracy")]
    pub gps_accuracy_m: f64,
    #[serde(default = "default_transition_beta")]
    pub transition_beta_m: f64,
    #[serde(default = "default_transition_distance")]
    pub max_transition_distance_m: f64,
    #[serde(default = "default_time_gap")]
    pub max_time_gap_s: f64,
    #[serde(default = "default_max_speed")]
    pub max_speed_mps: f64,
    /// Hard work limit per origin candidate/history transition search.
    /// Exceeding it fails the request, rather than returning a partial optimum.
    #[serde(default = "default_transition_states")]
    pub max_transition_states: usize,
    /// Hard total of newly discovered network states across the entire request.
    #[serde(default = "default_search_states")]
    pub max_search_states: usize,
    #[serde(default, flatten)]
    pub temporal: TemporalRequestOptions,
}

fn default_trace_snap() -> SnapOptions {
    SnapOptions {
        max_distance_m: 50.0,
        ..Default::default()
    }
}
fn default_match_candidates() -> usize {
    8
}
fn default_gps_accuracy() -> f64 {
    10.0
}
fn default_transition_beta() -> f64 {
    50.0
}
fn default_transition_distance() -> f64 {
    20_000.0
}
fn default_time_gap() -> f64 {
    60.0
}
fn default_max_speed() -> f64 {
    70.0
}
fn default_transition_states() -> usize {
    100_000
}
fn default_search_states() -> usize {
    2_000_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchedTracePoint {
    pub observation_index: usize,
    pub matching_index: usize,
    pub snapped: SnappedPoint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceMatching {
    pub observation_indices: Vec<usize>,
    pub edge_path: Vec<u32>,
    pub geometry: Vec<[f64; 3]>,
    pub total_distance_m: f64,
    /// Sum of Gaussian residual and network-distance discrepancy penalties.
    pub score: f64,
    /// exp(-score / (2 * observation_count - 1)). This measures model fit;
    /// it is not a posterior probability and does not measure road ambiguity.
    pub confidence: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceGapReason {
    NoCandidates,
    TimeGap,
    NoLegalTransition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceGap {
    pub before_observation: usize,
    pub reason: TraceGapReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceMatchResult {
    pub trace_id: String,
    /// Input-aligned results. Unsnappable observations and isolated singletons
    /// are null; single nearest points are not presented as matched traces.
    pub tracepoints: Vec<Option<MatchedTracePoint>>,
    pub matchings: Vec<TraceMatching>,
    pub gaps: Vec<TraceGap>,
    pub confidence_method: String,
}

struct ViterbiState {
    candidate: usize,
    automaton: usize,
    score: f64,
    previous: Option<usize>,
    path: Vec<usize>,
    distance: f64,
}
struct Layer {
    observation: usize,
    candidates: Vec<SnappedPoint>,
    states: Vec<ViterbiState>,
}

pub(crate) fn execute_trace_match_with_graph(
    topology: &TopologyBundle,
    graph: &RoutingGraph,
    request: &TraceMatchRequest,
) -> Result<TraceMatchResult> {
    validate_request(request)?;
    let mut result = TraceMatchResult {
        trace_id: request.trace_id.clone(),
        tracepoints: vec![None; request.observations.len()],
        matchings: Vec::new(),
        gaps: Vec::new(),
        confidence_method: "exp_negative_mean_model_penalty_not_probability".into(),
    };
    let mut budget = SearchBudget {
        per_transition: request.max_transition_states,
        total_limit: request.max_search_states,
        used: 0,
    };
    let mut layers = Vec::<Layer>::new();
    for (index, observation) in request.observations.iter().enumerate() {
        let candidates = edge_candidates(
            topology,
            graph,
            &observation.point,
            &request.snap,
            request.max_candidates,
        )?;
        if candidates.is_empty() {
            finish_matching(topology, &mut layers, &mut result);
            result.gaps.push(TraceGap {
                before_observation: index,
                reason: TraceGapReason::NoCandidates,
            });
            continue;
        }
        let sigma = observation.accuracy_m.unwrap_or(request.gps_accuracy_m);
        ensure!(
            candidates
                .iter()
                .all(|candidate| emission(candidate, sigma).is_finite()),
            "trace emission score overflowed; increase gps_accuracy_m or observation accuracy_m"
        );
        if let Some(previous) = layers.last() {
            let previous_observation = &request.observations[previous.observation];
            if let (Some(before), Some(after)) =
                (previous_observation.timestamp_s, observation.timestamp_s)
                && after - before > request.max_time_gap_s
            {
                finish_matching(topology, &mut layers, &mut result);
                result.gaps.push(TraceGap {
                    before_observation: index,
                    reason: TraceGapReason::TimeGap,
                });
            }
        }
        if layers.is_empty() {
            layers.push(initial_layer(
                graph,
                index,
                candidates,
                observation.accuracy_m.unwrap_or(request.gps_accuracy_m),
            ));
            continue;
        }
        let previous = layers.last().unwrap();
        let previous_observation = &request.observations[previous.observation];
        let observed_distance = haversine_meters(
            previous_observation.point.lon,
            previous_observation.point.lat,
            observation.point.lon,
            observation.point.lat,
        );
        let bound = match (previous_observation.timestamp_s, observation.timestamp_s) {
            (Some(before), Some(after)) => request
                .max_transition_distance_m
                .min((after - before) * request.max_speed_mps),
            _ => request.max_transition_distance_m,
        };
        let sigma = observation.accuracy_m.unwrap_or(request.gps_accuracy_m);
        let mut states = Vec::<ViterbiState>::new();
        let mut state_slots = FxHashMap::<(usize, usize), usize>::default();
        for (prior_index, prior) in previous.states.iter().enumerate() {
            let transitions = network_transitions(
                topology,
                graph,
                &previous.candidates[prior.candidate],
                prior.automaton,
                &candidates,
                bound,
                &mut budget,
            )?;
            for transition in transitions {
                let score = prior.score
                    + emission(&candidates[transition.candidate], sigma)
                    + (transition.distance - observed_distance).abs() / request.transition_beta_m;
                ensure!(
                    score.is_finite(),
                    "trace model score overflowed; increase transition_beta_m or positional accuracy"
                );
                let key = (transition.candidate, transition.automaton);
                let state = ViterbiState {
                    candidate: transition.candidate,
                    automaton: transition.automaton,
                    score,
                    previous: Some(prior_index),
                    path: transition.path,
                    distance: transition.distance,
                };
                if let Some(&slot) = state_slots.get(&key) {
                    if states[slot].score > score {
                        states[slot] = state;
                    }
                } else {
                    state_slots.insert(key, states.len());
                    states.push(state);
                }
            }
        }
        if states.is_empty() {
            finish_matching(topology, &mut layers, &mut result);
            result.gaps.push(TraceGap {
                before_observation: index,
                reason: TraceGapReason::NoLegalTransition,
            });
            layers.push(initial_layer(graph, index, candidates, sigma));
        } else {
            layers.push(Layer {
                observation: index,
                candidates,
                states,
            });
        }
    }
    finish_matching(topology, &mut layers, &mut result);
    Ok(result)
}

fn validate_request(request: &TraceMatchRequest) -> Result<()> {
    ensure!(
        (2..=512).contains(&request.observations.len()),
        "trace matching requires between 2 and 512 observations"
    );
    ensure!(
        (1..=8).contains(&request.max_candidates),
        "max_candidates must be between 1 and 8"
    );
    ensure!(
        (1..=2_000_000).contains(&request.max_transition_states),
        "max_transition_states must be between 1 and 2000000"
    );
    ensure!(
        (1..=20_000_000).contains(&request.max_search_states),
        "max_search_states must be between 1 and 20000000"
    );
    for (name, value) in [
        ("gps_accuracy_m", request.gps_accuracy_m),
        ("transition_beta_m", request.transition_beta_m),
        (
            "max_transition_distance_m",
            request.max_transition_distance_m,
        ),
        ("max_time_gap_s", request.max_time_gap_s),
        ("max_speed_mps", request.max_speed_mps),
    ] {
        ensure!(
            value.is_finite() && value > 0.0,
            "{name} must be finite and positive"
        );
    }
    ensure!(
        !request.temporal.requires_exact_labels() && request.temporal.holiday_calendar.is_none(),
        "trace matching supports static road costs only; timestamps constrain trace continuity and do not enable time-dependent routing"
    );
    let timestamped = request.observations[0].timestamp_s.is_some();
    let mut previous = None;
    for observation in &request.observations {
        ensure!(
            observation.timestamp_s.is_some() == timestamped,
            "timestamps must be supplied for every observation or none"
        );
        if let Some(time) = observation.timestamp_s {
            ensure!(
                time.is_finite() && previous.is_none_or(|prior| time > prior),
                "trace timestamps must be finite and strictly increasing"
            );
            previous = Some(time);
        }
        ensure!(
            observation
                .accuracy_m
                .is_none_or(|sigma| sigma.is_finite() && sigma > 0.0),
            "observation accuracy_m must be finite and positive"
        );
    }
    Ok(())
}

fn emission(point: &SnappedPoint, sigma: f64) -> f64 {
    0.5 * (point.snap_distance_m / sigma).powi(2)
}

fn initial_layer(
    graph: &RoutingGraph,
    observation: usize,
    candidates: Vec<SnappedPoint>,
    sigma: f64,
) -> Layer {
    let states = candidates
        .iter()
        .enumerate()
        .map(|(candidate, point)| ViterbiState {
            candidate,
            automaton: graph
                .automaton
                .transition(0, point.snapped_edge_id.unwrap() as usize),
            score: emission(point, sigma),
            previous: None,
            path: Vec::new(),
            distance: 0.0,
        })
        .collect();
    Layer {
        observation,
        candidates,
        states,
    }
}

fn edge_candidates(
    topology: &TopologyBundle,
    graph: &RoutingGraph,
    point: &LabeledPoint,
    options: &SnapOptions,
    limit: usize,
) -> Result<Vec<SnappedPoint>> {
    let mut candidates = Vec::new();
    for is_origin in [true, false] {
        match snap_candidates_with_options(topology, graph, point, options, is_origin) {
            Ok(snaps) => {
                for snapped in snaps {
                    if snapped.snapped_edge_id.is_some() {
                        candidates.push(snapped);
                        continue;
                    }
                    for &edge_id in graph
                        .incoming_edges(snapped.snapped_node_id as usize)
                        .iter()
                        .chain(graph.outgoing_edges(snapped.snapped_node_id as usize))
                    {
                        let edge = topology.routing_edge(edge_id as usize);
                        if !options.attribute_filters.iter().all(|(name, value)| {
                            topology.edge_attribute_matches(edge_id as usize, name, value)
                        }) {
                            continue;
                        }
                        let mut candidate = snapped.clone();
                        candidate.snapped_edge_id = Some(edge_id);
                        candidate.snapped_edge_fraction =
                            Some(if edge.from.0 == snapped.snapped_node_id {
                                0.0
                            } else {
                                1.0
                            });
                        candidate.snapped_from_node_id = Some(edge.from.0);
                        candidate.snapped_to_node_id = Some(edge.to.0);
                        candidate.component_id = topology.edge_component_id(edge_id);
                        candidates.push(candidate);
                    }
                }
            }
            Err(error) if analysis_failure(&error).is_some() => {}
            Err(error) => return Err(error),
        }
    }
    candidates.sort_by(|left, right| {
        left.snap_distance_m
            .total_cmp(&right.snap_distance_m)
            .then_with(|| snap_cache_key(left).cmp(&snap_cache_key(right)))
    });
    candidates.dedup_by_key(|point| snap_cache_key(point));
    candidates.truncate(limit);
    Ok(candidates)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct NetworkKey {
    edge: usize,
    automaton: usize,
    initial: bool,
}
struct NetworkLabel {
    distance: f64,
    previous: Option<NetworkKey>,
}
#[derive(Clone, Copy)]
struct NetworkQueue {
    key: NetworkKey,
    distance: f64,
}
impl PartialEq for NetworkQueue {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.distance.to_bits() == other.distance.to_bits()
    }
}
impl Eq for NetworkQueue {}
impl PartialOrd for NetworkQueue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for NetworkQueue {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .distance
            .total_cmp(&self.distance)
            .then_with(|| other.key.cmp(&self.key))
    }
}
struct NetworkTransition {
    candidate: usize,
    automaton: usize,
    path: Vec<usize>,
    distance: f64,
}

struct SearchBudget {
    per_transition: usize,
    total_limit: usize,
    used: usize,
}

impl SearchBudget {
    fn discover(&mut self) -> Result<()> {
        ensure!(
            self.used < self.total_limit,
            "trace matching exceeded max_search_states={}; increase the explicit limit or reduce the trace length or transition distance",
            self.total_limit
        );
        self.used += 1;
        Ok(())
    }
}

fn network_transitions(
    topology: &TopologyBundle,
    graph: &RoutingGraph,
    origin: &SnappedPoint,
    automaton: usize,
    destinations: &[SnappedPoint],
    bound: f64,
    budget: &mut SearchBudget,
) -> Result<Vec<NetworkTransition>> {
    let origin_edge = origin.snapped_edge_id.unwrap() as usize;
    let origin_fraction = origin.snapped_edge_fraction.unwrap();
    let initial = NetworkKey {
        edge: origin_edge,
        automaton,
        initial: true,
    };
    let distance = f64::from(topology.routing_edge(origin_edge).length_m) * (1.0 - origin_fraction);
    let mut labels = FxHashMap::default();
    budget.discover()?;
    labels.insert(
        initial,
        NetworkLabel {
            distance,
            previous: None,
        },
    );
    let mut heap = BinaryHeap::from([NetworkQueue {
        key: initial,
        distance,
    }]);
    while let Some(NetworkQueue { key, distance }) = heap.pop() {
        if distance > bound {
            break;
        }
        if distance > labels[&key].distance {
            continue;
        }
        for slot in graph.transition_range(key.edge) {
            let edge = graph.transition_edges[slot] as usize;
            if !graph.automaton.is_transition_allowed(key.automaton, edge) {
                continue;
            }
            let next_key = NetworkKey {
                edge,
                automaton: graph.automaton.transition(key.automaton, edge),
                initial: false,
            };
            let next_distance = distance + f64::from(topology.routing_edge(edge).length_m);
            if labels
                .get(&next_key)
                .is_some_and(|label| label.distance <= next_distance)
            {
                continue;
            }
            if !labels.contains_key(&next_key) {
                ensure!(
                    labels.len() < budget.per_transition,
                    "trace transition search exceeded max_transition_states={}; increase the explicit limit or reduce max_transition_distance_m",
                    budget.per_transition
                );
                budget.discover()?;
            }
            labels.insert(
                next_key,
                NetworkLabel {
                    distance: next_distance,
                    previous: Some(key),
                },
            );
            // Even an edge whose end is beyond the bound can contain a valid
            // destination phantom. Record it, but do not expand it past the bound.
            if next_distance <= bound {
                heap.push(NetworkQueue {
                    key: next_key,
                    distance: next_distance,
                });
            }
        }
    }
    let mut best = FxHashMap::<(usize, usize), (f64, NetworkKey)>::default();
    for (&key, label) in &labels {
        for (candidate, destination) in destinations.iter().enumerate() {
            if destination.snapped_edge_id != Some(key.edge as u32) {
                continue;
            }
            let fraction = destination.snapped_edge_fraction.unwrap();
            if key.initial && fraction < origin_fraction {
                continue;
            }
            let distance = label.distance
                - f64::from(topology.routing_edge(key.edge).length_m) * (1.0 - fraction);
            if distance < -1e-9 || distance > bound {
                continue;
            }
            let entry = best
                .entry((candidate, key.automaton))
                .or_insert((f64::INFINITY, key));
            if distance < entry.0 {
                *entry = (distance.max(0.0), key);
            }
        }
    }
    let mut transitions = Vec::new();
    for ((candidate, automaton), (distance, mut key)) in best {
        let mut path = Vec::new();
        loop {
            path.push(key.edge);
            let Some(previous) = labels[&key].previous else {
                break;
            };
            key = previous;
        }
        path.reverse();
        transitions.push(NetworkTransition {
            candidate,
            automaton,
            path,
            distance,
        });
    }
    transitions.sort_by_key(|transition| (transition.candidate, transition.automaton));
    Ok(transitions)
}

fn finish_matching(
    topology: &TopologyBundle,
    layers: &mut Vec<Layer>,
    result: &mut TraceMatchResult,
) {
    if layers.len() < 2 {
        layers.clear();
        return;
    }
    let last = layers.last().unwrap();
    let mut selected = last
        .states
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| left.score.total_cmp(&right.score))
        .unwrap()
        .0;
    let score = last.states[selected].score;
    let mut choices = vec![0; layers.len()];
    for index in (0..layers.len()).rev() {
        choices[index] = selected;
        if let Some(previous) = layers[index].states[selected].previous {
            selected = previous;
        }
    }
    let mut path = Vec::new();
    let mut distance = 0.0;
    let matching_index = result.matchings.len();
    for (index, layer) in layers.iter().enumerate() {
        let state = &layer.states[choices[index]];
        let snapped = layer.candidates[state.candidate].clone();
        result.tracepoints[layer.observation] = Some(MatchedTracePoint {
            observation_index: layer.observation,
            matching_index,
            snapped,
        });
        if index > 0 {
            let skip = usize::from(path.last() == state.path.first());
            path.extend_from_slice(&state.path[skip..]);
            distance += state.distance;
        }
    }
    let origin = &result.tracepoints[layers[0].observation]
        .as_ref()
        .unwrap()
        .snapped;
    let destination = &result.tracepoints[last.observation]
        .as_ref()
        .unwrap()
        .snapped;
    let geometry = build_route_geometry(topology, &path, origin, destination);
    result.matchings.push(TraceMatching {
        observation_indices: layers.iter().map(|layer| layer.observation).collect(),
        edge_path: path.iter().map(|&edge| edge as u32).collect(),
        geometry,
        total_distance_m: distance,
        score,
        confidence: (-score / (2 * layers.len() - 1) as f64).exp(),
    });
    layers.clear();
}
