use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Sentinel used by edges that are not derived from a source feature row.
pub const NO_FEATURE_ROW: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureAttributeType {
    Boolean,
    Integer,
    Float,
    String,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureAttributeDefinition {
    /// Original source column name. Keeping this verbatim is part of the
    /// lossless import contract.
    pub name: String,
    /// Optional stable name used by profiles and other engine surfaces
    /// (`covered`, `feature_type`, `indoor_location`, ...).
    pub semantic_role: Option<String>,
    pub value_type: FeatureAttributeType,
    /// Coded-value labels keyed by the source value's canonical string form.
    pub domain: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureAttributeColumnData {
    Boolean(Vec<Option<bool>>),
    Integer(Vec<Option<i64>>),
    Float(Vec<Option<f64>>),
    /// Indexes into [`FeatureAttributeTable::strings`].
    String(Vec<Option<u32>>),
    /// Raw JSON, also interned through [`FeatureAttributeTable::strings`].
    Json(Vec<Option<u32>>),
}

impl FeatureAttributeColumnData {
    pub fn len(&self) -> usize {
        match self {
            Self::Boolean(values) => values.len(),
            Self::Integer(values) => values.len(),
            Self::Float(values) => values.len(),
            Self::String(values) | Self::Json(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureAttributeColumn {
    pub definition: FeatureAttributeDefinition,
    pub data: FeatureAttributeColumnData,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeatureAttributeTable {
    pub row_count: u32,
    /// Shared string dictionary for string and raw-JSON columns.
    pub strings: Vec<String>,
    pub columns: Vec<FeatureAttributeColumn>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FeatureAttributeValueRef<'a> {
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(&'a str),
    Json(&'a str),
}

impl FeatureAttributeTable {
    pub fn validate(&self) -> Result<(), String> {
        for column in &self.columns {
            if column.data.len() != self.row_count as usize {
                return Err(format!(
                    "feature attribute column '{}' has {} rows, expected {}",
                    column.definition.name,
                    column.data.len(),
                    self.row_count
                ));
            }
            match &column.data {
                FeatureAttributeColumnData::String(values)
                | FeatureAttributeColumnData::Json(values) => {
                    if let Some(index) = values.iter().flatten().find(|index| {
                        usize::try_from(**index)
                            .map(|index| index >= self.strings.len())
                            .unwrap_or(true)
                    }) {
                        return Err(format!(
                            "feature attribute column '{}' references missing string {}",
                            column.definition.name, index
                        ));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Finds a column by its original source name or stable semantic role.
    /// Matching is case-insensitive and treats punctuation as underscores.
    pub fn column(&self, name: &str) -> Option<&FeatureAttributeColumn> {
        self.column_index(name)
            .and_then(|index| self.columns.get(index))
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        let requested = normalize_attribute_name(name);
        self.columns.iter().position(|column| {
            normalize_attribute_name(&column.definition.name) == requested
                || column
                    .definition
                    .semantic_role
                    .as_deref()
                    .is_some_and(|role| normalize_attribute_name(role) == requested)
        })
    }

    pub fn value(&self, row: u32, name: &str) -> Option<FeatureAttributeValueRef<'_>> {
        self.value_at(row, self.column_index(name)?)
    }

    pub fn value_at(&self, row: u32, column_index: usize) -> Option<FeatureAttributeValueRef<'_>> {
        let column = self.columns.get(column_index)?;
        let row = row as usize;
        match &column.data {
            FeatureAttributeColumnData::Boolean(values) => values
                .get(row)
                .copied()
                .flatten()
                .map(FeatureAttributeValueRef::Boolean),
            FeatureAttributeColumnData::Integer(values) => values
                .get(row)
                .copied()
                .flatten()
                .map(FeatureAttributeValueRef::Integer),
            FeatureAttributeColumnData::Float(values) => values
                .get(row)
                .copied()
                .flatten()
                .map(FeatureAttributeValueRef::Float),
            FeatureAttributeColumnData::String(values) => values
                .get(row)
                .copied()
                .flatten()
                .and_then(|index| self.strings.get(index as usize))
                .map(String::as_str)
                .map(FeatureAttributeValueRef::String),
            FeatureAttributeColumnData::Json(values) => values
                .get(row)
                .copied()
                .flatten()
                .and_then(|index| self.strings.get(index as usize))
                .map(String::as_str)
                .map(FeatureAttributeValueRef::Json),
        }
    }

    /// Compares a typed source value to profile/config text. In addition to
    /// the raw value, coded-value domain labels are accepted. This lets a
    /// profile say `feature_type: staircase` while retaining the original
    /// integer code losslessly.
    pub fn value_matches(&self, row: u32, name: &str, expected: &str) -> bool {
        let Some(column_index) = self.column_index(name) else {
            return false;
        };
        self.value_matches_at(row, column_index, expected)
    }

    pub fn value_matches_at(&self, row: u32, column_index: usize, expected: &str) -> bool {
        let Some(column) = self.columns.get(column_index) else {
            return false;
        };
        let Some(value) = self.value_at(row, column_index) else {
            return expected.eq_ignore_ascii_case("null");
        };
        let canonical = value.canonical_string();
        if normalized_value_eq(&canonical, expected) {
            return true;
        }
        column
            .definition
            .domain
            .get(&canonical)
            .is_some_and(|decoded| normalized_value_eq(decoded, expected))
    }
}

impl FeatureAttributeValueRef<'_> {
    pub fn canonical_string(self) -> String {
        match self {
            Self::Boolean(value) => value.to_string(),
            Self::Integer(value) => value.to_string(),
            Self::Float(value) => {
                if value.fract() == 0.0 {
                    format!("{value:.0}")
                } else {
                    value.to_string()
                }
            }
            Self::String(value) | Self::Json(value) => value.to_string(),
        }
    }
}

pub fn normalize_attribute_name(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut previous_separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            normalized.push(character);
            previous_separator = false;
        } else if !previous_separator && !normalized.is_empty() {
            normalized.push('_');
            previous_separator = true;
        }
    }
    while normalized.ends_with('_') {
        normalized.pop();
    }
    normalized
}

fn normalized_value_eq(left: &str, right: &str) -> bool {
    normalize_attribute_name(left) == normalize_attribute_name(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_original_names_semantic_roles_and_domain_labels() {
        let table = FeatureAttributeTable {
            row_count: 1,
            strings: Vec::new(),
            columns: vec![FeatureAttributeColumn {
                definition: FeatureAttributeDefinition {
                    name: "FeatureType".to_string(),
                    semantic_role: Some("feature_type".to_string()),
                    value_type: FeatureAttributeType::Integer,
                    domain: BTreeMap::from([("12".to_string(), "Staircase".to_string())]),
                },
                data: FeatureAttributeColumnData::Integer(vec![Some(12)]),
            }],
        };

        assert!(table.value_matches(0, "Feature Type", "12"));
        assert!(table.value_matches(0, "feature_type", "staircase"));
        assert!(!table.value_matches(0, "feature_type", "lift"));
    }
}
