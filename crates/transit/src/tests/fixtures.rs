//! GTFS feeds, import helpers, and request builders shared by the transit tests.
//!
//! Every feed uses Europe/Amsterdam; stops lie along latitude 53, mostly
//! 0.01 degrees (~670 m) apart. Requests run from `origin` (lon 6.0, stop A)
//! to `dest` (lon 6.02, stop C).

use std::path::PathBuf;
use std::sync::Arc;

pub(crate) use crate::gtfs::GtfsFiles;
use crate::gtfs::build_bundle_from_files;
use crate::*;

pub const SERVICE_START: &str = "2026-05-11";

const STOP_TIMES_HEADER: &str = "trip_id,arrival_time,departure_time,stop_id,stop_sequence";

/// Seconds after the first service day's origin for a wall-clock time.
pub const fn hm(hours: u32, minutes: u32) -> u32 {
    hours * 3600 + minutes * 60
}

/// CSV file contents from a header and rows.
pub fn csv(header: &str, rows: &[&str]) -> String {
    let mut contents = String::from(header);
    for row in rows {
        contents.push('\n');
        contents.push_str(row);
    }
    contents.push('\n');
    contents
}

pub fn stop_times(rows: &[&str]) -> String {
    csv(STOP_TIMES_HEADER, rows)
}

/// A `calendar.txt` with service `WEEK` running every day in the range.
pub fn calendar(start_date: &str, end_date: &str) -> String {
    csv(
        "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date",
        &[&format!("WEEK,1,1,1,1,1,1,1,{start_date},{end_date}")],
    )
}

fn agency() -> String {
    csv(
        "agency_id,agency_name,agency_url,agency_timezone",
        &["A,Fixture,https://example.test,Europe/Amsterdam"],
    )
}

/// A feed whose `WEEK` service runs every day of May 2026.
pub fn feed(stops: &[&str], routes: &[&str], trips: &[&str], stop_time_rows: &[&str]) -> GtfsFiles {
    let mut files = GtfsFiles::default();
    files.insert("agency.txt", agency());
    files.insert(
        "stops.txt",
        csv("stop_id,stop_name,stop_lat,stop_lon", stops),
    );
    files.insert(
        "routes.txt",
        csv(
            "route_id,route_short_name,route_long_name,route_type",
            routes,
        ),
    );
    files.insert(
        "trips.txt",
        csv("route_id,service_id,trip_id,trip_headsign", trips),
    );
    files.insert("calendar.txt", calendar("20260501", "20260531"));
    files.insert(
        "calendar_dates.txt",
        csv("service_id,date,exception_type", &[]),
    );
    files.insert("stop_times.txt", stop_times(stop_time_rows));
    files
}

const ABC_STOPS: [&str; 3] = ["A,A,53.0,6.0", "B,B,53.0,6.01", "C,C,53.0,6.02"];

/// One bus trip: A 08:10:30, B 08:15:00-08:15:30, C 08:20:00.
pub fn line_feed() -> GtfsFiles {
    feed(
        &ABC_STOPS,
        &["R,1,Line 1,3"],
        &["R,WEEK,T1,C"],
        &[
            "T1,08:10:00,08:10:30,A,1",
            "T1,08:15:00,08:15:30,B,2",
            "T1,08:20:00,08:20:00,C,3",
        ],
    )
}

/// `line_feed` with a wiggling shape between the stops.
pub fn shaped_feed() -> GtfsFiles {
    let mut files = line_feed();
    files.insert(
        "trips.txt",
        csv(
            "route_id,service_id,trip_id,trip_headsign,shape_id",
            &["R,WEEK,T1,C,S1"],
        ),
    );
    files.insert(
        "shapes.txt",
        csv(
            "shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence",
            &[
                "S1,53.0,6.0,1",
                "S1,53.001,6.005,2",
                "S1,53.0,6.01,3",
                "S1,52.999,6.015,4",
                "S1,53.0,6.02,5",
            ],
        ),
    );
    files
}

