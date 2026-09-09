use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const TRANSIT_BUNDLE_SCHEMA_VERSION: u32 = 6;
pub const TRANSIT_STOP_BINDING_SCHEMA_VERSION: u32 = 1;
pub const TRANSIT_TRANSFER_TABLE_SCHEMA_VERSION: u32 = 1;

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
    pub agency_timezone: String,
    pub time_origin_unix_s: i64,
    pub stop_count: usize,
    pub route_count: usize,
    pub trip_count: usize,
    pub connection_count: usize,
    pub transfer_rule_count: usize,
    pub diagnostics: Vec<String>,
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
    pub agency_timezone: String,
    pub time_origin_unix_s: i64,
    pub stop_count: u64,
    pub route_count: u64,
    pub trip_count: u64,
    pub connection_count: u64,
    pub bundle_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_binding_source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_binding_sha256: Option<String>,
    #[serde(default)]
    pub bound_stop_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transfer_tables: Vec<TransitTransferTableManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitTransferTableManifest {
    pub profile_id: String,
    pub profile_hash: String,
    pub dataset_id: String,
    pub path: String,
    pub created_at: String,
    pub transfer_count: u64,
    pub max_transfer_distance_m: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitBundle {
    pub schema_version: u32,
    pub feed_id: String,
    pub source_label: String,
    pub source_sha256: String,
    #[serde(default)]
    pub stop_binding_sha256: Option<String>,
    pub service_dates: Vec<String>,
    pub agency_timezone: String,
    /// All departure_s/arrival_s values are elapsed UTC seconds from this instant.
    pub time_origin_unix_s: i64,
    pub stops: Vec<TransitStop>,
    pub routes: Vec<TransitRoute>,
    pub trips: Vec<TransitTrip>,
    pub shapes: Vec<TransitShape>,
    pub connections: Vec<TransitConnection>,
    pub transfer_rules: Vec<TransitTransferRule>,
    pub import_diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitTransferType {
    Recommended,
    Timed,
    MinimumTime,
    Forbidden,
}

/// A transfers.txt rule expanded to platform stop indexes. Trip selectors
/// take precedence over route selectors on their respective sides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitTransferRule {
    pub from_stop_index: u32,
    pub to_stop_index: u32,
    pub from_route_index: Option<u32>,
    pub to_route_index: Option<u32>,
    pub from_trip_index: Option<u32>,
    pub to_trip_index: Option<u32>,
    pub rule_type: TransitTransferType,
    pub min_transfer_time_s: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitStop {
    pub stop_id: String,
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    /// Optional explicit location on the street/indoor graph. Network-routing
    /// hosts consume this target; straight-line transit routing continues to
    /// use the GTFS stop coordinate.
    #[serde(default)]
    pub binding: Option<TransitStopBindingTarget>,
}

/// One stop-to-graph binding file. Bindings are intentionally feed-scoped so
/// stop identifiers from multiple imported feeds cannot be mixed silently.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitStopBindingTable {
    #[serde(default = "default_stop_binding_schema_version")]
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed_id: Option<String>,
    pub bindings: Vec<TransitStopBinding>,
}

fn default_stop_binding_schema_version() -> u32 {
    TRANSIT_STOP_BINDING_SCHEMA_VERSION
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitStopBinding {
    pub stop_id: String,
    #[serde(flatten)]
    pub target: TransitStopBindingTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitStopBindingSummary {
    pub feed_id: String,
    pub supplied_binding_count: usize,
    pub bound_stop_count: usize,
    pub unbound_stop_count: usize,
}

/// A stable graph target for a transit stop. Coordinate bindings retain an
/// open attribute filter so multilayer snaps can distinguish platforms
/// from geometrically coincident tunnels, concourses, or surface links.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitStopBindingTarget {
    Node {
        node_id: u32,
    },
    Edge {
        edge_id: u32,
        #[serde(default)]
        fraction: Option<f64>,
    },
    Coordinate {
        lon: f64,
        lat: f64,
        #[serde(default)]
        z: Option<f64>,
        /// Optional vertical tolerance used while resolving the coordinate to
        /// graph candidates. Without one, candidates nearest the requested Z
        /// are retained.
        #[serde(default)]
        z_window_m: Option<f64>,
        #[serde(default)]
        attribute_filter: BTreeMap<String, String>,
    },
}

/// Rich network path returned by a street/indoor router. This is persisted in
/// transfer tables and exposed on access, transfer, and egress legs so an
/// in-station walk is auditable instead of appearing as a teleport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitStreetPath {
    pub travel_time_s: u32,
    #[serde(default)]
    pub distance_m: Option<f64>,
    #[serde(default)]
    pub edge_path: Vec<u32>,
    #[serde(default)]
    pub geometry: Vec<[f64; 3]>,
    #[serde(default)]
    pub components: BTreeMap<String, f64>,
}

impl TransitStreetPath {
    pub fn time_only(travel_time_s: u32) -> Self {
        Self {
            travel_time_s,
            distance_m: None,
            edge_path: Vec::new(),
            geometry: Vec::new(),
            components: BTreeMap::new(),
        }
    }
}

/// Directed, profile-specific stop-to-stop network transfer table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitTransferTable {
    #[serde(default = "default_transfer_table_schema_version")]
    pub schema_version: u32,
    pub feed_id: String,
    pub source_sha256: String,
    #[serde(default)]
    pub stop_binding_sha256: Option<String>,
    pub dataset_id: String,
    pub profile_id: String,
    pub profile_hash: String,
    pub max_transfer_distance_m: f64,
    pub transfers: Vec<TransitNetworkTransfer>,
}

