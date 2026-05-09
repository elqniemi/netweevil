use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, bail};
use netan_core::{
    AccelerationBuildSettings, AccelerationBundleStats, AccessMask, BuildStage, CacheBundleId,
    ConnectedComponentKind, ConnectedComponentsMeta, DatasetAccelerationBundle, DatasetId,
    DirectedEdge, EDGE_FLAG_ROUNDABOUT, EDGE_FLAG_TARGET_TRAFFIC_SIGNAL, EdgeBasedTopology, EdgeId,
    EdgeNameBundle, NodeId, RoadClass, SmoothnessClass, SpatialIndexCell, SurfaceClass,
    TopologyBounds, TopologyBundle, TopologyBundleMeta, TopologyNode, TurnRestriction,
    TurnRestrictionKind,
};
use netan_persist::{
    WorkspacePaths, write_acceleration_bundle, write_dataset_manifest, write_edge_name_bundle,
    write_topology_bundle,
};
use netan_report::{BundleRef, DatasetManifest, now_rfc3339};
use osmpbfreader::{OsmId, OsmObj, OsmPbfReader, Relation, Tags};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct DatasetImportOptions {
    pub name: String,
    pub source: String,
    pub acceleration_settings: AccelerationBuildSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatasetImportStage {
    HashSource,
    ScanRoutableObjects,
    LoadNodeCoords,
    BuildTopology,
    BuildAcceleration,
    WriteTopologyBundle,
    WriteManifest,
    Complete,
}

impl DatasetImportStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::HashSource => "Hash Source",
            Self::ScanRoutableObjects => "Scan Routable Objects",
            Self::LoadNodeCoords => "Load Node Coords",
            Self::BuildTopology => "Build Topology",
            Self::BuildAcceleration => "Build Acceleration",
            Self::WriteTopologyBundle => "Write Topology Bundle",
            Self::WriteManifest => "Write Dataset Manifest",
            Self::Complete => "Complete",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DatasetImportProgress {
    pub stage: DatasetImportStage,
    pub stage_percent: Option<f64>,
    pub message: String,
}

pub fn import_dataset(
    paths: &WorkspacePaths,
    source_path: impl AsRef<Path>,
    options: DatasetImportOptions,
) -> Result<DatasetManifest> {
    import_dataset_with_progress(paths, source_path, options, |_| {})
}

pub fn import_dataset_with_progress<F>(
    paths: &WorkspacePaths,
    source_path: impl AsRef<Path>,
    options: DatasetImportOptions,
    mut progress: F,
) -> Result<DatasetManifest>
where
    F: FnMut(DatasetImportProgress),
{
    let source_path = source_path.as_ref();
    if !source_path.exists() {
        bail!("dataset source does not exist: {}", source_path.display());
    }

    let file = File::open(source_path)
        .with_context(|| format!("opening dataset source {}", source_path.display()))?;
    let size = file
        .metadata()
        .with_context(|| format!("reading metadata for {}", source_path.display()))?
        .len();
    emit_progress(
        &mut progress,
        DatasetImportStage::HashSource,
        Some(0.0),
        format!("Hashing source {}", source_path.display()),
    );
    let sha256 = sha256_file(file, size, &mut progress)?;
    let dataset_id = DatasetId::new(options.name);
    let bundle_id = CacheBundleId::new(format!("topology-{}-{}", dataset_id.0, &sha256[..12]));
    let bundle_path = paths
        .topology_bundles_dir
        .join(format!("{}.bin", bundle_id.0));
    let edge_name_bundle_id =
        CacheBundleId::new(format!("edge-names-{}-{}", dataset_id.0, &sha256[..12]));
    let edge_name_bundle_path = paths
        .edge_name_bundles_dir
        .join(format!("{}.bin", edge_name_bundle_id.0));
    let acceleration_bundle_id =
        CacheBundleId::new(format!("acceleration-{}-{}", dataset_id.0, &sha256[..12]));
    let acceleration_bundle_path = paths
        .acceleration_bundles_dir
        .join(format!("{}.bin", acceleration_bundle_id.0));
    let (bundle, edge_name_bundle, topology_meta) =
        build_topology_bundle(source_path, size, &sha256, &mut progress)?;
    let acceleration_bundle = build_dataset_acceleration_bundle_with_progress(
        &bundle,
        bundle_id.clone(),
        options.acceleration_settings.clone(),
        &mut progress,
    );
    emit_progress(
        &mut progress,
        DatasetImportStage::WriteTopologyBundle,
        None,
        format!("Writing topology bundle {}", bundle_path.display()),
    );
    write_topology_bundle(&bundle_path, &bundle)?;
    emit_progress(
        &mut progress,
        DatasetImportStage::WriteTopologyBundle,
        None,
        format!(
            "Writing edge-name bundle {}",
            edge_name_bundle_path.display()
        ),
    );
    write_edge_name_bundle(&edge_name_bundle_path, &edge_name_bundle)?;
    emit_progress(
        &mut progress,
        DatasetImportStage::WriteTopologyBundle,
        None,
        format!(
            "Writing acceleration bundle {}",
            acceleration_bundle_path.display()
        ),
    );
    write_acceleration_bundle(&acceleration_bundle_path, &acceleration_bundle)?;

    let manifest = DatasetManifest {
        dataset_id,
        label: options.source,
        source_path: source_path.display().to_string(),
        source_sha256: sha256,
        source_size_bytes: size,
        imported_at: now_rfc3339()?,
        build_stage: BuildStage::TopologyReady,
        topology_bundle: Some(BundleRef {
            bundle_id,
            path: bundle_path.display().to_string(),
        }),
        edge_name_bundle: Some(BundleRef {
            bundle_id: edge_name_bundle_id,
            path: edge_name_bundle_path.display().to_string(),
        }),
        acceleration_bundle: Some(BundleRef {
            bundle_id: acceleration_bundle_id,
            path: acceleration_bundle_path.display().to_string(),
        }),
        topology_meta: Some(topology_meta),
        acceleration_settings: Some(acceleration_bundle.build_settings.clone()),
        acceleration_stats: Some(acceleration_bundle.stats.clone()),
    };

    emit_progress(
        &mut progress,
        DatasetImportStage::WriteManifest,
        None,
        format!("Writing dataset manifest for '{}'", manifest.dataset_id.0),
    );
    write_dataset_manifest(paths, &manifest)?;
    emit_progress(
        &mut progress,
        DatasetImportStage::Complete,
        Some(100.0),
        format!(
            "Imported dataset '{}' with {} nodes and {} directed edges",
            manifest.dataset_id.0,
            manifest
                .topology_meta
                .as_ref()
                .map(|meta| meta.node_count)
                .unwrap_or_default(),
            manifest
                .topology_meta
                .as_ref()
                .map(|meta| meta.edge_count)
                .unwrap_or_default()
        ),
    );
    Ok(manifest)
}

fn sha256_file<F>(file: File, source_size_bytes: u64, progress: &mut F) -> Result<String>
where
    F: FnMut(DatasetImportProgress),
{
    let mut reader = BufReader::new(file);
    let mut buffer = [0_u8; 64 * 1024];
    let mut hasher = Sha256::new();
    let mut reporter = PercentReporter::starting_at_zero();
    let mut total_read = 0_u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .context("reading file for hashing")?;
        if read == 0 {
            break;
        }
        total_read += read as u64;
        hasher.update(&buffer[..read]);
        reporter.emit_if_needed(
            total_read,
            source_size_bytes,
            DatasetImportStage::HashSource,
            progress,
            |percent| format!("Hashing source {:.0}%", percent),
        );
    }
    emit_progress(
        progress,
        DatasetImportStage::HashSource,
        Some(100.0),
        "Hashing source 100%".to_string(),
    );
    Ok(hex::encode(hasher.finalize()))
}

