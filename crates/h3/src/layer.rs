//! Stored cell layers: named values per cell, such as population, jobs,
//! ride demand and vehicle supply. A layer is filled by importing cell or
//! point tables with one aggregation per column, and edited by painting
//! values onto cells.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use h3o::{CellIndex, Resolution};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::io::WeightedCell;
use crate::{cell_at, cells_geojson, parse_cell};

/// How repeated values of one cell combine into one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aggregation {
    #[default]
    Sum,
    Mean,
    Min,
    Max,
    /// Number of rows with a value.
    Count,
    /// The value of the last row.
    Last,
}

impl std::str::FromStr for Aggregation {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        Ok(match text.trim() {
            "sum" => Self::Sum,
            "mean" | "avg" => Self::Mean,
            "min" => Self::Min,
            "max" => Self::Max,
            "count" => Self::Count,
            "last" => Self::Last,
            other => bail!("unknown aggregation '{other}'; use sum, mean, min, max, count or last"),
        })
    }
}

/// One layer column filled from a source column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnSpec {
    /// Column of the imported table. Empty counts rows.
    #[serde(default)]
    pub source: String,
    /// Layer column; defaults to `source`.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub aggregation: Aggregation,
}

impl ColumnSpec {
    /// Parses `source`, `source:aggregation` or `source:aggregation=name`.
    /// `:count=name` counts rows.
    pub fn parse(text: &str) -> Result<Self> {
        let (rest, name) = match text.split_once('=') {
            Some((rest, name)) => (rest, Some(name.trim())),
            None => (text, None),
        };
        let (source, aggregation) = match rest.split_once(':') {
            Some((source, aggregation)) => (source.trim(), aggregation.parse()?),
            None => (rest.trim(), Aggregation::Sum),
        };
        let spec = Self {
            source: source.to_string(),
            name: name.unwrap_or(source).to_string(),
            aggregation,
        };
        if spec.layer_column().is_empty() {
            bail!("column spec '{text}' needs a column name");
        }
        if spec.source.is_empty() && spec.aggregation != Aggregation::Count {
            bail!("column spec '{text}' needs a source column");
        }
        Ok(spec)
    }

    /// Name of the layer column.
    pub fn layer_column(&self) -> &str {
        if self.name.is_empty() {
            &self.source
        } else {
            &self.name
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Accumulator {
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
    last: f64,
}

impl Accumulator {
    const EMPTY: Self = Self {
        count: 0,
        sum: 0.0,
        min: f64::INFINITY,
        max: f64::NEG_INFINITY,
        last: f64::NAN,
    };

    fn add(&mut self, value: f64) {
        self.count += 1;
        self.sum += value;
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        self.last = value;
    }

    fn finish(&self, aggregation: Aggregation) -> Option<f64> {
        if self.count == 0 {
            return None;
        }
        Some(match aggregation {
            Aggregation::Sum => self.sum,
            Aggregation::Mean => self.sum / self.count as f64,
            Aggregation::Min => self.min,
            Aggregation::Max => self.max,
            Aggregation::Count => self.count as f64,
            Aggregation::Last => self.last,
        })
    }
}

/// How a painted value changes a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaintOp {
    #[default]
    Set,
    /// Adds to the value; a cell without a value starts at zero.
    Add,
    /// Multiplies the value; a cell without a value stays without one.
    Multiply,
    /// Removes the value of the column.
    Clear,
}

/// Statistics of one layer column.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ColumnSummary {
    pub name: String,
    /// Cells with a value.
    pub count: usize,
    pub sum: f64,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CellLayerSummary {
    pub layer_id: String,
    pub label: String,
    pub description: String,
    pub cell_count: usize,
    /// Resolutions in use, coarse to fine.
    pub resolutions: Vec<u8>,
    pub columns: Vec<ColumnSummary>,
}

mod cell_rows {
    use super::*;

    pub fn serialize<S: serde::Serializer>(
        rows: &BTreeMap<CellIndex, Vec<Option<f64>>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_map(rows.iter().map(|(cell, values)| (cell.to_string(), values)))
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<CellIndex, Vec<Option<f64>>>, D::Error> {
        BTreeMap::<String, Vec<Option<f64>>>::deserialize(deserializer)?
            .into_iter()
            .map(|(cell, values)| {
                Ok((parse_cell(&cell).map_err(serde::de::Error::custom)?, values))
            })
            .collect()
    }
}

/// Cells with one optional value per column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellLayer {
    pub layer_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    pub columns: Vec<String>,
    /// Cell id to one value per column; `null` is no value.
    #[serde(with = "cell_rows")]
    pub cells: BTreeMap<CellIndex, Vec<Option<f64>>>,
}

/// Checks a layer id: ASCII letters, digits, `_` and `-`.
pub fn validate_layer_id(layer_id: &str) -> Result<()> {
    if layer_id.is_empty()
        || !layer_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("layer id '{layer_id}' must be non-empty ASCII letters, digits, '_' or '-'");
    }
    Ok(())
}

impl CellLayer {
    pub fn new(layer_id: &str) -> Result<Self> {
        validate_layer_id(layer_id)?;
        Ok(Self {
            layer_id: layer_id.to_string(),
            label: String::new(),
            description: String::new(),
            columns: Vec::new(),
            cells: BTreeMap::new(),
        })
    }

