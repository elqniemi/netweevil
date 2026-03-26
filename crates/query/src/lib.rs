use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::error::Error as StdError;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use netan_core::{
    CompiledEdgeMetric, CompiledProfileBundle, DirectedEdge, EDGE_FLAG_ROUNDABOUT,
    EDGE_FLAG_TARGET_TRAFFIC_SIGNAL, RoadClass, SurfaceClass, TopologyBundle, TopologyNode,
};
use netan_profile::ReturnConfig;
use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabeledPoint {
    pub id: String,
    pub lon: f64,
    pub lat: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapOptions {
    #[serde(default = "default_snap_distance")]
    pub max_distance_m: f64,
}

impl Default for SnapOptions {
    fn default() -> Self {
        Self {
            max_distance_m: default_snap_distance(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisOutcome {
    #[default]
    Legal,
    Degraded,
    Partial,
    Unreachable,
    NotImplemented,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisDiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisDiagnosticCode {
    SnapNoCandidate,
    DisconnectedComponents,
    LegalRouteUnreachable,
    FallbackUsed,
    AnalysisNotImplemented,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisDiagnostic {
    pub code: AnalysisDiagnosticCode,
    pub severity: AnalysisDiagnosticSeverity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub point_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_ids: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_actions: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AnalysisFailure {
    pub message: String,
    pub outcome: AnalysisOutcome,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

impl AnalysisFailure {
    pub fn new(
        message: impl Into<String>,
        outcome: AnalysisOutcome,
        diagnostics: Vec<AnalysisDiagnostic>,
    ) -> Self {
        Self {
            message: message.into(),
            outcome,
            diagnostics,
        }
    }
}

impl fmt::Display for AnalysisFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for AnalysisFailure {}

pub fn analysis_failure(error: &anyhow::Error) -> Option<&AnalysisFailure> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<AnalysisFailure>())
}

fn route_snap_failure(point: &LabeledPoint, max_distance_m: f64) -> AnalysisFailure {
    AnalysisFailure::new(
        format!(
            "point '{}' has no traversable candidate node or edge within {:.1} m",
            point.id, max_distance_m
        ),
        AnalysisOutcome::Unreachable,
        vec![AnalysisDiagnostic {
            code: AnalysisDiagnosticCode::SnapNoCandidate,
            severity: AnalysisDiagnosticSeverity::Error,
            message: format!(
                "No traversable node or edge was found within {:.1} m for point '{}'.",
                max_distance_m, point.id
            ),
            point_ids: vec![point.id.clone()],
            component_ids: Vec::new(),
            suggested_actions: vec![
                "Increase snap.max_distance_m.".to_string(),
                "Move the point closer to the routable network.".to_string(),
            ],
        }],
    )
}

pub fn load_route_request(path: impl AsRef<Path>) -> Result<RouteRequest> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading route request {}", path.display()))?;
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => serde_json::from_str(&raw).context("parsing JSON route request"),
        Some("yaml") | Some("yml") => {
            serde_yaml::from_str(&raw).context("parsing YAML route request")
        }
        other => bail!(
            "unsupported route request extension {:?}; use .json, .yml, or .yaml",
            other
        ),
    }
}

pub fn load_service_area_request(path: impl AsRef<Path>) -> Result<ServiceAreaRequest> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading service-area request {}", path.display()))?;
    let request = match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => serde_json::from_str(&raw).context("parsing JSON service-area request")?,
        Some("yaml") | Some("yml") => {
            serde_yaml::from_str(&raw).context("parsing YAML service-area request")?
        }
        other => bail!(
            "unsupported service-area request extension {:?}; use .json, .yml, or .yaml",
            other
        ),
    };
    validate_service_area_request(request)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdPair {
    pub pair_id: String,
    pub origin: LabeledPoint,
    pub destination: LabeledPoint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdPairsDocument {
    #[serde(default)]
    pub pairs: Vec<OdPair>,
    #[serde(default)]
    pub snap: SnapOptions,
    #[serde(default)]
    pub connectivity: ConnectivityPolicy,
    #[serde(default)]
    pub fallback: FallbackPolicy,
    #[serde(default)]
    pub returns: ReturnConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointSetDocument {
    #[serde(default)]
    pub points: Vec<LabeledPoint>,
    #[serde(default)]
    pub snap: SnapOptions,
    #[serde(default)]
    pub connectivity: ConnectivityPolicy,
    #[serde(default)]
    pub fallback: FallbackPolicy,
    #[serde(default)]
    pub returns: ReturnConfig,
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
}

impl Default for ServiceAreaPolygonOptions {
    fn default() -> Self {
        Self {
            hull_aggressiveness: default_hull_aggressiveness(),
            simplification_tolerance_m: None,
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
}

impl Default for ServiceAreaReturnOptions {
    fn default() -> Self {
        Self {
            geometry: true,
            attributes: true,
            per_threshold_summary: true,
            diagnostics: true,
        }
    }
}

fn default_true() -> bool {
    true
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAreaGeometryType {
    Network,
    Polygon,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAreaFeature {
    #[serde(default)]
    pub origin_id: Option<String>,
    #[serde(default)]
    pub band_start_limit: Option<f64>,
    #[serde(default)]
    pub threshold_id: Option<String>,
    pub threshold_limit: f64,
    pub threshold_metric: ServiceAreaThresholdMetric,
    pub geometry_type: ServiceAreaGeometryType,
    #[serde(default)]
    pub fallback_used: bool,
    #[serde(default)]
    pub origin_component_id: Option<u32>,
    #[serde(default)]
    pub origin_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub reachable_network_length_m: Option<f64>,
    #[serde(default)]
    pub reachable_edge_count: Option<u64>,
    #[serde(default)]
    pub geometry: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAreaThresholdSummary {
    #[serde(default)]
    pub origin_id: Option<String>,
    #[serde(default)]
    pub band_start_limit: Option<f64>,
    #[serde(default)]
    pub threshold_id: Option<String>,
    pub threshold_limit: f64,
    pub threshold_metric: ServiceAreaThresholdMetric,
    #[serde(default)]
    pub fallback_used: bool,
    #[serde(default)]
    pub origin_component_id: Option<u32>,
    #[serde(default)]
    pub origin_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub reachable_network_length_m: Option<f64>,
    #[serde(default)]
    pub reachable_edge_count: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAreaResult {
    pub analysis_id: String,
    #[serde(default)]
    pub outcome: AnalysisOutcome,
    #[serde(default)]
    pub output_mode: ServiceAreaOutputMode,
    #[serde(default)]
    pub band_mode: ServiceAreaBandMode,
    #[serde(default)]
    pub boundary_mode: ServiceAreaBoundaryMode,
    #[serde(default)]
    pub multi_origin_mode: ServiceAreaMultiOriginMode,
    pub origin_count: usize,
    #[serde(default)]
    pub processed_origin_count: usize,
    #[serde(default)]
    pub skipped_origin_count: usize,
    #[serde(default)]
    pub fallback_origin_count: usize,
    pub threshold_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<ServiceAreaFeature>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summaries: Vec<ServiceAreaThresholdSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum OdPairsFile {
    Document(OdPairsDocument),
    Bare(Vec<OdPair>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum PointSetFile {
    Document(PointSetDocument),
    Bare(Vec<LabeledPoint>),
}

pub fn load_od_pairs(path: impl AsRef<Path>) -> Result<OdPairsDocument> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading OD pairs file {}", path.display()))?;
    let document = match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => {
            parse_structured_od_pairs(serde_json::from_str(&raw).context("parsing JSON OD pairs")?)
        }
        Some("yaml") | Some("yml") => {
            parse_structured_od_pairs(serde_yaml::from_str(&raw).context("parsing YAML OD pairs")?)
        }
        Some("csv") => load_od_pairs_csv(&raw)?,
        other => bail!(
            "unsupported OD pairs extension {:?}; use .json, .yml, .yaml, or .csv",
            other
        ),
    };
    if document.pairs.is_empty() {
        bail!("OD pairs document must contain at least one pair");
    }
    Ok(document)
}

pub fn load_point_set(path: impl AsRef<Path>) -> Result<PointSetDocument> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading point set {}", path.display()))?;
    let document = match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => parse_structured_point_set(
            serde_json::from_str(&raw).context("parsing JSON point set")?,
        ),
        Some("yaml") | Some("yml") => parse_structured_point_set(
            serde_yaml::from_str(&raw).context("parsing YAML point set")?,
        ),
        Some("csv") => load_point_set_csv(&raw)?,
        other => bail!(
            "unsupported point-set extension {:?}; use .json, .yml, .yaml, or .csv",
            other
        ),
    };
    if document.points.is_empty() {
        bail!("point set must contain at least one point");
    }
    Ok(document)
}

fn load_od_pairs_csv(raw: &str) -> Result<OdPairsDocument> {
    let mut rows = csv_rows(raw);
    let header = rows
        .next()
        .context("OD CSV must start with a header row: id,source_x,source_y,target_x,target_y")?;
    let id_index = header_index(&header, &["id", "pair_id"])?;
    let source_x_index = header_index(
        &header,
        &["source_x", "source_lon", "origin_x", "origin_lon"],
    )?;
    let source_y_index = header_index(
        &header,
        &["source_y", "source_lat", "origin_y", "origin_lat"],
    )?;
    let target_x_index = header_index(
        &header,
        &["target_x", "target_lon", "destination_x", "destination_lon"],
    )?;
    let target_y_index = header_index(
        &header,
        &["target_y", "target_lat", "destination_y", "destination_lat"],
    )?;

    let mut pairs = Vec::new();
    for (row_number, row) in rows.enumerate() {
        let pair_id = csv_field(&row, id_index, row_number + 2, "id")?.to_string();
        let source_x = parse_csv_f64(&row, source_x_index, row_number + 2, "source_x")?;
        let source_y = parse_csv_f64(&row, source_y_index, row_number + 2, "source_y")?;
        let target_x = parse_csv_f64(&row, target_x_index, row_number + 2, "target_x")?;
        let target_y = parse_csv_f64(&row, target_y_index, row_number + 2, "target_y")?;

        pairs.push(OdPair {
            pair_id: pair_id.clone(),
            origin: LabeledPoint {
                id: format!("{pair_id}:source"),
                lon: source_x,
                lat: source_y,
            },
            destination: LabeledPoint {
                id: format!("{pair_id}:target"),
                lon: target_x,
                lat: target_y,
            },
        });
    }

    Ok(OdPairsDocument {
        pairs,
        snap: SnapOptions::default(),
        connectivity: ConnectivityPolicy::default(),
        fallback: FallbackPolicy::default(),
        returns: ReturnConfig::default(),
    })
}

fn load_point_set_csv(raw: &str) -> Result<PointSetDocument> {
    let mut rows = csv_rows(raw);
    let header = rows
        .next()
        .context("point-set CSV must start with a header row: id,x,y")?;
    let id_index = header_index(&header, &["id"])?;
    let x_index = header_index(&header, &["x", "lon", "longitude"])?;
    let y_index = header_index(&header, &["y", "lat", "latitude"])?;

    let mut points = Vec::new();
    for (row_number, row) in rows.enumerate() {
        points.push(LabeledPoint {
            id: csv_field(&row, id_index, row_number + 2, "id")?.to_string(),
            lon: parse_csv_f64(&row, x_index, row_number + 2, "x")?,
            lat: parse_csv_f64(&row, y_index, row_number + 2, "y")?,
        });
    }

    Ok(PointSetDocument {
        points,
        snap: SnapOptions::default(),
        connectivity: ConnectivityPolicy::default(),
        fallback: FallbackPolicy::default(),
        returns: ReturnConfig::default(),
    })
}

fn header_index(header: &[String], accepted: &[&str]) -> Result<usize> {
    header
        .iter()
        .position(|value| {
            accepted
                .iter()
                .any(|candidate| value.eq_ignore_ascii_case(candidate))
        })
        .with_context(|| {
            format!(
                "missing required CSV column; expected one of {}",
                accepted.join(", ")
            )
        })
}

fn csv_field<'a>(
    row: &'a [String],
    index: usize,
    row_number: usize,
    column_name: &str,
) -> Result<&'a str> {
    row.get(index)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .with_context(|| {
            format!("CSV row {row_number} is missing a value for column '{column_name}'")
        })
}

fn parse_csv_f64(
    row: &[String],
    index: usize,
    row_number: usize,
    column_name: &str,
) -> Result<f64> {
    csv_field(row, index, row_number, column_name)?
        .parse::<f64>()
        .with_context(|| {
            format!("CSV row {row_number} has an invalid float in column '{column_name}'")
        })
}

fn csv_rows(raw: &str) -> impl Iterator<Item = Vec<String>> + '_ {
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_csv_line)
}

fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut in_quotes = false;

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                if in_quotes && chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    in_quotes = !in_quotes;
                }
            }
            ',' if !in_quotes => {
                fields.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    fields.push(current.trim().to_string());
    fields
}

fn parse_structured_point_set(parsed: PointSetFile) -> PointSetDocument {
    match parsed {
        PointSetFile::Document(document) => document,
        PointSetFile::Bare(points) => PointSetDocument {
            points,
            snap: SnapOptions::default(),
            connectivity: ConnectivityPolicy::default(),
            fallback: FallbackPolicy::default(),
            returns: ReturnConfig::default(),
        },
    }
}

fn parse_structured_od_pairs(parsed: OdPairsFile) -> OdPairsDocument {
    match parsed {
        OdPairsFile::Document(document) => document,
        OdPairsFile::Bare(pairs) => OdPairsDocument {
            pairs,
            snap: SnapOptions::default(),
            connectivity: ConnectivityPolicy::default(),
            fallback: FallbackPolicy::default(),
            returns: ReturnConfig::default(),
        },
    }
}

fn validate_service_area_request(request: ServiceAreaRequest) -> Result<ServiceAreaRequest> {
    if request.analysis_id.trim().is_empty() {
        bail!("service-area request must contain a non-empty analysis_id");
    }
    if request.origins.is_empty() {
        bail!("service-area request must contain at least one origin");
    }
    if request.thresholds.is_empty() {
        bail!("service-area request must contain at least one threshold");
    }
    if request
        .thresholds
        .iter()
        .any(|threshold| threshold.limit <= 0.0)
    {
        bail!("service-area thresholds must be greater than zero");
    }
    if request.fallback != FallbackPolicy::default() {
        bail!(
            "service-area execution does not support fallback failure modes yet; leave fallback at the default strict value"
        );
    }
    Ok(request)
}

fn merge_point_set_returns(left: &ReturnConfig, right: &ReturnConfig) -> ReturnConfig {
    let mut merged = left.clone();
    if matches!(merged.geometry, netan_profile::ReturnGeometry::None) {
        merged.geometry = right.geometry;
    }
    merged.segment_rows |= right.segment_rows;
    if merged.road_type_breakdown.is_empty() {
        merged.road_type_breakdown = right.road_type_breakdown.clone();
    }
    if merged.surface_breakdown.is_empty() {
        merged.surface_breakdown = right.surface_breakdown.clone();
    }
    merged.penalty_breakdown |= right.penalty_breakdown;
    merged.explain_cost_derivation |= right.explain_cost_derivation;
    merged
}

fn merge_point_set_connectivity_policy(
    left: &ConnectivityPolicy,
    right: &ConnectivityPolicy,
) -> ConnectivityPolicy {
    if *left == ConnectivityPolicy::default() {
        right.clone()
    } else {
        left.clone()
    }
}

fn merge_point_set_fallback_policy(
    left: &FallbackPolicy,
    right: &FallbackPolicy,
) -> FallbackPolicy {
    if *left == FallbackPolicy::default() {
        right.clone()
    } else {
        left.clone()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnappedPoint {
    pub point_id: String,
    pub requested_lon: f64,
    pub requested_lat: f64,
    pub snapped_node_id: u32,
    pub snapped_lon: f64,
    pub snapped_lat: f64,
    pub snap_distance_m: f64,
    #[serde(default)]
    pub snapped_edge_id: Option<u32>,
    #[serde(default)]
    pub snapped_edge_fraction: Option<f64>,
    #[serde(default)]
    pub snapped_from_node_id: Option<u32>,
    #[serde(default)]
    pub snapped_to_node_id: Option<u32>,
    #[serde(default)]
    pub component_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteSummary {
    #[serde(default)]
    pub network_distance_m: u64,
    #[serde(default)]
    pub network_travel_time_s: f64,
    #[serde(default)]
    pub network_generalized_cost: f64,
    #[serde(default)]
    pub illegal_movement_penalty_s: f64,
    #[serde(default)]
    pub illegal_movement_penalty_cost: f64,
    #[serde(default)]
    pub violation_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violation_types: Vec<RouteViolationType>,
    pub total_distance_m: u64,
    pub total_travel_time_s: f64,
    pub total_generalized_cost: f64,
    pub segment_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteViolationType {
    ReverseOneway,
    IllegalTurn,
    IgnoredTurnRestriction,
    ForbiddenUturn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteViolation {
    pub violation_type: RouteViolationType,
    #[serde(default)]
    pub edge_id: Option<u32>,
    #[serde(default)]
    pub from_edge_id: Option<u32>,
    #[serde(default)]
    pub to_edge_id: Option<u32>,
    #[serde(default)]
    pub distance_m: Option<f64>,
    #[serde(default)]
    pub penalty_s: f64,
    #[serde(default)]
    pub penalty_generalized_cost: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HopEndpoint {
    Origin,
    Destination,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteHopSegment {
    pub endpoint: HopEndpoint,
    pub distance_m: f64,
    pub geometry: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteSegment {
    pub edge_id: u32,
    pub from_node_id: u32,
    pub to_node_id: u32,
    pub source_way_id: i64,
    pub length_m: u32,
    pub travel_time_s: f64,
    pub generalized_cost: f64,
    pub road_class: RoadClass,
    pub surface: SurfaceClass,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub violation_type: Option<RouteViolationType>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MetricBreakdown {
    #[serde(default)]
    pub distance_m: Option<u64>,
    #[serde(default)]
    pub time_s: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RouteBreakdowns {
    #[serde(default)]
    pub road_class: BTreeMap<String, MetricBreakdown>,
    #[serde(default)]
    pub surface: BTreeMap<String, MetricBreakdown>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteResult {
    pub route_id: String,
    pub origin: SnappedPoint,
    pub destination: SnappedPoint,
    #[serde(default)]
    pub outcome: AnalysisOutcome,
    #[serde(default)]
    pub fallback_used: bool,
    #[serde(default)]
    pub origin_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub destination_hop_distance_m: Option<f64>,
    pub summary: RouteSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_path: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edge_path: Vec<u32>,
    #[serde(default)]
    pub geometry: Option<Vec<[f64; 2]>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hop_segments: Vec<RouteHopSegment>,
    #[serde(default)]
    pub segments: Option<Vec<RouteSegment>>,
    #[serde(default)]
    pub breakdowns: Option<RouteBreakdowns>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<RouteViolation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchItemStatus {
    Succeeded,
    Ignored,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdPairResult {
    pub pair_id: String,
    pub origin_id: String,
    pub destination_id: String,
    pub status: BatchItemStatus,
    #[serde(default)]
    pub outcome: AnalysisOutcome,
    #[serde(default)]
    pub fallback_used: bool,
    #[serde(default)]
    pub origin_component_id: Option<u32>,
    #[serde(default)]
    pub destination_component_id: Option<u32>,
    #[serde(default)]
    pub origin_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub destination_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub origin_snap_distance_m: Option<f64>,
    #[serde(default)]
    pub destination_snap_distance_m: Option<f64>,
    #[serde(default)]
    pub total_distance_m: Option<u64>,
    #[serde(default)]
    pub total_travel_time_s: Option<f64>,
    #[serde(default)]
    pub total_generalized_cost: Option<f64>,
    #[serde(default)]
    pub illegal_movement_penalty_s: Option<f64>,
    #[serde(default)]
    pub illegal_movement_penalty_cost: Option<f64>,
    #[serde(default)]
    pub violation_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violation_types: Vec<RouteViolationType>,
    #[serde(default)]
    pub geometry: Option<Vec<[f64; 2]>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdResult {
    pub pair_count: usize,
    pub succeeded_count: usize,
    pub failed_count: usize,
    #[serde(default)]
    pub ignored_count: usize,
    pub pairs: Vec<OdPairResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixCellResult {
    pub origin_id: String,
    pub destination_id: String,
    pub status: BatchItemStatus,
    #[serde(default)]
    pub outcome: AnalysisOutcome,
    #[serde(default)]
    pub fallback_used: bool,
    #[serde(default)]
    pub origin_component_id: Option<u32>,
    #[serde(default)]
    pub destination_component_id: Option<u32>,
    #[serde(default)]
    pub origin_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub destination_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub origin_snap_distance_m: Option<f64>,
    #[serde(default)]
    pub destination_snap_distance_m: Option<f64>,
    #[serde(default)]
    pub total_distance_m: Option<u64>,
    #[serde(default)]
    pub total_travel_time_s: Option<f64>,
    #[serde(default)]
    pub total_generalized_cost: Option<f64>,
    #[serde(default)]
    pub illegal_movement_penalty_s: Option<f64>,
    #[serde(default)]
    pub illegal_movement_penalty_cost: Option<f64>,
    #[serde(default)]
    pub violation_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violation_types: Vec<RouteViolationType>,
    #[serde(default)]
    pub geometry: Option<Vec<[f64; 2]>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixResult {
    pub origin_count: usize,
    pub destination_count: usize,
    pub cell_count: usize,
    pub succeeded_count: usize,
    pub failed_count: usize,
    #[serde(default)]
    pub ignored_count: usize,
    pub cells: Vec<MatrixCellResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

pub struct PreparedRoutingEngine {
    topology: Arc<TopologyBundle>,
    metrics: Arc<CompiledProfileBundle>,
    default_routing_graph: RoutingGraph,
    ignore_multi_edge_restrictions_graph: Option<RoutingGraph>,
}

impl PreparedRoutingEngine {
    pub fn new(topology: Arc<TopologyBundle>, metrics: Arc<CompiledProfileBundle>) -> Result<Self> {
        validate_execution_inputs(topology.as_ref(), metrics.as_ref())?;
        let default_routing_graph = build_routing_graph(topology.as_ref(), metrics.as_ref())?;
        let ignore_multi_edge_restrictions_graph =
            if default_routing_graph.has_restriction_sequences() {
                Some(build_routing_graph_with_options(
                    topology.as_ref(),
                    metrics.as_ref(),
                    RoutingGraphBuildOptions {
                        ignore_multi_edge_restriction_sequences: true,
                        search_time_turn_restrictions: false,
                    },
                )?)
            } else {
                None
            };
        Ok(Self {
            topology,
            metrics,
            default_routing_graph,
            ignore_multi_edge_restrictions_graph,
        })
    }

    pub fn topology(&self) -> &TopologyBundle {
        self.topology.as_ref()
    }

    pub fn metrics(&self) -> &CompiledProfileBundle {
        self.metrics.as_ref()
    }

    pub fn execute_route(&self, request: &RouteRequest) -> Result<RouteResult> {
        self.execute_route_with_optional_edge_names(request, None, EngineMode::Auto)
    }

    pub fn execute_route_with_edge_names(
        &self,
        request: &RouteRequest,
        edge_names: &[String],
    ) -> Result<RouteResult> {
        self.execute_route_with_optional_edge_names(request, Some(edge_names), EngineMode::Auto)
    }

    pub fn execute_route_with_mode(
        &self,
        request: &RouteRequest,
        mode: EngineMode,
    ) -> Result<RouteResult> {
        self.execute_route_with_optional_edge_names(request, None, mode)
    }

    pub fn execute_route_with_edge_names_and_mode(
        &self,
        request: &RouteRequest,
        edge_names: &[String],
        mode: EngineMode,
    ) -> Result<RouteResult> {
        self.execute_route_with_optional_edge_names(request, Some(edge_names), mode)
    }

    fn execute_route_with_optional_edge_names(
        &self,
        request: &RouteRequest,
        edge_names: Option<&[String]>,
        mode: EngineMode,
    ) -> Result<RouteResult> {
        if has_failure_modes(&request.fallback) {
            let (degraded_topology, degraded_metrics, degraded_routing_graph) =
                build_failure_mode_bundle(
                    self.topology.as_ref(),
                    self.metrics.as_ref(),
                    &request.fallback,
                )?;
            return execute_route_with_graph(
                &degraded_topology,
                &degraded_metrics,
                &degraded_routing_graph,
                request,
                edge_names,
            );
        }
        let (routing_graph, _) = self.routing_graph_for_mode(mode);
        execute_route_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            routing_graph,
            request,
            edge_names,
        )
    }

    pub fn execute_od(&self, document: &OdPairsDocument) -> Result<OdResult> {
        self.execute_od_with_mode(document, EngineMode::Auto)
    }

    pub fn execute_od_with_mode(
        &self,
        document: &OdPairsDocument,
        mode: EngineMode,
    ) -> Result<OdResult> {
        if has_failure_modes(&document.fallback) {
            let (degraded_topology, degraded_metrics, degraded_routing_graph) =
                build_failure_mode_bundle(
                    self.topology.as_ref(),
                    self.metrics.as_ref(),
                    &document.fallback,
                )?;
            return execute_od_with_graph(
                &degraded_topology,
                &degraded_metrics,
                &degraded_routing_graph,
                document,
            );
        }
        let (routing_graph, _) = self.routing_graph_for_mode(mode);
        execute_od_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            routing_graph,
            document,
        )
    }

    pub fn execute_matrix(
        &self,
        origins: &PointSetDocument,
        destinations: &PointSetDocument,
    ) -> Result<MatrixResult> {
        self.execute_matrix_with_mode(origins, destinations, EngineMode::Auto)
    }

    pub fn execute_matrix_with_mode(
        &self,
        origins: &PointSetDocument,
        destinations: &PointSetDocument,
        mode: EngineMode,
    ) -> Result<MatrixResult> {
        let fallback = merge_point_set_fallback_policy(&origins.fallback, &destinations.fallback);
        if has_failure_modes(&fallback) {
            let (degraded_topology, degraded_metrics, degraded_routing_graph) =
                build_failure_mode_bundle(
                    self.topology.as_ref(),
                    self.metrics.as_ref(),
                    &fallback,
                )?;
            return execute_matrix_with_graph(
                &degraded_topology,
                &degraded_metrics,
                &degraded_routing_graph,
                origins,
                destinations,
            );
        }
        let (routing_graph, _) = self.routing_graph_for_mode(mode);
        execute_matrix_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            routing_graph,
            origins,
            destinations,
        )
    }

    pub fn execute_service_area(&self, request: &ServiceAreaRequest) -> Result<ServiceAreaResult> {
        self.execute_service_area_with_mode(request, EngineMode::Auto)
    }

    pub fn execute_service_area_with_mode(
        &self,
        request: &ServiceAreaRequest,
        mode: EngineMode,
    ) -> Result<ServiceAreaResult> {
        let (routing_graph, _) = self.routing_graph_for_mode(mode);
        execute_service_area_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            routing_graph,
            request,
        )
    }

    pub fn effective_engine_description(&self, mode: EngineMode) -> EffectiveEngineDescription {
        self.routing_graph_for_mode(mode).1
    }

    fn routing_graph_for_mode(
        &self,
        mode: EngineMode,
    ) -> (&RoutingGraph, EffectiveEngineDescription) {
        match mode {
            EngineMode::Auto => (
                &self.default_routing_graph,
                effective_engine_description(&self.default_routing_graph),
            ),
            EngineMode::IgnoreMultiEdgeRestrictions => {
                let graph = self
                    .ignore_multi_edge_restrictions_graph
                    .as_ref()
                    .unwrap_or(&self.default_routing_graph);
                (graph, effective_engine_description(graph))
            }
        }
    }
}

fn effective_engine_description(routing_graph: &RoutingGraph) -> EffectiveEngineDescription {
    if routing_graph.has_restriction_sequences() {
        EffectiveEngineDescription {
            route_engine: "astar_exact_multi_edge_turns",
            batch_engine: "astar_exact_multi_edge_turns_batch_reuse",
            acceleration: "spatial_index+a_star+turn_automaton",
        }
    } else {
        EffectiveEngineDescription {
            route_engine: "bidirectional_exact_pairwise_turns",
            batch_engine: "bidirectional_exact_pairwise_turns_batch_reuse",
            acceleration: "spatial_index+edge_phantoms",
        }
    }
}

fn execute_od_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    document: &OdPairsDocument,
) -> Result<OdResult> {
    let mut snap_cache = HashMap::new();
    let origin_snaps = document
        .pairs
        .iter()
        .map(|pair| {
            cached_snap_candidates(
                &mut snap_cache,
                topology,
                routing_graph,
                &pair.origin,
                document.snap.max_distance_m,
                true,
            )
        })
        .collect::<Vec<_>>();
    let destination_snaps = document
        .pairs
        .iter()
        .map(|pair| {
            cached_snap_candidates(
                &mut snap_cache,
                topology,
                routing_graph,
                &pair.destination,
                document.snap.max_distance_m,
                false,
            )
        })
        .collect::<Vec<_>>();
    let (origin_refs, unique_origin_candidates) = intern_candidate_sets(origin_snaps);
    let (destination_refs, unique_destination_candidates) =
        intern_candidate_sets(destination_snaps);
    let mut route_cache = HashMap::new();
    let mut origin_tree_cache = HashMap::new();
    let mut pairs = Vec::with_capacity(document.pairs.len());
    let mut succeeded_count = 0_usize;
    let mut ignored_count = 0_usize;

    for ((pair, origin_ref), destination_ref) in document
        .pairs
        .iter()
        .zip(&origin_refs)
        .zip(&destination_refs)
    {
        let route = match (origin_ref, destination_ref) {
            (Ok(origin_set_id), Ok(destination_set_id)) => cached_batch_route_result(
                &mut route_cache,
                &mut origin_tree_cache,
                topology,
                metrics,
                routing_graph,
                "",
                document.snap.max_distance_m,
                &document.connectivity,
                &document.fallback,
                &document.returns,
                &unique_origin_candidates,
                *origin_set_id,
                &unique_destination_candidates,
                *destination_set_id,
            ),
            (Err(error), _) => Err(anyhow::anyhow!(error.clone())),
            (_, Err(error)) => Err(anyhow::anyhow!(error.clone())),
        };
        match route {
            Ok(route) => {
                let status = batch_status_for_route(&route);
                if matches!(status, BatchItemStatus::Succeeded) {
                    succeeded_count += 1;
                } else {
                    ignored_count += 1;
                }
                let ignored = matches!(status, BatchItemStatus::Ignored);
                let error = batch_ignored_message(&route);
                let geometry = route.geometry;
                let diagnostics = route.diagnostics;
                pairs.push(OdPairResult {
                    pair_id: pair.pair_id.clone(),
                    origin_id: pair.origin.id.clone(),
                    destination_id: pair.destination.id.clone(),
                    status,
                    outcome: route.outcome,
                    fallback_used: route.fallback_used,
                    origin_component_id: route.origin.component_id,
                    destination_component_id: route.destination.component_id,
                    origin_hop_distance_m: route.origin_hop_distance_m,
                    destination_hop_distance_m: route.destination_hop_distance_m,
                    origin_snap_distance_m: Some(route.origin.snap_distance_m),
                    destination_snap_distance_m: Some(route.destination.snap_distance_m),
                    total_distance_m: (!ignored).then_some(route.summary.total_distance_m),
                    total_travel_time_s: (!ignored).then_some(route.summary.total_travel_time_s),
                    total_generalized_cost: (!ignored)
                        .then_some(route.summary.total_generalized_cost),
                    illegal_movement_penalty_s: (!ignored)
                        .then_some(route.summary.illegal_movement_penalty_s),
                    illegal_movement_penalty_cost: (!ignored)
                        .then_some(route.summary.illegal_movement_penalty_cost),
                    violation_count: route.summary.violation_count,
                    violation_types: route.summary.violation_types.clone(),
                    geometry,
                    diagnostics,
                    error,
                });
            }
            Err(error) => {
                let (outcome, diagnostics) = failure_outcome_and_diagnostics(&error);
                pairs.push(OdPairResult {
                    pair_id: pair.pair_id.clone(),
                    origin_id: pair.origin.id.clone(),
                    destination_id: pair.destination.id.clone(),
                    status: BatchItemStatus::Failed,
                    outcome,
                    fallback_used: false,
                    origin_component_id: None,
                    destination_component_id: None,
                    origin_hop_distance_m: None,
                    destination_hop_distance_m: None,
                    origin_snap_distance_m: None,
                    destination_snap_distance_m: None,
                    total_distance_m: None,
                    total_travel_time_s: None,
                    total_generalized_cost: None,
                    illegal_movement_penalty_s: None,
                    illegal_movement_penalty_cost: None,
                    violation_count: 0,
                    violation_types: Vec::new(),
                    geometry: None,
                    diagnostics,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    Ok(OdResult {
        pair_count: pairs.len(),
        succeeded_count,
        failed_count: pairs.len() - succeeded_count - ignored_count,
        ignored_count,
        pairs,
        diagnostics: Vec::new(),
        warnings: {
            let mut warnings = vec![
                "Batch OD execution now reuses exact single-source search trees and duplicate snapped solves within the batch; multi-edge restriction cases still fall back to per-pair automaton search.".to_string(),
            ];
            if ignored_count > 0 {
                warnings.push(batch_ignored_unreachable_warning("OD", ignored_count));
            }
            warnings.extend(execution_warnings(metrics));
            warnings
        },
    })
}

fn execute_matrix_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
) -> Result<MatrixResult> {
    let snap_max_distance_m = origins
        .snap
        .max_distance_m
        .max(destinations.snap.max_distance_m);
    let returns = merge_point_set_returns(&origins.returns, &destinations.returns);
    let connectivity =
        merge_point_set_connectivity_policy(&origins.connectivity, &destinations.connectivity);
    let fallback = merge_point_set_fallback_policy(&origins.fallback, &destinations.fallback);
    let origin_snaps = presnap_point_set(
        topology,
        routing_graph,
        &origins.points,
        snap_max_distance_m,
        true,
    );
    let (origin_refs, unique_origin_candidates) = intern_candidate_sets(origin_snaps);
    let destination_snaps = presnap_point_set(
        topology,
        routing_graph,
        &destinations.points,
        snap_max_distance_m,
        false,
    );
    let (destination_refs, unique_destination_candidates) =
        intern_candidate_sets(destination_snaps);
    let mut route_cache = HashMap::new();
    let mut origin_tree_cache = HashMap::new();
    let mut cells = Vec::with_capacity(origins.points.len() * destinations.points.len());
    let mut succeeded_count = 0_usize;
    let mut ignored_count = 0_usize;

    for (origin, origin_ref) in origins.points.iter().zip(&origin_refs) {
        for (destination, destination_ref) in destinations.points.iter().zip(&destination_refs) {
            let route = match (origin_ref, destination_ref) {
                (Ok(origin_set_id), Ok(destination_set_id)) => cached_batch_route_result(
                    &mut route_cache,
                    &mut origin_tree_cache,
                    topology,
                    metrics,
                    routing_graph,
                    "",
                    snap_max_distance_m,
                    &connectivity,
                    &fallback,
                    &returns,
                    &unique_origin_candidates,
                    *origin_set_id,
                    &unique_destination_candidates,
                    *destination_set_id,
                ),
                (Err(error), _) => Err(anyhow::anyhow!(error.clone())),
                (_, Err(error)) => Err(anyhow::anyhow!(error.clone())),
            };
            match route {
                Ok(route) => {
                    let status = batch_status_for_route(&route);
                    if matches!(status, BatchItemStatus::Succeeded) {
                        succeeded_count += 1;
                    } else {
                        ignored_count += 1;
                    }
                    let ignored = matches!(status, BatchItemStatus::Ignored);
                    let error = batch_ignored_message(&route);
                    let geometry = route.geometry;
                    let diagnostics = route.diagnostics;
                    cells.push(MatrixCellResult {
                        origin_id: origin.id.clone(),
                        destination_id: destination.id.clone(),
                        status,
                        outcome: route.outcome,
                        fallback_used: route.fallback_used,
                        origin_component_id: route.origin.component_id,
                        destination_component_id: route.destination.component_id,
                        origin_hop_distance_m: route.origin_hop_distance_m,
                        destination_hop_distance_m: route.destination_hop_distance_m,
                        origin_snap_distance_m: Some(route.origin.snap_distance_m),
                        destination_snap_distance_m: Some(route.destination.snap_distance_m),
                        total_distance_m: (!ignored).then_some(route.summary.total_distance_m),
                        total_travel_time_s: (!ignored)
                            .then_some(route.summary.total_travel_time_s),
                        total_generalized_cost: (!ignored)
                            .then_some(route.summary.total_generalized_cost),
                        illegal_movement_penalty_s: (!ignored)
                            .then_some(route.summary.illegal_movement_penalty_s),
                        illegal_movement_penalty_cost: (!ignored)
                            .then_some(route.summary.illegal_movement_penalty_cost),
                        violation_count: route.summary.violation_count,
                        violation_types: route.summary.violation_types.clone(),
                        geometry,
                        diagnostics,
                        error,
                    });
                }
                Err(error) => {
                    let (outcome, diagnostics) = failure_outcome_and_diagnostics(&error);
                    cells.push(MatrixCellResult {
                        origin_id: origin.id.clone(),
                        destination_id: destination.id.clone(),
                        status: BatchItemStatus::Failed,
                        outcome,
                        fallback_used: false,
                        origin_component_id: None,
                        destination_component_id: None,
                        origin_hop_distance_m: None,
                        destination_hop_distance_m: None,
                        origin_snap_distance_m: None,
                        destination_snap_distance_m: None,
                        total_distance_m: None,
                        total_travel_time_s: None,
                        total_generalized_cost: None,
                        illegal_movement_penalty_s: None,
                        illegal_movement_penalty_cost: None,
                        violation_count: 0,
                        violation_types: Vec::new(),
                        geometry: None,
                        diagnostics,
                        error: Some(error.to_string()),
                    });
                }
            }
        }
    }

    Ok(MatrixResult {
        origin_count: origins.points.len(),
        destination_count: destinations.points.len(),
        cell_count: cells.len(),
        succeeded_count,
        failed_count: cells.len() - succeeded_count - ignored_count,
        ignored_count,
        cells,
        diagnostics: Vec::new(),
        warnings: {
            let mut warnings = vec![
                "Matrix execution now reuses exact single-source search trees and duplicate snapped solves within the batch; multi-edge restriction cases still fall back to per-cell automaton search.".to_string(),
            ];
            if ignored_count > 0 {
                warnings.push(batch_ignored_unreachable_warning("matrix", ignored_count));
            }
            warnings.extend(execution_warnings(metrics));
            warnings
        },
    })
}

pub fn execute_service_area(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &ServiceAreaRequest,
) -> Result<ServiceAreaResult> {
    validate_execution_inputs(topology, metrics)?;
    let routing_graph = build_routing_graph(topology, metrics)?;
    execute_service_area_with_graph(topology, metrics, &routing_graph, request)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ServiceAreaMetricKind {
    DistanceM,
    TravelTimeS,
}

impl From<ServiceAreaThresholdMetric> for ServiceAreaMetricKind {
    fn from(value: ServiceAreaThresholdMetric) -> Self {
        match value {
            ServiceAreaThresholdMetric::DistanceM => Self::DistanceM,
            ServiceAreaThresholdMetric::TravelTimeS => Self::TravelTimeS,
        }
    }
}

#[derive(Debug, Clone)]
struct ReachableEdgeInterval {
    edge_index: usize,
    start_fraction: f64,
    end_fraction: f64,
    midpoint_cost: f64,
}

#[derive(Debug, Clone)]
struct ServiceAreaOriginExpansion {
    origin_id: String,
    representative_origin: SnappedPoint,
    fallback_used: bool,
    origin_hop_distance_m: Option<f64>,
    edge_before_costs: Vec<f64>,
    edge_end_costs: Vec<f64>,
    edge_start_fractions: Vec<f64>,
    diagnostics: Vec<AnalysisDiagnostic>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct ServiceAreaOriginBand {
    origin_id: String,
    origin_component_id: Option<u32>,
    fallback_used: bool,
    origin_hop_distance_m: Option<f64>,
    threshold_id: Option<String>,
    band_start_limit: Option<f64>,
    threshold_limit: f64,
    threshold_metric: ServiceAreaThresholdMetric,
    segments: Vec<ReachableEdgeInterval>,
}

#[derive(Debug, Clone)]
struct ServiceAreaOriginResolution {
    seed_candidates: Vec<SnappedPoint>,
    representative_origin: SnappedPoint,
    fallback_used: bool,
    origin_hop_distance_m: Option<f64>,
    diagnostics: Vec<AnalysisDiagnostic>,
    warnings: Vec<String>,
}

fn execute_service_area_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &ServiceAreaRequest,
) -> Result<ServiceAreaResult> {
    let search_distance_m = request
        .connectivity
        .max_hop_distance_m
        .unwrap_or(request.snap.max_distance_m)
        .max(request.snap.max_distance_m);
    let thresholds_by_metric = thresholds_for_service_area(request);
    let mut snap_cache = HashMap::new();
    let origin_candidate_sets = request
        .origins
        .iter()
        .map(|origin| {
            cached_snap_candidates(
                &mut snap_cache,
                topology,
                routing_graph,
                origin,
                search_distance_m,
                true,
            )
        })
        .collect::<Vec<_>>();
    let (origin_refs, unique_origin_candidates) = intern_candidate_sets(origin_candidate_sets);

    let mut expansion_cache = HashMap::<
        (usize, ServiceAreaMetricKind),
        std::result::Result<ServiceAreaOriginExpansion, AnalysisFailure>,
    >::new();
    let mut diagnostics = Vec::new();
    let mut warnings = execution_warnings(metrics);
    let mut threshold_summaries = Vec::new();
    let mut origin_bands = Vec::new();
    let mut processed_origin_count = 0_usize;
    let mut skipped_origin_count = 0_usize;
    let mut fallback_origin_count = 0_usize;

    for (origin, origin_ref) in request.origins.iter().zip(&origin_refs) {
        let origin_set_id = match origin_ref {
            Ok(origin_set_id) => *origin_set_id,
            Err(error) => {
                if matches!(
                    request.connectivity.disconnected,
                    DisconnectedNetworkMode::IgnoreUnreachable
                ) {
                    skipped_origin_count += 1;
                    diagnostics.extend(error.diagnostics.clone());
                    warnings.push(format!(
                        "Service-area origin '{}' was ignored because it had no legal snap candidate within policy.",
                        origin.id
                    ));
                    continue;
                }
                return Err(anyhow::Error::new(error.clone()))
                    .with_context(|| format!("executing service-area origin '{}'", origin.id));
            }
        };

        let mut origin_processed = false;
        let mut origin_skipped = false;

        for (metric_kind, thresholds) in &thresholds_by_metric {
            let expansion = expansion_cache
                .entry((origin_set_id, *metric_kind))
                .or_insert_with(|| {
                    build_service_area_expansion(
                        topology,
                        metrics,
                        routing_graph,
                        origin,
                        &unique_origin_candidates[origin_set_id],
                        request.snap.max_distance_m,
                        &request.connectivity,
                        *metric_kind,
                    )
                    .map_err(|error| {
                        analysis_failure(&error).cloned().unwrap_or_else(|| {
                            AnalysisFailure::new(
                                error.to_string(),
                                AnalysisOutcome::Unreachable,
                                Vec::new(),
                            )
                        })
                    })
                })
                .clone();

            let expansion = match expansion {
                Ok(expansion) => expansion,
                Err(error)
                    if matches!(
                        request.connectivity.disconnected,
                        DisconnectedNetworkMode::IgnoreUnreachable
                    ) =>
                {
                    origin_skipped = true;
                    diagnostics.extend(error.diagnostics);
                    warnings.push(format!(
                        "Service-area origin '{}' was ignored because it could not be reached under the current connectivity policy.",
                        origin.id
                    ));
                    continue;
                }
                Err(error) => {
                    return Err(anyhow::Error::new(error))
                        .with_context(|| format!("executing service-area origin '{}'", origin.id));
                }
            };

            origin_processed = true;
            if expansion.fallback_used {
                fallback_origin_count += 1;
            }
            diagnostics.extend(expansion.diagnostics.clone());
            warnings.extend(expansion.warnings.clone());

            let mut previous_limit = None;
            for threshold in thresholds {
                let cumulative = service_area_intervals_for_threshold(
                    topology,
                    metrics,
                    &expansion,
                    *metric_kind,
                    threshold.limit,
                    request.boundary_mode,
                );
                let segments = if matches!(request.band_mode, ServiceAreaBandMode::Ring) {
                    let previous = previous_limit.map(|limit| {
                        service_area_intervals_for_threshold(
                            topology,
                            metrics,
                            &expansion,
                            *metric_kind,
                            limit,
                            request.boundary_mode,
                        )
                    });
                    difference_service_area_intervals(cumulative, previous.unwrap_or_default())
                } else {
                    cumulative
                };

                if request.returns.per_threshold_summary {
                    let (reachable_network_length_m, reachable_edge_count) =
                        summarize_service_area_segments(
                            topology,
                            &segments,
                            request.returns.attributes,
                        );
                    threshold_summaries.push(ServiceAreaThresholdSummary {
                        origin_id: Some(expansion.origin_id.clone()),
                        band_start_limit: previous_limit,
                        threshold_id: threshold.id.clone(),
                        threshold_limit: threshold.limit,
                        threshold_metric: threshold.metric,
                        fallback_used: expansion.fallback_used,
                        origin_component_id: expansion.representative_origin.component_id,
                        origin_hop_distance_m: expansion.origin_hop_distance_m,
                        reachable_network_length_m,
                        reachable_edge_count,
                    });
                }

                origin_bands.push(ServiceAreaOriginBand {
                    origin_id: expansion.origin_id.clone(),
                    origin_component_id: expansion.representative_origin.component_id,
                    fallback_used: expansion.fallback_used,
                    origin_hop_distance_m: expansion.origin_hop_distance_m,
                    threshold_id: threshold.id.clone(),
                    band_start_limit: previous_limit,
                    threshold_limit: threshold.limit,
                    threshold_metric: threshold.metric,
                    segments,
                });
                previous_limit = Some(threshold.limit);
            }
        }

        if origin_processed {
            processed_origin_count += 1;
        } else if origin_skipped {
            skipped_origin_count += 1;
        }
    }

    let origin_bands = match request.multi_origin_mode {
        ServiceAreaMultiOriginMode::Overlap => origin_bands,
        ServiceAreaMultiOriginMode::Merge => merge_service_area_bands(origin_bands),
        ServiceAreaMultiOriginMode::Cut => cut_service_area_bands(origin_bands),
    };

    let features = if request.returns.geometry || request.returns.attributes {
        build_service_area_features(topology, request, origin_bands)
    } else {
        Vec::new()
    };

    let diagnostics = if request.returns.diagnostics {
        diagnostics
    } else {
        Vec::new()
    };

    Ok(ServiceAreaResult {
        analysis_id: request.analysis_id.clone(),
        outcome: service_area_result_outcome(
            processed_origin_count,
            skipped_origin_count,
            fallback_origin_count,
        ),
        output_mode: request.output_mode,
        band_mode: request.band_mode,
        boundary_mode: request.boundary_mode,
        multi_origin_mode: request.multi_origin_mode,
        origin_count: request.origins.len(),
        processed_origin_count,
        skipped_origin_count,
        fallback_origin_count,
        threshold_count: request.thresholds.len(),
        features,
        summaries: threshold_summaries,
        diagnostics,
        warnings,
    })
}

fn thresholds_for_service_area(
    request: &ServiceAreaRequest,
) -> Vec<(ServiceAreaMetricKind, Vec<&ServiceAreaThreshold>)> {
    let mut distance = request
        .thresholds
        .iter()
        .filter(|threshold| matches!(threshold.metric, ServiceAreaThresholdMetric::DistanceM))
        .collect::<Vec<_>>();
    let mut time = request
        .thresholds
        .iter()
        .filter(|threshold| matches!(threshold.metric, ServiceAreaThresholdMetric::TravelTimeS))
        .collect::<Vec<_>>();
    distance.sort_by(|left, right| left.limit.total_cmp(&right.limit));
    time.sort_by(|left, right| left.limit.total_cmp(&right.limit));

    let mut groups = Vec::new();
    if !distance.is_empty() {
        groups.push((ServiceAreaMetricKind::DistanceM, distance));
    }
    if !time.is_empty() {
        groups.push((ServiceAreaMetricKind::TravelTimeS, time));
    }
    groups
}

fn build_service_area_expansion(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    candidates: &[SnappedPoint],
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    metric_kind: ServiceAreaMetricKind,
) -> Result<ServiceAreaOriginExpansion> {
    let resolution =
        resolve_service_area_origin(point, candidates, snap_max_distance_m, connectivity)?;
    let edge_count = topology.edges.len();
    let mut edge_before_costs = vec![f64::INFINITY; edge_count];
    let mut edge_end_costs = vec![f64::INFINITY; edge_count];
    let mut edge_start_fractions = vec![0.0_f64; edge_count];

    if routing_graph.has_restriction_sequences() {
        let mut dist = HashMap::<SearchStateKey, f64>::new();
        let mut heap = BinaryHeap::new();
        for candidate in &resolution.seed_candidates {
            for seed in
                service_area_seed_specs(routing_graph, topology, metrics, candidate, metric_kind)
            {
                let automaton_state = routing_graph.automaton.transition(0, seed.edge_index);
                let key = SearchStateKey {
                    edge_index: seed.edge_index,
                    automaton_state,
                };
                let previous = dist.get(&key).copied().unwrap_or(f64::INFINITY);
                if seed.end_cost + f64::EPSILON >= previous {
                    continue;
                }
                dist.insert(key, seed.end_cost);
                update_service_area_edge_best(
                    &mut edge_before_costs,
                    &mut edge_end_costs,
                    &mut edge_start_fractions,
                    seed.edge_index,
                    seed.before_cost,
                    seed.end_cost,
                    seed.start_fraction,
                );
                heap.push(State {
                    edge_index: seed.edge_index,
                    automaton_state,
                    cost: seed.end_cost,
                    score: seed.end_cost,
                });
            }
        }

        while let Some(State {
            edge_index,
            automaton_state,
            cost,
            score: _,
        }) = heap.pop()
        {
            let key = SearchStateKey {
                edge_index,
                automaton_state,
            };
            if cost > dist.get(&key).copied().unwrap_or(f64::INFINITY) {
                continue;
            }

            for transition_index in routing_graph.transition_range(edge_index) {
                let next_edge = routing_graph.transition_edges[transition_index] as usize;
                if !routing_graph
                    .automaton
                    .is_transition_allowed(automaton_state, next_edge)
                {
                    continue;
                }
                let Some(edge_cost) =
                    service_area_edge_cost(topology, metrics, next_edge, metric_kind)
                else {
                    continue;
                };
                let next_before = cost
                    + service_area_turn_cost(topology, metrics, edge_index, next_edge, metric_kind);
                let next_end = next_before + edge_cost;
                let next_state = routing_graph
                    .automaton
                    .transition(automaton_state, next_edge);
                let next_key = SearchStateKey {
                    edge_index: next_edge,
                    automaton_state: next_state,
                };
                let previous = dist.get(&next_key).copied().unwrap_or(f64::INFINITY);
                if next_end + f64::EPSILON >= previous {
                    continue;
                }
                dist.insert(next_key, next_end);
                update_service_area_edge_best(
                    &mut edge_before_costs,
                    &mut edge_end_costs,
                    &mut edge_start_fractions,
                    next_edge,
                    next_before,
                    next_end,
                    0.0,
                );
                heap.push(State {
                    edge_index: next_edge,
                    automaton_state: next_state,
                    cost: next_end,
                    score: next_end,
                });
            }
        }
    } else {
        SINGLE_SOURCE_SEARCH_SCRATCH.with(|scratch| {
            let mut scratch = scratch.borrow_mut();
            scratch.prepare(edge_count);
            for candidate in &resolution.seed_candidates {
                for seed in service_area_seed_specs(
                    routing_graph,
                    topology,
                    metrics,
                    candidate,
                    metric_kind,
                ) {
                    if !scratch.update(seed.edge_index, seed.end_cost, NO_PREVIOUS_EDGE) {
                        continue;
                    }
                    update_service_area_edge_best(
                        &mut edge_before_costs,
                        &mut edge_end_costs,
                        &mut edge_start_fractions,
                        seed.edge_index,
                        seed.before_cost,
                        seed.end_cost,
                        seed.start_fraction,
                    );
                    scratch.heap.push(State {
                        edge_index: seed.edge_index,
                        automaton_state: 0,
                        cost: seed.end_cost,
                        score: seed.end_cost,
                    });
                }
            }

            while let Some(State {
                edge_index,
                automaton_state: _,
                cost,
                score: _,
            }) = scratch.heap.pop()
            {
                if cost > scratch.dist[edge_index] {
                    continue;
                }

                for transition_index in routing_graph.transition_range(edge_index) {
                    let next_edge = routing_graph.transition_edges[transition_index] as usize;
                    let Some(edge_cost) =
                        service_area_edge_cost(topology, metrics, next_edge, metric_kind)
                    else {
                        continue;
                    };
                    let next_before = cost
                        + service_area_turn_cost(
                            topology,
                            metrics,
                            edge_index,
                            next_edge,
                            metric_kind,
                        );
                    let next_end = next_before + edge_cost;
                    if !scratch.update(next_edge, next_end, edge_index as u32) {
                        continue;
                    }
                    update_service_area_edge_best(
                        &mut edge_before_costs,
                        &mut edge_end_costs,
                        &mut edge_start_fractions,
                        next_edge,
                        next_before,
                        next_end,
                        0.0,
                    );
                    scratch.heap.push(State {
                        edge_index: next_edge,
                        automaton_state: 0,
                        cost: next_end,
                        score: next_end,
                    });
                }
            }
        });
    }

    Ok(ServiceAreaOriginExpansion {
        origin_id: point.id.clone(),
        representative_origin: resolution.representative_origin,
        fallback_used: resolution.fallback_used,
        origin_hop_distance_m: resolution.origin_hop_distance_m,
        edge_before_costs,
        edge_end_costs,
        edge_start_fractions,
        diagnostics: resolution.diagnostics,
        warnings: resolution.warnings,
    })
}

#[derive(Debug, Clone, Copy)]
struct ServiceAreaSeedSpec {
    edge_index: usize,
    before_cost: f64,
    end_cost: f64,
    start_fraction: f64,
}

fn service_area_seed_specs(
    routing_graph: &RoutingGraph,
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    candidate: &SnappedPoint,
    metric_kind: ServiceAreaMetricKind,
) -> Vec<ServiceAreaSeedSpec> {
    if let (Some(edge_id), Some(fraction)) =
        (candidate.snapped_edge_id, candidate.snapped_edge_fraction)
    {
        let edge_index = edge_id as usize;
        let Some(full_cost) = service_area_edge_cost(topology, metrics, edge_index, metric_kind)
        else {
            return Vec::new();
        };
        let remaining = full_cost * (1.0 - fraction);
        if remaining <= f64::EPSILON {
            return Vec::new();
        }
        return vec![ServiceAreaSeedSpec {
            edge_index,
            before_cost: 0.0,
            end_cost: remaining,
            start_fraction: fraction,
        }];
    }

    routing_graph
        .outgoing_edges(candidate.snapped_node_id as usize)
        .iter()
        .filter_map(|&edge_index| {
            let edge_index = edge_index as usize;
            service_area_edge_cost(topology, metrics, edge_index, metric_kind).and_then(|cost| {
                (cost > f64::EPSILON).then_some(ServiceAreaSeedSpec {
                    edge_index,
                    before_cost: 0.0,
                    end_cost: cost,
                    start_fraction: 0.0,
                })
            })
        })
        .collect()
}

fn update_service_area_edge_best(
    edge_before_costs: &mut [f64],
    edge_end_costs: &mut [f64],
    edge_start_fractions: &mut [f64],
    edge_index: usize,
    before_cost: f64,
    end_cost: f64,
    start_fraction: f64,
) {
    if end_cost + f64::EPSILON < edge_end_costs[edge_index] {
        edge_before_costs[edge_index] = before_cost;
        edge_end_costs[edge_index] = end_cost;
        edge_start_fractions[edge_index] = start_fraction;
    }
}

fn resolve_service_area_origin(
    point: &LabeledPoint,
    candidates: &[SnappedPoint],
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
) -> Result<ServiceAreaOriginResolution> {
    let mut warnings = Vec::new();
    let max_hop_distance_m = connectivity
        .max_hop_distance_m
        .unwrap_or(snap_max_distance_m);

    let nearest_candidates = |limit_m: f64| -> Vec<SnappedPoint> {
        let min_distance = candidates
            .iter()
            .filter(|candidate| candidate.snap_distance_m <= limit_m)
            .map(|candidate| candidate.snap_distance_m)
            .min_by(|left, right| left.total_cmp(right));
        candidates
            .iter()
            .filter(|candidate| {
                min_distance.is_some_and(|distance| {
                    candidate.snap_distance_m <= limit_m
                        && (candidate.snap_distance_m - distance).abs() <= 1e-6
                })
            })
            .cloned()
            .collect()
    };

    let strict_candidates = nearest_candidates(snap_max_distance_m);
    let hop_candidates = nearest_candidates(max_hop_distance_m);

    let seed_candidates = match connectivity.disconnected {
        DisconnectedNetworkMode::Strict
        | DisconnectedNetworkMode::IgnoreUnreachable
        | DisconnectedNetworkMode::HopDestinationToNearestReachableComponent => {
            if matches!(
                connectivity.disconnected,
                DisconnectedNetworkMode::HopDestinationToNearestReachableComponent
            ) {
                warnings.push(
                    "connectivity.disconnected=hop_destination_to_nearest_reachable_component has no effect for service-area origins; strict origin snapping was used.".to_string(),
                );
            }
            strict_candidates
        }
        DisconnectedNetworkMode::HopOriginToNearestReachableComponent
        | DisconnectedNetworkMode::HopEitherEnd => {
            if !strict_candidates.is_empty() {
                strict_candidates
            } else {
                hop_candidates
            }
        }
    };

    let Some(representative_origin) = seed_candidates
        .iter()
        .min_by(|left, right| left.snap_distance_m.total_cmp(&right.snap_distance_m))
        .cloned()
    else {
        return Err(route_snap_failure(point, snap_max_distance_m).into());
    };

    let fallback_used = representative_origin.snap_distance_m > snap_max_distance_m;
    let origin_hop_distance_m = fallback_used.then_some(representative_origin.snap_distance_m);
    let mut diagnostics = Vec::new();
    if fallback_used {
        diagnostics.push(AnalysisDiagnostic {
            code: AnalysisDiagnosticCode::FallbackUsed,
            severity: AnalysisDiagnosticSeverity::Warning,
            message: format!(
                "Connectivity fallback hopped service-area origin '{}' {:.1} m to reach component {}.",
                point.id,
                representative_origin.snap_distance_m,
                representative_origin.component_id.unwrap_or_default()
            ),
            point_ids: vec![point.id.clone()],
            component_ids: representative_origin.component_id.into_iter().collect(),
            suggested_actions: vec![],
        });
        warnings.push(format!(
            "Service-area connectivity fallback used a non-network origin hop of {:.1} m for '{}'.",
            representative_origin.snap_distance_m, point.id
        ));
    }

    Ok(ServiceAreaOriginResolution {
        seed_candidates,
        representative_origin,
        fallback_used,
        origin_hop_distance_m,
        diagnostics,
        warnings,
    })
}

fn service_area_edge_cost(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    edge_index: usize,
    metric_kind: ServiceAreaMetricKind,
) -> Option<f64> {
    match metric_kind {
        ServiceAreaMetricKind::DistanceM => Some(topology.edges[edge_index].length_m as f64),
        ServiceAreaMetricKind::TravelTimeS => metrics.edge_metrics[edge_index].travel_time_s,
    }
}

fn service_area_turn_cost(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    previous_edge_index: usize,
    next_edge_index: usize,
    metric_kind: ServiceAreaMetricKind,
) -> f64 {
    match metric_kind {
        ServiceAreaMetricKind::DistanceM => 0.0,
        ServiceAreaMetricKind::TravelTimeS => {
            turn_penalty_seconds(topology, metrics, previous_edge_index, next_edge_index)
        }
    }
}

fn service_area_intervals_for_threshold(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    expansion: &ServiceAreaOriginExpansion,
    metric_kind: ServiceAreaMetricKind,
    threshold_limit: f64,
    boundary_mode: ServiceAreaBoundaryMode,
) -> Vec<ReachableEdgeInterval> {
    let mut segments = Vec::new();
    for edge_index in 0..topology.edges.len() {
        let before_cost = expansion.edge_before_costs[edge_index];
        let end_cost = expansion.edge_end_costs[edge_index];
        if !before_cost.is_finite() || !end_cost.is_finite() || threshold_limit <= before_cost {
            continue;
        }
        let start_fraction = expansion.edge_start_fractions[edge_index];
        let mut end_fraction = 1.0_f64;
        if matches!(boundary_mode, ServiceAreaBoundaryMode::CutAtBoundary)
            && threshold_limit + f64::EPSILON < end_cost
        {
            let remaining_cost = end_cost - before_cost;
            if remaining_cost <= f64::EPSILON {
                continue;
            }
            let progress = ((threshold_limit - before_cost) / remaining_cost).clamp(0.0, 1.0);
            end_fraction = start_fraction + (1.0 - start_fraction) * progress;
        }
        if end_fraction <= start_fraction + f64::EPSILON {
            continue;
        }
        let midpoint_fraction = (start_fraction + end_fraction) / 2.0;
        let midpoint_progress = if start_fraction >= 1.0 - f64::EPSILON {
            1.0
        } else {
            ((midpoint_fraction - start_fraction) / (1.0 - start_fraction)).clamp(0.0, 1.0)
        };
        let full_edge_cost =
            service_area_edge_cost(topology, metrics, edge_index, metric_kind).unwrap_or_default();
        let midpoint_cost =
            before_cost + full_edge_cost * (1.0 - start_fraction) * midpoint_progress;
        segments.push(ReachableEdgeInterval {
            edge_index,
            start_fraction,
            end_fraction,
            midpoint_cost,
        });
    }
    normalize_service_area_segments(segments)
}

fn difference_service_area_intervals(
    current: Vec<ReachableEdgeInterval>,
    previous: Vec<ReachableEdgeInterval>,
) -> Vec<ReachableEdgeInterval> {
    let mut previous_by_edge = BTreeMap::<usize, Vec<ReachableEdgeInterval>>::new();
    for interval in previous {
        previous_by_edge
            .entry(interval.edge_index)
            .or_default()
            .push(interval);
    }

    let mut ring = Vec::new();
    for interval in current {
        let mut start = interval.start_fraction;
        if let Some(previous_intervals) = previous_by_edge.get(&interval.edge_index) {
            for previous in previous_intervals {
                if previous.end_fraction <= start + f64::EPSILON {
                    continue;
                }
                start = start.max(previous.end_fraction);
            }
        }
        if interval.end_fraction > start + f64::EPSILON {
            ring.push(ReachableEdgeInterval {
                start_fraction: start,
                ..interval
            });
        }
    }

    normalize_service_area_segments(ring)
}

fn normalize_service_area_segments(
    mut segments: Vec<ReachableEdgeInterval>,
) -> Vec<ReachableEdgeInterval> {
    segments.sort_by(|left, right| {
        left.edge_index
            .cmp(&right.edge_index)
            .then_with(|| left.start_fraction.total_cmp(&right.start_fraction))
            .then_with(|| left.end_fraction.total_cmp(&right.end_fraction))
    });

    let mut merged: Vec<ReachableEdgeInterval> = Vec::new();
    for segment in segments {
        if let Some(previous) = merged.last_mut() {
            if previous.edge_index == segment.edge_index
                && segment.start_fraction <= previous.end_fraction + 1e-9
            {
                previous.end_fraction = previous.end_fraction.max(segment.end_fraction);
                previous.midpoint_cost = previous.midpoint_cost.min(segment.midpoint_cost);
                continue;
            }
        }
        merged.push(segment);
    }
    merged
}

fn summarize_service_area_segments(
    topology: &TopologyBundle,
    segments: &[ReachableEdgeInterval],
    include_attributes: bool,
) -> (Option<f64>, Option<u64>) {
    if !include_attributes {
        return (None, None);
    }
    let length = segments
        .iter()
        .map(|segment| {
            topology.edges[segment.edge_index].length_m as f64
                * (segment.end_fraction - segment.start_fraction)
        })
        .sum::<f64>();
    let edge_count = segments
        .iter()
        .map(|segment| segment.edge_index)
        .collect::<std::collections::BTreeSet<_>>()
        .len() as u64;
    (Some(length), Some(edge_count))
}

fn merge_service_area_bands(bands: Vec<ServiceAreaOriginBand>) -> Vec<ServiceAreaOriginBand> {
    let mut merged = BTreeMap::<
        (
            Option<String>,
            Option<String>,
            u64,
            ServiceAreaThresholdMetric,
        ),
        Vec<ServiceAreaOriginBand>,
    >::new();
    for band in bands {
        let key = (
            band.threshold_id.clone(),
            band.band_start_limit
                .map(f64::to_bits)
                .map(|bits| bits.to_string()),
            band.threshold_limit.to_bits(),
            band.threshold_metric,
        );
        merged.entry(key).or_default().push(band);
    }

    merged
        .into_values()
        .map(|group| {
            let mut segments = Vec::new();
            let mut fallback_used = false;
            let mut origin_hop_distance_m = None;
            let threshold_id = group[0].threshold_id.clone();
            let band_start_limit = group[0].band_start_limit;
            let threshold_limit = group[0].threshold_limit;
            let threshold_metric = group[0].threshold_metric;
            for band in group {
                fallback_used |= band.fallback_used;
                origin_hop_distance_m = origin_hop_distance_m.or(band.origin_hop_distance_m);
                segments.extend(band.segments);
            }
            ServiceAreaOriginBand {
                origin_id: String::new(),
                origin_component_id: None,
                fallback_used,
                origin_hop_distance_m,
                threshold_id,
                band_start_limit,
                threshold_limit,
                threshold_metric,
                segments: normalize_service_area_segments(segments),
            }
        })
        .collect()
}

fn cut_service_area_bands(bands: Vec<ServiceAreaOriginBand>) -> Vec<ServiceAreaOriginBand> {
    let mut groups = BTreeMap::<
        (
            Option<String>,
            Option<String>,
            u64,
            ServiceAreaThresholdMetric,
        ),
        Vec<ServiceAreaOriginBand>,
    >::new();
    for band in bands {
        let key = (
            band.threshold_id.clone(),
            band.band_start_limit
                .map(f64::to_bits)
                .map(|bits| bits.to_string()),
            band.threshold_limit.to_bits(),
            band.threshold_metric,
        );
        groups.entry(key).or_default().push(band);
    }

    let mut cut = Vec::new();
    for group in groups.into_values() {
        let mut per_origin = BTreeMap::<String, ServiceAreaOriginBand>::new();
        let mut winners = BTreeMap::<usize, (String, ReachableEdgeInterval)>::new();

        for band in group {
            let entry =
                per_origin
                    .entry(band.origin_id.clone())
                    .or_insert_with(|| ServiceAreaOriginBand {
                        origin_id: band.origin_id.clone(),
                        origin_component_id: band.origin_component_id,
                        fallback_used: band.fallback_used,
                        origin_hop_distance_m: band.origin_hop_distance_m,
                        threshold_id: band.threshold_id.clone(),
                        band_start_limit: band.band_start_limit,
                        threshold_limit: band.threshold_limit,
                        threshold_metric: band.threshold_metric,
                        segments: Vec::new(),
                    });
            entry.fallback_used |= band.fallback_used;
            entry.origin_hop_distance_m =
                entry.origin_hop_distance_m.or(band.origin_hop_distance_m);

            for segment in band.segments {
                let winner = winners
                    .entry(segment.edge_index)
                    .or_insert_with(|| (band.origin_id.clone(), segment.clone()));
                if segment.midpoint_cost + f64::EPSILON < winner.1.midpoint_cost
                    || ((segment.midpoint_cost - winner.1.midpoint_cost).abs() <= 1e-9
                        && band.origin_id < winner.0)
                {
                    *winner = (band.origin_id.clone(), segment.clone());
                }
            }
        }

        for (edge_index, (origin_id, winner_segment)) in winners {
            if let Some(band) = per_origin.get_mut(&origin_id) {
                band.segments.push(ReachableEdgeInterval {
                    edge_index,
                    ..winner_segment
                });
            }
        }

        cut.extend(per_origin.into_values().map(|mut band| {
            band.segments = normalize_service_area_segments(band.segments);
            band
        }));
    }

    cut
}

fn build_service_area_features(
    topology: &TopologyBundle,
    request: &ServiceAreaRequest,
    bands: Vec<ServiceAreaOriginBand>,
) -> Vec<ServiceAreaFeature> {
    let mut features = Vec::new();
    for band in bands {
        let (reachable_network_length_m, reachable_edge_count) =
            summarize_service_area_segments(topology, &band.segments, request.returns.attributes);
        let origin_id = (!band.origin_id.is_empty()).then_some(band.origin_id.clone());

        if matches!(
            request.output_mode,
            ServiceAreaOutputMode::Network | ServiceAreaOutputMode::Both
        ) {
            features.push(ServiceAreaFeature {
                origin_id: origin_id.clone(),
                band_start_limit: band.band_start_limit,
                threshold_id: band.threshold_id.clone(),
                threshold_limit: band.threshold_limit,
                threshold_metric: band.threshold_metric,
                geometry_type: ServiceAreaGeometryType::Network,
                fallback_used: band.fallback_used,
                origin_component_id: band.origin_component_id,
                origin_hop_distance_m: band.origin_hop_distance_m,
                reachable_network_length_m,
                reachable_edge_count,
                geometry: request
                    .returns
                    .geometry
                    .then(|| service_area_network_geometry(topology, &band.segments)),
            });
        }

        if matches!(
            request.output_mode,
            ServiceAreaOutputMode::Polygon | ServiceAreaOutputMode::Both
        ) {
            features.push(ServiceAreaFeature {
                origin_id,
                band_start_limit: band.band_start_limit,
                threshold_id: band.threshold_id,
                threshold_limit: band.threshold_limit,
                threshold_metric: band.threshold_metric,
                geometry_type: ServiceAreaGeometryType::Polygon,
                fallback_used: band.fallback_used,
                origin_component_id: band.origin_component_id,
                origin_hop_distance_m: band.origin_hop_distance_m,
                reachable_network_length_m,
                reachable_edge_count,
                geometry: request.returns.geometry.then(|| {
                    service_area_polygon_geometry(topology, &band.segments, &request.polygon)
                }),
            });
        }
    }

    features
}

fn service_area_result_outcome(
    processed_origin_count: usize,
    skipped_origin_count: usize,
    fallback_origin_count: usize,
) -> AnalysisOutcome {
    if processed_origin_count == 0 {
        AnalysisOutcome::Unreachable
    } else if skipped_origin_count > 0 {
        AnalysisOutcome::Partial
    } else if fallback_origin_count > 0 {
        AnalysisOutcome::Degraded
    } else {
        AnalysisOutcome::Legal
    }
}

fn service_area_network_geometry(
    topology: &TopologyBundle,
    segments: &[ReachableEdgeInterval],
) -> serde_json::Value {
    let coordinates = segments
        .iter()
        .map(|segment| service_area_segment_coords(topology, segment))
        .collect::<Vec<_>>();
    serde_json::json!({
        "type": "MultiLineString",
        "coordinates": coordinates,
    })
}

fn service_area_polygon_geometry(
    topology: &TopologyBundle,
    segments: &[ReachableEdgeInterval],
    options: &ServiceAreaPolygonOptions,
) -> serde_json::Value {
    let buffer_width_m = (25.0 * options.hull_aggressiveness.max(0.25)).max(5.0);
    let mut per_component = BTreeMap::<u32, Vec<[f64; 2]>>::new();

    for segment in segments {
        let component_id = topology
            .edge_component_id(segment.edge_index as u32)
            .unwrap_or_default();
        let coords = service_area_segment_coords(topology, segment);
        let midpoint_lat = (coords[0][1] + coords[1][1]) / 2.0;
        per_component
            .entry(component_id)
            .or_default()
            .extend(buffered_segment_corners(
                coords[0],
                coords[1],
                buffer_width_m,
                midpoint_lat,
            ));
    }

    if per_component.is_empty() {
        return serde_json::json!({
            "type": "MultiPolygon",
            "coordinates": Vec::<Vec<Vec<[f64; 2]>>>::new(),
        });
    }

    let tolerance = options.simplification_tolerance_m.unwrap_or(0.0);
    let polygons = per_component
        .into_values()
        .filter_map(|points| {
            let hull = convex_hull(points);
            simplify_polygon_ring(hull, tolerance)
        })
        .collect::<Vec<_>>();

    if polygons.len() == 1 {
        serde_json::json!({
            "type": "Polygon",
            "coordinates": [polygons[0].clone()],
        })
    } else {
        serde_json::json!({
            "type": "MultiPolygon",
            "coordinates": polygons.into_iter().map(|ring| vec![ring]).collect::<Vec<_>>(),
        })
    }
}

fn service_area_segment_coords(
    topology: &TopologyBundle,
    segment: &ReachableEdgeInterval,
) -> Vec<[f64; 2]> {
    let edge = &topology.edges[segment.edge_index];
    let from_node = &topology.nodes[edge.from.0 as usize];
    let to_node = &topology.nodes[edge.to.0 as usize];
    vec![
        interpolate_edge_point(
            from_node.lon,
            from_node.lat,
            to_node.lon,
            to_node.lat,
            segment.start_fraction,
        ),
        interpolate_edge_point(
            from_node.lon,
            from_node.lat,
            to_node.lon,
            to_node.lat,
            segment.end_fraction,
        ),
    ]
}

fn interpolate_edge_point(
    from_lon: f64,
    from_lat: f64,
    to_lon: f64,
    to_lat: f64,
    fraction: f64,
) -> [f64; 2] {
    [
        from_lon + (to_lon - from_lon) * fraction,
        from_lat + (to_lat - from_lat) * fraction,
    ]
}

fn buffered_segment_corners(
    start: [f64; 2],
    end: [f64; 2],
    buffer_width_m: f64,
    at_lat: f64,
) -> Vec<[f64; 2]> {
    let dx_m = longitude_delta_to_meters(end[0] - start[0], at_lat);
    let dy_m = latitude_delta_to_meters(end[1] - start[1]);
    let length_m = (dx_m * dx_m + dy_m * dy_m).sqrt();
    let (unit_px, unit_py) = if length_m <= 1e-6 {
        (0.0, 1.0)
    } else {
        (-dy_m / length_m, dx_m / length_m)
    };
    let offset_lon = meters_to_longitude_delta(unit_px * buffer_width_m, at_lat);
    let offset_lat = meters_to_latitude_delta(unit_py * buffer_width_m);
    vec![
        [start[0] + offset_lon, start[1] + offset_lat],
        [start[0] - offset_lon, start[1] - offset_lat],
        [end[0] - offset_lon, end[1] - offset_lat],
        [end[0] + offset_lon, end[1] + offset_lat],
    ]
}

fn convex_hull(mut points: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    points.sort_by(|left, right| {
        left[0]
            .total_cmp(&right[0])
            .then_with(|| left[1].total_cmp(&right[1]))
    });
    points.dedup_by(|left, right| {
        (left[0] - right[0]).abs() <= 1e-12 && (left[1] - right[1]).abs() <= 1e-12
    });

    if points.len() <= 1 {
        return points;
    }

    let mut lower = Vec::<[f64; 2]>::new();
    for point in &points {
        while lower.len() >= 2
            && cross(lower[lower.len() - 2], lower[lower.len() - 1], *point) <= 0.0
        {
            lower.pop();
        }
        lower.push(*point);
    }

    let mut upper = Vec::<[f64; 2]>::new();
    for point in points.iter().rev() {
        while upper.len() >= 2
            && cross(upper[upper.len() - 2], upper[upper.len() - 1], *point) <= 0.0
        {
            upper.pop();
        }
        upper.push(*point);
    }

    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

fn cross(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

fn simplify_polygon_ring(mut ring: Vec<[f64; 2]>, tolerance_m: f64) -> Option<Vec<[f64; 2]>> {
    if ring.is_empty() {
        return None;
    }
    if tolerance_m > 0.0 {
        let mut simplified = Vec::new();
        for point in ring {
            if simplified.last().is_none_or(|previous: &[f64; 2]| {
                haversine_meters(previous[0], previous[1], point[0], point[1]) >= tolerance_m
            }) {
                simplified.push(point);
            }
        }
        ring = simplified;
    }
    if ring.len() == 1 {
        let point = ring[0];
        let offset_lon = meters_to_longitude_delta(5.0, point[1]);
        let offset_lat = meters_to_latitude_delta(5.0);
        ring = vec![
            [point[0] - offset_lon, point[1] - offset_lat],
            [point[0] + offset_lon, point[1] - offset_lat],
            [point[0] + offset_lon, point[1] + offset_lat],
            [point[0] - offset_lon, point[1] + offset_lat],
        ];
    } else if ring.len() == 2 {
        let corners =
            buffered_segment_corners(ring[0], ring[1], 5.0, (ring[0][1] + ring[1][1]) / 2.0);
        ring = convex_hull(corners);
    }
    if ring.len() < 3 {
        return None;
    }
    if ring.first() != ring.last() {
        ring.push(ring[0]);
    }
    Some(ring)
}

fn longitude_delta_to_meters(delta_lon: f64, at_lat: f64) -> f64 {
    delta_lon * 111_320.0 * at_lat.to_radians().cos().abs().max(0.01)
}

fn latitude_delta_to_meters(delta_lat: f64) -> f64 {
    delta_lat * 110_540.0
}

fn meters_to_longitude_delta(meters: f64, at_lat: f64) -> f64 {
    meters / (111_320.0 * at_lat.to_radians().cos().abs().max(0.01))
}

fn meters_to_latitude_delta(meters: f64) -> f64 {
    meters / 110_540.0
}

pub fn execute_route(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &RouteRequest,
) -> Result<RouteResult> {
    execute_route_with_optional_edge_names(topology, metrics, request, None)
}

pub fn execute_route_with_edge_names(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &RouteRequest,
    edge_names: &[String],
) -> Result<RouteResult> {
    execute_route_with_optional_edge_names(topology, metrics, request, Some(edge_names))
}

fn execute_route_with_optional_edge_names(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &RouteRequest,
    edge_names: Option<&[String]>,
) -> Result<RouteResult> {
    if has_failure_modes(&request.fallback) {
        validate_execution_inputs(topology, metrics)?;
        let (degraded_topology, degraded_metrics, degraded_routing_graph) =
            build_failure_mode_bundle(topology, metrics, &request.fallback)?;
        return execute_route_with_graph(
            &degraded_topology,
            &degraded_metrics,
            &degraded_routing_graph,
            request,
            edge_names,
        );
    }
    validate_execution_inputs(topology, metrics)?;
    let routing_graph = build_routing_graph(topology, metrics)?;
    execute_route_with_graph(topology, metrics, &routing_graph, request, edge_names)
}

pub fn execute_od(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    document: &OdPairsDocument,
) -> Result<OdResult> {
    if has_failure_modes(&document.fallback) {
        validate_execution_inputs(topology, metrics)?;
        let (degraded_topology, degraded_metrics, degraded_routing_graph) =
            build_failure_mode_bundle(topology, metrics, &document.fallback)?;
        return execute_od_with_graph(
            &degraded_topology,
            &degraded_metrics,
            &degraded_routing_graph,
            document,
        );
    }
    validate_execution_inputs(topology, metrics)?;
    let routing_graph = build_routing_graph(topology, metrics)?;
    execute_od_with_graph(topology, metrics, &routing_graph, document)
}

pub fn execute_matrix(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
) -> Result<MatrixResult> {
    let fallback = merge_point_set_fallback_policy(&origins.fallback, &destinations.fallback);
    if has_failure_modes(&fallback) {
        validate_execution_inputs(topology, metrics)?;
        let (degraded_topology, degraded_metrics, degraded_routing_graph) =
            build_failure_mode_bundle(topology, metrics, &fallback)?;
        return execute_matrix_with_graph(
            &degraded_topology,
            &degraded_metrics,
            &degraded_routing_graph,
            origins,
            destinations,
        );
    }
    validate_execution_inputs(topology, metrics)?;
    let routing_graph = build_routing_graph(topology, metrics)?;
    execute_matrix_with_graph(topology, metrics, &routing_graph, origins, destinations)
}

fn validate_execution_inputs(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
) -> Result<()> {
    if topology.edges.len() != metrics.edge_metrics.len() {
        bail!(
            "topology edge count ({}) does not match compiled metric count ({})",
            topology.edges.len(),
            metrics.edge_metrics.len()
        );
    }
    Ok(())
}

fn has_failure_modes(fallback: &FallbackPolicy) -> bool {
    fallback.allow_reverse_oneway
        || fallback.allow_illegal_turn
        || fallback.ignore_turn_restrictions
        || fallback.allow_uturn_where_normally_forbidden
}

fn auto_relaxation_requested(fallback: &FallbackPolicy) -> bool {
    fallback.auto_relax_unreachable
}

fn fallback_without_auto_relaxation(fallback: &FallbackPolicy) -> FallbackPolicy {
    let mut sanitized = fallback.clone();
    sanitized.auto_relax_unreachable = false;
    sanitized
}

fn auto_relaxed_seed_fallback(fallback: &FallbackPolicy) -> FallbackPolicy {
    let mut seed = fallback_without_auto_relaxation(fallback);
    seed.allow_reverse_oneway = true;
    seed.allow_illegal_turn = true;
    seed.ignore_turn_restrictions = true;
    seed.allow_uturn_where_normally_forbidden = true;
    seed.penalties.reverse_oneway_penalty_s =
        seed.penalties.reverse_oneway_penalty_s.or(Some(120.0));
    seed.penalties.illegal_turn_penalty_s = seed.penalties.illegal_turn_penalty_s.or(Some(90.0));
    seed.penalties.ignored_turn_restriction_penalty_s = seed
        .penalties
        .ignored_turn_restriction_penalty_s
        .or(Some(180.0));
    seed.penalties.forbidden_uturn_penalty_s =
        seed.penalties.forbidden_uturn_penalty_s.or(Some(60.0));
    seed
}

fn auto_selected_fallback(
    requested: &FallbackPolicy,
    seed: &FallbackPolicy,
    route: &RouteResult,
) -> FallbackPolicy {
    let mut selected = fallback_without_auto_relaxation(requested);
    for violation in &route.violations {
        match violation.violation_type {
            RouteViolationType::ReverseOneway => {
                selected.allow_reverse_oneway = true;
            }
            RouteViolationType::IllegalTurn => {
                selected.allow_illegal_turn = true;
            }
            RouteViolationType::IgnoredTurnRestriction => {
                selected.ignore_turn_restrictions = true;
            }
            RouteViolationType::ForbiddenUturn => {
                selected.allow_uturn_where_normally_forbidden = true;
            }
        }
    }

    if selected.allow_reverse_oneway {
        selected.penalties.reverse_oneway_penalty_s = seed.penalties.reverse_oneway_penalty_s;
    }
    if selected.allow_illegal_turn {
        selected.penalties.illegal_turn_penalty_s = seed.penalties.illegal_turn_penalty_s;
    }
    if selected.ignore_turn_restrictions {
        selected.penalties.ignored_turn_restriction_penalty_s =
            seed.penalties.ignored_turn_restriction_penalty_s;
    }
    if selected.allow_uturn_where_normally_forbidden {
        selected.penalties.forbidden_uturn_penalty_s = seed.penalties.forbidden_uturn_penalty_s;
    }

    selected
}

fn fallback_mode_labels(fallback: &FallbackPolicy) -> Vec<&'static str> {
    let mut labels = Vec::new();
    if fallback.allow_uturn_where_normally_forbidden {
        labels.push("allow_uturn_where_normally_forbidden");
    }
    if fallback.allow_illegal_turn {
        labels.push("allow_illegal_turn");
    }
    if fallback.allow_reverse_oneway {
        labels.push("allow_reverse_oneway");
    }
    if fallback.ignore_turn_restrictions {
        labels.push("ignore_turn_restrictions");
    }
    labels
}

fn build_failure_mode_bundle(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    fallback: &FallbackPolicy,
) -> Result<(TopologyBundle, CompiledProfileBundle, RoutingGraph)> {
    let mut degraded_topology = topology.clone();
    let mut degraded_metrics = metrics.clone();
    degraded_metrics.acceleration = None;
    let mut virtual_reverse_of = vec![None; degraded_topology.edges.len()];

    if fallback.allow_reverse_oneway {
        let existing_edges = degraded_topology
            .edges
            .iter()
            .enumerate()
            .filter(|(edge_index, _)| {
                degraded_metrics
                    .edge_metrics
                    .get(*edge_index)
                    .and_then(|metric| metric.generalized_cost)
                    .is_some()
            })
            .map(|(_, edge)| (edge.from.0, edge.to.0, edge.source_way_id))
            .collect::<std::collections::BTreeSet<_>>();
        let original_edge_count = degraded_topology.edges.len();
        for edge_index in 0..original_edge_count {
            let edge = &degraded_topology.edges[edge_index];
            let metric = &degraded_metrics.edge_metrics[edge_index];
            if metric.generalized_cost.is_none() || metric.travel_time_s.is_none() {
                continue;
            }
            if existing_edges.contains(&(edge.to.0, edge.from.0, edge.source_way_id)) {
                continue;
            }
            degraded_topology.edges.push(DirectedEdge {
                edge_id: edge.edge_id,
                from: edge.to,
                to: edge.from,
                source_way_id: edge.source_way_id,
                length_m: edge.length_m,
                duration_s: edge.duration_s,
                road_class: edge.road_class,
                surface: edge.surface,
                smoothness: edge.smoothness,
                access_mask: edge.access_mask,
                is_toll: edge.is_toll,
                name_index: edge.name_index,
                geometry_offset: edge.geometry_offset,
                geometry_len: edge.geometry_len,
                flags: 0,
            });
            degraded_metrics.edge_metrics.push(CompiledEdgeMetric {
                edge_id: metric.edge_id,
                travel_time_s: metric.travel_time_s,
                generalized_cost: metric.generalized_cost,
            });
            degraded_topology.edge_component_ids.push(
                topology
                    .edge_component_id(edge_index as u32)
                    .unwrap_or_default(),
            );
            virtual_reverse_of.push(Some(edge_index));
        }
    }

    degraded_topology.edge_based_topology = build_edge_based_topology_fallback(&degraded_topology);
    let mut routing_graph = build_routing_graph_with_options(
        &degraded_topology,
        &degraded_metrics,
        RoutingGraphBuildOptions {
            search_time_turn_restrictions: true,
            ..RoutingGraphBuildOptions::default()
        },
    )?;
    routing_graph.acceleration = None;
    routing_graph.virtual_reverse_of = virtual_reverse_of;
    Ok((degraded_topology, degraded_metrics, routing_graph))
}

fn execute_route_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &RouteRequest,
    edge_names: Option<&[String]>,
) -> Result<RouteResult> {
    let search_distance_m = request
        .connectivity
        .max_hop_distance_m
        .unwrap_or(request.snap.max_distance_m)
        .max(request.snap.max_distance_m);
    let origin_candidates = snap_candidates(
        topology,
        routing_graph,
        &request.origin,
        search_distance_m,
        true,
    )?;
    let destination_candidates = snap_candidates(
        topology,
        routing_graph,
        &request.destination,
        search_distance_m,
        false,
    )?;
    execute_route_with_candidates(
        topology,
        metrics,
        routing_graph,
        &request.route_id,
        request.snap.max_distance_m,
        &request.connectivity,
        &request.fallback,
        &request.returns,
        &origin_candidates,
        &destination_candidates,
        edge_names,
    )
}

fn execute_route_with_candidates(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    route_id: &str,
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    fallback: &FallbackPolicy,
    returns: &ReturnConfig,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    edge_names: Option<&[String]>,
) -> Result<RouteResult> {
    if auto_relaxation_requested(fallback) && !has_failure_modes(fallback) {
        let strict_fallback = fallback_without_auto_relaxation(fallback);
        if let Err(error) = route_between_candidates(
            topology,
            metrics,
            routing_graph,
            snap_max_distance_m,
            connectivity,
            &strict_fallback,
            origin_candidates,
            destination_candidates,
        ) {
            if let Some(result) = try_auto_relaxed_route_with_candidates(
                topology,
                metrics,
                route_id,
                snap_max_distance_m,
                connectivity,
                fallback,
                returns,
                origin_candidates,
                destination_candidates,
                edge_names,
            )? {
                return Ok(result);
            }
            if matches!(
                connectivity.disconnected,
                DisconnectedNetworkMode::IgnoreUnreachable
            ) {
                if let Some(failure) = analysis_failure(&error) {
                    if matches!(failure.outcome, AnalysisOutcome::Unreachable) {
                        return Ok(ignored_unreachable_route_result(
                            route_id,
                            origin_candidates,
                            destination_candidates,
                            failure.clone(),
                            execution_warnings(metrics),
                        ));
                    }
                }
            }
            return Err(error);
        }
    }

    let (origin, destination, path, hop_info) = match route_between_candidates(
        topology,
        metrics,
        routing_graph,
        snap_max_distance_m,
        connectivity,
        fallback,
        origin_candidates,
        destination_candidates,
    ) {
        Ok(result) => result,
        Err(error)
            if matches!(
                connectivity.disconnected,
                DisconnectedNetworkMode::IgnoreUnreachable
            ) =>
        {
            if let Some(failure) = analysis_failure(&error) {
                if matches!(failure.outcome, AnalysisOutcome::Unreachable) {
                    return Ok(ignored_unreachable_route_result(
                        route_id,
                        origin_candidates,
                        destination_candidates,
                        failure.clone(),
                        execution_warnings(metrics),
                    ));
                }
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };

    let include_detailed_paths = request_returns_detailed_path(returns);
    let needs_node_path =
        include_detailed_paths || !matches!(returns.geometry, netan_profile::ReturnGeometry::None);
    let node_path = if needs_node_path {
        let mut node_path = Vec::with_capacity(path.edge_indexes.len() + 1);
        if let Some(&first_edge) = path.edge_indexes.first() {
            node_path.push(topology.edges[first_edge].from.0);
            for &edge_index in &path.edge_indexes {
                node_path.push(topology.edges[edge_index].to.0);
            }
        } else if origin.snapped_edge_id.is_none() {
            node_path.push(origin.snapped_node_id);
        }
        node_path
    } else {
        Vec::new()
    };

    let geometry = match returns.geometry {
        netan_profile::ReturnGeometry::None => None,
        _ => Some(build_route_geometry(
            topology,
            &path.edge_indexes,
            &origin,
            &destination,
        )),
    };

    let segments = if returns.segment_rows {
        let edge_names = edge_names.unwrap_or(&topology.names);
        Some(
            path.edge_indexes
                .iter()
                .map(|&edge_index| {
                    let edge = &topology.edges[edge_index];
                    let metric = &metrics.edge_metrics[edge_index];
                    let factor = edge_traversal_factor(
                        edge_index,
                        path.edge_indexes.first().copied(),
                        path.edge_indexes.last().copied(),
                        &origin,
                        &destination,
                    );
                    RouteSegment {
                        edge_id: edge.edge_id.0,
                        from_node_id: edge.from.0,
                        to_node_id: edge.to.0,
                        source_way_id: edge.source_way_id,
                        length_m: (edge.length_m as f64 * factor).round() as u32,
                        travel_time_s: metric.travel_time_s.unwrap_or_default() * factor,
                        generalized_cost: metric.generalized_cost.unwrap_or_default() * factor,
                        road_class: edge.road_class,
                        surface: edge.surface,
                        name: edge
                            .name_index
                            .and_then(|index| edge_names.get(index as usize))
                            .cloned(),
                        violation_type: None,
                    }
                })
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };

    let analysis = analyze_route_path(
        topology,
        metrics,
        routing_graph,
        fallback,
        &path,
        &origin,
        &destination,
    )?;
    let mut segments = segments;
    if let Some(segment_rows) = segments.as_mut() {
        for (segment, edge_index) in segment_rows.iter_mut().zip(&path.edge_indexes) {
            segment.violation_type = analysis
                .segment_violation_types
                .get(edge_index)
                .copied()
                .flatten();
        }
    }
    let breakdowns = build_breakdowns(topology, metrics, &path.edge_indexes, returns);
    let warnings = execution_warnings(metrics);

    Ok(RouteResult {
        route_id: route_id.to_string(),
        origin,
        destination,
        outcome: if !analysis.violations.is_empty() || hop_info.fallback_used {
            AnalysisOutcome::Degraded
        } else {
            AnalysisOutcome::Legal
        },
        fallback_used: hop_info.fallback_used || !analysis.violations.is_empty(),
        origin_hop_distance_m: hop_info.origin_hop_distance_m,
        destination_hop_distance_m: hop_info.destination_hop_distance_m,
        summary: analysis.summary,
        node_path,
        edge_path: if include_detailed_paths {
            path.edge_indexes
                .iter()
                .map(|&edge_index| topology.edges[edge_index].edge_id.0)
                .collect()
        } else {
            Vec::new()
        },
        geometry,
        hop_segments: hop_info.hop_segments,
        segments,
        breakdowns,
        violations: analysis.violations,
        diagnostics: hop_info.diagnostics,
        warnings: merge_warnings(
            merge_warnings(warnings, analysis.warnings),
            hop_info.warnings,
        ),
    })
}

fn try_auto_relaxed_route_with_candidates(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    route_id: &str,
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    fallback: &FallbackPolicy,
    returns: &ReturnConfig,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    edge_names: Option<&[String]>,
) -> Result<Option<RouteResult>> {
    let seed_fallback = auto_relaxed_seed_fallback(fallback);
    let (degraded_topology, degraded_metrics, degraded_routing_graph) =
        build_failure_mode_bundle(topology, metrics, &seed_fallback)?;
    let seed_result = match execute_route_with_candidates(
        &degraded_topology,
        &degraded_metrics,
        &degraded_routing_graph,
        route_id,
        snap_max_distance_m,
        connectivity,
        &seed_fallback,
        returns,
        origin_candidates,
        destination_candidates,
        edge_names,
    ) {
        Ok(result) => result,
        Err(error) => {
            if analysis_failure(&error)
                .is_some_and(|failure| matches!(failure.outcome, AnalysisOutcome::Unreachable))
            {
                return Ok(None);
            }
            return Err(error);
        }
    };

    let selected_fallback = auto_selected_fallback(fallback, &seed_fallback, &seed_result);
    let mut result = if selected_fallback == seed_fallback {
        seed_result
    } else {
        let (selected_topology, selected_metrics, selected_routing_graph) =
            build_failure_mode_bundle(topology, metrics, &selected_fallback)?;
        execute_route_with_candidates(
            &selected_topology,
            &selected_metrics,
            &selected_routing_graph,
            route_id,
            snap_max_distance_m,
            connectivity,
            &selected_fallback,
            returns,
            origin_candidates,
            destination_candidates,
            edge_names,
        )?
    };

    let selected_labels = fallback_mode_labels(&selected_fallback);
    if !selected_labels.is_empty() {
        result.diagnostics.push(AnalysisDiagnostic {
            code: AnalysisDiagnosticCode::FallbackUsed,
            severity: AnalysisDiagnosticSeverity::Warning,
            message: format!(
                "Auto fallback resolved an unreachable strict route by enabling {}.",
                selected_labels.join(", ")
            ),
            point_ids: vec![result.origin.point_id.clone(), result.destination.point_id.clone()],
            component_ids: Vec::new(),
            suggested_actions: vec![
                "Review the returned violations and fallback_used fields before using this route for strict legal-network analysis.".to_string(),
            ],
        });
        result.warnings.push(format!(
            "Auto fallback enabled {} after the strict route was unreachable.",
            selected_labels.join(", ")
        ));
    }

    Ok(Some(result))
}

fn request_returns_detailed_path(returns: &ReturnConfig) -> bool {
    !matches!(returns.geometry, netan_profile::ReturnGeometry::None)
        || returns.segment_rows
        || !returns.road_type_breakdown.is_empty()
        || !returns.surface_breakdown.is_empty()
        || returns.penalty_breakdown
        || returns.explain_cost_derivation
}

#[derive(Debug, Clone)]
struct HopSelectionInfo {
    fallback_used: bool,
    origin_hop_distance_m: Option<f64>,
    destination_hop_distance_m: Option<f64>,
    hop_segments: Vec<RouteHopSegment>,
    diagnostics: Vec<AnalysisDiagnostic>,
    warnings: Vec<String>,
}

struct RoutePathAnalysis {
    summary: RouteSummary,
    violations: Vec<RouteViolation>,
    segment_violation_types: HashMap<usize, Option<RouteViolationType>>,
    warnings: Vec<String>,
}

fn merge_warnings(mut warnings: Vec<String>, extra: Vec<String>) -> Vec<String> {
    warnings.extend(extra);
    warnings
}

fn analyze_route_path(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    fallback: &FallbackPolicy,
    path: &RoutePath,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> Result<RoutePathAnalysis> {
    let mut network_distance_m = 0_u64;
    let mut network_travel_time_s = 0.0;
    let mut network_generalized_cost = 0.0;
    let mut penalty_s = 0.0;
    let mut penalty_cost = 0.0;
    let mut previous_edge_index = None;
    let mut automaton_state = 0_usize;
    let mut violations = Vec::new();
    let mut violation_types = std::collections::BTreeSet::<RouteViolationType>::new();
    let mut segment_violation_types = HashMap::<usize, Option<RouteViolationType>>::new();
    let first_edge = path.edge_indexes.first().copied();
    let last_edge = path.edge_indexes.last().copied();

    for &edge_index in &path.edge_indexes {
        let edge = &topology.edges[edge_index];
        let metric = &metrics.edge_metrics[edge_index];
        let factor = edge_traversal_factor(edge_index, first_edge, last_edge, origin, destination);
        network_distance_m += (edge.length_m as f64 * factor).round() as u64;
        network_travel_time_s += metric.travel_time_s.unwrap_or_default() * factor;
        network_generalized_cost += metric.generalized_cost.unwrap_or_default() * factor;

        if let Some(original_edge_index) = routing_graph
            .virtual_reverse_of
            .get(edge_index)
            .and_then(|value| *value)
        {
            let route_violation_type = RouteViolationType::ReverseOneway;
            let edge_penalty_s = fallback
                .penalties
                .reverse_oneway_penalty_s
                .unwrap_or_default();
            let edge_penalty_cost = edge_penalty_s * metrics.turn_costs.cost_time_weight;
            penalty_s += edge_penalty_s;
            penalty_cost += edge_penalty_cost;
            violations.push(RouteViolation {
                violation_type: route_violation_type,
                edge_id: Some(topology.edges[original_edge_index].edge_id.0),
                from_edge_id: None,
                to_edge_id: None,
                distance_m: Some(edge.length_m as f64 * factor),
                penalty_s: edge_penalty_s,
                penalty_generalized_cost: edge_penalty_cost,
            });
            violation_types.insert(route_violation_type);
            segment_violation_types.insert(edge_index, Some(route_violation_type));
        } else {
            segment_violation_types.entry(edge_index).or_insert(None);
        }

        if let Some(previous_edge_index) = previous_edge_index {
            let turn_penalty_s =
                turn_penalty_seconds(topology, metrics, previous_edge_index, edge_index);
            network_travel_time_s += turn_penalty_s;
            network_generalized_cost += turn_penalty_s * metrics.turn_costs.cost_time_weight;
            if let Some(sequence_len) = routing_graph
                .automaton
                .prohibited_sequence_len(automaton_state, edge_index)
            {
                let (violation_type, local_penalty_s) = if fallback.ignore_turn_restrictions {
                    (
                        RouteViolationType::IgnoredTurnRestriction,
                        fallback
                            .penalties
                            .ignored_turn_restriction_penalty_s
                            .unwrap_or_default(),
                    )
                } else if sequence_len == 2
                    && classify_turn(topology, previous_edge_index, edge_index)
                        == TurnDirection::Uturn
                    && fallback.allow_uturn_where_normally_forbidden
                {
                    (
                        RouteViolationType::ForbiddenUturn,
                        fallback
                            .penalties
                            .forbidden_uturn_penalty_s
                            .or(fallback.penalties.illegal_turn_penalty_s)
                            .unwrap_or_default(),
                    )
                } else if sequence_len == 2 && fallback.allow_illegal_turn {
                    (
                        RouteViolationType::IllegalTurn,
                        fallback
                            .penalties
                            .illegal_turn_penalty_s
                            .unwrap_or_default(),
                    )
                } else {
                    (RouteViolationType::IgnoredTurnRestriction, 0.0)
                };
                let local_penalty_cost = local_penalty_s * metrics.turn_costs.cost_time_weight;
                penalty_s += local_penalty_s;
                penalty_cost += local_penalty_cost;
                violations.push(RouteViolation {
                    violation_type,
                    edge_id: None,
                    from_edge_id: Some(topology.edges[previous_edge_index].edge_id.0),
                    to_edge_id: Some(topology.edges[edge_index].edge_id.0),
                    distance_m: None,
                    penalty_s: local_penalty_s,
                    penalty_generalized_cost: local_penalty_cost,
                });
                violation_types.insert(violation_type);
            }
        }
        automaton_state = routing_graph
            .automaton
            .transition(automaton_state, edge_index);
        previous_edge_index = Some(edge_index);
    }

    let reverse_distance_m = violations
        .iter()
        .filter(|violation| violation.violation_type == RouteViolationType::ReverseOneway)
        .filter_map(|violation| violation.distance_m)
        .sum::<f64>();
    if let Some(max_illegal_distance_m) = fallback.max_illegal_distance_m {
        if reverse_distance_m > max_illegal_distance_m + f64::EPSILON {
            return Err(AnalysisFailure::new(
                format!(
                    "degraded route exceeds fallback.max_illegal_distance_m ({:.1} m > {:.1} m)",
                    reverse_distance_m, max_illegal_distance_m
                ),
                AnalysisOutcome::Unreachable,
                vec![AnalysisDiagnostic {
                    code: AnalysisDiagnosticCode::FallbackUsed,
                    severity: AnalysisDiagnosticSeverity::Error,
                    message: format!(
                        "The selected degraded route required {:.1} m of reverse-oneway travel, exceeding fallback.max_illegal_distance_m={:.1}.",
                        reverse_distance_m, max_illegal_distance_m
                    ),
                    point_ids: vec![origin.point_id.clone(), destination.point_id.clone()],
                    component_ids: Vec::new(),
                    suggested_actions: vec![
                        "Increase fallback.max_illegal_distance_m only if this degraded behavior is still acceptable.".to_string(),
                        "Disable allow_reverse_oneway to keep the route strictly legal.".to_string(),
                    ],
                }],
            )
            .into());
        }
    }
    if let Some(max_illegal_turns) = fallback.max_illegal_turns {
        let illegal_turn_count = violations
            .iter()
            .filter(|violation| violation.violation_type != RouteViolationType::ReverseOneway)
            .count() as u32;
        if illegal_turn_count > max_illegal_turns {
            return Err(AnalysisFailure::new(
                format!(
                    "degraded route exceeds fallback.max_illegal_turns ({} > {})",
                    illegal_turn_count, max_illegal_turns
                ),
                AnalysisOutcome::Unreachable,
                vec![AnalysisDiagnostic {
                    code: AnalysisDiagnosticCode::FallbackUsed,
                    severity: AnalysisDiagnosticSeverity::Error,
                    message: format!(
                        "The selected degraded route required {} illegal turn or restriction override(s), exceeding fallback.max_illegal_turns={}.",
                        illegal_turn_count, max_illegal_turns
                    ),
                    point_ids: vec![origin.point_id.clone(), destination.point_id.clone()],
                    component_ids: Vec::new(),
                    suggested_actions: vec![
                        "Increase fallback.max_illegal_turns only if this degraded behavior is still acceptable.".to_string(),
                        "Disable turn-related fallback flags to keep the route strictly legal.".to_string(),
                    ],
                }],
            )
            .into());
        }
    }

    let mut warnings = Vec::new();
    if has_failure_modes(fallback) {
        if violations.is_empty() {
            warnings.push(
                "Unsafe failure-mode flags were enabled for this request, but the returned route did not require illegal movements."
                    .to_string(),
            );
        } else {
            warnings.push(
                "Unsafe failure-mode fallback was used; this route is degraded and noncompliant for strict legal routing analysis."
                    .to_string(),
            );
        }
    }

    Ok(RoutePathAnalysis {
        summary: RouteSummary {
            network_distance_m,
            network_travel_time_s,
            network_generalized_cost,
            illegal_movement_penalty_s: penalty_s,
            illegal_movement_penalty_cost: penalty_cost,
            violation_count: violations.len(),
            violation_types: violation_types.into_iter().collect(),
            total_distance_m: network_distance_m,
            total_travel_time_s: network_travel_time_s + penalty_s,
            total_generalized_cost: network_generalized_cost + penalty_cost,
            segment_count: path.edge_indexes.len(),
        },
        violations,
        segment_violation_types,
        warnings,
    })
}

fn ignored_unreachable_warning() -> String {
    "Connectivity policy ignored an unreachable pair; no legal path geometry or network cost was returned.".to_string()
}

fn batch_ignored_unreachable_warning(label: &str, ignored_count: usize) -> String {
    format!(
        "Connectivity policy ignored {} unreachable {} item(s); see per-item status='ignored' and outcome='partial'.",
        ignored_count, label
    )
}

fn ignored_unreachable_route_result(
    route_id: &str,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    failure: AnalysisFailure,
    warnings: Vec<String>,
) -> RouteResult {
    let origin = origin_candidates
        .first()
        .cloned()
        .expect("successful snapping must produce at least one origin candidate");
    let destination = destination_candidates
        .first()
        .cloned()
        .expect("successful snapping must produce at least one destination candidate");
    RouteResult {
        route_id: route_id.to_string(),
        origin,
        destination,
        outcome: AnalysisOutcome::Partial,
        fallback_used: false,
        origin_hop_distance_m: None,
        destination_hop_distance_m: None,
        summary: RouteSummary {
            network_distance_m: 0,
            network_travel_time_s: 0.0,
            network_generalized_cost: 0.0,
            illegal_movement_penalty_s: 0.0,
            illegal_movement_penalty_cost: 0.0,
            violation_count: 0,
            violation_types: Vec::new(),
            total_distance_m: 0,
            total_travel_time_s: 0.0,
            total_generalized_cost: 0.0,
            segment_count: 0,
        },
        node_path: Vec::new(),
        edge_path: Vec::new(),
        geometry: None,
        hop_segments: Vec::new(),
        segments: None,
        breakdowns: None,
        violations: Vec::new(),
        diagnostics: failure.diagnostics,
        warnings: merge_warnings(warnings, vec![ignored_unreachable_warning()]),
    }
}

fn batch_status_for_route(route: &RouteResult) -> BatchItemStatus {
    if matches!(route.outcome, AnalysisOutcome::Partial) {
        BatchItemStatus::Ignored
    } else {
        BatchItemStatus::Succeeded
    }
}

fn batch_ignored_message(route: &RouteResult) -> Option<String> {
    matches!(route.outcome, AnalysisOutcome::Partial).then(|| {
        route
            .diagnostics
            .first()
            .map(|diagnostic| diagnostic.message.clone())
            .unwrap_or_else(|| "Connectivity policy ignored an unreachable pair.".to_string())
    })
}

fn hop_info_for_pair(
    origin: &SnappedPoint,
    destination: &SnappedPoint,
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
) -> Option<HopSelectionInfo> {
    let origin_requires_hop = origin.snap_distance_m > snap_max_distance_m;
    let destination_requires_hop = destination.snap_distance_m > snap_max_distance_m;
    let max_hop_distance_m = connectivity
        .max_hop_distance_m
        .unwrap_or(snap_max_distance_m);

    if (origin_requires_hop && origin.snap_distance_m > max_hop_distance_m)
        || (destination_requires_hop && destination.snap_distance_m > max_hop_distance_m)
    {
        return None;
    }

    let allowed = match connectivity.disconnected {
        DisconnectedNetworkMode::Strict | DisconnectedNetworkMode::IgnoreUnreachable => {
            !origin_requires_hop && !destination_requires_hop
        }
        DisconnectedNetworkMode::HopOriginToNearestReachableComponent => !destination_requires_hop,
        DisconnectedNetworkMode::HopDestinationToNearestReachableComponent => !origin_requires_hop,
        DisconnectedNetworkMode::HopEitherEnd => !(origin_requires_hop && destination_requires_hop),
    };
    if !allowed {
        return None;
    }

    let mut hop_segments = Vec::new();
    let mut diagnostics = Vec::new();
    let mut warnings = Vec::new();
    if origin_requires_hop {
        hop_segments.push(RouteHopSegment {
            endpoint: HopEndpoint::Origin,
            distance_m: origin.snap_distance_m,
            geometry: vec![
                [origin.requested_lon, origin.requested_lat],
                [origin.snapped_lon, origin.snapped_lat],
            ],
        });
        diagnostics.push(AnalysisDiagnostic {
            code: AnalysisDiagnosticCode::FallbackUsed,
            severity: AnalysisDiagnosticSeverity::Warning,
            message: format!(
                "Connectivity fallback hopped the origin {:.1} m to reach component {}.",
                origin.snap_distance_m,
                origin.component_id.unwrap_or_default()
            ),
            point_ids: vec![origin.point_id.clone()],
            component_ids: origin.component_id.into_iter().collect(),
            suggested_actions: vec![],
        });
        warnings.push(format!(
            "Connectivity fallback used a non-network origin hop of {:.1} m.",
            origin.snap_distance_m
        ));
    }
    if destination_requires_hop {
        hop_segments.push(RouteHopSegment {
            endpoint: HopEndpoint::Destination,
            distance_m: destination.snap_distance_m,
            geometry: vec![
                [destination.snapped_lon, destination.snapped_lat],
                [destination.requested_lon, destination.requested_lat],
            ],
        });
        diagnostics.push(AnalysisDiagnostic {
            code: AnalysisDiagnosticCode::FallbackUsed,
            severity: AnalysisDiagnosticSeverity::Warning,
            message: format!(
                "Connectivity fallback hopped the destination {:.1} m to reach component {}.",
                destination.snap_distance_m,
                destination.component_id.unwrap_or_default()
            ),
            point_ids: vec![destination.point_id.clone()],
            component_ids: destination.component_id.into_iter().collect(),
            suggested_actions: vec![],
        });
        warnings.push(format!(
            "Connectivity fallback used a non-network destination hop of {:.1} m.",
            destination.snap_distance_m
        ));
    }

    Some(HopSelectionInfo {
        fallback_used: origin_requires_hop || destination_requires_hop,
        origin_hop_distance_m: origin_requires_hop.then_some(origin.snap_distance_m),
        destination_hop_distance_m: destination_requires_hop.then_some(destination.snap_distance_m),
        hop_segments,
        diagnostics,
        warnings,
    })
}

fn no_route_failure(
    topology: &TopologyBundle,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
) -> AnalysisFailure {
    let origin_components = candidate_component_ids(origin_candidates);
    let destination_components = candidate_component_ids(destination_candidates);
    if !origin_components.is_empty()
        && !destination_components.is_empty()
        && origin_components.is_disjoint(&destination_components)
    {
        let component_ids = origin_components
            .union(&destination_components)
            .copied()
            .collect::<Vec<_>>();
        let point_ids = vec![
            origin_candidates
                .first()
                .map(|candidate| candidate.point_id.clone())
                .unwrap_or_else(|| "origin".to_string()),
            destination_candidates
                .first()
                .map(|candidate| candidate.point_id.clone())
                .unwrap_or_else(|| "destination".to_string()),
        ];
        return AnalysisFailure::new(
            "origin and destination snapped to different weakly connected components",
            AnalysisOutcome::Unreachable,
            vec![AnalysisDiagnostic {
                code: AnalysisDiagnosticCode::DisconnectedComponents,
                severity: AnalysisDiagnosticSeverity::Error,
                message: format!(
                    "Origin and destination snapped to different weak components in the legal graph for dataset '{}'.",
                    topology.source_path
                ),
                point_ids,
                component_ids,
                suggested_actions: vec![
                    "Choose points in the same connected subnetwork.".to_string(),
                    "Set connectivity.disconnected to ignore_unreachable or a hop_* mode for exploratory analysis.".to_string(),
                ],
            }],
        );
    }

    AnalysisFailure::new(
        "no route found between the snapped origin and destination",
        AnalysisOutcome::Unreachable,
        vec![AnalysisDiagnostic {
            code: AnalysisDiagnosticCode::LegalRouteUnreachable,
            severity: AnalysisDiagnosticSeverity::Error,
            message: "Snapping succeeded, but no legal route was found between the snapped origin and destination candidates.".to_string(),
            point_ids: vec![
                origin_candidates
                    .first()
                    .map(|candidate| candidate.point_id.clone())
                    .unwrap_or_else(|| "origin".to_string()),
                destination_candidates
                    .first()
                    .map(|candidate| candidate.point_id.clone())
                    .unwrap_or_else(|| "destination".to_string()),
            ],
            component_ids: Vec::new(),
            suggested_actions: vec![
                "Inspect one-way and turn-restriction constraints near the endpoints.".to_string(),
                "If exploratory degraded analysis is acceptable, enable explicit fallback flags in fallback.".to_string(),
            ],
        }],
    )
}

fn candidate_component_ids(candidates: &[SnappedPoint]) -> std::collections::BTreeSet<u32> {
    candidates
        .iter()
        .filter_map(|candidate| candidate.component_id)
        .collect()
}

fn failure_outcome_and_diagnostics(
    error: &anyhow::Error,
) -> (AnalysisOutcome, Vec<AnalysisDiagnostic>) {
    if let Some(failure) = analysis_failure(error) {
        (failure.outcome, failure.diagnostics.clone())
    } else {
        (AnalysisOutcome::Unreachable, Vec::new())
    }
}

fn presnap_point_set(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    points: &[LabeledPoint],
    max_distance_m: f64,
    is_origin: bool,
) -> Vec<std::result::Result<Vec<SnappedPoint>, AnalysisFailure>> {
    let mut snap_cache = HashMap::new();
    points
        .iter()
        .map(|point| {
            cached_snap_candidates(
                &mut snap_cache,
                topology,
                routing_graph,
                point,
                max_distance_m,
                is_origin,
            )
        })
        .collect()
}

fn cached_snap_candidates(
    cache: &mut HashMap<
        (bool, u64, u64, u64),
        std::result::Result<Vec<SnappedPoint>, AnalysisFailure>,
    >,
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    max_distance_m: f64,
    is_origin: bool,
) -> std::result::Result<Vec<SnappedPoint>, AnalysisFailure> {
    let key = snap_point_cache_key(point, max_distance_m, is_origin);
    let value = cache.entry(key).or_insert_with(|| {
        snap_candidates(topology, routing_graph, point, max_distance_m, is_origin).map_err(
            |error| {
                analysis_failure(&error)
                    .cloned()
                    .unwrap_or_else(|| route_snap_failure(point, max_distance_m))
            },
        )
    });
    value.clone()
}

fn snap_point_cache_key(
    point: &LabeledPoint,
    max_distance_m: f64,
    is_origin: bool,
) -> (bool, u64, u64, u64) {
    (
        is_origin,
        point.lon.to_bits(),
        point.lat.to_bits(),
        max_distance_m.to_bits(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct BatchSnapCandidateKey {
    snapped_edge_id: u32,
    snapped_edge_fraction_bits: u64,
    snapped_node_id: u32,
    snap_distance_bits: u64,
}

fn batch_snap_candidate_key(candidate: &SnappedPoint) -> BatchSnapCandidateKey {
    BatchSnapCandidateKey {
        snapped_edge_id: candidate.snapped_edge_id.unwrap_or(u32::MAX),
        snapped_edge_fraction_bits: candidate
            .snapped_edge_fraction
            .unwrap_or_default()
            .to_bits(),
        snapped_node_id: candidate.snapped_node_id,
        snap_distance_bits: candidate.snap_distance_m.to_bits(),
    }
}

fn batch_candidate_set_key(candidates: &[SnappedPoint]) -> Vec<BatchSnapCandidateKey> {
    candidates.iter().map(batch_snap_candidate_key).collect()
}

fn intern_candidate_sets(
    candidate_sets: Vec<std::result::Result<Vec<SnappedPoint>, AnalysisFailure>>,
) -> (
    Vec<std::result::Result<usize, AnalysisFailure>>,
    Vec<Vec<SnappedPoint>>,
) {
    let mut unique = Vec::new();
    let mut interned = HashMap::new();
    let mut refs = Vec::with_capacity(candidate_sets.len());

    for candidates in candidate_sets {
        match candidates {
            Ok(candidates) => {
                let key = batch_candidate_set_key(&candidates);
                if let Some(&set_id) = interned.get(&key) {
                    refs.push(Ok(set_id));
                    continue;
                }
                let set_id = unique.len();
                interned.insert(key, set_id);
                unique.push(candidates);
                refs.push(Ok(set_id));
            }
            Err(error) => refs.push(Err(error)),
        }
    }

    (refs, unique)
}

fn cached_batch_route_result(
    cache: &mut HashMap<(usize, usize), std::result::Result<RouteResult, AnalysisFailure>>,
    origin_tree_cache: &mut HashMap<(u32, u64, u64), Result<SingleSourceEdgeTree, String>>,
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    route_id: &str,
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    fallback: &FallbackPolicy,
    returns: &ReturnConfig,
    origin_candidates: &[Vec<SnappedPoint>],
    origin_set_id: usize,
    destination_candidates: &[Vec<SnappedPoint>],
    destination_set_id: usize,
) -> Result<RouteResult> {
    let key = (origin_set_id, destination_set_id);
    let value = cache.entry(key).or_insert_with(|| {
        execute_batched_route_with_candidates(
            origin_tree_cache,
            topology,
            metrics,
            routing_graph,
            route_id,
            snap_max_distance_m,
            connectivity,
            fallback,
            returns,
            &origin_candidates[origin_set_id],
            &destination_candidates[destination_set_id],
        )
        .map_err(|error| {
            analysis_failure(&error).cloned().unwrap_or_else(|| {
                AnalysisFailure::new(error.to_string(), AnalysisOutcome::Unreachable, Vec::new())
            })
        })
    });
    value.clone().map_err(anyhow::Error::new)
}

fn execute_batched_route_with_candidates(
    origin_tree_cache: &mut HashMap<(u32, u64, u64), Result<SingleSourceEdgeTree, String>>,
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    route_id: &str,
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    fallback: &FallbackPolicy,
    returns: &ReturnConfig,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
) -> Result<RouteResult> {
    if routing_graph.has_restriction_sequences()
        || has_failure_modes(fallback)
        || auto_relaxation_requested(fallback)
    {
        return execute_route_with_candidates(
            topology,
            metrics,
            routing_graph,
            route_id,
            snap_max_distance_m,
            connectivity,
            fallback,
            returns,
            origin_candidates,
            destination_candidates,
            None,
        );
    }

    for origin in origin_candidates {
        let tree =
            cached_single_source_edge_tree(origin_tree_cache, topology, routing_graph, origin)?;
        for destination in destination_candidates {
            if same_edge_reverse_pair(origin, destination) {
                continue;
            }
            let Some(hop_info) =
                hop_info_for_pair(origin, destination, snap_max_distance_m, connectivity)
            else {
                continue;
            };
            let path = best_path_from_origin_tree(routing_graph, &tree, origin, destination);
            if let Some(path) = path {
                let path =
                    finalize_route_path(topology, metrics, path.edge_indexes, origin, destination);
                let analysis = analyze_route_path(
                    topology,
                    metrics,
                    routing_graph,
                    fallback,
                    &path,
                    origin,
                    destination,
                )?;
                return Ok(RouteResult {
                    route_id: route_id.to_string(),
                    origin: origin.clone(),
                    destination: destination.clone(),
                    outcome: if !analysis.violations.is_empty() || hop_info.fallback_used {
                        AnalysisOutcome::Degraded
                    } else {
                        AnalysisOutcome::Legal
                    },
                    fallback_used: hop_info.fallback_used || !analysis.violations.is_empty(),
                    origin_hop_distance_m: hop_info.origin_hop_distance_m,
                    destination_hop_distance_m: hop_info.destination_hop_distance_m,
                    summary: analysis.summary,
                    node_path: Vec::new(),
                    edge_path: Vec::new(),
                    geometry: match returns.geometry {
                        netan_profile::ReturnGeometry::None => None,
                        _ => Some(build_route_geometry(
                            topology,
                            &path.edge_indexes,
                            origin,
                            destination,
                        )),
                    },
                    hop_segments: hop_info.hop_segments,
                    segments: if returns.segment_rows {
                        Some(
                            path.edge_indexes
                                .iter()
                                .map(|&edge_index| {
                                    let edge = &topology.edges[edge_index];
                                    let metric = &metrics.edge_metrics[edge_index];
                                    let factor = edge_traversal_factor(
                                        edge_index,
                                        path.edge_indexes.first().copied(),
                                        path.edge_indexes.last().copied(),
                                        origin,
                                        destination,
                                    );
                                    RouteSegment {
                                        edge_id: edge.edge_id.0,
                                        from_node_id: edge.from.0,
                                        to_node_id: edge.to.0,
                                        source_way_id: edge.source_way_id,
                                        length_m: (edge.length_m as f64 * factor).round() as u32,
                                        travel_time_s: metric.travel_time_s.unwrap_or_default()
                                            * factor,
                                        generalized_cost: metric
                                            .generalized_cost
                                            .unwrap_or_default()
                                            * factor,
                                        road_class: edge.road_class,
                                        surface: edge.surface,
                                        name: edge
                                            .name_index
                                            .and_then(|index| topology.names.get(index as usize))
                                            .cloned(),
                                        violation_type: analysis
                                            .segment_violation_types
                                            .get(&edge_index)
                                            .copied()
                                            .flatten(),
                                    }
                                })
                                .collect(),
                        )
                    } else {
                        None
                    },
                    breakdowns: build_breakdowns(topology, metrics, &path.edge_indexes, returns),
                    violations: analysis.violations,
                    diagnostics: hop_info.diagnostics,
                    warnings: merge_warnings(
                        merge_warnings(execution_warnings(metrics), analysis.warnings),
                        hop_info.warnings,
                    ),
                });
            }
        }
    }

    let failure = no_route_failure(topology, origin_candidates, destination_candidates);
    if matches!(
        connectivity.disconnected,
        DisconnectedNetworkMode::IgnoreUnreachable
    ) && matches!(failure.outcome, AnalysisOutcome::Unreachable)
    {
        return Ok(ignored_unreachable_route_result(
            route_id,
            origin_candidates,
            destination_candidates,
            failure,
            execution_warnings(metrics),
        ));
    }

    Err(failure.into())
}

fn cached_single_source_edge_tree(
    cache: &mut HashMap<(u32, u64, u64), Result<SingleSourceEdgeTree, String>>,
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    origin: &SnappedPoint,
) -> Result<SingleSourceEdgeTree> {
    let key = snap_cache_key(origin);
    let value = cache.entry(key).or_insert_with(|| {
        build_single_source_edge_tree(topology, routing_graph, origin)
            .map_err(|error| error.to_string())
    });
    value.clone().map_err(anyhow::Error::msg)
}

fn build_single_source_edge_tree(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    origin: &SnappedPoint,
) -> Result<SingleSourceEdgeTree> {
    let _ = topology;
    let origin_seeds = origin_edge_seeds(routing_graph, origin);
    SINGLE_SOURCE_SEARCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.prepare(routing_graph.edge_costs.len());

        for (edge_index, cost) in origin_seeds {
            if !scratch.update(edge_index, cost, NO_PREVIOUS_EDGE) {
                continue;
            }
            scratch.heap.push(State {
                edge_index,
                automaton_state: 0,
                cost,
                score: cost,
            });
        }

        while let Some(State {
            edge_index,
            automaton_state: _,
            cost,
            score: _,
        }) = scratch.heap.pop()
        {
            if cost > scratch.dist[edge_index] {
                continue;
            }
            for transition_index in routing_graph.transition_range(edge_index) {
                let next_edge = routing_graph.transition_edges[transition_index] as usize;
                let next_cost = cost + routing_graph.transition_costs[transition_index];
                if !scratch.update(next_edge, next_cost, edge_index as u32) {
                    continue;
                }
                scratch.heap.push(State {
                    edge_index: next_edge,
                    automaton_state: 0,
                    cost: next_cost,
                    score: next_cost,
                });
            }
        }

        Ok(SingleSourceEdgeTree {
            dist: scratch.dist.clone(),
            previous: scratch.previous.clone(),
        })
    })
}

fn best_path_from_origin_tree(
    routing_graph: &RoutingGraph,
    tree: &SingleSourceEdgeTree,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> Option<RoutePath> {
    let mut best_path = direct_same_edge_path(routing_graph, origin, destination);
    let mut best_cost = best_path
        .as_ref()
        .map(|path| path.total_generalized_cost)
        .unwrap_or(f64::INFINITY);
    let mut best_edge = None;

    for (edge_index, adjustment) in destination_edge_seeds(routing_graph, destination) {
        let base_cost = tree.dist.get(edge_index).copied().unwrap_or(f64::INFINITY);
        if !base_cost.is_finite() {
            continue;
        }
        let total_cost = base_cost + adjustment;
        if total_cost < best_cost {
            best_cost = total_cost;
            best_edge = Some(edge_index);
        }
    }

    if let Some(edge_index) = best_edge {
        best_path = Some(reconstruct_single_source_route_path(
            tree, edge_index, best_cost,
        ));
    }

    best_path
}

fn reconstruct_single_source_route_path(
    tree: &SingleSourceEdgeTree,
    target_edge: usize,
    total_generalized_cost: f64,
) -> RoutePath {
    let mut edge_indexes = Vec::new();
    let mut cursor = target_edge;
    loop {
        edge_indexes.push(cursor);
        let previous_edge = tree.previous[cursor];
        if previous_edge == NO_PREVIOUS_EDGE {
            break;
        }
        cursor = previous_edge as usize;
    }
    edge_indexes.reverse();

    RoutePath {
        edge_indexes,
        total_generalized_cost,
    }
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

pub fn load_experiment(path: impl AsRef<Path>) -> Result<ExperimentDocument> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading experiment file {}", path.display()))?;
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("toml") => toml::from_str(&raw).context("parsing TOML experiment"),
        Some("yaml") | Some("yml") => serde_yaml::from_str(&raw).context("parsing YAML experiment"),
        other => bail!(
            "unsupported experiment extension {:?}; use .yml, .yaml, or .toml",
            other
        ),
    }
}

#[derive(Clone)]
struct RoutePath {
    edge_indexes: Vec<usize>,
    total_generalized_cost: f64,
}

struct RoutingGraph {
    first_out: Vec<u32>,
    edge_order: Vec<u32>,
    incoming_first_out: Vec<u32>,
    incoming_edge_order: Vec<u32>,
    edge_costs: Vec<f64>,
    transition_first_out: Vec<u32>,
    transition_edges: Vec<u32>,
    transition_costs: Vec<f64>,
    reverse_transition_first_out: Vec<u32>,
    reverse_transition_edges: Vec<u32>,
    reverse_transition_costs: Vec<f64>,
    acceleration: Option<AccelerationGraph>,
    automaton: RestrictionAutomaton,
    virtual_reverse_of: Vec<Option<usize>>,
}

impl RoutingGraph {
    fn outgoing_edges(&self, node_index: usize) -> &[u32] {
        let start = self.first_out[node_index] as usize;
        let end = self.first_out[node_index + 1] as usize;
        &self.edge_order[start..end]
    }

    fn incoming_edges(&self, node_index: usize) -> &[u32] {
        let start = self.incoming_first_out[node_index] as usize;
        let end = self.incoming_first_out[node_index + 1] as usize;
        &self.incoming_edge_order[start..end]
    }

    fn has_restriction_sequences(&self) -> bool {
        !self.automaton.is_trivial()
    }

    fn transition_range(&self, edge_index: usize) -> std::ops::Range<usize> {
        self.transition_first_out[edge_index] as usize
            ..self.transition_first_out[edge_index + 1] as usize
    }

    fn reverse_transition_range(&self, edge_index: usize) -> std::ops::Range<usize> {
        self.reverse_transition_first_out[edge_index] as usize
            ..self.reverse_transition_first_out[edge_index + 1] as usize
    }

    #[cfg(test)]
    fn has_edge_transition(&self, from_edge: usize, to_edge: usize) -> bool {
        self.transition_range(from_edge)
            .any(|index| self.transition_edges[index] == to_edge as u32)
    }
}

struct AccelerationGraph {
    upward_tail: Vec<u32>,
    upward_first_out: Vec<u32>,
    upward_head: Vec<u32>,
    upward_weight: Vec<f64>,
    upward_path_first_out: Vec<u32>,
    upward_path_edges: Vec<u32>,
    downward_head: Vec<u32>,
    downward_path_first_out: Vec<u32>,
    downward_path_edges: Vec<u32>,
    reverse_downward_first_out: Vec<u32>,
    reverse_downward_edge: Vec<u32>,
    reverse_downward_arc: Vec<u32>,
    reverse_downward_weight: Vec<f64>,
}

#[derive(Default)]
struct RestrictionAutomaton {
    states: Vec<AutomatonState>,
}

#[derive(Default)]
struct AutomatonState {
    transitions: HashMap<usize, usize>,
    failure: usize,
    prohibited_next: Vec<usize>,
    prohibited_meta: HashMap<usize, usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SearchStateKey {
    edge_index: usize,
    automaton_state: usize,
}

const NO_PREVIOUS_EDGE: u32 = u32::MAX;
const NO_PREVIOUS_ARC: u32 = u32::MAX;

thread_local! {
    static EDGE_SEARCH_SCRATCH: RefCell<BidirectionalEdgeSearchScratch> =
        RefCell::new(BidirectionalEdgeSearchScratch::default());
    static ACCELERATION_SEARCH_SCRATCH: RefCell<BidirectionalAccelerationScratch> =
        RefCell::new(BidirectionalAccelerationScratch::default());
    static RESTRICTED_SEARCH_SCRATCH: RefCell<RestrictedSearchScratch> =
        RefCell::new(RestrictedSearchScratch::default());
    static SINGLE_SOURCE_SEARCH_SCRATCH: RefCell<SingleSourceEdgeSearchScratch> =
        RefCell::new(SingleSourceEdgeSearchScratch::default());
}

#[derive(Default)]
struct BidirectionalEdgeSearchScratch {
    forward_dist: Vec<f64>,
    forward_previous: Vec<u32>,
    forward_touched: Vec<u32>,
    forward_heap: BinaryHeap<State>,
    backward_dist: Vec<f64>,
    backward_next: Vec<u32>,
    backward_touched: Vec<u32>,
    backward_heap: BinaryHeap<State>,
}

#[derive(Default)]
struct BidirectionalAccelerationScratch {
    forward_dist: Vec<f64>,
    forward_previous_arc: Vec<u32>,
    forward_touched: Vec<u32>,
    forward_heap: BinaryHeap<State>,
    backward_dist: Vec<f64>,
    backward_next_arc: Vec<u32>,
    backward_touched: Vec<u32>,
    backward_heap: BinaryHeap<State>,
}

impl BidirectionalAccelerationScratch {
    fn prepare(&mut self, edge_count: usize) {
        if self.forward_dist.len() < edge_count {
            self.forward_dist.resize(edge_count, f64::INFINITY);
            self.forward_previous_arc
                .resize(edge_count, NO_PREVIOUS_ARC);
            self.backward_dist.resize(edge_count, f64::INFINITY);
            self.backward_next_arc.resize(edge_count, NO_PREVIOUS_ARC);
        }
        for &edge_index in &self.forward_touched {
            self.forward_dist[edge_index as usize] = f64::INFINITY;
            self.forward_previous_arc[edge_index as usize] = NO_PREVIOUS_ARC;
        }
        for &edge_index in &self.backward_touched {
            self.backward_dist[edge_index as usize] = f64::INFINITY;
            self.backward_next_arc[edge_index as usize] = NO_PREVIOUS_ARC;
        }
        self.forward_touched.clear();
        self.forward_heap.clear();
        self.backward_touched.clear();
        self.backward_heap.clear();
    }

    fn update_forward(&mut self, edge_index: usize, cost: f64, previous_arc: u32) -> bool {
        if !cost.is_finite() {
            return false;
        }
        if cost + f64::EPSILON >= self.forward_dist[edge_index] {
            return false;
        }
        if !self.forward_dist[edge_index].is_finite() {
            self.forward_touched.push(edge_index as u32);
        }
        self.forward_dist[edge_index] = cost;
        self.forward_previous_arc[edge_index] = previous_arc;
        true
    }

    fn update_backward(&mut self, edge_index: usize, cost: f64, next_arc: u32) -> bool {
        if !cost.is_finite() {
            return false;
        }
        if cost + f64::EPSILON >= self.backward_dist[edge_index] {
            return false;
        }
        if !self.backward_dist[edge_index].is_finite() {
            self.backward_touched.push(edge_index as u32);
        }
        self.backward_dist[edge_index] = cost;
        self.backward_next_arc[edge_index] = next_arc;
        true
    }
}

impl BidirectionalEdgeSearchScratch {
    fn prepare(&mut self, edge_count: usize) {
        if self.forward_dist.len() < edge_count {
            self.forward_dist.resize(edge_count, f64::INFINITY);
            self.forward_previous.resize(edge_count, NO_PREVIOUS_EDGE);
            self.backward_dist.resize(edge_count, f64::INFINITY);
            self.backward_next.resize(edge_count, NO_PREVIOUS_EDGE);
        }
        for &edge_index in &self.forward_touched {
            self.forward_dist[edge_index as usize] = f64::INFINITY;
            self.forward_previous[edge_index as usize] = NO_PREVIOUS_EDGE;
        }
        for &edge_index in &self.backward_touched {
            self.backward_dist[edge_index as usize] = f64::INFINITY;
            self.backward_next[edge_index as usize] = NO_PREVIOUS_EDGE;
        }
        self.forward_touched.clear();
        self.forward_heap.clear();
        self.backward_touched.clear();
        self.backward_heap.clear();
    }

    fn update_forward(&mut self, edge_index: usize, cost: f64, previous_edge: u32) -> bool {
        if !cost.is_finite() {
            return false;
        }
        if cost + f64::EPSILON >= self.forward_dist[edge_index] {
            return false;
        }
        if !self.forward_dist[edge_index].is_finite() {
            self.forward_touched.push(edge_index as u32);
        }
        self.forward_dist[edge_index] = cost;
        self.forward_previous[edge_index] = previous_edge;
        true
    }

    fn update_backward(&mut self, edge_index: usize, cost: f64, next_edge: u32) -> bool {
        if !cost.is_finite() {
            return false;
        }
        if cost + f64::EPSILON >= self.backward_dist[edge_index] {
            return false;
        }
        if !self.backward_dist[edge_index].is_finite() {
            self.backward_touched.push(edge_index as u32);
        }
        self.backward_dist[edge_index] = cost;
        self.backward_next[edge_index] = next_edge;
        true
    }
}

#[derive(Default)]
struct RestrictedSearchScratch {
    dist: HashMap<SearchStateKey, f64>,
    previous: HashMap<SearchStateKey, Option<SearchStateKey>>,
    heap: BinaryHeap<State>,
}

impl RestrictedSearchScratch {
    fn prepare(&mut self) {
        self.dist.clear();
        self.previous.clear();
        self.heap.clear();
    }
}

#[derive(Default)]
struct SingleSourceEdgeSearchScratch {
    dist: Vec<f64>,
    previous: Vec<u32>,
    touched: Vec<u32>,
    heap: BinaryHeap<State>,
}

impl SingleSourceEdgeSearchScratch {
    fn prepare(&mut self, edge_count: usize) {
        if self.dist.len() < edge_count {
            self.dist.resize(edge_count, f64::INFINITY);
            self.previous.resize(edge_count, NO_PREVIOUS_EDGE);
        }
        for &edge_index in &self.touched {
            self.dist[edge_index as usize] = f64::INFINITY;
            self.previous[edge_index as usize] = NO_PREVIOUS_EDGE;
        }
        self.touched.clear();
        self.heap.clear();
    }

    fn update(&mut self, edge_index: usize, cost: f64, previous_edge: u32) -> bool {
        if !cost.is_finite() || cost + f64::EPSILON >= self.dist[edge_index] {
            return false;
        }
        if !self.dist[edge_index].is_finite() {
            self.touched.push(edge_index as u32);
        }
        self.dist[edge_index] = cost;
        self.previous[edge_index] = previous_edge;
        true
    }
}

#[derive(Clone)]
struct SingleSourceEdgeTree {
    dist: Vec<f64>,
    previous: Vec<u32>,
}

struct EdgeBasedTopologyView<'a> {
    node_first_out: &'a [u32],
    node_edge_order: &'a [u32],
    edge_transition_first_out: &'a [u32],
    edge_transition_edges: &'a [u32],
}

fn edge_based_topology_view(topology: &TopologyBundle) -> Option<EdgeBasedTopologyView<'_>> {
    let edge_topology = &topology.edge_based_topology;
    if edge_topology.node_first_out.len() != topology.nodes.len() + 1
        || edge_topology.node_edge_order.len() != topology.edges.len()
        || edge_topology.edge_transition_first_out.len() != topology.edges.len() + 1
    {
        return None;
    }
    let expected_transition_len = edge_topology
        .edge_transition_first_out
        .last()
        .copied()
        .unwrap_or_default() as usize;
    if edge_topology.edge_transition_edges.len() != expected_transition_len {
        return None;
    }
    Some(EdgeBasedTopologyView {
        node_first_out: &edge_topology.node_first_out,
        node_edge_order: &edge_topology.node_edge_order,
        edge_transition_first_out: &edge_topology.edge_transition_first_out,
        edge_transition_edges: &edge_topology.edge_transition_edges,
    })
}

fn build_edge_based_topology_fallback(topology: &TopologyBundle) -> netan_core::EdgeBasedTopology {
    let mut out_degree = vec![0_u32; topology.nodes.len()];
    let mut head = vec![0_u32; topology.edges.len()];
    for (edge_index, edge) in topology.edges.iter().enumerate() {
        out_degree[edge.from.0 as usize] += 1;
        head[edge_index] = edge.to.0;
    }

    let mut node_first_out = vec![0_u32; topology.nodes.len() + 1];
    for (node_index, degree) in out_degree.iter().enumerate() {
        node_first_out[node_index + 1] = node_first_out[node_index] + degree;
    }

    let mut node_edge_order = vec![0_u32; topology.edges.len()];
    let mut write_positions = node_first_out[..topology.nodes.len()].to_vec();
    for (edge_index, edge) in topology.edges.iter().enumerate() {
        let write_index = &mut write_positions[edge.from.0 as usize];
        node_edge_order[*write_index as usize] = edge_index as u32;
        *write_index += 1;
    }

    let mut edge_transition_first_out = vec![0_u32; topology.edges.len() + 1];
    for edge_index in 0..topology.edges.len() {
        let head_node = head[edge_index] as usize;
        edge_transition_first_out[edge_index + 1] = edge_transition_first_out[edge_index]
            + (node_first_out[head_node + 1] - node_first_out[head_node]);
    }

    let mut edge_transition_edges =
        vec![0_u32; edge_transition_first_out[topology.edges.len()] as usize];
    let mut transition_write_positions = edge_transition_first_out[..topology.edges.len()].to_vec();
    for edge_index in 0..topology.edges.len() {
        let head_node = head[edge_index] as usize;
        for &next_edge in &node_edge_order
            [node_first_out[head_node] as usize..node_first_out[head_node + 1] as usize]
        {
            let write_index = &mut transition_write_positions[edge_index];
            edge_transition_edges[*write_index as usize] = next_edge;
            *write_index += 1;
        }
    }

    netan_core::EdgeBasedTopology {
        node_first_out,
        node_edge_order,
        edge_transition_first_out,
        edge_transition_edges,
    }
}

impl RestrictionAutomaton {
    fn build(sequences: &[Vec<usize>]) -> Self {
        let mut automaton = Self {
            states: vec![AutomatonState::default()],
        };

        for sequence in sequences {
            if sequence.len() < 2 {
                continue;
            }
            let mut state_index = 0_usize;
            for &edge_index in sequence.iter().take(sequence.len() - 1) {
                if let Some(&next_state) =
                    automaton.states[state_index].transitions.get(&edge_index)
                {
                    state_index = next_state;
                    continue;
                }
                let next_state = automaton.states.len();
                automaton.states.push(AutomatonState::default());
                automaton.states[state_index]
                    .transitions
                    .insert(edge_index, next_state);
                state_index = next_state;
            }
            automaton.states[state_index]
                .prohibited_next
                .push(*sequence.last().unwrap());
            let next_edge = *sequence.last().unwrap();
            let sequence_len = sequence.len();
            automaton.states[state_index]
                .prohibited_meta
                .entry(next_edge)
                .and_modify(|existing| *existing = (*existing).max(sequence_len))
                .or_insert(sequence_len);
        }

        let mut queue = std::collections::VecDeque::new();
        let root_transitions = automaton.states[0]
            .transitions
            .values()
            .copied()
            .collect::<Vec<_>>();
        for state_index in root_transitions {
            automaton.states[state_index].failure = 0;
            queue.push_back(state_index);
        }

        while let Some(state_index) = queue.pop_front() {
            let transitions = automaton.states[state_index]
                .transitions
                .clone()
                .into_iter()
                .collect::<Vec<_>>();
            for (edge_index, next_state) in transitions {
                let mut failure = automaton.states[state_index].failure;
                while failure != 0
                    && !automaton.states[failure]
                        .transitions
                        .contains_key(&edge_index)
                {
                    failure = automaton.states[failure].failure;
                }
                let failure_state = automaton.states[failure]
                    .transitions
                    .get(&edge_index)
                    .copied()
                    .unwrap_or(0);
                automaton.states[next_state].failure = failure_state;
                let inherited = automaton.states[failure_state].prohibited_next.clone();
                automaton.states[next_state]
                    .prohibited_next
                    .extend(inherited);
                for (&next_edge, &sequence_len) in
                    &automaton.states[failure_state].prohibited_meta.clone()
                {
                    automaton.states[next_state]
                        .prohibited_meta
                        .entry(next_edge)
                        .and_modify(|existing| *existing = (*existing).max(sequence_len))
                        .or_insert(sequence_len);
                }
                automaton.states[next_state].prohibited_next.sort_unstable();
                automaton.states[next_state].prohibited_next.dedup();
                queue.push_back(next_state);
            }
        }

        automaton
    }

    fn is_trivial(&self) -> bool {
        self.states.len() <= 1
    }

    fn transition(&self, state_index: usize, edge_index: usize) -> usize {
        let mut cursor = state_index;
        loop {
            if let Some(&next_state) = self.states[cursor].transitions.get(&edge_index) {
                return next_state;
            }
            if cursor == 0 {
                return 0;
            }
            cursor = self.states[cursor].failure;
        }
    }

    fn is_transition_allowed(&self, state_index: usize, edge_index: usize) -> bool {
        self.states[state_index]
            .prohibited_next
            .binary_search(&edge_index)
            .is_err()
    }

    fn prohibited_sequence_len(&self, state_index: usize, edge_index: usize) -> Option<usize> {
        self.states[state_index]
            .prohibited_meta
            .get(&edge_index)
            .copied()
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct RoutingGraphBuildOptions {
    ignore_multi_edge_restriction_sequences: bool,
    search_time_turn_restrictions: bool,
}

fn build_routing_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
) -> Result<RoutingGraph> {
    build_routing_graph_with_options(topology, metrics, RoutingGraphBuildOptions::default())
}

fn build_routing_graph_with_options(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    options: RoutingGraphBuildOptions,
) -> Result<RoutingGraph> {
    let mut out_degree = vec![0_u32; topology.nodes.len()];
    let mut in_degree = vec![0_u32; topology.nodes.len()];
    let mut edge_costs = vec![f64::INFINITY; topology.edges.len()];

    for (edge_index, edge) in topology.edges.iter().enumerate() {
        let metric = &metrics.edge_metrics[edge_index];
        if let (Some(cost), Some(_)) = (metric.generalized_cost, metric.travel_time_s) {
            edge_costs[edge_index] = cost;
            in_degree[edge.to.0 as usize] += 1;
        }
    }

    let edge_topology = edge_based_topology_view(topology).context(
        "topology bundle is missing persisted edge-based adjacency; re-import the dataset with the current format",
    )?;

    for node_index in 0..topology.nodes.len() {
        for &edge_index in &edge_topology.node_edge_order[edge_topology.node_first_out[node_index]
            as usize
            ..edge_topology.node_first_out[node_index + 1] as usize]
        {
            if edge_costs[edge_index as usize].is_finite() {
                out_degree[node_index] += 1;
            }
        }
    }

    let mut first_out = vec![0_u32; topology.nodes.len() + 1];
    for (node_index, degree) in out_degree.iter().enumerate() {
        first_out[node_index + 1] = first_out[node_index] + degree;
    }
    let mut edge_order = vec![0_u32; first_out[topology.nodes.len()] as usize];
    let mut write_positions = first_out[..topology.nodes.len()].to_vec();
    let mut incoming_first_out = vec![0_u32; topology.nodes.len() + 1];
    for (node_index, degree) in in_degree.iter().enumerate() {
        incoming_first_out[node_index + 1] = incoming_first_out[node_index] + degree;
    }
    let mut incoming_edge_order = vec![0_u32; incoming_first_out[topology.nodes.len()] as usize];
    let mut incoming_write_positions = incoming_first_out[..topology.nodes.len()].to_vec();
    for node_index in 0..topology.nodes.len() {
        for &edge_index in &edge_topology.node_edge_order[edge_topology.node_first_out[node_index]
            as usize
            ..edge_topology.node_first_out[node_index + 1] as usize]
        {
            if !edge_costs[edge_index as usize].is_finite() {
                continue;
            }
            let write_index = &mut write_positions[node_index];
            edge_order[*write_index as usize] = edge_index;
            *write_index += 1;
            let head_node = topology.edges[edge_index as usize].to.0 as usize;
            let incoming_write_index = &mut incoming_write_positions[head_node];
            incoming_edge_order[*incoming_write_index as usize] = edge_index;
            *incoming_write_index += 1;
        }
    }

    let mode_bit = metrics.mode.access_bit();
    let mut pairwise_forbidden = Vec::<(u32, u32)>::new();
    let mut restricted_sequences = Vec::new();
    for restriction in topology
        .turn_restrictions
        .iter()
        .filter(|restriction| restriction.mode_mask.contains(mode_bit))
    {
        if restriction.edge_path.len() < 2 {
            continue;
        }
        if options.search_time_turn_restrictions {
            if restriction.edge_path.len() > 2 && options.ignore_multi_edge_restriction_sequences {
                continue;
            }
            restricted_sequences.push(
                restriction
                    .edge_path
                    .iter()
                    .map(|edge| edge.0 as usize)
                    .collect::<Vec<_>>(),
            );
            continue;
        }
        if restriction.edge_path.len() == 2 {
            pairwise_forbidden.push((restriction.edge_path[0].0, restriction.edge_path[1].0));
            continue;
        }
        if options.ignore_multi_edge_restriction_sequences {
            continue;
        }
        restricted_sequences.push(
            restriction
                .edge_path
                .iter()
                .map(|edge| edge.0 as usize)
                .collect::<Vec<_>>(),
        );
    }

    pairwise_forbidden.sort_unstable();
    pairwise_forbidden.dedup();
    let mut forbidden_turn_first_out = vec![0_u32; topology.edges.len() + 1];
    for &(from_edge, _) in &pairwise_forbidden {
        forbidden_turn_first_out[from_edge as usize + 1] += 1;
    }
    for edge_index in 0..topology.edges.len() {
        forbidden_turn_first_out[edge_index + 1] += forbidden_turn_first_out[edge_index];
    }
    let mut forbidden_turns = vec![0_u32; pairwise_forbidden.len()];
    let mut write_positions = forbidden_turn_first_out[..topology.edges.len()].to_vec();
    for (from_edge, to_edge) in pairwise_forbidden {
        let write_index = &mut write_positions[from_edge as usize];
        forbidden_turns[*write_index as usize] = to_edge;
        *write_index += 1;
    }

    let mut transition_degree = vec![0_u32; topology.edges.len()];
    for edge_index in 0..topology.edges.len() {
        if !edge_costs[edge_index].is_finite() {
            continue;
        }
        for &next_edge in &edge_topology.edge_transition_edges[edge_topology
            .edge_transition_first_out[edge_index]
            as usize
            ..edge_topology.edge_transition_first_out[edge_index + 1] as usize]
        {
            if !edge_costs[next_edge as usize].is_finite() {
                continue;
            }
            if options.search_time_turn_restrictions
                || !forbidden_turns[forbidden_turn_first_out[edge_index] as usize
                    ..forbidden_turn_first_out[edge_index + 1] as usize]
                    .binary_search(&next_edge)
                    .is_ok()
            {
                transition_degree[edge_index] += 1;
            }
        }
    }
    let mut transition_first_out = vec![0_u32; topology.edges.len() + 1];
    for edge_index in 0..topology.edges.len() {
        transition_first_out[edge_index + 1] =
            transition_first_out[edge_index] + transition_degree[edge_index];
    }
    let transition_count = transition_first_out[topology.edges.len()] as usize;
    let mut transition_edges = vec![0_u32; transition_count];
    let mut transition_costs = vec![0.0_f64; transition_count];
    let mut transition_write_positions = transition_first_out[..topology.edges.len()].to_vec();
    for edge_index in 0..topology.edges.len() {
        if !edge_costs[edge_index].is_finite() {
            continue;
        }
        for &next_edge in &edge_topology.edge_transition_edges[edge_topology
            .edge_transition_first_out[edge_index]
            as usize
            ..edge_topology.edge_transition_first_out[edge_index + 1] as usize]
        {
            let next_edge = next_edge as usize;
            if !edge_costs[next_edge].is_finite() {
                continue;
            }
            let write_index = &mut transition_write_positions[edge_index];
            if !options.search_time_turn_restrictions
                && forbidden_turns[forbidden_turn_first_out[edge_index] as usize
                    ..forbidden_turn_first_out[edge_index + 1] as usize]
                    .binary_search(&(next_edge as u32))
                    .is_ok()
            {
                continue;
            }
            let slot = *write_index as usize;
            transition_edges[slot] = next_edge as u32;
            transition_costs[slot] =
                edge_costs[next_edge] + turn_penalty_cost(metrics, topology, edge_index, next_edge);
            *write_index += 1;
        }
    }
    let mut reverse_transition_first_out = vec![0_u32; topology.edges.len() + 1];
    for &next_edge in &transition_edges {
        reverse_transition_first_out[next_edge as usize + 1] += 1;
    }
    for edge_index in 0..topology.edges.len() {
        reverse_transition_first_out[edge_index + 1] += reverse_transition_first_out[edge_index];
    }
    let mut reverse_transition_edges = vec![0_u32; transition_count];
    let mut reverse_transition_costs = vec![0.0_f64; transition_count];
    let mut reverse_write_positions = reverse_transition_first_out[..topology.edges.len()].to_vec();
    for edge_index in 0..topology.edges.len() {
        for transition_index in
            transition_first_out[edge_index] as usize..transition_first_out[edge_index + 1] as usize
        {
            let next_edge = transition_edges[transition_index] as usize;
            let write_index = &mut reverse_write_positions[next_edge];
            let slot = *write_index as usize;
            reverse_transition_edges[slot] = edge_index as u32;
            reverse_transition_costs[slot] = transition_costs[transition_index];
            *write_index += 1;
        }
    }

    Ok(RoutingGraph {
        first_out,
        edge_order,
        incoming_first_out,
        incoming_edge_order,
        edge_costs,
        transition_first_out,
        transition_edges,
        transition_costs,
        reverse_transition_first_out,
        reverse_transition_edges,
        reverse_transition_costs,
        acceleration: build_acceleration_graph(metrics, topology.edges.len())?,
        automaton: RestrictionAutomaton::build(&restricted_sequences),
        virtual_reverse_of: vec![None; topology.edges.len()],
    })
}

fn build_acceleration_graph(
    metrics: &CompiledProfileBundle,
    edge_count: usize,
) -> Result<Option<AccelerationGraph>> {
    let Some(acceleration) = metrics.acceleration.as_ref() else {
        return Ok(None);
    };
    if acceleration.edge_order.len() != edge_count
        || acceleration.edge_rank.len() != edge_count
        || acceleration.upward_first_out.len() != edge_count + 1
        || acceleration.downward_first_out.len() != edge_count + 1
    {
        bail!(
            "compiled acceleration bundle does not match topology edge count; recompile the profile with the current acceleration format"
        );
    }
    let upward_len = acceleration
        .upward_first_out
        .last()
        .copied()
        .unwrap_or_default() as usize;
    let downward_len = acceleration
        .downward_first_out
        .last()
        .copied()
        .unwrap_or_default() as usize;
    if acceleration.upward_head.len() != upward_len
        || acceleration.upward_weight.len() != upward_len
        || acceleration.upward_path_first_out.len() != upward_len + 1
    {
        bail!(
            "compiled acceleration upward arrays are inconsistent; recompile the profile with the current acceleration format"
        );
    }
    if acceleration.downward_head.len() != downward_len
        || acceleration.downward_weight.len() != downward_len
        || acceleration.downward_path_first_out.len() != downward_len + 1
    {
        bail!(
            "compiled acceleration downward arrays are inconsistent; recompile the profile with the current acceleration format"
        );
    }
    let upward_path_len = acceleration
        .upward_path_first_out
        .last()
        .copied()
        .unwrap_or_default() as usize;
    if acceleration.upward_path_edges.len() != upward_path_len {
        bail!(
            "compiled acceleration upward path arrays are inconsistent; recompile the profile with the current acceleration format"
        );
    }
    let downward_path_len = acceleration
        .downward_path_first_out
        .last()
        .copied()
        .unwrap_or_default() as usize;
    if acceleration.downward_path_edges.len() != downward_path_len {
        bail!(
            "compiled acceleration downward path arrays are inconsistent; recompile the profile with the current acceleration format"
        );
    }

    let mut reverse_downward_first_out = vec![0_u32; edge_count + 1];
    for &next_edge in &acceleration.downward_head {
        reverse_downward_first_out[next_edge as usize + 1] += 1;
    }
    for edge_index in 0..edge_count {
        reverse_downward_first_out[edge_index + 1] += reverse_downward_first_out[edge_index];
    }
    let mut reverse_downward_edge = vec![0_u32; downward_len];
    let mut reverse_downward_arc = vec![0_u32; downward_len];
    let mut reverse_downward_weight = vec![0.0_f64; downward_len];
    let mut write_positions = reverse_downward_first_out[..edge_count].to_vec();
    for edge_index in 0..edge_count {
        for slot in acceleration.downward_first_out[edge_index] as usize
            ..acceleration.downward_first_out[edge_index + 1] as usize
        {
            let next_edge = acceleration.downward_head[slot] as usize;
            let write_index = &mut write_positions[next_edge];
            let target_slot = *write_index as usize;
            reverse_downward_edge[target_slot] = edge_index as u32;
            reverse_downward_arc[target_slot] = slot as u32;
            reverse_downward_weight[target_slot] = acceleration.downward_weight[slot];
            *write_index += 1;
        }
    }

    let mut upward_tail = vec![0_u32; upward_len];
    for edge_index in 0..edge_count {
        for slot in acceleration.upward_first_out[edge_index] as usize
            ..acceleration.upward_first_out[edge_index + 1] as usize
        {
            upward_tail[slot] = edge_index as u32;
        }
    }
    Ok(Some(AccelerationGraph {
        upward_tail,
        upward_first_out: acceleration.upward_first_out.clone(),
        upward_head: acceleration.upward_head.clone(),
        upward_weight: acceleration.upward_weight.clone(),
        upward_path_first_out: acceleration.upward_path_first_out.clone(),
        upward_path_edges: acceleration.upward_path_edges.clone(),
        downward_head: acceleration.downward_head.clone(),
        downward_path_first_out: acceleration.downward_path_first_out.clone(),
        downward_path_edges: acceleration.downward_path_edges.clone(),
        reverse_downward_first_out,
        reverse_downward_edge,
        reverse_downward_arc,
        reverse_downward_weight,
    }))
}

fn route_between_candidates(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    fallback: &FallbackPolicy,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
) -> Result<(SnappedPoint, SnappedPoint, RoutePath, HopSelectionInfo)> {
    let origin_components = candidate_component_ids(origin_candidates);
    let destination_components = candidate_component_ids(destination_candidates);
    if !origin_components.is_empty()
        && !destination_components.is_empty()
        && origin_components.is_disjoint(&destination_components)
        && matches!(connectivity.disconnected, DisconnectedNetworkMode::Strict)
    {
        return Err(no_route_failure(topology, origin_candidates, destination_candidates).into());
    }

    let mut path_cache = HashMap::<((u32, u64, u64), (u32, u64, u64)), Option<RoutePath>>::new();

    for origin in origin_candidates {
        for destination in destination_candidates {
            if same_edge_reverse_pair(origin, destination) {
                continue;
            }
            let Some(hop_info) =
                hop_info_for_pair(origin, destination, snap_max_distance_m, connectivity)
            else {
                continue;
            };
            let key = (snap_cache_key(origin), snap_cache_key(destination));
            let path = if let Some(cached) = path_cache.get(&key) {
                cached.clone()
            } else {
                let direct_path = direct_same_edge_path(routing_graph, origin, destination);
                let path = if routing_graph.has_restriction_sequences() {
                    let origin_seeds = origin_edge_seeds(routing_graph, origin);
                    let destination_seeds = destination_edge_seeds(routing_graph, destination);
                    if has_failure_modes(fallback) {
                        astar_between_edge_seeds_with_failure_modes(
                            topology,
                            metrics,
                            routing_graph,
                            &origin_seeds,
                            &destination_seeds,
                            direct_path.clone(),
                            fallback,
                        )?
                    } else {
                        let pairwise_candidate = seeded_bidirectional_dijkstra_on_edge_transitions(
                            topology,
                            routing_graph,
                            &origin_seeds,
                            &destination_seeds,
                            direct_path.clone(),
                        )?;
                        match pairwise_candidate {
                            Some(path)
                                if path_respects_restriction_sequences(
                                    routing_graph,
                                    &path.edge_indexes,
                                ) =>
                            {
                                Some(path)
                            }
                            Some(_) => astar_between_edge_seeds_with_failure_modes(
                                topology,
                                metrics,
                                routing_graph,
                                &origin_seeds,
                                &destination_seeds,
                                direct_path.clone(),
                                fallback,
                            )?,
                            None => None,
                        }
                    }
                } else if routing_graph.acceleration.is_some() {
                    let origin_seeds = origin_edge_seeds(routing_graph, origin);
                    let destination_seeds = destination_edge_seeds(routing_graph, destination);
                    let acceleration_candidate = accelerated_route_query_seeded(
                        topology,
                        routing_graph,
                        &origin_seeds,
                        &destination_seeds,
                        direct_path.clone(),
                    )?;
                    seeded_bidirectional_dijkstra_on_edge_transitions(
                        topology,
                        routing_graph,
                        &origin_seeds,
                        &destination_seeds,
                        acceleration_candidate.or(direct_path.clone()),
                    )?
                } else {
                    let origin_seeds = origin_edge_seeds(routing_graph, origin);
                    let destination_seeds = destination_edge_seeds(routing_graph, destination);
                    if has_failure_modes(fallback) {
                        astar_between_edge_seeds_with_failure_modes(
                            topology,
                            metrics,
                            routing_graph,
                            &origin_seeds,
                            &destination_seeds,
                            direct_path.clone(),
                            fallback,
                        )?
                    } else {
                        seeded_bidirectional_dijkstra_on_edge_transitions(
                            topology,
                            routing_graph,
                            &origin_seeds,
                            &destination_seeds,
                            direct_path.clone(),
                        )?
                    }
                };
                path_cache.insert(key, path.clone());
                path
            };
            if let Some(path) = path {
                let path =
                    finalize_route_path(topology, metrics, path.edge_indexes, origin, destination);
                return Ok((origin.clone(), destination.clone(), path, hop_info));
            }
        }
    }

    Err(no_route_failure(topology, origin_candidates, destination_candidates).into())
}

fn snap_cache_key(point: &SnappedPoint) -> (u32, u64, u64) {
    (
        point.snapped_edge_id.unwrap_or(u32::MAX),
        point.snapped_edge_fraction.unwrap_or_default().to_bits(),
        point.snapped_node_id.to_owned() as u64,
    )
}

fn same_edge_reverse_pair(origin: &SnappedPoint, destination: &SnappedPoint) -> bool {
    matches!(
        (
            origin.snapped_edge_id,
            origin.snapped_edge_fraction,
            destination.snapped_edge_id,
            destination.snapped_edge_fraction
        ),
        (Some(origin_edge), Some(origin_fraction), Some(destination_edge), Some(destination_fraction))
            if origin_edge == destination_edge && origin_fraction > destination_fraction
    )
}

fn direct_same_edge_path(
    routing_graph: &RoutingGraph,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> Option<RoutePath> {
    let (Some(edge_id), Some(origin_fraction), Some(destination_fraction)) = (
        origin.snapped_edge_id,
        origin.snapped_edge_fraction,
        destination.snapped_edge_fraction,
    ) else {
        if origin.snapped_edge_id.is_none()
            && destination.snapped_edge_id.is_none()
            && origin.snapped_node_id == destination.snapped_node_id
        {
            return Some(RoutePath {
                edge_indexes: Vec::new(),
                total_generalized_cost: 0.0,
            });
        }
        return None;
    };
    if destination.snapped_edge_id != Some(edge_id) || origin_fraction > destination_fraction {
        return None;
    }
    let edge_cost = routing_graph.edge_costs[edge_id as usize];
    if !edge_cost.is_finite() {
        return None;
    }
    Some(RoutePath {
        edge_indexes: vec![edge_id as usize],
        total_generalized_cost: edge_cost * (destination_fraction - origin_fraction),
    })
}

fn path_respects_restriction_sequences(
    routing_graph: &RoutingGraph,
    edge_indexes: &[usize],
) -> bool {
    if !routing_graph.has_restriction_sequences() {
        return true;
    }

    let mut automaton_state = 0_usize;
    for &edge_index in edge_indexes {
        if routing_graph
            .automaton
            .prohibited_sequence_len(automaton_state, edge_index)
            .is_some()
        {
            return false;
        }
        automaton_state = routing_graph
            .automaton
            .transition(automaton_state, edge_index);
    }

    true
}

fn origin_edge_seeds(routing_graph: &RoutingGraph, origin: &SnappedPoint) -> Vec<(usize, f64)> {
    if let (Some(edge_id), Some(fraction)) = (origin.snapped_edge_id, origin.snapped_edge_fraction)
    {
        let edge_cost = routing_graph.edge_costs[edge_id as usize];
        if edge_cost.is_finite() {
            return vec![(edge_id as usize, edge_cost * (1.0 - fraction))];
        }
        return Vec::new();
    }
    routing_graph
        .outgoing_edges(origin.snapped_node_id as usize)
        .iter()
        .filter_map(|&edge_index| {
            let edge_cost = routing_graph.edge_costs[edge_index as usize];
            edge_cost
                .is_finite()
                .then_some((edge_index as usize, edge_cost))
        })
        .collect()
}

fn destination_edge_seeds(
    routing_graph: &RoutingGraph,
    destination: &SnappedPoint,
) -> Vec<(usize, f64)> {
    if let (Some(edge_id), Some(fraction)) = (
        destination.snapped_edge_id,
        destination.snapped_edge_fraction,
    ) {
        let edge_cost = routing_graph.edge_costs[edge_id as usize];
        if edge_cost.is_finite() {
            return vec![(edge_id as usize, edge_cost * fraction - edge_cost)];
        }
        return Vec::new();
    }
    routing_graph
        .incoming_edges(destination.snapped_node_id as usize)
        .iter()
        .map(|&edge_index| (edge_index as usize, 0.0))
        .collect()
}

fn reconstruct_bidirectional_route_path(
    scratch: &BidirectionalEdgeSearchScratch,
    meeting_edge: usize,
    total_generalized_cost: f64,
) -> RoutePath {
    let mut cursor = meeting_edge;
    let mut edge_indexes = Vec::new();
    loop {
        edge_indexes.push(cursor);
        let previous_edge = scratch.forward_previous[cursor];
        if previous_edge == NO_PREVIOUS_EDGE {
            break;
        }
        cursor = previous_edge as usize;
    }
    edge_indexes.reverse();

    let mut cursor = meeting_edge;
    loop {
        let next_edge = scratch.backward_next[cursor];
        if next_edge == NO_PREVIOUS_EDGE {
            break;
        }
        edge_indexes.push(next_edge as usize);
        cursor = next_edge as usize;
    }

    RoutePath {
        edge_indexes,
        total_generalized_cost,
    }
}

fn reconstruct_accelerated_route_path(
    acceleration: &AccelerationGraph,
    scratch: &BidirectionalAccelerationScratch,
    meeting_edge: usize,
    total_generalized_cost: f64,
) -> RoutePath {
    let mut edge_indexes = Vec::new();
    let mut forward_arc_ids = Vec::new();
    let mut cursor = meeting_edge;
    loop {
        let previous_arc = scratch.forward_previous_arc[cursor];
        if previous_arc == NO_PREVIOUS_ARC {
            break;
        }
        let previous_arc = previous_arc as usize;
        forward_arc_ids.push(previous_arc);
        cursor = acceleration.upward_tail[previous_arc] as usize;
    }
    edge_indexes.push(cursor);
    for arc_index in forward_arc_ids.into_iter().rev() {
        let start = acceleration.upward_path_first_out[arc_index] as usize;
        let end = acceleration.upward_path_first_out[arc_index + 1] as usize;
        edge_indexes.extend(
            acceleration.upward_path_edges[start..end]
                .iter()
                .map(|&edge_index| edge_index as usize),
        );
    }

    let mut cursor = meeting_edge;
    loop {
        let next_arc = scratch.backward_next_arc[cursor];
        if next_arc == NO_PREVIOUS_ARC {
            break;
        }
        let next_arc = next_arc as usize;
        let start = acceleration.downward_path_first_out[next_arc] as usize;
        let end = acceleration.downward_path_first_out[next_arc + 1] as usize;
        edge_indexes.extend(
            acceleration.downward_path_edges[start..end]
                .iter()
                .map(|&edge_index| edge_index as usize),
        );
        cursor = acceleration.downward_head[next_arc] as usize;
    }

    RoutePath {
        edge_indexes,
        total_generalized_cost,
    }
}

#[cfg(test)]
fn accelerated_route_query(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    source: usize,
    target: usize,
) -> Result<Option<RoutePath>> {
    if source >= topology.nodes.len() || target >= topology.nodes.len() {
        bail!("source or target node is out of bounds for the topology bundle");
    }
    let origin_seeds = routing_graph
        .outgoing_edges(source)
        .iter()
        .filter_map(|&edge_index| {
            let edge_cost = routing_graph.edge_costs[edge_index as usize];
            edge_cost
                .is_finite()
                .then_some((edge_index as usize, edge_cost))
        })
        .collect::<Vec<_>>();
    let destination_seeds = routing_graph
        .incoming_edges(target)
        .iter()
        .map(|&edge_index| (edge_index as usize, 0.0))
        .collect::<Vec<_>>();
    let initial_path = (source == target).then_some(RoutePath {
        edge_indexes: Vec::new(),
        total_generalized_cost: 0.0,
    });
    accelerated_route_query_seeded(
        topology,
        routing_graph,
        &origin_seeds,
        &destination_seeds,
        initial_path,
    )
}

fn accelerated_route_query_seeded(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    origin_seeds: &[(usize, f64)],
    destination_seeds: &[(usize, f64)],
    initial_path: Option<RoutePath>,
) -> Result<Option<RoutePath>> {
    let Some(acceleration) = routing_graph.acceleration.as_ref() else {
        return Ok(None);
    };

    ACCELERATION_SEARCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.prepare(topology.edges.len());

        for &(edge_index, cost) in origin_seeds {
            if !scratch.update_forward(edge_index, cost, NO_PREVIOUS_ARC) {
                continue;
            }
            scratch.forward_heap.push(State {
                edge_index,
                automaton_state: 0,
                cost,
                score: cost,
            });
        }
        for &(edge_index, cost) in destination_seeds {
            if !scratch.update_backward(edge_index, cost, NO_PREVIOUS_ARC) {
                continue;
            }
            scratch.backward_heap.push(State {
                edge_index,
                automaton_state: 0,
                cost,
                score: cost,
            });
        }

        let mut best_path = initial_path;
        let mut best_cost = best_path
            .as_ref()
            .map(|path| path.total_generalized_cost)
            .unwrap_or(f64::INFINITY);
        let mut best_edge = None;

        while !(scratch.forward_heap.is_empty() || scratch.backward_heap.is_empty()) {
            let next_forward_cost = scratch
                .forward_heap
                .peek()
                .map(|state| state.cost)
                .unwrap_or(f64::INFINITY);
            let next_backward_cost = scratch
                .backward_heap
                .peek()
                .map(|state| state.cost)
                .unwrap_or(f64::INFINITY);
            if next_forward_cost + next_backward_cost >= best_cost {
                break;
            }

            if next_forward_cost <= next_backward_cost {
                let Some(State {
                    edge_index,
                    automaton_state: _,
                    cost,
                    score: _,
                }) = scratch.forward_heap.pop()
                else {
                    break;
                };
                if cost > scratch.forward_dist[edge_index] {
                    continue;
                }
                if scratch.backward_dist[edge_index].is_finite() {
                    let candidate_cost = cost + scratch.backward_dist[edge_index];
                    if candidate_cost < best_cost {
                        best_cost = candidate_cost;
                        best_edge = Some(edge_index);
                    }
                }
                if cost > best_cost {
                    continue;
                }

                for slot in acceleration.upward_first_out[edge_index] as usize
                    ..acceleration.upward_first_out[edge_index + 1] as usize
                {
                    let next_edge = acceleration.upward_head[slot] as usize;
                    let next_cost = cost + acceleration.upward_weight[slot];
                    if !scratch.update_forward(next_edge, next_cost, slot as u32) {
                        continue;
                    }
                    scratch.forward_heap.push(State {
                        edge_index: next_edge,
                        automaton_state: 0,
                        cost: next_cost,
                        score: next_cost,
                    });
                }
            } else {
                let Some(State {
                    edge_index,
                    automaton_state: _,
                    cost,
                    score: _,
                }) = scratch.backward_heap.pop()
                else {
                    break;
                };
                if cost > scratch.backward_dist[edge_index] {
                    continue;
                }
                if scratch.forward_dist[edge_index].is_finite() {
                    let candidate_cost = scratch.forward_dist[edge_index] + cost;
                    if candidate_cost < best_cost {
                        best_cost = candidate_cost;
                        best_edge = Some(edge_index);
                    }
                }
                if cost > best_cost {
                    continue;
                }

                for slot in acceleration.reverse_downward_first_out[edge_index] as usize
                    ..acceleration.reverse_downward_first_out[edge_index + 1] as usize
                {
                    let previous_edge = acceleration.reverse_downward_edge[slot] as usize;
                    let next_cost = cost + acceleration.reverse_downward_weight[slot];
                    if !scratch.update_backward(
                        previous_edge,
                        next_cost,
                        acceleration.reverse_downward_arc[slot],
                    ) {
                        continue;
                    }
                    scratch.backward_heap.push(State {
                        edge_index: previous_edge,
                        automaton_state: 0,
                        cost: next_cost,
                        score: next_cost,
                    });
                }
            }
        }

        let Some(meeting_edge) = best_edge else {
            return Ok(best_path);
        };

        best_path = Some(reconstruct_accelerated_route_path(
            acceleration,
            &scratch,
            meeting_edge,
            best_cost,
        ));

        Ok(best_path)
    })
}

fn seeded_bidirectional_dijkstra_on_edge_transitions(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    origin_seeds: &[(usize, f64)],
    destination_seeds: &[(usize, f64)],
    initial_upper_bound: Option<RoutePath>,
) -> Result<Option<RoutePath>> {
    EDGE_SEARCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.prepare(topology.edges.len());

        for &(edge_index, cost) in origin_seeds {
            if !scratch.update_forward(edge_index, cost, NO_PREVIOUS_EDGE) {
                continue;
            }
            scratch.forward_heap.push(State {
                edge_index,
                automaton_state: 0,
                cost,
                score: cost,
            });
        }
        for &(edge_index, cost) in destination_seeds {
            if !scratch.update_backward(edge_index, cost, NO_PREVIOUS_EDGE) {
                continue;
            }
            scratch.backward_heap.push(State {
                edge_index,
                automaton_state: 0,
                cost,
                score: cost,
            });
        }

        let mut best_path = initial_upper_bound;
        let mut best_cost = best_path
            .as_ref()
            .map(|path| path.total_generalized_cost)
            .unwrap_or(f64::INFINITY);
        let mut best_edge = None;

        while !(scratch.forward_heap.is_empty() || scratch.backward_heap.is_empty()) {
            let next_forward_cost = scratch
                .forward_heap
                .peek()
                .map(|state| state.cost)
                .unwrap_or(f64::INFINITY);
            let next_backward_cost = scratch
                .backward_heap
                .peek()
                .map(|state| state.cost)
                .unwrap_or(f64::INFINITY);
            if best_path.is_some() && next_forward_cost + next_backward_cost >= best_cost {
                break;
            }

            if next_forward_cost <= next_backward_cost {
                let Some(State {
                    edge_index,
                    automaton_state: _,
                    cost,
                    score: _,
                }) = scratch.forward_heap.pop()
                else {
                    break;
                };
                if cost > scratch.forward_dist[edge_index] {
                    continue;
                }
                if scratch.backward_dist[edge_index].is_finite() {
                    let candidate_cost = cost + scratch.backward_dist[edge_index];
                    if candidate_cost < best_cost {
                        best_cost = candidate_cost;
                        best_edge = Some(edge_index);
                    }
                }
                if cost > best_cost {
                    continue;
                }

                for transition_index in routing_graph.transition_range(edge_index) {
                    let next_edge = routing_graph.transition_edges[transition_index] as usize;
                    let next_cost = cost + routing_graph.transition_costs[transition_index];
                    if !scratch.update_forward(next_edge, next_cost, edge_index as u32) {
                        continue;
                    }
                    scratch.forward_heap.push(State {
                        edge_index: next_edge,
                        automaton_state: 0,
                        cost: next_cost,
                        score: next_cost,
                    });
                }
            } else {
                let Some(State {
                    edge_index,
                    automaton_state: _,
                    cost,
                    score: _,
                }) = scratch.backward_heap.pop()
                else {
                    break;
                };
                if cost > scratch.backward_dist[edge_index] {
                    continue;
                }
                if scratch.forward_dist[edge_index].is_finite() {
                    let candidate_cost = scratch.forward_dist[edge_index] + cost;
                    if candidate_cost < best_cost {
                        best_cost = candidate_cost;
                        best_edge = Some(edge_index);
                    }
                }
                if cost > best_cost {
                    continue;
                }

                for transition_index in routing_graph.reverse_transition_range(edge_index) {
                    let previous_edge =
                        routing_graph.reverse_transition_edges[transition_index] as usize;
                    let next_cost = cost + routing_graph.reverse_transition_costs[transition_index];
                    if !scratch.update_backward(previous_edge, next_cost, edge_index as u32) {
                        continue;
                    }
                    scratch.backward_heap.push(State {
                        edge_index: previous_edge,
                        automaton_state: 0,
                        cost: next_cost,
                        score: next_cost,
                    });
                }
            }
        }

        if let Some(meeting_edge) = best_edge {
            best_path = Some(reconstruct_bidirectional_route_path(
                &scratch,
                meeting_edge,
                best_cost,
            ));
        }

        Ok(best_path)
    })
}

fn edge_failure_mode_penalty(
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    fallback: &FallbackPolicy,
    edge_index: usize,
) -> (f64, f64) {
    if routing_graph
        .virtual_reverse_of
        .get(edge_index)
        .and_then(|value| *value)
        .is_some()
    {
        let penalty_s = fallback
            .penalties
            .reverse_oneway_penalty_s
            .unwrap_or_default();
        return (penalty_s, penalty_s * metrics.turn_costs.cost_time_weight);
    }
    (0.0, 0.0)
}

fn transition_failure_mode_penalty(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    automaton_state: usize,
    previous_edge_index: usize,
    next_edge_index: usize,
    fallback: &FallbackPolicy,
) -> Option<(f64, f64)> {
    let mut penalty_s = 0.0;
    if let Some(sequence_len) = routing_graph
        .automaton
        .prohibited_sequence_len(automaton_state, next_edge_index)
    {
        if fallback.ignore_turn_restrictions {
            penalty_s += fallback
                .penalties
                .ignored_turn_restriction_penalty_s
                .unwrap_or_default();
        } else if sequence_len == 2
            && classify_turn(topology, previous_edge_index, next_edge_index) == TurnDirection::Uturn
            && fallback.allow_uturn_where_normally_forbidden
        {
            penalty_s += fallback
                .penalties
                .forbidden_uturn_penalty_s
                .or(fallback.penalties.illegal_turn_penalty_s)
                .unwrap_or_default();
        } else if sequence_len == 2 && fallback.allow_illegal_turn {
            penalty_s += fallback
                .penalties
                .illegal_turn_penalty_s
                .unwrap_or_default();
        } else {
            return None;
        }
    }
    let (_, edge_penalty_cost) =
        edge_failure_mode_penalty(metrics, routing_graph, fallback, next_edge_index);
    Some((
        penalty_s,
        penalty_s * metrics.turn_costs.cost_time_weight + edge_penalty_cost,
    ))
}

fn astar_between_edge_seeds_with_failure_modes(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origin_seeds: &[(usize, f64)],
    destination_seeds: &[(usize, f64)],
    initial_upper_bound: Option<RoutePath>,
    fallback: &FallbackPolicy,
) -> Result<Option<RoutePath>> {
    RESTRICTED_SEARCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.prepare();

        let mut best_path = initial_upper_bound;
        let mut best_cost = best_path
            .as_ref()
            .map(|path| path.total_generalized_cost)
            .unwrap_or(f64::INFINITY);
        let mut best_state = None;
        let destination_adjustments = destination_seeds.iter().fold(
            HashMap::<usize, f64>::new(),
            |mut acc, (edge_index, adjustment)| {
                acc.entry(*edge_index)
                    .and_modify(|existing| *existing = existing.min(*adjustment))
                    .or_insert(*adjustment);
                acc
            },
        );

        for &(edge_index, cost) in origin_seeds {
            let (edge_penalty_s, edge_penalty_cost) =
                edge_failure_mode_penalty(metrics, routing_graph, fallback, edge_index);
            let seeded_cost = cost + edge_penalty_cost;
            if !seeded_cost.is_finite() {
                continue;
            }
            let automaton_state = routing_graph.automaton.transition(0, edge_index);
            let key = SearchStateKey {
                edge_index,
                automaton_state,
            };
            scratch.dist.insert(key, seeded_cost);
            scratch.previous.insert(key, None);
            scratch.heap.push(State {
                edge_index,
                automaton_state,
                cost: seeded_cost,
                score: seeded_cost + edge_penalty_s * 0.0,
            });
        }

        while let Some(State {
            edge_index,
            automaton_state,
            cost,
            score: _,
        }) = scratch.heap.pop()
        {
            let key = SearchStateKey {
                edge_index,
                automaton_state,
            };
            let Some(&known_cost) = scratch.dist.get(&key) else {
                continue;
            };
            if cost > known_cost {
                continue;
            }

            if let Some(&adjustment) = destination_adjustments.get(&edge_index) {
                let candidate_cost = cost + adjustment;
                if candidate_cost < best_cost {
                    best_cost = candidate_cost;
                    best_state = Some(key);
                }
            }
            if cost >= best_cost {
                continue;
            }

            for transition_index in routing_graph.transition_range(edge_index) {
                let next_edge = routing_graph.transition_edges[transition_index] as usize;
                let Some((penalty_s, penalty_cost)) = transition_failure_mode_penalty(
                    topology,
                    metrics,
                    routing_graph,
                    automaton_state,
                    edge_index,
                    next_edge,
                    fallback,
                ) else {
                    continue;
                };
                let next_cost =
                    cost + routing_graph.transition_costs[transition_index] + penalty_cost;
                let next_automaton_state = routing_graph
                    .automaton
                    .transition(automaton_state, next_edge);
                let next_key = SearchStateKey {
                    edge_index: next_edge,
                    automaton_state: next_automaton_state,
                };
                if next_cost + f64::EPSILON < *scratch.dist.get(&next_key).unwrap_or(&f64::INFINITY)
                {
                    scratch.dist.insert(next_key, next_cost);
                    scratch.previous.insert(next_key, Some(key));
                    scratch.heap.push(State {
                        edge_index: next_edge,
                        automaton_state: next_automaton_state,
                        cost: next_cost,
                        score: next_cost + penalty_s * 0.0,
                    });
                }
            }
        }

        let Some(mut cursor) = best_state else {
            return Ok(best_path);
        };

        let mut edge_indexes = Vec::new();
        loop {
            edge_indexes.push(cursor.edge_index);
            let previous_state = scratch.previous.get(&cursor).copied().flatten();
            let Some(previous_state) = previous_state else {
                break;
            };
            cursor = previous_state;
            if !scratch.previous.contains_key(&cursor) {
                bail!("failed to reconstruct route path");
            }
        }
        edge_indexes.reverse();

        best_path = Some(RoutePath {
            edge_indexes,
            total_generalized_cost: best_cost,
        });

        Ok(best_path)
    })
}

fn finalize_route_path(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    edge_indexes: Vec<usize>,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> RoutePath {
    let mut total_generalized_cost = 0.0;
    let mut previous_edge_index = None;
    let first_edge = edge_indexes.first().copied();
    let last_edge = edge_indexes.last().copied();
    for &edge_index in &edge_indexes {
        let metric = &metrics.edge_metrics[edge_index];
        let factor = edge_traversal_factor(edge_index, first_edge, last_edge, origin, destination);
        total_generalized_cost += metric.generalized_cost.unwrap_or_default() * factor;
        if let Some(previous_edge_index) = previous_edge_index {
            total_generalized_cost +=
                turn_penalty_seconds(topology, metrics, previous_edge_index, edge_index)
                    * metrics.turn_costs.cost_time_weight;
        }
        previous_edge_index = Some(edge_index);
    }

    RoutePath {
        edge_indexes,
        total_generalized_cost,
    }
}

fn edge_traversal_factor(
    edge_index: usize,
    first_edge: Option<usize>,
    last_edge: Option<usize>,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> f64 {
    let mut start_factor = 1.0;
    let mut end_factor = 1.0;
    if first_edge == Some(edge_index)
        && origin.snapped_edge_id == Some(edge_index as u32)
        && origin.snapped_edge_fraction.is_some()
    {
        start_factor = 1.0 - origin.snapped_edge_fraction.unwrap_or_default();
    }
    if last_edge == Some(edge_index)
        && destination.snapped_edge_id == Some(edge_index as u32)
        && destination.snapped_edge_fraction.is_some()
    {
        end_factor = destination.snapped_edge_fraction.unwrap_or(1.0);
    }
    if first_edge == Some(edge_index) && last_edge == Some(edge_index) {
        if origin.snapped_edge_id == Some(edge_index as u32)
            && destination.snapped_edge_id == Some(edge_index as u32)
        {
            return (destination.snapped_edge_fraction.unwrap_or(1.0)
                - origin.snapped_edge_fraction.unwrap_or_default())
            .clamp(0.0, 1.0);
        }
    }
    start_factor.min(end_factor)
}

fn build_route_geometry(
    topology: &TopologyBundle,
    edge_indexes: &[usize],
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> Vec<[f64; 2]> {
    let mut geometry = Vec::with_capacity(edge_indexes.len() + 2);
    geometry.push([origin.snapped_lon, origin.snapped_lat]);
    for &edge_index in edge_indexes {
        let node = &topology.nodes[topology.edges[edge_index].to.0 as usize];
        geometry.push([node.lon, node.lat]);
    }
    if geometry.last().is_none_or(|point| {
        point[0] != destination.snapped_lon || point[1] != destination.snapped_lat
    }) {
        geometry.push([destination.snapped_lon, destination.snapped_lat]);
    }
    geometry
}

fn execution_warnings(metrics: &CompiledProfileBundle) -> Vec<String> {
    let _ = metrics;
    Vec::new()
}

fn turn_penalty_cost(
    metrics: &CompiledProfileBundle,
    topology: &TopologyBundle,
    previous_edge_index: usize,
    next_edge_index: usize,
) -> f64 {
    turn_penalty_seconds(topology, metrics, previous_edge_index, next_edge_index)
        * metrics.turn_costs.cost_time_weight
}

fn turn_penalty_seconds(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    previous_edge_index: usize,
    next_edge_index: usize,
) -> f64 {
    let previous = &topology.edges[previous_edge_index];
    let next = &topology.edges[next_edge_index];

    if previous.to != next.from {
        return 0.0;
    }

    let mut penalty_s = 0.0;
    if previous.flags & EDGE_FLAG_TARGET_TRAFFIC_SIGNAL != 0 {
        penalty_s += metrics.turn_costs.traffic_signal_penalty_s;
    }
    if previous.flags & EDGE_FLAG_ROUNDABOUT == 0 && next.flags & EDGE_FLAG_ROUNDABOUT != 0 {
        penalty_s += metrics.turn_costs.roundabout_entry_penalty_s;
    }

    if previous.from == next.to {
        return penalty_s + metrics.turn_costs.uturn_penalty_s;
    }

    if previous.source_way_id == next.source_way_id {
        return penalty_s;
    }

    penalty_s
        + match classify_turn(topology, previous_edge_index, next_edge_index) {
            TurnDirection::Straight => 0.0,
            TurnDirection::Left => metrics.turn_costs.left_penalty_s,
            TurnDirection::Right => metrics.turn_costs.right_penalty_s,
            TurnDirection::Uturn => metrics.turn_costs.uturn_penalty_s,
        }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnDirection {
    Straight,
    Left,
    Right,
    Uturn,
}

fn classify_turn(
    topology: &TopologyBundle,
    previous_edge_index: usize,
    next_edge_index: usize,
) -> TurnDirection {
    const STRAIGHT_THRESHOLD_RAD: f64 = 30.0_f64.to_radians();
    const UTURN_THRESHOLD_RAD: f64 = 150.0_f64.to_radians();

    let previous = &topology.edges[previous_edge_index];
    let next = &topology.edges[next_edge_index];
    let from = &topology.nodes[previous.from.0 as usize];
    let via = &topology.nodes[previous.to.0 as usize];
    let to = &topology.nodes[next.to.0 as usize];

    let in_x = projected_delta_x(from.lon, via.lat, via.lon);
    let in_y = projected_delta_y(from.lat, via.lat);
    let out_x = projected_delta_x(via.lon, via.lat, to.lon);
    let out_y = projected_delta_y(via.lat, to.lat);

    let in_norm = (in_x * in_x + in_y * in_y).sqrt();
    let out_norm = (out_x * out_x + out_y * out_y).sqrt();
    if in_norm <= f64::EPSILON || out_norm <= f64::EPSILON {
        return TurnDirection::Straight;
    }

    let dot = ((in_x * out_x + in_y * out_y) / (in_norm * out_norm)).clamp(-1.0, 1.0);
    let cross = in_x * out_y - in_y * out_x;
    let angle = cross.atan2(dot);
    let abs_angle = angle.abs();

    if abs_angle <= STRAIGHT_THRESHOLD_RAD {
        TurnDirection::Straight
    } else if abs_angle >= UTURN_THRESHOLD_RAD {
        TurnDirection::Uturn
    } else if angle > 0.0 {
        TurnDirection::Left
    } else {
        TurnDirection::Right
    }
}

fn projected_delta_x(from_lon: f64, reference_lat: f64, to_lon: f64) -> f64 {
    let earth_radius_m = 6_371_000.0_f64;
    let lon_delta = (to_lon - from_lon).to_radians();
    lon_delta * reference_lat.to_radians().cos() * earth_radius_m
}

fn projected_delta_y(from_lat: f64, to_lat: f64) -> f64 {
    let earth_radius_m = 6_371_000.0_f64;
    (to_lat - from_lat).to_radians() * earth_radius_m
}

fn snap_candidates(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    max_distance_m: f64,
    is_origin: bool,
) -> Result<Vec<SnappedPoint>> {
    const MAX_SNAP_CANDIDATES: usize = 8;

    let nearby_nodes = if let Some(spatial_index) = topology.spatial_index.as_ref() {
        spatial_snap_nodes(topology, spatial_index, point, max_distance_m)
    } else {
        topology
            .nodes
            .iter()
            .map(|node| {
                (
                    node.node_id.0,
                    haversine_meters(point.lon, point.lat, node.lon, node.lat),
                )
            })
            .filter(|(_, distance_m)| *distance_m <= max_distance_m)
            .collect()
    };

    let mut candidates = Vec::with_capacity(MAX_SNAP_CANDIDATES * 2);
    for &(node_id, distance_m) in &nearby_nodes {
        if !node_is_traversable_for_snap(routing_graph, node_id as usize, is_origin) {
            continue;
        }
        push_best_snap_candidate(
            &mut candidates,
            SnappedPoint {
                point_id: point.id.clone(),
                requested_lon: point.lon,
                requested_lat: point.lat,
                snapped_node_id: node_id,
                snapped_lon: topology.nodes[node_id as usize].lon,
                snapped_lat: topology.nodes[node_id as usize].lat,
                snap_distance_m: distance_m,
                snapped_edge_id: None,
                snapped_edge_fraction: None,
                snapped_from_node_id: None,
                snapped_to_node_id: None,
                component_id: traversable_node_component_id(
                    topology,
                    routing_graph,
                    node_id as usize,
                    is_origin,
                ),
            },
            MAX_SNAP_CANDIDATES,
        );
    }

    let mut candidate_edges = Vec::<u32>::new();
    for &(node_id, _) in &nearby_nodes {
        candidate_edges.extend_from_slice(routing_graph.outgoing_edges(node_id as usize));
        candidate_edges.extend_from_slice(routing_graph.incoming_edges(node_id as usize));
    }
    if candidate_edges.is_empty() {
        for edge_index in 0..topology.edges.len() {
            if routing_graph.edge_costs[edge_index].is_finite() {
                candidate_edges.push(edge_index as u32);
            }
        }
    } else {
        candidate_edges.sort_unstable();
        candidate_edges.dedup();
    }

    for edge_index in candidate_edges {
        let edge = &topology.edges[edge_index as usize];
        let from = &topology.nodes[edge.from.0 as usize];
        let to = &topology.nodes[edge.to.0 as usize];
        let projection = project_point_onto_segment(point.lon, point.lat, from, to);
        if projection.distance_m > max_distance_m {
            continue;
        }
        if projection.fraction <= 1.0e-6 || projection.fraction >= 1.0 - 1.0e-6 {
            continue;
        }
        let snapped_node_id = if projection.fraction <= 0.5 {
            edge.from.0
        } else {
            edge.to.0
        };
        push_best_snap_candidate(
            &mut candidates,
            SnappedPoint {
                point_id: point.id.clone(),
                requested_lon: point.lon,
                requested_lat: point.lat,
                snapped_node_id,
                snapped_lon: projection.lon,
                snapped_lat: projection.lat,
                snap_distance_m: projection.distance_m,
                snapped_edge_id: Some(edge_index),
                snapped_edge_fraction: Some(projection.fraction),
                snapped_from_node_id: Some(edge.from.0),
                snapped_to_node_id: Some(edge.to.0),
                component_id: topology.edge_component_id(edge_index),
            },
            MAX_SNAP_CANDIDATES,
        );
    }

    if candidates.is_empty() {
        return Err(route_snap_failure(point, max_distance_m).into());
    }

    candidates.sort_by(|left, right| left.snap_distance_m.total_cmp(&right.snap_distance_m));
    candidates.dedup_by(|left, right| snap_candidate_key(left) == snap_candidate_key(right));
    candidates.truncate(MAX_SNAP_CANDIDATES);
    Ok(candidates)
}

fn node_is_traversable_for_snap(
    routing_graph: &RoutingGraph,
    node_index: usize,
    is_origin: bool,
) -> bool {
    if is_origin {
        !routing_graph.outgoing_edges(node_index).is_empty()
    } else {
        !routing_graph.incoming_edges(node_index).is_empty()
    }
}

fn traversable_node_component_id(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    node_index: usize,
    is_origin: bool,
) -> Option<u32> {
    let edge_index = if is_origin {
        routing_graph.outgoing_edges(node_index).first().copied()
    } else {
        routing_graph.incoming_edges(node_index).first().copied()
    };
    edge_index
        .and_then(|edge_index| topology.edge_component_id(edge_index))
        .or_else(|| topology.node_component_id(node_index as u32))
}

struct SegmentProjection {
    fraction: f64,
    lon: f64,
    lat: f64,
    distance_m: f64,
}

fn project_point_onto_segment(
    lon: f64,
    lat: f64,
    from: &TopologyNode,
    to: &TopologyNode,
) -> SegmentProjection {
    let origin_x = 0.0;
    let origin_y = 0.0;
    let point_x = projected_delta_x(from.lon, from.lat, lon);
    let point_y = projected_delta_y(from.lat, lat);
    let segment_x = projected_delta_x(from.lon, from.lat, to.lon);
    let segment_y = projected_delta_y(from.lat, to.lat);
    let segment_len_sq = segment_x * segment_x + segment_y * segment_y;
    let fraction = if segment_len_sq <= f64::EPSILON {
        0.0
    } else {
        ((point_x * segment_x + point_y * segment_y) / segment_len_sq).clamp(0.0, 1.0)
    };
    let snapped_x = origin_x + segment_x * fraction;
    let snapped_y = origin_y + segment_y * fraction;
    let distance_m = ((point_x - snapped_x).powi(2) + (point_y - snapped_y).powi(2)).sqrt();
    SegmentProjection {
        fraction,
        lon: from.lon + (to.lon - from.lon) * fraction,
        lat: from.lat + (to.lat - from.lat) * fraction,
        distance_m,
    }
}

fn snap_candidate_key(candidate: &SnappedPoint) -> (u32, u64, u64) {
    (
        candidate.snapped_edge_id.unwrap_or(u32::MAX),
        candidate
            .snapped_edge_fraction
            .unwrap_or_default()
            .to_bits(),
        candidate.snapped_node_id as u64,
    )
}

fn spatial_snap_nodes(
    topology: &TopologyBundle,
    spatial_index: &netan_core::NodeSpatialIndex,
    point: &LabeledPoint,
    max_distance_m: f64,
) -> Vec<(u32, f64)> {
    let Some((center_col, center_row)) =
        spatial_index_cell_for_point(spatial_index, point.lon, point.lat)
    else {
        return Vec::new();
    };
    let cell_height_m = haversine_meters(
        point.lon,
        point.lat,
        point.lon,
        point.lat + spatial_index.cell_height_deg,
    )
    .max(1.0);
    let cell_width_m = haversine_meters(
        point.lon,
        point.lat,
        point.lon + spatial_index.cell_width_deg,
        point.lat,
    )
    .max(1.0);
    let ring_limit = ((max_distance_m / cell_height_m.min(cell_width_m)).ceil() as i32).max(1) + 1;
    let mut candidates = Vec::with_capacity(8);

    for row in center_row - ring_limit..=center_row + ring_limit {
        if !(0..spatial_index.rows as i32).contains(&row) {
            continue;
        }
        for col in center_col - ring_limit..=center_col + ring_limit {
            if !(0..spatial_index.columns as i32).contains(&col) {
                continue;
            }
            let cell =
                &spatial_index.cells[row as usize * spatial_index.columns as usize + col as usize];
            let start = cell.node_start as usize;
            let end = start + cell.node_len as usize;
            for &node_id in &spatial_index.node_ids[start..end] {
                let node = &topology.nodes[node_id as usize];
                let distance_m = haversine_meters(point.lon, point.lat, node.lon, node.lat);
                if distance_m <= max_distance_m {
                    let insert_index =
                        candidates.partition_point(|(_, existing)| *existing <= distance_m);
                    candidates.insert(insert_index, (node_id, distance_m));
                    if candidates.len() > 32 {
                        candidates.pop();
                    }
                }
            }
        }
    }

    candidates
}

fn push_best_snap_candidate(
    candidates: &mut Vec<SnappedPoint>,
    candidate: SnappedPoint,
    max_candidates: usize,
) {
    let insert_index = candidates
        .partition_point(|existing| existing.snap_distance_m <= candidate.snap_distance_m);
    if insert_index >= max_candidates * 2 {
        return;
    }
    candidates.insert(insert_index, candidate);
    if candidates.len() > max_candidates * 2 {
        candidates.pop();
    }
}

fn spatial_index_cell_for_point(
    spatial_index: &netan_core::NodeSpatialIndex,
    lon: f64,
    lat: f64,
) -> Option<(i32, i32)> {
    if spatial_index.columns == 0 || spatial_index.rows == 0 {
        return None;
    }
    let col = (((lon - spatial_index.bounds.min_lon) / spatial_index.cell_width_deg).floor()
        as i32)
        .clamp(0, spatial_index.columns as i32 - 1);
    let row = (((lat - spatial_index.bounds.min_lat) / spatial_index.cell_height_deg).floor()
        as i32)
        .clamp(0, spatial_index.rows as i32 - 1);
    Some((col, row))
}

fn build_breakdowns(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    edge_indexes: &[usize],
    returns: &ReturnConfig,
) -> Option<RouteBreakdowns> {
    let want_road = !returns.road_type_breakdown.is_empty();
    let want_surface = !returns.surface_breakdown.is_empty();
    if !want_road && !want_surface {
        return None;
    }

    let mut breakdowns = RouteBreakdowns::default();
    for &edge_index in edge_indexes {
        let edge = &topology.edges[edge_index];
        let metric = &metrics.edge_metrics[edge_index];

        if want_road {
            let entry = breakdowns
                .road_class
                .entry(format!("{:?}", edge.road_class).to_lowercase())
                .or_default();
            fill_metric_breakdown(
                entry,
                edge.length_m as u64,
                metric.travel_time_s.unwrap_or_default(),
                returns,
                true,
            );
        }
        if want_surface {
            let entry = breakdowns
                .surface
                .entry(format!("{:?}", edge.surface).to_lowercase())
                .or_default();
            fill_metric_breakdown(
                entry,
                edge.length_m as u64,
                metric.travel_time_s.unwrap_or_default(),
                returns,
                false,
            );
        }
    }

    Some(breakdowns)
}

fn fill_metric_breakdown(
    entry: &mut MetricBreakdown,
    distance_m: u64,
    time_s: f64,
    returns: &ReturnConfig,
    road: bool,
) {
    let metrics = if road {
        &returns.road_type_breakdown
    } else {
        &returns.surface_breakdown
    };
    for metric in metrics {
        match metric {
            netan_profile::BreakdownMetric::DistanceM => {
                *entry.distance_m.get_or_insert(0) += distance_m;
            }
            netan_profile::BreakdownMetric::TimeS => {
                *entry.time_s.get_or_insert(0.0) += time_s;
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct State {
    edge_index: usize,
    automaton_state: usize,
    cost: f64,
    score: f64,
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        self.edge_index == other.edge_index
            && self.automaton_state == other.automaton_state
            && self.score.to_bits() == other.score.to_bits()
    }
}

impl Eq for State {}

impl PartialOrd for State {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| self.cost.total_cmp(&other.cost))
            .then_with(|| self.edge_index.cmp(&other.edge_index))
            .then_with(|| self.automaton_state.cmp(&other.automaton_state))
    }
}

fn haversine_meters(from_lon: f64, from_lat: f64, to_lon: f64, to_lat: f64) -> f64 {
    let earth_radius_m = 6_371_000.0_f64;
    let d_lat = (to_lat - from_lat).to_radians();
    let d_lon = (to_lon - from_lon).to_radians();
    let from_lat = from_lat.to_radians();
    let to_lat = to_lat.to_radians();
    let a =
        (d_lat / 2.0).sin().powi(2) + from_lat.cos() * to_lat.cos() * (d_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    earth_radius_m * c
}

#[cfg(test)]
mod tests {
    use super::{
        AnalysisDiagnosticCode, AnalysisKind, ConnectivityPolicy, DisconnectedNetworkMode,
        EngineMode, FallbackPolicy, IllegalMovementPenaltyPolicy, OdPair, OdPairsDocument,
        PointSetDocument, PreparedRoutingEngine, RouteRequest, ServiceAreaBandMode,
        ServiceAreaBoundaryMode, ServiceAreaMultiOriginMode, ServiceAreaOutputMode,
        ServiceAreaThreshold, ServiceAreaThresholdMetric, SnapOptions, analysis_failure,
        build_routing_graph, execute_matrix, execute_od, execute_route,
        execute_route_with_edge_names, execute_service_area, load_experiment, load_od_pairs,
        load_point_set, load_service_area_request,
    };
    use netan_core::{
        AccessMask, CacheBundleId, CompiledAcceleration, CompiledEdgeMetric, CompiledProfileBundle,
        CompiledTurnCostConfig, DirectedEdge, EDGE_FLAG_ROUNDABOUT,
        EDGE_FLAG_TARGET_TRAFFIC_SIGNAL, EdgeId, NodeId, NodeSpatialIndex, RoadClass,
        SmoothnessClass, SpatialIndexCell, SurfaceClass, TopologyBounds, TopologyBundle,
        TopologyNode, TravelMode, TurnRestriction, TurnRestrictionKind,
    };
    use netan_profile::{BreakdownMetric, ReturnConfig, ReturnGeometry};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn executes_exact_route_and_breakdowns() {
        let topology = test_topology();
        let metrics = CompiledProfileBundle {
            schema_version: 2,
            profile_id: "test".to_string(),
            profile_hash: "abc".to_string(),
            mode: TravelMode::Car,
            turn_costs: CompiledTurnCostConfig::default(),
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            acceleration: None,
            edge_metrics: vec![
                CompiledEdgeMetric {
                    edge_id: EdgeId(0),
                    travel_time_s: Some(10.0),
                    generalized_cost: Some(10.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(1),
                    travel_time_s: Some(20.0),
                    generalized_cost: Some(20.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(2),
                    travel_time_s: Some(100.0),
                    generalized_cost: Some(100.0),
                },
            ],
        };
        let request = RouteRequest {
            route_id: "route".to_string(),
            origin: super::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                segment_rows: true,
                road_type_breakdown: vec![BreakdownMetric::DistanceM, BreakdownMetric::TimeS],
                surface_breakdown: vec![BreakdownMetric::DistanceM],
                penalty_breakdown: false,
                explain_cost_derivation: false,
            },
        };

        let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
        assert_eq!(result.edge_path, vec![0, 1]);
        assert_eq!(result.summary.segment_count, 2);
        assert_eq!(result.summary.total_distance_m, 300);
        assert_eq!(result.summary.total_travel_time_s, 30.0);
        assert!(result.geometry.is_some());
        assert!(result.segments.is_some());
        assert_eq!(
            result
                .breakdowns
                .as_ref()
                .and_then(|value| value.road_class.get("residential"))
                .and_then(|value| value.distance_m),
            Some(300)
        );
    }

    #[test]
    fn uses_external_edge_name_bundle_for_segment_rows() {
        let mut topology = test_topology();
        topology.names.clear();
        topology.edges[0].name_index = Some(0);
        topology.edges[1].name_index = Some(1);
        let request = RouteRequest {
            route_id: "route".to_string(),
            origin: super::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::None,
                segment_rows: true,
                ..ReturnConfig::default()
            },
        };

        let result = execute_route_with_edge_names(
            &topology,
            &test_metrics(),
            &request,
            &["alpha".to_string(), "beta".to_string()],
        )
        .expect("route succeeds");

        let segments = result.segments.expect("segments requested");
        assert_eq!(segments[0].name.as_deref(), Some("alpha"));
        assert_eq!(segments[1].name.as_deref(), Some("beta"));
    }

    #[test]
    fn prepared_engine_reuses_prebuilt_graph() {
        let engine =
            PreparedRoutingEngine::new(Arc::new(test_topology()), Arc::new(test_metrics()))
                .expect("prepared engine builds");
        let request = RouteRequest {
            route_id: "prepared-route".to_string(),
            origin: super::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };

        let route = engine.execute_route(&request).expect("route succeeds");
        assert_eq!(route.edge_path, vec![0, 1]);
        assert_eq!(route.summary.total_distance_m, 300);
    }

    #[test]
    fn accelerated_query_returns_an_unpacked_path() {
        let topology = test_topology();
        let graph =
            build_routing_graph(&topology, &accelerated_test_metrics()).expect("graph builds");

        let path = super::accelerated_route_query(&topology, &graph, 0, 2)
            .expect("accelerated query succeeds")
            .expect("accelerated path exists");

        assert_eq!(path.edge_indexes, vec![0, 1]);
        assert_eq!(path.total_generalized_cost, 30.0);
    }

    #[test]
    fn pure_summary_routes_omit_path_payloads() {
        let request = RouteRequest {
            route_id: "summary-route".to_string(),
            origin: super::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
        };

        let route =
            execute_route(&test_topology(), &test_metrics(), &request).expect("route succeeds");
        assert!(route.node_path.is_empty());
        assert!(route.edge_path.is_empty());
        assert!(route.geometry.is_none());
        assert!(route.segments.is_none());
    }

    #[test]
    fn routes_between_phantom_edge_snaps_with_partial_edge_costs() {
        let request = RouteRequest {
            route_id: "phantom-route".to_string(),
            origin: super::LabeledPoint {
                id: "a_mid".to_string(),
                lon: 6.0005,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "b_mid".to_string(),
                lon: 6.0015,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                segment_rows: true,
                ..ReturnConfig::default()
            },
        };

        let route =
            execute_route(&test_topology(), &test_metrics(), &request).expect("route succeeds");

        assert_eq!(route.edge_path, vec![0, 1]);
        assert_eq!(route.summary.total_distance_m, 150);
        assert!((route.summary.total_travel_time_s - 15.0).abs() < 1.0e-6);
        assert_eq!(route.origin.snapped_edge_id, Some(0));
        assert_eq!(route.destination.snapped_edge_id, Some(1));
        let geometry = route.geometry.expect("geometry requested");
        assert_eq!(geometry.first().copied(), Some([6.0005, 53.0]));
        assert_eq!(geometry.last().copied(), Some([6.0015, 53.0]));
        let segments = route.segments.expect("segments requested");
        assert_eq!(segments[0].length_m, 50);
        assert!((segments[0].travel_time_s - 5.0).abs() < 1.0e-6);
        assert_eq!(segments[1].length_m, 100);
        assert!((segments[1].travel_time_s - 10.0).abs() < 1.0e-6);
    }

    #[test]
    fn rejects_snap_beyond_threshold() {
        let request = RouteRequest {
            route_id: "route".to_string(),
            origin: super::LabeledPoint {
                id: "a".to_string(),
                lon: 0.0,
                lat: 0.0,
            },
            destination: super::LabeledPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 10.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
        };

        let error = execute_route(&test_topology(), &test_metrics(), &request)
            .expect_err("snap should fail");
        assert!(
            error
                .to_string()
                .contains("no traversable candidate node or edge within")
        );
    }

    #[test]
    fn snap_candidates_keep_only_the_nearest_eight() {
        let topology = snap_test_topology();
        let metrics = uniform_metrics(topology.edges.len(), 1.0);
        let graph = build_routing_graph(&topology, &metrics).expect("graph builds");
        let point = super::LabeledPoint {
            id: "snap".to_string(),
            lon: 6.0,
            lat: 53.0,
        };

        let candidates =
            super::snap_candidates(&topology, &graph, &point, 500.0, true).expect("snap works");

        assert_eq!(candidates.len(), 8);
        assert!(
            candidates
                .windows(2)
                .all(|window| window[0].snap_distance_m <= window[1].snap_distance_m)
        );
        assert_eq!(candidates[0].snapped_node_id, 0);
        assert_eq!(candidates[7].snapped_node_id, 7);
    }

    #[test]
    fn executes_od_batch_with_failures() {
        let document = OdPairsDocument {
            pairs: vec![
                OdPair {
                    pair_id: "ok".to_string(),
                    origin: super::LabeledPoint {
                        id: "a".to_string(),
                        lon: 6.0,
                        lat: 53.0,
                    },
                    destination: super::LabeledPoint {
                        id: "c".to_string(),
                        lon: 6.002,
                        lat: 53.0,
                    },
                },
                OdPair {
                    pair_id: "bad".to_string(),
                    origin: super::LabeledPoint {
                        id: "far".to_string(),
                        lon: 0.0,
                        lat: 0.0,
                    },
                    destination: super::LabeledPoint {
                        id: "c".to_string(),
                        lon: 6.002,
                        lat: 53.0,
                    },
                },
            ],
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };

        let result = execute_od(&test_topology(), &test_metrics(), &document).expect("OD succeeds");
        assert_eq!(result.succeeded_count, 1);
        assert_eq!(result.failed_count, 1);
        assert_eq!(result.pairs[0].total_distance_m, Some(300));
        assert!(
            result.pairs[0]
                .geometry
                .as_ref()
                .is_some_and(|coords| coords.len() >= 2)
        );
        assert_eq!(result.pairs[1].status, super::BatchItemStatus::Failed);
    }

    #[test]
    fn executes_matrix_batch() {
        let origins = PointSetDocument {
            points: vec![
                super::LabeledPoint {
                    id: "a".to_string(),
                    lon: 6.0,
                    lat: 53.0,
                },
                super::LabeledPoint {
                    id: "b".to_string(),
                    lon: 6.001,
                    lat: 53.0,
                },
            ],
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };
        let destinations = PointSetDocument {
            points: vec![super::LabeledPoint {
                id: "c".to_string(),
                lon: 6.002,
                lat: 53.0,
            }],
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };

        let result = execute_matrix(&test_topology(), &test_metrics(), &origins, &destinations)
            .expect("matrix succeeds");
        assert_eq!(result.cell_count, 2);
        assert_eq!(result.succeeded_count, 2);
        assert_eq!(result.cells[0].total_distance_m, Some(300));
        assert_eq!(result.cells[1].total_distance_m, Some(200));
        assert!(
            result.cells[0]
                .geometry
                .as_ref()
                .is_some_and(|coords| coords.len() >= 2)
        );
    }

    #[test]
    fn executes_matrix_batch_with_presnapped_failures() {
        let origins = PointSetDocument {
            points: vec![
                super::LabeledPoint {
                    id: "a".to_string(),
                    lon: 6.0,
                    lat: 53.0,
                },
                super::LabeledPoint {
                    id: "far".to_string(),
                    lon: 0.0,
                    lat: 0.0,
                },
            ],
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };
        let destinations = PointSetDocument {
            points: vec![
                super::LabeledPoint {
                    id: "c".to_string(),
                    lon: 6.002,
                    lat: 53.0,
                },
                super::LabeledPoint {
                    id: "b".to_string(),
                    lon: 6.001,
                    lat: 53.0,
                },
            ],
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };

        let result = execute_matrix(&test_topology(), &test_metrics(), &origins, &destinations)
            .expect("matrix succeeds");

        assert_eq!(result.cell_count, 4);
        assert_eq!(result.succeeded_count, 2);
        assert_eq!(result.failed_count, 2);
        assert_eq!(result.cells[0].status, super::BatchItemStatus::Succeeded);
        assert_eq!(result.cells[1].status, super::BatchItemStatus::Succeeded);
        assert_eq!(result.cells[2].status, super::BatchItemStatus::Failed);
        assert_eq!(result.cells[3].status, super::BatchItemStatus::Failed);
        assert!(
            result.cells[2]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("no traversable candidate node or edge within"))
        );
    }

    #[test]
    fn matrix_matches_repeated_exact_route_execution() {
        let topology = test_topology();
        let metrics = test_metrics();
        let origins = PointSetDocument {
            points: vec![
                super::LabeledPoint {
                    id: "a".to_string(),
                    lon: 6.0,
                    lat: 53.0,
                },
                super::LabeledPoint {
                    id: "b".to_string(),
                    lon: 6.001,
                    lat: 53.0,
                },
            ],
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };
        let destinations = PointSetDocument {
            points: vec![
                super::LabeledPoint {
                    id: "b".to_string(),
                    lon: 6.001,
                    lat: 53.0,
                },
                super::LabeledPoint {
                    id: "c".to_string(),
                    lon: 6.002,
                    lat: 53.0,
                },
            ],
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };

        let matrix =
            execute_matrix(&topology, &metrics, &origins, &destinations).expect("matrix succeeds");

        for (cell, (origin, destination)) in
            matrix
                .cells
                .iter()
                .zip(origins.points.iter().flat_map(|origin| {
                    destinations
                        .points
                        .iter()
                        .map(move |destination| (origin, destination))
                }))
        {
            let route = execute_route(
                &topology,
                &metrics,
                &RouteRequest {
                    route_id: format!("{}__{}", origin.id, destination.id),
                    origin: origin.clone(),
                    destination: destination.clone(),
                    snap: origins.snap.clone(),
                    connectivity: origins.connectivity.clone(),
                    fallback: origins.fallback.clone(),
                    returns: origins.returns.clone(),
                },
            )
            .expect("route succeeds");
            assert_eq!(cell.total_distance_m, Some(route.summary.total_distance_m));
            assert_eq!(
                cell.total_travel_time_s,
                Some(route.summary.total_travel_time_s)
            );
            assert_eq!(
                cell.total_generalized_cost,
                Some(route.summary.total_generalized_cost)
            );
            assert_eq!(cell.geometry, route.geometry);
        }
    }

    #[test]
    fn accelerated_engine_matches_exact_engine_on_small_topology() {
        let topology = test_topology();
        let exact_engine =
            PreparedRoutingEngine::new(Arc::new(topology.clone()), Arc::new(test_metrics()))
                .expect("exact engine builds");
        let accelerated_engine =
            PreparedRoutingEngine::new(Arc::new(topology), Arc::new(accelerated_test_metrics()))
                .expect("accelerated engine builds");

        let requests = [
            RouteRequest {
                route_id: "a_to_c".to_string(),
                origin: super::LabeledPoint {
                    id: "a".to_string(),
                    lon: 6.0,
                    lat: 53.0,
                },
                destination: super::LabeledPoint {
                    id: "c".to_string(),
                    lon: 6.002,
                    lat: 53.0,
                },
                snap: SnapOptions {
                    max_distance_m: 500.0,
                },
                connectivity: Default::default(),
                fallback: Default::default(),
                returns: ReturnConfig {
                    geometry: ReturnGeometry::Full,
                    ..ReturnConfig::default()
                },
            },
            RouteRequest {
                route_id: "a_to_b".to_string(),
                origin: super::LabeledPoint {
                    id: "a".to_string(),
                    lon: 6.0,
                    lat: 53.0,
                },
                destination: super::LabeledPoint {
                    id: "b".to_string(),
                    lon: 6.001,
                    lat: 53.0,
                },
                snap: SnapOptions {
                    max_distance_m: 500.0,
                },
                connectivity: Default::default(),
                fallback: Default::default(),
                returns: ReturnConfig::default(),
            },
        ];

        for request in requests {
            let exact = exact_engine
                .execute_route(&request)
                .expect("exact route succeeds");
            let accelerated = accelerated_engine
                .execute_route(&request)
                .expect("accelerated route succeeds");
            assert_eq!(
                accelerated.summary.total_distance_m,
                exact.summary.total_distance_m
            );
            assert_eq!(
                accelerated.summary.total_travel_time_s,
                exact.summary.total_travel_time_s
            );
            assert_eq!(
                accelerated.summary.total_generalized_cost,
                exact.summary.total_generalized_cost
            );
            assert_eq!(accelerated.geometry, exact.geometry);
            assert_eq!(accelerated.edge_path, exact.edge_path);
        }
    }

    #[test]
    fn respects_turn_restrictions() {
        let topology = restricted_topology();
        let metrics = restricted_metrics();
        let request = RouteRequest {
            route_id: "restricted".to_string(),
            origin: super::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "d".to_string(),
                lon: 6.003,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };

        let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
        assert_eq!(result.edge_path, vec![3, 2]);
        assert_eq!(result.summary.total_distance_m, 300);
    }

    #[test]
    fn builds_pairwise_turn_table_without_automaton_for_two_edge_restrictions() {
        let graph = build_routing_graph(&restricted_topology(), &restricted_metrics())
            .expect("graph builds");

        assert!(!graph.has_restriction_sequences());
        assert!(!graph.has_edge_transition(1, 2));
        assert!(graph.has_edge_transition(0, 1));
    }

    #[test]
    fn builds_routing_graph_from_persisted_edge_based_topology() {
        let mut topology = restricted_topology();
        topology.edge_based_topology = super::build_edge_based_topology_fallback(&topology);

        let graph = build_routing_graph(&topology, &restricted_metrics())
            .expect("graph builds from bundle");

        assert!(!graph.has_restriction_sequences());
        assert!(!graph.has_edge_transition(1, 2));
        assert!(graph.has_edge_transition(0, 1));
    }

    #[test]
    fn applies_turn_penalties_when_selecting_routes() {
        let topology = turn_penalty_topology();
        let mut metrics = turn_penalty_metrics();
        let request = RouteRequest {
            route_id: "turn-penalty".to_string(),
            origin: super::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "d".to_string(),
                lon: 6.002,
                lat: 53.001,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };

        let without_penalty =
            execute_route(&topology, &metrics, &request).expect("route without turn penalty");
        assert_eq!(without_penalty.edge_path, vec![0, 1, 2]);
        assert_eq!(without_penalty.summary.total_travel_time_s, 15.0);

        metrics.turn_costs.left_penalty_s = 10.0;
        metrics.turn_costs.right_penalty_s = 5.0;

        let with_penalty =
            execute_route(&topology, &metrics, &request).expect("route with turn penalty");
        assert_eq!(with_penalty.edge_path, vec![3, 4]);
        assert_eq!(with_penalty.summary.total_travel_time_s, 25.0);
    }

    #[test]
    fn applies_traffic_signal_penalties() {
        let topology = traffic_signal_penalty_topology();
        let mut metrics = turn_penalty_metrics();
        let request = RouteRequest {
            route_id: "traffic-signal-penalty".to_string(),
            origin: super::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "d".to_string(),
                lon: 6.002,
                lat: 53.001,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };

        let without_penalty =
            execute_route(&topology, &metrics, &request).expect("route without signal penalty");
        assert_eq!(without_penalty.edge_path, vec![0, 1, 2]);
        assert_eq!(without_penalty.summary.total_travel_time_s, 15.0);

        metrics.turn_costs.traffic_signal_penalty_s = 10.0;

        let with_penalty =
            execute_route(&topology, &metrics, &request).expect("route with signal penalty");
        assert_eq!(with_penalty.edge_path, vec![3, 4]);
        assert_eq!(with_penalty.summary.total_travel_time_s, 20.0);
    }

    #[test]
    fn applies_roundabout_entry_penalties() {
        let topology = roundabout_entry_penalty_topology();
        let mut metrics = roundabout_entry_penalty_metrics();
        let request = RouteRequest {
            route_id: "roundabout-entry-penalty".to_string(),
            origin: super::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "d".to_string(),
                lon: 6.002,
                lat: 53.001,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };

        let without_penalty =
            execute_route(&topology, &metrics, &request).expect("route without roundabout penalty");
        assert_eq!(without_penalty.edge_path, vec![0, 1]);
        assert_eq!(without_penalty.summary.total_travel_time_s, 10.0);

        metrics.turn_costs.roundabout_entry_penalty_s = 11.0;

        let with_penalty =
            execute_route(&topology, &metrics, &request).expect("route with roundabout penalty");
        assert_eq!(with_penalty.edge_path, vec![2, 3]);
        assert_eq!(with_penalty.summary.total_travel_time_s, 12.0);
    }

    #[test]
    fn uses_automaton_for_multi_edge_restriction_sequences() {
        let graph = build_routing_graph(&multi_edge_restricted_topology(), &restricted_metrics())
            .expect("graph builds");

        assert!(graph.has_restriction_sequences());
    }

    #[test]
    fn respects_multi_edge_restriction_sequences() {
        let topology = multi_edge_restricted_topology();
        let metrics = restricted_metrics();
        let request = RouteRequest {
            route_id: "multi-edge-restricted".to_string(),
            origin: super::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "d".to_string(),
                lon: 6.003,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };

        let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
        assert_eq!(result.edge_path, vec![3, 2]);
        assert_eq!(result.summary.total_distance_m, 300);
    }

    #[test]
    fn detects_when_a_path_violates_multi_edge_restriction_sequences() {
        let graph = build_routing_graph(&multi_edge_restricted_topology(), &restricted_metrics())
            .expect("graph builds");

        assert!(!super::path_respects_restriction_sequences(
            &graph,
            &[0, 1, 2]
        ));
        assert!(super::path_respects_restriction_sequences(&graph, &[3, 2]));
    }

    #[test]
    fn can_ignore_multi_edge_restriction_sequences_via_engine_mode() {
        let topology = multi_edge_restricted_topology();
        let mut metrics = restricted_metrics();
        metrics.edge_metrics[3].travel_time_s = Some(25.0);
        metrics.edge_metrics[3].generalized_cost = Some(25.0);
        let engine = PreparedRoutingEngine::new(Arc::new(topology), Arc::new(metrics))
            .expect("prepared engine builds");
        let request = RouteRequest {
            route_id: "multi-edge-override".to_string(),
            origin: super::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "d".to_string(),
                lon: 6.003,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };

        let exact = engine
            .execute_route_with_mode(&request, EngineMode::Auto)
            .expect("exact route succeeds");
        let override_route = engine
            .execute_route_with_mode(&request, EngineMode::IgnoreMultiEdgeRestrictions)
            .expect("override route succeeds");
        let override_engine =
            engine.effective_engine_description(EngineMode::IgnoreMultiEdgeRestrictions);

        assert_eq!(exact.edge_path, vec![3, 2]);
        assert_eq!(override_route.edge_path, vec![0, 1, 2]);
        assert_eq!(
            override_engine.route_engine,
            "bidirectional_exact_pairwise_turns"
        );
    }

    #[test]
    fn loads_od_pairs_from_csv() {
        let path = write_temp_file(
            "od_pairs.csv",
            "id,source_x,source_y,target_x,target_y\npair_1,6.1,53.1,6.2,53.2\n",
        );

        let document = load_od_pairs(&path).expect("CSV loads");

        assert_eq!(document.pairs.len(), 1);
        assert_eq!(document.pairs[0].pair_id, "pair_1");
        assert_eq!(document.pairs[0].origin.id, "pair_1:source");
        assert_eq!(document.pairs[0].destination.id, "pair_1:target");
        assert_eq!(document.pairs[0].origin.lon, 6.1);
        assert_eq!(document.pairs[0].destination.lat, 53.2);
        fs::remove_file(path).ok();
    }

    #[test]
    fn loads_point_set_from_csv() {
        let path = write_temp_file("points.csv", "id,x,y\na,6.1,53.1\nb,6.2,53.2\n");

        let document = load_point_set(&path).expect("CSV loads");

        assert_eq!(document.points.len(), 2);
        assert_eq!(document.points[0].id, "a");
        assert_eq!(document.points[1].lon, 6.2);
        assert_eq!(document.points[1].lat, 53.2);
        fs::remove_file(path).ok();
    }

    #[test]
    fn loads_experiment_with_route_and_matrix_scenarios() {
        let path = write_temp_file(
            "experiment.yml",
            r#"
experiment:
  id: baseline_sweep
  label: Baseline Sweep
  dataset: groningen_2026_03
scenarios:
  - id: baseline_route
    profile: ../profiles/car_research_v1.yml
    analysis: route
    request: ../requests/route.json
  - id: baseline_matrix
    profile: ../profiles/car_research_v1.yml
    analysis: matrix
    origins: ../requests/matrix_origins.csv
    destinations: ../requests/matrix_destinations.csv
    out: exports/matrix.csv
"#,
        );

        let document = load_experiment(&path).expect("experiment loads");

        assert_eq!(document.experiment.id, "baseline_sweep");
        assert_eq!(document.scenarios.len(), 2);
        assert_eq!(document.scenarios[0].analysis, AnalysisKind::Route);
        assert_eq!(
            document.scenarios[1].origins.as_deref(),
            Some(PathBuf::from("../requests/matrix_origins.csv").as_path())
        );
        assert_eq!(
            document.scenarios[1].destinations.as_deref(),
            Some(PathBuf::from("../requests/matrix_destinations.csv").as_path())
        );
        fs::remove_file(path).ok();
    }

    fn test_topology() -> TopologyBundle {
        with_edge_based_topology(TopologyBundle {
            schema_version: 1,
            source_path: "test".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 1,
                    lon: 6.0,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 2,
                    lon: 6.001,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 3,
                    lon: 6.002,
                    lat: 53.0,
                },
            ],
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 10,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(1),
                    to: NodeId(2),
                    source_way_id: 10,
                    length_m: 200,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(2),
                    from: NodeId(0),
                    to: NodeId(2),
                    source_way_id: 11,
                    length_m: 500,
                    duration_s: None,
                    road_class: RoadClass::Service,
                    surface: SurfaceClass::Paved,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
            ],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: Default::default(),
            spatial_index: Some(spatial_index_for_all_nodes(3, 6.0, 53.0, 6.002, 53.0)),
            node_component_ids: vec![],
            edge_component_ids: vec![],
        })
    }

    fn test_metrics() -> CompiledProfileBundle {
        CompiledProfileBundle {
            schema_version: 2,
            profile_id: "test".to_string(),
            profile_hash: "abc".to_string(),
            mode: TravelMode::Car,
            turn_costs: CompiledTurnCostConfig::default(),
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            acceleration: None,
            edge_metrics: vec![
                CompiledEdgeMetric {
                    edge_id: EdgeId(0),
                    travel_time_s: Some(10.0),
                    generalized_cost: Some(10.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(1),
                    travel_time_s: Some(20.0),
                    generalized_cost: Some(20.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(2),
                    travel_time_s: Some(100.0),
                    generalized_cost: Some(100.0),
                },
            ],
        }
    }

    fn service_area_linear_topology() -> TopologyBundle {
        with_edge_based_topology(TopologyBundle {
            schema_version: 2,
            source_path: "service-area-linear".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 1,
                    lon: 6.0,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 2,
                    lon: 6.001,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 3,
                    lon: 6.002,
                    lat: 53.0,
                },
            ],
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 30,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(1),
                    to: NodeId(2),
                    source_way_id: 31,
                    length_m: 200,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
            ],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: Default::default(),
            spatial_index: Some(spatial_index_for_all_nodes(3, 6.0, 53.0, 6.002, 53.0)),
            node_component_ids: vec![],
            edge_component_ids: vec![],
        })
    }

    fn service_area_linear_metrics() -> CompiledProfileBundle {
        CompiledProfileBundle {
            schema_version: 2,
            profile_id: "service-area-test".to_string(),
            profile_hash: "abc".to_string(),
            mode: TravelMode::Car,
            turn_costs: CompiledTurnCostConfig::default(),
            source_topology_bundle_id: CacheBundleId::new("service-area-test"),
            acceleration: None,
            edge_metrics: vec![
                CompiledEdgeMetric {
                    edge_id: EdgeId(0),
                    travel_time_s: Some(10.0),
                    generalized_cost: Some(10.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(1),
                    travel_time_s: Some(20.0),
                    generalized_cost: Some(20.0),
                },
            ],
        }
    }

    fn accelerated_test_metrics() -> CompiledProfileBundle {
        let mut metrics = test_metrics();
        metrics.acceleration = Some(CompiledAcceleration {
            schema_version: 1,
            source_acceleration_bundle_id: CacheBundleId::new("acceleration-test"),
            algorithm: "test".to_string(),
            edge_order: vec![0, 1, 2],
            edge_rank: vec![2, 0, 1],
            upward_first_out: vec![0, 0, 0, 0],
            upward_head: vec![],
            upward_weight: vec![],
            upward_path_first_out: vec![0],
            upward_path_edges: vec![],
            downward_first_out: vec![0, 1, 1, 1],
            downward_head: vec![1],
            downward_weight: vec![20.0],
            downward_path_first_out: vec![0, 1],
            downward_path_edges: vec![1],
        });
        metrics
    }

    fn restricted_topology() -> TopologyBundle {
        with_edge_based_topology(TopologyBundle {
            schema_version: 3,
            source_path: "test".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 1,
                    lon: 6.0,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 2,
                    lon: 6.001,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 3,
                    lon: 6.002,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(3),
                    osm_node_id: 4,
                    lon: 6.003,
                    lat: 53.0,
                },
            ],
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 10,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(1),
                    to: NodeId(2),
                    source_way_id: 11,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(2),
                    from: NodeId(2),
                    to: NodeId(3),
                    source_way_id: 12,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(3),
                    from: NodeId(0),
                    to: NodeId(2),
                    source_way_id: 20,
                    length_m: 200,
                    duration_s: None,
                    road_class: RoadClass::Service,
                    surface: SurfaceClass::Paved,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(4),
                    from: NodeId(2),
                    to: NodeId(3),
                    source_way_id: 21,
                    length_m: 200,
                    duration_s: None,
                    road_class: RoadClass::Service,
                    surface: SurfaceClass::Paved,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
            ],
            turn_restrictions: vec![TurnRestriction {
                relation_id: 99,
                kind: TurnRestrictionKind::NoTurn,
                edge_path: vec![EdgeId(1), EdgeId(2)],
                mode_mask: AccessMask::new(AccessMask::CAR),
            }],
            names: vec![],
            edge_based_topology: Default::default(),
            spatial_index: Some(spatial_index_for_all_nodes(4, 6.0, 53.0, 6.003, 53.0)),
            node_component_ids: vec![],
            edge_component_ids: vec![],
        })
    }

    fn spatial_index_for_all_nodes(
        node_count: u32,
        min_lon: f64,
        min_lat: f64,
        max_lon: f64,
        max_lat: f64,
    ) -> NodeSpatialIndex {
        NodeSpatialIndex {
            bounds: TopologyBounds {
                min_lon,
                min_lat,
                max_lon,
                max_lat,
            },
            columns: 1,
            rows: 1,
            cell_width_deg: (max_lon - min_lon).max(0.001),
            cell_height_deg: (max_lat - min_lat).max(0.001),
            cells: vec![SpatialIndexCell {
                node_start: 0,
                node_len: node_count,
            }],
            node_ids: (0..node_count).collect(),
        }
    }

    fn restricted_metrics() -> CompiledProfileBundle {
        CompiledProfileBundle {
            schema_version: 2,
            profile_id: "test".to_string(),
            profile_hash: "abc".to_string(),
            mode: TravelMode::Car,
            turn_costs: CompiledTurnCostConfig::default(),
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            acceleration: None,
            edge_metrics: vec![
                CompiledEdgeMetric {
                    edge_id: EdgeId(0),
                    travel_time_s: Some(10.0),
                    generalized_cost: Some(10.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(1),
                    travel_time_s: Some(10.0),
                    generalized_cost: Some(10.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(2),
                    travel_time_s: Some(10.0),
                    generalized_cost: Some(10.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(3),
                    travel_time_s: Some(20.0),
                    generalized_cost: Some(20.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(4),
                    travel_time_s: Some(20.0),
                    generalized_cost: Some(20.0),
                },
            ],
        }
    }

    fn turn_penalty_topology() -> TopologyBundle {
        with_edge_based_topology(TopologyBundle {
            schema_version: 3,
            source_path: "test".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 1,
                    lon: 6.0,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 2,
                    lon: 6.001,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 3,
                    lon: 6.001,
                    lat: 53.001,
                },
                TopologyNode {
                    node_id: NodeId(3),
                    osm_node_id: 4,
                    lon: 6.002,
                    lat: 53.001,
                },
                TopologyNode {
                    node_id: NodeId(4),
                    osm_node_id: 5,
                    lon: 6.0,
                    lat: 53.001,
                },
            ],
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 10,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(1),
                    to: NodeId(2),
                    source_way_id: 11,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(2),
                    from: NodeId(2),
                    to: NodeId(3),
                    source_way_id: 12,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(3),
                    from: NodeId(0),
                    to: NodeId(4),
                    source_way_id: 20,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Service,
                    surface: SurfaceClass::Paved,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(4),
                    from: NodeId(4),
                    to: NodeId(3),
                    source_way_id: 21,
                    length_m: 200,
                    duration_s: None,
                    road_class: RoadClass::Service,
                    surface: SurfaceClass::Paved,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
            ],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: Default::default(),
            spatial_index: Some(spatial_index_for_all_nodes(5, 6.0, 53.0, 6.002, 53.001)),
            node_component_ids: vec![],
            edge_component_ids: vec![],
        })
    }

    fn multi_edge_restricted_topology() -> TopologyBundle {
        let mut topology = restricted_topology();
        topology.turn_restrictions = vec![TurnRestriction {
            relation_id: 100,
            kind: TurnRestrictionKind::NoTurn,
            edge_path: vec![EdgeId(0), EdgeId(1), EdgeId(2)],
            mode_mask: AccessMask::new(AccessMask::CAR),
        }];
        topology
    }

    fn traffic_signal_penalty_topology() -> TopologyBundle {
        let mut topology = turn_penalty_topology();
        topology.edges[1].flags = EDGE_FLAG_TARGET_TRAFFIC_SIGNAL;
        topology
    }

    fn roundabout_entry_penalty_topology() -> TopologyBundle {
        with_edge_based_topology(TopologyBundle {
            schema_version: 3,
            source_path: "test".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 1,
                    lon: 6.0,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 2,
                    lon: 6.001,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 3,
                    lon: 6.002,
                    lat: 53.001,
                },
                TopologyNode {
                    node_id: NodeId(3),
                    osm_node_id: 4,
                    lon: 6.0,
                    lat: 53.001,
                },
            ],
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 10,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(1),
                    to: NodeId(2),
                    source_way_id: 11,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: EDGE_FLAG_ROUNDABOUT,
                },
                DirectedEdge {
                    edge_id: EdgeId(2),
                    from: NodeId(0),
                    to: NodeId(3),
                    source_way_id: 20,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Service,
                    surface: SurfaceClass::Paved,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(3),
                    from: NodeId(3),
                    to: NodeId(2),
                    source_way_id: 21,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Service,
                    surface: SurfaceClass::Paved,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
            ],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: Default::default(),
            spatial_index: Some(spatial_index_for_all_nodes(4, 6.0, 53.0, 6.002, 53.001)),
            node_component_ids: vec![],
            edge_component_ids: vec![],
        })
    }

    fn roundabout_entry_penalty_metrics() -> CompiledProfileBundle {
        CompiledProfileBundle {
            schema_version: 3,
            profile_id: "test".to_string(),
            profile_hash: "abc".to_string(),
            mode: TravelMode::Car,
            turn_costs: CompiledTurnCostConfig {
                cost_time_weight: 1.0,
                ..CompiledTurnCostConfig::default()
            },
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            acceleration: None,
            edge_metrics: vec![
                CompiledEdgeMetric {
                    edge_id: EdgeId(0),
                    travel_time_s: Some(5.0),
                    generalized_cost: Some(5.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(1),
                    travel_time_s: Some(5.0),
                    generalized_cost: Some(5.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(2),
                    travel_time_s: Some(6.0),
                    generalized_cost: Some(6.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(3),
                    travel_time_s: Some(6.0),
                    generalized_cost: Some(6.0),
                },
            ],
        }
    }

    fn snap_test_topology() -> TopologyBundle {
        let nodes = (0..10)
            .map(|index| TopologyNode {
                node_id: NodeId(index),
                osm_node_id: (index + 1) as i64,
                lon: 6.0 + f64::from(index) * 0.0001,
                lat: 53.0,
            })
            .collect::<Vec<_>>();
        let edges = (0..9)
            .map(|index| DirectedEdge {
                edge_id: EdgeId(index),
                from: NodeId(index),
                to: NodeId(index + 1),
                source_way_id: i64::from(index) + 1,
                length_m: 10,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            })
            .collect::<Vec<_>>();
        with_edge_based_topology(TopologyBundle {
            schema_version: 3,
            source_path: "test".to_string(),
            source_sha256: "abc".to_string(),
            nodes,
            edges,
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: Default::default(),
            spatial_index: None,
            node_component_ids: vec![],
            edge_component_ids: vec![],
        })
    }

    fn with_edge_based_topology(mut topology: TopologyBundle) -> TopologyBundle {
        topology.edge_based_topology = super::build_edge_based_topology_fallback(&topology);
        if topology.node_component_ids.len() != topology.nodes.len()
            || topology.edge_component_ids.len() != topology.edges.len()
        {
            let (node_component_ids, edge_component_ids) = weak_components_for_test(&topology);
            topology.node_component_ids = node_component_ids;
            topology.edge_component_ids = edge_component_ids;
        }
        topology
    }

    fn weak_components_for_test(topology: &TopologyBundle) -> (Vec<u32>, Vec<u32>) {
        let node_count = topology.nodes.len();
        let mut parent = (0..node_count as u32).collect::<Vec<_>>();

        for edge in &topology.edges {
            union_test_components(&mut parent, edge.from.0 as usize, edge.to.0 as usize);
        }

        let mut remap = std::collections::BTreeMap::<u32, u32>::new();
        let mut node_component_ids = vec![0_u32; node_count];
        for node_index in 0..node_count {
            let root = find_test_component_root(&mut parent, node_index);
            let next_component_id = remap.len() as u32;
            let component_id = *remap.entry(root).or_insert(next_component_id);
            node_component_ids[node_index] = component_id;
        }

        let edge_component_ids = topology
            .edges
            .iter()
            .map(|edge| node_component_ids[edge.from.0 as usize])
            .collect();
        (node_component_ids, edge_component_ids)
    }

    fn find_test_component_root(parent: &mut [u32], index: usize) -> u32 {
        let parent_index = parent[index] as usize;
        if parent_index != index {
            parent[index] = find_test_component_root(parent, parent_index);
        }
        parent[index]
    }

    fn union_test_components(parent: &mut [u32], left: usize, right: usize) {
        let left_root = find_test_component_root(parent, left);
        let right_root = find_test_component_root(parent, right);
        if left_root == right_root {
            return;
        }
        if left_root <= right_root {
            parent[right_root as usize] = left_root;
        } else {
            parent[left_root as usize] = right_root;
        }
    }

    fn turn_penalty_metrics() -> CompiledProfileBundle {
        CompiledProfileBundle {
            schema_version: 3,
            profile_id: "test".to_string(),
            profile_hash: "abc".to_string(),
            mode: TravelMode::Car,
            turn_costs: CompiledTurnCostConfig {
                cost_time_weight: 1.0,
                ..CompiledTurnCostConfig::default()
            },
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            acceleration: None,
            edge_metrics: vec![
                CompiledEdgeMetric {
                    edge_id: EdgeId(0),
                    travel_time_s: Some(5.0),
                    generalized_cost: Some(5.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(1),
                    travel_time_s: Some(5.0),
                    generalized_cost: Some(5.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(2),
                    travel_time_s: Some(5.0),
                    generalized_cost: Some(5.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(3),
                    travel_time_s: Some(10.0),
                    generalized_cost: Some(10.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(4),
                    travel_time_s: Some(10.0),
                    generalized_cost: Some(10.0),
                },
            ],
        }
    }

    fn one_way_dead_end_topology() -> TopologyBundle {
        with_edge_based_topology(TopologyBundle {
            schema_version: 3,
            source_path: "oneway-dead-end".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 1,
                    lon: 6.0,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 2,
                    lon: 6.001,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 3,
                    lon: 6.002,
                    lat: 53.0,
                },
            ],
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 100,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(1),
                    to: NodeId(2),
                    source_way_id: 101,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
            ],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: Default::default(),
            spatial_index: Some(spatial_index_for_all_nodes(3, 6.0, 53.0, 6.002, 53.0)),
            node_component_ids: vec![],
            edge_component_ids: vec![],
        })
    }

    fn illegal_turn_only_topology() -> TopologyBundle {
        let mut topology = one_way_dead_end_topology();
        topology.turn_restrictions = vec![TurnRestriction {
            relation_id: 200,
            kind: TurnRestrictionKind::NoTurn,
            edge_path: vec![EdgeId(0), EdgeId(1)],
            mode_mask: AccessMask::new(AccessMask::CAR),
        }];
        topology
    }

    fn dead_node_snap_topology() -> TopologyBundle {
        with_edge_based_topology(TopologyBundle {
            schema_version: 3,
            source_path: "dead-node-snap".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 10,
                    lon: 6.0,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 11,
                    lon: 6.0001,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 12,
                    lon: 6.0002,
                    lat: 53.0,
                },
            ],
            edges: vec![DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(1),
                to: NodeId(2),
                source_way_id: 310,
                length_m: 10,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            }],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: Default::default(),
            spatial_index: Some(spatial_index_for_all_nodes(3, 6.0, 53.0, 6.0002, 53.0)),
            node_component_ids: vec![],
            edge_component_ids: vec![],
        })
    }

    fn ignored_restriction_only_topology() -> TopologyBundle {
        with_edge_based_topology(TopologyBundle {
            schema_version: 3,
            source_path: "ignored-restriction".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 1,
                    lon: 6.0,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 2,
                    lon: 6.001,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 3,
                    lon: 6.002,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(3),
                    osm_node_id: 4,
                    lon: 6.003,
                    lat: 53.0,
                },
            ],
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 210,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(1),
                    to: NodeId(2),
                    source_way_id: 211,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(2),
                    from: NodeId(2),
                    to: NodeId(3),
                    source_way_id: 212,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
            ],
            turn_restrictions: vec![TurnRestriction {
                relation_id: 201,
                kind: TurnRestrictionKind::NoTurn,
                edge_path: vec![EdgeId(0), EdgeId(1), EdgeId(2)],
                mode_mask: AccessMask::new(AccessMask::CAR),
            }],
            names: vec![],
            edge_based_topology: Default::default(),
            spatial_index: Some(spatial_index_for_all_nodes(4, 6.0, 53.0, 6.003, 53.0)),
            node_component_ids: vec![],
            edge_component_ids: vec![],
        })
    }

    fn forbidden_uturn_topology() -> TopologyBundle {
        with_edge_based_topology(TopologyBundle {
            schema_version: 3,
            source_path: "forbidden-uturn".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 1,
                    lon: 6.0,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 2,
                    lon: 6.001,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 3,
                    lon: 6.0,
                    lat: 53.001,
                },
            ],
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 300,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(1),
                    to: NodeId(0),
                    source_way_id: 301,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(2),
                    from: NodeId(0),
                    to: NodeId(2),
                    source_way_id: 302,
                    length_m: 120,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
            ],
            turn_restrictions: vec![TurnRestriction {
                relation_id: 202,
                kind: TurnRestrictionKind::NoTurn,
                edge_path: vec![EdgeId(0), EdgeId(1)],
                mode_mask: AccessMask::new(AccessMask::CAR),
            }],
            names: vec![],
            edge_based_topology: Default::default(),
            spatial_index: Some(spatial_index_for_all_nodes(3, 6.0, 53.0, 6.001, 53.001)),
            node_component_ids: vec![],
            edge_component_ids: vec![],
        })
    }

    fn ferry_only_subnetwork_topology() -> TopologyBundle {
        with_edge_based_topology(TopologyBundle {
            schema_version: 3,
            source_path: "ferry-only".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 1,
                    lon: 6.0,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 2,
                    lon: 6.001,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 3,
                    lon: 6.01,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(3),
                    osm_node_id: 4,
                    lon: 6.011,
                    lat: 53.0,
                },
            ],
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 400,
                    length_m: 100,
                    duration_s: Some(60.0),
                    road_class: RoadClass::Ferry,
                    surface: SurfaceClass::Unknown,
                    smoothness: SmoothnessClass::Unknown,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(2),
                    to: NodeId(3),
                    source_way_id: 401,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
            ],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: Default::default(),
            spatial_index: Some(spatial_index_for_all_nodes(4, 6.0, 53.0, 6.011, 53.0)),
            node_component_ids: vec![],
            edge_component_ids: vec![],
        })
    }

    fn uniform_metrics(edge_count: usize, travel_time_s: f64) -> CompiledProfileBundle {
        CompiledProfileBundle {
            schema_version: 3,
            profile_id: "test".to_string(),
            profile_hash: "abc".to_string(),
            mode: TravelMode::Car,
            turn_costs: CompiledTurnCostConfig {
                cost_time_weight: 1.0,
                ..CompiledTurnCostConfig::default()
            },
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            acceleration: None,
            edge_metrics: (0..edge_count)
                .map(|edge_index| CompiledEdgeMetric {
                    edge_id: EdgeId(edge_index as u32),
                    travel_time_s: Some(travel_time_s),
                    generalized_cost: Some(travel_time_s),
                })
                .collect(),
        }
    }

    #[test]
    fn reports_structured_diagnostics_for_component_mismatch() {
        let topology = disconnected_topology();
        let metrics = disconnected_metrics();
        let request = RouteRequest {
            route_id: "disconnected".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.01,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 100.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
        };

        let error = execute_route(&topology, &metrics, &request).expect_err("route should fail");
        let failure = analysis_failure(&error).expect("failure should be structured");

        assert_eq!(failure.outcome, super::AnalysisOutcome::Unreachable);
        assert_eq!(failure.diagnostics.len(), 1);
        assert_eq!(
            failure.diagnostics[0].code,
            AnalysisDiagnosticCode::DisconnectedComponents
        );
        assert_eq!(failure.diagnostics[0].component_ids, vec![0, 1]);
    }

    #[test]
    fn returns_partial_route_when_ignore_unreachable_is_enabled() {
        let topology = disconnected_topology();
        let metrics = disconnected_metrics();
        let request = RouteRequest {
            route_id: "disconnected-ignore".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.01,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 100.0,
            },
            connectivity: ConnectivityPolicy {
                disconnected: DisconnectedNetworkMode::IgnoreUnreachable,
                ..ConnectivityPolicy::default()
            },
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };

        let result = execute_route(&topology, &metrics, &request).expect("route returns partial");

        assert_eq!(result.outcome, super::AnalysisOutcome::Partial);
        assert!(!result.fallback_used);
        assert_eq!(result.summary.total_distance_m, 0);
        assert_eq!(result.summary.total_travel_time_s, 0.0);
        assert!(result.geometry.is_none());
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].code,
            AnalysisDiagnosticCode::DisconnectedComponents
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("ignored an unreachable pair"))
        );
    }

    #[test]
    fn marks_ignored_unreachable_pairs_explicitly_in_od_batches() {
        let topology = disconnected_topology();
        let metrics = disconnected_metrics();
        let document = OdPairsDocument {
            pairs: vec![
                OdPair {
                    pair_id: "connected".to_string(),
                    origin: super::LabeledPoint {
                        id: "origin_a".to_string(),
                        lon: 6.0,
                        lat: 53.0,
                    },
                    destination: super::LabeledPoint {
                        id: "destination_a".to_string(),
                        lon: 6.001,
                        lat: 53.0,
                    },
                },
                OdPair {
                    pair_id: "ignored".to_string(),
                    origin: super::LabeledPoint {
                        id: "origin_b".to_string(),
                        lon: 6.0,
                        lat: 53.0,
                    },
                    destination: super::LabeledPoint {
                        id: "destination_b".to_string(),
                        lon: 6.01,
                        lat: 53.0,
                    },
                },
            ],
            snap: SnapOptions {
                max_distance_m: 100.0,
            },
            connectivity: ConnectivityPolicy {
                disconnected: DisconnectedNetworkMode::IgnoreUnreachable,
                ..ConnectivityPolicy::default()
            },
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };

        let result = execute_od(&topology, &metrics, &document).expect("OD succeeds");

        assert_eq!(result.succeeded_count, 1);
        assert_eq!(result.ignored_count, 1);
        assert_eq!(result.failed_count, 0);
        assert_eq!(result.pairs[0].status, super::BatchItemStatus::Succeeded);
        assert_eq!(result.pairs[1].status, super::BatchItemStatus::Ignored);
        assert_eq!(result.pairs[1].outcome, super::AnalysisOutcome::Partial);
        assert_eq!(result.pairs[1].total_distance_m, None);
        assert!(result.pairs[1].geometry.is_none());
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("status='ignored'"))
        );
    }

    #[test]
    fn service_area_ignores_unreachable_origins_under_ignore_policy() {
        let topology = service_area_linear_topology();
        let metrics = service_area_linear_metrics();
        let request = super::ServiceAreaRequest {
            analysis_id: "service-area-ignore".to_string(),
            origins: vec![
                super::LabeledPoint {
                    id: "reachable".to_string(),
                    lon: 6.0,
                    lat: 53.0,
                },
                super::LabeledPoint {
                    id: "too_far".to_string(),
                    lon: 6.5,
                    lat: 53.5,
                },
            ],
            thresholds: vec![ServiceAreaThreshold {
                id: Some("band".to_string()),
                limit: 15.0,
                metric: ServiceAreaThresholdMetric::TravelTimeS,
            }],
            snap: SnapOptions {
                max_distance_m: 50.0,
            },
            connectivity: ConnectivityPolicy {
                disconnected: DisconnectedNetworkMode::IgnoreUnreachable,
                ..ConnectivityPolicy::default()
            },
            fallback: Default::default(),
            output_mode: ServiceAreaOutputMode::Network,
            band_mode: ServiceAreaBandMode::Cumulative,
            boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
            multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
            polygon: Default::default(),
            returns: Default::default(),
        };

        let result =
            execute_service_area(&topology, &metrics, &request).expect("service area succeeds");

        assert_eq!(result.outcome, super::AnalysisOutcome::Partial);
        assert_eq!(result.processed_origin_count, 1);
        assert_eq!(result.skipped_origin_count, 1);
        assert!(
            result
                .features
                .iter()
                .all(|feature| feature.origin_component_id.is_some())
        );
    }

    #[test]
    fn rejects_service_area_failure_modes_until_supported() {
        let request = super::ServiceAreaRequest {
            analysis_id: "service-area-fallback".to_string(),
            origins: vec![super::LabeledPoint {
                id: "a".to_string(),
                lon: 6.0,
                lat: 53.0,
            }],
            thresholds: vec![ServiceAreaThreshold {
                id: Some("band".to_string()),
                limit: 10.0,
                metric: ServiceAreaThresholdMetric::DistanceM,
            }],
            snap: Default::default(),
            connectivity: Default::default(),
            fallback: FallbackPolicy {
                allow_reverse_oneway: true,
                ..FallbackPolicy::default()
            },
            output_mode: ServiceAreaOutputMode::Network,
            band_mode: ServiceAreaBandMode::Cumulative,
            boundary_mode: ServiceAreaBoundaryMode::Overlap,
            multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
            polygon: Default::default(),
            returns: Default::default(),
        };

        let path = write_temp_file(
            "service_area_failure_mode.json",
            &serde_json::to_string_pretty(&request).expect("request serializes"),
        );
        let error = load_service_area_request(&path).expect_err("request should fail validation");
        assert!(
            error
                .to_string()
                .contains("does not support fallback failure modes")
        );
        fs::remove_file(path).ok();
    }

    #[test]
    fn allows_reverse_oneway_when_requested() {
        let topology = one_way_dead_end_topology();
        let metrics = uniform_metrics(topology.edges.len(), 10.0);
        let request = RouteRequest {
            route_id: "reverse-oneway".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.002,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 2.0,
            },
            connectivity: Default::default(),
            fallback: FallbackPolicy {
                allow_reverse_oneway: true,
                penalties: IllegalMovementPenaltyPolicy {
                    reverse_oneway_penalty_s: Some(30.0),
                    ..IllegalMovementPenaltyPolicy::default()
                },
                ..FallbackPolicy::default()
            },
            returns: ReturnConfig::default(),
        };

        let result = execute_route(&topology, &metrics, &request).expect("degraded route succeeds");
        assert_eq!(result.outcome, super::AnalysisOutcome::Degraded);
        assert_eq!(result.summary.violation_count, 2);
        assert!(
            result
                .summary
                .violation_types
                .contains(&super::RouteViolationType::ReverseOneway)
        );
    }

    #[test]
    fn strict_route_still_fails_on_oneway_dead_end() {
        let topology = one_way_dead_end_topology();
        let metrics = uniform_metrics(topology.edges.len(), 10.0);
        let request = RouteRequest {
            route_id: "reverse-oneway-strict".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.002,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.001,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 2.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
        };

        assert!(execute_route(&topology, &metrics, &request).is_err());
    }

    #[test]
    fn auto_relaxes_unreachable_route_with_minimal_reverse_oneway_policy() {
        let topology = one_way_dead_end_topology();
        let metrics = uniform_metrics(topology.edges.len(), 10.0);
        let request = RouteRequest {
            route_id: "reverse-oneway-auto".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0019,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.001,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 40.0,
            },
            connectivity: Default::default(),
            fallback: FallbackPolicy {
                auto_relax_unreachable: true,
                ..FallbackPolicy::default()
            },
            returns: ReturnConfig::default(),
        };

        let result = execute_route(&topology, &metrics, &request).expect("auto route succeeds");
        assert_eq!(result.outcome, super::AnalysisOutcome::Degraded);
        assert_eq!(
            result.summary.violation_types,
            vec![super::RouteViolationType::ReverseOneway]
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("allow_reverse_oneway"))
        );
    }

    #[test]
    fn allows_illegal_turn_when_requested() {
        let topology = illegal_turn_only_topology();
        let metrics = uniform_metrics(topology.edges.len(), 10.0);
        let request = RouteRequest {
            route_id: "illegal-turn".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.002,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 40.0,
            },
            connectivity: Default::default(),
            fallback: FallbackPolicy {
                allow_illegal_turn: true,
                penalties: IllegalMovementPenaltyPolicy {
                    illegal_turn_penalty_s: Some(45.0),
                    ..IllegalMovementPenaltyPolicy::default()
                },
                ..FallbackPolicy::default()
            },
            returns: ReturnConfig::default(),
        };

        let result =
            execute_route(&topology, &metrics, &request).expect("illegal turn route succeeds");
        assert_eq!(result.outcome, super::AnalysisOutcome::Degraded);
        assert!(
            result
                .summary
                .violation_types
                .contains(&super::RouteViolationType::IllegalTurn)
        );
    }

    #[test]
    fn ignores_multi_edge_restriction_only_when_explicitly_requested() {
        let topology = ignored_restriction_only_topology();
        let metrics = uniform_metrics(topology.edges.len(), 10.0);
        let strict_request = RouteRequest {
            route_id: "ignored-restriction-strict".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.003,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 40.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
        };
        assert!(execute_route(&topology, &metrics, &strict_request).is_err());

        let degraded_request = RouteRequest {
            fallback: FallbackPolicy {
                ignore_turn_restrictions: true,
                penalties: IllegalMovementPenaltyPolicy {
                    ignored_turn_restriction_penalty_s: Some(60.0),
                    ..IllegalMovementPenaltyPolicy::default()
                },
                ..FallbackPolicy::default()
            },
            ..strict_request
        };
        let result = execute_route(&topology, &metrics, &degraded_request)
            .expect("ignored restriction route succeeds");
        assert_eq!(result.outcome, super::AnalysisOutcome::Degraded);
        assert!(
            result
                .summary
                .violation_types
                .contains(&super::RouteViolationType::IgnoredTurnRestriction)
        );
    }

    #[test]
    fn allows_forbidden_uturn_when_requested() {
        let topology = forbidden_uturn_topology();
        let metrics = uniform_metrics(topology.edges.len(), 10.0);
        let strict_request = RouteRequest {
            route_id: "forbidden-uturn-strict".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0009,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.0,
                lat: 53.001,
            },
            snap: SnapOptions {
                max_distance_m: 40.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
        };

        let degraded_request = RouteRequest {
            fallback: FallbackPolicy {
                allow_uturn_where_normally_forbidden: true,
                penalties: IllegalMovementPenaltyPolicy {
                    forbidden_uturn_penalty_s: Some(15.0),
                    ..IllegalMovementPenaltyPolicy::default()
                },
                ..FallbackPolicy::default()
            },
            ..strict_request
        };
        let result =
            execute_route(&topology, &metrics, &degraded_request).expect("uturn route succeeds");
        assert_eq!(result.outcome, super::AnalysisOutcome::Degraded);
        assert!(
            result
                .summary
                .violation_types
                .contains(&super::RouteViolationType::ForbiddenUturn)
        );
    }

    #[test]
    fn detects_ferry_only_component_fixture() {
        let topology = ferry_only_subnetwork_topology();
        let metrics = uniform_metrics(topology.edges.len(), 10.0);
        let request = RouteRequest {
            route_id: "ferry-component".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.011,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 40.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
        };

        let error = execute_route(&topology, &metrics, &request).expect_err("route should fail");
        let failure = analysis_failure(&error).expect("failure should be structured");
        assert_eq!(
            failure.diagnostics[0].code,
            AnalysisDiagnosticCode::DisconnectedComponents
        );
    }

    #[test]
    fn loads_service_area_request_schema() {
        let path = write_temp_file(
            "service-area.json",
            r#"{
  "analysis_id": "sa_demo",
  "origins": [{"id":"o1","lon":6.56,"lat":53.22}],
  "thresholds": [{"id":"five_min","limit":300.0,"metric":"travel_time_s"}],
  "output_mode": "both",
  "band_mode": "cumulative",
  "boundary_mode": "overlap",
  "multi_origin_mode": "merge"
}"#,
        );

        let request = load_service_area_request(&path).expect("service-area request parses");
        fs::remove_file(path).expect("fixture removed");

        assert_eq!(request.analysis_id, "sa_demo");
        assert_eq!(request.origins.len(), 1);
        assert_eq!(request.thresholds.len(), 1);
        assert!(matches!(
            request.thresholds[0].metric,
            super::ServiceAreaThresholdMetric::TravelTimeS
        ));
    }

    #[test]
    fn executes_service_area_with_partial_edge_frontier() {
        let request = super::ServiceAreaRequest {
            analysis_id: "service-area-cut".to_string(),
            origins: vec![super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0,
                lat: 53.0,
            }],
            thresholds: vec![ServiceAreaThreshold {
                id: Some("fifteen_s".to_string()),
                limit: 15.0,
                metric: ServiceAreaThresholdMetric::TravelTimeS,
            }],
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            output_mode: ServiceAreaOutputMode::Network,
            band_mode: ServiceAreaBandMode::Cumulative,
            boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
            multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
            polygon: Default::default(),
            returns: Default::default(),
        };

        let result = execute_service_area(
            &service_area_linear_topology(),
            &service_area_linear_metrics(),
            &request,
        )
        .expect("service area succeeds");

        assert_eq!(result.outcome, super::AnalysisOutcome::Legal);
        assert_eq!(result.features.len(), 1);
        assert_eq!(result.summaries.len(), 1);
        assert_eq!(result.summaries[0].reachable_edge_count, Some(2));
        assert_eq!(result.summaries[0].reachable_network_length_m, Some(150.0));
        assert_eq!(
            result.features[0].geometry_type,
            super::ServiceAreaGeometryType::Network
        );
        assert_eq!(
            result.features[0]
                .geometry
                .as_ref()
                .and_then(|value| value.get("type"))
                .and_then(|value| value.as_str()),
            Some("MultiLineString")
        );
    }

    #[test]
    fn executes_service_area_ring_bands() {
        let request = super::ServiceAreaRequest {
            analysis_id: "service-area-ring".to_string(),
            origins: vec![super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0,
                lat: 53.0,
            }],
            thresholds: vec![
                ServiceAreaThreshold {
                    id: Some("ten_s".to_string()),
                    limit: 10.0,
                    metric: ServiceAreaThresholdMetric::TravelTimeS,
                },
                ServiceAreaThreshold {
                    id: Some("thirty_s".to_string()),
                    limit: 30.0,
                    metric: ServiceAreaThresholdMetric::TravelTimeS,
                },
            ],
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            output_mode: ServiceAreaOutputMode::Network,
            band_mode: ServiceAreaBandMode::Ring,
            boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
            multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
            polygon: Default::default(),
            returns: Default::default(),
        };

        let result = execute_service_area(
            &service_area_linear_topology(),
            &service_area_linear_metrics(),
            &request,
        )
        .expect("service area succeeds");

        assert_eq!(result.features.len(), 2);
        assert_eq!(result.features[0].reachable_network_length_m, Some(100.0));
        assert_eq!(result.features[1].band_start_limit, Some(10.0));
        assert_eq!(result.features[1].reachable_network_length_m, Some(200.0));
    }

    #[test]
    fn merges_multi_origin_service_areas() {
        let request = super::ServiceAreaRequest {
            analysis_id: "service-area-merge".to_string(),
            origins: vec![
                super::LabeledPoint {
                    id: "origin_a".to_string(),
                    lon: 6.0,
                    lat: 53.0,
                },
                super::LabeledPoint {
                    id: "origin_b".to_string(),
                    lon: 6.001,
                    lat: 53.0,
                },
            ],
            thresholds: vec![ServiceAreaThreshold {
                id: Some("thirty_s".to_string()),
                limit: 30.0,
                metric: ServiceAreaThresholdMetric::TravelTimeS,
            }],
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            output_mode: ServiceAreaOutputMode::Network,
            band_mode: ServiceAreaBandMode::Cumulative,
            boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
            multi_origin_mode: ServiceAreaMultiOriginMode::Merge,
            polygon: Default::default(),
            returns: Default::default(),
        };

        let result = execute_service_area(
            &service_area_linear_topology(),
            &service_area_linear_metrics(),
            &request,
        )
        .expect("service area succeeds");

        assert_eq!(result.features.len(), 1);
        assert_eq!(result.features[0].origin_id, None);
        assert_eq!(result.summaries.len(), 2);
        assert_eq!(result.processed_origin_count, 2);
    }

    #[test]
    fn emits_service_area_polygon_output() {
        let request = super::ServiceAreaRequest {
            analysis_id: "service-area-polygon".to_string(),
            origins: vec![super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0,
                lat: 53.0,
            }],
            thresholds: vec![ServiceAreaThreshold {
                id: Some("three_hundred_m".to_string()),
                limit: 300.0,
                metric: ServiceAreaThresholdMetric::DistanceM,
            }],
            snap: SnapOptions {
                max_distance_m: 500.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            output_mode: ServiceAreaOutputMode::Both,
            band_mode: ServiceAreaBandMode::Cumulative,
            boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
            multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
            polygon: Default::default(),
            returns: Default::default(),
        };

        let result = execute_service_area(
            &service_area_linear_topology(),
            &service_area_linear_metrics(),
            &request,
        )
        .expect("service area succeeds");

        assert_eq!(result.features.len(), 2);
        assert!(result.features.iter().any(|feature| {
            feature.geometry_type == super::ServiceAreaGeometryType::Polygon
                && feature
                    .geometry
                    .as_ref()
                    .and_then(|value| value.get("type"))
                    .and_then(|value| value.as_str())
                    .is_some()
        }));
    }

    #[test]
    fn allows_origin_hop_fallback_between_components() {
        let topology = hop_disconnected_topology();
        let metrics = hop_disconnected_metrics();
        let request = RouteRequest {
            route_id: "origin-hop".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0002,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.0015,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 40.0,
            },
            connectivity: ConnectivityPolicy {
                disconnected: DisconnectedNetworkMode::HopOriginToNearestReachableComponent,
                max_hop_distance_m: Some(120.0),
                report_hop_distance_separately: true,
            },
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
        };

        let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
        assert_eq!(result.outcome, super::AnalysisOutcome::Degraded);
        assert!(result.fallback_used);
        assert!(result.origin_hop_distance_m.is_some());
        assert_eq!(result.destination_hop_distance_m, None);
        assert_eq!(result.hop_segments.len(), 1);
        assert_eq!(result.hop_segments[0].endpoint, super::HopEndpoint::Origin);
    }

    #[test]
    fn allows_destination_hop_fallback_between_components() {
        let topology = hop_disconnected_topology();
        let metrics = hop_disconnected_metrics();
        let request = RouteRequest {
            route_id: "destination-hop".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0010,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.0003,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 40.0,
            },
            connectivity: ConnectivityPolicy {
                disconnected: DisconnectedNetworkMode::HopDestinationToNearestReachableComponent,
                max_hop_distance_m: Some(120.0),
                report_hop_distance_separately: true,
            },
            fallback: Default::default(),
            returns: ReturnConfig::default(),
        };

        let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
        assert_eq!(result.outcome, super::AnalysisOutcome::Degraded);
        assert!(result.fallback_used);
        assert_eq!(result.origin_hop_distance_m, None);
        assert!(result.destination_hop_distance_m.is_some());
        assert_eq!(result.hop_segments.len(), 1);
        assert_eq!(
            result.hop_segments[0].endpoint,
            super::HopEndpoint::Destination
        );
    }

    #[test]
    fn allows_either_end_hop_fallback_between_components() {
        let topology = hop_disconnected_topology();
        let metrics = hop_disconnected_metrics();
        let request = RouteRequest {
            route_id: "either-hop".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0002,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.0015,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 40.0,
            },
            connectivity: ConnectivityPolicy {
                disconnected: DisconnectedNetworkMode::HopEitherEnd,
                max_hop_distance_m: Some(120.0),
                report_hop_distance_separately: true,
            },
            fallback: Default::default(),
            returns: ReturnConfig::default(),
        };

        let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
        assert!(result.fallback_used);
        assert_eq!(result.outcome, super::AnalysisOutcome::Degraded);
    }

    #[test]
    fn hop_fallback_keeps_legal_network_cost_unchanged() {
        let topology = hop_disconnected_topology();
        let metrics = hop_disconnected_metrics();
        let legal_request = RouteRequest {
            route_id: "legal".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0010,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.0015,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 40.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
        };
        let hopped_request = RouteRequest {
            route_id: "hopped".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0002,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.0015,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 40.0,
            },
            connectivity: ConnectivityPolicy {
                disconnected: DisconnectedNetworkMode::HopOriginToNearestReachableComponent,
                max_hop_distance_m: Some(120.0),
                report_hop_distance_separately: true,
            },
            fallback: Default::default(),
            returns: ReturnConfig::default(),
        };

        let legal = execute_route(&topology, &metrics, &legal_request).expect("legal route");
        let hopped = execute_route(&topology, &metrics, &hopped_request).expect("hopped route");

        assert_eq!(
            hopped.summary.total_distance_m,
            legal.summary.total_distance_m
        );
        assert_eq!(
            hopped.summary.total_travel_time_s,
            legal.summary.total_travel_time_s
        );
        assert_eq!(
            hopped.summary.total_generalized_cost,
            legal.summary.total_generalized_cost
        );
        assert!(hopped.origin_hop_distance_m.is_some());
    }

    #[test]
    fn skips_non_traversable_nodes_when_snapping_route_endpoints() {
        let topology = dead_node_snap_topology();
        let metrics = uniform_metrics(topology.edges.len(), 10.0);
        let request = RouteRequest {
            route_id: "dead-node-snap".to_string(),
            origin: super::LabeledPoint {
                id: "origin".to_string(),
                lon: 6.0,
                lat: 53.0,
            },
            destination: super::LabeledPoint {
                id: "destination".to_string(),
                lon: 6.0002,
                lat: 53.0,
            },
            snap: SnapOptions {
                max_distance_m: 30.0,
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
        };

        let result = execute_route(&topology, &metrics, &request).expect("route succeeds");
        assert_ne!(result.origin.snapped_node_id, 0);
        assert_eq!(result.origin.snapped_node_id, 1);
        assert_eq!(result.destination.snapped_node_id, 2);
    }

    fn disconnected_topology() -> TopologyBundle {
        with_edge_based_topology(TopologyBundle {
            schema_version: 7,
            source_path: "disconnected".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 100,
                    lon: 6.0,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 101,
                    lon: 6.001,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 102,
                    lon: 6.01,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(3),
                    osm_node_id: 103,
                    lon: 6.011,
                    lat: 53.0,
                },
            ],
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 10,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(2),
                    to: NodeId(3),
                    source_way_id: 11,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
            ],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: Default::default(),
            spatial_index: Some(spatial_index_for_all_nodes(4, 6.0, 53.0, 6.011, 53.0)),
            node_component_ids: vec![],
            edge_component_ids: vec![],
        })
    }

    fn hop_disconnected_topology() -> TopologyBundle {
        with_edge_based_topology(TopologyBundle {
            schema_version: 7,
            source_path: "hop-disconnected".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 200,
                    lon: 6.0,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 201,
                    lon: 6.0005,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 202,
                    lon: 6.0010,
                    lat: 53.0,
                },
                TopologyNode {
                    node_id: NodeId(3),
                    osm_node_id: 203,
                    lon: 6.0015,
                    lat: 53.0,
                },
            ],
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 20,
                    length_m: 50,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
                DirectedEdge {
                    edge_id: EdgeId(1),
                    from: NodeId(2),
                    to: NodeId(3),
                    source_way_id: 21,
                    length_m: 50,
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::CAR),
                    is_toll: false,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
            ],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: Default::default(),
            spatial_index: Some(spatial_index_for_all_nodes(4, 6.0, 53.0, 6.0015, 53.0)),
            node_component_ids: vec![],
            edge_component_ids: vec![],
        })
    }

    fn hop_disconnected_metrics() -> CompiledProfileBundle {
        CompiledProfileBundle {
            schema_version: 3,
            profile_id: "test".to_string(),
            profile_hash: "abc".to_string(),
            mode: TravelMode::Car,
            turn_costs: CompiledTurnCostConfig::default(),
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            acceleration: None,
            edge_metrics: vec![
                CompiledEdgeMetric {
                    edge_id: EdgeId(0),
                    travel_time_s: Some(5.0),
                    generalized_cost: Some(5.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(1),
                    travel_time_s: Some(5.0),
                    generalized_cost: Some(5.0),
                },
            ],
        }
    }

    fn disconnected_metrics() -> CompiledProfileBundle {
        CompiledProfileBundle {
            schema_version: 3,
            profile_id: "test".to_string(),
            profile_hash: "abc".to_string(),
            mode: TravelMode::Car,
            turn_costs: CompiledTurnCostConfig::default(),
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            acceleration: None,
            edge_metrics: vec![
                CompiledEdgeMetric {
                    edge_id: EdgeId(0),
                    travel_time_s: Some(5.0),
                    generalized_cost: Some(5.0),
                },
                CompiledEdgeMetric {
                    edge_id: EdgeId(1),
                    travel_time_s: Some(5.0),
                    generalized_cost: Some(5.0),
                },
            ],
        }
    }

    fn write_temp_file(name: &str, contents: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time works")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("netan-query-{unique}-{name}"));
        fs::write(&path, contents).expect("fixture written");
        path
    }
}