fn default_transfer_table_schema_version() -> u32 {
    TRANSIT_TRANSFER_TABLE_SCHEMA_VERSION
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitNetworkTransfer {
    pub from_stop_id: String,
    pub to_stop_id: String,
    /// Straight-line separation used to enforce request transfer limits. The
    /// actual network distance, when available, lives in `path.distance_m`.
    pub straight_line_distance_m: f64,
    pub path: TransitStreetPath,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitTransferBuildOptions {
    pub dataset_id: String,
    pub profile_id: String,
    pub profile_hash: String,
    #[serde(default = "default_transfer_distance_m")]
    pub max_transfer_distance_m: f64,
    #[serde(default = "default_transfer_build_candidates")]
    pub max_candidates_per_stop: usize,
}

impl Default for TransitTransferBuildOptions {
    fn default() -> Self {
        Self {
            dataset_id: String::new(),
            profile_id: "walk".to_string(),
            profile_hash: String::new(),
            max_transfer_distance_m: default_transfer_distance_m(),
            max_candidates_per_stop: default_transfer_build_candidates(),
        }
    }
}

fn default_transfer_build_candidates() -> usize {
    32
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
    pub shape_index: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitShape {
    pub shape_id: String,
    pub points: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TransitConnection {
    pub trip_index: u32,
    /// One vehicle run, distinct for every service date and frequency departure.
    pub run_index: u32,
    /// Regular scheduled pickup only; prohibited and arranged pickups are excluded.
    pub pickup_allowed: bool,
    /// Regular scheduled drop-off only; prohibited and arranged drop-offs are excluded.
    pub drop_off_allowed: bool,
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
    pub(crate) fn from_gtfs_route_type(value: &str) -> Self {
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    #[default]
    Walk,
    Bicycle,
    Car,
}

impl AccessMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Walk => "walk",
            Self::Bicycle => "bicycle",
            Self::Car => "car",
        }
    }

    pub fn speed_kph(self, modes: &TransitModeOptions) -> f64 {
        match self {
            Self::Walk => modes.walk_speed_kph,
            Self::Bicycle => modes.bicycle_speed_kph,
            Self::Car => modes.car_access_speed_kph,
        }
    }

    pub fn max_access_distance_m(self, modes: &TransitModeOptions) -> f64 {
        match self {
            Self::Walk => modes.max_access_distance_m,
            Self::Bicycle => modes.max_bicycle_access_distance_m,
            Self::Car => modes.max_car_access_distance_m,
        }
    }

    pub fn max_egress_distance_m(self, modes: &TransitModeOptions) -> f64 {
        match self {
            Self::Walk => modes.max_egress_distance_m,
            Self::Bicycle => modes.max_bicycle_egress_distance_m,
            Self::Car => modes.max_car_egress_distance_m,
        }
    }
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
    #[serde(default)]
    pub alternatives: TransitAlternativeOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitServiceAreaRequest {
    pub analysis_id: String,
    #[serde(default)]
    pub origins: Vec<TransitPoint>,
    pub time: TransitQueryTime,
    #[serde(default)]
    pub modes: TransitModeOptions,
    #[serde(default = "default_transit_service_area_max_travel_time_s")]
    pub max_travel_time_s: u32,
    #[serde(default)]
    pub returns: TransitServiceAreaReturnOptions,
}

fn default_transit_service_area_max_travel_time_s() -> u32 {
    60 * 60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitServiceAreaReturnOptions {
    #[serde(default = "default_true")]
    pub include_stops: bool,
    #[serde(default = "default_true")]
    pub include_stop_segments: bool,
    #[serde(default = "default_true")]
    pub include_geometry: bool,
    #[serde(default = "default_transit_service_area_max_stops")]
    pub max_stops: usize,
    #[serde(default = "default_transit_service_area_max_stop_segments")]
    pub max_stop_segments: usize,
    #[serde(default = "default_transit_service_area_max_geometry_points")]
    pub max_geometry_points: usize,
}

impl Default for TransitServiceAreaReturnOptions {
    fn default() -> Self {
        Self {
            include_stops: true,
            include_stop_segments: true,
            include_geometry: true,
            max_stops: default_transit_service_area_max_stops(),
            max_stop_segments: default_transit_service_area_max_stop_segments(),
            max_geometry_points: default_transit_service_area_max_geometry_points(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_transit_service_area_max_stops() -> usize {
    100_000
}

fn default_transit_service_area_max_stop_segments() -> usize {
    200_000
}

fn default_transit_service_area_max_geometry_points() -> usize {
    1_000_000
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitAlternativeOptions {
    #[serde(default = "default_transit_alternative_max_routes")]
    pub max_routes: usize,
    #[serde(default = "default_transit_alternative_max_time_ratio")]
    pub max_time_ratio: f64,
    #[serde(default)]
    pub max_extra_time_s: Option<u32>,
}

impl Default for TransitAlternativeOptions {
    fn default() -> Self {
        Self {
            max_routes: default_transit_alternative_max_routes(),
            max_time_ratio: default_transit_alternative_max_time_ratio(),
            max_extra_time_s: None,
        }
    }
}

fn default_transit_alternative_max_routes() -> usize {
    1
}

fn default_transit_alternative_max_time_ratio() -> f64 {
    1.5
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
    /// Advanced first/last-mile support: must be set to allow access and
    /// egress mode lists that differ from each other.
    #[serde(default)]
    pub mixed_access_egress: bool,
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
    #[serde(default = "default_bicycle_access_distance_m")]
    pub max_bicycle_access_distance_m: f64,
    #[serde(default = "default_bicycle_access_distance_m")]
    pub max_bicycle_egress_distance_m: f64,
    #[serde(default = "default_car_access_distance_m")]
    pub max_car_access_distance_m: f64,
    #[serde(default = "default_car_access_distance_m")]
    pub max_car_egress_distance_m: f64,
    #[serde(default = "default_transfer_distance_m")]
    pub max_transfer_distance_m: f64,
    /// Selects a precomputed, profile-specific network transfer table. When
    /// absent, transfers use straight-line walking distances.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_profile_id: Option<String>,
    #[serde(default = "default_board_slack_s")]
    pub board_slack_s: u32,
    #[serde(default = "default_transfer_slack_s")]
    pub transfer_slack_s: u32,
    #[serde(default = "default_max_transfers")]
    pub max_transfers: u8,
    #[serde(default)]
    pub min_transit_leg_duration_s: u32,
    #[serde(default)]
    pub min_transit_leg_distance_m: f64,
    /// How access and egress leg travel times are estimated. `network` uses
    /// a street routing engine when the host provides one (API/CLI with a
    /// matching street profile loaded). Candidates without a network path are
    /// unreachable; straight-line estimates are used only by `straight_line`.
    #[serde(default)]
    pub street_access: TransitStreetAccessModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransitStreetAccessModel {
    #[default]
    StraightLine,
    Network,
}

impl Default for TransitModeOptions {
    fn default() -> Self {
        Self {
            access: default_access_modes(),
            egress: default_access_modes(),
            mixed_access_egress: false,
            transit: default_transit_modes(),
            walk_speed_kph: default_walk_speed_kph(),
            bicycle_speed_kph: default_bicycle_speed_kph(),
            car_access_speed_kph: default_car_access_speed_kph(),
            max_access_distance_m: default_access_distance_m(),
            max_egress_distance_m: default_access_distance_m(),
            max_bicycle_access_distance_m: default_bicycle_access_distance_m(),
            max_bicycle_egress_distance_m: default_bicycle_access_distance_m(),
            max_car_access_distance_m: default_car_access_distance_m(),
            max_car_egress_distance_m: default_car_access_distance_m(),
            max_transfer_distance_m: default_transfer_distance_m(),
            transfer_profile_id: None,
            board_slack_s: default_board_slack_s(),
            transfer_slack_s: default_transfer_slack_s(),
            max_transfers: default_max_transfers(),
            min_transit_leg_duration_s: 0,
            min_transit_leg_distance_m: 0.0,
            street_access: TransitStreetAccessModel::default(),
        }
    }
}

impl TransitModeOptions {
    /// Deduplicated access modes; errors when the list is empty.
    pub fn validated_access_modes(&self) -> Result<Vec<AccessMode>> {
        let modes = dedup_access_modes(&self.access);
        if modes.is_empty() {
            bail!("modes.access must contain at least one access mode");
        }
        Ok(modes)
    }

    /// Deduplicated egress modes; errors when the list is empty or differs
    /// from the access list without `mixed_access_egress` enabled.
    pub fn validated_egress_modes(&self) -> Result<Vec<AccessMode>> {
        let egress = dedup_access_modes(&self.egress);
        if egress.is_empty() {
            bail!("modes.egress must contain at least one egress mode");
        }
        if !self.mixed_access_egress {
            let access = dedup_access_modes(&self.access);
            let mut access_sorted = access.clone();
            let mut egress_sorted = egress.clone();
            access_sorted.sort_by_key(|mode| mode.label());
            egress_sorted.sort_by_key(|mode| mode.label());
            if access_sorted != egress_sorted {
                bail!(
                    "access modes {:?} and egress modes {:?} differ; set modes.mixed_access_egress to true to plan different first- and last-mile modes",
                    access.iter().map(|mode| mode.label()).collect::<Vec<_>>(),
                    egress.iter().map(|mode| mode.label()).collect::<Vec<_>>()
                );
            }
        }
        Ok(egress)
    }
}

fn dedup_access_modes(modes: &[AccessMode]) -> Vec<AccessMode> {
    let mut deduped = Vec::new();
    for &mode in modes {
        if !deduped.contains(&mode) {
            deduped.push(mode);
        }
    }
    deduped
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

fn default_bicycle_access_distance_m() -> f64 {
    5_000.0
}

fn default_car_access_distance_m() -> f64 {
    15_000.0
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitReturnOptions {
    #[serde(default)]
    pub include_geometry: bool,
    #[serde(default)]
    pub walking_geometry: TransitWalkingGeometry,
    #[serde(default)]
    pub include_stops: bool,
    #[serde(default)]
    pub include_stop_segments: bool,
}

impl Default for TransitReturnOptions {
    fn default() -> Self {
        Self {
            include_geometry: false,
            walking_geometry: TransitWalkingGeometry::StraightLine,
            include_stops: false,
            include_stop_segments: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransitWalkingGeometry {
    #[default]
    StraightLine,
    Network,
}

/// Converts timetable-relative departure_s/arrival_s values to real instants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitTimeContext {
    pub agency_timezone: String,
    pub time_origin_unix_s: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitRouteResult {
    pub route_id: String,
    pub time_context: TransitTimeContext,
    pub outcome: TransitOutcome,
    pub summary: TransitRouteSummary,
    pub legs: Vec<TransitLeg>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stops: Vec<TransitRouteStop>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_segments: Vec<TransitRouteStopSegment>,
    pub diagnostics: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<TransitRouteAlternative>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitServiceAreaResult {
    pub analysis_id: String,
    pub time_context: TransitTimeContext,
    pub outcome: TransitOutcome,
    pub origin_count: usize,
    pub processed_origin_count: usize,
    pub skipped_origin_count: usize,
    pub max_travel_time_s: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stops: Vec<TransitServiceAreaStop>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_segments: Vec<TransitServiceAreaSegment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitServiceAreaStop {
    pub origin_id: String,
    pub stop_id: String,
    pub stop_name: String,
    pub lon: f64,
    pub lat: f64,
    pub arrival_s: u32,
    pub travel_time_s: u32,
    pub boarding_count: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_mode: Option<AccessMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitServiceAreaSegment {
    pub origin_id: String,
    pub from_stop_id: String,
    pub to_stop_id: String,
    pub from_stop_name: String,
    pub to_stop_name: String,
    pub departure_s: u32,
    pub arrival_s: u32,
    pub duration_s: u32,
    pub travel_time_s: u32,
    pub boarding_count: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<TransitMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_short_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trip_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub geometry: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitRouteAlternative {
    pub alternative_index: u32,
    pub rank: u32,
    pub summary: TransitRouteSummary,
    pub legs: Vec<TransitLeg>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stops: Vec<TransitRouteStop>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_segments: Vec<TransitRouteStopSegment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitOutcome {
    Scheduled,
    Unreachable,
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
    /// Street mode used for access, transfer, and egress legs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub street_mode: Option<AccessMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_short_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trip_id: Option<String>,
    /// Identifies the vehicle run inside this imported timetable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headsign: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub geometry: Vec<[f64; 3]>,
    /// Exact street/indoor path details when the host or a precomputed
    /// transfer table supplied them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_path: Option<TransitStreetPath>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitRouteStop {
    pub sequence: u32,
    pub stop_id: String,
    pub stop_name: String,
    pub lon: f64,
    pub lat: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arrival_s: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub departure_s: Option<u32>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitRouteStopSegment {
    pub segment_index: u32,
    pub from_stop_id: String,
    pub to_stop_id: String,
    pub from_stop_name: String,
    pub to_stop_name: String,
    pub departure_s: u32,
    pub arrival_s: u32,
    pub duration_s: u32,
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
    pub geometry: Vec<[f64; 3]>,
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
