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

pub(super) fn write_csv(path: &Path, header: &[&str], rows: Vec<Vec<String>>) -> Result<()> {
    ensure_parent_dir(path)?;
    let mut output = String::new();
    push_csv_row(
        &mut output,
        &header
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>(),
    );
    for row in rows {
        push_csv_row(&mut output, &row);
    }
    fs::write(path, output).with_context(|| format!("writing {}", path.display()))
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
        AnalysisOutcome::NotImplemented => "not_implemented",
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

pub(super) fn coerce_linestring_coords(coords: &[[f64; 2]]) -> Vec<[f64; 2]> {
    match coords {
        [] => Vec::new(),
        [only] => vec![*only, *only],
        _ => coords.to_vec(),
    }
}

pub(super) fn linestring_wkt(coords: &[[f64; 2]]) -> String {
    let coords = coerce_linestring_coords(coords);
    if coords.is_empty() {
        return String::new();
    }
    let body = coords
        .iter()
        .map(|[x, y]| format!("{x} {y}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("LINESTRING({body})")
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

pub(super) fn geoparquet_schema(fields: Vec<Field>, extent: Extent) -> Schema {
    geoparquet_schema_with_types(fields, extent, &["LineString"])
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
    use super::{coerce_linestring_coords, csv_escape, linestring_wkt};

    #[test]
    fn duplicates_single_vertex_linestrings() {
        assert_eq!(
            coerce_linestring_coords(&[[1.0, 2.0]]),
            vec![[1.0, 2.0], [1.0, 2.0]]
        );
    }

    #[test]
    fn escapes_csv_fields() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn formats_linestring_wkt() {
        assert_eq!(
            linestring_wkt(&[[6.0, 53.0], [6.1, 53.1]]),
            "LINESTRING(6 53, 6.1 53.1)"
        );
    }
}
