use netweevil_core::{
    AccessMask, FeatureAttributeTable, HighwayClass, NO_FEATURE_ROW, RoadClass, SmoothnessClass,
    SurfaceClass, TemporalRule, TemporalRuleSet, VehicleRestriction,
};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::access::{ConditionalCounts, classify_way_access};
use crate::classify::{
    classify_smoothness, classify_speed_and_lanes, classify_surface, classify_toll, classify_way,
    conditional_speed_rules, parse_duration_from_tags, tag,
};
use crate::guidance::{JunctionNodeTags, WayGuidanceTags, way_guidance_tags};
use crate::restrictions::TurnRestrictionCandidate;
use crate::vehicle_tags::way_vehicle_restrictions;

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
    pub(crate) forward_destination_only: AccessMask,
    pub(crate) reverse_destination_only: AccessMask,
    pub(crate) forward_conditional_access: AccessMask,
    pub(crate) reverse_conditional_access: AccessMask,
    pub(crate) is_toll: bool,
    pub(crate) is_roundabout: bool,
    pub(crate) forward_extra_flags: u32,
    pub(crate) reverse_extra_flags: u32,
    pub(crate) forward_max_speed_kph: Option<f32>,
    pub(crate) reverse_max_speed_kph: Option<f32>,
    pub(crate) forward_lanes: Option<u8>,
    pub(crate) reverse_lanes: Option<u8>,
    pub(crate) name: Option<String>,
    /// Route number, lane and signpost tags; `None` for most ways.
    pub(crate) guidance: Option<Box<WayGuidanceTags>>,
    pub(crate) feature_row: u32,
    pub(crate) forward_temporal_rule_id: Option<u32>,
    pub(crate) reverse_temporal_rule_id: Option<u32>,
    pub(crate) vehicle_restrictions: Vec<VehicleRestriction>,
}

impl PendingWay {
    /// Whether the forward direction becomes an edge.
    pub(crate) fn has_forward_edge(&self) -> bool {
        self.forward_access_mask.0 | self.forward_conditional_access.0 != 0
    }

    pub(crate) fn has_reverse_edge(&self) -> bool {
        self.reverse_access_mask.0 | self.reverse_conditional_access.0 != 0
    }
}

/// Deduplicates temporal rule sets shared by many ways and relations.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RuleSetInterner {
    sets: Vec<TemporalRuleSet>,
    lookup: FxHashMap<String, u32>,
}

impl RuleSetInterner {
    pub(crate) fn intern(&mut self, set: TemporalRuleSet) -> u32 {
        let key = serde_json::to_string(&set).expect("rule sets serialize");
        *self.lookup.entry(key).or_insert_with(|| {
            self.sets.push(set);
            (self.sets.len() - 1) as u32
        })
    }

    /// Interns `rules` unless empty.
    pub(crate) fn intern_rules(&mut self, rules: Vec<TemporalRule>) -> Option<u32> {
        (!rules.is_empty()).then(|| self.intern(TemporalRuleSet { rules }))
    }

    pub(crate) fn into_sets(self) -> Vec<TemporalRuleSet> {
        self.sets
    }

    pub(crate) fn sets(&self) -> &[TemporalRuleSet] {
        &self.sets
    }
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
    /// Guidance tags of motorway-junction nodes, keyed by source node id.
    pub(crate) junction_nodes: FxHashMap<i64, JunctionNodeTags>,
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

/// Classifies one routable way's tags into a pending way.
pub(crate) fn pending_osm_way(
    osm_way_id: i64,
    node_ids: Vec<i64>,
    tags: &osmpbfreader::Tags,
    rule_sets: &mut RuleSetInterner,
    conditionals: &mut ConditionalCounts,
) -> Option<PendingWay> {
    let (road_class, highway) = classify_way(tags)?;
    if node_ids.len() < 2 {
        return None;
    }
    let access = classify_way_access(tags, highway, road_class);
    let speed_lanes = classify_speed_and_lanes(tags, road_class);
    let (forward_oneway, backward_oneway) =
        oneway_directions(access.forward.access, access.reverse.access);
    let (forward_speed_rules, reverse_speed_rules, speed_counts) = conditional_speed_rules(tags);
    *conditionals += access.conditionals;
    *conditionals += speed_counts;
    let mut vehicle_restrictions = way_vehicle_restrictions(tags);
    for restriction in access.vehicle_restrictions {
        if !vehicle_restrictions.contains(&restriction) {
            vehicle_restrictions.push(restriction);
        }
    }
    let mut direction_rules = |mut rules: Vec<TemporalRule>, speed_rules: Vec<TemporalRule>| {
        rules.extend(speed_rules);
        rule_sets.intern_rules(rules)
    };
    let forward_temporal_rule_id = direction_rules(access.forward.rules, forward_speed_rules);
    let reverse_temporal_rule_id = direction_rules(access.reverse.rules, reverse_speed_rules);

    Some(PendingWay {
        osm_way_id,
        node_ids,
        road_class,
        highway,
        duration_s: parse_duration_from_tags(tags),
        surface: classify_surface(tags),
        smoothness: classify_smoothness(tags),
        forward_access_mask: access.forward.access,
        reverse_access_mask: access.reverse.access,
        forward_destination_only: access.forward.destination_only,
        reverse_destination_only: access.reverse.destination_only,
        forward_conditional_access: access.forward.conditional_access,
        reverse_conditional_access: access.reverse.conditional_access,
        is_toll: classify_toll(tags),
        is_roundabout: tag(tags, "junction") == Some("roundabout"),
        forward_extra_flags: access.forward.flags,
        reverse_extra_flags: access.reverse.flags,
        forward_max_speed_kph: speed_lanes.forward_max_speed_kph,
        reverse_max_speed_kph: speed_lanes.reverse_max_speed_kph,
        forward_lanes: speed_lanes.forward_lanes,
        reverse_lanes: speed_lanes.reverse_lanes,
        name: tags.get("name").map(ToString::to_string),
        guidance: way_guidance_tags(tags, forward_oneway, backward_oneway),
        feature_row: NO_FEATURE_ROW,
        forward_temporal_rule_id,
        reverse_temporal_rule_id,
        vehicle_restrictions,
    })
}

/// Whether the way is one-way forward / backward for the most capable mode
/// that may use it at all (car, then bicycle, then foot), so foot or
/// contraflow cycling on a one-way street does not make it two-way.
fn oneway_directions(forward: AccessMask, reverse: AccessMask) -> (bool, bool) {
    [AccessMask::CAR, AccessMask::BICYCLE, AccessMask::FOOT]
        .into_iter()
        .find(|&mode| forward.contains(mode) || reverse.contains(mode))
        .map_or((false, false), |mode| {
            (
                forward.contains(mode) && !reverse.contains(mode),
                reverse.contains(mode) && !forward.contains(mode),
            )
        })
}
