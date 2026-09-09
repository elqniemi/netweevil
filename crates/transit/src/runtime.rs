use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use anyhow::Result;

use crate::backward::BackwardIndex;
use crate::legs::{haversine_m, seconds_for_distance};
use crate::model::{
    AccessMode, TransitBundle, TransitConnection, TransitModeOptions, TransitStop,
    TransitStreetPath,
};

pub(crate) struct DepartureIndex {
    pub(crate) by_stop: Vec<Vec<u32>>,
    pub(crate) next_connection: Vec<u32>,
}

/// Links adjacent connections on each run in timetable order. Position matters
/// when a vehicle visits the same stop more than once.
pub(crate) fn build_run_links(bundle: &TransitBundle) -> (Vec<u32>, Vec<u32>) {
    let mut previous = vec![u32::MAX; bundle.connections.len()];
    let mut next = vec![u32::MAX; bundle.connections.len()];
    let run_count = bundle
        .connections
        .iter()
        .map(|connection| connection.run_index as usize + 1)
        .max()
        .unwrap_or_default();
    let mut last_by_run = vec![u32::MAX; run_count];
    let mut indexes = (0..bundle.connections.len() as u32).collect::<Vec<_>>();
    if !bundle
        .connections
        .windows(2)
        .all(|pair| pair[0].departure_s <= pair[1].departure_s)
    {
        indexes.sort_by_key(|index| bundle.connections[*index as usize].departure_s);
    }
    for index in indexes {
        let connection = &bundle.connections[index as usize];
        let last_index = &mut last_by_run[connection.run_index as usize];
        if let Some(last) = bundle.connections.get(*last_index as usize)
            && last.to_stop_index == connection.from_stop_index
            && last.arrival_s <= connection.departure_s
        {
            previous[index as usize] = *last_index;
            next[*last_index as usize] = index;
        }
        *last_index = index;
    }
    (previous, next)
}

pub(crate) fn build_departures_by_stop(bundle: &TransitBundle) -> DepartureIndex {
    let mut departures_by_stop: Vec<Vec<u32>> = vec![Vec::new(); bundle.stops.len()];
    let mut unsorted_stops = Vec::<usize>::new();
    for (connection_index, connection) in bundle.connections.iter().enumerate() {
        let stop_index = connection.from_stop_index as usize;
        let departures = &mut departures_by_stop[stop_index];
        if departures.last().is_some_and(|previous| {
            bundle.connections[*previous as usize].departure_s > connection.departure_s
        }) {
            unsorted_stops.push(stop_index);
        }
        departures.push(connection_index as u32);
    }
    unsorted_stops.sort_unstable();
    unsorted_stops.dedup();
    for stop_index in unsorted_stops {
        departures_by_stop[stop_index]
            .sort_by_key(|index| bundle.connections[*index as usize].departure_s);
    }
    DepartureIndex {
        by_stop: departures_by_stop,
        next_connection: build_run_links(bundle).1,
    }
}

#[derive(Debug)]
pub(crate) struct StopSpatialIndex {
    cell_degrees: f64,
    cells: HashMap<(i32, i32), Vec<u32>>,
}

impl StopSpatialIndex {
    pub(crate) fn new(stops: &[TransitStop]) -> Self {
        let cell_degrees = 0.01_f64;
        let mut cells = HashMap::<(i32, i32), Vec<u32>>::new();
        for (stop_index, stop) in stops.iter().enumerate() {
            cells
                .entry(Self::cell_key(stop.lon, stop.lat, cell_degrees))
                .or_default()
                .push(stop_index as u32);
        }
        Self {
            cell_degrees,
            cells,
        }
    }

