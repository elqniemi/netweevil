use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{Field, Schema};
use netweevil_query::{AnalysisOutcome, BatchItemStatus};
use parquet::arrow::ArrowWriter;
use serde::Serialize;
use serde_json::json;

pub(super) fn write_csv<S: AsRef<str>>(
    path: &Path,
    header: &[S],
    rows: Vec<Vec<String>>,
) -> Result<()> {
    ensure_parent_dir(path)?;
    let mut output = String::new();
    push_csv_row(
        &mut output,
        &header
            .iter()
            .map(|value| value.as_ref().to_string())
            .collect::<Vec<_>>(),
    );
    for row in rows {
        push_csv_row(&mut output, &row);
    }
    fs::write(path, output).with_context(|| format!("writing {}", path.display()))
}

/// A stable mapping from a profile component name to its flat export column.
///
/// Component maps remain nested in JSON/GeoJSON. Tabular and GIS formats use
/// these columns so the values remain numeric and are easy to style or query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ComponentColumn {
    pub(super) component_name: String,
    pub(super) column_name: String,
}

pub(super) fn component_columns<'a>(
    maps: impl IntoIterator<Item = &'a BTreeMap<String, f64>>,
) -> Vec<ComponentColumn> {
    let names = maps
        .into_iter()
        .flat_map(BTreeMap::keys)
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut used = BTreeSet::new();

    names
        .into_iter()
        .map(|component_name| {
            let base = format!("component_{}", sanitize_component_name(&component_name));
            let mut column_name = base.clone();
            let mut suffix = 2_u32;
            while !used.insert(column_name.clone()) {
                column_name = format!("{base}_{suffix}");
                suffix += 1;
            }
            ComponentColumn {
                component_name,
                column_name,
            }
        })
        .collect()
}

fn sanitize_component_name(name: &str) -> String {
    let mut output = String::new();
    let mut pending_separator = false;
    for character in name.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            if pending_separator && !output.is_empty() {
                output.push('_');
            }
            pending_separator = false;
            output.push(character);
        } else {
            pending_separator = true;
        }
    }
    if output.is_empty() {
        "unnamed".to_string()
    } else {
        output
    }
}

pub(super) fn component_csv_values(
    components: &BTreeMap<String, f64>,
    columns: &[ComponentColumn],
) -> Vec<String> {
    columns
        .iter()
        .map(|column| optional_string(components.get(&column.component_name)))
        .collect()
}

pub(super) fn component_arrow_arrays<'a>(
    maps: impl IntoIterator<Item = &'a BTreeMap<String, f64>> + Clone,
    columns: &[ComponentColumn],
) -> Vec<ArrayRef> {
    columns
        .iter()
        .map(|column| {
            Arc::new(arrow_array::Float64Array::from(
                maps.clone()
                    .into_iter()
                    .map(|components| components.get(&column.component_name).copied())
                    .collect::<Vec<_>>(),
            )) as ArrayRef
        })
        .collect()
}

fn push_csv_row(output: &mut String, row: &[String]) {
    let escaped = row
        .iter()
        .map(|value| csv_escape(value))
        .collect::<Vec<_>>();
    output.push_str(&escaped.join(","));
    output.push('\n');
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

pub(super) fn optional_string<T: ToString>(value: Option<T>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

pub(super) fn optional_u32_as_i64(value: Option<u32>) -> Option<i64> {
    value.map(i64::from)
}

pub(super) fn outcome_name(outcome: AnalysisOutcome) -> &'static str {
    match outcome {
        AnalysisOutcome::Legal => "legal",
        AnalysisOutcome::Degraded => "degraded",
        AnalysisOutcome::Partial => "partial",
        AnalysisOutcome::Unreachable => "unreachable",
    }
}

pub(super) fn diagnostics_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[]".to_string())
}

pub(super) fn batch_status_name(status: BatchItemStatus) -> &'static str {
    match status {
        BatchItemStatus::Succeeded => "succeeded",
        BatchItemStatus::Ignored => "ignored",
        BatchItemStatus::Failed => "failed",
    }
}

pub(super) fn coerce_linestring_coords_z(coords: &[[f64; 3]]) -> Vec<[f64; 3]> {
    match coords {
        [] => Vec::new(),
        [only] => vec![*only, *only],
        _ => coords.to_vec(),
    }
}

