use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use anyhow::{Context, Result, anyhow};
use time::{OffsetDateTime, PrimitiveDateTime, Time, format_description::well_known::Rfc3339};

use crate::gtfs::parse_iso_date;
use crate::legs::haversine_m;
use crate::model::{TransitBundle, TransitConnection, TransitModeOptions, TransitStop};

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

    fn nearby_stops(
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

#[derive(Debug)]
pub(crate) struct TransitRuntime<'a> {
    pub(crate) bundle: &'a TransitBundle,
    pub(crate) departures_by_stop: &'a [Vec<TransitConnection>],
    stop_index: &'a StopSpatialIndex,
    pub(crate) allowed_routes: Vec<bool>,
}

impl<'a> TransitRuntime<'a> {
    pub(crate) fn new(
        bundle: &'a TransitBundle,
        departures_by_stop: &'a [Vec<TransitConnection>],
        stop_index: &'a StopSpatialIndex,
        modes: &TransitModeOptions,
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
            allowed_routes,
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
        self.nearby_stops(lon, lat, max_distance_m, None)
    }

    fn nearby_transfer_stops(&self, lon: f64, lat: f64, max_distance_m: f64) -> Vec<StopCandidate> {
        self.nearby_stops(lon, lat, max_distance_m, Some(32))
    }

    fn nearby_stops(
        &self,
        lon: f64,
        lat: f64,
        max_distance_m: f64,
        limit: Option<usize>,
    ) -> Vec<StopCandidate> {
        let mut candidates = self
            .stop_index
            .nearby_stops(self.bundle, lon, lat, max_distance_m);
        candidates.sort_by(|left, right| left.distance_m.total_cmp(&right.distance_m));
        if let Some(limit) = limit {
            candidates.truncate(limit);
        }
        candidates
    }

    pub(crate) fn nearby_stop_indexes(
        &self,
        stop_index: u32,
        max_distance_m: f64,
    ) -> Vec<StopCandidate> {
        let stop = &self.bundle.stops[stop_index as usize];
        self.nearby_transfer_stops(stop.lon, stop.lat, max_distance_m)
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

#[derive(Debug, Clone, Copy)]
pub(crate) struct StopCandidate {
    pub(crate) stop_index: u32,
    pub(crate) distance_m: f64,
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
        distance_m: f64,
        departure_s: u32,
    },
    Transfer {
        previous: StateKey,
        distance_m: f64,
        departure_s: u32,
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
