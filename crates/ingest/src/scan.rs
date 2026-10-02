use std::fs::File;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use netweevil_core::{
    AccessMask, FeatureAttributeTable, HighwayClass, NO_FEATURE_ROW, RoadClass, SmoothnessClass,
    SurfaceClass, TemporalRuleSet,
};
use osmpbfreader::{OsmObj, OsmPbfReader};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::classify::{
    classify_directional_access, classify_smoothness, classify_speed_and_lanes, classify_surface,
    classify_toll, classify_way, is_traffic_signal_node, parse_duration_from_tags, tag,
};
use crate::guidance::{JunctionNodeTags, WayGuidanceTags, junction_node_tags, way_guidance_tags};
use crate::import::{
    CountingReader, DatasetImportProgress, DatasetImportStage, PercentReporter, emit_progress,
};
use crate::restrictions::{TurnRestrictionCandidate, ViaSpec, parse_turn_restriction_relation};

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
    /// `highway=motorway_junction` nodes with exit numbers or names.
    pub(crate) junction_nodes: FxHashMap<i64, JunctionNodeTags>,
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

pub(crate) fn scan_routable_objects(
    source_path: &Path,
    source_size_bytes: u64,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<(
    Vec<PendingWay>,
    Vec<TurnRestrictionCandidate>,
    FxHashSet<i64>,
    FxHashSet<i64>,
    FxHashMap<i64, JunctionNodeTags>,
    ObjectCounts,
)> {
    let file = File::open(source_path)
        .with_context(|| format!("opening dataset source {}", source_path.display()))?;
    let bytes_read = Arc::new(AtomicU64::new(0));
    let counting_file = CountingReader::new(file, Arc::clone(&bytes_read));
    let mut reader = OsmPbfReader::new(counting_file);
    let mut ways = Vec::new();
    let mut restriction_candidates = Vec::new();
    let mut node_ids = FxHashSet::default();
    let mut traffic_signal_nodes = FxHashSet::default();
    let mut junction_nodes = FxHashMap::default();
    let mut counts = ObjectCounts::default();
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
                if let Some(junction) = junction_node_tags(&node.tags) {
                    junction_nodes.insert(node.id.0, junction);
                }
            }
            OsmObj::Way(way) => {
                counts.ways += 1;
                let Some((road_class, highway, access_mask)) = classify_way(&way.tags) else {
                    continue;
                };
                if way.nodes.len() < 2 {
                    continue;
                }
                let directional_access =
                    classify_directional_access(&way.tags, highway, road_class, access_mask);
                let speed_lanes = classify_speed_and_lanes(&way.tags, road_class);
                let (forward_oneway, backward_oneway) = oneway_directions(
                    directional_access.forward_access,
                    directional_access.reverse_access,
                );

                let pending = PendingWay {
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
                    guidance: way_guidance_tags(&way.tags, forward_oneway, backward_oneway),
                    feature_row: NO_FEATURE_ROW,
                    temporal_rule_id: None,
                };

                for node_id in &pending.node_ids {
                    node_ids.insert(*node_id);
                }
                ways.push(pending);
            }
            OsmObj::Relation(relation) => {
                counts.relations += 1;
                if let Some(candidate) = parse_turn_restriction_relation(&relation) {
                    if let ViaSpec::Node(via_node_id) = candidate.via {
                        node_ids.insert(via_node_id);
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
            "Scanning routable OSM objects 100% ({} ways kept, {} restrictions)",
            ways.len(),
            restriction_candidates.len()
        ),
    );

    Ok((
        ways,
        restriction_candidates,
        node_ids,
        traffic_signal_nodes,
        junction_nodes,
        counts,
    ))
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
    let (
        pending_ways,
        restriction_candidates,
        needed_nodes,
        traffic_signal_nodes,
        mut junction_nodes,
        counts,
    ) = scan_routable_objects(source_path, source_size_bytes, progress)?;
    junction_nodes.retain(|node_id, _| needed_nodes.contains(node_id));
    let node_coords = load_node_coords(source_path, source_size_bytes, &needed_nodes, progress)?;
    Ok(ScanOutput {
        pending_ways,
        restriction_candidates,
        node_coords,
        traffic_signal_nodes,
        junction_nodes,
        counts,
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
    })
}