    /// Checks the id, the column names and the row widths.
    pub fn validate(&self) -> Result<()> {
        validate_layer_id(&self.layer_id)?;
        for (index, column) in self.columns.iter().enumerate() {
            if column.trim().is_empty() || matches!(column.as_str(), "cell" | "resolution") {
                bail!("'{column}' is not a valid layer column name");
            }
            if self.columns[..index].contains(column) {
                bail!("layer column '{column}' is defined more than once");
            }
        }
        if let Some((cell, _)) = self
            .cells
            .iter()
            .find(|(_, values)| values.len() != self.columns.len())
        {
            bail!("cell {cell} does not have one value per column");
        }
        if self
            .cells
            .values()
            .flatten()
            .any(|value| value.is_some_and(|value| !value.is_finite()))
        {
            bail!("layer values must be finite");
        }
        Ok(())
    }

    pub fn column_index(&self, column: &str) -> Option<usize> {
        self.columns.iter().position(|name| name == column)
    }

    /// Index of a column, added when the layer does not have it.
    pub fn ensure_column(&mut self, column: &str) -> Result<usize> {
        if let Some(index) = self.column_index(column) {
            return Ok(index);
        }
        if column.trim().is_empty() || matches!(column, "cell" | "resolution") {
            bail!("'{column}' is not a valid layer column name");
        }
        self.columns.push(column.to_string());
        for values in self.cells.values_mut() {
            values.push(None);
        }
        Ok(self.columns.len() - 1)
    }

    pub fn value(&self, cell: CellIndex, column: &str) -> Option<f64> {
        let index = self.column_index(column)?;
        *self.cells.get(&cell)?.get(index)?
    }

    /// Paints `value` onto cells of one column; a cell listed more than
    /// once is painted once. Cells left without any value are removed.
    /// Returns the number of cells changed.
    pub fn paint(
        &mut self,
        cells: impl IntoIterator<Item = CellIndex>,
        column: &str,
        op: PaintOp,
        value: f64,
    ) -> Result<usize> {
        if op != PaintOp::Clear && !value.is_finite() {
            bail!("the painted value must be finite");
        }
        let index = match (op, self.column_index(column)) {
            (PaintOp::Clear | PaintOp::Multiply, None) => return Ok(0),
            _ => self.ensure_column(column)?,
        };
        let width = self.columns.len();
        let mut changed = 0;
        for cell in cells.into_iter().collect::<std::collections::BTreeSet<_>>() {
            let current = self.cells.get(&cell).and_then(|values| values[index]);
            let next = match op {
                PaintOp::Set => Some(value),
                PaintOp::Add => Some(current.unwrap_or(0.0) + value),
                PaintOp::Multiply => current.map(|current| current * value),
                PaintOp::Clear => None,
            };
            if next == current {
                continue;
            }
            changed += 1;
            let row = self.cells.entry(cell).or_insert_with(|| vec![None; width]);
            row[index] = next;
            if row.iter().all(Option::is_none) {
                self.cells.remove(&cell);
            }
        }
        Ok(changed)
    }

    /// Removes a column and the cells left without any value.
    pub fn remove_column(&mut self, column: &str) -> bool {
        let Some(index) = self.column_index(column) else {
            return false;
        };
        self.columns.remove(index);
        self.cells.retain(|_, values| {
            values.remove(index);
            values.iter().any(Option::is_some)
        });
        true
    }

    /// The cells that have a value in `column`, as weights.
    pub fn weighted(&self, column: &str) -> Result<Vec<WeightedCell>> {
        let index = self.column_index(column).with_context(|| {
            format!(
                "layer '{}' has no column '{column}' (columns: {})",
                self.layer_id,
                self.columns.join(", ")
            )
        })?;
        Ok(self
            .cells
            .iter()
            .filter_map(|(cell, values)| {
                values[index].map(|weight| WeightedCell {
                    cell: *cell,
                    weight,
                })
            })
            .collect())
    }

