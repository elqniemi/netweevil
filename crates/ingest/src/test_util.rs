use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::scan::PendingWay;
use crate::topology::build_edge_based_topology;
use netweevil_core::{
    AccessMask, DirectedEdge, EdgeId, HighwayClass, NO_FEATURE_ROW, NodeId, RoadClass,
    SurfaceClass, TOPOLOGY_BUNDLE_SCHEMA_VERSION, TopologyBundle, TopologyEdgeLayers, TopologyNode,
};

pub(crate) fn pending_way(osm_way_id: i64, node_ids: &[i64]) -> PendingWay {
    PendingWay {
        osm_way_id,
        node_ids: node_ids.to_vec(),
        road_class: RoadClass::Residential,
        highway: HighwayClass::Residential,
        duration_s: None,
        surface: SurfaceClass::Asphalt,
        smoothness: Default::default(),
        forward_access_mask: AccessMask::new(AccessMask::CAR),
        reverse_access_mask: AccessMask::new(AccessMask::CAR),
        is_toll: false,
        is_roundabout: false,
        forward_extra_flags: 0,
        reverse_extra_flags: 0,
        forward_max_speed_kph: None,
        reverse_max_speed_kph: None,
        forward_lanes: None,
        reverse_lanes: None,
        name: None,
        feature_row: NO_FEATURE_ROW,
        temporal_rule_id: None,
    }
}

pub(crate) fn edge(edge_id: u32, from: u32, to: u32, source_way_id: i64) -> DirectedEdge {
    DirectedEdge {
        edge_id: EdgeId(edge_id),
        from: NodeId(from),
        to: NodeId(to),
        source_way_id,
        length_m: 100,
        ascent_m: 0.0,
        descent_m: 0.0,
        feature_row: NO_FEATURE_ROW,
        source_direction: 1,
        temporal_rule_id: None,
        duration_s: None,
        road_class: RoadClass::Residential,
        surface: SurfaceClass::Asphalt,
        smoothness: Default::default(),
        access_mask: AccessMask::new(AccessMask::CAR),
        is_toll: false,
        max_speed_kph: None,
        lanes: None,
        name_index: None,
        geometry_offset: 0,
        geometry_len: 0,
        flags: 0,
    }
}

/// Single-component topology over `(lon, lat, z)` nodes with its
/// edge-based transition graph built from `edges`.
pub(crate) fn topology(nodes: &[(f64, f64, f64)], edges: &[DirectedEdge]) -> TopologyBundle {
    TopologyBundle {
        schema_version: TOPOLOGY_BUNDLE_SCHEMA_VERSION,
        source_path: "test".to_string(),
        source_sha256: "abc".to_string(),
        nodes: nodes
            .iter()
            .enumerate()
            .map(|(node_id, &(lon, lat, z))| TopologyNode {
                node_id: NodeId(node_id as u32),
                lon,
                lat,
                z,
            })
            .collect(),
        edge_layers: TopologyEdgeLayers::from_directed_edges(edges),
        turn_restrictions: Vec::new(),
        names: Vec::new(),
        edge_based_topology: build_edge_based_topology(nodes.len(), edges),
        spatial_index: None,
        node_component_ids: vec![0; nodes.len()],
        edge_component_ids: vec![0; edges.len()],
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
    }
}

/// Fresh directory under the system temp dir, removed on drop.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "netweevil-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir creates");
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    pub(crate) fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// GeoPackage binary header (EPSG:4326, no envelope) followed by a
/// little-endian WKB LineString Z.
pub(crate) fn gpkg_linestring_z(points: &[[f64; 3]]) -> Vec<u8> {
    let mut blob = b"GP\0\x01".to_vec();
    blob.extend_from_slice(&4326_i32.to_le_bytes());
    blob.push(1);
    blob.extend_from_slice(&1002_u32.to_le_bytes());
    blob.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for value in points.iter().flatten() {
        blob.extend_from_slice(&value.to_le_bytes());
    }
    blob
}