/// Metro "MTR" A -> B -> C (5 minutes per hop) leaving A every 10 minutes
/// from 08:00 to 08:30 through `frequencies.txt`.
pub fn frequency_feed() -> GtfsFiles {
    let mut files = feed(
        &ABC_STOPS,
        &["M,MTR,Metro,1"],
        &["M,WEEK,T1,C"],
        &[
            "T1,00:00:00,00:00:00,A,1",
            "T1,00:05:00,00:05:00,B,2",
            "T1,00:10:00,00:10:00,C,3",
        ],
    );
    files.insert(
        "frequencies.txt",
        csv(
            "trip_id,start_time,end_time,headway_secs",
            &["T1,08:00:00,08:31:00,600"],
        ),
    );
    files
}

/// Two rail lines meeting at a station: T1 runs A 08:05 -> B1 08:10, T2 runs
/// B2 08:15 -> C 08:25. Platforms B1 and B2 are ~7 m apart.
pub fn two_line_feed() -> GtfsFiles {
    feed(
        &[
            "A,A,53.0,6.0",
            "B1,Platform 1,53.0,6.01",
            "B2,Platform 2,53.0,6.0101",
            "C,C,53.0,6.02",
        ],
        &["R1,1,Line 1,1", "R2,2,Line 2,1"],
        &["R1,WEEK,T1,B1", "R2,WEEK,T2,C"],
        &[
            "T1,08:05:00,08:05:00,A,1",
            "T1,08:10:00,08:10:00,B1,2",
            "T2,08:15:00,08:15:00,B2,1",
            "T2,08:25:00,08:25:00,C,2",
        ],
    )
}

/// Byte-order marks, blank intermediate stop times, a sentinel calendar
/// overridden by calendar_dates, and after-midnight times.
pub fn hardened_feed() -> GtfsFiles {
    let mut files = GtfsFiles::default();
    files.insert("agency.txt", agency());
    files.insert(
        "stops.txt",
        csv(
            "\u{feff}stop_id,stop_name,stop_lat,stop_lon",
            &[
                "A,A,53.0,6.0",
                "B,B,53.0,6.01",
                "C,C,53.0,6.02",
                "D,D,53.0,6.03",
            ],
        ),
    );
    files.insert(
        "routes.txt",
        csv(
            "\u{feff}route_id,route_short_name,route_long_name,route_type",
            &["R,1,Night,3"],
        ),
    );
    files.insert(
        "trips.txt",
        csv(
            "\u{feff}route_id,service_id,trip_id,trip_headsign",
            &["R,WEEK,T1,D"],
        ),
    );
    files.insert(
        "calendar.txt",
        csv(
            "\u{feff}service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date",
            &["WEEK,1,0,0,0,0,0,0,20200101,20991231"],
        ),
    );
    files.insert(
        "calendar_dates.txt",
        csv(
            "\u{feff}service_id,date,exception_type",
            &["WEEK,20260511,2", "WEEK,20260512,1"],
        ),
    );
    files.insert(
        "stop_times.txt",
        csv(
            &format!("\u{feff}{STOP_TIMES_HEADER}"),
            &[
                "T1,25:00:00,25:00:00,A,1",
                "T1,,,B,2",
                "T1,,,C,3",
                "T1,26:00:00,26:00:00,D,4",
            ],
        ),
    );
    files
}

pub fn try_import(files: GtfsFiles, start_date: &str, days: u32) -> anyhow::Result<TransitBundle> {
    build_bundle_from_files(
        files,
        "fixture-sha256".to_string(),
        TransitImportOptions {
            name: "fixture".to_string(),
            source_label: "fixture".to_string(),
            service_start_date: start_date.to_string(),
            service_days: days,
        },
    )
}

/// Imports `days` service days starting on [`SERVICE_START`].
pub fn import(files: GtfsFiles, days: u32) -> TransitBundle {
    try_import(files, SERVICE_START, days).expect("fixture imports")
}

pub fn router(bundle: TransitBundle) -> PreparedTransitRouter {
    PreparedTransitRouter::new(Arc::new(bundle))
}

pub fn point(id: &str, lon: f64) -> TransitPoint {
    TransitPoint {
        id: id.to_string(),
        lon,
        lat: 53.0,
        z: None,
    }
}

/// 100 m access and egress radii: endpoints only reach their own stop.
pub fn short_access_modes() -> TransitModeOptions {
    TransitModeOptions {
        max_access_distance_m: 100.0,
        max_egress_distance_m: 100.0,
        ..TransitModeOptions::default()
    }
}

pub fn full_returns() -> TransitReturnOptions {
    TransitReturnOptions {
        include_geometry: true,
        include_stops: true,
        include_stop_segments: true,
        ..TransitReturnOptions::default()
    }
}

