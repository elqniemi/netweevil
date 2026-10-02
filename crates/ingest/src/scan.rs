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

use crate::admin::{
    AdminBoundary, AdminRelationCandidate, assemble_boundaries, boundary_node_ids,
    load_boundary_ways, parse_admin_relation,
};
use crate::classify::{
    classify_directional_access, classify_smoothness, classify_speed_and_lanes, classify_surface,
    classify_toll, classify_way, is_traffic_signal_node, parse_duration_from_tags, tag,
};
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
    /// Ways (by `osm_way_id`) on bridges or in tunnels, whose nodes do not
    /// sit on the terrain.
    pub(crate) structure_ways: FxHashSet<i64>,
    /// Admin boundaries assembled from the source extract.
    pub(crate) admin_boundaries: Vec<AdminBoundary>,
    /// Admin relations in the extract that could not be assembled.
    pub(crate) incomplete_admin_relations: Vec<i64>,
}

/// Whether OSM tags put a way on a bridge or in a tunnel. Building passages
/// run at ground level.
pub(crate) fn is_structure_way(tags: &osmpbfreader::Tags) -> bool {
    let value = |key: &str| tags.get(key).map(|value| value.as_str());
    value("bridge").is_some_and(|bridge| bridge != "no")
        || value("tunnel").is_some_and(|tunnel| tunnel != "no" && tunnel != "building_passage")
}

/// First-pass result over an OSM PBF.
pub(crate) struct RoutableScan {
    pub(crate) ways: Vec<PendingWay>,
    pub(crate) restriction_candidates: Vec<TurnRestrictionCandidate>,
    pub(crate) needed_nodes: FxHashSet<i64>,
    pub(crate) traffic_signal_nodes: FxHashSet<i64>,
    pub(crate) counts: ObjectCounts,
    pub(crate) structure_ways: FxHashSet<i64>,
    pub(crate) admin_relations: Vec<AdminRelationCandidate>,
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
) -> Result<RoutableScan> {
    let file = File::open(source_path)
        .with_context(|| format!("opening dataset source {}", source_path.display()))?;
    let bytes_read = Arc::new(AtomicU64::new(0));
    let counting_file = CountingReader::new(file, Arc::clone(&bytes_read));
    let mut reader = OsmPbfReader::new(counting_file);
    let mut ways = Vec::new();
    let mut restriction_candidates = Vec::new();
    let mut node_ids = FxHashSet::default();
    let mut traffic_signal_nodes = FxHashSet::default();
    let mut structure_ways = FxHashSet::default();
    let mut admin_relations = Vec::new();
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
            }
            OsmObj::Way(way) => {
                counts.ways += 1;
                let Some((road_class, highway, access_mask)) = classify_way(&way.tags) else {
                    continue;
                };
                if way.nodes.len() < 2 {
                    continue;
                }
                if is_structure_way(&way.tags) {
                    structure_ways.insert(way.id.0);
                }
                let directional_access =
                    classify_directional_access(&way.tags, highway, road_class, access_mask);
                let speed_lanes = classify_speed_and_lanes(&way.tags, road_class);

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
                } else if let Some(candidate) = parse_admin_relation(&relation) {
                    admin_relations.push(candidate);
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

    Ok(RoutableScan {
        ways,
        restriction_candidates,
        needed_nodes: node_ids,
        traffic_signal_nodes,
        counts,
        structure_ways,
        admin_relations,
    })
}

/// Routing node coordinates (with zero elevation) and, separately, the
/// coordinates of `boundary_nodes`.
pub(crate) fn load_node_coords(
    source_path: &Path,
    needed_nodes: &FxHashSet<i64>,
    boundary_nodes: &FxHashSet<i64>,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<(FxHashMap<i64, (f64, f64, f64)>, FxHashMap<i64, (f64, f64)>)> {
    let file = File::open(source_path)
        .with_context(|| format!("opening dataset source {}", source_path.display()))?;
    let bytes_read = Arc::new(AtomicU64::new(0));
    let counting_file = CountingReader::new(file, Arc::clone(&bytes_read));
    let mut reader = OsmPbfReader::new(counting_file);
    let mut coords = FxHashMap::with_capacity_and_hasher(needed_nodes.len(), Default::default());
    let mut boundary_coords =
        FxHashMap::with_capacity_and_hasher(boundary_nodes.len(), Default::default());
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
        if boundary_nodes.contains(&node_id) {
            boundary_coords.insert(node_id, (node.lon(), node.lat()));
        }
        if needed_nodes.contains(&node_id) {
            coords.insert(node_id, (node.lon(), node.lat(), 0.0));
        }
        if coords.len() == needed_nodes.len() && boundary_coords.len() == boundary_nodes.len() {
            break;
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

    Ok((coords, boundary_coords))
}

/// Runs the PBF passes and packages the result for the topology build. With
/// `collect_admin`, admin relations found in the extract are assembled; the
/// member ways cost one extra pass and their nodes load with the routing
/// nodes.
pub(crate) fn scan_osm(
    source_path: &Path,
    source_size_bytes: u64,
    collect_admin: bool,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<ScanOutput> {
    let scan = scan_routable_objects(source_path, source_size_bytes, progress)?;
    let admin_relations = if collect_admin {
        scan.admin_relations
    } else {
        Vec::new()
    };
    let admin_way_nodes = load_boundary_ways(source_path, &admin_relations)?;
    let (node_coords, boundary_coords) = load_node_coords(
        source_path,
        &scan.needed_nodes,
        &boundary_node_ids(&admin_way_nodes),
        progress,
    )?;
    let (admin_boundaries, incomplete_admin_relations) =
        assemble_boundaries(&admin_relations, &admin_way_nodes, &boundary_coords);
    Ok(ScanOutput {
        pending_ways: scan.ways,
        restriction_candidates: scan.restriction_candidates,
        node_coords,
        traffic_signal_nodes: scan.traffic_signal_nodes,
        counts: scan.counts,
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
        structure_ways: scan.structure_ways,
        admin_boundaries,
        incomplete_admin_relations,
    })
}
