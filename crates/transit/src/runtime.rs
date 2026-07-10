use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use anyhow::{Context, Result, anyhow};
use time::{OffsetDateTime, PrimitiveDateTime, Time, format_description::well_known::Rfc3339};

use crate::gtfs::parse_iso_date;
use crate::legs::{haversine_m, seconds_for_distance};
use crate::model::{
    AccessMode, TransitBundle, TransitConnection, TransitModeOptions, TransitStop,
    TransitStreetPath,
};

pub(crate) fn build_departures_by_stop(bundle: &TransitBundle) -> Vec<Vec<TransitConnection>> {
    let mut departures_by_stop: Vec<Vec<TransitConnection>> = vec![Vec::new(); bundle.stops.len()];
    let mut unsorted_stops = Vec::<usize>::new();
    for connection in &bundle.connections {
        let stop_index = connection.from_stop_index as usize;
        let departures = &mut departures_by_stop[stop_index];
        if departures
            .last()
            .is_some_and(|previous| previous.departure_s > connection.departure_s)
        {
            unsorted_stops.push(stop_index);
        }
        departures.push(*connection);
    }
    unsorted_stops.sort_unstable();
    unsorted_stops.dedup();
    for stop_index in unsorted_stops {
        departures_by_stop[stop_index].sort_by_key(|connection| connection.departure_s);
    }
    departures_by_stop
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
        if max_distance_m <= 0.0 {
            return Vec::new();
        }
        let lat_radius = max_distance_m / 110_540.0;
        let lon_radius = max_distance_m / (111_320.0 * lat.to_radians().cos().abs().max(0.01));
        let min_col = ((lon - lon_radius) / self.cell_degrees).floor() as i32;
        let max_col = ((lon + lon_radius) / self.cell_degrees).floor() as i32;
        let min_row = ((lat - lat_radius) / self.cell_degrees).floor() as i32;
        let max_row = ((lat + lat_radius) / self.cell_degrees).floor() as i32;
        let mut candidates = Vec::new();
        for col in min_col..=max_col {
            for row in min_row..=max_row {
                let Some(stop_indexes) = self.cells.get(&(col, row)) else {
                    continue;
                };
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

/// Transfer expansion considers at most this many nearby stops per stop.
pub(crate) const MAX_TRANSFER_CANDIDATES: usize = 32;

/// Expanding-radius search beyond which a stop is considered to have no more
/// transfer neighbours worth indexing.
const MAX_TRANSFER_INDEX_RADIUS_M: f64 = 100_000.0;

/// Precomputes, for every stop, its `MAX_TRANSFER_CANDIDATES` nearest stops
/// (including itself) sorted by distance. Search-time transfer expansion
/// filters this static list by the requested max transfer distance instead of
/// re-querying and re-sorting the spatial index on every settled state.
pub(crate) fn build_transfer_candidates(
    bundle: &TransitBundle,
    stop_index: &StopSpatialIndex,
) -> Vec<Vec<StopCandidate>> {
    use rayon::prelude::*;

    bundle
        .stops
        .par_iter()
        .map(|stop| {
            let mut radius_m = 500.0_f64;
            loop {
                let mut candidates = stop_index.nearby_stops(bundle, stop.lon, stop.lat, radius_m);
                if candidates.len() >= MAX_TRANSFER_CANDIDATES
                    || radius_m >= MAX_TRANSFER_INDEX_RADIUS_M
                {
                    candidates.sort_by(|left, right| left.distance_m.total_cmp(&right.distance_m));
                    candidates.truncate(MAX_TRANSFER_CANDIDATES);
                    return candidates;
                }
                radius_m *= 2.0;
            }
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
    /// stop, preserving compatibility with coordinate-only estimators. A
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
    pub(crate) departures_by_stop: &'a [Vec<TransitConnection>],
    stop_index: &'a StopSpatialIndex,
    transfer_candidates: &'a [Vec<StopCandidate>],
    pub(crate) allowed_routes: Vec<bool>,
    pub(crate) street_estimator: Option<&'a dyn StreetTimeEstimator>,
}

impl<'a> TransitRuntime<'a> {
    pub(crate) fn new(
        bundle: &'a TransitBundle,
        departures_by_stop: &'a [Vec<TransitConnection>],
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
        }
    }

    pub(crate) fn request_departure_seconds(&self, raw: &str) -> Result<u32> {
        let parsed = match OffsetDateTime::parse(raw, &Rfc3339) {
            Ok(parsed) => parsed,
            Err(_) => parse_naive_datetime(raw)
                .with_context(|| format!("parsing transit datetime '{raw}'"))?,
        };
        let date = parsed.date().to_string();
        let day_index = self
            .bundle
            .service_dates
            .iter()
            .position(|candidate| candidate == &date)
            .ok_or_else(|| {
                anyhow!(
                    "request date {date} is outside transit bundle window {:?}",
                    self.bundle.service_dates
                )
            })?;
        Ok((day_index as u32) * 86_400
            + parsed.hour() as u32 * 3600
            + parsed.minute() as u32 * 60
            + parsed.second() as u32)
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
}

fn parse_naive_datetime(raw: &str) -> Result<OffsetDateTime> {
    let (date, time) = raw.split_once('T').unwrap_or((raw, "00:00:00"));
    let date = parse_iso_date(date)?;
    let parts = time
        .split(':')
        .map(str::parse::<u8>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("invalid time in datetime '{raw}'"))?;
    let time = Time::from_hms(
        parts.first().copied().unwrap_or_default(),
        parts.get(1).copied().unwrap_or_default(),
        parts.get(2).copied().unwrap_or_default(),
    )?;
    Ok(PrimitiveDateTime::new(date, time).assume_utc())
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
    pub(crate) trip_index: u32,
}

pub(crate) fn can_start_transfer_walk(state: StateKey) -> bool {
    state.boardings > 0 && state.trip_index != u32::MAX
}

pub(crate) fn can_finish_with_egress(state: StateKey) -> bool {
    state.boardings > 0 && state.trip_index != u32::MAX
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
            .then_with(|| self.state.trip_index.cmp(&other.state.trip_index))
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
