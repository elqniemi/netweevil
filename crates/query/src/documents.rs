use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use netweevil_profile::ReturnConfig;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::*;

/// Reads a JSON/YAML request document from a local file (CLI side) and
/// inlines the local files it references (see [`inline_file_references`]).
pub fn read_request_document<T: DeserializeOwned>(path: &Path, label: &str) -> Result<T> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("reading {label} {}", path.display()))?;
    let mut value: Value = match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => {
            serde_json::from_str(&raw).with_context(|| format!("parsing JSON {label}"))?
        }
        Some("yaml") | Some("yml") => {
            serde_yaml::from_str(&raw).with_context(|| format!("parsing YAML {label}"))?
        }
        other => bail!(
            "unsupported {label} extension {:?}; use .json, .yml, or .yaml",
            other
        ),
    };
    inline_file_references(&mut value, path.parent().unwrap_or_else(|| Path::new(".")))
        .with_context(|| format!("inlining files referenced by {label} {}", path.display()))?;
    serde_json::from_value(value).with_context(|| format!("parsing {label} {}", path.display()))
}

/// Request documents on disk may name local files that the API never reads.
/// This replaces every such reference, anywhere in the document, with the
/// file's content; relative paths resolve against `base` (the document's
/// directory):
///
/// - `scenario_file` -> `scenario` (scenario overlay, JSON/YAML)
/// - `holiday_calendar_file` -> appended to `holidays` (JSON/YAML dates)
/// - `overlay_files` -> appended to `overlays` (CSV/Parquet temporal overlays)
/// - `overlay_file` -> `overlay` (scenario-batch case overlay, JSON/YAML)
/// - `ranking_file` -> `ranking` (scenario-batch top-k feature scores)
pub fn inline_file_references(value: &mut Value, base: &Path) -> Result<()> {
    match value {
        Value::Array(items) => {
            for item in items {
                inline_file_references(item, base)?;
            }
        }
        Value::Object(object) => {
            for item in object.values_mut() {
                inline_file_references(item, base)?;
            }
            let path_value = |value: Value, key: &str| -> Result<std::path::PathBuf> {
                let Value::String(path) = value else {
                    bail!("'{key}' must be a file path string");
                };
                Ok(base.join(path))
            };
            if let Some(path) = object.remove("scenario_file") {
                let path = path_value(path, "scenario_file")?;
                object.insert(
                    "scenario".into(),
                    serde_json::to_value(load_scenario_overlay(&path)?)?,
                );
            }
            if let Some(path) = object.remove("holiday_calendar_file") {
                let path = path_value(path, "holiday_calendar_file")?;
                let dates = load_holiday_calendar(&path)?;
                append_array(object, "holidays", dates.into_iter().map(Value::String))?;
            }
            if let Some(paths) = object.remove("overlay_files") {
                let Value::Array(paths) = paths else {
                    bail!("'overlay_files' must be a list of file paths");
                };
                let overlays = paths
                    .into_iter()
                    .map(|path| {
                        let path = path_value(path, "overlay_files")?;
                        Ok(serde_json::to_value(load_temporal_overlay(&path)?)?)
                    })
                    .collect::<Result<Vec<_>>>()?;
                append_array(object, "overlays", overlays)?;
            }
            if let Some(path) = object.remove("overlay_file") {
                let path = path_value(path, "overlay_file")?;
                object.insert(
                    "overlay".into(),
                    serde_json::to_value(load_scenario_overlay(&path)?)?,
                );
            }
            if let Some(path) = object.remove("ranking_file") {
                let path = path_value(path, "ranking_file")?;
                object.insert(
                    "ranking".into(),
                    serde_json::to_value(load_scenario_feature_ranking(&path)?)?,
                );
            }
        }
        _ => {}
    }
    Ok(())
}

fn append_array(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    values: impl IntoIterator<Item = Value>,
) -> Result<()> {
    match object
        .entry(key)
        .or_insert_with(|| Value::Array(Vec::new()))
    {
        Value::Array(items) => {
            items.extend(values);
            Ok(())
        }
        _ => bail!("'{key}' must be a list"),
    }
}

pub fn load_route_request(path: impl AsRef<Path>) -> Result<RouteRequest> {
    read_request_document(path.as_ref(), "route request")
}

pub fn load_trip_request(path: impl AsRef<Path>) -> Result<TripRequest> {
    read_request_document(path.as_ref(), "trip request")
}

pub fn load_route_batch(path: impl AsRef<Path>) -> Result<RouteBatchDocument> {
    read_request_document(path.as_ref(), "route batch")
}

pub fn load_service_area_request(path: impl AsRef<Path>) -> Result<ServiceAreaRequest> {
    validate_service_area_request(read_request_document(
        path.as_ref(),
        "service-area request",
    )?)
}

