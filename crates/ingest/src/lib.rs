use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};
use netan_core::{
    AccessMask, BuildStage, CacheBundleId, DatasetId, DirectedEdge, EdgeId, NodeId, RoadClass,
    SmoothnessClass, SurfaceClass, TopologyBundle, TopologyBundleMeta, TopologyNode,
    TurnRestriction, TurnRestrictionKind,
};
use netan_persist::{WorkspacePaths, write_dataset_manifest, write_topology_bundle};
use netan_report::{BundleRef, DatasetManifest, now_rfc3339};
use osmpbfreader::{OsmId, OsmObj, OsmPbfReader, Relation, Tags};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct DatasetImportOptions {
    pub name: String,
    pub source: String,
}

pub fn import_dataset(
    paths: &WorkspacePaths,
    source_path: impl AsRef<Path>,
    options: DatasetImportOptions,
) -> Result<DatasetManifest> {
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
    let sha256 = sha256_file(file)?;
    let dataset_id = DatasetId::new(options.name);
    let bundle_id = CacheBundleId::new(format!("topology-{}-{}", dataset_id.0, &sha256[..12]));
    let bundle_path = paths
        .topology_bundles_dir
        .join(format!("{}.bin", bundle_id.0));
    let (bundle, topology_meta) = build_topology_bundle(source_path, &sha256)?;
    write_topology_bundle(&bundle_path, &bundle)?;

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
        topology_meta: Some(topology_meta),
    };

    write_dataset_manifest(paths, &manifest)?;
    Ok(manifest)
}

fn sha256_file(file: File) -> Result<String> {
    let mut reader = BufReader::new(file);
    let mut buffer = [0_u8; 64 * 1024];
    let mut hasher = Sha256::new();
    loop {
        let read = reader
            .read(&mut buffer)
            .context("reading file for hashing")?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
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
    source_sha256: &str,
) -> Result<(TopologyBundle, TopologyBundleMeta)> {
    let (pending_ways, restriction_candidates, needed_nodes, counts) =
        scan_routable_objects(source_path)?;
    let node_coords = load_node_coords(source_path, &needed_nodes)?;
    let (nodes, node_lookup) = build_nodes(&pending_ways, &node_coords);
    let name_lookup = build_name_lookup(&pending_ways);

    let mut edges = Vec::new();
    let mut skipped_way_count = 0_u64;
    for way in &pending_ways {
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
            segments.push((from_id, to_id, length_m));
        }

        if segments.is_empty() {
            skipped_way_count += 1;
            continue;
        }

        let total_length_m = segments
            .iter()
            .map(|(_, _, length_m)| *length_m as u64)
            .sum();
        let segment_count = segments.len() as u32;
        for (from_id, to_id, length_m) in segments {
            let duration_s =
                apportioned_duration_s(way.duration_s, length_m, total_length_m, segment_count);

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
                    flags: 0,
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
                    flags: 0,
                });
            }
        }
    }

    let turn_restrictions =
        build_turn_restrictions(&restriction_candidates, &pending_ways, &node_lookup, &edges);

    let mut names = vec![String::new(); name_lookup.len()];
    for (name, index) in name_lookup {
        names[index as usize] = name;
    }

    let bundle = TopologyBundle {
        schema_version: 4,
        source_path: source_path.display().to_string(),
        source_sha256: source_sha256.to_string(),
        nodes,
        edges,
        turn_restrictions,
        names,
    };
    let meta = TopologyBundleMeta {
        node_count: bundle.nodes.len() as u64,
        edge_count: bundle.edges.len() as u64,
        geometry_bytes: 0,
        turn_count: bundle.turn_restrictions.len() as u64,
        source_node_count: counts.nodes,
        source_way_count: counts.ways,
        source_relation_count: counts.relations,
        routable_way_count: pending_ways.len() as u64,
        skipped_way_count,
    };
    Ok((bundle, meta))
}

fn scan_routable_objects(
    source_path: &Path,
) -> Result<(
    Vec<PendingWay>,
    Vec<TurnRestrictionCandidate>,
    HashSet<i64>,
    ObjectCounts,
)> {
    let file = File::open(source_path)
        .with_context(|| format!("opening dataset source {}", source_path.display()))?;
    let mut reader = OsmPbfReader::new(file);
    let mut ways = Vec::new();
    let mut restriction_candidates = Vec::new();
    let mut node_ids = HashSet::new();
    let mut counts = ObjectCounts::default();

    for object in reader.iter() {
        match object.context("reading PBF object")? {
            OsmObj::Node(_) => counts.nodes += 1,
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
    }

    Ok((ways, restriction_candidates, node_ids, counts))
}

fn load_node_coords(
    source_path: &Path,
    needed_nodes: &HashSet<i64>,
) -> Result<HashMap<i64, (f64, f64)>> {
    let file = File::open(source_path)
        .with_context(|| format!("opening dataset source {}", source_path.display()))?;
    let mut reader = OsmPbfReader::new(file);
    let mut coords = HashMap::with_capacity(needed_nodes.len());

    for object in reader.iter() {
        let OsmObj::Node(node) = object.context("reading PBF node")? else {
            continue;
        };
        let node_id = node.id.0;
        if needed_nodes.contains(&node_id) {
            coords.insert(node_id, (node.lon(), node.lat()));
        }
    }

    Ok(coords)
}

fn build_nodes(
    ways: &[PendingWay],
    node_coords: &HashMap<i64, (f64, f64)>,
) -> (Vec<TopologyNode>, HashMap<i64, (NodeId, f64, f64)>) {
    let mut used_nodes = BTreeSet::new();
    for way in ways {
        for node_id in &way.node_ids {
            if node_coords.contains_key(node_id) {
                used_nodes.insert(*node_id);
            }
        }
    }

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
    let mut distinct_names = BTreeSet::new();
    for way in ways {
        if let Some(name) = &way.name {
            distinct_names.insert(name.clone());
        }
    }

    distinct_names
        .into_iter()
        .enumerate()
        .map(|(index, name)| (name, index as u32))
        .collect()
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
        return Some((road_class, classify_access(highway, tag(tags, "route"))));
    }

    if tag(tags, "route") == Some("ferry") || tag(tags, "ferry").is_some() {
        return Some((
            RoadClass::Ferry,
            AccessMask::new(
                AccessMask::CAR
                    | AccessMask::BICYCLE
                    | AccessMask::FOOT
                    | AccessMask::TRANSIT
                    | AccessMask::HGV,
            ),
        ));
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

fn classify_access(highway: &str, route: Option<&str>) -> AccessMask {
    if route == Some("ferry") {
        return AccessMask::new(
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
    AccessMask::new(bits)
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
        apportioned_duration_s, build_turn_restrictions, classify_access, classify_direction,
        classify_highway, haversine_meters, parse_duration_seconds,
        parse_turn_restriction_relation,
    };
    use netan_core::{
        AccessMask, DirectedEdge, EdgeId, NodeId, RoadClass, SurfaceClass, TurnRestrictionKind,
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
        assert_eq!(
            classify_access("motorway", None),
            AccessMask::new(AccessMask::CAR | AccessMask::HGV)
        );
        assert_eq!(
            classify_access("cycleway", None),
            AccessMask::new(AccessMask::BICYCLE | AccessMask::FOOT)
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
