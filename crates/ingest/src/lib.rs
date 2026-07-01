//! Dataset import pipeline: scans `.osm.pbf` extracts or Overture Maps
//! transportation GeoParquet, builds the routable topology (edge-based
//! graph, turn restrictions, connected components, road classification),
//! and preprocesses the CCH acceleration bundle used by the query engine.

mod acceleration;
mod classify;
mod import;
mod overture;
mod restrictions;
mod scan;
mod topology;

pub use import::{
    DatasetImportOptions, DatasetImportProgress, DatasetImportStage, detect_source_format,
    import_dataset, import_dataset_with_progress,
};

#[cfg(test)]
pub(crate) mod test_util {
    use crate::scan::PendingWay;
    use netweevil_core::{
        AccessMask, DirectedEdge, EdgeId, HighwayClass, NodeId, RoadClass, SurfaceClass,
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
        }
    }

    pub(crate) fn edge(edge_id: u32, from: u32, to: u32, source_way_id: i64) -> DirectedEdge {
        DirectedEdge {
            edge_id: EdgeId(edge_id),
            from: NodeId(from),
            to: NodeId(to),
            source_way_id,
            length_m: 100,
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
}