    pub fn summary(&self) -> CellLayerSummary {
        let mut resolutions = self
            .cells
            .keys()
            .map(|cell| u8::from(cell.resolution()))
            .collect::<Vec<_>>();
        resolutions.sort_unstable();
        resolutions.dedup();
        let columns = self
            .columns
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let mut accumulator = Accumulator::EMPTY;
                for value in self.cells.values().filter_map(|values| values[index]) {
                    accumulator.add(value);
                }
                ColumnSummary {
                    name: name.clone(),
                    count: accumulator.count as usize,
                    sum: accumulator.sum,
                    min: accumulator.finish(Aggregation::Min),
                    max: accumulator.finish(Aggregation::Max),
                }
            })
            .collect();
        CellLayerSummary {
            layer_id: self.layer_id.clone(),
            label: self.label.clone(),
            description: self.description.clone(),
            cell_count: self.cells.len(),
            resolutions,
            columns,
        }
    }

    fn properties(&self, values: &[Option<f64>]) -> Map<String, Value> {
        self.columns
            .iter()
            .zip(values)
            .filter_map(|(column, value)| Some((column.clone(), json!((*value)?))))
            .collect()
    }

    /// One hexagon per cell with the column values as properties.
    pub fn geojson(&self) -> Value {
        cells_geojson(
            self.cells
                .iter()
                .map(|(cell, values)| (*cell, self.properties(values))),
        )
    }

    /// CSV with `cell`, `resolution` and one column per layer column.
    pub fn to_csv(&self) -> Result<String> {
        let mut writer = csv::Writer::from_writer(Vec::new());
        writer.write_record(
            ["cell", "resolution"]
                .into_iter()
                .chain(self.columns.iter().map(String::as_str)),
        )?;
        for (cell, values) in &self.cells {
            let mut record = vec![cell.to_string(), u8::from(cell.resolution()).to_string()];
            record.extend(
                values
                    .iter()
                    .map(|value| value.map(|value| value.to_string()).unwrap_or_default()),
            );
            writer.write_record(record)?;
        }
        Ok(String::from_utf8(writer.into_inner()?)?)
    }

    /// Merges aggregated rows into the layer; a merged value replaces the
    /// value the cell had.
    pub fn merge(&mut self, builder: LayerBuilder) -> Result<()> {
        let indexes = builder
            .specs
            .iter()
            .map(|spec| self.ensure_column(spec.layer_column()))
            .collect::<Result<Vec<_>>>()?;
        let width = self.columns.len();
        for (cell, accumulators) in builder.cells {
            let row = self.cells.entry(cell).or_insert_with(|| vec![None; width]);
            for ((accumulator, spec), index) in
                accumulators.iter().zip(&builder.specs).zip(&indexes)
            {
                if let Some(value) = accumulator.finish(spec.aggregation) {
                    row[*index] = Some(value);
                }
            }
            if row.iter().all(Option::is_none) {
                self.cells.remove(&cell);
            }
        }
        self.validate()
    }

    pub fn path(dir: &Path, layer_id: &str) -> PathBuf {
        dir.join(format!("{layer_id}.json"))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        self.validate()?;
        std::fs::create_dir_all(dir)?;
        let path = Self::path(dir, &self.layer_id);
        std::fs::write(&path, serde_json::to_vec(self)?)
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn load(dir: &Path, layer_id: &str) -> Result<Self> {
        validate_layer_id(layer_id)?;
        let path = Self::path(dir, layer_id);
        let bytes = std::fs::read(&path).with_context(|| format!("no cell layer '{layer_id}'"))?;
        let layer: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", path.display()))?;
        layer.validate()?;
        Ok(layer)
    }

    /// Summaries of the stored layers, ordered by id.
    pub fn list(dir: &Path) -> Result<Vec<CellLayerSummary>> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(Vec::new());
        };
        let mut ids = entries
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                (path.extension()? == "json")
                    .then(|| path.file_stem()?.to_str().map(String::from))?
            })
            .collect::<Vec<_>>();
        ids.sort();
        ids.iter()
            .map(|id| Ok(Self::load(dir, id)?.summary()))
            .collect()
    }

    /// Removes a stored layer; `false` when there was none.
    pub fn remove(dir: &Path, layer_id: &str) -> Result<bool> {
        validate_layer_id(layer_id)?;
        match std::fs::remove_file(Self::path(dir, layer_id)) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

/// Aggregates table rows per cell, one accumulator per column spec.
#[derive(Debug, Clone)]
pub struct LayerBuilder {
    specs: Vec<ColumnSpec>,
    cells: BTreeMap<CellIndex, Vec<Accumulator>>,
}

impl LayerBuilder {
    pub fn new(specs: Vec<ColumnSpec>) -> Result<Self> {
        if specs.is_empty() {
            bail!("at least one column is required");
        }
        Ok(Self {
            specs,
            cells: BTreeMap::new(),
        })
    }

    pub fn specs(&self) -> &[ColumnSpec] {
        &self.specs
    }

    /// Adds one row: one optional value per spec. A `Count` spec without a
    /// source counts every row.
    pub fn add(&mut self, cell: CellIndex, values: &[Option<f64>]) {
        let row = self
            .cells
            .entry(cell)
            .or_insert_with(|| vec![Accumulator::EMPTY; self.specs.len()]);
        for ((accumulator, spec), value) in row.iter_mut().zip(&self.specs).zip(values) {
            if spec.source.is_empty() {
                accumulator.add(1.0);
            } else if let Some(value) = value.filter(|value| value.is_finite()) {
                accumulator.add(value);
            }
        }
    }

    /// Reads a CSV. With `resolution`, rows are points (`lon_column`,
    /// `lat_column`) aggregated into cells of that resolution; without it,
    /// rows carry a `cell` (or `h3`) column. Empty and non-numeric values
    /// count as missing. Returns the number of rows read and skipped.
    pub fn add_csv(
        &mut self,
        text: &str,
        resolution: Option<Resolution>,
        lon_column: &str,
        lat_column: &str,
    ) -> Result<(usize, usize)> {
        let mut reader = csv::Reader::from_reader(text.as_bytes());
        let headers = reader.headers()?.clone();
        let position = |name: &str| headers.iter().position(|header| header.trim() == name);
        let required =
            |name: &str| position(name).with_context(|| format!("CSV has no '{name}' column"));
        let locate = match resolution {
            Some(_) => (required(lon_column)?, required(lat_column)?),
            None => {
                let cell = position("cell")
                    .or_else(|| position("h3"))
                    .context("CSV has no 'cell' column; pass a resolution to import points")?;
                (cell, cell)
            }
        };
        let sources = self
            .specs
            .iter()
            .map(|spec| match spec.source.as_str() {
                "" => Ok(None),
                source => required(source).map(Some),
            })
            .collect::<Result<Vec<_>>>()?;
        let (mut rows, mut skipped) = (0, 0);
        let mut values = vec![None; self.specs.len()];
        for (row, record) in reader.records().enumerate() {
            let record = record?;
            let number = |index: usize| record.get(index)?.trim().parse::<f64>().ok();
            let cell = match resolution {
                Some(resolution) => number(locate.0)
                    .zip(number(locate.1))
                    .and_then(|(lon, lat)| cell_at(lon, lat, resolution)),
                None => Some(
                    parse_cell(record.get(locate.0).unwrap_or(""))
                        .with_context(|| format!("CSV row {}", row + 2))?,
                ),
            };
            let Some(cell) = cell else {
                skipped += 1;
                continue;
            };
            for (value, source) in values.iter_mut().zip(&sources) {
                *value = source.and_then(number);
            }
            self.add(cell, &values);
            rows += 1;
        }
        Ok((rows, skipped))
    }
}

/// Column specs for every numeric-looking column of a CSV header, summed.
/// The cell and coordinate columns are left out.
pub fn default_column_specs(text: &str, skip: &[&str]) -> Result<Vec<ColumnSpec>> {
    let mut reader = csv::Reader::from_reader(text.as_bytes());
    let headers = reader.headers()?.clone();
    let first = reader.records().next().transpose()?;
    Ok(headers
        .iter()
        .enumerate()
        .filter(|(index, header)| {
            let header = header.trim();
            !matches!(header, "cell" | "h3" | "resolution" | "id" | "")
                && !skip.contains(&header)
                && first.as_ref().is_none_or(|record| {
                    record.get(*index).is_some_and(|value| {
                        value.trim().is_empty() || value.trim().parse::<f64>().is_ok()
                    })
                })
        })
        .map(|(_, header)| ColumnSpec {
            source: header.trim().to_string(),
            name: String::new(),
            aggregation: Aggregation::Sum,
        })
        .collect())
}
