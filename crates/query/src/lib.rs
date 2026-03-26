use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use netan_core::{
    CompiledProfileBundle, EDGE_FLAG_ROUNDABOUT, EDGE_FLAG_TARGET_TRAFFIC_SIGNAL, RoadClass,
    SurfaceClass, TopologyBundle, TopologyNode,
};
use netan_profile::ReturnConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRequest {
    pub route_id: String,
    pub origin: LabeledPoint,
    pub destination: LabeledPoint,
    #[serde(default)]
    pub snap: SnapOptions,
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
    pub returns: ReturnConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PointSetDocument {
    #[serde(default)]
    pub points: Vec<LabeledPoint>,
    #[serde(default)]
    pub snap: SnapOptions,
    #[serde(default)]
    pub returns: ReturnConfig,
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
            returns: ReturnConfig::default(),
        },
    }
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteSummary {
    pub total_distance_m: u64,
    pub total_travel_time_s: f64,
    pub total_generalized_cost: f64,
    pub segment_count: usize,
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
    pub summary: RouteSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_path: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edge_path: Vec<u32>,
    #[serde(default)]
    pub geometry: Option<Vec<[f64; 2]>>,
    #[serde(default)]
    pub segments: Option<Vec<RouteSegment>>,
    #[serde(default)]
    pub breakdowns: Option<RouteBreakdowns>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchItemStatus {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdPairResult {
    pub pair_id: String,
    pub origin_id: String,
    pub destination_id: String,
    pub status: BatchItemStatus,
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
    pub geometry: Option<Vec<[f64; 2]>>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdResult {
    pub pair_count: usize,
    pub succeeded_count: usize,
    pub failed_count: usize,
    pub pairs: Vec<OdPairResult>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixCellResult {
    pub origin_id: String,
    pub destination_id: String,
    pub status: BatchItemStatus,
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
    pub geometry: Option<Vec<[f64; 2]>>,
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
    pub cells: Vec<MatrixCellResult>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

pub struct PreparedRoutingEngine {
    topology: Arc<TopologyBundle>,
    metrics: Arc<CompiledProfileBundle>,
    routing_graph: RoutingGraph,
}

impl PreparedRoutingEngine {
    pub fn new(topology: Arc<TopologyBundle>, metrics: Arc<CompiledProfileBundle>) -> Result<Self> {
        validate_execution_inputs(topology.as_ref(), metrics.as_ref())?;
        let routing_graph = build_routing_graph(topology.as_ref(), metrics.as_ref())?;
        Ok(Self {
            topology,
            metrics,
            routing_graph,
        })
    }

    pub fn topology(&self) -> &TopologyBundle {
        self.topology.as_ref()
    }

    pub fn metrics(&self) -> &CompiledProfileBundle {
        self.metrics.as_ref()
    }

    pub fn execute_route(&self, request: &RouteRequest) -> Result<RouteResult> {
        self.execute_route_with_optional_edge_names(request, None)
    }

    pub fn execute_route_with_edge_names(
        &self,
        request: &RouteRequest,
        edge_names: &[String],
    ) -> Result<RouteResult> {
        self.execute_route_with_optional_edge_names(request, Some(edge_names))
    }

    fn execute_route_with_optional_edge_names(
        &self,
        request: &RouteRequest,
        edge_names: Option<&[String]>,
    ) -> Result<RouteResult> {
        execute_route_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            &self.routing_graph,
            request,
            edge_names,
        )
    }

    pub fn execute_od(&self, document: &OdPairsDocument) -> Result<OdResult> {
        execute_od_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            &self.routing_graph,
            document,
        )
    }

    pub fn execute_matrix(
        &self,
        origins: &PointSetDocument,
        destinations: &PointSetDocument,
    ) -> Result<MatrixResult> {
        execute_matrix_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            &self.routing_graph,
            origins,
            destinations,
        )
    }
}

fn execute_od_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    document: &OdPairsDocument,
) -> Result<OdResult> {
    let mut snap_cache = HashMap::new();
    let mut pairs = Vec::with_capacity(document.pairs.len());
    let mut succeeded_count = 0_usize;

    for pair in &document.pairs {
        let request = RouteRequest {
            route_id: pair.pair_id.clone(),
            origin: pair.origin.clone(),
            destination: pair.destination.clone(),
            snap: document.snap.clone(),
            returns: document.returns.clone(),
        };
        let origin_candidates = cached_snap_candidates(
            &mut snap_cache,
            topology,
            routing_graph,
            &pair.origin,
            document.snap.max_distance_m,
            true,
        );
        let destination_candidates = cached_snap_candidates(
            &mut snap_cache,
            topology,
            routing_graph,
            &pair.destination,
            document.snap.max_distance_m,
            false,
        );
        let route = match (origin_candidates, destination_candidates) {
            (Ok(origin_candidates), Ok(destination_candidates)) => execute_route_with_candidates(
                topology,
                metrics,
                routing_graph,
                &request,
                &origin_candidates,
                &destination_candidates,
                None,
            ),
            (Err(error), _) => Err(anyhow::anyhow!(error)),
            (_, Err(error)) => Err(anyhow::anyhow!(error)),
        };
        match route {
            Ok(route) => {
                succeeded_count += 1;
                pairs.push(OdPairResult {
                    pair_id: pair.pair_id.clone(),
                    origin_id: pair.origin.id.clone(),
                    destination_id: pair.destination.id.clone(),
                    status: BatchItemStatus::Succeeded,
                    origin_snap_distance_m: Some(route.origin.snap_distance_m),
                    destination_snap_distance_m: Some(route.destination.snap_distance_m),
                    total_distance_m: Some(route.summary.total_distance_m),
                    total_travel_time_s: Some(route.summary.total_travel_time_s),
                    total_generalized_cost: Some(route.summary.total_generalized_cost),
                    geometry: route.geometry,
                    error: None,
                });
            }
            Err(error) => {
                pairs.push(OdPairResult {
                    pair_id: pair.pair_id.clone(),
                    origin_id: pair.origin.id.clone(),
                    destination_id: pair.destination.id.clone(),
                    status: BatchItemStatus::Failed,
                    origin_snap_distance_m: None,
                    destination_snap_distance_m: None,
                    total_distance_m: None,
                    total_travel_time_s: None,
                    total_generalized_cost: None,
                    geometry: None,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    Ok(OdResult {
        pair_count: pairs.len(),
        succeeded_count,
        failed_count: pairs.len() - succeeded_count,
        pairs,
        warnings: {
            let mut warnings = vec![
                "Batch OD execution repeats the exact single-route solver per pair.".to_string(),
            ];
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
    let origin_snaps = presnap_point_set(
        topology,
        routing_graph,
        &origins.points,
        snap_max_distance_m,
    );
    let destination_snaps = presnap_point_set(
        topology,
        routing_graph,
        &destinations.points,
        snap_max_distance_m,
    );
    let mut cells = Vec::with_capacity(origins.points.len() * destinations.points.len());
    let mut succeeded_count = 0_usize;

    for (origin, origin_candidates) in origins.points.iter().zip(&origin_snaps) {
        for (destination, destination_candidates) in
            destinations.points.iter().zip(&destination_snaps)
        {
            let request = RouteRequest {
                route_id: format!("{}__{}", origin.id, destination.id),
                origin: origin.clone(),
                destination: destination.clone(),
                snap: SnapOptions {
                    max_distance_m: snap_max_distance_m,
                },
                returns: returns.clone(),
            };
            let route = match (origin_candidates, destination_candidates) {
                (Ok(origin_candidates), Ok(destination_candidates)) => {
                    execute_route_with_candidates(
                        topology,
                        metrics,
                        routing_graph,
                        &request,
                        origin_candidates,
                        destination_candidates,
                        None,
                    )
                }
                (Err(error), _) => Err(anyhow::anyhow!(error.clone())),
                (_, Err(error)) => Err(anyhow::anyhow!(error.clone())),
            };
            match route {
                Ok(route) => {
                    succeeded_count += 1;
                    cells.push(MatrixCellResult {
                        origin_id: origin.id.clone(),
                        destination_id: destination.id.clone(),
                        status: BatchItemStatus::Succeeded,
                        origin_snap_distance_m: Some(route.origin.snap_distance_m),
                        destination_snap_distance_m: Some(route.destination.snap_distance_m),
                        total_distance_m: Some(route.summary.total_distance_m),
                        total_travel_time_s: Some(route.summary.total_travel_time_s),
                        total_generalized_cost: Some(route.summary.total_generalized_cost),
                        geometry: route.geometry,
                        error: None,
                    });
                }
                Err(error) => {
                    cells.push(MatrixCellResult {
                        origin_id: origin.id.clone(),
                        destination_id: destination.id.clone(),
                        status: BatchItemStatus::Failed,
                        origin_snap_distance_m: None,
                        destination_snap_distance_m: None,
                        total_distance_m: None,
                        total_travel_time_s: None,
                        total_generalized_cost: None,
                        geometry: None,
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
        failed_count: cells.len() - succeeded_count,
        cells,
        warnings: {
            let mut warnings = vec![
                "Matrix execution currently repeats the exact single-route solver per origin/destination cell."
                    .to_string(),
            ];
            warnings.extend(execution_warnings(metrics));
            warnings
        },
    })
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
    validate_execution_inputs(topology, metrics)?;
    let routing_graph = build_routing_graph(topology, metrics)?;
    execute_route_with_graph(topology, metrics, &routing_graph, request, edge_names)
}

pub fn execute_od(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    document: &OdPairsDocument,
) -> Result<OdResult> {
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

fn execute_route_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &RouteRequest,
    edge_names: Option<&[String]>,
) -> Result<RouteResult> {
    let origin_candidates = snap_candidates(
        topology,
        routing_graph,
        &request.origin,
        request.snap.max_distance_m,
    )?;
    let destination_candidates = snap_candidates(
        topology,
        routing_graph,
        &request.destination,
        request.snap.max_distance_m,
    )?;
    execute_route_with_candidates(
        topology,
        metrics,
        routing_graph,
        request,
        &origin_candidates,
        &destination_candidates,
        edge_names,
    )
}

fn execute_route_with_candidates(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &RouteRequest,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
    edge_names: Option<&[String]>,
) -> Result<RouteResult> {
    let (origin, destination, path) = route_between_candidates(
        topology,
        metrics,
        routing_graph,
        origin_candidates,
        destination_candidates,
    )?;

    let include_detailed_paths = request_returns_detailed_path(request);
    let needs_node_path = include_detailed_paths
        || !matches!(
            request.returns.geometry,
            netan_profile::ReturnGeometry::None
        );
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

    let geometry = match request.returns.geometry {
        netan_profile::ReturnGeometry::None => None,
        _ => Some(build_route_geometry(
            topology,
            &path.edge_indexes,
            &origin,
            &destination,
        )),
    };

    let segments = if request.returns.segment_rows {
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
                    }
                })
                .collect(),
        )
    } else {
        None
    };

    let breakdowns = build_breakdowns(topology, metrics, &path.edge_indexes, &request.returns);
    let warnings = execution_warnings(metrics);

    Ok(RouteResult {
        route_id: request.route_id.clone(),
        origin,
        destination,
        summary: RouteSummary {
            total_distance_m: path.total_distance_m,
            total_travel_time_s: path.total_travel_time_s,
            total_generalized_cost: path.total_generalized_cost,
            segment_count: path.edge_indexes.len(),
        },
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
        segments,
        breakdowns,
        warnings,
    })
}

fn request_returns_detailed_path(request: &RouteRequest) -> bool {
    !matches!(
        request.returns.geometry,
        netan_profile::ReturnGeometry::None
    ) || request.returns.segment_rows
        || !request.returns.road_type_breakdown.is_empty()
        || !request.returns.surface_breakdown.is_empty()
        || request.returns.penalty_breakdown
        || request.returns.explain_cost_derivation
}

fn presnap_point_set(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    points: &[LabeledPoint],
    max_distance_m: f64,
) -> Vec<Result<Vec<SnappedPoint>, String>> {
    points
        .iter()
        .map(|point| {
            snap_candidates(topology, routing_graph, point, max_distance_m)
                .map_err(|error| error.to_string())
        })
        .collect()
}

fn cached_snap_candidates(
    cache: &mut HashMap<(bool, String, u64, u64, u64), Result<Vec<SnappedPoint>, String>>,
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    max_distance_m: f64,
    is_origin: bool,
) -> Result<Vec<SnappedPoint>, String> {
    let key = (
        is_origin,
        point.id.clone(),
        point.lon.to_bits(),
        point.lat.to_bits(),
        max_distance_m.to_bits(),
    );
    let value = cache.entry(key).or_insert_with(|| {
        snap_candidates(topology, routing_graph, point, max_distance_m)
            .map_err(|error| error.to_string())
    });
    value.clone()
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
    total_distance_m: u64,
    total_travel_time_s: f64,
    total_generalized_cost: f64,
}

struct RoutingGraph {
    first_out: Vec<u32>,
    edge_order: Vec<u32>,
    incoming_first_out: Vec<u32>,
    incoming_edge_order: Vec<u32>,
    head: Vec<u32>,
    edge_costs: Vec<f64>,
    transition_first_out: Vec<u32>,
    transition_edges: Vec<u32>,
    transition_costs: Vec<f64>,
    reverse_transition_first_out: Vec<u32>,
    reverse_transition_edges: Vec<u32>,
    reverse_transition_costs: Vec<f64>,
    acceleration: Option<AccelerationGraph>,
    automaton: RestrictionAutomaton,
    min_cost_per_meter: f64,
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

#[cfg(test)]
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
}

fn build_routing_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
) -> Result<RoutingGraph> {
    let mut out_degree = vec![0_u32; topology.nodes.len()];
    let mut in_degree = vec![0_u32; topology.nodes.len()];
    let mut head = vec![0_u32; topology.edges.len()];
    let mut edge_costs = vec![f64::INFINITY; topology.edges.len()];
    let mut min_cost_per_meter = f64::INFINITY;

    for (edge_index, edge) in topology.edges.iter().enumerate() {
        head[edge_index] = edge.to.0;
        let metric = &metrics.edge_metrics[edge_index];
        if let (Some(cost), Some(_)) = (metric.generalized_cost, metric.travel_time_s) {
            edge_costs[edge_index] = cost;
            in_degree[edge.to.0 as usize] += 1;
            if edge.length_m > 0 {
                min_cost_per_meter = min_cost_per_meter.min(cost / edge.length_m as f64);
            }
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
            let head_node = head[edge_index as usize] as usize;
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
        if restriction.edge_path.len() == 2 {
            pairwise_forbidden.push((restriction.edge_path[0].0, restriction.edge_path[1].0));
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
            if !forbidden_turns[forbidden_turn_first_out[edge_index] as usize
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
            if forbidden_turns[forbidden_turn_first_out[edge_index] as usize
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
        head,
        edge_costs,
        transition_first_out,
        transition_edges,
        transition_costs,
        reverse_transition_first_out,
        reverse_transition_edges,
        reverse_transition_costs,
        acceleration: build_acceleration_graph(metrics, topology.edges.len())?,
        automaton: RestrictionAutomaton::build(&restricted_sequences),
        min_cost_per_meter: if min_cost_per_meter.is_finite() {
            min_cost_per_meter
        } else {
            0.0
        },
    })
}

fn build_acceleration_graph(
    metrics: &CompiledProfileBundle,
    edge_count: usize,
) -> Result<Option<AccelerationGraph>> {
    let Some(acceleration) = metrics.acceleration.as_ref() else {
        return Ok(None);
    };
    if acceleration.algorithm == "oriented_paths_v1"
        || acceleration.algorithm.ends_with("+oriented_paths_v1")
    {
        // The current persisted oriented transition bundle is only preprocessing scaffolding.
        // It is not a shortcut graph and should not be used on the hot query path.
        return Ok(None);
    }
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
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
) -> Result<(SnappedPoint, SnappedPoint, RoutePath)> {
    let mut path_cache = HashMap::<((u32, u64, u64), (u32, u64, u64)), Option<RoutePath>>::new();

    for origin in origin_candidates {
        for destination in destination_candidates {
            if same_edge_reverse_pair(origin, destination) {
                continue;
            }
            let key = (snap_cache_key(origin), snap_cache_key(destination));
            let path = if let Some(cached) = path_cache.get(&key) {
                cached.clone()
            } else {
                let direct_path = direct_same_edge_path(routing_graph, origin, destination);
                let path = if routing_graph.has_restriction_sequences() {
                    astar_with_restriction_sequences(
                        topology,
                        routing_graph,
                        origin.snapped_node_id as usize,
                        destination.snapped_node_id as usize,
                    )?
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
                    seeded_bidirectional_dijkstra_on_edge_transitions(
                        topology,
                        routing_graph,
                        &origin_seeds,
                        &destination_seeds,
                        direct_path.clone(),
                    )?
                };
                path_cache.insert(key, path.clone());
                path
            };
            if let Some(path) = path {
                let path =
                    finalize_route_path(topology, metrics, path.edge_indexes, origin, destination);
                return Ok((origin.clone(), destination.clone(), path));
            }
        }
    }

    bail!("no route found between the snapped origin and destination")
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
                total_distance_m: 0,
                total_travel_time_s: 0.0,
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
        total_distance_m: 0,
        total_travel_time_s: 0.0,
        total_generalized_cost: edge_cost * (destination_fraction - origin_fraction),
    })
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
        total_distance_m: 0,
        total_travel_time_s: 0.0,
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
        total_distance_m: 0,
        total_travel_time_s: 0.0,
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
        total_distance_m: 0,
        total_travel_time_s: 0.0,
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

fn astar_with_restriction_sequences(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    source: usize,
    target: usize,
) -> Result<Option<RoutePath>> {
    if source >= topology.nodes.len() || target >= topology.nodes.len() {
        bail!("source or target node is out of bounds for the topology bundle");
    }
    if source == target {
        return Ok(Some(RoutePath {
            edge_indexes: Vec::new(),
            total_distance_m: 0,
            total_travel_time_s: 0.0,
            total_generalized_cost: 0.0,
        }));
    }

    RESTRICTED_SEARCH_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        scratch.prepare();

        let mut best_cost = f64::INFINITY;
        let mut best_state = None;

        for &edge_index in routing_graph.outgoing_edges(source) {
            let edge_index = edge_index as usize;
            let cost = routing_graph.edge_costs[edge_index];
            if !cost.is_finite() {
                continue;
            }
            let automaton_state = routing_graph.automaton.transition(0, edge_index);
            let key = SearchStateKey {
                edge_index,
                automaton_state,
            };
            scratch.dist.insert(key, cost);
            scratch.previous.insert(key, None);
            scratch.heap.push(State {
                edge_index,
                automaton_state,
                cost,
                score: cost
                    + heuristic_cost(
                        topology,
                        routing_graph,
                        routing_graph.head[edge_index] as usize,
                        target,
                    ),
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

            let node_index = routing_graph.head[edge_index] as usize;
            if node_index == target {
                best_cost = cost;
                best_state = Some(key);
                break;
            }
            if cost >= best_cost {
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
                let next_cost = cost + routing_graph.transition_costs[transition_index];
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
                        score: next_cost
                            + heuristic_cost(
                                topology,
                                routing_graph,
                                routing_graph.head[next_edge] as usize,
                                target,
                            ),
                    });
                }
            }
        }

        let Some(mut cursor) = best_state else {
            return Ok(None);
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

        Ok(Some(RoutePath {
            edge_indexes,
            total_distance_m: 0,
            total_travel_time_s: 0.0,
            total_generalized_cost: best_cost,
        }))
    })
}

fn finalize_route_path(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    edge_indexes: Vec<usize>,
    origin: &SnappedPoint,
    destination: &SnappedPoint,
) -> RoutePath {
    let mut total_distance_m = 0_u64;
    let mut total_travel_time_s = 0.0;
    let mut total_generalized_cost = 0.0;
    let mut previous_edge_index = None;
    let first_edge = edge_indexes.first().copied();
    let last_edge = edge_indexes.last().copied();
    for &edge_index in &edge_indexes {
        let edge = &topology.edges[edge_index];
        let metric = &metrics.edge_metrics[edge_index];
        let factor = edge_traversal_factor(edge_index, first_edge, last_edge, origin, destination);
        total_distance_m += (edge.length_m as f64 * factor).round() as u64;
        total_travel_time_s += metric.travel_time_s.unwrap_or_default() * factor;
        total_generalized_cost += metric.generalized_cost.unwrap_or_default() * factor;
        if let Some(previous_edge_index) = previous_edge_index {
            let turn_penalty_s =
                turn_penalty_seconds(topology, metrics, previous_edge_index, edge_index);
            total_travel_time_s += turn_penalty_s;
            total_generalized_cost += turn_penalty_s * metrics.turn_costs.cost_time_weight;
        }
        previous_edge_index = Some(edge_index);
    }

    RoutePath {
        edge_indexes,
        total_distance_m,
        total_travel_time_s,
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

fn heuristic_cost(
    topology: &TopologyBundle,
    routing_graph: &RoutingGraph,
    from_node: usize,
    to_node: usize,
) -> f64 {
    haversine_meters(
        topology.nodes[from_node].lon,
        topology.nodes[from_node].lat,
        topology.nodes[to_node].lon,
        topology.nodes[to_node].lat,
    ) * routing_graph.min_cost_per_meter
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
            },
            MAX_SNAP_CANDIDATES,
        );
    }

    let mut candidate_edges = BTreeSet::<u32>::new();
    for &(node_id, _) in &nearby_nodes {
        for &edge_index in routing_graph.outgoing_edges(node_id as usize) {
            candidate_edges.insert(edge_index);
        }
        for &edge_index in routing_graph.incoming_edges(node_id as usize) {
            candidate_edges.insert(edge_index);
        }
    }
    if candidate_edges.is_empty() {
        for edge_index in 0..topology.edges.len() {
            if routing_graph.edge_costs[edge_index].is_finite() {
                candidate_edges.insert(edge_index as u32);
            }
        }
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
            },
            MAX_SNAP_CANDIDATES,
        );
    }

    if candidates.is_empty() {
        bail!(
            "point '{}' has no traversable candidate node or edge within {:.1} m",
            point.id,
            max_distance_m
        );
    }

    candidates.sort_by(|left, right| left.snap_distance_m.total_cmp(&right.snap_distance_m));
    candidates.dedup_by(|left, right| snap_candidate_key(left) == snap_candidate_key(right));
    candidates.truncate(MAX_SNAP_CANDIDATES);
    Ok(candidates)
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
        AnalysisKind, OdPair, OdPairsDocument, PointSetDocument, PreparedRoutingEngine,
        RouteRequest, SnapOptions, build_routing_graph, execute_matrix, execute_od, execute_route,
        execute_route_with_edge_names, load_experiment, load_od_pairs, load_point_set,
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
        let metrics = CompiledProfileBundle {
            schema_version: 2,
            profile_id: "snap".to_string(),
            profile_hash: "abc".to_string(),
            mode: TravelMode::Car,
            turn_costs: CompiledTurnCostConfig::default(),
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            acceleration: None,
            edge_metrics: vec![CompiledEdgeMetric {
                edge_id: EdgeId(0),
                travel_time_s: Some(1.0),
                generalized_cost: Some(1.0),
            }],
        };
        let graph = build_routing_graph(&topology, &metrics).expect("graph builds");
        let point = super::LabeledPoint {
            id: "snap".to_string(),
            lon: 6.0,
            lat: 53.0,
        };

        let candidates =
            super::snap_candidates(&topology, &graph, &point, 500.0).expect("snap works");

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
        with_edge_based_topology(TopologyBundle {
            schema_version: 3,
            source_path: "test".to_string(),
            source_sha256: "abc".to_string(),
            nodes,
            edges: vec![DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 1,
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
            spatial_index: None,
        })
    }

    fn with_edge_based_topology(mut topology: TopologyBundle) -> TopologyBundle {
        topology.edge_based_topology = super::build_edge_based_topology_fallback(&topology);
        topology
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