    pub(crate) fn nearby_stops(
        &self,
        bundle: &TransitBundle,
        lon: f64,
        lat: f64,
        max_distance_m: f64,
    ) -> Vec<StopCandidate> {
        if !max_distance_m.is_finite()
            || max_distance_m <= 0.0
            || !lon.is_finite()
            || !lat.is_finite()
        {
            return Vec::new();
        }
        // Bound the same sphere used by the exact haversine filter. The
        // longitude arc widens near a pole; a wrapped box scans occupied
        // latitude cells and lets the exact distance decide membership.
        let angular_radius = (max_distance_m / 6_371_000.0).min(std::f64::consts::PI);
        let lat_radius = angular_radius.to_degrees();
        let lon_radius = if lat.abs() + lat_radius >= 90.0 {
            180.0
        } else {
            (angular_radius.sin() / lat.to_radians().cos())
                .clamp(-1.0, 1.0)
                .asin()
                .to_degrees()
        };
        let wraps_longitude = lon - lon_radius <= -180.0 || lon + lon_radius >= 180.0;
        let min_col = ((lon - lon_radius) / self.cell_degrees).floor() as i32;
        let max_col = ((lon + lon_radius) / self.cell_degrees).floor() as i32;
        let min_row = (((lat - lat_radius).max(-90.0)) / self.cell_degrees).floor() as i32;
        let max_row = (((lat + lat_radius).min(90.0)) / self.cell_degrees).floor() as i32;
        let mut candidates = Vec::new();
        let cell_count = (i64::from(max_col) - i64::from(min_col) + 1)
            .saturating_mul(i64::from(max_row) - i64::from(min_row) + 1);
        let mut visit = |stop_indexes: &[u32]| {
            for &stop_index in stop_indexes {
                let stop = &bundle.stops[stop_index as usize];
                let distance_m = haversine_m(lon, lat, stop.lon, stop.lat);
                if distance_m <= max_distance_m {
                    candidates.push(StopCandidate {
                        stop_index,
                        distance_m,
                        transfer_time_s: None,
                        network_path: None,
                    });
                }
            }
        };
        if wraps_longitude || cell_count > self.cells.len() as i64 {
            for (&(col, row), stops) in &self.cells {
                if (wraps_longitude || (col >= min_col && col <= max_col))
                    && row >= min_row
                    && row <= max_row
                {
                    visit(stops);
                }
            }
        } else {
            for col in min_col..=max_col {
                for row in min_row..=max_row {
                    if let Some(stops) = self.cells.get(&(col, row)) {
                        visit(stops);
                    }
                }
            }
        }
        candidates
    }

    fn cell_key(lon: f64, lat: f64, cell_degrees: f64) -> (i32, i32) {
        (
            (lon / cell_degrees).floor() as i32,
            (lat / cell_degrees).floor() as i32,
        )
    }
}

/// Every stop within the requested walking radius, sorted by distance. A
/// nearest-neighbour cap can hide the only usable platform in a busy station.
pub(crate) fn build_transfer_candidates(
    bundle: &TransitBundle,
    stop_index: &StopSpatialIndex,
    max_distance_m: f64,
) -> Vec<Vec<StopCandidate>> {
    use rayon::prelude::*;

    bundle
        .stops
        .par_iter()
        .map(|stop| {
            let mut candidates =
                stop_index.nearby_stops(bundle, stop.lon, stop.lat, max_distance_m);
            candidates.sort_by(|left, right| {
                left.distance_m
                    .total_cmp(&right.distance_m)
                    .then_with(|| left.stop_index.cmp(&right.stop_index))
            });
            candidates
        })
        .collect()
}

/// Network street-time oracle for access and egress legs. Hosts with street
/// routing engines loaded (the API, the CLI) inject an implementation so the
/// transit crate can price legs with real network times while staying
/// decoupled from the street router.
pub trait StreetTimeEstimator: Send + Sync {
    /// Network travel time in seconds between two points for a street mode,
    /// or `None` when no engine covers the mode or the pair is unreachable.
    /// In network street-access mode, `None` makes the candidate unreachable;
    /// straight-line pricing is used only when explicitly requested.
    fn street_time_s(
        &self,
        mode: AccessMode,
        egress: bool,
        from_lon: f64,
        from_lat: f64,
        to_lon: f64,
        to_lat: f64,
    ) -> Option<u32>;

    /// Rich path from an arbitrary origin point to a transit stop. Binding-
    /// aware hosts override this method. The caller separately consults
    /// `street_time_s` when this richer method returns `None` for an unbound
    /// stop. This supports coordinate-only estimators for unbound stops. A
    /// bound stop never falls back to coordinates because that would bypass
    /// its platform/node feasibility constraint.
    fn point_to_stop_path(
        &self,
        _mode: AccessMode,
        _from_lon: f64,
        _from_lat: f64,
        _stop: &TransitStop,
    ) -> Option<TransitStreetPath> {
        None
    }

    /// Rich path from a bound transit stop to an arbitrary destination point.
    fn stop_to_point_path(
        &self,
        _mode: AccessMode,
        _stop: &TransitStop,
        _to_lon: f64,
        _to_lat: f64,
    ) -> Option<TransitStreetPath> {
        None
    }

