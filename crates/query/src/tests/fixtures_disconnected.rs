use netweevil_core::{
    AccessMask, CacheBundleId, CompiledEdgeMetric, CompiledProfileBundle, CompiledTurnCostConfig,
    DirectedEdge, EdgeId, NodeId, RoadClass, SmoothnessClass, SurfaceClass, TopologyBundle,
    TopologyNode, TravelMode,
};

use std::fs;
use std::path::PathBuf;

use super::fixtures::*;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn disconnected_topology() -> TopologyBundle {
    with_edge_based_topology(TopologyBundle {
        schema_version: 7,
        source_path: "disconnected".to_string(),
        source_sha256: "abc".to_string(),
        nodes: vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 6.0,
                lat: 53.0,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 6.001,
                lat: 53.0,
            },
            TopologyNode {
                node_id: NodeId(2),
                lon: 6.01,
                lat: 53.0,
            },
            TopologyNode {
                node_id: NodeId(3),
                lon: 6.011,
                lat: 53.0,
            },
        ],
        edge_layers: Default::default(),
        edges: vec![
            DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 10,
                length_m: 100,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(1),
                from: NodeId(2),
                to: NodeId(3),
                source_way_id: 11,
                length_m: 100,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
        ],
        turn_restrictions: vec![],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: Some(spatial_index_for_all_nodes(4, 6.0, 53.0, 6.011, 53.0)),
        node_component_ids: vec![],
        edge_component_ids: vec![],
    })
}

pub(super) fn hop_disconnected_topology() -> TopologyBundle {
    with_edge_based_topology(TopologyBundle {
        schema_version: 7,
        source_path: "hop-disconnected".to_string(),
        source_sha256: "abc".to_string(),
        nodes: vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 6.0,
                lat: 53.0,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 6.0005,
                lat: 53.0,
            },
            TopologyNode {
                node_id: NodeId(2),
                lon: 6.0010,
                lat: 53.0,
            },
            TopologyNode {
                node_id: NodeId(3),
                lon: 6.0015,
                lat: 53.0,
            },
        ],
        edge_layers: Default::default(),
        edges: vec![
            DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 20,
                length_m: 50,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(1),
                from: NodeId(2),
                to: NodeId(3),
                source_way_id: 21,
                length_m: 50,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
        ],
        turn_restrictions: vec![],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: Some(spatial_index_for_all_nodes(4, 6.0, 53.0, 6.0015, 53.0)),
        node_component_ids: vec![],
        edge_component_ids: vec![],
    })
}

pub(super) fn hop_disconnected_metrics() -> CompiledProfileBundle {
    CompiledProfileBundle {
        schema_version: 3,
        profile_id: "test".to_string(),
        profile_hash: "abc".to_string(),
        mode: TravelMode::Car,
        turn_costs: CompiledTurnCostConfig::default(),
        source_topology_bundle_id: CacheBundleId::new("topology-test"),
        acceleration: None,
        edge_metrics: vec![
            CompiledEdgeMetric {
                edge_id: EdgeId(0),
                travel_time_s: Some(5.0),
                generalized_cost: Some(5.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(1),
                travel_time_s: Some(5.0),
                generalized_cost: Some(5.0),
            },
        ],
    }
}

pub(super) fn disconnected_metrics() -> CompiledProfileBundle {
    CompiledProfileBundle {
        schema_version: 3,
        profile_id: "test".to_string(),
        profile_hash: "abc".to_string(),
        mode: TravelMode::Car,
        turn_costs: CompiledTurnCostConfig::default(),
        source_topology_bundle_id: CacheBundleId::new("topology-test"),
        acceleration: None,
        edge_metrics: vec![
            CompiledEdgeMetric {
                edge_id: EdgeId(0),
                travel_time_s: Some(5.0),
                generalized_cost: Some(5.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(1),
                travel_time_s: Some(5.0),
                generalized_cost: Some(5.0),
            },
        ],
    }
}

pub(super) fn write_temp_file(name: &str, contents: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time works")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("netweevil-query-{unique}-{name}"));
    fs::write(&path, contents).expect("fixture written");
    path
}
