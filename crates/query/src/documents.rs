use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use netweevil_profile::ReturnConfig;
use serde::{Deserialize, Serialize};

use crate::*;

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

pub fn load_route_batch(path: impl AsRef<Path>) -> Result<RouteBatchDocument> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading route batch {}", path.display()))?;
    let document = match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => serde_json::from_str(&raw).context("parsing JSON route batch")?,
        Some("yaml") | Some("yml") => {
            serde_yaml::from_str(&raw).context("parsing YAML route batch")?
        }
        other => bail!(
            "unsupported route batch extension {:?}; use .json, .yml, or .yaml",
            other
        ),
    };
    Ok(document)
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
pub struct RouteBatchEntry {
    #[serde(default)]
    pub profile_id: Option<String>,
    pub request: RouteRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteBatchDocument {
    #[serde(default)]
    pub requests: Vec<RouteBatchEntry>,
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
    #[serde(default)]
    pub alternatives: AlternativeRouteOptions,
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
    #[serde(default)]
    pub alternatives: AlternativeRouteOptions,
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
        alternatives: AlternativeRouteOptions::default(),
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
        alternatives: AlternativeRouteOptions::default(),
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
            alternatives: AlternativeRouteOptions::default(),
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
            alternatives: AlternativeRouteOptions::default(),
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

pub(crate) fn merge_point_set_returns(left: &ReturnConfig, right: &ReturnConfig) -> ReturnConfig {
    let mut merged = left.clone();
    if matches!(merged.geometry, netweevil_profile::ReturnGeometry::None) {
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

pub(crate) fn merge_point_set_connectivity_policy(
    left: &ConnectivityPolicy,
    right: &ConnectivityPolicy,
) -> ConnectivityPolicy {
    if *left == ConnectivityPolicy::default() {
        right.clone()
    } else {
        left.clone()
    }
}

pub(crate) fn merge_point_set_fallback_policy(
    left: &FallbackPolicy,
    right: &FallbackPolicy,
) -> FallbackPolicy {
    if *left == FallbackPolicy::default() {
        right.clone()
    } else {
        left.clone()
    }
}

pub(crate) fn merge_point_set_alternatives(
    left: &AlternativeRouteOptions,
    right: &AlternativeRouteOptions,
) -> AlternativeRouteOptions {
    if *left == AlternativeRouteOptions::default() {
        right.clone()
    } else {
        left.clone()
    }
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
