use std::path::{Path, PathBuf};

use anyhow::Result;
use netweevil_core::{
    ConnectedComponentKind, ConnectedComponentsMeta, DirectedEdge, EDGE_FLAG_ROUNDABOUT,
    EDGE_FLAG_TARGET_TRAFFIC_SIGNAL, EdgeBasedTopology, EdgeId, EdgeNameBundle, NodeId,
    TOPOLOGY_BUNDLE_SCHEMA_VERSION, TopologyBundle, TopologyBundleMeta, TopologyNode,
    VehicleRestriction, VehicleRestrictionTable,
};
use std::collections::BTreeMap;

use rustc_hash::FxHashMap;

use crate::guidance::GuidanceInterner;
use crate::import::{DatasetImportProgress, DatasetImportStage, PercentReporter, emit_progress};
use crate::restrictions::build_turn_restrictions;
use crate::scan::{PendingWay, ScanOutput};
use crate::source::OsmSourceState;
use netweevil_core::SourceFormat;

/// A topology built from a dataset source.
pub(crate) struct BuiltTopology {
    pub(crate) bundle: TopologyBundle,
    pub(crate) names: EdgeNameBundle,
    pub(crate) meta: TopologyBundleMeta,
    /// For OSM sources: the scanned source objects and the OSM ids of the
    /// topology nodes, which become the dataset's source snapshot.
    pub(crate) osm_source: Option<(OsmSourceState, Vec<i64>)>,
}

