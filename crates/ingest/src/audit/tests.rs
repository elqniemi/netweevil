use super::*;
use crate::test_util::{edge, topology};
use netweevil_core::{FeatureAttributeColumn, FeatureAttributeColumnData, FeatureAttributeType};

/// Three collinear nodes; every edge belongs to source feature 7.
fn feature_topology(segment_nodes: &[(u32, u32)]) -> TopologyBundle {
    let edges = segment_nodes
        .iter()
        .enumerate()
        .map(|(edge_id, &(from, to))| edge(edge_id as u32, from, to, 7))
        .collect::<Vec<_>>();
    topology(
        &[
            (114.0, 22.0, 0.0),
            (114.000_001, 22.0, 1.0),
            (114.000_002, 22.0, 2.0),
        ],
        &edges,
    )
}

fn single_row_table(
    name: &str,
    semantic_role: Option<&str>,
    value_type: FeatureAttributeType,
    data: FeatureAttributeColumnData,
) -> FeatureAttributeTable {
    FeatureAttributeTable {
        row_count: 1,
        strings: Vec::new(),
        columns: vec![FeatureAttributeColumn {
            definition: FeatureAttributeDefinition {
                name: name.to_string(),
                semantic_role: semantic_role.map(str::to_string),
                value_type,
                domain: BTreeMap::new(),
            },
            data,
        }],
    }
}

#[test]
fn ordered_geometry_detects_reversed_vertex_chain() {
    let forward =
        ordered_feature_segments(&feature_topology(&[(0, 1), (1, 2)])).expect("forward geometry");
    let reversed =
        ordered_feature_segments(&feature_topology(&[(2, 1), (1, 0)])).expect("reversed geometry");
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
    let source = single_row_table(
        "all_null",
        None,
        FeatureAttributeType::String,
        FeatureAttributeColumnData::String(vec![None]),
    );
    let (missing, unexpected, drifts) =
        compare_attribute_schemas(&retained, &source).expect("schema comparison");
    assert_eq!(missing, vec!["all_null"]);
    assert!(unexpected.is_empty());
    assert!(drifts.is_empty());
}

#[test]
fn detects_an_edge_linked_to_the_wrong_feature_row() {
    let mut topology = feature_topology(&[(0, 1)]);
    topology.edge_layers.routing[0].feature_row = 0;
    topology.feature_attributes = single_row_table(
        "feature_id",
        Some("feature_id"),
        FeatureAttributeType::Integer,
        FeatureAttributeColumnData::Integer(vec![Some(8)]),
    );
    let (count, drifts) = edge_attribute_link_drifts(&topology).expect("link audit");
    assert_eq!(count, 1);
    assert_eq!(drifts[0].source_feature_id, 7);
    assert_eq!(drifts[0].row_feature_id, Some(8));
}
