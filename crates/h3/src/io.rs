//! Cell tables as CSV and GeoJSON.

use anyhow::{Context, Result, bail};
use h3o::CellIndex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};

use crate::{cell_center, cell_ring, parse_cell};

pub(crate) fn serialize_cell<S: Serializer>(
    cell: &CellIndex,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(cell)
}

fn deserialize_cell<'de, D: Deserializer<'de>>(deserializer: D) -> Result<CellIndex, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse_cell(&text).map_err(serde::de::Error::custom)
}

fn default_weight() -> f64 {
    1.0
}

/// A cell with a weight such as a population or a POI count.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WeightedCell {
    #[serde(
        serialize_with = "serialize_cell",
        deserialize_with = "deserialize_cell"
    )]
    pub cell: CellIndex,
    #[serde(default = "default_weight")]
    pub weight: f64,
}

/// Reads a CSV with a `cell` column (or `h3`) and a weight column. Without
/// `weight_column` the `weight` column is used when present; rows weigh 1
/// otherwise. Repeated cells add up.
pub fn read_cells_csv(text: &str, weight_column: Option<&str>) -> Result<Vec<WeightedCell>> {
    let mut reader = csv::Reader::from_reader(text.as_bytes());
    let headers = reader.headers()?.clone();
    let position = |name: &str| headers.iter().position(|header| header.trim() == name);
    let cell_column = position("cell")
        .or_else(|| position("h3"))
        .context("cell CSV is missing the 'cell' column")?;
    let weight_index = match weight_column {
        Some(name) => Some(
            position(name).with_context(|| format!("cell CSV is missing the '{name}' column"))?,
        ),
        None => position("weight"),
    };
    let mut order = Vec::new();
    let mut weights = rustc_hash::FxHashMap::<CellIndex, f64>::default();
    for (row, record) in reader.records().enumerate() {
        let record = record?;
        let context = || format!("cell CSV row {}", row + 2);
        let cell = parse_cell(record.get(cell_column).unwrap_or("")).with_context(context)?;
        let weight = match weight_index {
            Some(index) => {
                let raw = record.get(index).unwrap_or("").trim();
                let weight = raw
                    .parse::<f64>()
                    .with_context(|| format!("invalid weight '{raw}'"))
                    .with_context(context)?;
                if !weight.is_finite() {
                    bail!("{}: weight must be finite", context());
                }
                weight
            }
            None => 1.0,
        };
        match weights.entry(cell) {
            std::collections::hash_map::Entry::Occupied(mut entry) => *entry.get_mut() += weight,
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(weight);
                order.push(cell);
            }
        }
    }
    Ok(order
        .into_iter()
        .map(|cell| WeightedCell {
            cell,
            weight: weights[&cell],
        })
        .collect())
}

/// A GeoJSON `FeatureCollection` with one hexagon per cell. Every feature
/// gets `cell`, `resolution`, `center_lon` and `center_lat` plus the given
/// properties.
pub fn cells_geojson(cells: impl IntoIterator<Item = (CellIndex, Map<String, Value>)>) -> Value {
    let features = cells
        .into_iter()
        .map(|(cell, extra)| {
            let center = cell_center(cell);
            let mut properties = Map::new();
            properties.insert("cell".into(), json!(cell.to_string()));
            properties.insert("resolution".into(), json!(u8::from(cell.resolution())));
            properties.insert("center_lon".into(), json!(center[0]));
            properties.insert("center_lat".into(), json!(center[1]));
            properties.extend(extra);
            json!({
                "type": "Feature",
                "geometry": {"type": "Polygon", "coordinates": [cell_ring(cell)]},
                "properties": properties,
            })
        })
        .collect::<Vec<_>>();
    json!({"type": "FeatureCollection", "features": features})
}
