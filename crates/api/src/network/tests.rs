use super::*;
use netweevil_core::{
    FeatureAttributeColumn, FeatureAttributeColumnData, FeatureAttributeDefinition,
    FeatureAttributeType, NodeId,
};
use std::collections::BTreeMap;

fn node(z: f64) -> TopologyNode {
    TopologyNode {
        node_id: NodeId(0),
        lon: 114.16,
        lat: 22.28,
        z,
    }
}

#[test]
fn network_coordinates_and_ranges_preserve_vertical_edges() {
    let from = node(-18.5);
    let to = node(6.5);
    assert_eq!(node_coordinate(&from), [114.16, 22.28, -18.5]);
    let properties = vertical_properties(&from, &to, &FeatureAttributeTable::default(), 0);
    assert_eq!(properties["elevation_known"], true);
    assert_eq!(properties["min_elevation_m"], -18.5);
    assert_eq!(properties["max_elevation_m"], 6.5);
    assert_eq!(properties["elevation_m"], -6.0);
    assert!(
        properties["underground"].is_null(),
        "absolute source Z does not indicate depth below terrain"
    );
}

#[test]
fn unknown_elevation_is_explicit_and_serializes_without_nan() {
    let properties = vertical_properties(
        &node(f64::NAN),
        &node(12.0),
        &FeatureAttributeTable::default(),
        0,
    );
    assert_eq!(node_coordinate(&node(f64::NAN))[2], 0.0);
    assert_eq!(properties["elevation_known"], false);
    assert!(properties["elevation_m"].is_null());
    assert!(properties["from_z_m"].is_null());
    assert_eq!(properties["to_z_m"], 12.0);
    serde_json::to_string(&properties).unwrap();
}

#[test]
fn network_decodes_pedestrian_kind_without_losing_source_codes() {
    let table = FeatureAttributeTable {
        row_count: 1,
        strings: vec![],
        columns: vec![FeatureAttributeColumn {
            definition: FeatureAttributeDefinition {
                name: "FeatureType".into(),
                semantic_role: Some("feature_type".into()),
                value_type: FeatureAttributeType::Integer,
                domain: BTreeMap::from([("4".into(), "subway".into())]),
            },
            data: FeatureAttributeColumnData::Integer(vec![Some(4)]),
        }],
    };
    let properties = vertical_properties(&node(2.0), &node(3.0), &table, 0);
    assert_eq!(properties["pedestrian_kind"], "subway");
    assert_eq!(properties["source_attributes"]["FeatureType"], 4);
    assert_eq!(properties["structure"], "underground");
    assert_eq!(properties["underground"], true);
}