pub fn load_service_area_sequence_request(
    path: impl AsRef<Path>,
) -> Result<ServiceAreaSequenceRequest> {
    let request: ServiceAreaSequenceRequest =
        read_request_document(path.as_ref(), "service-area sequence request")?;
    request.validate_timing()?;
    Ok(request)
}

pub fn load_betweenness_request(path: impl AsRef<Path>) -> Result<BetweennessRequest> {
    read_request_document(path.as_ref(), "betweenness request")
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
    #[serde(default, flatten)]
    pub temporal: TemporalRequestOptions,
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
    #[serde(default, flatten)]
    pub temporal: TemporalRequestOptions,
}

pub fn load_od_pairs(path: impl AsRef<Path>) -> Result<OdPairsDocument> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading OD pairs file {}", path.display()))?;
    let document = match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") | Some("yaml") | Some("yml") => read_request_document(path, "OD pairs")?,
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
        Some("json") | Some("yaml") | Some("yml") => read_request_document(path, "point set")?,
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
    let header = rows.next().context(
        "OD CSV must start with a header row: id,source_lon,source_lat,target_lon,target_lat",
    )?;
    let id_index = header_index(&header, "id")?;
    let source_lon_index = header_index(&header, "source_lon")?;
    let source_lat_index = header_index(&header, "source_lat")?;
    let target_lon_index = header_index(&header, "target_lon")?;
    let target_lat_index = header_index(&header, "target_lat")?;
    let source_z_index = optional_header_index(&header, "source_z");
    let target_z_index = optional_header_index(&header, "target_z");

    let mut pairs = Vec::new();
    for (row_number, row) in rows.enumerate() {
        let pair_id = csv_field(&row, id_index, row_number + 2, "id")?.to_string();
        let source_lon = parse_csv_f64(&row, source_lon_index, row_number + 2, "source_lon")?;
        let source_lat = parse_csv_f64(&row, source_lat_index, row_number + 2, "source_lat")?;
        let target_lon = parse_csv_f64(&row, target_lon_index, row_number + 2, "target_lon")?;
        let target_lat = parse_csv_f64(&row, target_lat_index, row_number + 2, "target_lat")?;

        pairs.push(OdPair {
            pair_id: pair_id.clone(),
            origin: LabeledPoint {
                id: format!("{pair_id}:source"),
                lon: source_lon,
                lat: source_lat,
                z: parse_optional_csv_f64(&row, source_z_index, row_number + 2, "source_z")?,
            },
            destination: LabeledPoint {
                id: format!("{pair_id}:target"),
                lon: target_lon,
                lat: target_lat,
                z: parse_optional_csv_f64(&row, target_z_index, row_number + 2, "target_z")?,
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
        temporal: TemporalRequestOptions::default(),
    })
}

fn load_point_set_csv(raw: &str) -> Result<PointSetDocument> {
    let mut rows = csv_rows(raw);
    let header = rows
        .next()
        .context("point-set CSV must start with a header row: id,lon,lat")?;
    let id_index = header_index(&header, "id")?;
    let x_index = header_index(&header, "lon")?;
    let y_index = header_index(&header, "lat")?;
    let z_index = optional_header_index(&header, "z");

    let mut points = Vec::new();
    for (row_number, row) in rows.enumerate() {
        points.push(LabeledPoint {
            id: csv_field(&row, id_index, row_number + 2, "id")?.to_string(),
            lon: parse_csv_f64(&row, x_index, row_number + 2, "lon")?,
            lat: parse_csv_f64(&row, y_index, row_number + 2, "lat")?,
            z: parse_optional_csv_f64(&row, z_index, row_number + 2, "z")?,
        });
    }

    Ok(PointSetDocument {
        points,
        snap: SnapOptions::default(),
        connectivity: ConnectivityPolicy::default(),
        fallback: FallbackPolicy::default(),
        returns: ReturnConfig::default(),
        alternatives: AlternativeRouteOptions::default(),
        temporal: TemporalRequestOptions::default(),
    })
}

fn header_index(header: &[String], column: &str) -> Result<usize> {
    optional_header_index(header, column)
        .with_context(|| format!("missing required CSV column '{column}'"))
}

fn optional_header_index(header: &[String], column: &str) -> Option<usize> {
    header.iter().position(|value| value == column)
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

fn parse_optional_csv_f64(
    row: &[String],
    index: Option<usize>,
    row_number: usize,
    column_name: &str,
) -> Result<Option<f64>> {
    let Some(index) = index else {
        return Ok(None);
    };
    let Some(value) = row.get(index).map(|value| value.trim()) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    value.parse::<f64>().map(Some).with_context(|| {
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
    merged.admin_areas |= right.admin_areas;
    merged.elevation |= right.elevation;
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
