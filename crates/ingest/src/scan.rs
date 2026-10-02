use std::fs::File;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use netweevil_core::{
    AccessMask, FeatureAttributeTable, HighwayClass, NO_FEATURE_ROW, RoadClass, SmoothnessClass,
    SurfaceClass, TemporalRule, TemporalRuleSet, VehicleRestriction,
};
use osmpbfreader::{OsmObj, OsmPbfReader};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::access::{ConditionalCounts, classify_way_access};
use crate::classify::{
    classify_smoothness, classify_speed_and_lanes, classify_surface, classify_toll, classify_way,
    conditional_speed_rules, is_traffic_signal_node, parse_duration_from_tags, tag,
};
use crate::import::{
    CountingReader, DatasetImportProgress, DatasetImportStage, PercentReporter, emit_progress,
};
use crate::restrictions::{TurnRestrictionCandidate, ViaSpec, parse_turn_restriction_relation};
use crate::vehicle_tags::way_vehicle_restrictions;

#[derive(Debug, Clone)]
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
#[derive(Debug, Default)]
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

#[derive(Debug, Default)]
pub(crate) struct ObjectCounts {
    pub(crate) nodes: u64,
    pub(crate) ways: u64,
    pub(crate) relations: u64,
}

/// First-pass result of an OSM scan.
struct RoutableObjects {
    ways: Vec<PendingWay>,
    restriction_candidates: Vec<TurnRestrictionCandidate>,
    needed_nodes: FxHashSet<i64>,
    traffic_signal_nodes: FxHashSet<i64>,
    counts: ObjectCounts,
    rule_sets: RuleSetInterner,
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
        feature_row: NO_FEATURE_ROW,
        forward_temporal_rule_id,
        reverse_temporal_rule_id,
        vehicle_restrictions,
    })
}

fn scan_routable_objects(
    source_path: &Path,
    source_size_bytes: u64,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<RoutableObjects> {
    let file = File::open(source_path)
        .with_context(|| format!("opening dataset source {}", source_path.display()))?;
    let bytes_read = Arc::new(AtomicU64::new(0));
    let counting_file = CountingReader::new(file, Arc::clone(&bytes_read));
    let mut reader = OsmPbfReader::new(counting_file);
    let mut ways = Vec::new();
    let mut restriction_candidates = Vec::new();
    let mut needed_nodes = FxHashSet::default();
    let mut traffic_signal_nodes = FxHashSet::default();
    let mut counts = ObjectCounts::default();
    let mut rule_sets = RuleSetInterner::default();
    let mut conditionals = ConditionalCounts::default();
    let mut reporter = PercentReporter::starting_at_zero();

    emit_progress(
        progress,
        DatasetImportStage::ScanRoutableObjects,
        Some(0.0),
        "Scanning routable OSM objects 0%".to_string(),
    );

    for object in reader.iter() {
        match object.context("reading PBF object")? {
            OsmObj::Node(node) => {
                counts.nodes += 1;
                if is_traffic_signal_node(&node.tags) {
                    traffic_signal_nodes.insert(node.id.0);
                }
            }
            OsmObj::Way(way) => {
                counts.ways += 1;
                let Some(pending) = pending_osm_way(
                    way.id.0,
                    way.nodes.iter().map(|node| node.0).collect(),
                    &way.tags,
                    &mut rule_sets,
                    &mut conditionals,
                ) else {
                    continue;
                };
                needed_nodes.extend(pending.node_ids.iter().copied());
                ways.push(pending);
            }
            OsmObj::Relation(relation) => {
                counts.relations += 1;
                for candidate in parse_turn_restriction_relation(&relation, &mut rule_sets) {
                    if let ViaSpec::Node(via_node_id) = candidate.via {
                        needed_nodes.insert(via_node_id);
                    }
                    restriction_candidates.push(candidate);
                }
            }
        }

        reporter.emit_if_needed(
            bytes_read.load(Ordering::Relaxed),
            source_size_bytes,
            DatasetImportStage::ScanRoutableObjects,
            progress,
            |percent| {
                format!(
                    "Scanning routable OSM objects {:.0}% ({} ways kept, {} restrictions)",
                    percent,
                    ways.len(),
                    restriction_candidates.len()
                )
            },
        );
    }

    emit_progress(
        progress,
        DatasetImportStage::ScanRoutableObjects,
        Some(100.0),
        format!(
            "Scanning routable OSM objects 100% ({} ways kept, {} restrictions, {} conditional \
             tags imported, {} skipped as unsupported)",
            ways.len(),
            restriction_candidates.len(),
            conditionals.imported,
            conditionals.skipped
        ),
    );

    Ok(RoutableObjects {
        ways,
        restriction_candidates,
        needed_nodes,
        traffic_signal_nodes,
        counts,
        rule_sets,
    })
}

pub(crate) fn load_node_coords(
    source_path: &Path,
    _source_size_bytes: u64,
    needed_nodes: &FxHashSet<i64>,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<FxHashMap<i64, (f64, f64, f64)>> {
    let file = File::open(source_path)
        .with_context(|| format!("opening dataset source {}", source_path.display()))?;
    let bytes_read = Arc::new(AtomicU64::new(0));
    let counting_file = CountingReader::new(file, Arc::clone(&bytes_read));
    let mut reader = OsmPbfReader::new(counting_file);
    let mut coords = FxHashMap::with_capacity_and_hasher(needed_nodes.len(), Default::default());
    let mut reporter = PercentReporter::new();

    emit_progress(
        progress,
        DatasetImportStage::LoadNodeCoords,
        Some(0.0),
        format!("Loading node coordinates 0% (0/{})", needed_nodes.len()),
    );

    for object in reader.iter() {
        let OsmObj::Node(node) = object.context("reading PBF node")? else {
            continue;
        };
        let node_id = node.id.0;
        if needed_nodes.contains(&node_id) {
            coords.insert(node_id, (node.lon(), node.lat(), 0.0));
            if coords.len() == needed_nodes.len() {
                break;
            }
        }

        reporter.emit_if_needed(
            coords.len() as u64,
            needed_nodes.len() as u64,
            DatasetImportStage::LoadNodeCoords,
            progress,
            |percent| {
                format!(
                    "Loading node coordinates {:.0}% ({}/{})",
                    percent,
                    coords.len(),
                    needed_nodes.len()
                )
            },
        );
    }

    emit_progress(
        progress,
        DatasetImportStage::LoadNodeCoords,
        Some(100.0),
        format!(
            "Loading node coordinates 100% ({}/{})",
            coords.len(),
            needed_nodes.len()
        ),
    );

    Ok(coords)
}

/// Runs both PBF passes and packages the result for the topology build.
pub(crate) fn scan_osm(
    source_path: &Path,
    source_size_bytes: u64,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<ScanOutput> {
    let objects = scan_routable_objects(source_path, source_size_bytes, progress)?;
    let node_coords = load_node_coords(
        source_path,
        source_size_bytes,
        &objects.needed_nodes,
        progress,
    )?;
    Ok(ScanOutput {
        pending_ways: objects.ways,
        restriction_candidates: objects.restriction_candidates,
        node_coords,
        traffic_signal_nodes: objects.traffic_signal_nodes,
        counts: objects.counts,
        feature_attributes: Default::default(),
        temporal_rule_sets: objects.rule_sets.into_sets(),
    })
}
