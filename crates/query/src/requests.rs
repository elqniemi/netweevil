use std::collections::BTreeMap;
use std::path::PathBuf;

use netweevil_profile::ReturnConfig;
use serde::{Deserialize, Serialize};

use crate::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EngineMode {
    #[default]
    Auto,
    IgnoreMultiEdgeRestrictions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveEngineDescription {
    pub route_engine: &'static str,
    pub batch_engine: &'static str,
    pub acceleration: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRequest {
    pub route_id: String,
    pub origin: LabeledPoint,
    pub destination: LabeledPoint,
    #[serde(default)]
    pub snap: SnapOptions,
    #[serde(default)]
    pub connectivity: ConnectivityPolicy,
    #[serde(default)]
    pub fallback: FallbackPolicy,
    #[serde(default)]
    pub returns: ReturnConfig,
    #[serde(default)]
    pub alternatives: AlternativeRouteOptions,
    /// Exact time-dependent routing options. Flattening keeps request files
    /// ergonomic (`departure_time: ...`, `scenario: ...`) while grouping the
    /// runtime contract in Rust.
    #[serde(default, flatten)]
    pub temporal: TemporalRequestOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemporalRequestOptions {
    #[serde(default)]
    pub departure_time: Option<String>,
    #[serde(default)]
    pub arrive_by: Option<String>,
    #[serde(default)]
    pub scenario: Option<PathBuf>,
    #[serde(default)]
    pub holiday_calendar: Option<PathBuf>,
    #[serde(default, rename = "overlay")]
    pub overlays: Vec<PathBuf>,
    /// Per-state nondominated label guard for temporal/generalized searches.
    #[serde(default = "default_max_temporal_labels_per_state")]
    pub max_labels_per_state: usize,
    #[serde(default = "default_arrive_by_lookback_s")]
    pub arrive_by_lookback_s: f64,
    /// Hard budgets on named compiled cost components. These requests use
    /// exact label-setting rather than CCH.
    #[serde(default)]
    pub constraints: Vec<ComponentConstraint>,
    /// Optional generalized-cost/component Pareto frontier.
    #[serde(default)]
    pub pareto: Option<ParetoRouteOptions>,
}

impl Default for TemporalRequestOptions {
    fn default() -> Self {
        Self {
            departure_time: None,
            arrive_by: None,
            scenario: None,
            holiday_calendar: None,
            overlays: Vec::new(),
            max_labels_per_state: default_max_temporal_labels_per_state(),
            arrive_by_lookback_s: default_arrive_by_lookback_s(),
            constraints: Vec::new(),
            pareto: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentConstraint {
    pub component: String,
    pub max_value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParetoRouteOptions {
    pub component: String,
    #[serde(default = "default_pareto_max_routes")]
    pub max_routes: usize,
    #[serde(default = "default_pareto_max_labels_per_state")]
    pub max_labels_per_state: usize,
}

fn default_pareto_max_routes() -> usize {
    16
}

fn default_pareto_max_labels_per_state() -> usize {
    64
}

impl TemporalRequestOptions {
    pub fn is_temporal(&self) -> bool {
        self.departure_time.is_some()
            || self.arrive_by.is_some()
            || self.scenario.is_some()
            || !self.overlays.is_empty()
    }

    pub fn requires_exact_labels(&self) -> bool {
        self.is_temporal() || !self.constraints.is_empty() || self.pareto.is_some()
    }
}

fn default_max_temporal_labels_per_state() -> usize {
    16
}

fn default_arrive_by_lookback_s() -> f64 {
    86_400.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlternativeRouteOptions {
    #[serde(default = "default_alternative_max_routes")]
    pub max_routes: usize,
    #[serde(default = "default_alternative_max_cost_ratio")]
    pub max_cost_ratio: f64,
    #[serde(default)]
    pub max_extra_time_s: Option<f64>,
    #[serde(default)]
    pub max_extra_distance_m: Option<u64>,
    #[serde(default = "default_alternative_min_jaccard_distance")]
    pub min_jaccard_distance: f64,
    /// Upper bound on banned-edge searches while collecting alternatives.
    /// Each attempt is a full unaccelerated route query, so long best paths
    /// would otherwise trigger thousands of searches.
    #[serde(default = "default_alternative_max_search_attempts")]
    pub max_search_attempts: usize,
}

impl Default for AlternativeRouteOptions {
    fn default() -> Self {
        Self {
            max_routes: default_alternative_max_routes(),
            max_cost_ratio: default_alternative_max_cost_ratio(),
            max_extra_time_s: None,
            max_extra_distance_m: None,
            min_jaccard_distance: default_alternative_min_jaccard_distance(),
            max_search_attempts: default_alternative_max_search_attempts(),
        }
    }
}

fn default_alternative_max_routes() -> usize {
    1
}

fn default_alternative_max_cost_ratio() -> f64 {
    1.35
}

fn default_alternative_min_jaccard_distance() -> f64 {
    0.2
}

fn default_alternative_max_search_attempts() -> usize {
    24
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabeledPoint {
    pub id: String,
    pub lon: f64,
    pub lat: f64,
    /// Optional elevation in the dataset's vertical datum, in metres.
    #[serde(default)]
    pub z: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapOptions {
    #[serde(default = "default_snap_distance")]
    pub max_distance_m: f64,
    /// When the input point has Z, reject network positions farther away in
    /// elevation. This prevents street-level points snapping to stacked
    /// tunnels, footbridges, or station levels.
    #[serde(default)]
    pub z_window_m: Option<f64>,
    /// Semantic/source feature attributes that candidate edges must match.
    #[serde(default)]
    pub attribute_filters: BTreeMap<String, String>,
}

impl Default for SnapOptions {
    fn default() -> Self {
        Self {
            max_distance_m: default_snap_distance(),
            z_window_m: None,
            attribute_filters: BTreeMap::new(),
        }
    }
}

fn default_snap_distance() -> f64 {
    500.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DisconnectedNetworkMode {
    #[default]
    Strict,
    IgnoreUnreachable,
    HopOriginToNearestReachableComponent,
    HopDestinationToNearestReachableComponent,
    HopEitherEnd,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectivityPolicy {
    #[serde(default)]
    pub disconnected: DisconnectedNetworkMode,
    #[serde(default)]
    pub max_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub report_hop_distance_separately: bool,
}

impl Default for ConnectivityPolicy {
    fn default() -> Self {
        Self {
            disconnected: DisconnectedNetworkMode::Strict,
            max_hop_distance_m: None,
            report_hop_distance_separately: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct IllegalMovementPenaltyPolicy {
    #[serde(default)]
    pub reverse_oneway_penalty_s: Option<f64>,
    #[serde(default)]
    pub illegal_turn_penalty_s: Option<f64>,
    #[serde(default)]
    pub ignored_turn_restriction_penalty_s: Option<f64>,
    #[serde(default)]
    pub forbidden_uturn_penalty_s: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct FallbackPolicy {
    #[serde(default)]
    pub allow_reverse_oneway: bool,
    #[serde(default)]
    pub allow_illegal_turn: bool,
    #[serde(default)]
    pub ignore_turn_restrictions: bool,
    #[serde(default)]
    pub allow_uturn_where_normally_forbidden: bool,
    #[serde(default)]
    pub auto_relax_unreachable: bool,
    #[serde(default)]
    pub penalties: IllegalMovementPenaltyPolicy,
    #[serde(default)]
    pub max_illegal_distance_m: Option<f64>,
    #[serde(default)]
    pub max_illegal_turns: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAreaThresholdMetric {
    DistanceM,
    TravelTimeS,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAreaThreshold {
    #[serde(default)]
    pub id: Option<String>,
    pub limit: f64,
    pub metric: ServiceAreaThresholdMetric,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAreaOutputMode {
    Network,
    Polygon,
    #[default]
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAreaBandMode {
    #[default]
    Cumulative,
    Ring,
    #[serde(rename = "none")]
    Unbanded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAreaBoundaryMode {
    #[default]
    Overlap,
    CutAtBoundary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAreaMultiOriginMode {
    #[default]
    Merge,
    Overlap,
    Cut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAreaPolygonOptions {
    #[serde(default = "default_hull_aggressiveness")]
    pub hull_aggressiveness: f64,
    #[serde(default)]
    pub simplification_tolerance_m: Option<f64>,
    /// Grid cell size used to trace the concave polygon boundary. Smaller
    /// cells hug the network more tightly at higher cost; defaults to
    /// 25m x hull_aggressiveness (minimum 10m).
    #[serde(default)]
    pub cell_size_m: Option<f64>,
}

impl Default for ServiceAreaPolygonOptions {
    fn default() -> Self {
        Self {
            hull_aggressiveness: default_hull_aggressiveness(),
            simplification_tolerance_m: None,
            cell_size_m: None,
        }
    }
}

fn default_hull_aggressiveness() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAreaReturnOptions {
    #[serde(default = "default_true")]
    pub geometry: bool,
    #[serde(default = "default_true")]
    pub attributes: bool,
    #[serde(default = "default_true")]
    pub per_threshold_summary: bool,
    #[serde(default = "default_true")]
    pub diagnostics: bool,
    #[serde(default)]
    pub segments: bool,
    #[serde(default = "default_service_area_max_features")]
    pub max_features: usize,
    #[serde(default = "default_service_area_max_segments")]
    pub max_segments: usize,
    #[serde(default = "default_service_area_max_geometry_points")]
    pub max_geometry_points: usize,
}

impl Default for ServiceAreaReturnOptions {
    fn default() -> Self {
        Self {
            geometry: true,
            attributes: true,
            per_threshold_summary: true,
            diagnostics: true,
            segments: false,
            max_features: default_service_area_max_features(),
            max_segments: default_service_area_max_segments(),
            max_geometry_points: default_service_area_max_geometry_points(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_service_area_max_features() -> usize {
    250_000
}

fn default_service_area_max_segments() -> usize {
    250_000
}

fn default_service_area_max_geometry_points() -> usize {
    1_000_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAreaRequest {
    pub analysis_id: String,
    #[serde(default)]
    pub origins: Vec<LabeledPoint>,
    #[serde(default)]
    pub thresholds: Vec<ServiceAreaThreshold>,
    #[serde(default)]
    pub snap: SnapOptions,
    #[serde(default)]
    pub connectivity: ConnectivityPolicy,
    #[serde(default)]
    pub fallback: FallbackPolicy,
    #[serde(default)]
    pub output_mode: ServiceAreaOutputMode,
    #[serde(default)]
    pub band_mode: ServiceAreaBandMode,
    #[serde(default)]
    pub boundary_mode: ServiceAreaBoundaryMode,
    #[serde(default)]
    pub multi_origin_mode: ServiceAreaMultiOriginMode,
    #[serde(default)]
    pub polygon: ServiceAreaPolygonOptions,
    #[serde(default)]
    pub returns: ServiceAreaReturnOptions,
    #[serde(default, flatten)]
    pub temporal: TemporalRequestOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessibilityCategoryRequest {
    pub category_id: String,
    pub destinations: PointSetDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessibilityRequest {
    pub origins: PointSetDocument,
    #[serde(default)]
    pub categories: Vec<AccessibilityCategoryRequest>,
    #[serde(default)]
    pub thresholds_s: Vec<f64>,
    pub max_travel_time_s: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdAnalysisRequest {
    pub pairs_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixAnalysisRequest {
    pub origins_path: PathBuf,
    pub destinations_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentDocument {
    pub experiment: ExperimentHeader,
    #[serde(default)]
    pub scenarios: Vec<ScenarioSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentHeader {
    pub id: String,
    pub label: String,
    pub dataset: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisKind {
    Route,
    Od,
    Matrix,
    ServiceArea,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioSpec {
    #[serde(default)]
    pub id: Option<String>,
    pub profile: PathBuf,
    pub analysis: AnalysisKind,
    #[serde(default)]
    pub request: Option<PathBuf>,
    #[serde(default)]
    pub origins: Option<PathBuf>,
    #[serde(default)]
    pub destinations: Option<PathBuf>,
    #[serde(default)]
    pub out: Option<PathBuf>,
}
