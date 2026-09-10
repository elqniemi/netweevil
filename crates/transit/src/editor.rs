//! GTFS scenario editor: a compact, map-friendly description of transit lines
//! that builds either a complete GTFS feed from scratch or an overlay on top
//! of an imported feed (new lines, express variants, replaced routes).
//!
//! An overlay never touches the imported feed: its source archive, bundle and
//! manifest stay as they are, and the scenario builds a separate feed that can
//! be removed again. Scenario documents are plain JSON so they can be shared
//! and versioned like profiles.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::gtfs::{GtfsFiles, build_bundle_from_files, parse_iso_date};
use crate::model::{
    TRANSIT_BUNDLE_SCHEMA_VERSION, TransitBundle, TransitConnection, TransitImportOptions,
    TransitMode, TransitStop, TransitTrip,
};

pub const GTFS_SCENARIO_SCHEMA_VERSION: u32 = 1;

fn default_scenario_schema_version() -> u32 {
    GTFS_SCENARIO_SCHEMA_VERSION
}

fn default_speed_kph() -> f64 {
    30.0
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GtfsScenario {
    #[serde(default = "default_scenario_schema_version")]
    pub schema_version: u32,
    pub scenario_id: String,
    pub label: String,
    /// Imported feed this scenario extends. `None` builds a stand-alone feed
    /// from the scenario's own stops and lines.
    #[serde(default)]
    pub base_feed_id: Option<String>,
    /// Feed identifier of the built result; defaults to the scenario id.
    #[serde(default)]
    pub output_feed_id: Option<String>,
    #[serde(default)]
    pub agency: ScenarioAgency,
    /// First service date (`YYYY-MM-DD`) and window length. Overlays inherit
    /// the base feed's window and ignore these.
    #[serde(default)]
    pub service_start_date: Option<String>,
    #[serde(default)]
    pub service_days: Option<u32>,
    /// Stops created in the editor. Overlay lines may also reference base
    /// feed stops by their GTFS `stop_id`.
    #[serde(default)]
    pub stops: Vec<ScenarioStop>,
    #[serde(default)]
    pub lines: Vec<ScenarioLine>,
    /// Base feed routes whose trips are dropped from the built feed, so a new
    /// line can replace an existing one.
    #[serde(default)]
    pub removed_route_ids: Vec<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioAgency {
    pub name: String,
    #[serde(default)]
    pub url: String,
    pub timezone: String,
}

impl Default for ScenarioAgency {
    fn default() -> Self {
        Self {
            name: "Scenario agency".to_string(),
            url: String::new(),
            timezone: "Europe/Amsterdam".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioStop {
    pub stop_id: String,
    pub name: String,
    pub lon: f64,
    pub lat: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioLine {
    pub line_id: String,
    pub short_name: String,
    #[serde(default)]
    pub long_name: String,
    pub mode: TransitMode,
    /// Hex colour without `#`, as in GTFS `route_color`.
    #[serde(default)]
    pub color: Option<String>,
    /// Headsign for the listed direction; the last stop name when absent.
    #[serde(default)]
    pub headsign: Option<String>,
    pub stops: Vec<ScenarioLineStop>,
    /// Speed used to derive segment running times when a stop has no
    /// explicit `travel_s`.
    #[serde(default = "default_speed_kph")]
    pub average_speed_kph: f64,
    #[serde(default)]
    pub default_dwell_s: u32,
    /// Also run trips in the reverse stop order.
    #[serde(default = "default_true")]
    pub bidirectional: bool,
    #[serde(default)]
    pub services: Vec<ScenarioService>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioLineStop {
    pub stop_id: String,
    /// Running time from the previous stop in seconds; derived from the
    /// straight-line distance and the line speed when absent.
    #[serde(default)]
    pub travel_s: Option<u32>,
    #[serde(default)]
    pub dwell_s: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioService {
    /// Monday through Sunday.
    pub days: [bool; 7],
    #[serde(default)]
    pub windows: Vec<ScenarioHeadwayWindow>,
    /// Explicit departures from the first stop as `HH:MM` (hours may exceed 24).
    #[serde(default)]
    pub departures: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioHeadwayWindow {
    pub start: String,
    pub end: String,
    pub headway_min: u32,
}

/// Counts of what a scenario expands to, for the editor status line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioSummary {
    pub stop_count: usize,
    pub line_count: usize,
    pub trip_count: usize,
    pub stop_time_count: usize,
}

#[derive(Debug, Clone)]
pub struct ScenarioBuildOptions {
    pub feed_id: String,
    pub source_label: String,
    pub service_start_date: String,
    pub service_days: u32,
}

/// Generated GTFS text files, in feed order.
pub type GtfsTextFiles = Vec<(&'static str, String)>;

impl TransitMode {
    pub fn gtfs_route_type(self) -> u32 {
        match self {
            Self::Tram => 0,
            Self::Subway => 1,
            Self::Rail => 2,
            Self::Bus => 3,
            Self::Ferry => 4,
            Self::CableCar => 5,
            Self::Gondola => 6,
            Self::Funicular => 7,
            Self::Coach => 200,
            Self::Air => 1100,
            Self::Other => 1700,
        }
    }
}

impl GtfsScenario {
    pub fn output_feed_id(&self) -> &str {
        self.output_feed_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or(&self.scenario_id)
    }

    /// Checks identifiers, references and schedules. `base_stops` supplies
    /// the stops an overlay may reference from its base feed.
    pub fn validate(&self, base_stops: Option<&HashMap<String, TransitStop>>) -> Result<()> {
        if self.schema_version != GTFS_SCENARIO_SCHEMA_VERSION {
            bail!(
                "scenario schema version {} is not supported (expected {})",
                self.schema_version,
                GTFS_SCENARIO_SCHEMA_VERSION
            );
        }
        validate_identifier("scenario_id", &self.scenario_id)?;
        validate_identifier("output_feed_id", self.output_feed_id())?;
        if self.agency.name.trim().is_empty() {
            bail!("agency name must not be empty");
        }
        self.agency
            .timezone
            .parse::<chrono_tz::Tz>()
            .with_context(|| format!("invalid agency timezone '{}'", self.agency.timezone))?;
        if self.base_feed_id.is_none() {
            let start = self
                .service_start_date
                .as_deref()
                .context("a stand-alone scenario needs service_start_date")?;
            parse_iso_date(start)?;
            if self.service_days.unwrap_or(0) == 0 {
                bail!("a stand-alone scenario needs service_days greater than zero");
            }
        }
        let mut stop_ids = HashSet::new();
        for stop in &self.stops {
            validate_identifier("stop_id", &stop.stop_id)?;
            if !stop_ids.insert(stop.stop_id.as_str()) {
                bail!("duplicate scenario stop id '{}'", stop.stop_id);
            }
            if !(-180.0..=180.0).contains(&stop.lon) || !(-90.0..=90.0).contains(&stop.lat) {
                bail!(
                    "stop '{}' has coordinates outside the valid range",
                    stop.stop_id
                );
            }
            if let Some(base) = base_stops
                && base.contains_key(&stop.stop_id)
            {
                bail!(
                    "scenario stop id '{}' collides with a stop of the base feed",
                    stop.stop_id
                );
            }
        }
        let mut line_ids = HashSet::new();
        for line in &self.lines {
            validate_identifier("line_id", &line.line_id)?;
            if !line_ids.insert(line.line_id.as_str()) {
                bail!("duplicate line id '{}'", line.line_id);
            }
            if line.short_name.trim().is_empty() && line.long_name.trim().is_empty() {
                bail!("line '{}' needs a short or long name", line.line_id);
            }
            if line.stops.len() < 2 {
                bail!("line '{}' needs at least two stops", line.line_id);
            }
            if line.average_speed_kph <= 0.0 || !line.average_speed_kph.is_finite() {
                bail!("line '{}' needs a positive average speed", line.line_id);
            }
            for stop in &line.stops {
                let known = stop_ids.contains(stop.stop_id.as_str())
                    || base_stops.is_some_and(|base| base.contains_key(&stop.stop_id));
                if !known {
                    bail!(
                        "line '{}' references unknown stop '{}'",
                        line.line_id,
                        stop.stop_id
                    );
                }
            }
            for pair in line.stops.windows(2) {
                if pair[0].stop_id == pair[1].stop_id {
                    bail!(
                        "line '{}' lists stop '{}' twice in a row",
                        line.line_id,
                        pair[0].stop_id
                    );
                }
            }
            if line.services.is_empty() {
                bail!(
                    "line '{}' has no service (days and departures)",
                    line.line_id
                );
            }
            for (index, service) in line.services.iter().enumerate() {
                if !service.days.iter().any(|day| *day) {
                    bail!(
                        "line '{}' service {} has no operating days",
                        line.line_id,
                        index + 1
                    );
                }
                if service.windows.is_empty() && service.departures.is_empty() {
                    bail!(
                        "line '{}' service {} has neither headway windows nor departures",
                        line.line_id,
                        index + 1
                    );
                }
                for window in &service.windows {
                    let start = parse_clock(&window.start)?;
                    let end = parse_clock(&window.end)?;
                    if window.headway_min == 0 {
                        bail!("line '{}' has a zero headway window", line.line_id);
                    }
                    if end < start {
                        bail!(
                            "line '{}' has a headway window ending before it starts",
                            line.line_id
                        );
                    }
                }
                for departure in &service.departures {
                    parse_clock(departure)?;
                }
            }
        }
        if self.base_feed_id.is_none() && !self.removed_route_ids.is_empty() {
            bail!("removed_route_ids only apply to scenarios that extend a base feed");
        }
        Ok(())
    }

    /// Expands the scenario into GTFS text files. `base_stops` provides the
    /// coordinates of referenced base feed stops so the archive stands alone.
    pub fn gtfs_files(
        &self,
        base_stops: Option<&HashMap<String, TransitStop>>,
    ) -> Result<GtfsTextFiles> {
        self.validate(base_stops)?;
        let (start_date, days) = self.service_window()?;
        let end_date = start_date + time::Duration::days(i64::from(days.max(1)) - 1);

        let mut lookup = HashMap::<String, (String, f64, f64)>::new();
        for stop in &self.stops {
            lookup.insert(
                stop.stop_id.clone(),
                (stop.name.clone(), stop.lon, stop.lat),
            );
        }
        if let Some(base) = base_stops {
            for line in &self.lines {
                for stop in &line.stops {
                    if !lookup.contains_key(&stop.stop_id)
                        && let Some(found) = base.get(&stop.stop_id)
                    {
                        lookup.insert(
                            stop.stop_id.clone(),
                            (found.name.clone(), found.lon, found.lat),
                        );
                    }
                }
            }
        }

        let agency_url = if self.agency.url.trim().is_empty() {
            "https://example.invalid".to_string()
        } else {
            self.agency.url.trim().to_string()
        };
        let mut agency = String::from("agency_id,agency_name,agency_url,agency_timezone\n");
        push_row(
            &mut agency,
            &["SCN", &self.agency.name, &agency_url, &self.agency.timezone],
        );

        let mut used_stop_ids = Vec::new();
        let mut seen = HashSet::new();
        for line in &self.lines {
            for stop in &line.stops {
                if seen.insert(stop.stop_id.as_str()) {
                    used_stop_ids.push(stop.stop_id.as_str());
                }
            }
        }
        for stop in &self.stops {
            if seen.insert(stop.stop_id.as_str()) {
                used_stop_ids.push(stop.stop_id.as_str());
            }
        }
        let mut stops = String::from("stop_id,stop_name,stop_lat,stop_lon\n");
        for stop_id in used_stop_ids {
            let (name, lon, lat) = &lookup[stop_id];
            push_row(
                &mut stops,
                &[stop_id, name, &format!("{lat:.7}"), &format!("{lon:.7}")],
            );
        }

        let mut routes = String::from(
            "route_id,agency_id,route_short_name,route_long_name,route_type,route_color\n",
        );
        let mut calendar = String::from(
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n",
        );
        let mut trips = String::from("route_id,service_id,trip_id,trip_headsign,direction_id\n");
        let mut stop_times =
            String::from("trip_id,arrival_time,departure_time,stop_id,stop_sequence\n");
        let start_yyyymmdd = gtfs_date(start_date);
        let end_yyyymmdd = gtfs_date(end_date);

        for line in &self.lines {
            push_row(
                &mut routes,
                &[
                    &line.line_id,
                    "SCN",
                    &line.short_name,
                    &line.long_name,
                    &line.mode.gtfs_route_type().to_string(),
                    line.color.as_deref().unwrap_or("").trim_start_matches('#'),
                ],
            );
            let forward = line_timing(line, &lookup);
            let directions: Vec<(u8, Vec<TimedStop>)> = if line.bidirectional {
                vec![(0, forward.clone()), (1, reverse_timing(&forward))]
            } else {
                vec![(0, forward)]
            };
            for (service_index, service) in line.services.iter().enumerate() {
                let service_id = format!("{}_s{}", line.line_id, service_index + 1);
                let mut row = vec![service_id.clone()];
                row.extend(service.days.iter().map(|day| {
                    if *day {
                        "1".to_string()
                    } else {
                        "0".to_string()
                    }
                }));
                row.push(start_yyyymmdd.clone());
                row.push(end_yyyymmdd.clone());
                push_row(
                    &mut calendar,
                    &row.iter().map(String::as_str).collect::<Vec<_>>(),
                );
                let departures = service_departures(service)?;
                for (direction, timed) in &directions {
                    let headsign = match direction {
                        0 => line.headsign.clone().unwrap_or_else(|| {
                            timed
                                .last()
                                .map(|stop| stop.name.clone())
                                .unwrap_or_default()
                        }),
                        _ => timed
                            .last()
                            .map(|stop| stop.name.clone())
                            .unwrap_or_default(),
                    };
                    for (trip_index, departure_s) in departures.iter().enumerate() {
                        let trip_id = format!(
                            "{}_s{}_d{}_{:04}",
                            line.line_id,
                            service_index + 1,
                            direction,
                            trip_index + 1
                        );
                        push_row(
                            &mut trips,
                            &[
                                &line.line_id,
                                &service_id,
                                &trip_id,
                                &headsign,
                                &direction.to_string(),
                            ],
                        );
                        for (sequence, stop) in timed.iter().enumerate() {
                            push_row(
                                &mut stop_times,
                                &[
                                    &trip_id,
                                    &gtfs_time(departure_s + stop.arrival_offset_s),
                                    &gtfs_time(departure_s + stop.departure_offset_s),
                                    &stop.stop_id,
                                    &(sequence + 1).to_string(),
                                ],
                            );
                        }
                    }
                }
            }
        }

        Ok(vec![
            ("agency.txt", agency),
            ("stops.txt", stops),
            ("routes.txt", routes),
            ("calendar.txt", calendar),
            ("trips.txt", trips),
            ("stop_times.txt", stop_times),
        ])
    }

    /// Counts of the expanded schedule without building a bundle.
    pub fn summary(
        &self,
        base_stops: Option<&HashMap<String, TransitStop>>,
    ) -> Result<ScenarioSummary> {
        let files = self.gtfs_files(base_stops)?;
        let count = |name: &str| {
            files
                .iter()
                .find(|(file, _)| *file == name)
                .map(|(_, text)| text.lines().count().saturating_sub(1))
                .unwrap_or(0)
        };
        Ok(ScenarioSummary {
            stop_count: count("stops.txt"),
            line_count: self.lines.len(),
            trip_count: count("trips.txt"),
            stop_time_count: count("stop_times.txt"),
        })
    }

    fn service_window(&self) -> Result<(time::Date, u32)> {
        let start = self
            .service_start_date
            .as_deref()
            .context("scenario has no service_start_date")?;
        Ok((
            parse_iso_date(start)?,
            self.service_days.unwrap_or(1).max(1),
        ))
    }
}

#[derive(Debug, Clone)]
struct TimedStop {
    stop_id: String,
    name: String,
    arrival_offset_s: u32,
    departure_offset_s: u32,
}

/// Running-time profile of the listed direction: cumulative offsets from the
/// first departure with per-stop dwell.
fn line_timing(
    line: &ScenarioLine,
    lookup: &HashMap<String, (String, f64, f64)>,
) -> Vec<TimedStop> {
    let mut timed = Vec::with_capacity(line.stops.len());
    let mut clock = 0_u32;
    for (index, stop) in line.stops.iter().enumerate() {
        let (name, lon, lat) = &lookup[&stop.stop_id];
        if index > 0 {
            let previous = &lookup[&line.stops[index - 1].stop_id];
            clock = clock.saturating_add(segment_travel_s(
                line, stop, previous.1, previous.2, *lon, *lat,
            ));
        }
        let arrival = clock;
        // Terminals do not dwell: a trip leaves its first stop at the
        // scheduled departure and ends on arrival at its last stop.
        let dwell = if index == 0 || index + 1 == line.stops.len() {
            0
        } else {
            stop.dwell_s.unwrap_or(line.default_dwell_s)
        };
        clock = clock.saturating_add(dwell);
        timed.push(TimedStop {
            stop_id: stop.stop_id.clone(),
            name: name.clone(),
            arrival_offset_s: arrival,
            departure_offset_s: clock,
        });
    }
    timed
}

/// The reverse direction reuses the forward segment times in reverse order
/// and the same dwell at each intermediate stop.
fn reverse_timing(forward: &[TimedStop]) -> Vec<TimedStop> {
    let n = forward.len();
    let segments = (1..n)
        .map(|index| forward[index].arrival_offset_s - forward[index - 1].departure_offset_s)
        .collect::<Vec<_>>();
    let dwells = forward
        .iter()
        .map(|stop| stop.departure_offset_s - stop.arrival_offset_s)
        .collect::<Vec<_>>();
    let mut timed = Vec::with_capacity(n);
    let mut clock = 0_u32;
    for (position, source) in forward.iter().rev().enumerate() {
        if position > 0 {
            clock += segments[n - 1 - position];
        }
        let arrival = clock;
        let dwell = if position == 0 || position + 1 == n {
            0
        } else {
            dwells[n - 1 - position]
        };
        clock += dwell;
        timed.push(TimedStop {
            stop_id: source.stop_id.clone(),
            name: source.name.clone(),
            arrival_offset_s: arrival,
            departure_offset_s: clock,
        });
    }
    timed
}

fn segment_travel_s(
    line: &ScenarioLine,
    stop: &ScenarioLineStop,
    from_lon: f64,
    from_lat: f64,
    to_lon: f64,
    to_lat: f64,
) -> u32 {
    if let Some(travel) = stop.travel_s {
        return travel;
    }
    let distance_m = haversine_m(from_lon, from_lat, to_lon, to_lat);
    let seconds = distance_m / (line.average_speed_kph * 1000.0 / 3600.0);
    seconds.round().max(1.0) as u32
}

pub fn haversine_m(lon_a: f64, lat_a: f64, lon_b: f64, lat_b: f64) -> f64 {
    let (lat_a, lat_b) = (lat_a.to_radians(), lat_b.to_radians());
    let d_lat = lat_b - lat_a;
    let d_lon = (lon_b - lon_a).to_radians();
    let h = (d_lat / 2.0).sin().powi(2) + lat_a.cos() * lat_b.cos() * (d_lon / 2.0).sin().powi(2);
    2.0 * 6_371_008.8 * h.sqrt().asin()
}

fn service_departures(service: &ScenarioService) -> Result<Vec<u32>> {
    let mut departures = Vec::new();
    for window in &service.windows {
        let start = parse_clock(&window.start)?;
        let end = parse_clock(&window.end)?;
        let headway = window.headway_min * 60;
        let mut t = start;
        while t <= end {
            departures.push(t);
            t += headway;
        }
    }
    for departure in &service.departures {
        departures.push(parse_clock(departure)?);
    }
    departures.sort_unstable();
    departures.dedup();
    Ok(departures)
}

/// Parses `HH:MM` or `HH:MM:SS`; hours may exceed 24 for after-midnight trips.
pub fn parse_clock(raw: &str) -> Result<u32> {
    let parts = raw
        .trim()
        .split(':')
        .map(|part| part.trim().parse::<u32>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("invalid time '{raw}', expected HH:MM"))?;
    match parts.as_slice() {
        [hours, minutes] | [hours, minutes, _] if *minutes < 60 => {
            let seconds = parts.get(2).copied().unwrap_or(0);
            if seconds >= 60 {
                bail!("invalid time '{raw}', expected HH:MM");
            }
            Ok(hours * 3600 + minutes * 60 + seconds)
        }
        _ => bail!("invalid time '{raw}', expected HH:MM"),
    }
}

fn gtfs_time(seconds: u32) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

fn gtfs_date(date: time::Date) -> String {
    format!(
        "{:04}{:02}{:02}",
        date.year(),
        date.month() as u8,
        date.day()
    )
}

fn validate_identifier(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    if value.contains(['/', '\\', '\n', '\r', ',', '"']) {
        bail!("{field} '{value}' must not contain slashes, commas, quotes or line breaks");
    }
    Ok(())
}

fn push_row(out: &mut String, fields: &[&str]) {
    let mut first = true;
    for field in fields {
        if !first {
            out.push(',');
        }
        first = false;
        if field.contains([',', '"', '\n', '\r']) {
            out.push('"');
            out.push_str(&field.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(field);
        }
    }
    out.push('\n');
}

pub fn gtfs_files_sha256(files: &GtfsTextFiles) -> String {
    let mut hasher = Sha256::new();
    for (name, text) in files {
        hasher.update(name.as_bytes());
        hasher.update(text.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Writes the generated files as a GTFS zip archive.
pub fn write_gtfs_zip(path: impl AsRef<Path>, files: &GtfsTextFiles) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, text) in files {
        writer
            .start_file(*name, options)
            .with_context(|| format!("adding {name} to {}", path.display()))?;
        writer.write_all(text.as_bytes())?;
    }
    writer.finish().context("finishing GTFS zip")?;
    Ok(())
}

/// Builds the routable bundle of a scenario: stand-alone from its own files,
/// or merged on top of `base` with the base's service window and timezone.
pub fn build_scenario_bundle(
    scenario: &GtfsScenario,
    base: Option<&TransitBundle>,
    options: &ScenarioBuildOptions,
) -> Result<TransitBundle> {
    let base_stops = base.map(|bundle| {
        bundle
            .stops
            .iter()
            .map(|stop| (stop.stop_id.clone(), stop.clone()))
            .collect::<HashMap<_, _>>()
    });
    let mut effective = scenario.clone();
    if let Some(base) = base {
        effective.agency.timezone = base.agency_timezone.clone();
        effective.service_start_date = base.service_dates.first().cloned();
        effective.service_days = Some(base.service_dates.len() as u32);
    }
    let files = effective.gtfs_files(base_stops.as_ref())?;
    let sha256 = gtfs_files_sha256(&files);
    let mut gtfs = GtfsFiles::default();
    for (name, text) in &files {
        gtfs.insert(name, text.clone());
    }
    let (service_start_date, service_days) = match base {
        Some(base) => (
            base.service_dates.first().cloned().unwrap_or_default(),
            base.service_dates.len() as u32,
        ),
        None => (options.service_start_date.clone(), options.service_days),
    };
    let overlay = build_bundle_from_files(
        gtfs,
        sha256,
        TransitImportOptions {
            name: options.feed_id.clone(),
            source_label: options.source_label.clone(),
            service_start_date,
            service_days,
        },
    )?;
    match base {
        None => Ok(overlay),
        Some(base) => merge_overlay(base, overlay, &scenario.removed_route_ids, options),
    }
}

fn merge_overlay(
    base: &TransitBundle,
    overlay: TransitBundle,
    removed_route_ids: &[String],
    options: &ScenarioBuildOptions,
) -> Result<TransitBundle> {
    if base.time_origin_unix_s != overlay.time_origin_unix_s
        || base.service_dates != overlay.service_dates
        || base.agency_timezone != overlay.agency_timezone
    {
        bail!("scenario overlay does not share the base feed's service window and timezone");
    }
    let base_stop_index = base
        .stops
        .iter()
        .enumerate()
        .map(|(index, stop)| (stop.stop_id.as_str(), index as u32))
        .collect::<HashMap<_, _>>();
    let mut stops = base.stops.clone();
    let stop_map = overlay
        .stops
        .iter()
        .map(|stop| {
            if let Some(index) = base_stop_index.get(stop.stop_id.as_str()) {
                *index
            } else {
                stops.push(stop.clone());
                (stops.len() - 1) as u32
            }
        })
        .collect::<Vec<_>>();

    let removed = removed_route_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut removed_route_indexes = HashSet::new();
    for (index, route) in base.routes.iter().enumerate() {
        if removed.contains(route.route_id.as_str()) {
            removed_route_indexes.insert(index as u32);
        }
    }
    for route_id in &removed {
        if !base.routes.iter().any(|route| route.route_id == *route_id) {
            bail!(
                "removed route '{route_id}' does not exist in base feed '{}'",
                base.feed_id
            );
        }
    }

    let route_offset = base.routes.len() as u32;
    let trip_offset = base.trips.len() as u32;
    let shape_offset = base.shapes.len() as u32;
    let run_offset = base
        .connections
        .iter()
        .map(|connection| connection.run_index)
        .max()
        .map_or(0, |max| max + 1);

    let mut routes = base.routes.clone();
    routes.extend(overlay.routes.iter().cloned());
    let mut shapes = base.shapes.clone();
    shapes.extend(overlay.shapes.iter().cloned());
    let mut trips = base.trips.clone();
    trips.extend(overlay.trips.iter().map(|trip| TransitTrip {
        trip_id: trip.trip_id.clone(),
        route_index: trip.route_index + route_offset,
        headsign: trip.headsign.clone(),
        shape_index: trip.shape_index.map(|index| index + shape_offset),
    }));
    let mut connections = base
        .connections
        .iter()
        .filter(|connection| !removed_route_indexes.contains(&connection.route_index))
        .copied()
        .collect::<Vec<_>>();
    connections.extend(
        overlay
            .connections
            .iter()
            .map(|connection| TransitConnection {
                trip_index: connection.trip_index + trip_offset,
                run_index: connection.run_index + run_offset,
                pickup_allowed: connection.pickup_allowed,
                drop_off_allowed: connection.drop_off_allowed,
                route_index: connection.route_index + route_offset,
                from_stop_index: stop_map[connection.from_stop_index as usize],
                to_stop_index: stop_map[connection.to_stop_index as usize],
                departure_s: connection.departure_s,
                arrival_s: connection.arrival_s,
            }),
    );
    connections.sort_by_key(|connection| connection.departure_s);

    let mut hasher = Sha256::new();
    hasher.update(base.source_sha256.as_bytes());
    hasher.update(overlay.source_sha256.as_bytes());
    hasher.update(removed_route_ids.join(",").as_bytes());
    let mut import_diagnostics = base.import_diagnostics.clone();
    import_diagnostics.push(format!(
        "scenario overlay on feed '{}': {} added routes, {} added trips, {} removed base routes",
        base.feed_id,
        overlay.routes.len(),
        overlay.trips.len(),
        removed_route_indexes.len()
    ));
    Ok(TransitBundle {
        schema_version: TRANSIT_BUNDLE_SCHEMA_VERSION,
        feed_id: options.feed_id.clone(),
        source_label: options.source_label.clone(),
        source_sha256: hex::encode(hasher.finalize()),
        stop_binding_sha256: base.stop_binding_sha256.clone(),
        service_dates: base.service_dates.clone(),
        agency_timezone: base.agency_timezone.clone(),
        time_origin_unix_s: base.time_origin_unix_s,
        stops,
        routes,
        trips,
        shapes,
        connections,
        transfer_rules: base.transfer_rules.clone(),
        import_diagnostics,
    })
}

/// One stop of a route pattern with its typical offset from the first departure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitPatternStop {
    pub stop_id: String,
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    pub arrival_offset_s: u32,
    pub departure_offset_s: u32,
}

/// A distinct stop sequence served by a route, with how many vehicle runs use it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitRoutePattern {
    pub route_id: String,
    pub headsign: String,
    pub run_count: usize,
    pub stops: Vec<TransitPatternStop>,
}

/// Groups the vehicle runs of a route by their stop sequence, most common
/// first. This is the starting point for express variants of an existing line.
pub fn route_patterns(
    bundle: &TransitBundle,
    route_id: &str,
    limit: usize,
) -> Vec<TransitRoutePattern> {
    let Some(route_index) = bundle
        .routes
        .iter()
        .position(|route| route.route_id == route_id)
        .map(|index| index as u32)
    else {
        return Vec::new();
    };
    let mut runs = BTreeMap::<u32, Vec<&TransitConnection>>::new();
    for connection in &bundle.connections {
        if connection.route_index == route_index {
            runs.entry(connection.run_index)
                .or_default()
                .push(connection);
        }
    }
    struct PatternAccumulator {
        count: usize,
        headsign: String,
        arrivals: Vec<u32>,
        departures: Vec<u32>,
    }
    let mut patterns = HashMap::<Vec<u32>, PatternAccumulator>::new();
    for connections in runs.values_mut() {
        connections.sort_by_key(|connection| connection.departure_s);
        let Some(first) = connections.first() else {
            continue;
        };
        let mut sequence = vec![first.from_stop_index];
        let mut arrivals = vec![0_u32];
        let mut departures = vec![0_u32];
        let origin = first.departure_s;
        for connection in connections.iter() {
            sequence.push(connection.to_stop_index);
            arrivals.push(connection.arrival_s - origin);
            departures.push(connection.arrival_s - origin);
        }
        // The departure from an intermediate stop is the next connection's departure.
        for (position, connection) in connections.iter().enumerate().skip(1) {
            departures[position] = connection.departure_s - origin;
        }
        let entry = patterns
            .entry(sequence)
            .or_insert_with(|| PatternAccumulator {
                count: 0,
                headsign: bundle
                    .trips
                    .get(first.trip_index as usize)
                    .map(|trip| trip.headsign.clone())
                    .unwrap_or_default(),
                arrivals: arrivals.clone(),
                departures: departures.clone(),
            });
        entry.count += 1;
    }
    let mut output = patterns
        .into_iter()
        .map(|(sequence, accumulator)| TransitRoutePattern {
            route_id: route_id.to_string(),
            headsign: accumulator.headsign,
            run_count: accumulator.count,
            stops: sequence
                .iter()
                .enumerate()
                .map(|(position, stop_index)| {
                    let stop = &bundle.stops[*stop_index as usize];
                    TransitPatternStop {
                        stop_id: stop.stop_id.clone(),
                        name: stop.name.clone(),
                        lon: stop.lon,
                        lat: stop.lat,
                        arrival_offset_s: accumulator.arrivals[position],
                        departure_offset_s: accumulator.departures[position],
                    }
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    output.sort_by(|a, b| {
        b.run_count
            .cmp(&a.run_count)
            .then_with(|| a.headsign.cmp(&b.headsign))
    });
    output.truncate(limit);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario() -> GtfsScenario {
        GtfsScenario {
            schema_version: GTFS_SCENARIO_SCHEMA_VERSION,
            scenario_id: "lelylijn".to_string(),
            label: "Lelylijn".to_string(),
            base_feed_id: None,
            output_feed_id: None,
            agency: ScenarioAgency::default(),
            service_start_date: Some("2026-05-11".to_string()),
            service_days: Some(7),
            stops: vec![
                ScenarioStop {
                    stop_id: "gn".into(),
                    name: "Groningen".into(),
                    lon: 6.5665,
                    lat: 53.2107,
                },
                ScenarioStop {
                    stop_id: "hv".into(),
                    name: "Heerenveen".into(),
                    lon: 5.9167,
                    lat: 52.9600,
                },
                ScenarioStop {
                    stop_id: "ly".into(),
                    name: "Lelystad".into(),
                    lon: 5.4744,
                    lat: 52.5079,
                },
            ],
            lines: vec![ScenarioLine {
                line_id: "LL".into(),
                short_name: "LL".into(),
                long_name: "Lelylijn".into(),
                mode: TransitMode::Rail,
                color: Some("0E7C86".into()),
                headsign: None,
                stops: vec![
                    ScenarioLineStop {
                        stop_id: "gn".into(),
                        travel_s: None,
                        dwell_s: None,
                    },
                    ScenarioLineStop {
                        stop_id: "hv".into(),
                        travel_s: Some(1200),
                        dwell_s: Some(60),
                    },
                    ScenarioLineStop {
                        stop_id: "ly".into(),
                        travel_s: None,
                        dwell_s: None,
                    },
                ],
                average_speed_kph: 160.0,
                default_dwell_s: 30,
                bidirectional: true,
                services: vec![ScenarioService {
                    days: [true, true, true, true, true, false, false],
                    windows: vec![ScenarioHeadwayWindow {
                        start: "06:00".into(),
                        end: "07:00".into(),
                        headway_min: 30,
                    }],
                    departures: vec!["23:30".into()],
                }],
            }],
            removed_route_ids: Vec::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn expands_headways_and_both_directions() {
        let scenario = scenario();
        let files = scenario.gtfs_files(None).unwrap();
        let trips = &files
            .iter()
            .find(|(name, _)| *name == "trips.txt")
            .unwrap()
            .1;
        // 3 headway departures + 1 explicit, times two directions.
        assert_eq!(trips.lines().count() - 1, 8);
        let stop_times = &files
            .iter()
            .find(|(name, _)| *name == "stop_times.txt")
            .unwrap()
            .1;
        assert!(stop_times.contains("LL_s1_d0_0001,06:00:00,06:00:00,gn,1"));
        assert!(stop_times.contains("LL_s1_d0_0001,06:20:00,06:21:00,hv,2"));
        // Reverse direction reuses the forward segment times in reverse order.
        assert!(stop_times.contains("LL_s1_d1_0002,06:30:00,06:30:00,ly,1"));
        let summary = scenario.summary(None).unwrap();
        assert_eq!(summary.trip_count, 8);
        assert_eq!(summary.stop_count, 3);
    }

    #[test]
    fn builds_a_stand_alone_bundle() {
        let scenario = scenario();
        let bundle = build_scenario_bundle(
            &scenario,
            None,
            &ScenarioBuildOptions {
                feed_id: "lelylijn".into(),
                source_label: "scenario".into(),
                service_start_date: "2026-05-11".into(),
                service_days: 7,
            },
        )
        .unwrap();
        assert_eq!(bundle.stops.len(), 3);
        assert_eq!(bundle.routes.len(), 1);
        assert_eq!(bundle.trips.len(), 8);
        // 5 weekdays x 8 trips x 2 connections.
        assert_eq!(bundle.connections.len(), 80);
        let patterns = route_patterns(&bundle, "LL", 10);
        assert_eq!(patterns.len(), 2);
        assert_eq!(patterns[0].stops.len(), 3);
    }

    #[test]
    fn overlays_on_a_base_feed_and_removes_routes() {
        let base = build_scenario_bundle(
            &scenario(),
            None,
            &ScenarioBuildOptions {
                feed_id: "base".into(),
                source_label: "base".into(),
                service_start_date: "2026-05-11".into(),
                service_days: 7,
            },
        )
        .unwrap();
        let mut overlay = scenario();
        overlay.scenario_id = "express".into();
        overlay.base_feed_id = Some("base".into());
        overlay.stops = vec![ScenarioStop {
            stop_id: "dr".into(),
            name: "Drachten".into(),
            lon: 6.09,
            lat: 53.11,
        }];
        overlay.lines[0].line_id = "LLX".into();
        overlay.lines[0].stops = vec![
            ScenarioLineStop {
                stop_id: "gn".into(),
                travel_s: None,
                dwell_s: None,
            },
            ScenarioLineStop {
                stop_id: "dr".into(),
                travel_s: None,
                dwell_s: None,
            },
            ScenarioLineStop {
                stop_id: "ly".into(),
                travel_s: None,
                dwell_s: None,
            },
        ];
        overlay.removed_route_ids = vec!["LL".into()];
        let merged = build_scenario_bundle(
            &overlay,
            Some(&base),
            &ScenarioBuildOptions {
                feed_id: "base_express".into(),
                source_label: "scenario".into(),
                service_start_date: String::new(),
                service_days: 0,
            },
        )
        .unwrap();
        assert_eq!(merged.stops.len(), 4);
        assert_eq!(merged.routes.len(), 2);
        assert!(
            merged
                .connections
                .iter()
                .all(|connection| connection.route_index == 1)
        );
        assert_eq!(merged.connections.len(), 80);
        let gn = merged
            .stops
            .iter()
            .position(|stop| stop.stop_id == "gn")
            .unwrap() as u32;
        assert!(
            merged
                .connections
                .iter()
                .any(|connection| connection.from_stop_index == gn)
        );
        assert_eq!(merged.time_origin_unix_s, base.time_origin_unix_s);
    }

    #[test]
    fn rejects_unknown_stop_references() {
        let mut scenario = scenario();
        scenario.lines[0].stops[1].stop_id = "missing".into();
        let error = scenario.validate(None).unwrap_err().to_string();
        assert!(error.contains("unknown stop 'missing'"));
    }
}
