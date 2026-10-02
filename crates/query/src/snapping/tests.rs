use super::*;
use netweevil_core::{NodeId, NodeSpatialIndex, SpatialIndexCell, TopologyBounds};

#[test]
fn finite_global_snap_radius_is_bounded_by_the_node_grid() {
    let topology = TopologyBundle {
        schema_version: 1,
        source_path: "test".into(),
        source_sha256: "test".into(),
        nodes: vec![TopologyNode {
            node_id: NodeId(0),
            lon: 6.0,
            lat: 53.0,
            z: 0.0,
        }],
        edge_layers: Default::default(),
        feature_attributes: Default::default(),
        temporal_rule_sets: vec![],
        regions: Default::default(),
        elevation: Default::default(),
        turn_restrictions: vec![],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: None,
        node_component_ids: vec![0],
        edge_component_ids: vec![],
    };
    let index = NodeSpatialIndex {
        bounds: TopologyBounds {
            min_lon: 6.0,
            min_lat: 53.0,
            max_lon: 6.001,
            max_lat: 53.001,
        },
        columns: 1,
        rows: 1,
        cell_width_deg: 0.001,
        cell_height_deg: 0.001,
        cells: vec![SpatialIndexCell {
            node_start: 0,
            node_len: 1,
        }],
        node_ids: vec![0],
    };
    for (lon, lat) in [(6.0, 53.0), (-180.0, -90.0), (180.0, 90.0)] {
        let point = LabeledPoint {
            id: "global".into(),
            lon,
            lat,
            z: None,
        };
        let candidates = spatial_snap_nodes(&topology, &index, &point, f64::MAX);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, 0);
    }
}
