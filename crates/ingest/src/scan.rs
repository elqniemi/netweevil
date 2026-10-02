use netweevil_core::{
    AccessMask, FeatureAttributeTable, HighwayClass, NO_FEATURE_ROW, RoadClass, SmoothnessClass,
    SurfaceClass, TemporalRuleSet,
};
use osmpbfreader::Way;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::classify::{
    classify_directional_access, classify_smoothness, classify_speed_and_lanes, classify_surface,
    classify_toll, classify_way, parse_duration_from_tags, tag,
};
use crate::restrictions::TurnRestrictionCandidate;

/// One routable source way after classification. OSM sources persist these
/// records in the dataset's source snapshot, so every field is serialized.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PendingWay {
    pub(crate) osm_way_id: i64,
    pub(crate) node_ids: Vec<i64>,
    pub(crate) road_class: RoadClass,
    pub(crate) highway: HighwayClass,
    pub(crate) duration_s: Option<f64>,
    pub(crate) surface: SurfaceClass,
    pub(crate) smoothness: SmoothnessClass,
    pub(crate) forward_access_mask: AccessMask,
    pub(crate) reverse_access_mask: AccessMask,
    pub(crate) is_toll: bool,
    pub(crate) is_roundabout: bool,
    pub(crate) forward_extra_flags: u32,
    pub(crate) reverse_extra_flags: u32,
    pub(crate) forward_max_speed_kph: Option<f32>,
    pub(crate) reverse_max_speed_kph: Option<f32>,
    pub(crate) forward_lanes: Option<u8>,
    pub(crate) reverse_lanes: Option<u8>,
    pub(crate) name: Option<String>,
    pub(crate) feature_row: u32,
    pub(crate) temporal_rule_id: Option<u32>,
}

/// Source-agnostic scan result consumed by the topology build. The OSM and
/// Overture scanners both produce this shape; ids are OSM object ids for PBF
/// sources and synthetic ids for Overture sources.
#[derive(Debug, Default)]
pub(crate) struct ScanOutput {
    pub(crate) pending_ways: Vec<PendingWay>,
    pub(crate) restriction_candidates: Vec<TurnRestrictionCandidate>,
    /// Longitude, latitude and elevation in metres. Sources without
    /// elevation data explicitly use zero; `NaN` is reserved for readers
    /// that need to represent an unknown source value.
    pub(crate) node_coords: FxHashMap<i64, (f64, f64, f64)>,
    pub(crate) traffic_signal_nodes: FxHashSet<i64>,
    pub(crate) counts: ObjectCounts,
    pub(crate) feature_attributes: FeatureAttributeTable,
    pub(crate) temporal_rule_sets: Vec<TemporalRuleSet>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ObjectCounts {
    pub(crate) nodes: u64,
    pub(crate) ways: u64,
    pub(crate) relations: u64,
}

/// Classifies one OSM way; `None` when it is not routable or has fewer
/// than two node references.
pub(crate) fn classify_routable_way(way: Way) -> Option<PendingWay> {
    let (road_class, highway, access_mask) = classify_way(&way.tags)?;
    if way.nodes.len() < 2 {
        return None;
    }
    let directional_access =
        classify_directional_access(&way.tags, highway, road_class, access_mask);
    let speed_lanes = classify_speed_and_lanes(&way.tags, road_class);
    Some(PendingWay {
        osm_way_id: way.id.0,
        node_ids: way.nodes.into_iter().map(|node| node.0).collect(),
        road_class,
        highway,
        duration_s: parse_duration_from_tags(&way.tags),
        surface: classify_surface(&way.tags),
        smoothness: classify_smoothness(&way.tags),
        forward_access_mask: directional_access.forward_access,
        reverse_access_mask: directional_access.reverse_access,
        is_toll: classify_toll(&way.tags),
        is_roundabout: tag(&way.tags, "junction") == Some("roundabout"),
        forward_extra_flags: directional_access.forward_flags,
        reverse_extra_flags: directional_access.reverse_flags,
        forward_max_speed_kph: speed_lanes.forward_max_speed_kph,
        reverse_max_speed_kph: speed_lanes.reverse_max_speed_kph,
        forward_lanes: speed_lanes.forward_lanes,
        reverse_lanes: speed_lanes.reverse_lanes,
        name: way.tags.get("name").map(ToString::to_string),
        feature_row: NO_FEATURE_ROW,
        temporal_rule_id: None,
    })
}