#[derive(Debug, Clone)]
struct PendingWay {
    osm_way_id: i64,
    node_ids: Vec<i64>,
    road_class: RoadClass,
    duration_s: Option<f64>,
    surface: SurfaceClass,
    smoothness: SmoothnessClass,
    access_mask: AccessMask,
    is_toll: bool,
    is_roundabout: bool,
    direction: EdgeDirection,
    name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestrictionKind {
    NoTurn,
    OnlyTurn,
}

#[derive(Debug, Clone)]
struct TurnRestrictionCandidate {
    relation_id: i64,
    from_way_id: i64,
    via: ViaSpec,
    to_way_id: i64,
    kind: RestrictionKind,
    mode_mask: AccessMask,
}

#[derive(Debug, Clone)]
enum ViaSpec {
    Node(i64),
    Ways(Vec<i64>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeDirection {
    Both,
    ForwardOnly,
    ReverseOnly,
}

#[derive(Debug, Default)]
struct ObjectCounts {
    nodes: u64,
    ways: u64,
    relations: u64,
}

fn build_topology_bundle(
    source_path: &Path,
    source_size_bytes: u64,
    source_sha256: &str,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<(TopologyBundle, EdgeNameBundle, TopologyBundleMeta)> {
    let (pending_ways, restriction_candidates, needed_nodes, traffic_signal_nodes, counts) =
        scan_routable_objects(source_path, source_size_bytes, progress)?;
    let node_coords = load_node_coords(source_path, source_size_bytes, &needed_nodes, progress)?;
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

            let Some(&(from_id, from_lon, from_lat)) = node_lookup.get(&from) else {
                continue;
            };
            let Some(&(to_id, to_lon, to_lat)) = node_lookup.get(&to) else {
                continue;
            };

            let length_m = haversine_meters(from_lon, from_lat, to_lon, to_lat).round() as u32;
            segments.push((from_id, to_id, from, to, length_m));
        }

        if segments.is_empty() {
            skipped_way_count += 1;
            continue;
        }

        let total_length_m = segments
            .iter()
            .map(|(_, _, _, _, length_m)| *length_m as u64)
            .sum();
        let segment_count = segments.len() as u32;
        for (from_id, to_id, from_osm_id, to_osm_id, length_m) in segments {
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

            if matches!(
                way.direction,
                EdgeDirection::Both | EdgeDirection::ForwardOnly
            ) {
                edges.push(DirectedEdge {
                    edge_id: EdgeId(edges.len() as u32),
                    from: from_id,
                    to: to_id,
                    source_way_id: way.osm_way_id,
                    length_m,
                    duration_s,
                    road_class: way.road_class,
                    surface: way.surface,
                    smoothness: way.smoothness,
                    access_mask: way.access_mask,
                    is_toll: way.is_toll,
                    name_index,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: forward_flags,
                });
            }

            if matches!(
                way.direction,
                EdgeDirection::Both | EdgeDirection::ReverseOnly
            ) {
                edges.push(DirectedEdge {
                    edge_id: EdgeId(edges.len() as u32),
                    from: to_id,
                    to: from_id,
                    source_way_id: way.osm_way_id,
                    length_m,
                    duration_s,
                    road_class: way.road_class,
                    surface: way.surface,
                    smoothness: way.smoothness,
                    access_mask: way.access_mask,
                    is_toll: way.is_toll,
                    name_index,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: reverse_flags,
                });
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

    let edge_name_bundle = EdgeNameBundle { names };

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

    let edge_layers = netan_core::TopologyEdgeLayers::from_directed_edges(&edges);
    let bundle = TopologyBundle {
        schema_version: 8,
        source_path: source_path.display().to_string(),
        source_sha256: source_sha256.to_string(),
        nodes,
        edge_layers,
        edges: Vec::new(),
        turn_restrictions,
        names: Vec::new(),
        edge_based_topology,
        spatial_index,
        node_component_ids: components.node_component_ids.clone(),
        edge_component_ids: components.edge_component_ids.clone(),
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

fn build_edge_based_topology(node_count: usize, edges: &[DirectedEdge]) -> EdgeBasedTopology {
    let mut out_degree = vec![0_u32; node_count];
    let mut head = vec![0_u32; edges.len()];
    for (edge_index, edge) in edges.iter().enumerate() {
        out_degree[edge.from.0 as usize] += 1;
        head[edge_index] = edge.to.0;
    }

    let mut node_first_out = vec![0_u32; node_count + 1];
    for (node_index, degree) in out_degree.iter().enumerate() {
        node_first_out[node_index + 1] = node_first_out[node_index] + degree;
    }

    let mut node_edge_order = vec![0_u32; edges.len()];
    let mut write_positions = node_first_out[..node_count].to_vec();
    for (edge_index, edge) in edges.iter().enumerate() {
        let write_index = &mut write_positions[edge.from.0 as usize];
        node_edge_order[*write_index as usize] = edge_index as u32;
        *write_index += 1;
    }

    let mut edge_transition_first_out = vec![0_u32; edges.len() + 1];
    for edge_index in 0..edges.len() {
        let head_node = head[edge_index] as usize;
        edge_transition_first_out[edge_index + 1] = edge_transition_first_out[edge_index]
            + (node_first_out[head_node + 1] - node_first_out[head_node]);
    }

    let mut edge_transition_edges = vec![0_u32; edge_transition_first_out[edges.len()] as usize];
    let mut transition_write_positions = edge_transition_first_out[..edges.len()].to_vec();
    for edge_index in 0..edges.len() {
        let head_node = head[edge_index] as usize;
        for &next_edge in &node_edge_order
            [node_first_out[head_node] as usize..node_first_out[head_node + 1] as usize]
        {
            let write_index = &mut transition_write_positions[edge_index];
            edge_transition_edges[*write_index as usize] = next_edge;
            *write_index += 1;
        }
    }

    EdgeBasedTopology {
        node_first_out,
        node_edge_order,
        edge_transition_first_out,
        edge_transition_edges,
    }
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

    for node_index in 0..node_count {
        let root = find_component_root(&mut parent, node_index);
        let component_id = if let Some(&component_id) = root_to_component.get(&root) {
            component_id
        } else {
            let component_id = root_to_component.len() as u32;
            root_to_component.insert(root, component_id);
            node_counts.push(0);
            component_id
        };
        node_component_ids[node_index] = component_id;
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
    let parent_index = parent[index] as usize;
    if parent_index != index {
        parent[index] = find_component_root(parent, parent_index);
    }
    parent[index]
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

#[cfg(test)]
fn build_dataset_acceleration_bundle(
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
) -> DatasetAccelerationBundle {
    build_dataset_acceleration_bundle_with_progress(
        topology,
        source_topology_bundle_id,
        AccelerationBuildSettings::default(),
        &mut |_| {},
    )
}

#[cfg(test)]
fn build_dataset_acceleration_bundle_with_settings(
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
    settings: AccelerationBuildSettings,
) -> DatasetAccelerationBundle {
    build_dataset_acceleration_bundle_with_progress(
        topology,
        source_topology_bundle_id,
        settings,
        &mut |_| {},
    )
}

fn build_dataset_acceleration_bundle_with_progress(
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
    settings: AccelerationBuildSettings,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> DatasetAccelerationBundle {
    let settings = settings.normalized();

    let transition_topology = &topology.edge_based_topology;
    let edge_count = topology.edge_count();
    let mut in_degree = vec![0_u32; edge_count];
    let mut out_degree = vec![0_u32; edge_count];

    if transition_topology.edge_transition_first_out.len() == edge_count + 1 {
        for edge_index in 0..edge_count {
            let start = transition_topology.edge_transition_first_out[edge_index] as usize;
            let end = transition_topology.edge_transition_first_out[edge_index + 1] as usize;
            let degree = end.saturating_sub(start) as u32;
            out_degree[edge_index] = degree;
            for &next_edge in &transition_topology.edge_transition_edges[start..end] {
                if let Some(entry) = in_degree.get_mut(next_edge as usize) {
                    *entry += 1;
                }
            }
        }
    }

    let edge_order = build_recursive_spatial_edge_order(topology, &in_degree, &out_degree);

    let mut edge_rank = vec![0_u32; edge_count];
    for (rank, &edge_index) in edge_order.iter().enumerate() {
        edge_rank[edge_index as usize] = rank as u32;
    }

    emit_progress(
        progress,
        DatasetImportStage::BuildAcceleration,
        Some(0.0),
        format!(
            "Building acceleration 0% ({} edge states, profile {:?}, budget {}/{})",
            edge_count,
            settings.profile,
            settings.max_shortcut_budget_per_edge,
            settings.max_shortcuts_per_contracted_edge
        ),
    );

    let mut arcs = Vec::<ShortcutArc>::new();
    let mut seen_arc_pairs = HashSet::with_capacity(
        transition_topology
            .edge_transition_edges
            .len()
            .saturating_mul(2),
    );
    let mut active_out = vec![Vec::<u32>::new(); edge_count];
    let mut active_in = vec![Vec::<u32>::new(); edge_count];
    if transition_topology.edge_transition_first_out.len() == edge_count + 1 {
        for edge_index in 0..edge_count {
            let start = transition_topology.edge_transition_first_out[edge_index] as usize;
            let end = transition_topology.edge_transition_first_out[edge_index + 1] as usize;
            for &next_edge in &transition_topology.edge_transition_edges[start..end] {
                if !seen_arc_pairs.insert(shortcut_arc_key(edge_index as u32, next_edge)) {
                    continue;
                }
                let arc_id = arcs.len() as u32;
                arcs.push(ShortcutArc {
                    tail: edge_index as u32,
                    head: next_edge,
                    path_len: 1,
                    kind: ShortcutArcKind::Base,
                });
                active_out[edge_index].push(arc_id);
                active_in[next_edge as usize].push(arc_id);
            }
        }
    }
    let base_arc_count = arcs.len();
    let max_shortcut_count =
        edge_count.saturating_mul(settings.max_shortcut_budget_per_edge as usize);
    let mut added_shortcuts = 0_usize;

    let mut active_vertex = vec![true; edge_count];
    let mut reporter = PercentReporter::starting_at_zero();
    for (order_index, &contracted_edge) in edge_order.iter().enumerate() {
        let contracted_edge = contracted_edge as usize;
        let incoming = active_in[contracted_edge]
            .iter()
            .copied()
            .filter(|&arc_id| {
                let arc = &arcs[arc_id as usize];
                active_vertex[arc.tail as usize]
                    && active_vertex[arc.head as usize]
                    && arc.head as usize == contracted_edge
                    && arc.tail as usize != contracted_edge
            })
            .collect::<Vec<_>>();
        let outgoing = active_out[contracted_edge]
            .iter()
            .copied()
            .filter(|&arc_id| {
                let arc = &arcs[arc_id as usize];
                active_vertex[arc.tail as usize]
                    && active_vertex[arc.head as usize]
                    && arc.tail as usize == contracted_edge
                    && arc.head as usize != contracted_edge
            })
            .collect::<Vec<_>>();

        let remaining_shortcut_budget = max_shortcut_count.saturating_sub(added_shortcuts);
        let local_shortcut_budget =
            remaining_shortcut_budget.min(settings.max_shortcuts_per_contracted_edge as usize);
        let mut added_for_vertex = 0_usize;
        for incoming_arc in incoming {
            let tail = arcs[incoming_arc as usize].tail as usize;
            for &outgoing_arc in &outgoing {
                if added_for_vertex >= local_shortcut_budget {
                    break;
                }
                let head = arcs[outgoing_arc as usize].head as usize;
                if tail == head {
                    continue;
                }
                let path_len = arcs[incoming_arc as usize]
                    .path_len
                    .saturating_add(arcs[outgoing_arc as usize].path_len);
                if path_len > settings.max_shortcut_path_len {
                    continue;
                }
                if !seen_arc_pairs.insert(shortcut_arc_key(tail as u32, head as u32)) {
                    continue;
                }
                let arc_id = arcs.len() as u32;
                arcs.push(ShortcutArc {
                    tail: tail as u32,
                    head: head as u32,
                    path_len,
                    kind: ShortcutArcKind::Shortcut {
                        left: incoming_arc,
                        right: outgoing_arc,
                    },
                });
                active_out[tail].push(arc_id);
                active_in[head].push(arc_id);
                added_shortcuts += 1;
                added_for_vertex += 1;
            }
            if added_for_vertex >= local_shortcut_budget {
                break;
            }
        }
        active_vertex[contracted_edge] = false;
        reporter.emit_if_needed(
            (order_index + 1) as u64,
            edge_order.len() as u64,
            DatasetImportStage::BuildAcceleration,
            progress,
            |percent| {
                format!(
                    "Building acceleration {:.0}% ({}/{}) with {} arcs ({} base, {} shortcuts)",
                    percent,
                    order_index + 1,
                    edge_order.len(),
                    arcs.len(),
                    base_arc_count,
                    added_shortcuts
                )
            },
        );
    }
    emit_progress(
        progress,
        DatasetImportStage::BuildAcceleration,
        Some(100.0),
        format!(
            "Building acceleration 100% ({} arcs: {} base, {} shortcuts)",
            arcs.len(),
            base_arc_count,
            added_shortcuts
        ),
    );

    let mut upward_first_out = Vec::with_capacity(edge_count + 1);
    let mut downward_first_out = Vec::with_capacity(edge_count + 1);
    upward_first_out.push(0);
    downward_first_out.push(0);

    let mut upward_head = Vec::new();
    let mut downward_head = Vec::new();
    let mut upward_path_first_out = Vec::new();
    let mut downward_path_first_out = Vec::new();
    let mut upward_path_edges = Vec::new();
    let mut downward_path_edges = Vec::new();
    let mut path_stack = Vec::new();
    upward_path_first_out.push(0);
    downward_path_first_out.push(0);

    for tail in 0..edge_count {
        for &arc_id in &active_out[tail] {
            let arc = &arcs[arc_id as usize];
            if edge_rank[arc.tail as usize] < edge_rank[arc.head as usize] {
                upward_head.push(arc.head);
                append_shortcut_arc_path(&arcs, arc_id, &mut upward_path_edges, &mut path_stack);
                upward_path_first_out.push(upward_path_edges.len() as u32);
            } else {
                downward_head.push(arc.head);
                append_shortcut_arc_path(&arcs, arc_id, &mut downward_path_edges, &mut path_stack);
                downward_path_first_out.push(downward_path_edges.len() as u32);
            }
        }
        upward_first_out.push(upward_head.len() as u32);
        downward_first_out.push(downward_head.len() as u32);
    }

    DatasetAccelerationBundle {
        schema_version: 2,
        source_topology_bundle_id,
        algorithm: "edge_based_shortcut_ch_v1".to_string(),
        build_settings: settings,
        stats: AccelerationBundleStats {
            base_arc_count: base_arc_count as u64,
            shortcut_arc_count: added_shortcuts as u64,
            total_arc_count: arcs.len() as u64,
        },
        edge_order,
        edge_rank,
        upward_first_out,
        upward_head,
        upward_path_first_out,
        upward_path_edges,
        downward_first_out,
        downward_head,
        downward_path_first_out,
        downward_path_edges,
    }
}

#[derive(Debug, Clone, Copy)]
struct ShortcutArc {
    tail: u32,
    head: u32,
    path_len: u32,
    kind: ShortcutArcKind,
}

#[derive(Debug, Clone, Copy)]
enum ShortcutArcKind {
    Base,
    Shortcut { left: u32, right: u32 },
}

fn append_shortcut_arc_path(
    arcs: &[ShortcutArc],
    arc_id: u32,
    output: &mut Vec<u32>,
    stack: &mut Vec<u32>,
) {
    stack.clear();
    stack.push(arc_id);
    while let Some(current_arc_id) = stack.pop() {
        match arcs[current_arc_id as usize].kind {
            ShortcutArcKind::Base => output.push(arcs[current_arc_id as usize].head),
            ShortcutArcKind::Shortcut { left, right } => {
                stack.push(right);
                stack.push(left);
            }
        }
    }
}

fn shortcut_arc_key(tail: u32, head: u32) -> u64 {
    ((tail as u64) << 32) | head as u64
}

fn build_recursive_spatial_edge_order(
    topology: &TopologyBundle,
    in_degree: &[u32],
    out_degree: &[u32],
) -> Vec<u32> {
    const LEAF_SIZE: usize = 1_024;

    fn sort_small_block(
        edges: &mut [u32],
        topology: &TopologyBundle,
        in_degree: &[u32],
        out_degree: &[u32],
    ) {
        edges.sort_unstable_by_key(|&edge_index| {
            let edge = topology.routing_edge(edge_index as usize);
            (
                out_degree[edge_index as usize] + in_degree[edge_index as usize],
                out_degree[edge_index as usize],
                in_degree[edge_index as usize],
                edge.from.0,
                edge.to.0,
                edge_index,
            )
        });
    }

    fn recurse(
        topology: &TopologyBundle,
        in_degree: &[u32],
        out_degree: &[u32],
        edges: &mut [u32],
        output: &mut Vec<u32>,
    ) {
        if edges.len() <= LEAF_SIZE {
            sort_small_block(edges, topology, in_degree, out_degree);
            output.extend_from_slice(edges);
            return;
        }

        let mut min_lon = f64::INFINITY;
        let mut max_lon = f64::NEG_INFINITY;
        let mut min_lat = f64::INFINITY;
        let mut max_lat = f64::NEG_INFINITY;
        let mut coords = Vec::with_capacity(edges.len());
        for &edge_index in edges.iter() {
            let edge = topology.routing_edge(edge_index as usize);
            let from = &topology.nodes[edge.from.0 as usize];
            let to = &topology.nodes[edge.to.0 as usize];
            let lon = (from.lon + to.lon) * 0.5;
            let lat = (from.lat + to.lat) * 0.5;
            min_lon = min_lon.min(lon);
            max_lon = max_lon.max(lon);
            min_lat = min_lat.min(lat);
            max_lat = max_lat.max(lat);
            coords.push((edge_index, lon, lat));
        }

        let split_lon = (max_lon - min_lon) >= (max_lat - min_lat);
        let mut axis_values = coords
            .iter()
            .map(|(_, lon, lat)| if split_lon { *lon } else { *lat })
            .collect::<Vec<_>>();
        axis_values
            .sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
        let pivot = axis_values[axis_values.len() / 2];

        let mut left = Vec::new();
        let mut right = Vec::new();
        let mut separator = Vec::new();
        for (edge_index, _, _) in coords {
            let edge = topology.routing_edge(edge_index as usize);
            let from = &topology.nodes[edge.from.0 as usize];
            let to = &topology.nodes[edge.to.0 as usize];
            let from_axis = if split_lon { from.lon } else { from.lat };
            let to_axis = if split_lon { to.lon } else { to.lat };
            if from_axis <= pivot && to_axis <= pivot {
                left.push(edge_index);
            } else if from_axis > pivot && to_axis > pivot {
                right.push(edge_index);
            } else {
                separator.push(edge_index);
            }
        }

        if left.is_empty() || right.is_empty() || separator.len() == edges.len() {
            sort_small_block(edges, topology, in_degree, out_degree);
            output.extend_from_slice(edges);
            return;
        }

        recurse(topology, in_degree, out_degree, &mut left, output);
        recurse(topology, in_degree, out_degree, &mut right, output);
        sort_small_block(&mut separator, topology, in_degree, out_degree);
        output.extend(separator);
    }

    let mut edge_order = (0..topology.edge_count() as u32).collect::<Vec<_>>();
    let mut ordered = Vec::with_capacity(edge_order.len());
    recurse(
        topology,
        in_degree,
        out_degree,
        &mut edge_order,
        &mut ordered,
    );
    ordered
}

fn scan_routable_objects(
    source_path: &Path,
    source_size_bytes: u64,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<(
    Vec<PendingWay>,
    Vec<TurnRestrictionCandidate>,
    HashSet<i64>,
    HashSet<i64>,
    ObjectCounts,
)> {
    let file = File::open(source_path)
        .with_context(|| format!("opening dataset source {}", source_path.display()))?;
    let bytes_read = Arc::new(AtomicU64::new(0));
    let counting_file = CountingReader::new(file, Arc::clone(&bytes_read));
    let mut reader = OsmPbfReader::new(counting_file);
    let mut ways = Vec::new();
    let mut restriction_candidates = Vec::new();
    let mut node_ids = HashSet::new();
    let mut traffic_signal_nodes = HashSet::new();
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
                let Some((road_class, access_mask)) = classify_way(&way.tags) else {
                    continue;
                };
                if way.nodes.len() < 2 {
                    continue;
                }

                let pending = PendingWay {
                    osm_way_id: way.id.0,
                    node_ids: way.nodes.into_iter().map(|node| node.0).collect(),
                    road_class,
                    duration_s: parse_duration_from_tags(&way.tags),
                    surface: classify_surface(&way.tags),
                    smoothness: classify_smoothness(&way.tags),
                    access_mask,
                    is_toll: classify_toll(&way.tags),
                    is_roundabout: tag(&way.tags, "junction") == Some("roundabout"),
                    direction: classify_direction(&way.tags, road_class),
                    name: way.tags.get("name").map(ToString::to_string),
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
        counts,
    ))
}

fn load_node_coords(
    source_path: &Path,
    _source_size_bytes: u64,
    needed_nodes: &HashSet<i64>,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<HashMap<i64, (f64, f64)>> {
    let file = File::open(source_path)
        .with_context(|| format!("opening dataset source {}", source_path.display()))?;
    let bytes_read = Arc::new(AtomicU64::new(0));
    let counting_file = CountingReader::new(file, Arc::clone(&bytes_read));
    let mut reader = OsmPbfReader::new(counting_file);
    let mut coords = HashMap::with_capacity(needed_nodes.len());
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
            coords.insert(node_id, (node.lon(), node.lat()));
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

fn build_nodes(
    node_coords: &HashMap<i64, (f64, f64)>,
) -> (Vec<TopologyNode>, HashMap<i64, (NodeId, f64, f64)>) {
    let mut used_nodes = node_coords.keys().copied().collect::<Vec<_>>();
    used_nodes.sort_unstable();

    let mut nodes = Vec::with_capacity(used_nodes.len());
    let mut lookup = HashMap::with_capacity(used_nodes.len());
    for osm_node_id in used_nodes {
        let Some(&(lon, lat)) = node_coords.get(&osm_node_id) else {
            continue;
        };
        let node_id = NodeId(nodes.len() as u32);
        nodes.push(TopologyNode {
            node_id,
            osm_node_id,
            lon,
            lat,
        });
        lookup.insert(osm_node_id, (node_id, lon, lat));
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

fn build_spatial_index(nodes: &[TopologyNode]) -> Option<netan_core::NodeSpatialIndex> {
    const TARGET_CELL_SPAN_M: f64 = 750.0;
    const METERS_PER_DEGREE_LAT: f64 = 111_320.0;

    let first = nodes.first()?;
    let mut bounds = TopologyBounds {
        min_lon: first.lon,
        min_lat: first.lat,
        max_lon: first.lon,
        max_lat: first.lat,
    };
    for node in nodes.iter().skip(1) {
        bounds.min_lon = bounds.min_lon.min(node.lon);
        bounds.min_lat = bounds.min_lat.min(node.lat);
        bounds.max_lon = bounds.max_lon.max(node.lon);
        bounds.max_lat = bounds.max_lat.max(node.lat);
    }

    let mid_lat = ((bounds.min_lat + bounds.max_lat) / 2.0).to_radians();
    let cos_lat = mid_lat.cos().abs().max(0.2);
    let cell_height_deg = (TARGET_CELL_SPAN_M / METERS_PER_DEGREE_LAT).max(f64::EPSILON);
    let cell_width_deg = (TARGET_CELL_SPAN_M / (METERS_PER_DEGREE_LAT * cos_lat)).max(f64::EPSILON);
    let columns =
        (((bounds.max_lon - bounds.min_lon) / cell_width_deg).floor() as u32).saturating_add(1);
    let rows =
        (((bounds.max_lat - bounds.min_lat) / cell_height_deg).floor() as u32).saturating_add(1);
    let cell_count = columns as usize * rows as usize;
    let mut counts = vec![0_u32; cell_count];

    for node in nodes {
        counts[spatial_cell_index(
            &bounds,
            columns,
            rows,
            cell_width_deg,
            cell_height_deg,
            node.lon,
            node.lat,
        )] += 1;
    }

    let mut cells = vec![SpatialIndexCell::default(); cell_count];
    let mut next_offset = 0_u32;
    for (cell, count) in cells.iter_mut().zip(&counts) {
        cell.node_start = next_offset;
        cell.node_len = *count;
        next_offset += *count;
    }

    let mut write_positions = cells
        .iter()
        .map(|cell| cell.node_start as usize)
        .collect::<Vec<_>>();
    let mut node_ids = vec![0_u32; nodes.len()];
    for node in nodes {
        let cell_index = spatial_cell_index(
            &bounds,
            columns,
            rows,
            cell_width_deg,
            cell_height_deg,
            node.lon,
            node.lat,
        );
        let write_index = write_positions[cell_index];
        node_ids[write_index] = node.node_id.0;
        write_positions[cell_index] += 1;
    }

    Some(netan_core::NodeSpatialIndex {
        bounds,
        columns,
        rows,
        cell_width_deg,
        cell_height_deg,
        cells,
        node_ids,
    })
}

fn spatial_cell_index(
    bounds: &TopologyBounds,
    columns: u32,
    rows: u32,
    cell_width_deg: f64,
    cell_height_deg: f64,
    lon: f64,
    lat: f64,
) -> usize {
    let column = (((lon - bounds.min_lon) / cell_width_deg).floor() as i64)
        .clamp(0, columns.saturating_sub(1) as i64) as usize;
    let row = (((lat - bounds.min_lat) / cell_height_deg).floor() as i64)
        .clamp(0, rows.saturating_sub(1) as i64) as usize;
    row * columns as usize + column
}

fn emit_progress(
    progress: &mut impl FnMut(DatasetImportProgress),
    stage: DatasetImportStage,
    stage_percent: Option<f64>,
    message: String,
) {
    progress(DatasetImportProgress {
        stage,
        stage_percent,
        message,
    });
}

struct PercentReporter {
    last_bucket: Option<u32>,
}

impl PercentReporter {
    fn new() -> Self {
        Self { last_bucket: None }
    }

    fn starting_at_zero() -> Self {
        Self {
            last_bucket: Some(0),
        }
    }

    fn emit_if_needed<F>(
        &mut self,
        completed: u64,
        total: u64,
        stage: DatasetImportStage,
        progress: &mut impl FnMut(DatasetImportProgress),
        message: F,
    ) where
        F: FnOnce(f64) -> String,
    {
        if total == 0 {
            return;
        }
        let percent = ((completed as f64 / total as f64) * 100.0).clamp(0.0, 100.0);
        if percent >= 100.0 {
            return;
        }
        let bucket = (percent / 5.0).floor() as u32;
        if self.last_bucket == Some(bucket) {
            return;
        }
        self.last_bucket = Some(bucket);
        let quantized_percent = (bucket * 5) as f64;
        emit_progress(
            progress,
            stage,
            Some(quantized_percent),
            message(quantized_percent),
        );
    }
}

struct CountingReader<R> {
    inner: R,
    bytes_read: Arc<AtomicU64>,
}

impl<R> CountingReader<R> {
    fn new(inner: R, bytes_read: Arc<AtomicU64>) -> Self {
        Self { inner, bytes_read }
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.bytes_read.fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

fn build_turn_restrictions(
    candidates: &[TurnRestrictionCandidate],
    ways: &[PendingWay],
    node_lookup: &HashMap<i64, (NodeId, f64, f64)>,
    edges: &[DirectedEdge],
) -> Vec<TurnRestriction> {
    let mut incoming_by_way_and_node: HashMap<(i64, NodeId), Vec<EdgeId>> = HashMap::new();
    let mut outgoing_by_way_and_node: HashMap<(i64, NodeId), Vec<EdgeId>> = HashMap::new();
    let mut edge_by_way_and_nodes: HashMap<(i64, NodeId, NodeId), EdgeId> = HashMap::new();
    let node_count = edges
        .iter()
        .flat_map(|edge| [edge.from.0 as usize, edge.to.0 as usize])
        .max()
        .map(|max_index| max_index + 1)
        .unwrap_or_default();
    let mut outgoing_by_node: Vec<Vec<EdgeId>> = vec![Vec::new(); node_count];

    for edge in edges {
        incoming_by_way_and_node
            .entry((edge.source_way_id, edge.to))
            .or_default()
            .push(edge.edge_id);
        outgoing_by_way_and_node
            .entry((edge.source_way_id, edge.from))
            .or_default()
            .push(edge.edge_id);
        edge_by_way_and_nodes.insert((edge.source_way_id, edge.from, edge.to), edge.edge_id);
        outgoing_by_node[edge.from.0 as usize].push(edge.edge_id);
    }

    let way_lookup: HashMap<_, _> = ways.iter().map(|way| (way.osm_way_id, way)).collect();
    let mut restrictions = Vec::new();
    let mut seen = HashSet::new();
    for candidate in candidates {
        match &candidate.via {
            ViaSpec::Node(via_node_id) => {
                let Some(&(via_node, _, _)) = node_lookup.get(via_node_id) else {
                    continue;
                };
                let Some(incoming_edges) =
                    incoming_by_way_and_node.get(&(candidate.from_way_id, via_node))
                else {
                    continue;
                };
                let Some(allowed_to_edges) =
                    outgoing_by_way_and_node.get(&(candidate.to_way_id, via_node))
                else {
                    continue;
                };
                expand_terminal_restrictions(
                    &mut restrictions,
                    &mut seen,
                    candidate,
                    incoming_edges,
                    &[],
                    via_node,
                    allowed_to_edges,
                    &outgoing_by_node,
                );
            }
            ViaSpec::Ways(via_way_ids) => {
                if via_way_ids.is_empty() {
                    continue;
                }
                let Some(from_way) = way_lookup.get(&candidate.from_way_id).copied() else {
                    continue;
                };
                let Some(to_way) = way_lookup.get(&candidate.to_way_id).copied() else {
                    continue;
                };
                let Some(via_ways) = via_way_ids
                    .iter()
                    .map(|way_id| way_lookup.get(way_id).copied())
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                let Some(shared_nodes) = via_shared_nodes(from_way, &via_ways, to_way) else {
                    continue;
                };
                let Some(&(entry_node, _, _)) = node_lookup.get(&shared_nodes[0]) else {
                    continue;
                };
                let Some(&(exit_node, _, _)) = node_lookup.get(shared_nodes.last().unwrap()) else {
                    continue;
                };
                let Some(incoming_edges) =
                    incoming_by_way_and_node.get(&(candidate.from_way_id, entry_node))
                else {
                    continue;
                };
                let Some(allowed_to_edges) =
                    outgoing_by_way_and_node.get(&(candidate.to_way_id, exit_node))
                else {
                    continue;
                };

                let mut via_edges = Vec::new();
                let mut valid = true;
                for (way, pair) in via_ways.iter().zip(shared_nodes.windows(2)) {
                    let Some(segment_edges) = way_edge_sequence_between(
                        way,
                        pair[0],
                        pair[1],
                        node_lookup,
                        &edge_by_way_and_nodes,
                    ) else {
                        valid = false;
                        break;
                    };
                    via_edges.extend(segment_edges);
                }
                if !valid || via_edges.is_empty() {
                    continue;
                }

                expand_terminal_restrictions(
                    &mut restrictions,
                    &mut seen,
                    candidate,
                    incoming_edges,
                    &via_edges,
                    exit_node,
                    allowed_to_edges,
                    &outgoing_by_node,
                );
                if candidate.kind == RestrictionKind::OnlyTurn {
                    expand_intermediate_only_turns(
                        &mut restrictions,
                        &mut seen,
                        candidate,
                        incoming_edges,
                        &via_edges,
                        entry_node,
                        &outgoing_by_node,
                        edges,
                    );
                }
            }
        }
    }

    restrictions.sort_by_key(|restriction| {
        (
            restriction
                .edge_path
                .first()
                .map(|edge| edge.0)
                .unwrap_or_default(),
            restriction.edge_path.len(),
            restriction
                .edge_path
                .last()
                .map(|edge| edge.0)
                .unwrap_or_default(),
            restriction.mode_mask.0,
            restriction.relation_id,
        )
    });
    restrictions
}

fn expand_terminal_restrictions(
    restrictions: &mut Vec<TurnRestriction>,
    seen: &mut HashSet<(Vec<u32>, u16)>,
    candidate: &TurnRestrictionCandidate,
    incoming_edges: &[EdgeId],
    via_edges: &[EdgeId],
    terminal_node: NodeId,
    allowed_to_edges: &[EdgeId],
    outgoing_by_node: &[Vec<EdgeId>],
) {
    match candidate.kind {
        RestrictionKind::NoTurn => {
            for &from_edge in incoming_edges {
                for &to_edge in allowed_to_edges {
                    let mut edge_path = Vec::with_capacity(via_edges.len() + 2);
                    edge_path.push(from_edge);
                    edge_path.extend_from_slice(via_edges);
                    edge_path.push(to_edge);
                    push_restriction(restrictions, seen, candidate, edge_path);
                }
            }
        }
        RestrictionKind::OnlyTurn => {
            for &from_edge in incoming_edges {
                let mut prefix = Vec::with_capacity(via_edges.len() + 2);
                prefix.push(from_edge);
                prefix.extend_from_slice(via_edges);
                for &to_edge in &outgoing_by_node[terminal_node.0 as usize] {
                    if allowed_to_edges.contains(&to_edge) {
                        continue;
                    }
                    let mut edge_path = prefix.clone();
                    edge_path.push(to_edge);
                    push_restriction(restrictions, seen, candidate, edge_path);
                }
            }
        }
    }
}

fn expand_intermediate_only_turns(
    restrictions: &mut Vec<TurnRestriction>,
    seen: &mut HashSet<(Vec<u32>, u16)>,
    candidate: &TurnRestrictionCandidate,
    incoming_edges: &[EdgeId],
    via_edges: &[EdgeId],
    entry_node: NodeId,
    outgoing_by_node: &[Vec<EdgeId>],
    edges: &[DirectedEdge],
) {
    for &from_edge in incoming_edges {
        let mut prefix = vec![from_edge];
        let mut current_node = entry_node;
        for &required_edge in via_edges {
            for &other_edge in &outgoing_by_node[current_node.0 as usize] {
                if other_edge == required_edge {
                    continue;
                }
                let mut edge_path = prefix.clone();
                edge_path.push(other_edge);
                push_restriction(restrictions, seen, candidate, edge_path);
            }
            prefix.push(required_edge);
            current_node = edges[required_edge.0 as usize].to;
        }
    }
}

fn push_restriction(
    restrictions: &mut Vec<TurnRestriction>,
    seen: &mut HashSet<(Vec<u32>, u16)>,
    candidate: &TurnRestrictionCandidate,
    edge_path: Vec<EdgeId>,
) {
    if edge_path.len() < 2 {
        return;
    }
    let key = (
        edge_path.iter().map(|edge| edge.0).collect::<Vec<_>>(),
        candidate.mode_mask.0,
    );
    if seen.insert(key) {
        restrictions.push(TurnRestriction {
            relation_id: candidate.relation_id,
            kind: match candidate.kind {
                RestrictionKind::NoTurn => TurnRestrictionKind::NoTurn,
                RestrictionKind::OnlyTurn => TurnRestrictionKind::OnlyTurn,
            },
            edge_path,
            mode_mask: candidate.mode_mask,
        });
    }
}

fn via_shared_nodes(
    from_way: &PendingWay,
    via_ways: &[&PendingWay],
    to_way: &PendingWay,
) -> Option<Vec<i64>> {
    let mut shared = Vec::with_capacity(via_ways.len() + 1);
    let mut previous = from_way;
    for way in via_ways {
        shared.push(unique_shared_node(previous, way)?);
        previous = way;
    }
    shared.push(unique_shared_node(previous, to_way)?);
    Some(shared)
}

fn unique_shared_node(left: &PendingWay, right: &PendingWay) -> Option<i64> {
    let right_nodes = right.node_ids.iter().copied().collect::<HashSet<_>>();
    let shared = left
        .node_ids
        .iter()
        .copied()
        .filter(|node_id| right_nodes.contains(node_id))
        .collect::<BTreeSet<_>>();
    if shared.len() == 1 {
        shared.into_iter().next()
    } else {
        None
    }
}

fn way_edge_sequence_between(
    way: &PendingWay,
    start_osm_node_id: i64,
    end_osm_node_id: i64,
    node_lookup: &HashMap<i64, (NodeId, f64, f64)>,
    edge_by_way_and_nodes: &HashMap<(i64, NodeId, NodeId), EdgeId>,
) -> Option<Vec<EdgeId>> {
    if start_osm_node_id == end_osm_node_id {
        return None;
    }

    let start_indexes = way
        .node_ids
        .iter()
        .enumerate()
        .filter_map(|(index, node_id)| (*node_id == start_osm_node_id).then_some(index))
        .collect::<Vec<_>>();
    let end_indexes = way
        .node_ids
        .iter()
        .enumerate()
        .filter_map(|(index, node_id)| (*node_id == end_osm_node_id).then_some(index))
        .collect::<Vec<_>>();

    let mut best: Option<Vec<EdgeId>> = None;
    for start_index in &start_indexes {
        for end_index in &end_indexes {
            if start_index == end_index {
                continue;
            }
            let step: isize = if start_index < end_index { 1 } else { -1 };
            let mut cursor = *start_index as isize;
            let end = *end_index as isize;
            let mut edge_path = Vec::new();
            let mut valid = true;
            while cursor != end {
                let next = cursor + step;
                let from_osm = way.node_ids[cursor as usize];
                let to_osm = way.node_ids[next as usize];
                let Some(&(from_node, _, _)) = node_lookup.get(&from_osm) else {
                    valid = false;
                    break;
                };
                let Some(&(to_node, _, _)) = node_lookup.get(&to_osm) else {
                    valid = false;
                    break;
                };
                let Some(&edge_id) =
                    edge_by_way_and_nodes.get(&(way.osm_way_id, from_node, to_node))
                else {
                    valid = false;
                    break;
                };
                edge_path.push(edge_id);
                cursor = next;
            }

            if valid
                && !edge_path.is_empty()
                && best
                    .as_ref()
                    .is_none_or(|current| edge_path.len() < current.len())
            {
                best = Some(edge_path);
            }
        }
    }

    best
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

fn classify_way(tags: &Tags) -> Option<(RoadClass, AccessMask)> {
    if let Some(highway) = tag(tags, "highway") {
        let road_class = classify_highway(highway)?;
        return Some((
            road_class,
            classify_access(tags, highway, tag(tags, "route")),
        ));
    }

    if tag(tags, "route") == Some("ferry") || tag(tags, "ferry").is_some() {
        return Some((RoadClass::Ferry, classify_access(tags, "", Some("ferry"))));
    }

    None
}

fn classify_highway(highway: &str) -> Option<RoadClass> {
    match highway {
        "motorway" | "motorway_link" => Some(RoadClass::Motorway),
        "trunk" | "trunk_link" => Some(RoadClass::Trunk),
        "primary" | "primary_link" => Some(RoadClass::Primary),
        "secondary" | "secondary_link" => Some(RoadClass::Secondary),
        "tertiary" | "tertiary_link" => Some(RoadClass::Tertiary),
        "residential" | "unclassified" | "living_street" => Some(RoadClass::Residential),
        "service" => Some(RoadClass::Service),
        "track" => Some(RoadClass::Track),
        "path" | "cycleway" | "footway" | "pedestrian" | "steps" | "bridleway" => {
            Some(RoadClass::Path)
        }
        _ => None,
    }
}

fn classify_surface(tags: &Tags) -> SurfaceClass {
    match tag(tags, "surface") {
        Some("asphalt") => SurfaceClass::Asphalt,
        Some("paved") | Some("concrete") | Some("concrete:lanes") | Some("concrete:plates") => {
            SurfaceClass::Paved
        }
        Some("gravel") | Some("fine_gravel") | Some("pebblestone") => SurfaceClass::Gravel,
        Some("cobblestone") | Some("sett") | Some("unhewn_cobblestone") => {
            SurfaceClass::Cobblestone
        }
        Some("ground") | Some("earth") | Some("grass") | Some("mud") => SurfaceClass::Ground,
        Some("dirt") | Some("compacted") => SurfaceClass::Dirt,
        Some("sand") => SurfaceClass::Sand,
        _ => SurfaceClass::Unknown,
    }
}

fn classify_smoothness(tags: &Tags) -> SmoothnessClass {
    match tag(tags, "smoothness") {
        Some("excellent") => SmoothnessClass::Excellent,
        Some("good") => SmoothnessClass::Good,
        Some("intermediate") => SmoothnessClass::Intermediate,
        Some("bad") => SmoothnessClass::Bad,
        Some("very_bad") => SmoothnessClass::VeryBad,
        Some("horrible") => SmoothnessClass::Horrible,
        Some("very_horrible") => SmoothnessClass::VeryHorrible,
        Some("impassable") => SmoothnessClass::Impassable,
        _ => SmoothnessClass::Unknown,
    }
}

fn classify_toll(tags: &Tags) -> bool {
    matches!(tag(tags, "toll"), Some("yes" | "true" | "1"))
}

fn classify_access(tags: &Tags, highway: &str, route: Option<&str>) -> AccessMask {
    if route == Some("ferry") {
        return apply_access_overrides(
            tags,
            AccessMask::CAR
                | AccessMask::BICYCLE
                | AccessMask::FOOT
                | AccessMask::TRANSIT
                | AccessMask::HGV,
        );
    }

    let bits = match highway {
        "motorway" | "motorway_link" => AccessMask::CAR | AccessMask::HGV,
        "trunk" | "trunk_link" | "primary" | "primary_link" | "secondary" | "secondary_link"
        | "tertiary" | "tertiary_link" | "residential" | "unclassified" | "living_street"
        | "service" => AccessMask::CAR | AccessMask::BICYCLE | AccessMask::FOOT | AccessMask::HGV,
        "track" => AccessMask::CAR | AccessMask::BICYCLE | AccessMask::FOOT | AccessMask::HGV,
        "path" | "footway" | "pedestrian" | "steps" => AccessMask::FOOT,
        "cycleway" => AccessMask::BICYCLE | AccessMask::FOOT,
        "bridleway" => AccessMask::FOOT,
        _ => AccessMask::CAR | AccessMask::BICYCLE | AccessMask::FOOT,
    };
    apply_access_overrides(tags, bits)
}

fn apply_access_overrides(tags: &Tags, base_bits: u16) -> AccessMask {
    let mut bits = base_bits;

    if tag_is_restricted(tags, "access") {
        bits = 0;
    }
    if tag_is_restricted(tags, "vehicle") {
        bits &= !(AccessMask::CAR | AccessMask::BICYCLE | AccessMask::TRANSIT | AccessMask::HGV);
    }
    if tag_is_restricted(tags, "motor_vehicle") {
        bits &= !(AccessMask::CAR | AccessMask::TRANSIT | AccessMask::HGV);
    }
    if tag_is_restricted(tags, "motorcar") {
        bits &= !AccessMask::CAR;
    }
    if tag_is_restricted(tags, "bicycle") {
        bits &= !AccessMask::BICYCLE;
    }
    if tag_is_restricted(tags, "foot") {
        bits &= !AccessMask::FOOT;
    }
    if tag_is_restricted(tags, "psv") || tag_is_restricted(tags, "bus") {
        bits &= !AccessMask::TRANSIT;
    }
    if tag_is_restricted(tags, "hgv") {
        bits &= !AccessMask::HGV;
    }

    if tag_is_allowed(tags, "vehicle") {
        bits |= AccessMask::CAR | AccessMask::BICYCLE | AccessMask::TRANSIT | AccessMask::HGV;
    }
    if tag_is_allowed(tags, "motor_vehicle") {
        bits |= AccessMask::CAR | AccessMask::TRANSIT | AccessMask::HGV;
    }
    if tag_is_allowed(tags, "motorcar") {
        bits |= AccessMask::CAR;
    }
    if tag_is_allowed(tags, "bicycle") {
        bits |= AccessMask::BICYCLE;
    }
    if tag_is_allowed(tags, "foot") {
        bits |= AccessMask::FOOT;
    }
    if tag_is_allowed(tags, "psv") || tag_is_allowed(tags, "bus") {
        bits |= AccessMask::TRANSIT;
    }
    if tag_is_allowed(tags, "hgv") {
        bits |= AccessMask::HGV;
    }

    AccessMask::new(bits)
}

fn tag_is_restricted(tags: &Tags, key: &str) -> bool {
    matches!(tag(tags, key), Some("no" | "private"))
}

fn tag_is_allowed(tags: &Tags, key: &str) -> bool {
    matches!(
        tag(tags, key),
        Some("yes" | "designated" | "official" | "permissive")
    )
}

fn is_traffic_signal_node(tags: &Tags) -> bool {
    tag(tags, "highway") == Some("traffic_signals")
}

fn classify_direction(tags: &Tags, road_class: RoadClass) -> EdgeDirection {
    match tag(tags, "oneway") {
        Some("-1") => EdgeDirection::ReverseOnly,
        Some("yes" | "true" | "1") => EdgeDirection::ForwardOnly,
        Some("no" | "false" | "0") => EdgeDirection::Both,
        _ if road_class == RoadClass::Motorway => EdgeDirection::ForwardOnly,
        _ if tag(tags, "junction") == Some("roundabout") => EdgeDirection::ForwardOnly,
        _ => EdgeDirection::Both,
    }
}

fn parse_turn_restriction_relation(relation: &Relation) -> Option<TurnRestrictionCandidate> {
    if tag(&relation.tags, "type") != Some("restriction") {
        return None;
    }

    let (kind, mode_mask) = parse_restriction_rule(&relation.tags)?;
    let mut from_way = None;
    let mut via_node = None;
    let mut via_ways = Vec::new();
    let mut to_way = None;

    for reference in &relation.refs {
        match (reference.role.as_str(), reference.member) {
            ("from", OsmId::Way(way_id)) => assign_unique(&mut from_way, way_id.0)?,
            ("via", OsmId::Node(node_id)) => assign_unique(&mut via_node, node_id.0)?,
            ("via", OsmId::Way(way_id)) => via_ways.push(way_id.0),
            ("to", OsmId::Way(way_id)) => assign_unique(&mut to_way, way_id.0)?,
            _ => {}
        }
    }

    let via = match (via_node, via_ways.is_empty()) {
        (Some(node_id), true) => ViaSpec::Node(node_id),
        (None, false) => ViaSpec::Ways(via_ways),
        _ => return None,
    };

    Some(TurnRestrictionCandidate {
        relation_id: relation.id.0,
        from_way_id: from_way?,
        via,
        to_way_id: to_way?,
        kind,
        mode_mask,
    })
}

fn parse_restriction_rule(tags: &Tags) -> Option<(RestrictionKind, AccessMask)> {
    let mut kind = None;
    let mut mode_bits = 0_u16;

    for (key, default_mask) in [
        (
            "restriction",
            AccessMask::CAR | AccessMask::HGV | AccessMask::TRANSIT,
        ),
        (
            "restriction:motor_vehicle",
            AccessMask::CAR | AccessMask::HGV | AccessMask::TRANSIT,
        ),
        (
            "restriction:vehicle",
            AccessMask::CAR | AccessMask::BICYCLE | AccessMask::HGV | AccessMask::TRANSIT,
        ),
        ("restriction:motorcar", AccessMask::CAR),
        ("restriction:motorcycle", AccessMask::CAR),
        ("restriction:moped", AccessMask::CAR),
        ("restriction:hgv", AccessMask::HGV),
        ("restriction:goods", AccessMask::HGV),
        ("restriction:bus", AccessMask::TRANSIT),
        ("restriction:psv", AccessMask::TRANSIT),
        ("restriction:taxi", AccessMask::TRANSIT),
        ("restriction:bicycle", AccessMask::BICYCLE),
        ("restriction:foot", AccessMask::FOOT),
    ] {
        let Some(value) = tag(tags, key) else {
            continue;
        };
        let parsed_kind = parse_restriction_kind(value)?;
        if let Some(existing_kind) = kind {
            if existing_kind != parsed_kind {
                return None;
            }
        } else {
            kind = Some(parsed_kind);
        }
        mode_bits |= default_mask;
    }

    mode_bits &= !parse_except_modes(tag(tags, "except"));
    if mode_bits == 0 {
        return None;
    }
    Some((kind?, AccessMask::new(mode_bits)))
}

fn parse_restriction_kind(value: &str) -> Option<RestrictionKind> {
    let value = value.trim();
    if value.starts_with("no_") {
        Some(RestrictionKind::NoTurn)
    } else if value.starts_with("only_") {
        Some(RestrictionKind::OnlyTurn)
    } else {
        None
    }
}

fn parse_except_modes(except: Option<&str>) -> u16 {
    let mut bits = 0_u16;
    let Some(except) = except else {
        return bits;
    };

    for mode in except
        .split([';', ','])
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        bits |= restriction_mode_bits(mode);
    }

    bits
}

fn restriction_mode_bits(mode: &str) -> u16 {
    match mode {
        "motorcar" | "motor_vehicle:conditional" => AccessMask::CAR,
        "motor_vehicle" => AccessMask::CAR | AccessMask::HGV | AccessMask::TRANSIT,
        "motorcycle" | "moped" => AccessMask::CAR,
        "vehicle" => AccessMask::CAR | AccessMask::BICYCLE | AccessMask::HGV | AccessMask::TRANSIT,
        "hgv" | "goods" => AccessMask::HGV,
        "bus" | "psv" | "taxi" => AccessMask::TRANSIT,
        "bicycle" => AccessMask::BICYCLE,
        "foot" | "pedestrian" => AccessMask::FOOT,
        _ => 0,
    }
}

fn assign_unique(slot: &mut Option<i64>, value: i64) -> Option<()> {
    match slot {
        Some(existing) if *existing != value => None,
        Some(_) => Some(()),
        None => {
            *slot = Some(value);
            Some(())
        }
    }
}

fn parse_duration_from_tags(tags: &Tags) -> Option<f64> {
    tag(tags, "duration:seconds")
        .and_then(|value| value.parse::<f64>().ok())
        .or_else(|| tag(tags, "duration").and_then(parse_duration_seconds))
}

fn parse_duration_seconds(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    raw.parse::<f64>()
        .ok()
        .or_else(|| parse_iso8601_duration(raw))
        .or_else(|| parse_clock_duration(raw))
        .or_else(|| parse_unit_duration(raw))
}

fn parse_clock_duration(raw: &str) -> Option<f64> {
    let parts: Vec<_> = raw.split(':').collect();
    match parts.as_slice() {
        [hours, minutes] => {
            let hours = hours.parse::<f64>().ok()?;
            let minutes = minutes.parse::<f64>().ok()?;
            Some(hours * 3600.0 + minutes * 60.0)
        }
        [hours, minutes, seconds] => {
            let hours = hours.parse::<f64>().ok()?;
            let minutes = minutes.parse::<f64>().ok()?;
            let seconds = seconds.parse::<f64>().ok()?;
            Some(hours * 3600.0 + minutes * 60.0 + seconds)
        }
        _ => None,
    }
}

fn parse_iso8601_duration(raw: &str) -> Option<f64> {
    let mut chars = raw.trim().chars().peekable();
    if chars.next()?.to_ascii_uppercase() != 'P' {
        return None;
    }

    let mut total_seconds = 0.0;
    let mut in_time = false;
    while chars.peek().is_some() {
        if chars.peek().is_some_and(|ch| ch.eq_ignore_ascii_case(&'T')) {
            chars.next();
            in_time = true;
            continue;
        }

        let value = parse_number(&mut chars)?;
        let unit = chars.next()?.to_ascii_uppercase();
        let multiplier = match unit {
            'D' => 86_400.0,
            'H' if in_time => 3_600.0,
            'M' if in_time => 60.0,
            'S' if in_time => 1.0,
            _ => return None,
        };
        total_seconds += value * multiplier;
    }

    Some(total_seconds)
}

fn parse_unit_duration(raw: &str) -> Option<f64> {
    let mut chars = raw.trim().chars().peekable();
    let mut total_seconds = 0.0;
    let mut consumed_any = false;

    while chars.peek().is_some() {
        skip_whitespace(&mut chars);
        if chars.peek().is_none() {
            break;
        }

        let value = parse_number(&mut chars)?;
        skip_whitespace(&mut chars);

        let mut unit = String::new();
        while let Some(ch) = chars.peek().copied() {
            if ch.is_ascii_alphabetic() {
                unit.push(ch.to_ascii_lowercase());
                chars.next();
            } else {
                break;
            }
        }
        if unit.is_empty() {
            return None;
        }

        let multiplier = match unit.as_str() {
            "d" | "day" | "days" => 86_400.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            _ => return None,
        };
        total_seconds += value * multiplier;
        consumed_any = true;

        skip_whitespace(&mut chars);
    }

    consumed_any.then_some(total_seconds)
}

fn parse_number<I>(chars: &mut std::iter::Peekable<I>) -> Option<f64>
where
    I: Iterator<Item = char>,
{
    let mut token = String::new();
    while let Some(ch) = chars.peek().copied() {
        if ch.is_ascii_digit() || ch == '.' {
            token.push(ch);
            chars.next();
        } else {
            break;
        }
    }

    (!token.is_empty())
        .then(|| token.parse::<f64>().ok())
        .flatten()
}

fn skip_whitespace<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    while chars.peek().is_some_and(|ch| ch.is_whitespace()) {
        chars.next();
    }
}

fn tag<'a>(tags: &'a Tags, key: &str) -> Option<&'a str> {
    tags.get(key).map(|value| value.as_str())
}

fn haversine_meters(from_lon: f64, from_lat: f64, to_lon: f64, to_lat: f64) -> f64 {
    let earth_radius_m = 6_371_000.0_f64;
    let d_lat = (to_lat - from_lat).to_radians();
    let d_lon = (to_lon - from_lon).to_radians();
    let from_lat = from_lat.to_radians();
    let to_lat = to_lat.to_radians();
    let a =
        (d_lat / 2.0).sin().powi(2) + from_lat.cos() * to_lat.cos() * (d_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    earth_radius_m * c
}

#[cfg(test)]
mod tests {
    use super::{
        EdgeDirection, PendingWay, RestrictionKind, TurnRestrictionCandidate, ViaSpec,
        apportioned_duration_s, build_dataset_acceleration_bundle,
        build_dataset_acceleration_bundle_with_settings, build_edge_based_topology,
        build_turn_restrictions, classify_access, classify_direction, classify_highway,
        haversine_meters, parse_duration_seconds, parse_turn_restriction_relation,
    };
    use netan_core::{
        AccelerationBuildProfile, AccelerationBuildSettings, AccessMask, CacheBundleId,
        DirectedEdge, EdgeId, NodeId, RoadClass, SurfaceClass, TopologyBundle, TopologyNode,
        TurnRestrictionKind,
    };
    use osmpbfreader::{NodeId as OsmNodeId, OsmId, Ref, Relation, RelationId, Tags, WayId};
    use std::collections::HashMap;

    #[test]
    fn classifies_major_highways() {
        assert_eq!(classify_highway("motorway"), Some(RoadClass::Motorway));
        assert_eq!(classify_highway("primary_link"), Some(RoadClass::Primary));
        assert_eq!(classify_highway("footway"), Some(RoadClass::Path));
        assert_eq!(classify_highway("construction"), None);
    }

    #[test]
    fn infers_access_masks() {
        let empty = Tags::new();
        assert_eq!(
            classify_access(&empty, "motorway", None),
            AccessMask::new(AccessMask::CAR | AccessMask::HGV)
        );
        assert_eq!(
            classify_access(&empty, "cycleway", None),
            AccessMask::new(AccessMask::BICYCLE | AccessMask::FOOT)
        );
    }

    #[test]
    fn applies_mode_specific_access_overrides() {
        let restricted = Tags::from_iter([
            ("access".into(), "no".into()),
            ("foot".into(), "yes".into()),
        ]);
        assert_eq!(
            classify_access(&restricted, "residential", None),
            AccessMask::new(AccessMask::FOOT)
        );

        let bicycle_forbidden = Tags::from_iter([("bicycle".into(), "no".into())]);
        assert_eq!(
            classify_access(&bicycle_forbidden, "cycleway", None),
            AccessMask::new(AccessMask::FOOT)
        );

        let ferry_foot_only = Tags::from_iter([
            ("route".into(), "ferry".into()),
            ("motor_vehicle".into(), "no".into()),
            ("bicycle".into(), "no".into()),
        ]);
        assert_eq!(
            classify_access(&ferry_foot_only, "", Some("ferry")),
            AccessMask::new(AccessMask::FOOT)
        );
    }

    #[test]
    fn infers_oneway_behavior() {
        let mut reverse = Tags::new();
        reverse.insert("oneway".into(), "-1".into());
        assert_eq!(
            classify_direction(&reverse, RoadClass::Residential),
            EdgeDirection::ReverseOnly
        );

        let roundabout = Tags::from_iter([("junction".into(), "roundabout".into())]);
        assert_eq!(
            classify_direction(&roundabout, RoadClass::Residential),
            EdgeDirection::ForwardOnly
        );

        let empty = Tags::new();
        assert_eq!(
            classify_direction(&empty, RoadClass::Residential),
            EdgeDirection::Both
        );
    }

    #[test]
    fn computes_reasonable_segment_length() {
        let meters = haversine_meters(6.5665, 53.2194, 6.5675, 53.2204);
        assert!(meters > 100.0);
        assert!(meters < 200.0);
    }

    #[test]
    fn parses_common_duration_formats() {
        assert_eq!(parse_duration_seconds("5400"), Some(5400.0));
        assert_eq!(parse_duration_seconds("01:30"), Some(5400.0));
        assert_eq!(parse_duration_seconds("01:02:03"), Some(3723.0));
        assert_eq!(parse_duration_seconds("PT45M"), Some(2700.0));
        assert_eq!(parse_duration_seconds("1h 15m"), Some(4500.0));
    }

    #[test]
    fn apportions_way_duration_by_segment_length() {
        let duration = apportioned_duration_s(Some(600.0), 250, 1_000, 2).expect("duration");
        assert!((duration - 150.0).abs() < f64::EPSILON);
    }

    #[test]
    fn falls_back_to_even_duration_apportion_when_length_is_zero() {
        let duration = apportioned_duration_s(Some(600.0), 0, 0, 3).expect("duration");
        assert!((duration - 200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parses_node_based_turn_restriction_relations() {
        let relation = Relation {
            id: RelationId(42),
            tags: Tags::from_iter([
                ("type".into(), "restriction".into()),
                ("restriction".into(), "no_left_turn".into()),
            ]),
            refs: vec![
                Ref {
                    member: OsmId::Way(WayId(10)),
                    role: "from".into(),
                },
                Ref {
                    member: OsmId::Node(OsmNodeId(20)),
                    role: "via".into(),
                },
                Ref {
                    member: OsmId::Way(WayId(30)),
                    role: "to".into(),
                },
            ],
        };

        let candidate = parse_turn_restriction_relation(&relation).expect("relation parses");
        assert_eq!(candidate.relation_id, 42);
        assert_eq!(candidate.from_way_id, 10);
        assert!(matches!(candidate.via, ViaSpec::Node(20)));
        assert_eq!(candidate.to_way_id, 30);
        assert_eq!(candidate.kind, RestrictionKind::NoTurn);
        assert_eq!(
            candidate.mode_mask,
            AccessMask::new(AccessMask::CAR | AccessMask::HGV | AccessMask::TRANSIT)
        );
    }

    #[test]
    fn expands_only_turn_relations_into_prohibited_transitions() {
        let candidates = vec![TurnRestrictionCandidate {
            relation_id: 7,
            from_way_id: 10,
            via: ViaSpec::Node(100),
            to_way_id: 11,
            kind: RestrictionKind::OnlyTurn,
            mode_mask: AccessMask::new(AccessMask::CAR),
        }];
        let ways = vec![];
        let node_lookup = HashMap::from([(100_i64, (NodeId(1), 0.0, 0.0))]);
        let edges = vec![
            DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 10,
                length_m: 100,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: Default::default(),
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(1),
                from: NodeId(1),
                to: NodeId(2),
                source_way_id: 11,
                length_m: 100,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: Default::default(),
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(2),
                from: NodeId(1),
                to: NodeId(3),
                source_way_id: 12,
                length_m: 100,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: Default::default(),
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
        ];

        let restrictions = build_turn_restrictions(&candidates, &ways, &node_lookup, &edges);
        assert_eq!(restrictions.len(), 1);
        assert_eq!(restrictions[0].kind, TurnRestrictionKind::OnlyTurn);
        assert_eq!(restrictions[0].edge_path, vec![EdgeId(0), EdgeId(2)]);
    }

    #[test]
    fn parses_via_way_turn_restriction_relations() {
        let relation = Relation {
            id: RelationId(43),
            tags: Tags::from_iter([
                ("type".into(), "restriction".into()),
                ("restriction:vehicle".into(), "no_straight_on".into()),
            ]),
            refs: vec![
                Ref {
                    member: OsmId::Way(WayId(10)),
                    role: "from".into(),
                },
                Ref {
                    member: OsmId::Way(WayId(20)),
                    role: "via".into(),
                },
                Ref {
                    member: OsmId::Way(WayId(30)),
                    role: "to".into(),
                },
            ],
        };

        let candidate = parse_turn_restriction_relation(&relation).expect("relation parses");
        assert!(matches!(candidate.via, ViaSpec::Ways(ref ways) if ways == &vec![20]));
        assert_eq!(
            candidate.mode_mask,
            AccessMask::new(
                AccessMask::CAR | AccessMask::BICYCLE | AccessMask::HGV | AccessMask::TRANSIT
            )
        );
    }

    #[test]
    fn expands_via_way_only_turns_into_multi_edge_prohibitions() {
        let candidates = vec![TurnRestrictionCandidate {
            relation_id: 8,
            from_way_id: 10,
            via: ViaSpec::Ways(vec![20]),
            to_way_id: 30,
            kind: RestrictionKind::OnlyTurn,
            mode_mask: AccessMask::new(AccessMask::CAR),
        }];
        let ways = vec![
            pending_way(10, &[1, 2]),
            pending_way(20, &[2, 3, 4]),
            pending_way(30, &[4, 5]),
        ];
        let node_lookup = HashMap::from([
            (1_i64, (NodeId(0), 0.0, 0.0)),
            (2_i64, (NodeId(1), 0.0, 0.0)),
            (3_i64, (NodeId(2), 0.0, 0.0)),
            (4_i64, (NodeId(3), 0.0, 0.0)),
            (5_i64, (NodeId(4), 0.0, 0.0)),
            (6_i64, (NodeId(5), 0.0, 0.0)),
        ]);
        let edges = vec![
            edge(0, 0, 1, 10),
            edge(1, 1, 2, 20),
            edge(2, 2, 3, 20),
            edge(3, 3, 4, 30),
            edge(4, 1, 5, 99),
            edge(5, 2, 5, 98),
            edge(6, 3, 5, 97),
        ];

        let restrictions = build_turn_restrictions(&candidates, &ways, &node_lookup, &edges);
        assert_eq!(restrictions.len(), 3);
        assert_eq!(restrictions[0].edge_path, vec![EdgeId(0), EdgeId(4)]);
        assert_eq!(
            restrictions[1].edge_path,
            vec![EdgeId(0), EdgeId(1), EdgeId(5)]
        );
        assert_eq!(
            restrictions[2].edge_path,
            vec![EdgeId(0), EdgeId(1), EdgeId(2), EdgeId(6)]
        );
    }

    #[test]
    fn builds_ordered_edge_transition_acceleration_bundle() {
        let edges = vec![edge(0, 0, 1, 10), edge(1, 1, 2, 11), edge(2, 2, 3, 12)];
        let topology = TopologyBundle {
            schema_version: 1,
            source_path: "test".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 100,
                    lon: 0.0,
                    lat: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 101,
                    lon: 1.0,
                    lat: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 102,
                    lon: 2.0,
                    lat: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(3),
                    osm_node_id: 103,
                    lon: 3.0,
                    lat: 0.0,
                },
            ],
            edge_layers: Default::default(),
            edges: edges.clone(),
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: build_edge_based_topology(4, &edges),
            spatial_index: None,
            node_component_ids: vec![0, 0, 0, 0],
            edge_component_ids: vec![0, 0, 0, 0],
        };

        let bundle =
            build_dataset_acceleration_bundle(&topology, CacheBundleId::new("topology-test"));

        assert_eq!(bundle.algorithm, "edge_based_shortcut_ch_v1");
        assert_eq!(
            bundle.build_settings,
            AccelerationBuildSettings::for_profile(AccelerationBuildProfile::Balanced)
        );
        assert_eq!(bundle.stats.base_arc_count, 2);
        assert_eq!(bundle.stats.shortcut_arc_count, 0);
        assert_eq!(bundle.stats.total_arc_count, 2);
        assert_eq!(bundle.edge_order, vec![2, 0, 1]);
        assert_eq!(bundle.edge_rank, vec![1, 2, 0]);
        assert_eq!(bundle.upward_first_out, vec![0, 1, 1, 1]);
        assert_eq!(bundle.upward_head, vec![1]);
        assert_eq!(bundle.upward_path_first_out, vec![0, 1]);
        assert_eq!(bundle.upward_path_edges, vec![1]);
        assert_eq!(bundle.downward_first_out, vec![0, 0, 1, 1]);
        assert_eq!(bundle.downward_head, vec![2]);
        assert_eq!(bundle.downward_path_first_out, vec![0, 1]);
        assert_eq!(bundle.downward_path_edges, vec![2]);
    }

    #[test]
    fn compact_acceleration_profile_is_more_conservative() {
        let compact = AccelerationBuildSettings::for_profile(AccelerationBuildProfile::Compact);
        let balanced = AccelerationBuildSettings::for_profile(AccelerationBuildProfile::Balanced);
        let aggressive =
            AccelerationBuildSettings::for_profile(AccelerationBuildProfile::Aggressive);

        assert!(compact.max_shortcut_path_len < balanced.max_shortcut_path_len);
        assert!(
            compact.max_shortcuts_per_contracted_edge < balanced.max_shortcuts_per_contracted_edge
        );
        assert!(compact.max_shortcut_budget_per_edge < balanced.max_shortcut_budget_per_edge);

        assert!(aggressive.max_shortcut_path_len > balanced.max_shortcut_path_len);
        assert!(
            aggressive.max_shortcuts_per_contracted_edge
                > balanced.max_shortcuts_per_contracted_edge
        );
        assert!(aggressive.max_shortcut_budget_per_edge > balanced.max_shortcut_budget_per_edge);
    }

    #[test]
    fn persists_custom_acceleration_build_settings_in_bundle() {
        let edges = vec![edge(0, 0, 1, 10), edge(1, 1, 2, 11), edge(2, 2, 3, 12)];
        let topology = TopologyBundle {
            schema_version: 1,
            source_path: "test".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 100,
                    lon: 0.0,
                    lat: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 101,
                    lon: 1.0,
                    lat: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 102,
                    lon: 2.0,
                    lat: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(3),
                    osm_node_id: 103,
                    lon: 3.0,
                    lat: 0.0,
                },
            ],
            edge_layers: Default::default(),
            edges: edges.clone(),
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: build_edge_based_topology(4, &edges),
            spatial_index: None,
            node_component_ids: vec![0, 0, 0, 0],
            edge_component_ids: vec![0, 0, 0, 0],
        };
        let settings = AccelerationBuildSettings {
            profile: AccelerationBuildProfile::Compact,
            max_shortcut_path_len: 8,
            max_shortcuts_per_contracted_edge: 2,
            max_shortcut_budget_per_edge: 0,
        };

        let bundle = build_dataset_acceleration_bundle_with_settings(
            &topology,
            CacheBundleId::new("topology-test"),
            settings.clone(),
        );

        assert_eq!(bundle.build_settings, settings.normalized());
        assert_eq!(bundle.stats.shortcut_arc_count, 0);
        assert_eq!(bundle.stats.total_arc_count, bundle.stats.base_arc_count);
    }

    #[test]
    fn labels_weak_components_for_disconnected_subnetworks() {
        let labels = super::label_weak_components(
            4,
            &[edge(0, 0, 1, 10), edge(1, 1, 0, 10), edge(2, 2, 3, 20)],
        );

        assert_eq!(labels.component_count, 2);
        assert_eq!(labels.node_component_ids, vec![0, 0, 1, 1]);
        assert_eq!(labels.edge_component_ids, vec![0, 0, 1]);
        assert_eq!(labels.largest_component_node_count, 2);
        assert_eq!(labels.largest_component_edge_count, 2);
    }

    fn pending_way(osm_way_id: i64, node_ids: &[i64]) -> PendingWay {
        PendingWay {
            osm_way_id,
            node_ids: node_ids.to_vec(),
            road_class: RoadClass::Residential,
            duration_s: None,
            surface: SurfaceClass::Asphalt,
            smoothness: Default::default(),
            access_mask: AccessMask::new(AccessMask::CAR),
            is_toll: false,
            is_roundabout: false,
            direction: EdgeDirection::Both,
            name: None,
        }
    }

    fn edge(edge_id: u32, from: u32, to: u32, source_way_id: i64) -> DirectedEdge {
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
            name_index: None,
            geometry_offset: 0,
            geometry_len: 0,
            flags: 0,
        }
    }
}