/// Origin -> dest with [`short_access_modes`] and a one-hour search window.
pub fn route_request(datetime: &str, arrive_by: bool) -> TransitRouteRequest {
    TransitRouteRequest {
        route_id: "route".to_string(),
        origin: point("origin", 6.0),
        destination: point("dest", 6.02),
        time: TransitQueryTime {
            datetime: datetime.to_string(),
            arrive_by,
            search_window_s: 3600,
        },
        modes: short_access_modes(),
        returns: TransitReturnOptions::default(),
        alternatives: TransitAlternativeOptions::default(),
    }
}

/// Bundle-relative seconds back to a request datetime on the first service day.
pub fn fixture_datetime(seconds: u32) -> String {
    format!(
        "{SERVICE_START}T{:02}:{:02}:{:02}+02:00",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

pub fn depart_at(seconds: u32) -> TransitRouteRequest {
    route_request(&fixture_datetime(seconds), false)
}

pub fn arrive_by(seconds: u32) -> TransitRouteRequest {
    route_request(&fixture_datetime(seconds), true)
}

/// Departs at `depart_s`, or arrives by `deadline_s` when `arrive_by` is set.
pub fn timed_request(arrive_by: bool, depart_s: u32, deadline_s: u32) -> TransitRouteRequest {
    let seconds = if arrive_by { deadline_s } else { depart_s };
    route_request(&fixture_datetime(seconds), arrive_by)
}

/// Stop catchment from `origin` with a one-hour travel budget.
pub fn stops_area(
    origin: TransitPoint,
    time: TransitQueryTime,
    modes: TransitModeOptions,
) -> TransitServiceAreaRequest {
    TransitServiceAreaRequest {
        catchment_mode: TransitCatchmentMode::Stops,
        analysis_id: "area".to_string(),
        origins: vec![origin],
        time,
        modes,
        max_travel_time_s: 3600,
        returns: TransitServiceAreaReturnOptions::default(),
    }
}

/// The service area equivalent of a route request: seeded at the destination
/// for arrive-by, otherwise at the origin.
pub fn area_for(request: &TransitRouteRequest) -> TransitServiceAreaRequest {
    let seed = if request.time.arrive_by {
        &request.destination
    } else {
        &request.origin
    };
    stops_area(seed.clone(), request.time.clone(), request.modes.clone())
}

/// The stop at the far end of an [`area_for`] catchment.
pub fn far_stop(arrive_by: bool) -> &'static str {
    if arrive_by { "A" } else { "C" }
}

pub fn area_stop<'a>(
    area: &'a TransitServiceAreaResult,
    stop_id: &str,
) -> Option<&'a TransitServiceAreaStop> {
    area.stops.iter().find(|stop| stop.stop_id == stop_id)
}

pub fn legs_of(result: &TransitRouteResult, leg_type: TransitLegType) -> Vec<&TransitLeg> {
    result
        .legs
        .iter()
        .filter(|leg| leg.leg_type == leg_type)
        .collect()
}

pub fn first_leg(result: &TransitRouteResult, leg_type: TransitLegType) -> &TransitLeg {
    legs_of(result, leg_type)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no {leg_type:?} leg"))
}

pub fn access_and_egress_legs(result: &TransitRouteResult) -> impl Iterator<Item = &TransitLeg> {
    result.legs.iter().filter(|leg| {
        matches!(
            leg.leg_type,
            TransitLegType::Access | TransitLegType::Egress
        )
    })
}

pub fn temp_path(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "netweevil-transit-{label}-{}-{nanos}.bin",
        std::process::id()
    ))
}

/// Coordinate-only street estimator with a fixed time per direction; `None`
/// means no street path in that direction.
pub struct FixedStreetTime {
    pub access_s: Option<u32>,
    pub egress_s: Option<u32>,
}

impl FixedStreetTime {
    pub fn both(seconds: u32) -> Self {
        Self {
            access_s: Some(seconds),
            egress_s: Some(seconds),
        }
    }
}

impl StreetTimeEstimator for FixedStreetTime {
    fn street_time_s(
        &self,
        _: AccessMode,
        egress: bool,
        _: f64,
        _: f64,
        _: f64,
        _: f64,
    ) -> Option<u32> {
        if egress { self.egress_s } else { self.access_s }
    }
}
