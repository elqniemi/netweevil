use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoPackageMapping {
    #[serde(default = "default_mapping_version")]
    pub version: u32,
    /// GeoPackage ingest is intentionally projection-free. Input coordinates
    /// therefore have to be geographic longitude/latitude with Z in metres.
    #[serde(default)]
    pub coordinates: CoordinateMapping,
    #[serde(default)]
    pub retain: RetainMode,
    #[serde(default)]
    pub quantization: CoordinateQuantization,
    pub layers: Vec<GeoPackageLayerMapping>,
    /// Features listed by a scenario direction override must have both edge
    /// directions available for runtime gating.
    #[serde(default)]
    pub materialize_both_directions_for: BTreeSet<i64>,
}

fn default_mapping_version() -> u32 {
    1
}

impl GeoPackageMapping {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!(
                "unsupported GeoPackage mapping version {}; expected 1",
                self.version
            );
        }
        if self.layers.is_empty() {
            bail!("GeoPackage mapping must contain at least one layer");
        }
        if !matches!(self.retain, RetainMode::All) {
            bail!("GeoPackage mapping currently requires `retain: all` for lossless imports");
        }
        if self.quantization.xy_degrees <= 0.0 || self.quantization.z_m <= 0.0 {
            bail!("GeoPackage coordinate quantization values must be greater than zero");
        }
        if !self.coordinates.geographic_wgs84 {
            bail!(
                "GeoPackage coordinates must be pre-reprojected to WGS84 longitude/latitude; netweevil does not reproject source data"
            );
        }

        let mut tables = BTreeSet::new();
        for layer in &self.layers {
            if layer.table.trim().is_empty() {
                bail!("GeoPackage layer table must not be empty");
            }
            let key = (
                layer.source.clone().unwrap_or_default(),
                layer.table.to_ascii_lowercase(),
            );
            if !tables.insert(key) {
                bail!(
                    "duplicate GeoPackage layer mapping for table '{}'",
                    layer.table
                );
            }
            if layer.defaults.access.is_empty() {
                bail!(
                    "GeoPackage layer '{}' has an empty defaults.access list; lossless routable ingest requires at least one access mode",
                    layer.table
                );
            }
            let roles = layer
                .fields
                .values()
                .map(|field| field.role().to_ascii_lowercase())
                .collect::<Vec<_>>();
            if !roles.iter().any(|role| role == "feature_id") {
                bail!(
                    "GeoPackage layer '{}' has no `feature_id` field role",
                    layer.table
                );
            }
            if roles.iter().filter(|role| *role == "feature_id").count() > 1 {
                bail!(
                    "GeoPackage layer '{}' has more than one `feature_id` role",
                    layer.table
                );
            }
        }
        Ok(())
    }
}

pub fn load_gpkg_mapping(path: impl AsRef<Path>) -> Result<GeoPackageMapping> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading GeoPackage mapping {}", path.display()))?;
    let mapping: GeoPackageMapping = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("json") => serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing GeoPackage mapping {}", path.display()))?,
        _ => serde_yaml::from_slice(&bytes)
            .with_context(|| format!("parsing GeoPackage mapping {}", path.display()))?,
    };
    mapping.validate()?;
    Ok(mapping)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinateMapping {
    #[serde(default = "default_true")]
    pub geographic_wgs84: bool,
    #[serde(default = "default_true")]
    pub z_metres: bool,
}

impl Default for CoordinateMapping {
    fn default() -> Self {
        Self {
            geographic_wgs84: true,
            z_metres: true,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RetainMode {
    #[default]
    All,
    Unmapped,
    None,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CoordinateQuantization {
    #[serde(default = "default_xy_quantization")]
    pub xy_degrees: f64,
    #[serde(default = "default_z_quantization")]
    pub z_m: f64,
}

impl Default for CoordinateQuantization {
    fn default() -> Self {
        Self {
            xy_degrees: default_xy_quantization(),
            z_m: default_z_quantization(),
        }
    }
}

fn default_xy_quantization() -> f64 {
    1.0e-9
}

fn default_z_quantization() -> f64 {
    0.01
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoPackageLayerMapping {
    /// Optional source file name/path suffix. Useful when multiple inputs
    /// contain tables with the same name.
    #[serde(default)]
    pub source: Option<String>,
    pub table: String,
    #[serde(default)]
    pub geometry_column: Option<String>,
    #[serde(default)]
    pub fields: BTreeMap<String, FieldMapping>,
    #[serde(default)]
    pub defaults: LayerDefaults,
}

impl GeoPackageLayerMapping {
    pub fn field_for_role(&self, role: &str) -> Option<(&str, &FieldMapping)> {
        self.fields
            .iter()
            .find(|(_, mapping)| mapping.role().eq_ignore_ascii_case(role))
            .map(|(name, mapping)| (name.as_str(), mapping))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldMapping {
    Role(String),
    Detailed(FieldMappingDetail),
}

impl FieldMapping {
    pub fn role(&self) -> &str {
        match self {
            Self::Role(role) => role,
            Self::Detailed(mapping) => &mapping.role,
        }
    }

    pub fn decode(&self) -> Option<&BTreeMap<String, String>> {
        match self {
            Self::Role(_) => None,
            Self::Detailed(mapping) => Some(&mapping.decode),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldMappingDetail {
    pub role: String,
    #[serde(default)]
    pub decode: BTreeMap<String, String>,
    /// Optional GeoPackage coded-value constraint name override.
    #[serde(default)]
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerDefaults {
    #[serde(default = "default_highway")]
    pub highway: String,
    #[serde(default = "default_access")]
    pub access: Vec<String>,
    #[serde(default)]
    pub direction: DirectionValue,
}

impl Default for LayerDefaults {
    fn default() -> Self {
        Self {
            highway: default_highway(),
            access: default_access(),
            direction: DirectionValue::Both,
        }
    }
}

fn default_highway() -> String {
    "footway".to_string()
}

fn default_access() -> Vec<String> {
    vec!["foot".to_string()]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DirectionValue {
    #[default]
    Both,
    Forward,
    Reverse,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compact_and_detailed_field_mappings() {
        let mapping: GeoPackageMapping = serde_yaml::from_str(
            r#"
retain: all
layers:
  - table: walkways
    fields:
      id: feature_id
      direction:
        role: direction
        decode: {"-1": reverse, "0": both, "1": forward}
"#,
        )
        .expect("mapping");
        mapping.validate().expect("valid mapping");
        assert_eq!(mapping.layers[0].fields["id"].role(), "feature_id");
        assert_eq!(
            mapping.layers[0].fields["direction"]
                .decode()
                .and_then(|decode| decode.get("-1"))
                .map(String::as_str),
            Some("reverse")
        );
    }

    #[test]
    fn rejects_a_layer_that_would_materialize_no_edges() {
        let mapping: GeoPackageMapping = serde_yaml::from_str(
            r#"
retain: all
layers:
  - table: walkways
    defaults: {access: []}
    fields: {id: feature_id}
"#,
        )
        .expect("mapping parses");
        let error = mapping
            .validate()
            .expect_err("empty access would make geometry unauditable");
        assert!(error.to_string().contains("empty defaults.access"));
    }
}