pub(super) fn linestring_wkt_z(coords: &[[f64; 3]]) -> String {
    let coords = coerce_linestring_coords_z(coords);
    if coords.is_empty() {
        return String::new();
    }
    let body = coords
        .iter()
        .map(|[x, y, z]| format!("{x} {y} {z}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("LINESTRING Z({body})")
}

pub(super) fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Extent {
    pub(super) min_x: f64,
    pub(super) min_y: f64,
    pub(super) max_x: f64,
    pub(super) max_y: f64,
}

pub(super) fn extent_for_features<'a>(
    features: impl IntoIterator<Item = &'a [[f64; 2]]>,
) -> Extent {
    let mut extent = Extent {
        min_x: f64::INFINITY,
        min_y: f64::INFINITY,
        max_x: f64::NEG_INFINITY,
        max_y: f64::NEG_INFINITY,
    };
    for feature in features {
        for [x, y] in feature {
            extent.min_x = extent.min_x.min(*x);
            extent.min_y = extent.min_y.min(*y);
            extent.max_x = extent.max_x.max(*x);
            extent.max_y = extent.max_y.max(*y);
        }
    }
    extent
}

pub(super) fn extent_for_features_z<'a>(
    features: impl IntoIterator<Item = &'a [[f64; 3]]>,
) -> Extent {
    let mut extent = Extent {
        min_x: f64::INFINITY,
        min_y: f64::INFINITY,
        max_x: f64::NEG_INFINITY,
        max_y: f64::NEG_INFINITY,
    };
    for feature in features {
        for [x, y, _] in feature {
            extent.min_x = extent.min_x.min(*x);
            extent.min_y = extent.min_y.min(*y);
            extent.max_x = extent.max_x.max(*x);
            extent.max_y = extent.max_y.max(*y);
        }
    }
    extent
}

pub(super) fn geoparquet_schema_z(fields: Vec<Field>, extent: Extent) -> Schema {
    geoparquet_schema_with_types(fields, extent, &["LineString Z"])
}

pub(super) fn geoparquet_schema_with_types(
    fields: Vec<Field>,
    extent: Extent,
    geometry_types: &[&str],
) -> Schema {
    let geo_metadata = json!({
        "version": "1.1.0",
        "primary_column": "geometry",
        "columns": {
            "geometry": {
                "encoding": "WKB",
                "geometry_types": geometry_types,
                "bbox": [extent.min_x, extent.min_y, extent.max_x, extent.max_y]
            }
        }
    });
    Schema::new_with_metadata(
        fields,
        std::collections::HashMap::from([("geo".to_string(), geo_metadata.to_string())]),
    )
}

pub(super) fn write_parquet_record_batch(
    path: &Path,
    schema: Schema,
    columns: Vec<ArrayRef>,
) -> Result<()> {
    ensure_parent_dir(path)?;
    let schema = Arc::new(schema);
    let batch =
        RecordBatch::try_new(schema.clone(), columns).context("building Parquet record batch")?;
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut writer = ArrowWriter::try_new(file, schema, None)
        .with_context(|| format!("opening Parquet writer {}", path.display()))?;
    writer
        .write(&batch)
        .with_context(|| format!("writing Parquet rows to {}", path.display()))?;
    writer
        .close()
        .with_context(|| format!("finalizing Parquet {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{coerce_linestring_coords_z, component_columns, csv_escape, linestring_wkt_z};

    #[test]
    fn formats_three_dimensional_linestrings() {
        assert_eq!(
            coerce_linestring_coords_z(&[[1.0, 2.0, 3.0]]),
            vec![[1.0, 2.0, 3.0], [1.0, 2.0, 3.0]]
        );
        assert_eq!(
            linestring_wkt_z(&[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]),
            "LINESTRING Z(1 2 3, 4 5 6)"
        );
    }

    #[test]
    fn escapes_csv_fields() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn component_columns_are_stable_sanitized_and_collision_safe() {
        let components = BTreeMap::from([
            ("Slope Cost".to_string(), 1.0),
            ("slope-cost".to_string(), 2.0),
            ("日照".to_string(), 3.0),
        ]);
        let columns = component_columns([&components]);
        assert_eq!(
            columns
                .iter()
                .map(|column| column.column_name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "component_slope_cost",
                "component_slope_cost_2",
                "component_unnamed"
            ]
        );
    }
}
