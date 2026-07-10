use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::{
    AccessibilityCategoryResult, AccessibilityRequest, AccessibilityResult, AnalysisOutcome,
    BatchItemStatus, BetweennessRequest, BetweennessResult, MatrixResult, OdPairsDocument,
    OdResult, PointSetDocument, PreparedRoutingEngine, RouteRequest, RouteResult,
    ServiceAreaRequest, ServiceAreaResult, TemporalRequestOptions,
};

/// A runtime scenario and the overlay file that defines its edge/feature
/// state. Paths in a loaded document are resolved relative to that document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioBatchCase {
    pub id: String,
    #[serde(flatten)]
    pub selector: ScenarioBatchSelector,
}

/// Exactly one scenario source. The untagged shape preserves existing
/// `overlay: path.yml` cases while adding generated closure selectors.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ScenarioBatchSelector {
    Overlay { overlay: PathBuf },
    TopK { top_k: ScenarioTopKSelector },
    Group { group: ScenarioGroupSelector },
}

impl<'de> Deserialize<'de> for ScenarioBatchSelector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SelectorFields {
            #[serde(default)]
            overlay: Option<PathBuf>,
            #[serde(default)]
            top_k: Option<ScenarioTopKSelector>,
            #[serde(default)]
            group: Option<ScenarioGroupSelector>,
        }

        let fields = SelectorFields::deserialize(deserializer)?;
        match (fields.overlay, fields.top_k, fields.group) {
            (Some(overlay), None, None) => Ok(Self::Overlay { overlay }),
            (None, Some(top_k), None) => Ok(Self::TopK { top_k }),
            (None, None, Some(group)) => Ok(Self::Group { group }),
            _ => Err(D::Error::custom(
                "scenario must specify exactly one selector: overlay, top_k, or group",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioTopKSelector {
    /// CSV/JSON/YAML feature-score input, or a prior betweenness result JSON.
    pub ranking: PathBuf,
    #[serde(alias = "k")]
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioGroupSelector {
    /// Retained source attribute name or semantic role (for example
    /// `mall_id`, `asset_group`, or `building_id`).
    pub attribute: String,
    /// Raw value or decoded domain label to close.
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioFeatureScore {
    #[serde(alias = "feature_id", alias = "source_way_id")]
    pub source_feature_id: i64,
    pub score: f64,
}

/// Mixed analysis batch evaluated once without a scenario and once for each
/// runtime overlay. Requests retain their holiday calendars and continuous
/// temporal overlays; only the scenario file is replaced per case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioBatchRequest {
    pub batch_id: String,
    /// Used when a scenario analysis request does not carry its own
    /// departure/arrive-by time.
    #[serde(default)]
    pub departure_time: Option<String>,
    #[serde(default)]
    pub scenarios: Vec<ScenarioBatchCase>,
    #[serde(default, alias = "route_requests")]
    pub routes: Vec<RouteRequest>,
    #[serde(default, alias = "service_area_requests")]
    pub service_areas: Vec<ServiceAreaRequest>,
    #[serde(default, alias = "accessibility_requests")]
    pub accessibility: Vec<AccessibilityRequest>,
    #[serde(default, alias = "od_requests")]
    pub od: Vec<ScenarioOdRequest>,
    #[serde(default, alias = "matrix", alias = "matrix_requests")]
    pub matrices: Vec<ScenarioMatrixRequest>,
    #[serde(default, alias = "betweenness_requests")]
    pub betweenness: Vec<BetweennessRequest>,
}

/// An OD document needs a stable analysis id in addition to its per-pair ids
/// so multiple OD analyses can coexist in one scenario batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioOdRequest {
    pub analysis_id: String,
    pub request: OdPairsDocument,
}

/// Matrix inputs plus a stable id used to match baseline and scenario runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioMatrixRequest {
    pub analysis_id: String,
    pub origins: PointSetDocument,
    pub destinations: PointSetDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioRouteExecution {
    pub route_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<RouteResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioServiceAreaExecution {
    pub analysis_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ServiceAreaResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioAccessibilityExecution {
    pub analysis_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<AccessibilityResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioOdExecution {
    pub analysis_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<OdResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioMatrixExecution {
    pub analysis_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<MatrixResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioBetweennessExecution {
    pub analysis_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<BetweennessResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScenarioAnalysisSnapshot {
    #[serde(default)]
    pub routes: Vec<ScenarioRouteExecution>,
    #[serde(default)]
    pub service_areas: Vec<ScenarioServiceAreaExecution>,
    #[serde(default)]
    pub accessibility: Vec<ScenarioAccessibilityExecution>,
    #[serde(default)]
    pub od: Vec<ScenarioOdExecution>,
    #[serde(default)]
    pub matrices: Vec<ScenarioMatrixExecution>,
    #[serde(default)]
    pub betweenness: Vec<ScenarioBetweennessExecution>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScenarioBatchDiff {
    /// Baseline-reachable routes that became unreachable.
    pub disconnected_route_count: usize,
    /// Baseline accessibility rows that became unreachable.
    pub disconnected_accessibility_row_count: usize,
    /// Baseline-reachable OD pairs that became unreachable.
    pub disconnected_od_pair_count: usize,
    /// Baseline-reachable matrix cells that became unreachable.
    pub disconnected_matrix_cell_count: usize,
    /// Baseline-routed weighted-betweenness OD pairs that became unreachable.
    pub disconnected_betweenness_pair_count: usize,
    /// Route requests plus accessibility rows that became disconnected.
    pub disconnected_demand_count: usize,
    /// Sum of signed travel-time changes across routes reachable in both runs.
    pub route_time_change_s: f64,
    /// Sum of positive route travel-time changes.
    pub rerouting_burden_s: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_rerouting_burden_s: Option<f64>,
    /// Signed travel-time change across comparable OD pairs.
    pub od_route_time_change_s: f64,
    /// Positive travel-time change across comparable OD pairs.
    pub od_rerouting_burden_s: f64,
    /// Signed travel-time change across comparable matrix cells.
    pub matrix_route_time_change_s: f64,
    /// Positive travel-time change across comparable matrix cells.
    pub matrix_rerouting_burden_s: f64,
    /// Demand from baseline-routed weighted-betweenness OD pairs that became
    /// unreachable.
    pub weighted_disconnected_demand: f64,
    /// L1 change in per-edge weighted usage scores.
    pub betweenness_edge_score_absolute_change: f64,
    /// Loss in reachable network length, comparing the largest threshold per
    /// service-area origin.
    pub service_area_reachable_network_loss_m: f64,
    /// Loss in destination counts at the largest accessibility threshold.
    pub accessibility_reachable_destination_loss: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioBatchCaseResult {
    pub scenario_id: String,
    #[serde(flatten)]
    pub selector: ScenarioBatchSelector,
    /// Generated selectors expose the concrete closed feature set for audit
    /// and reproducibility. Explicit overlays leave this empty because they
    /// may contain non-closure temporal rules as well.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed_source_feature_ids: Vec<i64>,
    pub analyses: ScenarioAnalysisSnapshot,
    pub diff: ScenarioBatchDiff,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioBatchResult {
    pub batch_id: String,
    pub baseline: ScenarioAnalysisSnapshot,
    pub scenarios: Vec<ScenarioBatchCaseResult>,
}

pub fn load_scenario_batch_request(path: impl AsRef<Path>) -> Result<ScenarioBatchRequest> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading scenario batch {}", path.display()))?;
    let mut request = match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => serde_json::from_str(&raw).context("parsing JSON scenario batch")?,
        Some("yaml") | Some("yml") => {
            serde_yaml::from_str(&raw).context("parsing YAML scenario batch")?
        }
        other => bail!(
            "unsupported scenario-batch extension {:?}; use .json, .yml, or .yaml",
            other
        ),
    };
    resolve_document_paths(
        &mut request,
        path.parent().unwrap_or_else(|| Path::new(".")),
    );
    validate_scenario_batch_request(&request)?;
    Ok(request)
}

pub fn execute_scenario_batch(
    engine: &PreparedRoutingEngine,
    request: &ScenarioBatchRequest,
) -> Result<ScenarioBatchResult> {
    validate_scenario_batch_request(request)?;
    // Resolve and validate every selector before spending time on the
    // baseline. This also prevents a stale ranking from yielding a partial
    // batch after otherwise-successful analyses have already run.
    let resolved_scenarios = request
        .scenarios
        .iter()
        .map(|scenario| resolve_scenario_overlay(engine, scenario))
        .collect::<Result<Vec<_>>>()?;
    let baseline = execute_snapshot(engine, request, None);
    let scenarios = request
        .scenarios
        .iter()
        .zip(&resolved_scenarios)
        .map(|(scenario, resolved)| {
            let analyses = execute_snapshot(engine, request, Some(&resolved.path));
            let diff = diff_snapshots(&baseline, &analyses);
            Ok(ScenarioBatchCaseResult {
                scenario_id: scenario.id.clone(),
                selector: scenario.selector.clone(),
                closed_source_feature_ids: resolved.closed_source_feature_ids.clone(),
                analyses,
                diff,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ScenarioBatchResult {
        batch_id: request.batch_id.clone(),
        baseline,
        scenarios,
    })
}

struct ResolvedScenarioOverlay {
    path: PathBuf,
    closed_source_feature_ids: Vec<i64>,
    remove_on_drop: bool,
}

impl Drop for ResolvedScenarioOverlay {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn resolve_scenario_overlay(
    engine: &PreparedRoutingEngine,
    scenario: &ScenarioBatchCase,
) -> Result<ResolvedScenarioOverlay> {
    match &scenario.selector {
        ScenarioBatchSelector::Overlay { overlay } => {
            let document = crate::load_scenario_overlay(overlay).with_context(|| {
                format!(
                    "validating scenario '{}' overlay {}",
                    scenario.id,
                    overlay.display()
                )
            })?;
            validate_overlay_source_features(engine, scenario, overlay, &document)?;
            Ok(ResolvedScenarioOverlay {
                path: overlay.clone(),
                closed_source_feature_ids: Vec::new(),
                remove_on_drop: false,
            })
        }
        ScenarioBatchSelector::TopK { top_k } => {
            if top_k.count == 0 {
                bail!(
                    "scenario '{}' top_k count must be greater than zero",
                    scenario.id
                );
            }
            let closed_source_feature_ids =
                load_ranked_source_features(&top_k.ranking, top_k.count)?;
            if closed_source_feature_ids.is_empty() {
                bail!(
                    "scenario '{}' top_k selector produced no source features",
                    scenario.id
                );
            }
            let topology = engine.topology();
            let active_source_feature_ids = (0..topology.edge_count())
                .map(|edge_index| topology.routing_edge(edge_index).source_way_id)
                .collect::<BTreeSet<_>>();
            let stale_source_feature_ids = closed_source_feature_ids
                .iter()
                .filter(|source_feature_id| !active_source_feature_ids.contains(source_feature_id))
                .copied()
                .collect::<Vec<_>>();
            if !stale_source_feature_ids.is_empty() {
                bail!(
                    "scenario '{}' top_k ranking contains source feature ids not present in the active topology: {}",
                    scenario.id,
                    stale_source_feature_ids
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            generated_closure_overlay(&scenario.id, closed_source_feature_ids)
        }
        ScenarioBatchSelector::Group { group } => {
            if group.attribute.trim().is_empty() || group.value.trim().is_empty() {
                bail!(
                    "scenario '{}' group attribute and value must not be empty",
                    scenario.id
                );
            }
            let topology = engine.topology();
            let closed_source_feature_ids = (0..topology.edge_count())
                .map(|edge_index| topology.routing_edge(edge_index))
                .filter(|edge| {
                    edge.feature_row != netweevil_core::NO_FEATURE_ROW
                        && topology.feature_attributes.value_matches(
                            edge.feature_row,
                            &group.attribute,
                            &group.value,
                        )
                })
                .map(|edge| edge.source_way_id)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            if closed_source_feature_ids.is_empty() {
                bail!(
                    "scenario '{}' group selector {}={} matched no retained source features",
                    scenario.id,
                    group.attribute,
                    group.value
                );
            }
            generated_closure_overlay(&scenario.id, closed_source_feature_ids)
        }
    }
}

fn validate_overlay_source_features(
    engine: &PreparedRoutingEngine,
    scenario: &ScenarioBatchCase,
    overlay_path: &Path,
    overlay: &crate::ScenarioOverlay,
) -> Result<()> {
    let topology = engine.topology();
    let active_source_feature_ids = (0..topology.edge_count())
        .map(|edge_index| topology.routing_edge(edge_index).source_way_id)
        .collect::<BTreeSet<_>>();
    let stale_source_feature_ids = overlay
        .features
        .iter()
        .map(|feature| feature.source_feature_id)
        .filter(|source_feature_id| !active_source_feature_ids.contains(source_feature_id))
        .collect::<BTreeSet<_>>();
    if !stale_source_feature_ids.is_empty() {
        bail!(
            "scenario '{}' overlay '{}' references source feature ids not present in the active topology: {}",
            scenario.id,
            overlay_path.display(),
            stale_source_feature_ids
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

fn generated_closure_overlay(
    scenario_id: &str,
    closed_source_feature_ids: Vec<i64>,
) -> Result<ResolvedScenarioOverlay> {
    static NEXT_OVERLAY_ID: AtomicU64 = AtomicU64::new(0);
    let overlay = crate::ScenarioOverlay {
        id: Some(scenario_id.to_string()),
        features: closed_source_feature_ids
            .iter()
            .map(|source_feature_id| crate::ScenarioFeatureOverride {
                source_feature_id: *source_feature_id,
                force_closed: true,
                force_open: false,
                replace_rules: false,
                rules: Vec::new(),
                speed_factor: None,
            })
            .collect(),
    };
    let temp_dir = std::env::temp_dir();
    for _ in 0..32 {
        let sequence = NEXT_OVERLAY_ID.fetch_add(1, Ordering::Relaxed);
        let path = temp_dir.join(format!(
            "netweevil-scenario-{}-{sequence}.json",
            std::process::id()
        ));
        let file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("creating generated scenario overlay {}", path.display())
                });
            }
        };
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, &overlay)
            .with_context(|| format!("writing generated scenario overlay {}", path.display()))?;
        writer
            .flush()
            .with_context(|| format!("flushing generated scenario overlay {}", path.display()))?;
        return Ok(ResolvedScenarioOverlay {
            path,
            closed_source_feature_ids,
            remove_on_drop: true,
        });
    }
    bail!("could not allocate a unique generated scenario overlay path")
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ScenarioFeatureRankingFile {
    Document {
        #[serde(alias = "scores", alias = "ranking")]
        features: Vec<ScenarioFeatureScore>,
    },
    Bare(Vec<ScenarioFeatureScore>),
}

fn load_ranked_source_features(path: &Path, count: usize) -> Result<Vec<i64>> {
    let scores = match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("csv") => {
            let mut reader = csv::Reader::from_path(path)
                .with_context(|| format!("opening scenario feature ranking {}", path.display()))?;
            reader
                .deserialize::<ScenarioFeatureScore>()
                .collect::<std::result::Result<Vec<_>, _>>()
                .with_context(|| format!("parsing scenario feature ranking {}", path.display()))?
        }
        Some(extension)
            if extension.eq_ignore_ascii_case("json")
                || extension.eq_ignore_ascii_case("yml")
                || extension.eq_ignore_ascii_case("yaml") =>
        {
            let raw = fs::read_to_string(path)
                .with_context(|| format!("reading scenario feature ranking {}", path.display()))?;
            if extension.eq_ignore_ascii_case("json") {
                if let Ok(result) = serde_json::from_str::<BetweennessResult>(&raw) {
                    result
                        .edges
                        .into_iter()
                        .map(|edge| ScenarioFeatureScore {
                            source_feature_id: edge.source_way_id,
                            score: edge.score,
                        })
                        .collect()
                } else {
                    match serde_json::from_str::<ScenarioFeatureRankingFile>(&raw).with_context(
                        || format!("parsing scenario feature ranking {}", path.display()),
                    )? {
                        ScenarioFeatureRankingFile::Document { features }
                        | ScenarioFeatureRankingFile::Bare(features) => features,
                    }
                }
            } else {
                if let Ok(result) = serde_yaml::from_str::<BetweennessResult>(&raw) {
                    result
                        .edges
                        .into_iter()
                        .map(|edge| ScenarioFeatureScore {
                            source_feature_id: edge.source_way_id,
                            score: edge.score,
                        })
                        .collect()
                } else {
                    match serde_yaml::from_str::<ScenarioFeatureRankingFile>(&raw).with_context(
                        || format!("parsing scenario feature ranking {}", path.display()),
                    )? {
                        ScenarioFeatureRankingFile::Document { features }
                        | ScenarioFeatureRankingFile::Bare(features) => features,
                    }
                }
            }
        }
        other => bail!(
            "unsupported scenario feature ranking extension {:?}; use .csv, .json, .yml, or .yaml",
            other
        ),
    };
    let mut aggregated = BTreeMap::<i64, f64>::new();
    for score in scores {
        if !score.score.is_finite() {
            bail!(
                "scenario feature {} has a non-finite ranking score",
                score.source_feature_id
            );
        }
        *aggregated.entry(score.source_feature_id).or_default() += score.score;
    }
    let mut ranked = aggregated.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    Ok(ranked
        .into_iter()
        .take(count)
        .map(|(source_feature_id, _)| source_feature_id)
        .collect())
}

fn validate_scenario_batch_request(request: &ScenarioBatchRequest) -> Result<()> {
    if request.batch_id.trim().is_empty() {
        bail!("scenario batch must have a non-empty batch_id");
    }
    if request.scenarios.is_empty() {
        bail!("scenario batch must contain at least one scenario");
    }
    if request.routes.is_empty()
        && request.service_areas.is_empty()
        && request.accessibility.is_empty()
        && request.od.is_empty()
        && request.matrices.is_empty()
        && request.betweenness.is_empty()
    {
        bail!("scenario batch must contain at least one analysis request");
    }
    let mut ids = HashSet::new();
    for scenario in &request.scenarios {
        if scenario.id.trim().is_empty() {
            bail!("scenario ids must not be empty");
        }
        if !ids.insert(scenario.id.as_str()) {
            bail!("duplicate scenario id '{}'", scenario.id);
        }
    }
    validate_unique_ids(
        "route",
        request.routes.iter().map(|value| value.route_id.as_str()),
    )?;
    validate_unique_ids(
        "service-area analysis",
        request
            .service_areas
            .iter()
            .map(|value| value.analysis_id.as_str()),
    )?;
    validate_unique_ids(
        "OD analysis",
        request.od.iter().map(|value| value.analysis_id.as_str()),
    )?;
    validate_unique_ids(
        "matrix analysis",
        request
            .matrices
            .iter()
            .map(|value| value.analysis_id.as_str()),
    )?;
    validate_unique_ids(
        "betweenness analysis",
        request
            .betweenness
            .iter()
            .map(|value| value.analysis_id.as_str()),
    )?;
    Ok(())
}

fn validate_unique_ids<'a>(label: &str, ids: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut seen = HashSet::new();
    for id in ids {
        if id.trim().is_empty() {
            bail!("{label} ids must not be empty");
        }
        if !seen.insert(id) {
            bail!("duplicate {label} id '{id}'");
        }
    }
    Ok(())
}

fn execute_snapshot(
    engine: &PreparedRoutingEngine,
    batch: &ScenarioBatchRequest,
    scenario: Option<&Path>,
) -> ScenarioAnalysisSnapshot {
    let routes = batch
        .routes
        .iter()
        .map(|request| {
            let mut request = request.clone();
            configure_temporal(
                &mut request.temporal,
                batch.departure_time.as_deref(),
                scenario,
            );
            match engine.execute_route(&request) {
                Ok(result) => ScenarioRouteExecution {
                    route_id: request.route_id,
                    result: Some(result),
                    error: None,
                },
                Err(error) => ScenarioRouteExecution {
                    route_id: request.route_id,
                    result: None,
                    error: Some(error.to_string()),
                },
            }
        })
        .collect();
    let service_areas = batch
        .service_areas
        .iter()
        .map(|request| {
            let mut request = request.clone();
            configure_temporal(
                &mut request.temporal,
                batch.departure_time.as_deref(),
                scenario,
            );
            match engine.execute_service_area(&request) {
                Ok(result) => ScenarioServiceAreaExecution {
                    analysis_id: request.analysis_id,
                    result: Some(result),
                    error: None,
                },
                Err(error) => ScenarioServiceAreaExecution {
                    analysis_id: request.analysis_id,
                    result: None,
                    error: Some(error.to_string()),
                },
            }
        })
        .collect();
    let accessibility = batch
        .accessibility
        .iter()
        .enumerate()
        .map(|(index, request)| {
            let analysis_id = format!("accessibility_{}", index + 1);
            match execute_temporal_accessibility(
                engine,
                request,
                batch.departure_time.as_deref(),
                scenario,
            ) {
                Ok(result) => ScenarioAccessibilityExecution {
                    analysis_id,
                    result: Some(result),
                    error: None,
                },
                Err(error) => ScenarioAccessibilityExecution {
                    analysis_id,
                    result: None,
                    error: Some(error.to_string()),
                },
            }
        })
        .collect();
    let od = batch
        .od
        .iter()
        .map(|analysis| {
            let mut request = analysis.request.clone();
            configure_temporal(
                &mut request.temporal,
                batch.departure_time.as_deref(),
                scenario,
            );
            match engine.execute_od(&request) {
                Ok(result) => ScenarioOdExecution {
                    analysis_id: analysis.analysis_id.clone(),
                    result: Some(result),
                    error: None,
                },
                Err(error) => ScenarioOdExecution {
                    analysis_id: analysis.analysis_id.clone(),
                    result: None,
                    error: Some(error.to_string()),
                },
            }
        })
        .collect();
    let matrices = batch
        .matrices
        .iter()
        .map(|analysis| {
            let mut origins = analysis.origins.clone();
            let mut destinations = analysis.destinations.clone();
            configure_matrix_temporal(
                &mut origins,
                &mut destinations,
                batch.departure_time.as_deref(),
                scenario,
            );
            match engine.execute_matrix(&origins, &destinations) {
                Ok(result) => ScenarioMatrixExecution {
                    analysis_id: analysis.analysis_id.clone(),
                    result: Some(result),
                    error: None,
                },
                Err(error) => ScenarioMatrixExecution {
                    analysis_id: analysis.analysis_id.clone(),
                    result: None,
                    error: Some(error.to_string()),
                },
            }
        })
        .collect();
    let betweenness = batch
        .betweenness
        .iter()
        .map(|analysis| {
            let mut request = analysis.clone();
            configure_temporal(
                &mut request.temporal,
                batch.departure_time.as_deref(),
                scenario,
            );
            match engine.execute_betweenness_with_pair_results(&request) {
                Ok(result) => ScenarioBetweennessExecution {
                    analysis_id: request.analysis_id,
                    result: Some(result),
                    error: None,
                },
                Err(error) => ScenarioBetweennessExecution {
                    analysis_id: request.analysis_id,
                    result: None,
                    error: Some(error.to_string()),
                },
            }
        })
        .collect();
    ScenarioAnalysisSnapshot {
        routes,
        service_areas,
        accessibility,
        od,
        matrices,
        betweenness,
    }
}

fn configure_temporal(
    temporal: &mut TemporalRequestOptions,
    default_departure: Option<&str>,
    scenario: Option<&Path>,
) {
    temporal.scenario = scenario.map(Path::to_path_buf);
    if temporal.departure_time.is_none() && temporal.arrive_by.is_none() {
        temporal.departure_time = default_departure.map(str::to_string);
    }
}

fn configure_matrix_temporal(
    origins: &mut PointSetDocument,
    destinations: &mut PointSetDocument,
    default_departure: Option<&str>,
    scenario: Option<&Path>,
) {
    match (
        origins.temporal.requires_exact_labels(),
        destinations.temporal.requires_exact_labels(),
    ) {
        (true, true) => {
            configure_temporal(&mut origins.temporal, default_departure, scenario);
            configure_temporal(&mut destinations.temporal, default_departure, scenario);
        }
        (false, true) => {
            configure_temporal(&mut destinations.temporal, default_departure, scenario);
        }
        _ => configure_temporal(&mut origins.temporal, default_departure, scenario),
    }
}

fn execute_temporal_accessibility(
    engine: &PreparedRoutingEngine,
    request: &AccessibilityRequest,
    default_departure: Option<&str>,
    scenario: Option<&Path>,
) -> Result<AccessibilityResult> {
    let mut request = request.clone();
    configure_temporal(&mut request.origins.temporal, default_departure, scenario);
    if scenario.is_some()
        && request.origins.temporal.departure_time.is_none()
        && request.origins.temporal.arrive_by.is_none()
    {
        bail!(
            "scenario accessibility requires departure_time/arrive_by on origins or batch departure_time"
        );
    }
    engine.execute_accessibility(&request)
}

fn diff_snapshots(
    baseline: &ScenarioAnalysisSnapshot,
    scenario: &ScenarioAnalysisSnapshot,
) -> ScenarioBatchDiff {
    let mut diff = ScenarioBatchDiff::default();
    let scenario_routes = scenario
        .routes
        .iter()
        .map(|route| (route.route_id.as_str(), route))
        .collect::<HashMap<_, _>>();
    let mut comparable_routes = 0_usize;
    for base in &baseline.routes {
        let Some(base_result) = base
            .result
            .as_ref()
            .filter(|result| route_reachable(result))
        else {
            continue;
        };
        let scenario_result = scenario_routes
            .get(base.route_id.as_str())
            .and_then(|execution| execution.result.as_ref())
            .filter(|result| route_reachable(result));
        let Some(scenario_result) = scenario_result else {
            diff.disconnected_route_count += 1;
            continue;
        };
        comparable_routes += 1;
        let change =
            scenario_result.summary.total_travel_time_s - base_result.summary.total_travel_time_s;
        diff.route_time_change_s += change;
        diff.rerouting_burden_s += change.max(0.0);
    }
    diff.mean_rerouting_burden_s =
        (comparable_routes > 0).then_some(diff.rerouting_burden_s / comparable_routes as f64);

    let scenario_service_areas = scenario
        .service_areas
        .iter()
        .map(|result| (result.analysis_id.as_str(), result))
        .collect::<HashMap<_, _>>();
    for base in &baseline.service_areas {
        let Some(base_result) = base.result.as_ref() else {
            continue;
        };
        let scenario_length = scenario_service_areas
            .get(base.analysis_id.as_str())
            .and_then(|execution| execution.result.as_ref())
            .map(max_service_area_network_length)
            .unwrap_or_default();
        diff.service_area_reachable_network_loss_m +=
            (max_service_area_network_length(base_result) - scenario_length).max(0.0);
    }

    let scenario_accessibility = scenario
        .accessibility
        .iter()
        .map(|result| (result.analysis_id.as_str(), result))
        .collect::<HashMap<_, _>>();
    for base in &baseline.accessibility {
        let Some(base_result) = base.result.as_ref() else {
            continue;
        };
        let scenario_result = scenario_accessibility
            .get(base.analysis_id.as_str())
            .and_then(|execution| execution.result.as_ref());
        let scenario_rows = scenario_result
            .map(|result| {
                result
                    .rows
                    .iter()
                    .map(|row| ((row.origin_id.as_str(), row.category_id.as_str()), row))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        for base_row in &base_result.rows {
            let scenario_row = scenario_rows
                .get(&(base_row.origin_id.as_str(), base_row.category_id.as_str()))
                .copied();
            if base_row.status == BatchItemStatus::Succeeded
                && scenario_row.is_none_or(|row| row.status != BatchItemStatus::Succeeded)
            {
                diff.disconnected_accessibility_row_count += 1;
            }
            let base_count = largest_threshold_count(base_row);
            let scenario_count = scenario_row
                .map(largest_threshold_count)
                .unwrap_or_default();
            diff.accessibility_reachable_destination_loss +=
                base_count.saturating_sub(scenario_count);
        }
    }
    diff_od_results(baseline, scenario, &mut diff);
    diff_matrix_results(baseline, scenario, &mut diff);
    diff_betweenness_results(baseline, scenario, &mut diff);
    diff.disconnected_demand_count = diff.disconnected_route_count
        + diff.disconnected_accessibility_row_count
        + diff.disconnected_od_pair_count
        + diff.disconnected_matrix_cell_count
        + diff.disconnected_betweenness_pair_count;
    diff
}

fn diff_od_results(
    baseline: &ScenarioAnalysisSnapshot,
    scenario: &ScenarioAnalysisSnapshot,
    diff: &mut ScenarioBatchDiff,
) {
    let scenario_analyses = scenario
        .od
        .iter()
        .map(|value| (value.analysis_id.as_str(), value))
        .collect::<HashMap<_, _>>();
    for baseline_analysis in &baseline.od {
        let Some(baseline_result) = baseline_analysis.result.as_ref() else {
            continue;
        };
        let scenario_pairs = scenario_analyses
            .get(baseline_analysis.analysis_id.as_str())
            .and_then(|value| value.result.as_ref())
            .map(|result| {
                result
                    .pairs
                    .iter()
                    .map(|pair| (pair.pair_id.as_str(), pair))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        for baseline_pair in &baseline_result.pairs {
            let Some(baseline_time) =
                reachable_batch_time(baseline_pair.status, baseline_pair.total_travel_time_s)
            else {
                continue;
            };
            let scenario_time = scenario_pairs
                .get(baseline_pair.pair_id.as_str())
                .and_then(|pair| reachable_batch_time(pair.status, pair.total_travel_time_s));
            let Some(scenario_time) = scenario_time else {
                diff.disconnected_od_pair_count += 1;
                continue;
            };
            let change = scenario_time - baseline_time;
            diff.od_route_time_change_s += change;
            diff.od_rerouting_burden_s += change.max(0.0);
        }
    }
}

fn diff_matrix_results(
    baseline: &ScenarioAnalysisSnapshot,
    scenario: &ScenarioAnalysisSnapshot,
    diff: &mut ScenarioBatchDiff,
) {
    let scenario_analyses = scenario
        .matrices
        .iter()
        .map(|value| (value.analysis_id.as_str(), value))
        .collect::<HashMap<_, _>>();
    for baseline_analysis in &baseline.matrices {
        let Some(baseline_result) = baseline_analysis.result.as_ref() else {
            continue;
        };
        let scenario_cells = scenario_analyses
            .get(baseline_analysis.analysis_id.as_str())
            .and_then(|value| value.result.as_ref())
            .map(|result| {
                result
                    .cells
                    .iter()
                    .map(|cell| {
                        (
                            (cell.origin_id.as_str(), cell.destination_id.as_str()),
                            cell,
                        )
                    })
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        for baseline_cell in &baseline_result.cells {
            let Some(baseline_time) =
                reachable_batch_time(baseline_cell.status, baseline_cell.total_travel_time_s)
            else {
                continue;
            };
            let scenario_time = scenario_cells
                .get(&(
                    baseline_cell.origin_id.as_str(),
                    baseline_cell.destination_id.as_str(),
                ))
                .and_then(|cell| reachable_batch_time(cell.status, cell.total_travel_time_s));
            let Some(scenario_time) = scenario_time else {
                diff.disconnected_matrix_cell_count += 1;
                continue;
            };
            let change = scenario_time - baseline_time;
            diff.matrix_route_time_change_s += change;
            diff.matrix_rerouting_burden_s += change.max(0.0);
        }
    }
}

fn diff_betweenness_results(
    baseline: &ScenarioAnalysisSnapshot,
    scenario: &ScenarioAnalysisSnapshot,
    diff: &mut ScenarioBatchDiff,
) {
    let scenario_analyses = scenario
        .betweenness
        .iter()
        .map(|value| (value.analysis_id.as_str(), value))
        .collect::<HashMap<_, _>>();
    for baseline_analysis in &baseline.betweenness {
        let Some(baseline_result) = baseline_analysis.result.as_ref() else {
            continue;
        };
        let scenario_result = scenario_analyses
            .get(baseline_analysis.analysis_id.as_str())
            .and_then(|value| value.result.as_ref());
        if baseline_result.pairs.is_empty() {
            // Compatibility for results serialized before pair outcomes were
            // added. New executions use the exact baseline-pair comparison.
            if let Some(scenario_result) = scenario_result {
                diff.disconnected_betweenness_pair_count += baseline_result
                    .routed_pair_count
                    .saturating_sub(scenario_result.routed_pair_count);
                diff.weighted_disconnected_demand +=
                    (baseline_result.routed_demand - scenario_result.routed_demand).max(0.0);
            } else {
                diff.disconnected_betweenness_pair_count += baseline_result.routed_pair_count;
                diff.weighted_disconnected_demand += baseline_result.routed_demand;
            }
        } else {
            let scenario_pairs = scenario_result
                .map(|result| {
                    result
                        .pairs
                        .iter()
                        .map(|pair| ((pair.origin_index, pair.destination_index), pair.routed))
                        .collect::<HashMap<_, _>>()
                })
                .unwrap_or_default();
            for baseline_pair in baseline_result.pairs.iter().filter(|pair| pair.routed) {
                if !scenario_pairs
                    .get(&(baseline_pair.origin_index, baseline_pair.destination_index))
                    .copied()
                    .unwrap_or(false)
                {
                    diff.disconnected_betweenness_pair_count += 1;
                    diff.weighted_disconnected_demand += baseline_pair.demand;
                }
            }
        }
        let Some(scenario_result) = scenario_result else {
            diff.betweenness_edge_score_absolute_change += baseline_result
                .edges
                .iter()
                .map(|edge| edge.score.abs())
                .sum::<f64>();
            continue;
        };
        let mut scenario_scores = scenario_result
            .edges
            .iter()
            .map(|edge| (edge.edge_id, edge.score))
            .collect::<HashMap<_, _>>();
        for baseline_edge in &baseline_result.edges {
            let scenario_score = scenario_scores
                .remove(&baseline_edge.edge_id)
                .unwrap_or_default();
            diff.betweenness_edge_score_absolute_change +=
                (baseline_edge.score - scenario_score).abs();
        }
        diff.betweenness_edge_score_absolute_change +=
            scenario_scores.into_values().map(f64::abs).sum::<f64>();
    }
}

fn reachable_batch_time(status: BatchItemStatus, travel_time_s: Option<f64>) -> Option<f64> {
    (status == BatchItemStatus::Succeeded)
        .then_some(travel_time_s)
        .flatten()
}

fn route_reachable(result: &RouteResult) -> bool {
    !matches!(
        result.outcome,
        AnalysisOutcome::Unreachable | AnalysisOutcome::NotImplemented
    )
}

fn max_service_area_network_length(result: &ServiceAreaResult) -> f64 {
    let mut per_origin = HashMap::<Option<&str>, (f64, f64)>::new();
    for summary in &result.summaries {
        let entry = per_origin
            .entry(summary.origin_id.as_deref())
            .or_insert((f64::NEG_INFINITY, 0.0));
        if summary.threshold_limit > entry.0 {
            *entry = (
                summary.threshold_limit,
                summary.reachable_network_length_m.unwrap_or_default(),
            );
        }
    }
    if per_origin.is_empty() {
        return result
            .features
            .iter()
            .filter_map(|feature| feature.reachable_network_length_m)
            .sum();
    }
    per_origin.values().map(|(_, length)| *length).sum()
}

fn largest_threshold_count(row: &AccessibilityCategoryResult) -> usize {
    row.counts_within_threshold_s
        .iter()
        .filter_map(|(threshold, count)| threshold.parse::<f64>().ok().map(|value| (value, count)))
        .max_by(|left, right| left.0.total_cmp(&right.0))
        .map(|(_, count)| *count)
        .unwrap_or_default()
}

#[cfg(test)]
fn threshold_key(threshold_s: f64) -> String {
    if threshold_s.fract().abs() <= f64::EPSILON {
        format!("{threshold_s:.0}")
    } else {
        threshold_s.to_string()
    }
}

fn resolve_document_paths(request: &mut ScenarioBatchRequest, base: &Path) {
    for scenario in &mut request.scenarios {
        match &mut scenario.selector {
            ScenarioBatchSelector::Overlay { overlay } => resolve_path(overlay, base),
            ScenarioBatchSelector::TopK { top_k } => resolve_path(&mut top_k.ranking, base),
            ScenarioBatchSelector::Group { .. } => {}
        }
    }
    for route in &mut request.routes {
        resolve_temporal_paths(&mut route.temporal, base);
    }
    for service_area in &mut request.service_areas {
        resolve_temporal_paths(&mut service_area.temporal, base);
    }
    for accessibility in &mut request.accessibility {
        resolve_temporal_paths(&mut accessibility.origins.temporal, base);
        for category in &mut accessibility.categories {
            resolve_temporal_paths(&mut category.destinations.temporal, base);
        }
    }
    for od in &mut request.od {
        resolve_temporal_paths(&mut od.request.temporal, base);
    }
    for matrix in &mut request.matrices {
        resolve_temporal_paths(&mut matrix.origins.temporal, base);
        resolve_temporal_paths(&mut matrix.destinations.temporal, base);
    }
    for betweenness in &mut request.betweenness {
        resolve_temporal_paths(&mut betweenness.temporal, base);
    }
}

fn resolve_temporal_paths(temporal: &mut TemporalRequestOptions, base: &Path) {
    if let Some(path) = &mut temporal.scenario {
        resolve_path(path, base);
    }
    if let Some(path) = &mut temporal.holiday_calendar {
        resolve_path(path, base);
    }
    for path in &mut temporal.overlays {
        resolve_path(path, base);
    }
}

fn resolve_path(path: &mut PathBuf, base: &Path) {
    if path.is_relative() {
        *path = base.join(&*path);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        ScenarioAnalysisSnapshot, ScenarioBatchDiff, ScenarioBatchSelector, diff_snapshots,
        load_ranked_source_features, load_scenario_batch_request, threshold_key,
    };
    use crate::{
        AccessibilityCategoryResult, AccessibilityResult, AnalysisOutcome, BatchItemStatus,
        BetweennessResult, MatrixResult, OdResult, RouteResult, ScenarioAccessibilityExecution,
        ScenarioBetweennessExecution, ScenarioMatrixExecution, ScenarioOdExecution,
        ScenarioRouteExecution,
    };

    #[test]
    fn computes_disconnection_and_accessibility_loss() {
        let baseline = ScenarioAnalysisSnapshot {
            routes: vec![ScenarioRouteExecution {
                route_id: "r1".to_string(),
                result: Some(route_result(10.0)),
                error: None,
            }],
            accessibility: vec![ScenarioAccessibilityExecution {
                analysis_id: "accessibility_1".to_string(),
                result: Some(accessibility_result(BatchItemStatus::Succeeded, 3)),
                error: None,
            }],
            ..Default::default()
        };
        let scenario = ScenarioAnalysisSnapshot {
            routes: vec![ScenarioRouteExecution {
                route_id: "r1".to_string(),
                result: None,
                error: Some("closed".to_string()),
            }],
            accessibility: vec![ScenarioAccessibilityExecution {
                analysis_id: "accessibility_1".to_string(),
                result: Some(accessibility_result(BatchItemStatus::Failed, 0)),
                error: None,
            }],
            ..Default::default()
        };
        let diff = diff_snapshots(&baseline, &scenario);
        assert_eq!(diff.disconnected_route_count, 1);
        assert_eq!(diff.disconnected_accessibility_row_count, 1);
        assert_eq!(diff.disconnected_demand_count, 2);
        assert_eq!(diff.accessibility_reachable_destination_loss, 3);
    }

    #[test]
    fn computes_od_matrix_and_weighted_betweenness_loss() {
        let baseline = ScenarioAnalysisSnapshot {
            od: vec![ScenarioOdExecution {
                analysis_id: "od".to_string(),
                result: Some(od_result(BatchItemStatus::Succeeded, Some(10.0))),
                error: None,
            }],
            matrices: vec![ScenarioMatrixExecution {
                analysis_id: "matrix".to_string(),
                result: Some(matrix_result(BatchItemStatus::Succeeded, Some(20.0))),
                error: None,
            }],
            betweenness: vec![ScenarioBetweennessExecution {
                analysis_id: "usage".to_string(),
                result: Some(betweenness_result(1, 0, 6.0, vec![(0, 10, 6.0)])),
                error: None,
            }],
            ..Default::default()
        };
        let scenario = ScenarioAnalysisSnapshot {
            od: vec![ScenarioOdExecution {
                analysis_id: "od".to_string(),
                result: None,
                error: Some("closed".to_string()),
            }],
            matrices: vec![ScenarioMatrixExecution {
                analysis_id: "matrix".to_string(),
                result: Some(matrix_result(BatchItemStatus::Failed, None)),
                error: None,
            }],
            betweenness: vec![ScenarioBetweennessExecution {
                analysis_id: "usage".to_string(),
                result: Some(betweenness_result(0, 1, 0.0, Vec::new())),
                error: None,
            }],
            ..Default::default()
        };

        let diff = diff_snapshots(&baseline, &scenario);
        assert_eq!(diff.disconnected_od_pair_count, 1);
        assert_eq!(diff.disconnected_matrix_cell_count, 1);
        assert_eq!(diff.disconnected_betweenness_pair_count, 1);
        assert_eq!(diff.disconnected_demand_count, 3);
        assert_eq!(diff.weighted_disconnected_demand, 6.0);
        assert_eq!(diff.betweenness_edge_score_absolute_change, 6.0);
    }

    #[test]
    fn loader_resolves_relative_scenario_and_temporal_paths() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("netweevil-scenario-batch-{unique}"));
        fs::create_dir_all(&directory).expect("temporary scenario directory");
        let request_path = directory.join("batch.yml");
        fs::write(
            &request_path,
            r#"batch_id: relative_paths
scenarios:
  - id: closure
    overlay: scenarios/closure.yml
  - id: top_usage
    top_k: {ranking: rankings/usage.json, count: 2}
  - id: mall
    group: {attribute: asset_group, value: mall}
routes:
  - route_id: example
    origin: {id: a, lon: 1.0, lat: 2.0}
    destination: {id: b, lon: 1.1, lat: 2.1}
    holiday_calendar: calendars/holidays.yml
    overlay: [weather/shade.csv]
od:
  - analysis_id: od
    request:
      pairs:
        - pair_id: a_b
          origin: {id: a, lon: 1.0, lat: 2.0}
          destination: {id: b, lon: 1.1, lat: 2.1}
      holiday_calendar: calendars/od.yml
matrices:
  - analysis_id: matrix
    origins:
      points: [{id: a, lon: 1.0, lat: 2.0}]
      overlay: [weather/matrix.csv]
    destinations:
      points: [{id: b, lon: 1.1, lat: 2.1}]
betweenness:
  - analysis_id: usage
    origins: [{id: a, lon: 1.0, lat: 2.0, weight: 2.0}]
    destinations: [{id: b, lon: 1.1, lat: 2.1, weight: 3.0}]
    holiday_calendar: calendars/usage.yml
"#,
        )
        .expect("scenario request fixture");

        let request = load_scenario_batch_request(&request_path).expect("loaded scenario batch");
        assert_eq!(
            request.scenarios[0].selector,
            ScenarioBatchSelector::Overlay {
                overlay: directory.join("scenarios/closure.yml")
            }
        );
        assert_eq!(
            request.scenarios[1].selector,
            ScenarioBatchSelector::TopK {
                top_k: super::ScenarioTopKSelector {
                    ranking: directory.join("rankings/usage.json"),
                    count: 2,
                }
            }
        );
        assert_eq!(
            request.scenarios[2].selector,
            ScenarioBatchSelector::Group {
                group: super::ScenarioGroupSelector {
                    attribute: "asset_group".to_string(),
                    value: "mall".to_string(),
                }
            }
        );
        assert_eq!(
            request.routes[0].temporal.holiday_calendar,
            Some(directory.join("calendars/holidays.yml"))
        );
        assert_eq!(
            request.routes[0].temporal.overlays,
            vec![directory.join("weather/shade.csv")]
        );
        assert_eq!(
            request.od[0].request.temporal.holiday_calendar,
            Some(directory.join("calendars/od.yml"))
        );
        assert_eq!(
            request.matrices[0].origins.temporal.overlays,
            vec![directory.join("weather/matrix.csv")]
        );
        assert_eq!(
            request.betweenness[0].temporal.holiday_calendar,
            Some(directory.join("calendars/usage.yml"))
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn scenario_selector_rejects_multiple_sources() {
        let error = serde_yaml::from_str::<super::ScenarioBatchRequest>(
            r#"batch_id: ambiguous
scenarios:
  - id: ambiguous
    overlay: closure.yml
    top_k: {ranking: usage.csv, count: 1}
routes: []
"#,
        )
        .expect_err("multiple scenario selector sources must be rejected");

        assert!(
            error.to_string().contains("exactly one selector"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn exact_betweenness_diff_tracks_lost_baseline_pairs() {
        let mut baseline_result = betweenness_result(2, 0, 12.0, Vec::new());
        baseline_result.pairs = vec![
            crate::BetweennessPairResult {
                origin_index: 0,
                destination_index: 0,
                demand: 5.0,
                routed: true,
            },
            crate::BetweennessPairResult {
                origin_index: 0,
                destination_index: 1,
                demand: 7.0,
                routed: true,
            },
        ];
        let mut scenario_result = betweenness_result(2, 1, 12.0, Vec::new());
        scenario_result.pairs = vec![
            crate::BetweennessPairResult {
                origin_index: 0,
                destination_index: 0,
                demand: 5.0,
                routed: false,
            },
            crate::BetweennessPairResult {
                origin_index: 0,
                destination_index: 1,
                demand: 7.0,
                routed: true,
            },
            crate::BetweennessPairResult {
                origin_index: 0,
                destination_index: 2,
                demand: 5.0,
                routed: true,
            },
        ];
        let baseline = ScenarioAnalysisSnapshot {
            betweenness: vec![ScenarioBetweennessExecution {
                analysis_id: "usage".to_string(),
                result: Some(baseline_result),
                error: None,
            }],
            ..Default::default()
        };
        let scenario = ScenarioAnalysisSnapshot {
            betweenness: vec![ScenarioBetweennessExecution {
                analysis_id: "usage".to_string(),
                result: Some(scenario_result),
                error: None,
            }],
            ..Default::default()
        };

        let diff = diff_snapshots(&baseline, &scenario);
        assert_eq!(diff.disconnected_betweenness_pair_count, 1);
        assert_eq!(diff.weighted_disconnected_demand, 5.0);
    }

    #[test]
    fn ranks_source_features_from_prior_betweenness_result() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("netweevil-ranking-{unique}.json"));
        fs::write(
            &path,
            serde_json::to_vec(&betweenness_result(
                1,
                0,
                1.0,
                vec![(0, 10, 3.0), (1, 10, 4.0), (2, 11, 6.0)],
            ))
            .expect("serialize betweenness result"),
        )
        .expect("write betweenness ranking");

        let ranked = load_ranked_source_features(&path, 2).expect("ranked source features");
        assert_eq!(ranked, vec![10, 11]);

        let _ = fs::remove_file(path);
    }

    fn route_result(travel_time_s: f64) -> RouteResult {
        serde_json::from_value(serde_json::json!({
            "route_id": "r1",
            "origin": snapped("a"),
            "destination": snapped("b"),
            "outcome": "legal",
            "summary": {
                "total_distance_m": 100,
                "total_travel_time_s": travel_time_s,
                "total_generalized_cost": travel_time_s,
                "segment_count": 1
            }
        }))
        .expect("minimal route result")
    }

    fn od_result(status: BatchItemStatus, travel_time_s: Option<f64>) -> OdResult {
        serde_json::from_value(serde_json::json!({
            "pair_count": 1,
            "succeeded_count": usize::from(status == BatchItemStatus::Succeeded),
            "failed_count": usize::from(status == BatchItemStatus::Failed),
            "pairs": [{
                "pair_id": "a_b",
                "origin_id": "a",
                "destination_id": "b",
                "status": status,
                "total_travel_time_s": travel_time_s,
            }]
        }))
        .expect("minimal OD result")
    }

    fn matrix_result(status: BatchItemStatus, travel_time_s: Option<f64>) -> MatrixResult {
        serde_json::from_value(serde_json::json!({
            "origin_count": 1,
            "destination_count": 1,
            "cell_count": 1,
            "succeeded_count": usize::from(status == BatchItemStatus::Succeeded),
            "failed_count": usize::from(status == BatchItemStatus::Failed),
            "cells": [{
                "origin_id": "a",
                "destination_id": "b",
                "status": status,
                "total_travel_time_s": travel_time_s,
            }]
        }))
        .expect("minimal matrix result")
    }

    fn betweenness_result(
        routed_pair_count: usize,
        unreachable_pair_count: usize,
        routed_demand: f64,
        edges: Vec<(u32, i64, f64)>,
    ) -> BetweennessResult {
        BetweennessResult {
            analysis_id: "usage".to_string(),
            departure_time: None,
            arrive_by: None,
            scenario_id: None,
            origin_count: 1,
            destination_count: 1,
            requested_pair_count: 1,
            routed_pair_count,
            unreachable_pair_count,
            routed_demand,
            time_dependent: true,
            pairs: Vec::new(),
            edges: edges
                .into_iter()
                .map(
                    |(edge_id, source_way_id, score)| crate::BetweennessEdgeScore {
                        edge_id,
                        source_way_id,
                        from_node_id: edge_id,
                        to_node_id: edge_id + 1,
                        score,
                        normalized_score: 1.0,
                        routed_pair_count: u64::from(routed_pair_count > 0),
                        geometry: Vec::new(),
                    },
                )
                .collect(),
            warnings: Vec::new(),
        }
    }

    fn snapped(id: &str) -> serde_json::Value {
        serde_json::json!({
            "point_id": id,
            "requested_lon": 0.0,
            "requested_lat": 0.0,
            "snapped_node_id": 0,
            "snapped_lon": 0.0,
            "snapped_lat": 0.0,
            "snapped_z": 0.0,
            "snap_distance_m": 0.0
        })
    }

    fn accessibility_result(status: BatchItemStatus, count: usize) -> AccessibilityResult {
        AccessibilityResult {
            departure_time: None,
            arrive_by: None,
            scenario_id: None,
            origin_count: 1,
            category_count: 1,
            destination_count: 3,
            max_travel_time_s: 900.0,
            thresholds_s: vec![900.0],
            row_count: 1,
            succeeded_count: usize::from(status == BatchItemStatus::Succeeded),
            failed_count: usize::from(status == BatchItemStatus::Failed),
            skipped_origin_count: 0,
            rows: vec![AccessibilityCategoryResult {
                origin_id: "a".to_string(),
                category_id: "jobs".to_string(),
                status,
                outcome: if status == BatchItemStatus::Succeeded {
                    AnalysisOutcome::Legal
                } else {
                    AnalysisOutcome::Unreachable
                },
                fallback_used: false,
                origin_component_id: None,
                origin_hop_distance_m: None,
                origin_snap_distance_m: None,
                destination_count: 3,
                snapped_destination_count: count,
                nearest_destination_id: None,
                nearest_travel_time_s: None,
                nearest_destination_snap_distance_m: None,
                counts_within_threshold_s: BTreeMap::from([(threshold_key(900.0), count)]),
                diagnostics: Vec::new(),
                error: None,
            }],
            diagnostics: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[allow(dead_code)]
    fn assert_default_diff(_: ScenarioBatchDiff) {}
}
