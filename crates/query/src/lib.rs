use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use netan_core::{CompiledProfileBundle, RoadClass, SurfaceClass, TopologyBundle};
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
    pub node_path: Vec<u32>,
    #[serde(default)]
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
        execute_route_with_graph(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            &self.routing_graph,
            request,
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
        match execute_route_with_graph(topology, metrics, routing_graph, &request) {
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
        warnings: vec![
            "Batch OD execution repeats the exact single-route solver per pair. Turn restrictions are modeled when present in the topology bundle; turn penalties are not modeled yet.".to_string(),
        ],
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
    let mut cells = Vec::with_capacity(origins.points.len() * destinations.points.len());
    let mut succeeded_count = 0_usize;

    for origin in &origins.points {
        for destination in &destinations.points {
            let request = RouteRequest {
                route_id: format!("{}__{}", origin.id, destination.id),
                origin: origin.clone(),
                destination: destination.clone(),
                snap: SnapOptions {
                    max_distance_m: snap_max_distance_m,
                },
                returns: returns.clone(),
            };
            match execute_route_with_graph(topology, metrics, routing_graph, &request) {
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
        warnings: vec![
            "Matrix execution currently repeats the exact single-route solver per origin/destination cell. Turn restrictions are modeled when present in the topology bundle; turn penalties are not modeled yet.".to_string(),
        ],
    })
}

pub fn execute_route(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &RouteRequest,
) -> Result<RouteResult> {
    validate_execution_inputs(topology, metrics)?;
    let routing_graph = build_routing_graph(topology, metrics)?;
    execute_route_with_graph(topology, metrics, &routing_graph, request)
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
) -> Result<RouteResult> {
    let origin_candidates = snap_candidates(
        topology,
        &request.origin,
        &routing_graph.origin_eligible,
        request.snap.max_distance_m,
    )?;
    let destination_candidates = snap_candidates(
        topology,
        &request.destination,
        &routing_graph.destination_eligible,
        request.snap.max_distance_m,
    )?;
    let (origin, destination, path) = route_between_candidates(
        topology,
        metrics,
        routing_graph,
        &origin_candidates,
        &destination_candidates,
    )?;

    let mut node_path = Vec::with_capacity(path.edge_indexes.len() + 1);
    node_path.push(origin.snapped_node_id);
    for &edge_index in &path.edge_indexes {
        node_path.push(topology.edges[edge_index].to.0);
    }

    let geometry = match request.returns.geometry {
        netan_profile::ReturnGeometry::None => None,
        _ => Some(
            node_path
                .iter()
                .map(|&node_id| {
                    let node = &topology.nodes[node_id as usize];
                    [node.lon, node.lat]
                })
                .collect(),
        ),
    };

    let segments = if request.returns.segment_rows {
        Some(
            path.edge_indexes
                .iter()
                .map(|&edge_index| {
                    let edge = &topology.edges[edge_index];
                    let metric = &metrics.edge_metrics[edge_index];
                    RouteSegment {
                        edge_id: edge.edge_id.0,
                        from_node_id: edge.from.0,
                        to_node_id: edge.to.0,
                        source_way_id: edge.source_way_id,
                        length_m: edge.length_m,
                        travel_time_s: metric.travel_time_s.unwrap_or_default(),
                        generalized_cost: metric.generalized_cost.unwrap_or_default(),
                        road_class: edge.road_class,
                        surface: edge.surface,
                        name: edge
                            .name_index
                            .and_then(|index| topology.names.get(index as usize))
                            .cloned(),
                    }
                })
                .collect(),
        )
    } else {
        None
    };

    let breakdowns = build_breakdowns(topology, metrics, &path.edge_indexes, &request.returns);
    let mut warnings = Vec::new();
    warnings.push(
        "Turn restrictions are modeled when present in the topology bundle; turn penalties are not modeled yet.".to_string(),
    );

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
        edge_path: path
            .edge_indexes
            .iter()
            .map(|&edge_index| topology.edges[edge_index].edge_id.0)
            .collect(),
        geometry,
        segments,
        breakdowns,
        warnings,
    })
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

struct RoutePath {
    edge_indexes: Vec<usize>,
    total_distance_m: u64,
    total_travel_time_s: f64,
    total_generalized_cost: f64,
}

struct RoutingGraph {
    outgoing: Vec<Vec<usize>>,
    edge_costs: Vec<f64>,
    automaton: RestrictionAutomaton,
    min_cost_per_meter: f64,
    origin_eligible: Vec<bool>,
    destination_eligible: Vec<bool>,
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
    let mut outgoing = vec![Vec::<usize>::new(); topology.nodes.len()];
    let mut destination_eligible = vec![false; topology.nodes.len()];
    let mut edge_costs = vec![f64::INFINITY; topology.edges.len()];
    let mut min_cost_per_meter = f64::INFINITY;

    for (edge_index, edge) in topology.edges.iter().enumerate() {
        let metric = &metrics.edge_metrics[edge_index];
        if let (Some(cost), Some(_)) = (metric.generalized_cost, metric.travel_time_s) {
            outgoing[edge.from.0 as usize].push(edge_index);
            destination_eligible[edge.to.0 as usize] = true;
            edge_costs[edge_index] = cost;
            if edge.length_m > 0 {
                min_cost_per_meter = min_cost_per_meter.min(cost / edge.length_m as f64);
            }
        }
    }

    let mode_bit = metrics.mode.access_bit();
    let mut restricted_sequences = Vec::new();
    for restriction in topology
        .turn_restrictions
        .iter()
        .filter(|restriction| restriction.mode_mask.contains(mode_bit))
    {
        let sequence = restriction
            .edge_path
            .iter()
            .map(|edge| edge.0 as usize)
            .collect::<Vec<_>>();
        if sequence.len() < 2 {
            continue;
        }
        restricted_sequences.push(sequence);
    }

    let origin_eligible = outgoing.iter().map(|edges| !edges.is_empty()).collect();
    Ok(RoutingGraph {
        outgoing,
        edge_costs,
        automaton: RestrictionAutomaton::build(&restricted_sequences),
        min_cost_per_meter: if min_cost_per_meter.is_finite() {
            min_cost_per_meter
        } else {
            0.0
        },
        origin_eligible,
        destination_eligible,
    })
}

fn route_between_candidates(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    origin_candidates: &[SnappedPoint],
    destination_candidates: &[SnappedPoint],
) -> Result<(SnappedPoint, SnappedPoint, RoutePath)> {
    for origin in origin_candidates {
        for destination in destination_candidates {
            let path = astar_with_restriction_sequences(
                topology,
                routing_graph,
                origin.snapped_node_id as usize,
                destination.snapped_node_id as usize,
            )?;
            if let Some(path) = path {
                let path = finalize_route_path(topology, metrics, path.edge_indexes);
                return Ok((origin.clone(), destination.clone(), path));
            }
        }
    }

    bail!("no route found between the snapped origin and destination")
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

    let mut best_cost = f64::INFINITY;
    let mut best_state = None;
    let mut dist = HashMap::<SearchStateKey, f64>::new();
    let mut previous = HashMap::<SearchStateKey, Option<SearchStateKey>>::new();
    let mut heap = BinaryHeap::new();

    for &edge_index in &routing_graph.outgoing[source] {
        let cost = routing_graph.edge_costs[edge_index];
        if !cost.is_finite() {
            continue;
        }
        let automaton_state = routing_graph.automaton.transition(0, edge_index);
        let key = SearchStateKey {
            edge_index,
            automaton_state,
        };
        dist.insert(key, cost);
        previous.insert(key, None);
        heap.push(State {
            edge_index,
            automaton_state,
            cost,
            score: cost
                + heuristic_cost(
                    topology,
                    routing_graph,
                    topology.edges[edge_index].to.0 as usize,
                    target,
                ),
        });
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
        let Some(&known_cost) = dist.get(&key) else {
            continue;
        };
        if cost > known_cost {
            continue;
        }

        let node_index = topology.edges[edge_index].to.0 as usize;
        if node_index == target {
            best_cost = cost;
            best_state = Some(key);
            break;
        }
        if cost >= best_cost {
            continue;
        }

        for &next_edge in &routing_graph.outgoing[node_index] {
            if !routing_graph
                .automaton
                .is_transition_allowed(automaton_state, next_edge)
            {
                continue;
            }
            let next_cost = cost + routing_graph.edge_costs[next_edge];
            let next_automaton_state = routing_graph
                .automaton
                .transition(automaton_state, next_edge);
            let next_key = SearchStateKey {
                edge_index: next_edge,
                automaton_state: next_automaton_state,
            };
            if next_cost + f64::EPSILON < *dist.get(&next_key).unwrap_or(&f64::INFINITY) {
                dist.insert(next_key, next_cost);
                previous.insert(next_key, Some(key));
                heap.push(State {
                    edge_index: next_edge,
                    automaton_state: next_automaton_state,
                    cost: next_cost,
                    score: next_cost
                        + heuristic_cost(
                            topology,
                            routing_graph,
                            topology.edges[next_edge].to.0 as usize,
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
        let previous_state = previous.get(&cursor).copied().flatten();
        let Some(previous_state) = previous_state else {
            break;
        };
        cursor = previous_state;
        if !previous.contains_key(&cursor) {
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
}

fn finalize_route_path(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    edge_indexes: Vec<usize>,
) -> RoutePath {
    let mut total_distance_m = 0_u64;
    let mut total_travel_time_s = 0.0;
    let mut total_generalized_cost = 0.0;
    for &edge_index in &edge_indexes {
        let edge = &topology.edges[edge_index];
        let metric = &metrics.edge_metrics[edge_index];
        total_distance_m += edge.length_m as u64;
        total_travel_time_s += metric.travel_time_s.unwrap_or_default();
        total_generalized_cost += metric.generalized_cost.unwrap_or_default();
    }

    RoutePath {
        edge_indexes,
        total_distance_m,
        total_travel_time_s,
        total_generalized_cost,
    }
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

fn snap_candidates(
    topology: &TopologyBundle,
    point: &LabeledPoint,
    eligible_nodes: &[bool],
    max_distance_m: f64,
) -> Result<Vec<SnappedPoint>> {
    const MAX_SNAP_CANDIDATES: usize = 8;

    if let Some(spatial_index) = topology.spatial_index.as_ref() {
        let candidates = spatial_snap_candidates(
            topology,
            spatial_index,
            point,
            eligible_nodes,
            max_distance_m,
        );
        if !candidates.is_empty() {
            return Ok(materialize_snap_candidates(
                topology,
                point,
                candidates,
                MAX_SNAP_CANDIDATES,
            ));
        }
    }

    let mut candidates: Vec<_> = topology
        .nodes
        .iter()
        .filter(|node| eligible_nodes[node.node_id.0 as usize])
        .map(|node| {
            (
                node,
                haversine_meters(point.lon, point.lat, node.lon, node.lat),
            )
        })
        .filter(|(_, distance_m)| *distance_m <= max_distance_m)
        .collect();

    candidates.sort_by(|(_, left), (_, right)| left.total_cmp(right));
    candidates.truncate(MAX_SNAP_CANDIDATES);

    if candidates.is_empty() {
        bail!(
            "point '{}' has no traversable candidate node within {:.1} m",
            point.id,
            max_distance_m
        );
    }

    Ok(materialize_snap_candidates(
        topology,
        point,
        candidates
            .into_iter()
            .map(|(node, snap_distance_m)| (node.node_id.0, snap_distance_m))
            .collect(),
        MAX_SNAP_CANDIDATES,
    ))
}

fn spatial_snap_candidates(
    topology: &TopologyBundle,
    spatial_index: &netan_core::NodeSpatialIndex,
    point: &LabeledPoint,
    eligible_nodes: &[bool],
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
    let mut candidates = Vec::new();

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
                if !eligible_nodes[node_id as usize] {
                    continue;
                }
                let node = &topology.nodes[node_id as usize];
                let distance_m = haversine_meters(point.lon, point.lat, node.lon, node.lat);
                if distance_m <= max_distance_m {
                    candidates.push((node_id, distance_m));
                }
            }
        }
    }

    candidates
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

fn materialize_snap_candidates(
    topology: &TopologyBundle,
    point: &LabeledPoint,
    mut candidates: Vec<(u32, f64)>,
    max_candidates: usize,
) -> Vec<SnappedPoint> {
    candidates.sort_by(|(_, left), (_, right)| left.total_cmp(right));
    candidates.truncate(max_candidates);
    candidates
        .into_iter()
        .map(|(node_id, snap_distance_m)| {
            let node = &topology.nodes[node_id as usize];
            SnappedPoint {
                point_id: point.id.clone(),
                requested_lon: point.lon,
                requested_lat: point.lat,
                snapped_node_id: node_id,
                snapped_lon: node.lon,
                snapped_lat: node.lat,
                snap_distance_m,
            }
        })
        .collect()
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
        RouteRequest, SnapOptions, execute_matrix, execute_od, execute_route, load_experiment,
        load_od_pairs, load_point_set,
    };
    use netan_core::{
        AccessMask, CacheBundleId, CompiledEdgeMetric, CompiledProfileBundle, DirectedEdge, EdgeId,
        NodeId, NodeSpatialIndex, RoadClass, SmoothnessClass, SpatialIndexCell, SurfaceClass,
        TopologyBounds, TopologyBundle, TopologyNode, TravelMode, TurnRestriction,
        TurnRestrictionKind,
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
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
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
            returns: ReturnConfig::default(),
        };

        let route = engine.execute_route(&request).expect("route succeeds");
        assert_eq!(route.edge_path, vec![0, 1]);
        assert_eq!(route.summary.total_distance_m, 300);
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
                .contains("no traversable candidate node within")
        );
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
            returns: ReturnConfig::default(),
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
            returns: ReturnConfig::default(),
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
        TopologyBundle {
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
            spatial_index: Some(spatial_index_for_all_nodes(3, 6.0, 53.0, 6.002, 53.0)),
        }
    }

    fn test_metrics() -> CompiledProfileBundle {
        CompiledProfileBundle {
            schema_version: 2,
            profile_id: "test".to_string(),
            profile_hash: "abc".to_string(),
            mode: TravelMode::Car,
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
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

    fn restricted_topology() -> TopologyBundle {
        TopologyBundle {
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
            spatial_index: Some(spatial_index_for_all_nodes(4, 6.0, 53.0, 6.003, 53.0)),
        }
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
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
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
