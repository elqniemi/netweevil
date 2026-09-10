use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use time::Date;
use zip::ZipArchive;

use crate::model::{
    TRANSIT_BUNDLE_SCHEMA_VERSION, TransitBundle, TransitConnection, TransitImportOptions,
    TransitImportSummary, TransitMode, TransitRoute, TransitShape, TransitStop, TransitTrip,
};

pub fn import_gtfs(path: impl AsRef<Path>, options: TransitImportOptions) -> Result<TransitBundle> {
    let path = path.as_ref();
    let source_sha256 = hash_gtfs_source(path)?;
    let files = read_gtfs_files(path)?;
    build_bundle_from_files(files, source_sha256, options)
}

pub fn write_transit_bundle(path: impl AsRef<Path>, bundle: &TransitBundle) -> Result<()> {
    let path = path.as_ref();
    if bundle.schema_version != TRANSIT_BUNDLE_SCHEMA_VERSION {
        bail!(
            "cannot write transit bundle schema version {}; expected {}",
            bundle.schema_version,
            TRANSIT_BUNDLE_SCHEMA_VERSION
        );
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    // Bincode writes individual scalars. Buffering avoids one filesystem call
    // per field for feeds with millions of timetable connections.
    let mut writer = BufWriter::with_capacity(1024 * 1024, file);
    bincode::serialize_into(&mut writer, bundle)
        .with_context(|| format!("serializing transit bundle {}", path.display()))?;
    writer
        .flush()
        .with_context(|| format!("flushing transit bundle {}", path.display()))
}

pub fn read_transit_bundle(path: impl AsRef<Path>) -> Result<TransitBundle> {
    let path = path.as_ref();
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let schema_version: u32 = bincode::deserialize_from(&mut bytes.as_slice())
        .with_context(|| format!("reading transit bundle header {}", path.display()))?;
    if schema_version != TRANSIT_BUNDLE_SCHEMA_VERSION {
        bail!(
            "transit bundle {} has schema version {}; expected {}; re-import the GTFS feed",
            path.display(),
            schema_version,
            TRANSIT_BUNDLE_SCHEMA_VERSION
        );
    }
    let bundle = bincode::deserialize(&bytes)
        .with_context(|| format!("parsing transit bundle {}", path.display()))?;
    Ok(bundle)
}

pub fn transit_import_summary(bundle: &TransitBundle) -> TransitImportSummary {
    TransitImportSummary {
        feed_id: bundle.feed_id.clone(),
        source_label: bundle.source_label.clone(),
        source_sha256: bundle.source_sha256.clone(),
        service_start_date: bundle.service_dates.first().cloned().unwrap_or_default(),
        service_days: bundle.service_dates.len() as u32,
        agency_timezone: bundle.agency_timezone.clone(),
        time_origin_unix_s: bundle.time_origin_unix_s,
        stop_count: bundle.stops.len(),
        route_count: bundle.routes.len(),
        trip_count: bundle.trips.len(),
        connection_count: bundle.connections.len(),
        transfer_rule_count: bundle.transfer_rules.len(),
        diagnostics: bundle.import_diagnostics.clone(),
    }
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
    files.require("agency.txt")?;
    files.require("stops.txt")?;
    files.require("routes.txt")?;
    files.require("trips.txt")?;
    files.require("stop_times.txt")?;
    Ok(files)
}

#[derive(Default)]
pub(crate) struct GtfsFiles {
    inner: HashMap<&'static str, String>,
}

impl GtfsFiles {
    fn names() -> [&'static str; 10] {
        [
            "agency.txt",
            "stops.txt",
            "routes.txt",
            "trips.txt",
            "stop_times.txt",
            "frequencies.txt",
            "transfers.txt",
            "shapes.txt",
            "calendar.txt",
            "calendar_dates.txt",
        ]
    }

    pub(crate) fn insert(&mut self, name: &'static str, text: String) {
        // UTF-8 BOMs are common in feeds exported by spreadsheet/database
        // tooling. Strip only a leading marker; embedded U+FEFF is data.
        self.inner
            .insert(name, text.trim_start_matches('\u{feff}').to_string());
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

pub(crate) fn build_bundle_from_files(
    files: GtfsFiles,
    source_sha256: String,
    options: TransitImportOptions,
) -> Result<TransitBundle> {
    let service_dates = service_date_window(&options.service_start_date, options.service_days)?;
    files.require("agency.txt")?;
    let agency_timezone = parse_agency_timezone(files.get("agency.txt").unwrap())?;
    let (time_origin_unix_s, service_day_offsets_s) =
        crate::timetable_time::service_time_basis(&agency_timezone, &service_dates)?;
    let active_services = active_services_by_date(&files, &service_dates)?;
    let (stops, stop_by_id) = parse_stops(files.get("stops.txt").unwrap())?;
    let (routes, route_by_id) = parse_routes(files.get("routes.txt").unwrap())?;
    let (shapes, shape_by_id) = files
        .get("shapes.txt")
        .map(parse_shapes)
        .transpose()?
        .unwrap_or_default();
    let (trips, trip_by_id, retained_gtfs_trips) = parse_trips(
        files.get("trips.txt").unwrap(),
        &route_by_id,
        &shape_by_id,
        &active_services,
    )?;
    let connections = parse_connections(
        files.get("stop_times.txt").unwrap(),
        files.get("frequencies.txt"),
        &stop_by_id,
        &trip_by_id,
        &trips,
        &retained_gtfs_trips,
        &service_day_offsets_s,
    )?;

    let transfer_rules = files
        .get("transfers.txt")
        .map(|raw| {
            crate::transfer_rules::parse_transfer_rules(
                raw,
                files.get("stops.txt").unwrap(),
                files.get("trips.txt").unwrap(),
                &stop_by_id,
                &route_by_id,
                &trip_by_id,
            )
        })
        .transpose()?
        .unwrap_or_default();
    let import_diagnostics = if transfer_rules
        .iter()
        .any(|rule| rule.rule_type == crate::model::TransitTransferType::Timed)
    {
        vec![crate::transfer_rules::TIMED_TRANSFER_DIAGNOSTIC.to_string()]
    } else {
        Vec::new()
    };

    Ok(TransitBundle {
        schema_version: TRANSIT_BUNDLE_SCHEMA_VERSION,
        feed_id: options.name,
        source_label: options.source_label,
        source_sha256,
        stop_binding_sha256: None,
        agency_timezone,
        time_origin_unix_s,
        service_dates: service_dates
            .iter()
            .map(|date| date.to_string())
            .collect::<Vec<_>>(),
        stops,
        routes,
        trips,
        shapes,
        connections,
        transfer_rules,
        import_diagnostics,
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
            binding: None,
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

fn parse_shapes(raw: &str) -> Result<(Vec<TransitShape>, HashMap<String, u32>)> {
    let mut reader = csv::Reader::from_reader(raw.as_bytes());
    let headers = reader.headers()?.clone();
    let shape_id = header_index(&headers, "shape_id")?;
    let lat = header_index(&headers, "shape_pt_lat")?;
    let lon = header_index(&headers, "shape_pt_lon")?;
    let sequence = header_index(&headers, "shape_pt_sequence")?;
    let mut rows_by_shape = BTreeMap::<String, Vec<(u32, [f64; 2])>>::new();
    for record in reader.records() {
        let record = record?;
        let id = record.get(shape_id).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let lon = record.get(lon).unwrap_or_default().parse::<f64>()?;
        let lat = record.get(lat).unwrap_or_default().parse::<f64>()?;
        let sequence = record
            .get(sequence)
            .unwrap_or_default()
            .parse::<u32>()
            .unwrap_or_default();
        rows_by_shape
            .entry(id.to_string())
            .or_default()
            .push((sequence, [lon, lat]));
    }

    let mut shapes = Vec::new();
    let mut shape_by_id = HashMap::new();
    for (shape_id, mut rows) in rows_by_shape {
        rows.sort_by_key(|(sequence, _)| *sequence);
        let points = rows.into_iter().map(|(_, point)| point).collect::<Vec<_>>();
        if points.len() < 2 {
            continue;
        }
        shape_by_id.insert(shape_id.clone(), shapes.len() as u32);
        shapes.push(TransitShape { shape_id, points });
    }
    Ok((shapes, shape_by_id))
}

fn parse_trips(
    raw: &str,
    route_by_id: &HashMap<String, u32>,
    shape_by_id: &HashMap<String, u32>,
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
    let shape_id = optional_header_index(&headers, "shape_id");
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
            shape_index: shape_id
                .and_then(|index| record.get(index))
                .and_then(|id| shape_by_id.get(id))
                .copied(),
        };
        trip_by_id.insert(gtfs_trip_id.clone(), trips.len() as u32);
        retained.insert(gtfs_trip_id, service_dates.clone());
        trips.push(trip);
    }
    Ok((trips, trip_by_id, retained))
}

fn parse_agency_timezone(raw: &str) -> Result<String> {
    let mut reader = csv::Reader::from_reader(raw.as_bytes());
    let timezone_column = header_index(reader.headers()?, "agency_timezone")?;
    let mut timezone = None::<String>;
    for record in reader.records() {
        let record = record?;
        let name = record.get(timezone_column).unwrap_or_default().trim();
        let parsed: chrono_tz::Tz = name
            .parse()
            .with_context(|| format!("invalid agency_timezone '{name}'"))?;
        if let Some(known) = timezone.as_ref()
            && known != parsed.name()
        {
            bail!("all agencies in one GTFS feed must use the same agency_timezone");
        }
        timezone = Some(parsed.name().to_string());
    }
    timezone.context("GTFS agency.txt has no agency rows")
}

fn parse_connections(
    raw: &str,
    frequencies_raw: Option<&str>,
    stop_by_id: &HashMap<String, u32>,
    trip_by_id: &HashMap<String, u32>,
    trips: &[TransitTrip],
    retained_gtfs_trips: &HashMap<String, Vec<u32>>,
    service_day_offsets_s: &[u32],
) -> Result<Vec<TransitConnection>> {
    let mut reader = csv::Reader::from_reader(raw.as_bytes());
    let headers = reader.headers()?.clone();
    let trip_id = header_index(&headers, "trip_id")?;
    let arrival_time = header_index(&headers, "arrival_time")?;
    let departure_time = header_index(&headers, "departure_time")?;
    let stop_id = header_index(&headers, "stop_id")?;
    let sequence = header_index(&headers, "stop_sequence")?;
    let pickup_type = headers.iter().position(|header| header == "pickup_type");
    let drop_off_type = headers.iter().position(|header| header == "drop_off_type");
    let mut stop_times_by_trip = BTreeMap::<u32, Vec<StopTimeRow>>::new();
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
            arrival_s: parse_optional_gtfs_time(record.get(arrival_time).unwrap_or_default())?,
            departure_s: parse_optional_gtfs_time(record.get(departure_time).unwrap_or_default())?,
            pickup_allowed: parse_pickup_drop_off(pickup_type.and_then(|index| record.get(index)))?,
            drop_off_allowed: parse_pickup_drop_off(
                drop_off_type.and_then(|index| record.get(index)),
            )?,
        };
        stop_times_by_trip.entry(trip_index).or_default().push(row);
    }
    let frequency_windows = match frequencies_raw {
        Some(raw) => parse_frequencies(raw, trip_by_id)?,
        None => HashMap::new(),
    };
    let mut connections = Vec::new();
    let mut next_run_index = 0_u32;
    for (trip_index, mut rows) in stop_times_by_trip {
        rows.sort_by_key(|row| row.sequence);
        let trip = &trips[trip_index as usize];
        interpolate_stop_times(&trip.trip_id, &mut rows)?;
        let date_offsets = retained_gtfs_trips
            .get(&trip.trip_id)
            .cloned()
            .unwrap_or_default();
        let anchor_departure_s = rows
            .first()
            .and_then(|row| row.departure_s)
            .unwrap_or_default();
        let starts = if let Some(windows) = frequency_windows.get(&trip_index) {
            windows
                .iter()
                .flat_map(|window| {
                    (window.start_s..window.end_s).step_by(window.headway_s as usize)
                })
                .collect::<Vec<_>>()
        } else {
            vec![anchor_departure_s]
        };
        for date_offset in date_offsets {
            let base = service_day_offsets_s[date_offset as usize];
            for &start_s in &starts {
                let run_index = next_run_index;
                next_run_index = next_run_index
                    .checked_add(1)
                    .context("GTFS has too many vehicle runs")?;
                for pair in rows.windows(2) {
                    let from = pair[0];
                    let to = pair[1];
                    let from_departure_s = from.departure_s.unwrap_or_default();
                    let to_arrival_s = to.arrival_s.unwrap_or_default();
                    if to_arrival_s < from_departure_s {
                        bail!("GTFS trip '{}' has decreasing stop times", trip.trip_id);
                    }
                    connections.push(TransitConnection {
                        trip_index,
                        run_index,
                        pickup_allowed: from.pickup_allowed,
                        drop_off_allowed: to.drop_off_allowed,
                        route_index: trip.route_index,
                        from_stop_index: from.stop_index,
                        to_stop_index: to.stop_index,
                        departure_s: base
                            .saturating_add(start_s)
                            .saturating_add(from_departure_s.saturating_sub(anchor_departure_s)),
                        arrival_s: base
                            .saturating_add(start_s)
                            .saturating_add(to_arrival_s.saturating_sub(anchor_departure_s)),
                    });
                }
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
    arrival_s: Option<u32>,
    departure_s: Option<u32>,
    pickup_allowed: bool,
    drop_off_allowed: bool,
}

fn parse_pickup_drop_off(raw: Option<&str>) -> Result<bool> {
    match raw.unwrap_or_default().trim() {
        "" | "0" => Ok(true),
        // Booking and driver coordination are not represented in requests.
        "1" | "2" | "3" => Ok(false),
        value => bail!("invalid GTFS pickup/drop-off type '{value}'; expected 0, 1, 2 or 3"),
    }
}

/// Fills GTFS non-timepoint rows by linearly interpolating between successive
/// timed rows. The HK feed has explicit first/last stop times and blank
/// interiors; multiple timing points and endpoint dwell times are retained.
fn interpolate_stop_times(trip_id: &str, rows: &mut [StopTimeRow]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    for row in rows.iter_mut() {
        match (row.arrival_s, row.departure_s) {
            (Some(arrival), None) => row.departure_s = Some(arrival),
            (None, Some(departure)) => row.arrival_s = Some(departure),
            _ => {}
        }
    }
    let anchors = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| row.arrival_s.zip(row.departure_s).map(|_| index))
        .collect::<Vec<_>>();
    if anchors.first().copied() != Some(0) || anchors.last().copied() != Some(rows.len() - 1) {
        bail!(
            "GTFS trip '{trip_id}' has blank stop times outside timed endpoints; only intermediate blanks can be interpolated"
        );
    }

    for anchors in anchors.windows(2) {
        let from_index = anchors[0];
        let to_index = anchors[1];
        if to_index == from_index + 1 {
            continue;
        }
        let from_s = rows[from_index].departure_s.unwrap_or_default();
        let to_s = rows[to_index].arrival_s.unwrap_or_default();
        if to_s < from_s {
            bail!(
                "GTFS trip '{trip_id}' has decreasing timing points at stop_sequence {} and {}",
                rows[from_index].sequence,
                rows[to_index].sequence
            );
        }
        let steps = (to_index - from_index) as u64;
        let duration = (to_s - from_s) as u64;
        for (relative_index, row) in rows[(from_index + 1)..to_index].iter_mut().enumerate() {
            let step = relative_index as u64 + 1;
            // Round to the nearest second while preserving monotonicity.
            let interpolated = from_s as u64 + (duration * step + steps / 2) / steps;
            let interpolated = u32::try_from(interpolated)
                .with_context(|| format!("interpolating GTFS trip '{trip_id}'"))?;
            row.arrival_s = Some(interpolated);
            row.departure_s = Some(interpolated);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct FrequencyWindow {
    start_s: u32,
    end_s: u32,
    headway_s: u32,
}

fn parse_frequencies(
    raw: &str,
    trip_by_id: &HashMap<String, u32>,
) -> Result<HashMap<u32, Vec<FrequencyWindow>>> {
    let mut reader = csv::Reader::from_reader(raw.as_bytes());
    let headers = reader.headers()?.clone();
    let trip_id = header_index(&headers, "trip_id")?;
    let start_time = header_index(&headers, "start_time")?;
    let end_time = header_index(&headers, "end_time")?;
    let headway_secs = header_index(&headers, "headway_secs")?;
    let mut windows = HashMap::<u32, Vec<FrequencyWindow>>::new();
    for record in reader.records() {
        let record = record?;
        let Some(&trip_index) = trip_by_id.get(record.get(trip_id).unwrap_or_default()) else {
            continue;
        };
        let headway_s = record
            .get(headway_secs)
            .unwrap_or_default()
            .parse::<u32>()
            .unwrap_or_default();
        if headway_s == 0 {
            continue;
        }
        let start_s = parse_gtfs_time(record.get(start_time).unwrap_or_default())?;
        let end_s = parse_gtfs_time(record.get(end_time).unwrap_or_default())?;
        if end_s <= start_s {
            continue;
        }
        windows
            .entry(trip_index)
            .or_default()
            .push(FrequencyWindow {
                start_s,
                end_s,
                headway_s,
            });
    }
    Ok(windows)
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
    for dates in service_dates_by_id.values_mut() {
        dates.sort_unstable();
        dates.dedup();
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
            "1" if !entry.contains(&offset) => {
                entry.push(offset);
                entry.sort_unstable();
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

pub(crate) fn parse_iso_date(raw: &str) -> Result<Date> {
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
    let raw = raw.trim();
    let parts = raw
        .split(':')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if parts.len() != 3 || parts[1] >= 60 || parts[2] >= 60 {
        bail!("invalid GTFS time '{raw}', expected HH:MM:SS");
    }
    parts[0]
        .checked_mul(3600)
        .and_then(|hours| hours.checked_add(parts[1] * 60 + parts[2]))
        .ok_or_else(|| anyhow!("GTFS time '{raw}' exceeds the supported range"))
}

fn parse_optional_gtfs_time(raw: &str) -> Result<Option<u32>> {
    if raw.trim().is_empty() {
        Ok(None)
    } else {
        parse_gtfs_time(raw).map(Some)
    }
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
