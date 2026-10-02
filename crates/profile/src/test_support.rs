use netweevil_core::{
    AccessMask, DirectedEdge, EdgeBasedTopology, EdgeId, NO_FEATURE_ROW, NodeId, RoadClass,
    SmoothnessClass, SurfaceClass, TopologyBundle, TopologyEdgeLayers,
};

use crate::ProfileDocument;

/// Parses a profile with a `test` header in `mode`, followed by `body` YAML.
pub(crate) fn profile(mode: &str, body: &str) -> ProfileDocument {
    serde_yaml::from_str(&format!(
        "profile: {{id: test, label: test, mode: {mode}, defaults_pack: test}}\n{body}"
    ))
    .expect("profile parses")
}

/// 1 km asphalt edge `index -> index + 1` from source way `10 + index`.
pub(crate) fn edge(index: u32, road_class: RoadClass, access: u16) -> DirectedEdge {
    DirectedEdge {
        edge_id: EdgeId(index),
        from: NodeId(index),
        to: NodeId(index + 1),
        source_way_id: 10 + i64::from(index),
        length_m: 1_000,
        ascent_m: 0.0,
        descent_m: 0.0,
        feature_row: NO_FEATURE_ROW,
        source_direction: 0,
        temporal_rule_id: None,
        duration_s: None,
        road_class,
        surface: SurfaceClass::Asphalt,
        smoothness: SmoothnessClass::Unknown,
        access_mask: AccessMask::new(access),
        is_toll: false,
        max_speed_kph: None,
        lanes: None,
        name_index: None,
        geometry_offset: 0,
        geometry_len: 0,
        flags: 0,
    }
}

/// Node-less topology over `edges` with no turn transitions.
pub(crate) fn topology(edges: &[DirectedEdge]) -> TopologyBundle {
    TopologyBundle {
        schema_version: netweevil_core::TOPOLOGY_BUNDLE_SCHEMA_VERSION,
        source_path: "test.osm.pbf".to_string(),
        source_sha256: "abc".to_string(),
        nodes: Vec::new(),
        edge_layers: TopologyEdgeLayers::from_directed_edges(edges),
        turn_restrictions: Vec::new(),
        names: Vec::new(),
        edge_based_topology: EdgeBasedTopology::default(),
        spatial_index: None,
        node_component_ids: vec![0; edges.len() + 1],
        edge_component_ids: vec![0; edges.len()],
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
    }
}