    /// Directed path between two transit stops. This is the extension point
    /// used by transfer-table precomputation. Both stops carry any explicit
    /// node/edge/3D-coordinate bindings loaded for the feed. The default
    /// coordinate-only implementation is available only when both stops are
    /// unbound; binding-aware hosts must override it.
    fn stop_to_stop_path(
        &self,
        mode: AccessMode,
        from: &TransitStop,
        to: &TransitStop,
    ) -> Option<TransitStreetPath> {
        if from.binding.is_some() || to.binding.is_some() {
            return None;
        }
        self.street_time_s(mode, false, from.lon, from.lat, to.lon, to.lat)
            .map(TransitStreetPath::time_only)
    }
}

pub(crate) struct TransitRuntime<'a> {
    pub(crate) bundle: &'a TransitBundle,
    pub(crate) departures_by_stop: &'a DepartureIndex,
    stop_index: &'a StopSpatialIndex,
    transfer_candidates: &'a [Vec<StopCandidate>],
    pub(crate) allowed_routes: Vec<bool>,
    pub(crate) street_estimator: Option<&'a dyn StreetTimeEstimator>,
    /// Reverse timetable and transfer indexes for arrive-by searches. Hosts
    /// that serve many requests from one prepared router share a cached index;
    /// otherwise the arrive-by scan builds its own.
    pub(crate) backward_index: Option<&'a BackwardIndex>,
}

impl<'a> TransitRuntime<'a> {
    pub(crate) fn new(
        bundle: &'a TransitBundle,
        departures_by_stop: &'a DepartureIndex,
        stop_index: &'a StopSpatialIndex,
        transfer_candidates: &'a [Vec<StopCandidate>],
        modes: &TransitModeOptions,
        street_estimator: Option<&'a dyn StreetTimeEstimator>,
    ) -> Self {
        let allowed_routes = bundle
            .routes
            .iter()
            .map(|route| modes.transit.contains(&route.mode))
            .collect();
        Self {
            bundle,
            departures_by_stop,
            stop_index,
            transfer_candidates,
            allowed_routes,
            street_estimator,
            backward_index: None,
        }
    }

    pub(crate) fn with_backward_index(mut self, index: &'a BackwardIndex) -> Self {
        self.backward_index = Some(index);
        self
    }

    /// Resolves explicit instants or unambiguous agency-local wall times.
    pub(crate) fn request_time_seconds(&self, raw: &str) -> Result<u32> {
        crate::timetable_time::request_time_seconds(self.bundle, raw)
    }

    pub(crate) fn nearby_access_stops(
        &self,
        lon: f64,
        lat: f64,
        max_distance_m: f64,
    ) -> Vec<StopCandidate> {
        let mut candidates = self
            .stop_index
            .nearby_stops(self.bundle, lon, lat, max_distance_m);
        candidates.sort_by(|left, right| left.distance_m.total_cmp(&right.distance_m));
        candidates
    }

    /// Precomputed nearest stops for transfer expansion, sorted by distance.
    /// Callers filter with `take_while(distance_m <= max)` to apply the
    /// request's transfer distance limit.
    pub(crate) fn transfer_candidates(&self, stop_index: u32) -> &[StopCandidate] {
        &self.transfer_candidates[stop_index as usize]
    }