pub(crate) fn build_topology_bundle(
    source_paths: &[PathBuf],
    source_size_bytes: u64,
    source_sha256: &str,
    source_format: SourceFormat,
    mapping: Option<&crate::GeoPackageMapping>,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<BuiltTopology> {
    let source_path = source_paths
        .first()
        .ok_or_else(|| anyhow::anyhow!("dataset import requires at least one source"))?;
    let mut osm_source = None;
    let scan = match source_format {
        SourceFormat::OsmPbf => {
            let state = OsmSourceState::scan_pbf(source_path, source_size_bytes, progress)?;
            let (scan, topology_node_ids) = state.scan_output();
            osm_source = Some((state, topology_node_ids));
            scan
        }
        SourceFormat::OvertureParquet => {
            crate::overture::scan_overture(source_path, source_size_bytes, progress)?
        }
        SourceFormat::GeoPackage => crate::geopackage::scan_geopackages(
            source_paths,
            mapping.ok_or_else(|| anyhow::anyhow!("GeoPackage import requires a mapping"))?,
            progress,
        )?,
    };
    let (mut bundle, names, meta) =
        build_topology_from_scan(source_path, source_sha256, scan, progress)?;
    bundle.source_path = source_paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(";");
    Ok(BuiltTopology {
        bundle,
        names,
        meta,
        osm_source,
    })
}

pub(crate) fn build_topology_from_scan(
    source_path: &Path,
    source_sha256: &str,
    scan: ScanOutput,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<(TopologyBundle, EdgeNameBundle, TopologyBundleMeta)> {
    let ScanOutput {
        pending_ways,
        restriction_candidates,
        node_coords,
        traffic_signal_nodes,
        junction_nodes,
        counts,
        feature_attributes,
        temporal_rule_sets,
    } = scan;
    let (nodes, node_lookup) = build_nodes(&node_coords);
    let name_lookup = build_name_lookup(&pending_ways);

    emit_progress(
        progress,
        DatasetImportStage::BuildTopology,
        Some(0.0),
        format!(
            "Building topology from {} routable ways and {} node coordinates",
            pending_ways.len(),
            node_coords.len()
        ),
    );
    let spatial_index = build_spatial_index(&nodes);

    let mut edges = Vec::new();
    let mut edge_highways = Vec::new();
    let mut guidance = GuidanceInterner::new();
    // (destination-only, conditional access) per edge.
    let mut edge_masks = Vec::new();
    let mut vehicle_restrictions = VehicleRestrictionTableBuilder::default();
    let mut skipped_way_count = 0_u64;
    let mut build_reporter = PercentReporter::starting_at_zero();
    for (way_index, way) in pending_ways.iter().enumerate() {
        let name_index = way
            .name
            .as_ref()
            .and_then(|name| name_lookup.get(name))
            .copied();
        let mut segments = Vec::new();

        for pair in way.node_ids.windows(2) {
            let from = pair[0];
            let to = pair[1];
            if from == to {
                continue;
            }

            let Some(&(from_id, from_lon, from_lat, from_z)) = node_lookup.get(&from) else {
                continue;
            };
            let Some(&(to_id, to_lon, to_lat, to_z)) = node_lookup.get(&to) else {
                continue;
            };

            let length_m =
                distance_3d_meters(from_lon, from_lat, from_z, to_lon, to_lat, to_z).round() as u32;
            let (ascent_m, descent_m) = ascent_descent_meters(from_z, to_z);
            segments.push((from_id, to_id, from, to, length_m, ascent_m, descent_m));
        }

        if segments.is_empty() {
            skipped_way_count += 1;
            continue;
        }

        let total_length_m = segments
            .iter()
            .map(|(_, _, _, _, length_m, _, _)| *length_m as u64)
            .sum();
        let segment_count = segments.len() as u32;
        for (from_id, to_id, from_osm_id, to_osm_id, length_m, ascent_m, descent_m) in segments {
            let duration_s =
                apportioned_duration_s(way.duration_s, length_m, total_length_m, segment_count);
            let mut forward_flags = 0_u32;
            let mut reverse_flags = 0_u32;
            if way.is_roundabout {
                forward_flags |= EDGE_FLAG_ROUNDABOUT;
                reverse_flags |= EDGE_FLAG_ROUNDABOUT;
            }
            if traffic_signal_nodes.contains(&to_osm_id) {
                forward_flags |= EDGE_FLAG_TARGET_TRAFFIC_SIGNAL;
            }
            if traffic_signal_nodes.contains(&from_osm_id) {
                reverse_flags |= EDGE_FLAG_TARGET_TRAFFIC_SIGNAL;
            }

            if way.has_forward_edge() {
                edge_masks.push((way.forward_destination_only, way.forward_conditional_access));
                vehicle_restrictions.add_edge(edges.len(), &way.vehicle_restrictions);
                edges.push(DirectedEdge {
                    edge_id: EdgeId(edges.len() as u32),
                    from: from_id,
                    to: to_id,
                    source_way_id: way.osm_way_id,
                    length_m,
                    ascent_m,
                    descent_m,
                    feature_row: way.feature_row,
                    source_direction: 1,
                    temporal_rule_id: way.forward_temporal_rule_id,
                    duration_s,
                    road_class: way.road_class,
                    surface: way.surface,
                    smoothness: way.smoothness,
                    access_mask: way.forward_access_mask,
                    is_toll: way.is_toll,
                    max_speed_kph: way.forward_max_speed_kph,
                    lanes: way.forward_lanes,
                    name_index,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: forward_flags | way.forward_extra_flags,
                });
                edge_highways.push(way.highway);
                guidance.push_edge(
                    way.guidance.as_deref(),
                    true,
                    way.highway,
                    junction_nodes.get(&from_osm_id),
                );
            }

            if way.has_reverse_edge() {
                edge_masks.push((way.reverse_destination_only, way.reverse_conditional_access));
                vehicle_restrictions.add_edge(edges.len(), &way.vehicle_restrictions);
                edges.push(DirectedEdge {
                    edge_id: EdgeId(edges.len() as u32),
                    from: to_id,
                    to: from_id,
                    source_way_id: way.osm_way_id,
                    length_m,
                    ascent_m: descent_m,
                    descent_m: ascent_m,
                    feature_row: way.feature_row,
                    source_direction: -1,
                    temporal_rule_id: way.reverse_temporal_rule_id,
                    duration_s,
                    road_class: way.road_class,
                    surface: way.surface,
                    smoothness: way.smoothness,
                    access_mask: way.reverse_access_mask,
                    is_toll: way.is_toll,
                    max_speed_kph: way.reverse_max_speed_kph,
                    lanes: way.reverse_lanes,
                    name_index,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: reverse_flags | way.reverse_extra_flags,
                });
                edge_highways.push(way.highway);
                guidance.push_edge(
                    way.guidance.as_deref(),
                    false,
                    way.highway,
                    junction_nodes.get(&to_osm_id),
                );
            }
        }

        build_reporter.emit_if_needed(
            (way_index + 1) as u64,
            pending_ways.len() as u64,
            DatasetImportStage::BuildTopology,
            progress,
            |percent| {
                format!(
                    "Building topology {:.0}% ({}/{})",
                    percent,
                    way_index + 1,
                    pending_ways.len()
                )
            },
        );
    }

    emit_progress(
        progress,
        DatasetImportStage::BuildTopology,
        Some(100.0),
        format!(
            "Resolving {} turn restrictions",
            restriction_candidates.len()
        ),
    );
    let turn_restrictions =
        build_turn_restrictions(&restriction_candidates, &pending_ways, &node_lookup, &edges);

    let mut names = vec![String::new(); name_lookup.len()];
    for (name, index) in name_lookup {
        names[index as usize] = name;
    }

    let edge_name_bundle = guidance.finish(names);

    emit_progress(
        progress,
        DatasetImportStage::BuildTopology,
        Some(100.0),
        format!(
            "Building edge-transition topology from {} directed edges",
            edges.len()
        ),
    );
    let edge_based_topology = build_edge_based_topology(nodes.len(), &edges);
    let components = label_weak_components(nodes.len(), &edges);

    let mut edge_layers = netweevil_core::TopologyEdgeLayers::from_directed_edges(&edges);
    for ((profile, highway), (destination_only, conditional_access)) in edge_layers
        .profile
        .iter_mut()
        .zip(edge_highways)
        .zip(edge_masks)
    {
        profile.highway = highway;
        profile.destination_only = destination_only;
        profile.conditional_access = conditional_access;
    }
    edge_layers.vehicle_restrictions = vehicle_restrictions.finish();
    let bundle = TopologyBundle {
        schema_version: TOPOLOGY_BUNDLE_SCHEMA_VERSION,
        source_path: source_path.display().to_string(),
        source_sha256: source_sha256.to_string(),
        nodes,
        edge_layers,
        turn_restrictions,
        names: Vec::new(),
        edge_based_topology,
        spatial_index,
        node_component_ids: components.node_component_ids.clone(),
        edge_component_ids: components.edge_component_ids.clone(),
        feature_attributes,
        temporal_rule_sets,
    };
    let meta = TopologyBundleMeta {
        node_count: bundle.nodes.len() as u64,
        edge_count: bundle.edge_count() as u64,
        geometry_bytes: 0,
        turn_count: bundle.turn_restrictions.len() as u64,
        connected_components: Some(ConnectedComponentsMeta {
            kind: ConnectedComponentKind::Weak,
            component_count: components.component_count,
            largest_component_node_count: components.largest_component_node_count,
            largest_component_edge_count: components.largest_component_edge_count,
        }),
        source_node_count: counts.nodes,
        source_way_count: counts.ways,
        source_relation_count: counts.relations,
        routable_way_count: pending_ways.len() as u64,
        skipped_way_count,
    };
    Ok((bundle, edge_name_bundle, meta))
}

/// Interns way-level restrictions into the sparse per-edge table. Edges are
/// added in index order, so rows stay sorted.
#[derive(Default)]
pub(crate) struct VehicleRestrictionTableBuilder {
    table: VehicleRestrictionTable,
    ids: FxHashMap<VehicleRestriction, u32>,
}

impl VehicleRestrictionTableBuilder {
    pub(crate) fn add_edge(&mut self, edge_index: usize, restrictions: &[VehicleRestriction]) {
        for restriction in restrictions {
            let next_id = self.table.restrictions.len() as u32;
            let id = *self.ids.entry(*restriction).or_insert_with(|| {
                self.table.restrictions.push(*restriction);
                next_id
            });
            self.table.edges.push(edge_index as u32);
            self.table.edge_restrictions.push(id);
        }
    }

    pub(crate) fn finish(self) -> VehicleRestrictionTable {
        self.table
    }
}

pub(crate) fn build_edge_based_topology(
    node_count: usize,
    edges: &[DirectedEdge],
) -> EdgeBasedTopology {
    EdgeBasedTopology::build(node_count, edges.len(), |edge_index| {
        (edges[edge_index].from.0, edges[edge_index].to.0)
    })
}

#[derive(Debug, Clone)]
struct WeakComponentLabels {
    node_component_ids: Vec<u32>,
    edge_component_ids: Vec<u32>,
    component_count: u32,
    largest_component_node_count: u64,
    largest_component_edge_count: u64,
}

fn label_weak_components(node_count: usize, edges: &[DirectedEdge]) -> WeakComponentLabels {
    let mut parent = (0..node_count as u32).collect::<Vec<_>>();
    let mut rank = vec![0_u8; node_count];

    for edge in edges {
        union_components(
            &mut parent,
            &mut rank,
            edge.from.0 as usize,
            edge.to.0 as usize,
        );
    }

    let mut root_to_component = BTreeMap::<u32, u32>::new();
    let mut node_counts = Vec::<u64>::new();
    let mut node_component_ids = vec![0_u32; node_count];

    for (node_index, component_slot) in node_component_ids.iter_mut().enumerate() {
        let root = find_component_root(&mut parent, node_index);
        let component_id = if let Some(&component_id) = root_to_component.get(&root) {
            component_id
        } else {
            let component_id = root_to_component.len() as u32;
            root_to_component.insert(root, component_id);
            node_counts.push(0);
            component_id
        };
        *component_slot = component_id;
        node_counts[component_id as usize] += 1;
    }

    let mut edge_counts = vec![0_u64; node_counts.len()];
    let edge_component_ids = edges
        .iter()
        .map(|edge| {
            let component_id = node_component_ids[edge.from.0 as usize];
            edge_counts[component_id as usize] += 1;
            component_id
        })
        .collect::<Vec<_>>();

    WeakComponentLabels {
        node_component_ids,
        edge_component_ids,
        component_count: node_counts.len() as u32,
        largest_component_node_count: node_counts.into_iter().max().unwrap_or_default(),
        largest_component_edge_count: edge_counts.into_iter().max().unwrap_or_default(),
    }
}

fn find_component_root(parent: &mut [u32], index: usize) -> u32 {
    // Iterative two-pass find with full path compression: long parent
    // chains on continent graphs would overflow the stack recursively.
    let mut root = index as u32;
    while parent[root as usize] != root {
        root = parent[root as usize];
    }
    let mut cursor = index as u32;
    while parent[cursor as usize] != root {
        let next = parent[cursor as usize];
        parent[cursor as usize] = root;
        cursor = next;
    }
    root
}

fn union_components(parent: &mut [u32], rank: &mut [u8], left: usize, right: usize) {
    let left_root = find_component_root(parent, left);
    let right_root = find_component_root(parent, right);
    if left_root == right_root {
        return;
    }

    let left_rank = rank[left_root as usize];
    let right_rank = rank[right_root as usize];
    if left_rank < right_rank {
        parent[left_root as usize] = right_root;
    } else if left_rank > right_rank {
        parent[right_root as usize] = left_root;
    } else if left_root <= right_root {
        parent[right_root as usize] = left_root;
        rank[left_root as usize] += 1;
    } else {
        parent[left_root as usize] = right_root;
        rank[right_root as usize] += 1;
    }
}

fn build_nodes(
    node_coords: &FxHashMap<i64, (f64, f64, f64)>,
) -> (Vec<TopologyNode>, FxHashMap<i64, (NodeId, f64, f64, f64)>) {
    let mut used_nodes = node_coords.keys().copied().collect::<Vec<_>>();
    used_nodes.sort_unstable();

    let mut nodes = Vec::with_capacity(used_nodes.len());
    let mut lookup = FxHashMap::with_capacity_and_hasher(used_nodes.len(), Default::default());
    for osm_node_id in used_nodes {
        let Some(&(lon, lat, z)) = node_coords.get(&osm_node_id) else {
            continue;
        };
        let node_id = NodeId(nodes.len() as u32);
        nodes.push(TopologyNode {
            node_id,
            lon,
            lat,
            z,
        });
        lookup.insert(osm_node_id, (node_id, lon, lat, z));
    }

    (nodes, lookup)
}

fn build_name_lookup(ways: &[PendingWay]) -> BTreeMap<String, u32> {
    let mut distinct_names = ways
        .iter()
        .filter_map(|way| way.name.clone())
        .collect::<Vec<_>>();
    distinct_names.sort_unstable();
    distinct_names.dedup();

    distinct_names
        .into_iter()
        .enumerate()
        .map(|(index, name)| (name, index as u32))
        .collect()
}

fn build_spatial_index(nodes: &[TopologyNode]) -> Option<netweevil_core::NodeSpatialIndex> {
    netweevil_core::NodeSpatialIndex::build(nodes)
}

fn apportioned_duration_s(
    total_duration_s: Option<f64>,
    segment_length_m: u32,
    total_length_m: u64,
    segment_count: u32,
) -> Option<f64> {
    let total_duration_s = total_duration_s?;
    if total_length_m > 0 {
        return Some(total_duration_s * segment_length_m as f64 / total_length_m as f64);
    }
    if segment_count > 0 {
        return Some(total_duration_s / segment_count as f64);
    }
    None
}

use netweevil_core::geo::{ascent_descent_meters, distance_3d_meters};

#[cfg(test)]
mod tests;
