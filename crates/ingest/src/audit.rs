use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use netweevil_core::{
    FeatureAttributeDefinition, FeatureAttributeTable, FeatureAttributeValueRef, TopologyBundle,
};
use serde::{Deserialize, Serialize};

use crate::geopackage::{
    SOURCE_GEOMETRY_LAYOUT_COLUMN, SOURCE_LAYER_SCHEMA_COLUMN, scan_geopackages,
};
use crate::mapping::GeoPackageMapping;
use crate::topology::build_topology_from_scan;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetAuditReport {
    pub passed: bool,
    pub source_count: usize,
    pub retained_feature_count: u32,
    pub retained_field_count: usize,
    pub compared_segment_count: usize,
    #[serde(default)]
    pub missing_features: Vec<i64>,
    #[serde(default)]
    pub unexpected_features: Vec<i64>,
    #[serde(default)]
    pub attribute_drifts: Vec<AttributeDrift>,
    #[serde(default)]
    pub missing_fields: Vec<String>,
    #[serde(default)]
    pub unexpected_fields: Vec<String>,
    #[serde(default)]
    pub attribute_schema_drifts: Vec<AttributeSchemaDrift>,
    /// Features whose source-oriented segment order, part layout, or
    /// collapsed-vertex positions differ from the audited source.
    #[serde(default)]
    pub geometry_order_drifts: Vec<i64>,
    /// Structured details for the first geometry drifts. Segment coordinates
    /// are exact retained/source values; layout JSON identifies part and
    /// collapsed-vertex indexes (including sparse quantization overrides).
    #[serde(default)]
    pub geometry_drifts: Vec<GeometryDrift>,
    #[serde(default)]
    pub edge_attribute_link_drift_count: usize,
    #[serde(default)]
    pub edge_attribute_link_drifts: Vec<EdgeAttributeLinkDrift>,
    #[serde(default)]
    pub missing_segments: usize,
    #[serde(default)]
    pub unexpected_segments: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeDrift {
    pub feature_id: i64,
    pub field: String,
    pub retained: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeSchemaDrift {
    pub field: String,
    pub retained: FeatureAttributeDefinition,
    pub source: FeatureAttributeDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeometrySegment {
    pub from: [f64; 3],
    pub to: [f64; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeometryDrift {
    pub feature_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_segment: Option<GeometrySegment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_segment: Option<GeometrySegment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_layout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_layout: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeAttributeLinkDrift {
    pub edge_id: u32,
    pub source_feature_id: i64,
    pub feature_row: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_feature_id: Option<i64>,
}

pub fn audit_geopackage_dataset(
    retained: &TopologyBundle,
    source_paths: &[PathBuf],
    mapping: &GeoPackageMapping,
) -> Result<DatasetAuditReport> {
    if source_paths.is_empty() {
        bail!("dataset audit requires at least one source");
    }
    let scan = scan_geopackages(source_paths, mapping, &mut |_| {})?;
    let (reference, _, _) = build_topology_from_scan(
        source_paths
            .first()
            .map(PathBuf::as_path)
            .unwrap_or(Path::new("audit.gpkg")),
        "audit",
        scan,
        &mut |_| {},
    )?;

    let retained_rows = feature_rows_by_id(&retained.feature_attributes)
        .context("retained topology has no usable feature_id attribute")?;
    let source_rows = feature_rows_by_id(&reference.feature_attributes)
        .context("source scan has no usable feature_id attribute")?;
    let retained_ids = retained_rows.keys().copied().collect::<BTreeSet<_>>();
    let source_ids = source_rows.keys().copied().collect::<BTreeSet<_>>();
    let missing_features: Vec<i64> = source_ids.difference(&retained_ids).copied().collect();
    let unexpected_features: Vec<i64> = retained_ids.difference(&source_ids).copied().collect();
    let (edge_attribute_link_drift_count, edge_attribute_link_drifts) =
        edge_attribute_link_drifts(retained)?;

    let (missing_fields, unexpected_fields, attribute_schema_drifts) =
        compare_attribute_schemas(&retained.feature_attributes, &reference.feature_attributes)?;

    let mut attribute_drifts = Vec::new();
    let field_columns = reference
        .feature_attributes
        .columns
        .iter()
        .enumerate()
        .filter(|(_, column)| column.definition.name != SOURCE_GEOMETRY_LAYOUT_COLUMN)
        .map(|(source_index, column)| {
            (
                column.definition.name.clone(),
                retained
                    .feature_attributes
                    .column_index(&column.definition.name),
                source_index,
            )
        })
        .collect::<Vec<_>>();
    const MAX_REPORTED_DRIFTS: usize = 1_000;
    for feature_id in source_ids.intersection(&retained_ids).copied() {
        let retained_row = retained_rows[&feature_id];
        let source_row = source_rows[&feature_id];
        for (field, retained_column, source_column) in &field_columns {
            let retained_value = canonical_value(
                retained_column
                    .and_then(|column| retained.feature_attributes.value_at(retained_row, column)),
            );
            let source_value = canonical_value(
                reference
                    .feature_attributes
                    .value_at(source_row, *source_column),
            );
            if retained_value != source_value && attribute_drifts.len() < MAX_REPORTED_DRIFTS {
                attribute_drifts.push(AttributeDrift {
                    feature_id,
                    field: field.clone(),
                    retained: retained_value,
                    source: source_value,
                });
            }
        }
    }

    let retained_geometry = ordered_feature_segments(retained)?;
    let source_geometry = ordered_feature_segments(&reference)?;
    let retained_layout_column = retained
        .feature_attributes
        .column_index(SOURCE_GEOMETRY_LAYOUT_COLUMN);
    let source_layout_column = reference
        .feature_attributes
        .column_index(SOURCE_GEOMETRY_LAYOUT_COLUMN);
    let mut geometry_order_drifts = Vec::new();
    let mut geometry_drifts = Vec::new();
    const MAX_REPORTED_GEOMETRY_DRIFTS: usize = 1_000;
    for feature_id in source_ids.intersection(&retained_ids).copied() {
        let retained_segments = retained_geometry
            .get(&feature_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let source_segments = source_geometry
            .get(&feature_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let retained_layout = canonical_value(retained_layout_column.and_then(|column| {
            retained
                .feature_attributes
                .value_at(retained_rows[&feature_id], column)
        }));
        let source_layout = canonical_value(source_layout_column.and_then(|column| {
            reference
                .feature_attributes
                .value_at(source_rows[&feature_id], column)
        }));
        let segment_index = retained_segments
            .iter()
            .zip(source_segments)
            .position(|(retained, source)| retained != source)
            .or_else(|| {
                (retained_segments.len() != source_segments.len())
                    .then_some(retained_segments.len().min(source_segments.len()))
            });
        let layout_differs = retained_layout != source_layout;
        if segment_index.is_some() || layout_differs {
            geometry_order_drifts.push(feature_id);
            if geometry_drifts.len() < MAX_REPORTED_GEOMETRY_DRIFTS {
                geometry_drifts.push(GeometryDrift {
                    feature_id,
                    segment_index,
                    retained_segment: segment_index
                        .and_then(|index| retained_segments.get(index))
                        .map(SegmentKey::coordinates),
                    source_segment: segment_index
                        .and_then(|index| source_segments.get(index))
                        .map(SegmentKey::coordinates),
                    retained_layout: layout_differs.then(|| retained_layout.clone()).flatten(),
                    source_layout: layout_differs.then(|| source_layout.clone()).flatten(),
                });
            }
        }
    }
    let retained_segments = segment_multiset(&retained_geometry);
    let source_segments = segment_multiset(&source_geometry);
    let missing_segments = multiset_difference_count(&source_segments, &retained_segments);
    let unexpected_segments = multiset_difference_count(&retained_segments, &source_segments);
    let passed = missing_features.is_empty()
        && unexpected_features.is_empty()
        && attribute_drifts.is_empty()
        && missing_fields.is_empty()
        && unexpected_fields.is_empty()
        && attribute_schema_drifts.is_empty()
        && geometry_order_drifts.is_empty()
        && edge_attribute_link_drift_count == 0
        && missing_segments == 0
        && unexpected_segments == 0;

    Ok(DatasetAuditReport {
        passed,
        source_count: source_paths.len(),
        retained_feature_count: retained.feature_attributes.row_count,
        retained_field_count: retained
            .feature_attributes
            .columns
            .iter()
            .filter(|column| {
                !matches!(
                    column.definition.name.as_str(),
                    SOURCE_GEOMETRY_LAYOUT_COLUMN | SOURCE_LAYER_SCHEMA_COLUMN
                )
            })
            .count(),
        compared_segment_count: source_segments.values().sum(),
        missing_features,
        unexpected_features,
        attribute_drifts,
        missing_fields,
        unexpected_fields,
        attribute_schema_drifts,
        geometry_order_drifts,
        geometry_drifts,
        edge_attribute_link_drift_count,
        edge_attribute_link_drifts,
        missing_segments,
        unexpected_segments,
    })
}

fn compare_attribute_schemas(
    retained: &FeatureAttributeTable,
    source: &FeatureAttributeTable,
) -> Result<(Vec<String>, Vec<String>, Vec<AttributeSchemaDrift>)> {
    let definitions = |table: &FeatureAttributeTable| {
        let mut definitions = BTreeMap::new();
        for column in &table.columns {
            let name = column.definition.name.clone();
            if definitions
                .insert(name.clone(), column.definition.clone())
                .is_some()
            {
                bail!("feature attribute table contains duplicate column '{name}'");
            }
        }
        Ok::<_, anyhow::Error>(definitions)
    };
    let retained = definitions(retained)?;
    let source = definitions(source)?;
    let retained_names = retained.keys().cloned().collect::<BTreeSet<_>>();
    let source_names = source.keys().cloned().collect::<BTreeSet<_>>();
    let missing = source_names.difference(&retained_names).cloned().collect();
    let unexpected = retained_names.difference(&source_names).cloned().collect();
    let mut drifts = Vec::new();
    for field in source_names.intersection(&retained_names) {
        let retained_definition = &retained[field];
        let source_definition = &source[field];
        if retained_definition.semantic_role != source_definition.semantic_role
            || retained_definition.value_type != source_definition.value_type
            || retained_definition.domain != source_definition.domain
        {
            drifts.push(AttributeSchemaDrift {
                field: field.clone(),
                retained: retained_definition.clone(),
                source: source_definition.clone(),
            });
        }
    }
    Ok((missing, unexpected, drifts))
}

fn feature_rows_by_id(table: &FeatureAttributeTable) -> Result<BTreeMap<i64, u32>> {
    let feature_id_column = table
        .column_index("feature_id")
        .context("feature attribute table has no feature_id semantic role")?;
    let mut rows = BTreeMap::new();
    for row in 0..table.row_count {
        let feature_id = feature_id_at(table, row, feature_id_column)?;
        if rows.insert(feature_id, row).is_some() {
            bail!("feature attribute table contains duplicate feature_id {feature_id}");
        }
    }
    Ok(rows)
}

fn feature_id_at(table: &FeatureAttributeTable, row: u32, column: usize) -> Result<i64> {
    let Some(value) = table.value_at(row, column) else {
        bail!("feature attribute row {row} has no feature_id");
    };
    match value {
        FeatureAttributeValueRef::Integer(value) => Ok(value),
        FeatureAttributeValueRef::Float(value) if value.fract() == 0.0 => Ok(value as i64),
        FeatureAttributeValueRef::String(value) => value
            .parse::<i64>()
            .with_context(|| format!("invalid feature_id '{value}' at row {row}")),
        _ => bail!("feature_id at row {row} is not an integer"),
    }
}

fn edge_attribute_link_drifts(
    topology: &TopologyBundle,
) -> Result<(usize, Vec<EdgeAttributeLinkDrift>)> {
    let feature_id_column = topology
        .feature_attributes
        .column_index("feature_id")
        .context("feature attribute table has no feature_id semantic role")?;
    let mut count = 0;
    let mut drifts = Vec::new();
    const MAX_REPORTED_LINK_DRIFTS: usize = 1_000;
    for edge_index in 0..topology.edge_count() {
        let edge = topology.routing_edge(edge_index);
        let row_feature_id = (edge.feature_row < topology.feature_attributes.row_count)
            .then(|| {
                feature_id_at(
                    &topology.feature_attributes,
                    edge.feature_row,
                    feature_id_column,
                )
            })
            .transpose()?;
        if row_feature_id != Some(edge.source_way_id) {
            count += 1;
            if drifts.len() < MAX_REPORTED_LINK_DRIFTS {
                drifts.push(EdgeAttributeLinkDrift {
                    edge_id: edge.edge_id.0,
                    source_feature_id: edge.source_way_id,
                    feature_row: edge.feature_row,
                    row_feature_id,
                });
            }
        }
    }
    Ok((count, drifts))
}

fn canonical_value(value: Option<FeatureAttributeValueRef<'_>>) -> Option<String> {
    value.map(FeatureAttributeValueRef::canonical_string)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SegmentKey {
    feature_id: i64,
    from_x: u64,
    from_y: u64,
    from_z: u64,
    to_x: u64,
    to_y: u64,
    to_z: u64,
}

impl SegmentKey {
    fn coordinates(&self) -> GeometrySegment {
        GeometrySegment {
            from: [
                f64::from_bits(self.from_x),
                f64::from_bits(self.from_y),
                f64::from_bits(self.from_z),
            ],
            to: [
                f64::from_bits(self.to_x),
                f64::from_bits(self.to_y),
                f64::from_bits(self.to_z),
            ],
        }
    }
}

fn coordinate_bits(value: f64) -> u64 {
    if value.is_nan() {
        f64::NAN.to_bits()
    } else {
        value.to_bits()
    }
}

fn ordered_feature_segments(topology: &TopologyBundle) -> Result<BTreeMap<i64, Vec<SegmentKey>>> {
    let mut selected_directions = BTreeMap::<i64, i8>::new();
    for index in 0..topology.edge_count() {
        let edge = topology.routing_edge(index);
        selected_directions
            .entry(edge.source_way_id)
            .and_modify(|direction| {
                if edge.source_direction == 1 {
                    *direction = 1;
                }
            })
            .or_insert(edge.source_direction);
    }
    let mut segments = BTreeMap::<i64, Vec<(u32, SegmentKey)>>::new();
    for index in 0..topology.edge_count() {
        let edge = topology.routing_edge(index);
        if selected_directions.get(&edge.source_way_id) != Some(&edge.source_direction) {
            continue;
        }
        let (from, to) = if edge.source_direction < 0 {
            (
                &topology.nodes[edge.to.0 as usize],
                &topology.nodes[edge.from.0 as usize],
            )
        } else {
            (
                &topology.nodes[edge.from.0 as usize],
                &topology.nodes[edge.to.0 as usize],
            )
        };
        segments.entry(edge.source_way_id).or_default().push((
            edge.edge_id.0,
            SegmentKey {
                feature_id: edge.source_way_id,
                from_x: coordinate_bits(from.lon),
                from_y: coordinate_bits(from.lat),
                from_z: coordinate_bits(from.z),
                to_x: coordinate_bits(to.lon),
                to_y: coordinate_bits(to.lat),
                to_z: coordinate_bits(to.z),
            },
        ));
    }
    Ok(segments
        .into_iter()
        .map(|(feature_id, mut segments)| {
            segments.sort_by_key(|(edge_id, _)| *edge_id);
            (
                feature_id,
                segments.into_iter().map(|(_, segment)| segment).collect(),
            )
        })
        .collect())
}

fn segment_multiset(geometry: &BTreeMap<i64, Vec<SegmentKey>>) -> BTreeMap<SegmentKey, usize> {
    let mut segments = BTreeMap::new();
    for segment in geometry.values().flatten() {
        *segments.entry(*segment).or_insert(0) += 1;
    }
    segments
}

fn multiset_difference_count<K: Ord>(
    left: &BTreeMap<K, usize>,
    right: &BTreeMap<K, usize>,
) -> usize {
    left.iter()
        .map(|(key, left_count)| left_count.saturating_sub(*right.get(key).unwrap_or(&0)))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::edge;
    use netweevil_core::{
        EdgeBasedTopology, FeatureAttributeColumn, FeatureAttributeColumnData,
        FeatureAttributeType, TopologyEdgeLayers, TopologyNode,
    };

    fn topology(segment_nodes: &[(u32, u32)]) -> TopologyBundle {
        let nodes = [
            (114.0, 22.0, 0.0),
            (114.000_001, 22.0, 1.0),
            (114.000_002, 22.0, 2.0),
        ]
        .into_iter()
        .enumerate()
        .map(|(node_id, (lon, lat, z))| TopologyNode {
            node_id: netweevil_core::NodeId(node_id as u32),
            lon,
            lat,
            z,
        })
        .collect();
        let edges = segment_nodes
            .iter()
            .enumerate()
            .map(|(edge_id, (from, to))| edge(edge_id as u32, *from, *to, 7))
            .collect::<Vec<_>>();
        TopologyBundle {
            schema_version: 0,
            source_path: String::new(),
            source_sha256: String::new(),
            nodes,
            edge_layers: TopologyEdgeLayers::from_directed_edges(&edges),
            turn_restrictions: Vec::new(),
            names: Vec::new(),
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
            node_component_ids: Vec::new(),
            edge_component_ids: Vec::new(),
            feature_attributes: FeatureAttributeTable::default(),
            temporal_rule_sets: Vec::new(),
        }
    }

    fn string_column(name: &str) -> FeatureAttributeColumn {
        FeatureAttributeColumn {
            definition: FeatureAttributeDefinition {
                name: name.to_string(),
                semantic_role: None,
                value_type: FeatureAttributeType::String,
                domain: BTreeMap::new(),
            },
            data: FeatureAttributeColumnData::String(vec![None]),
        }
    }

    fn feature_id_column(value: i64) -> FeatureAttributeColumn {
        FeatureAttributeColumn {
            definition: FeatureAttributeDefinition {
                name: "feature_id".to_string(),
                semantic_role: Some("feature_id".to_string()),
                value_type: FeatureAttributeType::Integer,
                domain: BTreeMap::new(),
            },
            data: FeatureAttributeColumnData::Integer(vec![Some(value)]),
        }
    }

    #[test]
    fn ordered_geometry_detects_reversed_vertex_chain() {
        let forward =
            ordered_feature_segments(&topology(&[(0, 1), (1, 2)])).expect("forward geometry");
        let reversed =
            ordered_feature_segments(&topology(&[(2, 1), (1, 0)])).expect("reversed geometry");
        assert_ne!(forward, reversed);
        assert_eq!(segment_multiset(&forward).len(), 2);
        assert_eq!(segment_multiset(&reversed).len(), 2);
    }

    #[test]
    fn schema_comparison_detects_missing_all_null_column() {
        let retained = FeatureAttributeTable {
            row_count: 1,
            ..FeatureAttributeTable::default()
        };
        let source = FeatureAttributeTable {
            row_count: 1,
            strings: Vec::new(),
            columns: vec![string_column("all_null")],
        };
        let (missing, unexpected, drifts) =
            compare_attribute_schemas(&retained, &source).expect("schema comparison");
        assert_eq!(missing, vec!["all_null"]);
        assert!(unexpected.is_empty());
        assert!(drifts.is_empty());
    }

    #[test]
    fn detects_an_edge_linked_to_the_wrong_feature_row() {
        let mut topology = topology(&[(0, 1)]);
        topology.edge_layers.routing[0].feature_row = 0;
        topology.feature_attributes = FeatureAttributeTable {
            row_count: 1,
            strings: Vec::new(),
            columns: vec![feature_id_column(8)],
        };
        let (count, drifts) = edge_attribute_link_drifts(&topology).expect("link audit");
        assert_eq!(count, 1);
        assert_eq!(drifts[0].source_feature_id, 7);
        assert_eq!(drifts[0].row_feature_id, Some(8));
    }
}
