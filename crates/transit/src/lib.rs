use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{
    Date, OffsetDateTime, PrimitiveDateTime, Time, format_description::well_known::Rfc3339,
};
use zip::ZipArchive;

pub const OPENOV_GTFS_URL: &str = "https://gtfs.openov.nl/gtfs-rt/gtfs-openov-nl.zip";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitImportOptions {
    pub name: String,
    pub source_label: String,
    pub service_start_date: String,
    pub service_days: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitImportSummary {
    pub feed_id: String,
    pub source_label: String,
    pub source_sha256: String,
    pub service_start_date: String,
    pub service_days: u32,
    pub stop_count: usize,
    pub route_count: usize,
    pub trip_count: usize,
    pub connection_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitFeedManifest {
    pub feed_id: String,
    pub label: String,
    pub source_path: String,
    pub source_sha256: String,
    pub imported_at: String,
    pub service_start_date: String,
    pub service_days: u32,
    pub stop_count: u64,
    pub route_count: u64,
    pub trip_count: u64,
    pub connection_count: u64,
    pub bundle_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitBundle {
    pub schema_version: u32,
    pub feed_id: String,
    pub source_label: String,
    pub source_sha256: String,
    pub service_dates: Vec<String>,
    pub stops: Vec<TransitStop>,
    pub routes: Vec<TransitRoute>,
    pub trips: Vec<TransitTrip>,
    pub connections: Vec<TransitConnection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitStop {
    pub stop_id: String,
    pub name: String,
    pub lon: f64,
    pub lat: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitRoute {
    pub route_id: String,
    pub short_name: String,
    pub long_name: String,
    pub mode: TransitMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitTrip {
    pub trip_id: String,
    pub route_index: u32,
    pub headsign: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TransitConnection {
    pub trip_index: u32,
    pub route_index: u32,
    pub from_stop_index: u32,
    pub to_stop_index: u32,
    pub departure_s: u32,
    pub arrival_s: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitMode {
    Tram,
    Subway,
    Rail,
    Bus,
    Ferry,
    CableCar,
    Gondola,
    Funicular,
    Coach,
    Air,
    Other,
}

impl TransitMode {
    fn from_gtfs_route_type(value: &str) -> Self {
        match value.parse::<u32>().unwrap_or(u32::MAX) {
            0 => Self::Tram,
            1 => Self::Subway,
            2 | 100..=199 => Self::Rail,
            3 | 700..=799 => Self::Bus,
            4 | 1000..=1099 => Self::Ferry,
            5 => Self::CableCar,
            6 => Self::Gondola,
            7 => Self::Funicular,
            200..=299 => Self::Coach,
            1100..=1199 => Self::Air,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    Walk,
    Bicycle,
    Car,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitRouteRequest {
    pub route_id: String,
    pub origin: TransitPoint,
    pub destination: TransitPoint,
    pub time: TransitQueryTime,
    #[serde(default)]
    pub modes: TransitModeOptions,
    #[serde(default)]
    pub returns: TransitReturnOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitPoint {
    pub id: String,
    pub lon: f64,
    pub lat: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitQueryTime {
    pub datetime: String,
    #[serde(default)]
    pub arrive_by: bool,
    #[serde(default = "default_search_window_s")]
    pub search_window_s: u32,
}

fn default_search_window_s() -> u32 {
    60 * 60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitModeOptions {
    #[serde(default = "default_access_modes")]
    pub access: Vec<AccessMode>,
    #[serde(default = "default_access_modes")]
    pub egress: Vec<AccessMode>,
    #[serde(default = "default_transit_modes")]
    pub transit: Vec<TransitMode>,
    #[serde(default = "default_walk_speed_kph")]
    pub walk_speed_kph: f64,
    #[serde(default = "default_bicycle_speed_kph")]
    pub bicycle_speed_kph: f64,
    #[serde(default = "default_car_access_speed_kph")]
    pub car_access_speed_kph: f64,
    #[serde(default = "default_access_distance_m")]
    pub max_access_distance_m: f64,
    #[serde(default = "default_access_distance_m")]
    pub max_egress_distance_m: f64,
    #[serde(default = "default_transfer_distance_m")]
    pub max_transfer_distance_m: f64,
    #[serde(default = "default_board_slack_s")]
    pub board_slack_s: u32,
    #[serde(default = "default_transfer_slack_s")]
    pub transfer_slack_s: u32,
    #[serde(default = "default_max_transfers")]
    pub max_transfers: u8,
}

impl Default for TransitModeOptions {
    fn default() -> Self {
        Self {
            access: default_access_modes(),
            egress: default_access_modes(),
            transit: default_transit_modes(),
            walk_speed_kph: default_walk_speed_kph(),
            bicycle_speed_kph: default_bicycle_speed_kph(),
            car_access_speed_kph: default_car_access_speed_kph(),
            max_access_distance_m: default_access_distance_m(),
            max_egress_distance_m: default_access_distance_m(),
            max_transfer_distance_m: default_transfer_distance_m(),
            board_slack_s: default_board_slack_s(),
            transfer_slack_s: default_transfer_slack_s(),
            max_transfers: default_max_transfers(),
        }
    }
}

fn default_access_modes() -> Vec<AccessMode> {
    vec![AccessMode::Walk]
}

fn default_transit_modes() -> Vec<TransitMode> {
    vec![
        TransitMode::Tram,
        TransitMode::Subway,
        TransitMode::Rail,
        TransitMode::Bus,
        TransitMode::Ferry,
        TransitMode::CableCar,
        TransitMode::Gondola,
        TransitMode::Funicular,
        TransitMode::Coach,
    ]
}

fn default_walk_speed_kph() -> f64 {
    4.8
}

fn default_bicycle_speed_kph() -> f64 {
    15.0
}

fn default_car_access_speed_kph() -> f64 {
    25.0
}

fn default_access_distance_m() -> f64 {
    1_000.0
}

fn default_transfer_distance_m() -> f64 {
    500.0
}

fn default_board_slack_s() -> u32 {
    30
}

fn default_transfer_slack_s() -> u32 {
    120
}

fn default_max_transfers() -> u8 {
    4
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TransitReturnOptions {
    #[serde(default)]
    pub include_geometry: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitRouteResult {
    pub route_id: String,
    pub outcome: TransitOutcome,
    pub summary: TransitRouteSummary,
    pub legs: Vec<TransitLeg>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitOutcome {
    Scheduled,
    Unreachable,
    NotImplemented,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TransitRouteSummary {
    pub departure_s: u32,
    pub arrival_s: Option<u32>,
    pub total_travel_time_s: Option<u32>,
    pub transit_time_s: u32,
    pub access_egress_time_s: u32,
    pub transfer_time_s: u32,
    pub wait_time_s: u32,
    pub boarding_count: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitLeg {
    pub leg_type: TransitLegType,
    pub from_id: String,
    pub to_id: String,
    pub from_name: String,
    pub to_name: String,
    pub departure_s: u32,
    pub arrival_s: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<TransitMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_short_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trip_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headsign: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub geometry: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitLegType {
    Access,
    Transit,
    Transfer,
    Egress,
}

pub fn load_transit_request(path: impl AsRef<Path>) -> Result<TransitRouteRequest> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading transit route request {}", path.display()))?;
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => serde_json::from_str(&raw).context("parsing JSON transit route request"),
        other => bail!(
            "unsupported transit route request extension {:?}; use .json",
            other
        ),
    }
}

pub fn import_gtfs(path: impl AsRef<Path>, options: TransitImportOptions) -> Result<TransitBundle> {
    let path = path.as_ref();
    let source_sha256 = hash_gtfs_source(path)?;
    let files = read_gtfs_files(path)?;
    build_bundle_from_files(files, source_sha256, options)
}

pub fn write_transit_bundle(path: impl AsRef<Path>, bundle: &TransitBundle) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    bincode::serialize_into(file, bundle)
        .with_context(|| format!("serializing transit bundle {}", path.display()))
}

pub fn read_transit_bundle(path: impl AsRef<Path>) -> Result<TransitBundle> {
    let path = path.as_ref();
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    bincode::deserialize(&bytes)
        .with_context(|| format!("parsing transit bundle {}", path.display()))
}

pub fn transit_import_summary(bundle: &TransitBundle) -> TransitImportSummary {
    TransitImportSummary {
        feed_id: bundle.feed_id.clone(),
        source_label: bundle.source_label.clone(),
        source_sha256: bundle.source_sha256.clone(),
        service_start_date: bundle.service_dates.first().cloned().unwrap_or_default(),
        service_days: bundle.service_dates.len() as u32,
        stop_count: bundle.stops.len(),
        route_count: bundle.routes.len(),
        trip_count: bundle.trips.len(),
        connection_count: bundle.connections.len(),
    }
}

pub fn execute_transit_route(
    bundle: &TransitBundle,
    request: &TransitRouteRequest,
) -> Result<TransitRouteResult> {
    if request.time.arrive_by {
        return Ok(TransitRouteResult {
            route_id: request.route_id.clone(),
            outcome: TransitOutcome::NotImplemented,
            summary: TransitRouteSummary::default(),
            legs: Vec::new(),
            diagnostics: vec![
                "arrive_by transit searches are not implemented yet; use depart-after timing"
                    .to_string(),
            ],
        });
    }
    if !request.modes.access.contains(&AccessMode::Walk)
        || !request.modes.egress.contains(&AccessMode::Walk)
    {
        return Ok(TransitRouteResult {
            route_id: request.route_id.clone(),
            outcome: TransitOutcome::NotImplemented,
            summary: TransitRouteSummary::default(),
            legs: Vec::new(),
            diagnostics: vec![
                "only pedestrian access and egress are implemented; bicycle and car access are reserved in the request schema".to_string(),
            ],
        });
    }

    let runtime = TransitRuntime::new(bundle, &request.modes);
    let departure_s = runtime.request_departure_seconds(&request.time.datetime)?;
    let search_end_s = departure_s.saturating_add(request.time.search_window_s);
    let access = runtime.nearby_stops(
        request.origin.lon,
        request.origin.lat,
        request.modes.max_access_distance_m,
    );
    let egress = runtime.nearby_stops(
        request.destination.lon,
        request.destination.lat,
        request.modes.max_egress_distance_m,
    );
    if access.is_empty() || egress.is_empty() {
        return Ok(TransitRouteResult {
            route_id: request.route_id.clone(),
            outcome: TransitOutcome::Unreachable,
            summary: TransitRouteSummary {
                departure_s,
                ..TransitRouteSummary::default()
            },
            legs: Vec::new(),
            diagnostics: vec![format!(
                "no stop found within access/egress limits (access candidates {}, egress candidates {})",
                access.len(),
                egress.len()
            )],
        });
    }

    let mut heap = BinaryHeap::new();
    let mut best = HashMap::<StateKey, u32>::new();
    let mut prev = HashMap::<StateKey, PrevStep>::new();
    for candidate in access {
        let access_time_s =
            seconds_for_distance(candidate.distance_m, request.modes.walk_speed_kph);
        let state = StateKey {
            stop_index: candidate.stop_index,
            boardings: 0,
            trip_index: u32::MAX,
        };
        let arrival_s = departure_s.saturating_add(access_time_s);
        relax_state(
            &mut heap,
            &mut best,
            &mut prev,
            state,
            arrival_s,
            PrevStep::Access {
                from_id: request.origin.id.clone(),
                from_name: request.origin.id.clone(),
                from_lon: request.origin.lon,
                from_lat: request.origin.lat,
                distance_m: candidate.distance_m,
                departure_s,
            },
        );
    }

    let egress_by_stop = egress
        .into_iter()
        .map(|candidate| (candidate.stop_index, candidate.distance_m))
        .collect::<HashMap<_, _>>();
    let mut best_final: Option<(u32, StateKey, u32)> = None;

    while let Some(entry) = heap.pop() {
        if best
            .get(&entry.state)
            .is_some_and(|known| *known < entry.time_s)
        {
            continue;
        }
        if let Some((arrival_s, _, _)) = best_final {
            if entry.time_s >= arrival_s {
                continue;
            }
        }
        if entry.time_s > search_end_s.saturating_add(6 * 60 * 60) {
            continue;
        }

        if let Some(distance_m) = egress_by_stop.get(&entry.state.stop_index).copied() {
            let walk_s = seconds_for_distance(distance_m, request.modes.walk_speed_kph);
            let arrival_s = entry.time_s.saturating_add(walk_s);
            if best_final
                .as_ref()
                .is_none_or(|(best_arrival_s, _, _)| arrival_s < *best_arrival_s)
            {
                best_final = Some((arrival_s, entry.state, walk_s));
            }
        }

        let transfer_departure_s = if entry.state.trip_index == u32::MAX {
            entry.time_s
        } else {
            entry.time_s.saturating_add(request.modes.transfer_slack_s)
        };
        for transfer in runtime.nearby_stop_indexes(
            entry.state.stop_index,
            request.modes.max_transfer_distance_m,
        ) {
            if transfer.stop_index == entry.state.stop_index {
                continue;
            }
            let walk_s = seconds_for_distance(transfer.distance_m, request.modes.walk_speed_kph);
            let next_state = StateKey {
                stop_index: transfer.stop_index,
                boardings: entry.state.boardings,
                trip_index: u32::MAX,
            };
            relax_state(
                &mut heap,
                &mut best,
                &mut prev,
                next_state,
                transfer_departure_s.saturating_add(walk_s),
                PrevStep::Transfer {
                    previous: entry.state,
                    distance_m: transfer.distance_m,
                    departure_s: transfer_departure_s,
                },
            );
        }

        if let Some(departures) = runtime
            .departures_by_stop
            .get(entry.state.stop_index as usize)
        {
            let start = departures.partition_point(|connection| {
                let slack = if entry.state.trip_index == connection.trip_index {
                    0
                } else {
                    request.modes.board_slack_s
                };
                connection.departure_s < entry.time_s.saturating_add(slack)
            });
            for connection in departures[start..].iter() {
                if connection.departure_s > search_end_s {
                    break;
                }
                if !runtime.allowed_routes[connection.route_index as usize] {
                    continue;
                }
                let same_trip = entry.state.trip_index == connection.trip_index;
                let next_boardings = if same_trip {
                    entry.state.boardings
                } else {
                    entry.state.boardings.saturating_add(1)
                };
                if next_boardings > request.modes.max_transfers.saturating_add(1) {
                    continue;
                }
                let next_state = StateKey {
                    stop_index: connection.to_stop_index,
                    boardings: next_boardings,
                    trip_index: connection.trip_index,
                };
                relax_state(
                    &mut heap,
                    &mut best,
                    &mut prev,
                    next_state,
                    connection.arrival_s,
                    PrevStep::Transit {
                        previous: entry.state,
                        connection: *connection,
                    },
                );
            }
        }
    }

    let Some((arrival_s, final_state, egress_walk_s)) = best_final else {
        return Ok(TransitRouteResult {
            route_id: request.route_id.clone(),
            outcome: TransitOutcome::Unreachable,
            summary: TransitRouteSummary {
                departure_s,
                ..TransitRouteSummary::default()
            },
            legs: Vec::new(),
            diagnostics: vec!["no scheduled journey found inside the search window".to_string()],
        });
    };

    let mut legs = reconstruct_legs(
        bundle,
        &runtime,
        request,
        &prev,
        final_state,
        arrival_s,
        egress_walk_s,
    )?;
    coalesce_transit_legs(&mut legs);
    let summary = summarize_legs(departure_s, arrival_s, &legs);
    Ok(TransitRouteResult {
        route_id: request.route_id.clone(),
        outcome: TransitOutcome::Scheduled,
        summary,
        legs,
        diagnostics: Vec::new(),
    })
}

fn hash_gtfs_source(path: &Path) -> Result<String> {
    if path.is_dir() {
        let mut hasher = Sha256::new();
        for name in [
            "agency.txt",
            "stops.txt",
            "routes.txt",
            "trips.txt",
            "stop_times.txt",
            "calendar.txt",
            "calendar_dates.txt",
        ] {
            let file_path = path.join(name);
            if file_path.exists() {
                hasher.update(name.as_bytes());
                hasher.update(fs::read(&file_path)?);
            }
        }
        Ok(hex::encode(hasher.finalize()))
    } else {
        let file =
            File::open(path).with_context(|| format!("opening GTFS source {}", path.display()))?;
        let mut reader = BufReader::new(file);
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = reader
                .read(&mut buffer)
                .with_context(|| format!("reading GTFS source {}", path.display()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hex::encode(hasher.finalize()))
    }
}

fn read_gtfs_files(path: &Path) -> Result<GtfsFiles> {
    let mut files = GtfsFiles::default();
    if path.is_dir() {
        for name in GtfsFiles::names() {
            let file_path = path.join(name);
            if file_path.exists() {
                files.insert(name, fs::read_to_string(&file_path)?);
            }
        }
    } else {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut archive = ZipArchive::new(file).context("opening GTFS zip")?;
        for name in GtfsFiles::names() {
            if let Ok(mut file) = archive.by_name(name) {
                let mut text = String::new();
                file.read_to_string(&mut text)
                    .with_context(|| format!("reading {name} from GTFS zip"))?;
                files.insert(name, text);
            }
        }
    }
    files.require("stops.txt")?;
    files.require("routes.txt")?;
    files.require("trips.txt")?;
    files.require("stop_times.txt")?;
    Ok(files)
}

#[derive(Default)]
struct GtfsFiles {
    inner: HashMap<&'static str, String>,
}

impl GtfsFiles {
    fn names() -> [&'static str; 7] {
        [
            "agency.txt",
            "stops.txt",
            "routes.txt",
            "trips.txt",
            "stop_times.txt",
            "calendar.txt",
            "calendar_dates.txt",
        ]
    }

    fn insert(&mut self, name: &'static str, text: String) {
        self.inner.insert(name, text);
    }

    fn get(&self, name: &'static str) -> Option<&str> {
        self.inner.get(name).map(String::as_str)
    }

    fn require(&self, name: &'static str) -> Result<()> {
        if self.inner.contains_key(name) {
            Ok(())
        } else {
            bail!("GTFS feed is missing required {name}")
        }
    }
}

fn build_bundle_from_files(
    files: GtfsFiles,
    source_sha256: String,
    options: TransitImportOptions,
) -> Result<TransitBundle> {
    let service_dates = service_date_window(&options.service_start_date, options.service_days)?;
    let active_services = active_services_by_date(&files, &service_dates)?;
    let (stops, stop_by_id) = parse_stops(files.get("stops.txt").unwrap())?;
    let (routes, route_by_id) = parse_routes(files.get("routes.txt").unwrap())?;
    let (trips, trip_by_id, retained_gtfs_trips) = parse_trips(
        files.get("trips.txt").unwrap(),
        &route_by_id,
        &active_services,
    )?;
    let connections = parse_connections(
        files.get("stop_times.txt").unwrap(),
        &stop_by_id,
        &trip_by_id,
        &trips,
        &active_services,
        &retained_gtfs_trips,
    )?;

    Ok(TransitBundle {
        schema_version: 1,
        feed_id: options.name,
        source_label: options.source_label,
        source_sha256,
        service_dates: service_dates
            .iter()
            .map(|date| date.to_string())
            .collect::<Vec<_>>(),
        stops,
        routes,
        trips,
        connections,
    })
}

fn parse_stops(raw: &str) -> Result<(Vec<TransitStop>, HashMap<String, u32>)> {
    let mut reader = csv::Reader::from_reader(raw.as_bytes());
    let headers = reader.headers()?.clone();
    let id = header_index(&headers, "stop_id")?;
    let name = optional_header_index(&headers, "stop_name");
    let lat = header_index(&headers, "stop_lat")?;
    let lon = header_index(&headers, "stop_lon")?;
    let location_type = optional_header_index(&headers, "location_type");
    let mut stops = Vec::new();
    let mut stop_by_id = HashMap::new();
    for record in reader.records() {
        let record = record?;
        if let Some(index) = location_type {
            let value = record.get(index).unwrap_or_default();
            if !value.is_empty() && value != "0" {
                continue;
            }
        }
        let stop_id = record.get(id).unwrap_or_default().to_string();
        if stop_id.is_empty() {
            continue;
        }
        let lon = record.get(lon).unwrap_or_default().parse::<f64>()?;
        let lat = record.get(lat).unwrap_or_default().parse::<f64>()?;
        let stop = TransitStop {
            stop_id: stop_id.clone(),
            name: name
                .and_then(|index| record.get(index))
                .unwrap_or(&stop_id)
                .to_string(),
            lon,
            lat,
        };
        stop_by_id.insert(stop_id, stops.len() as u32);
        stops.push(stop);
    }
    Ok((stops, stop_by_id))
}

fn parse_routes(raw: &str) -> Result<(Vec<TransitRoute>, HashMap<String, u32>)> {
    let mut reader = csv::Reader::from_reader(raw.as_bytes());
    let headers = reader.headers()?.clone();
    let id = header_index(&headers, "route_id")?;
    let short_name = optional_header_index(&headers, "route_short_name");
    let long_name = optional_header_index(&headers, "route_long_name");
    let route_type = header_index(&headers, "route_type")?;
    let mut routes = Vec::new();
    let mut route_by_id = HashMap::new();
    for record in reader.records() {
        let record = record?;
        let route_id = record.get(id).unwrap_or_default().to_string();
        if route_id.is_empty() {
            continue;
        }
        let route = TransitRoute {
            route_id: route_id.clone(),
            short_name: short_name
                .and_then(|index| record.get(index))
                .unwrap_or_default()
                .to_string(),
            long_name: long_name
                .and_then(|index| record.get(index))
                .unwrap_or_default()
                .to_string(),
            mode: TransitMode::from_gtfs_route_type(record.get(route_type).unwrap_or_default()),
        };
        route_by_id.insert(route_id, routes.len() as u32);
        routes.push(route);
    }
    Ok((routes, route_by_id))
}

fn parse_trips(
    raw: &str,
    route_by_id: &HashMap<String, u32>,
    active_services: &HashMap<String, Vec<u32>>,
) -> Result<(
    Vec<TransitTrip>,
    HashMap<String, u32>,
    HashMap<String, Vec<u32>>,
)> {
    let mut reader = csv::Reader::from_reader(raw.as_bytes());
    let headers = reader.headers()?.clone();
    let route_id = header_index(&headers, "route_id")?;
    let service_id = header_index(&headers, "service_id")?;
    let trip_id = header_index(&headers, "trip_id")?;
    let headsign = optional_header_index(&headers, "trip_headsign");
    let mut trips = Vec::new();
    let mut trip_by_id = HashMap::new();
    let mut retained = HashMap::new();
    for record in reader.records() {
        let record = record?;
        let Some(service_dates) = active_services.get(record.get(service_id).unwrap_or_default())
        else {
            continue;
        };
        let Some(route_index) = route_by_id.get(record.get(route_id).unwrap_or_default()) else {
            continue;
        };
        let gtfs_trip_id = record.get(trip_id).unwrap_or_default().to_string();
        if gtfs_trip_id.is_empty() {
            continue;
        }
        let trip = TransitTrip {
            trip_id: gtfs_trip_id.clone(),
            route_index: *route_index,
            headsign: headsign
                .and_then(|index| record.get(index))
                .unwrap_or_default()
                .to_string(),
        };
        trip_by_id.insert(gtfs_trip_id.clone(), trips.len() as u32);
        retained.insert(gtfs_trip_id, service_dates.clone());
        trips.push(trip);
    }
    Ok((trips, trip_by_id, retained))
}

fn parse_connections(
    raw: &str,
    stop_by_id: &HashMap<String, u32>,
    trip_by_id: &HashMap<String, u32>,
    trips: &[TransitTrip],
    active_services: &HashMap<String, Vec<u32>>,
    retained_gtfs_trips: &HashMap<String, Vec<u32>>,
) -> Result<Vec<TransitConnection>> {
    let _ = active_services;
    let mut reader = csv::Reader::from_reader(raw.as_bytes());
    let headers = reader.headers()?.clone();
    let trip_id = header_index(&headers, "trip_id")?;
    let arrival_time = header_index(&headers, "arrival_time")?;
    let departure_time = header_index(&headers, "departure_time")?;
    let stop_id = header_index(&headers, "stop_id")?;
    let sequence = header_index(&headers, "stop_sequence")?;
    let mut stop_times_by_trip = HashMap::<u32, Vec<StopTimeRow>>::new();
    for record in reader.records() {
        let record = record?;
        let gtfs_trip_id = record.get(trip_id).unwrap_or_default();
        let Some(&trip_index) = trip_by_id.get(gtfs_trip_id) else {
            continue;
        };
        let Some(&stop_index) = stop_by_id.get(record.get(stop_id).unwrap_or_default()) else {
            continue;
        };
        let row = StopTimeRow {
            stop_index,
            sequence: record
                .get(sequence)
                .unwrap_or_default()
                .parse::<u32>()
                .unwrap_or_default(),
            arrival_s: parse_gtfs_time(record.get(arrival_time).unwrap_or_default())?,
            departure_s: parse_gtfs_time(record.get(departure_time).unwrap_or_default())?,
        };
        stop_times_by_trip.entry(trip_index).or_default().push(row);
    }
    let mut connections = Vec::new();
    for (trip_index, mut rows) in stop_times_by_trip {
        rows.sort_by_key(|row| row.sequence);
        let trip = &trips[trip_index as usize];
        let date_offsets = retained_gtfs_trips
            .get(&trip.trip_id)
            .cloned()
            .unwrap_or_default();
        for pair in rows.windows(2) {
            let from = pair[0];
            let to = pair[1];
            if to.arrival_s < from.departure_s {
                continue;
            }
            for date_offset in &date_offsets {
                let base = date_offset.saturating_mul(86_400);
                connections.push(TransitConnection {
                    trip_index,
                    route_index: trip.route_index,
                    from_stop_index: from.stop_index,
                    to_stop_index: to.stop_index,
                    departure_s: base.saturating_add(from.departure_s),
                    arrival_s: base.saturating_add(to.arrival_s),
                });
            }
        }
    }
    connections.sort_by_key(|connection| connection.departure_s);
    Ok(connections)
}

#[derive(Debug, Clone, Copy)]
struct StopTimeRow {
    stop_index: u32,
    sequence: u32,
    arrival_s: u32,
    departure_s: u32,
}

fn active_services_by_date(
    files: &GtfsFiles,
    service_dates: &[Date],
) -> Result<HashMap<String, Vec<u32>>> {
    let mut service_dates_by_id = HashMap::<String, Vec<u32>>::new();
    if let Some(raw) = files.get("calendar.txt") {
        parse_calendar(raw, service_dates, &mut service_dates_by_id)?;
    }
    if let Some(raw) = files.get("calendar_dates.txt") {
        parse_calendar_dates(raw, service_dates, &mut service_dates_by_id)?;
    }
    service_dates_by_id.retain(|_, dates| !dates.is_empty());
    Ok(service_dates_by_id)
}

fn parse_calendar(
    raw: &str,
    service_dates: &[Date],
    active: &mut HashMap<String, Vec<u32>>,
) -> Result<()> {
    let mut reader = csv::Reader::from_reader(raw.as_bytes());
    let headers = reader.headers()?.clone();
    let service_id = header_index(&headers, "service_id")?;
    let days = [
        header_index(&headers, "monday")?,
        header_index(&headers, "tuesday")?,
        header_index(&headers, "wednesday")?,
        header_index(&headers, "thursday")?,
        header_index(&headers, "friday")?,
        header_index(&headers, "saturday")?,
        header_index(&headers, "sunday")?,
    ];
    let start_date = header_index(&headers, "start_date")?;
    let end_date = header_index(&headers, "end_date")?;
    for record in reader.records() {
        let record = record?;
        let id = record.get(service_id).unwrap_or_default().to_string();
        let start = parse_gtfs_date(record.get(start_date).unwrap_or_default())?;
        let end = parse_gtfs_date(record.get(end_date).unwrap_or_default())?;
        for (offset, date) in service_dates.iter().enumerate() {
            if *date < start || *date > end {
                continue;
            }
            let weekday_index = date.weekday().number_days_from_monday() as usize;
            if record.get(days[weekday_index]).unwrap_or_default() == "1" {
                active.entry(id.clone()).or_default().push(offset as u32);
            }
        }
    }
    Ok(())
}

fn parse_calendar_dates(
    raw: &str,
    service_dates: &[Date],
    active: &mut HashMap<String, Vec<u32>>,
) -> Result<()> {
    let date_offsets = service_dates
        .iter()
        .enumerate()
        .map(|(offset, date)| (*date, offset as u32))
        .collect::<BTreeMap<_, _>>();
    let mut reader = csv::Reader::from_reader(raw.as_bytes());
    let headers = reader.headers()?.clone();
    let service_id = header_index(&headers, "service_id")?;
    let date = header_index(&headers, "date")?;
    let exception_type = header_index(&headers, "exception_type")?;
    for record in reader.records() {
        let record = record?;
        let parsed_date = parse_gtfs_date(record.get(date).unwrap_or_default())?;
        let Some(&offset) = date_offsets.get(&parsed_date) else {
            continue;
        };
        let id = record.get(service_id).unwrap_or_default().to_string();
        let entry = active.entry(id).or_default();
        match record.get(exception_type).unwrap_or_default() {
            "1" => {
                if !entry.contains(&offset) {
                    entry.push(offset);
                    entry.sort_unstable();
                }
            }
            "2" => entry.retain(|candidate| *candidate != offset),
            _ => {}
        }
    }
    Ok(())
}

fn service_date_window(start: &str, days: u32) -> Result<Vec<Date>> {
    if days == 0 {
        bail!("service_days must be greater than zero");
    }
    let start = parse_iso_date(start)?;
    let mut dates = Vec::with_capacity(days as usize);
    for offset in 0..days {
        dates.push(start + time::Duration::days(offset as i64));
    }
    Ok(dates)
}

fn parse_iso_date(raw: &str) -> Result<Date> {
    let parts = raw
        .split('-')
        .map(str::parse::<i32>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if parts.len() != 3 {
        bail!("invalid date '{raw}', expected YYYY-MM-DD");
    }
    Date::from_calendar_date(
        parts[0],
        time::Month::try_from(parts[1] as u8)?,
        parts[2] as u8,
    )
    .with_context(|| format!("invalid date '{raw}'"))
}

fn parse_gtfs_date(raw: &str) -> Result<Date> {
    if raw.len() != 8 {
        bail!("invalid GTFS date '{raw}', expected YYYYMMDD");
    }
    let year = raw[0..4].parse::<i32>()?;
    let month = raw[4..6].parse::<u8>()?;
    let day = raw[6..8].parse::<u8>()?;
    Date::from_calendar_date(year, time::Month::try_from(month)?, day)
        .with_context(|| format!("invalid GTFS date '{raw}'"))
}

fn parse_gtfs_time(raw: &str) -> Result<u32> {
    let parts = raw
        .split(':')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if parts.len() != 3 || parts[1] >= 60 || parts[2] >= 60 {
        bail!("invalid GTFS time '{raw}', expected HH:MM:SS");
    }
    Ok(parts[0] * 3600 + parts[1] * 60 + parts[2])
}

fn header_index(headers: &csv::StringRecord, name: &str) -> Result<usize> {
    headers
        .iter()
        .position(|candidate| candidate == name)
        .ok_or_else(|| anyhow!("missing required GTFS column '{name}'"))
}

fn optional_header_index(headers: &csv::StringRecord, name: &str) -> Option<usize> {
    headers.iter().position(|candidate| candidate == name)
}

#[derive(Debug)]
struct TransitRuntime<'a> {
    bundle: &'a TransitBundle,
    departures_by_stop: Vec<Vec<TransitConnection>>,
    allowed_routes: Vec<bool>,
}

impl<'a> TransitRuntime<'a> {
    fn new(bundle: &'a TransitBundle, modes: &TransitModeOptions) -> Self {
        let mut departures_by_stop = vec![Vec::new(); bundle.stops.len()];
        for connection in &bundle.connections {
            departures_by_stop[connection.from_stop_index as usize].push(*connection);
        }
        for departures in &mut departures_by_stop {
            departures.sort_by_key(|connection| connection.departure_s);
        }
        let allowed_routes = bundle
            .routes
            .iter()
            .map(|route| modes.transit.contains(&route.mode))
            .collect();
        Self {
            bundle,
            departures_by_stop,
            allowed_routes,
        }
    }

    fn request_departure_seconds(&self, raw: &str) -> Result<u32> {
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

    fn nearby_stops(&self, lon: f64, lat: f64, max_distance_m: f64) -> Vec<StopCandidate> {
        let mut candidates = self
            .bundle
            .stops
            .iter()
            .enumerate()
            .filter_map(|(stop_index, stop)| {
                let distance_m = haversine_m(lon, lat, stop.lon, stop.lat);
                (distance_m <= max_distance_m).then_some(StopCandidate {
                    stop_index: stop_index as u32,
                    distance_m,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.distance_m.total_cmp(&right.distance_m));
        candidates.truncate(32);
        candidates
    }

    fn nearby_stop_indexes(&self, stop_index: u32, max_distance_m: f64) -> Vec<StopCandidate> {
        let stop = &self.bundle.stops[stop_index as usize];
        self.nearby_stops(stop.lon, stop.lat, max_distance_m)
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
struct StopCandidate {
    stop_index: u32,
    distance_m: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StateKey {
    stop_index: u32,
    boardings: u8,
    trip_index: u32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct QueueEntry {
    time_s: u32,
    state: StateKey,
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
enum PrevStep {
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

fn relax_state(
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

fn reconstruct_legs(
    bundle: &TransitBundle,
    runtime: &TransitRuntime,
    request: &TransitRouteRequest,
    prev: &HashMap<StateKey, PrevStep>,
    final_state: StateKey,
    arrival_s: u32,
    egress_walk_s: u32,
) -> Result<Vec<TransitLeg>> {
    let final_stop = &bundle.stops[final_state.stop_index as usize];
    let mut legs = vec![TransitLeg {
        leg_type: TransitLegType::Egress,
        from_id: final_stop.stop_id.clone(),
        to_id: request.destination.id.clone(),
        from_name: final_stop.name.clone(),
        to_name: request.destination.id.clone(),
        departure_s: arrival_s.saturating_sub(egress_walk_s),
        arrival_s,
        mode: None,
        route_id: None,
        route_short_name: None,
        trip_id: None,
        headsign: None,
        geometry: geometry_if_requested(
            request,
            [final_stop.lon, final_stop.lat],
            [request.destination.lon, request.destination.lat],
        ),
    }];
    let mut cursor = final_state;
    loop {
        let Some(step) = prev.get(&cursor) else {
            break;
        };
        match step {
            PrevStep::Access {
                from_id,
                from_name,
                from_lon,
                from_lat,
                distance_m,
                departure_s,
            } => {
                let stop = &bundle.stops[cursor.stop_index as usize];
                let walk_s = seconds_for_distance(*distance_m, request.modes.walk_speed_kph);
                legs.push(TransitLeg {
                    leg_type: TransitLegType::Access,
                    from_id: from_id.clone(),
                    to_id: stop.stop_id.clone(),
                    from_name: from_name.clone(),
                    to_name: stop.name.clone(),
                    departure_s: *departure_s,
                    arrival_s: departure_s.saturating_add(walk_s),
                    mode: None,
                    route_id: None,
                    route_short_name: None,
                    trip_id: None,
                    headsign: None,
                    geometry: geometry_if_requested(
                        request,
                        [*from_lon, *from_lat],
                        [stop.lon, stop.lat],
                    ),
                });
                break;
            }
            PrevStep::Transfer {
                previous,
                distance_m,
                departure_s,
            } => {
                let from = &bundle.stops[previous.stop_index as usize];
                let to = &bundle.stops[cursor.stop_index as usize];
                let walk_s = seconds_for_distance(*distance_m, request.modes.walk_speed_kph);
                legs.push(TransitLeg {
                    leg_type: TransitLegType::Transfer,
                    from_id: from.stop_id.clone(),
                    to_id: to.stop_id.clone(),
                    from_name: from.name.clone(),
                    to_name: to.name.clone(),
                    departure_s: *departure_s,
                    arrival_s: departure_s.saturating_add(walk_s),
                    mode: None,
                    route_id: None,
                    route_short_name: None,
                    trip_id: None,
                    headsign: None,
                    geometry: geometry_if_requested(
                        request,
                        [from.lon, from.lat],
                        [to.lon, to.lat],
                    ),
                });
                cursor = *previous;
            }
            PrevStep::Transit {
                previous,
                connection,
            } => {
                let from = &bundle.stops[connection.from_stop_index as usize];
                let to = &bundle.stops[connection.to_stop_index as usize];
                let route = &bundle.routes[connection.route_index as usize];
                let trip = &bundle.trips[connection.trip_index as usize];
                let _ = runtime;
                legs.push(TransitLeg {
                    leg_type: TransitLegType::Transit,
                    from_id: from.stop_id.clone(),
                    to_id: to.stop_id.clone(),
                    from_name: from.name.clone(),
                    to_name: to.name.clone(),
                    departure_s: connection.departure_s,
                    arrival_s: connection.arrival_s,
                    mode: Some(route.mode),
                    route_id: Some(route.route_id.clone()),
                    route_short_name: Some(route.short_name.clone()),
                    trip_id: Some(trip.trip_id.clone()),
                    headsign: Some(trip.headsign.clone()),
                    geometry: geometry_if_requested(
                        request,
                        [from.lon, from.lat],
                        [to.lon, to.lat],
                    ),
                });
                cursor = *previous;
            }
        }
    }
    legs.reverse();
    Ok(legs)
}

fn coalesce_transit_legs(legs: &mut Vec<TransitLeg>) {
    let mut coalesced = Vec::<TransitLeg>::new();
    for leg in legs.drain(..) {
        if let Some(last) = coalesced.last_mut() {
            if last.leg_type == TransitLegType::Transit
                && leg.leg_type == TransitLegType::Transit
                && last.trip_id == leg.trip_id
                && last.to_id == leg.from_id
            {
                last.to_id = leg.to_id;
                last.to_name = leg.to_name;
                last.arrival_s = leg.arrival_s;
                last.geometry.extend(leg.geometry.into_iter().skip(1));
                continue;
            }
        }
        coalesced.push(leg);
    }
    *legs = coalesced;
}

fn summarize_legs(departure_s: u32, arrival_s: u32, legs: &[TransitLeg]) -> TransitRouteSummary {
    let mut summary = TransitRouteSummary {
        departure_s,
        arrival_s: Some(arrival_s),
        total_travel_time_s: Some(arrival_s.saturating_sub(departure_s)),
        ..TransitRouteSummary::default()
    };
    let mut previous_arrival = departure_s;
    for leg in legs {
        if leg.departure_s > previous_arrival {
            summary.wait_time_s += leg.departure_s - previous_arrival;
        }
        let duration = leg.arrival_s.saturating_sub(leg.departure_s);
        match leg.leg_type {
            TransitLegType::Access | TransitLegType::Egress => {
                summary.access_egress_time_s += duration
            }
            TransitLegType::Transfer => summary.transfer_time_s += duration,
            TransitLegType::Transit => {
                summary.transit_time_s += duration;
                summary.boarding_count += 1;
            }
        }
        previous_arrival = leg.arrival_s;
    }
    summary
}

fn geometry_if_requested(
    request: &TransitRouteRequest,
    from: [f64; 2],
    to: [f64; 2],
) -> Vec<[f64; 2]> {
    if request.returns.include_geometry {
        vec![from, to]
    } else {
        Vec::new()
    }
}

fn seconds_for_distance(distance_m: f64, speed_kph: f64) -> u32 {
    if speed_kph <= 0.0 {
        return u32::MAX / 4;
    }
    ((distance_m / (speed_kph * 1000.0 / 3600.0)).ceil() as u32).max(1)
}

fn haversine_m(lon_a: f64, lat_a: f64, lon_b: f64, lat_b: f64) -> f64 {
    let earth_radius_m = 6_371_000.0_f64;
    let lat1 = lat_a.to_radians();
    let lat2 = lat_b.to_radians();
    let dlat = (lat_b - lat_a).to_radians();
    let dlon = (lon_b - lon_a).to_radians();
    let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * earth_radius_m * a.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_one_week_and_routes_walk_transit_walk() {
        let bundle = build_bundle_from_files(
            fixture_files(),
            "abc".to_string(),
            TransitImportOptions {
                name: "fixture".to_string(),
                source_label: "fixture".to_string(),
                service_start_date: "2026-05-11".to_string(),
                service_days: 7,
            },
        )
        .expect("fixture imports");
        assert_eq!(bundle.connections.len(), 14);
        let request = TransitRouteRequest {
            route_id: "r1".to_string(),
            origin: TransitPoint {
                id: "origin".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: TransitPoint {
                id: "dest".to_string(),
                lon: 6.02,
                lat: 53.0,
            },
            time: TransitQueryTime {
                datetime: "2026-05-11T08:00:00+02:00".to_string(),
                arrive_by: false,
                search_window_s: 3600,
            },
            modes: TransitModeOptions {
                max_access_distance_m: 100.0,
                max_egress_distance_m: 100.0,
                ..TransitModeOptions::default()
            },
            returns: TransitReturnOptions {
                include_geometry: true,
            },
        };
        let result = execute_transit_route(&bundle, &request).expect("route executes");
        assert_eq!(result.outcome, TransitOutcome::Scheduled);
        assert_eq!(result.summary.boarding_count, 1);
        assert_eq!(
            result
                .legs
                .iter()
                .filter(|leg| leg.leg_type == TransitLegType::Transit)
                .count(),
            1
        );
    }

    #[test]
    fn filters_disallowed_transit_modes() {
        let bundle = build_bundle_from_files(
            fixture_files(),
            "abc".to_string(),
            TransitImportOptions {
                name: "fixture".to_string(),
                source_label: "fixture".to_string(),
                service_start_date: "2026-05-11".to_string(),
                service_days: 7,
            },
        )
        .expect("fixture imports");
        let request = TransitRouteRequest {
            route_id: "r1".to_string(),
            origin: TransitPoint {
                id: "origin".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: TransitPoint {
                id: "dest".to_string(),
                lon: 6.02,
                lat: 53.0,
            },
            time: TransitQueryTime {
                datetime: "2026-05-11T08:00:00+02:00".to_string(),
                arrive_by: false,
                search_window_s: 3600,
            },
            modes: TransitModeOptions {
                transit: vec![TransitMode::Rail],
                max_access_distance_m: 100.0,
                max_egress_distance_m: 100.0,
                ..TransitModeOptions::default()
            },
            returns: TransitReturnOptions::default(),
        };
        let result = execute_transit_route(&bundle, &request).expect("route executes");
        assert_eq!(result.outcome, TransitOutcome::Unreachable);
    }

    fn fixture_files() -> GtfsFiles {
        let mut files = GtfsFiles::default();
        files.insert(
            "stops.txt",
            "stop_id,stop_name,stop_lat,stop_lon\nA,A,53.0,6.0\nB,B,53.0,6.01\nC,C,53.0,6.02\n"
                .to_string(),
        );
        files.insert(
            "routes.txt",
            "route_id,route_short_name,route_long_name,route_type\nR,1,Line 1,3\n".to_string(),
        );
        files.insert(
            "trips.txt",
            "route_id,service_id,trip_id,trip_headsign\nR,WEEK,T1,C\n".to_string(),
        );
        files.insert(
            "calendar.txt",
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nWEEK,1,1,1,1,1,1,1,20260501,20260531\n".to_string(),
        );
        files.insert(
            "calendar_dates.txt",
            "service_id,date,exception_type\n".to_string(),
        );
        files.insert(
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,08:10:00,08:10:30,A,1\nT1,08:15:00,08:15:30,B,2\nT1,08:20:00,08:20:00,C,3\n".to_string(),
        );
        files
    }
}