    /// Whole transfer table, used to build and read the reverse adjacency an
    /// arrive-by scan walks.
    pub(crate) fn all_transfer_candidates(&self) -> &'a [Vec<StopCandidate>] {
        self.transfer_candidates
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StopCandidate {
    pub(crate) stop_index: u32,
    pub(crate) distance_m: f64,
    /// `Some` for a precomputed network transfer; `None` means derive a
    /// straight-line walking time from `distance_m` at request time.
    pub(crate) transfer_time_s: Option<u32>,
    pub(crate) network_path: Option<TransitStreetPath>,
}

/// Best street-mode connection between a point and a stop, picked across the
/// requested access or egress modes.
#[derive(Debug, Clone)]
pub(crate) struct StreetCandidate {
    pub(crate) stop_index: u32,
    pub(crate) time_s: u32,
    pub(crate) mode: AccessMode,
    pub(crate) network_path: Option<TransitStreetPath>,
}

pub(crate) fn best_street_candidates(
    runtime: &TransitRuntime<'_>,
    lon: f64,
    lat: f64,
    street_modes: &[AccessMode],
    options: &TransitModeOptions,
    egress: bool,
) -> Vec<StreetCandidate> {
    let use_network = matches!(
        options.street_access,
        crate::model::TransitStreetAccessModel::Network
    );
    let network_estimator = use_network.then_some(runtime.street_estimator).flatten();
    let mut best = HashMap::<u32, StreetCandidate>::new();
    for &mode in street_modes {
        let max_distance_m = if egress {
            mode.max_egress_distance_m(options)
        } else {
            mode.max_access_distance_m(options)
        };
        for candidate in runtime.nearby_access_stops(lon, lat, max_distance_m) {
            // Candidates are pre-filtered by straight-line distance (a lower
            // bound on network distance). In network mode, absence of a
            // routed path/time means the pair is unreachable; it must not
            // silently turn into a geometric teleport.
            let (time_s, network_path) = if use_network {
                let Some(estimator) = network_estimator else {
                    continue;
                };
                let stop = &runtime.bundle.stops[candidate.stop_index as usize];
                let network_path = if egress {
                    estimator.stop_to_point_path(mode, stop, lon, lat)
                } else {
                    estimator.point_to_stop_path(mode, lon, lat, stop)
                };
                let network_time_s = network_path
                    .as_ref()
                    .map(|path| path.travel_time_s)
                    .or_else(|| {
                        stop.binding.is_none().then(|| {
                            if egress {
                                estimator.street_time_s(mode, true, stop.lon, stop.lat, lon, lat)
                            } else {
                                estimator.street_time_s(mode, false, lon, lat, stop.lon, stop.lat)
                            }
                        })?
                    });
                let Some(network_time_s) = network_time_s else {
                    continue;
                };
                (network_time_s, network_path)
            } else {
                (
                    seconds_for_distance(candidate.distance_m, mode.speed_kph(options)),
                    None,
                )
            };
            let entry = StreetCandidate {
                stop_index: candidate.stop_index,
                time_s,
                mode,
                network_path,
            };
            best.entry(candidate.stop_index)
                .and_modify(|known| {
                    if time_s < known.time_s {
                        *known = entry.clone();
                    }
                })
                .or_insert(entry);
        }
    }
    let mut candidates = best.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.time_s
            .cmp(&right.time_s)
            .then_with(|| left.stop_index.cmp(&right.stop_index))
    });
    candidates
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct StateKey {
    pub(crate) stop_index: u32,
    pub(crate) boardings: u8,
    pub(crate) connection_index: u32,
    pub(crate) can_alight: bool,
}

/// Staying aboard has no slack. Changing vehicles at the same stop needs
/// both transfer and boarding slack; a transfer walk already paid its slack.
pub(crate) fn boarding_slack_s(
    state: StateKey,
    continuing_run: bool,
    modes: &TransitModeOptions,
) -> u32 {
    if continuing_run {
        0
    } else if state.boardings > 0 && state.can_alight {
        modes.board_slack_s.saturating_add(modes.transfer_slack_s)
    } else {
        modes.board_slack_s
    }
}

pub(crate) fn can_start_transfer_walk(state: StateKey) -> bool {
    state.boardings > 0 && state.can_alight
}

pub(crate) fn can_finish_with_egress(state: StateKey) -> bool {
    state.boardings > 0 && state.can_alight
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct QueueEntry {
    pub(crate) time_s: u32,
    pub(crate) state: StateKey,
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .time_s
            .cmp(&self.time_s)
            .then_with(|| self.state.stop_index.cmp(&other.state.stop_index))
            .then_with(|| self.state.boardings.cmp(&other.state.boardings))
            .then_with(|| {
                self.state
                    .connection_index
                    .cmp(&other.state.connection_index)
            })
            .then_with(|| self.state.can_alight.cmp(&other.state.can_alight))
    }
}

impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone)]
pub(crate) enum PrevStep {
    Access {
        from_id: String,
        from_name: String,
        from_lon: f64,
        from_lat: f64,
        departure_s: u32,
        /// Access leg travel time as priced by the search (straight-line or
        /// network); leg reconstruction must reuse it, not re-derive it.
        time_s: u32,
        mode: AccessMode,
        network_path: Option<TransitStreetPath>,
    },
    Transfer {
        previous: StateKey,
        departure_s: u32,
        travel_time_s: u32,
        network_path: Option<TransitStreetPath>,
    },
    Transit {
        previous: StateKey,
        connection: TransitConnection,
    },
}

pub(crate) fn relax_state(
    heap: &mut BinaryHeap<QueueEntry>,
    best: &mut HashMap<StateKey, u32>,
    prev: &mut HashMap<StateKey, PrevStep>,
    state: StateKey,
    time_s: u32,
    previous: PrevStep,
) {
    if best.get(&state).is_none_or(|known| time_s < *known) {
        best.insert(state, time_s);
        prev.insert(state, previous);
        heap.push(QueueEntry { time_s, state });
    }
}
