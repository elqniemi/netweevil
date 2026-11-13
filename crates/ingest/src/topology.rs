use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use netweevil_core::{
    AdminBoundarySource, CacheBundleId, ConnectedComponentKind, ConnectedComponentsMeta,
    DemImportSummary, DirectedEdge, EDGE_FLAG_ROUNDABOUT, EDGE_FLAG_TARGET_TRAFFIC_SIGNAL,
    EdgeBasedTopology, EdgeId, EdgeNameBundle, NodeId, TOPOLOGY_BUNDLE_SCHEMA_VERSION,
    TopologyBundle, TopologyBundleMeta, TopologyElevation, TopologyNode, VehicleRestriction,
    VehicleRestrictionTable,
};
use std::collections::BTreeMap;

use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::admin::{AdminIndex, read_boundaries_file};
use crate::dem::{DemRaster, LonLatBounds};
use crate::elevation::{DemSampling, SegmentElevation, assign_node_elevations, segment_elevation};
use crate::guidance::GuidanceInterner;
use crate::import::{
    DatasetImportProgress, DatasetImportStage, DemImportOptions, PercentReporter, emit_progress,
};
use crate::pbf::IdIndex;
use crate::restrictions::build_turn_restrictions;
use crate::scan::{PendingWay, ScanOutput};
use crate::source::OsmSourceState;
use crate::timezones::assign_timezones;
use netweevil_core::SourceFormat;
use rayon::prelude::*;

/// Optional inputs that enrich nodes and edges beyond the network source.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct EnrichmentInputs {
    /// Boundaries file (GeoJSON or OSM PBF) replacing the extract's admin
    /// relations.
    pub(crate) admin_boundaries: Option<PathBuf>,
    /// DEM to sample.
    pub(crate) dem: Option<DemImportOptions>,
}

/// A topology built from a dataset source, plus what the import persists
/// next to it.
pub(crate) struct BuiltTopology {
    pub(crate) bundle: TopologyBundle,
    pub(crate) names: EdgeNameBundle,
    pub(crate) meta: TopologyBundleMeta,
    /// Whether the build wrote a source snapshot (OSM sources).
    pub(crate) source_snapshot_written: bool,
    /// The cropped DEM, for the runtime elevation raster.
    pub(crate) dem: Option<DemRaster>,
    pub(crate) warnings: Vec<String>,
}

/// Where an OSM build writes its source snapshot: the snapshot file and the
/// id of the topology bundle it belongs to.
pub(crate) struct SnapshotTarget<'a> {
    pub(crate) path: &'a Path,
    pub(crate) topology_bundle_id: CacheBundleId,
}

/// Scans the dataset source and builds its topology. An OSM source writes
/// its snapshot to `snapshot` right after the scan and releases the source
/// state before the topology build.
pub(crate) fn build_topology_bundle(
    source_paths: &[PathBuf],
    source_size_bytes: u64,
    source_format: SourceFormat,
    mapping: Option<&crate::GeoPackageMapping>,
    enrichment: &EnrichmentInputs,
    snapshot: SnapshotTarget<'_>,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<BuiltTopology> {
    let source_path = source_paths
        .first()
        .ok_or_else(|| anyhow::anyhow!("dataset import requires at least one source"))?;
    let mut source_snapshot_written = false;
    let scan = match source_format {
        SourceFormat::OsmPbf => {
            let state = OsmSourceState::scan_pbf(source_path, progress)?;
            let topology_node_ids = state.topology_node_ids();
            emit_progress(
                progress,
                DatasetImportStage::WriteTopologyBundle,
                None,
                format!("Writing source snapshot bundle {}", snapshot.path.display()),
            );
            state.write_snapshot(
                snapshot.path,
                snapshot.topology_bundle_id,
                &topology_node_ids,
                enrichment,
            )?;
            source_snapshot_written = true;
            state.into_scan_output(&topology_node_ids, enrichment.admin_boundaries.is_none())
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
    let mut built =
        build_enriched_topology(source_path, scan, source_format, enrichment, progress)?;
    built.bundle.source_path = source_paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(";");
    built.source_snapshot_written = source_snapshot_written;
    Ok(built)
}

/// Builds the topology of a scan and enriches it: DEM heights and grades,
/// timezones and admin areas. Imports and dataset updates share it, so an
/// update with the same inputs builds what a full import builds.
pub(crate) fn build_enriched_topology(
    source_path: &Path,
    scan: ScanOutput,
    source_format: SourceFormat,
    enrichment: &EnrichmentInputs,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<BuiltTopology> {
    let mut warnings = Vec::new();
    let bounds = node_bounds(&scan);
    let dem = match &enrichment.dem {
        None => None,
        Some(_) if source_format == SourceFormat::GeoPackage => bail!(
            "DEM sampling applies to OSM and Overture sources; GeoPackage imports keep their surveyed heights"
        ),
        Some(options) => {
            emit_progress(
                progress,
                DatasetImportStage::SampleElevation,
                Some(0.0),
                format!("Reading DEM {}", options.path.display()),
            );
            let bounds =
                bounds.ok_or_else(|| anyhow::anyhow!("the source has no nodes to sample"))?;
            Some(DemRaster::read(
                &options.path,
                options.crs.as_deref(),
                Some(bounds),
            )?)
        }
    };
    let (admin_source, boundaries, incomplete) = match &enrichment.admin_boundaries {
        Some(path) => {
            let (boundaries, incomplete) = read_boundaries_file(path)?;
            (
                AdminBoundarySource::File {
                    path: path.display().to_string(),
                },
                boundaries,
                incomplete,
            )
        }
        None => (
            AdminBoundarySource::SourceExtract,
            scan.admin_boundaries.clone(),
            scan.incomplete_admin_relations.clone(),
        ),
    };
    if let Some(first) = incomplete.first() {
        warnings.push(format!(
            "{} admin boundary relation(s) are incomplete in the boundaries source and were skipped (for example relation {first}); pass --admin-boundaries with complete boundaries",
            incomplete.len(),
        ));
    }

    let sampling = dem.as_ref().map(|dem| {
        let reference_lat = bounds.map_or(0.0, |bounds| (bounds.min_lat + bounds.max_lat) / 2.0);
        DemSampling::new(dem, reference_lat)
    });
    let (mut bundle, names, meta) = build_topology(source_path, scan, sampling.as_ref(), progress)?;
    if let (Some(sampling), Some(options)) = (&sampling, &enrichment.dem)
        && let Some(summary) = bundle.elevation.dem.as_mut()
    {
        summary.source = sampling
            .dem
            .source_info(&options.path, options.vertical_datum.clone());
    }

    emit_progress(
        progress,
        DatasetImportStage::AssignRegions,
        None,
        format!("Assigning timezones to {} nodes", bundle.nodes.len()),
    );
    let (timezones, node_timezone) = assign_timezones(&bundle.nodes);
    bundle.regions.timezones = timezones;
    bundle.regions.node_timezone = node_timezone;
    let index = AdminIndex::new(boundaries);
    if !index.is_empty() {
        emit_progress(
            progress,
            DatasetImportStage::AssignRegions,
            None,
            format!(
                "Assigning administrative areas to {} nodes",
                bundle.nodes.len()
            ),
        );
        let (areas, node_areas) = index.assign(&bundle.nodes);
        bundle.regions.admin_source = Some(admin_source);
        bundle.regions.admin_areas = areas;
        bundle.regions.node_admin_area = node_areas;
    } else if enrichment.admin_boundaries.is_some() {
        warnings
            .push("the boundaries file contains no usable admin_level 2 or 4 areas".to_string());
    }

    Ok(BuiltTopology {
        bundle,
        names,
        meta,
        source_snapshot_written: false,
        dem,
        warnings,
    })
}

fn node_bounds(scan: &ScanOutput) -> Option<LonLatBounds> {
    let mut coords = scan.node_coords.values();
    let &(lon, lat, _) = coords.next()?;
    let mut bounds = LonLatBounds {
        min_lon: lon,
        min_lat: lat,
        max_lon: lon,
        max_lat: lat,
    };
    for &(lon, lat, _) in coords {
        bounds.min_lon = bounds.min_lon.min(lon);
        bounds.min_lat = bounds.min_lat.min(lat);
        bounds.max_lon = bounds.max_lon.max(lon);
        bounds.max_lat = bounds.max_lat.max(lat);
    }
    Some(bounds)
}

/// Builds a topology from a scan without DEM sampling.
pub(crate) fn build_topology_from_scan(
    source_path: &Path,
    scan: ScanOutput,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<(TopologyBundle, EdgeNameBundle, TopologyBundleMeta)> {
    build_topology(source_path, scan, None, progress)
}

/// Builds the topology; with `dem`, node heights come from the DEM and every
/// edge carries a densified ascent/descent and its steepest grades.
pub(crate) fn build_topology(
    source_path: &Path,
    scan: ScanOutput,
    dem: Option<&DemSampling<'_>>,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<(TopologyBundle, EdgeNameBundle, TopologyBundleMeta)> {
    let ScanOutput {
        pending_ways,
        restriction_candidates,
        mut node_coords,
        traffic_signal_nodes,
        junction_nodes,
        counts,
        feature_attributes,
        temporal_rule_sets,
        structure_ways,
        ..
    } = scan;
    let node_elevation_stats = dem.map(|sampling| {
        emit_progress(
            progress,
            DatasetImportStage::SampleElevation,
            None,
            format!("Sampling DEM heights for {} nodes", node_coords.len()),
        );
        assign_node_elevations(sampling, &mut node_coords, &pending_ways, &structure_ways)
    });
    checked_u32(node_coords.len(), "nodes")?;
    let node_coordinate_count = node_coords.len();
    let (node_ids, coords) = node_coords.into_parts();
    let nodes = coords
        .into_iter()
        .map(|(lon, lat, z)| TopologyNode::new(lon, lat, z))
        .collect::<Vec<_>>();
    let name_lookup = build_name_lookup(&pending_ways);

    emit_progress(
        progress,
        DatasetImportStage::BuildTopology,
        Some(0.0),
        format!(
            "Building topology from {} routable ways and {} node coordinates",
            pending_ways.len(),
            node_coordinate_count
        ),
    );
    let spatial_index = build_spatial_index(&nodes);

    let mut edges = Vec::new();
    let mut edge_highways = Vec::new();
    let mut guidance = GuidanceInterner::new();
    // (destination-only, conditional access) per edge.
    let mut edge_masks = Vec::new();
    let mut vehicle_restrictions = VehicleRestrictionTableBuilder::default();
    let mut edge_grades = Vec::new();
    let mut skipped_way_count = 0_u64;
    let mut build_reporter = PercentReporter::starting_at_zero();
    let mut way_index = 0;
    for chunk in pending_ways.chunks(WAY_CHUNK) {
        // Segments are measured in parallel; edges are emitted in way order.
        let chunk_segments = chunk
            .par_iter()
            .map(|way| way_segments(way, &node_ids, &nodes, dem, &structure_ways))
            .collect::<Vec<_>>();
        for (way, segments) in chunk.iter().zip(chunk_segments) {
            way_index += 1;
            let name_index = way
                .name
                .as_ref()
                .and_then(|name| name_lookup.get(name))
                .copied();

            if segments.is_empty() {
                skipped_way_count += 1;
                continue;
            }

            let total_length_m = segments.iter().map(|segment| segment.length_m as u64).sum();
            let segment_count = segments.len() as u32;
            for segment in segments {
                let Segment {
                    from_id,
                    to_id,
                    from_osm_id,
                    to_osm_id,
                    length_m,
                    profile,
                } = segment;
                let SegmentElevation {
                    ascent_m,
                    descent_m,
                    ..
                } = profile;
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
                    let edge_id = checked_u32(edges.len(), "directed edges")?;
                    edge_masks.push((way.forward_destination_only, way.forward_conditional_access));
                    vehicle_restrictions.add_edge(edge_id, &way.vehicle_restrictions);
                    edges.push(DirectedEdge {
                        edge_id: EdgeId(edge_id),
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
                        flags: forward_flags | way.forward_extra_flags,
                    });
                    edge_highways.push(way.highway);
                    guidance.push_edge(
                        way.guidance.as_deref(),
                        true,
                        way.highway,
                        junction_nodes.get(&from_osm_id),
                    );
                    edge_grades.push(profile);
                }

                if way.has_reverse_edge() {
                    let edge_id = checked_u32(edges.len(), "directed edges")?;
                    edge_masks.push((way.reverse_destination_only, way.reverse_conditional_access));
                    vehicle_restrictions.add_edge(edge_id, &way.vehicle_restrictions);
                    edges.push(DirectedEdge {
                        edge_id: EdgeId(edge_id),
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
                        flags: reverse_flags | way.reverse_extra_flags,
                    });
                    edge_highways.push(way.highway);
                    guidance.push_edge(
                        way.guidance.as_deref(),
                        false,
                        way.highway,
                        junction_nodes.get(&to_osm_id),
                    );
                    edge_grades.push(profile.reversed());
                }
            }

            build_reporter.emit_if_needed(
                way_index as u64,
                pending_ways.len() as u64,
                DatasetImportStage::BuildTopology,
                progress,
                |percent| {
                    format!(
                        "Building topology {:.0}% ({}/{})",
                        percent,
                        way_index,
                        pending_ways.len()
                    )
                },
            );
        }
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
        build_turn_restrictions(&restriction_candidates, &pending_ways, &node_ids, &edges);

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
    let edge_based_topology = build_edge_based_topology(nodes.len(), &edges)?;
    let components = label_weak_components(nodes.len(), &edges);

    let mut edge_layers = netweevil_core::TopologyEdgeLayers::from_directed_edges(&edges);
    for (edge_index, (highway, (destination_only, conditional_access))) in
        edge_highways.into_iter().zip(edge_masks).enumerate()
    {
        edge_layers.update_profile(edge_index, |profile| {
            profile.highway = highway;
            profile.destination_only = destination_only;
            profile.conditional_access = conditional_access;
        });
    }
    edge_layers.vehicle_restrictions = vehicle_restrictions.finish();
    let bundle = TopologyBundle {
        schema_version: TOPOLOGY_BUNDLE_SCHEMA_VERSION,
        source_path: source_path.display().to_string(),
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
        regions: Default::default(),
        elevation: match (dem, node_elevation_stats) {
            (Some(sampling), Some(stats)) => TopologyElevation {
                dem: Some(DemImportSummary {
                    source: sampling.dem.source_info(source_path, None),
                    sample_spacing_m: sampling.spacing_m,
                    sampled_node_count: stats.sampled,
                    missing_node_count: stats.missing,
                    structure_node_count: stats.structure,
                }),
                edge_max_uphill_permille: edge_grades
                    .iter()
                    .map(|grade| grade.max_uphill_permille)
                    .collect(),
                edge_max_downhill_permille: edge_grades
                    .iter()
                    .map(|grade| grade.max_downhill_permille)
                    .collect(),
            },
            _ => TopologyElevation::default(),
        },
    };
    let meta = TopologyBundleMeta {
        node_count: bundle.nodes.len() as u64,
        edge_count: bundle.edge_count() as u64,
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
    pub(crate) fn add_edge(&mut self, edge_id: u32, restrictions: &[VehicleRestriction]) {
        for restriction in restrictions {
            let next_id = self.table.restrictions.len() as u32;
            let id = *self.ids.entry(*restriction).or_insert_with(|| {
                self.table.restrictions.push(*restriction);
                next_id
            });
            self.table.edges.push(edge_id);
            self.table.edge_restrictions.push(id);
        }
    }

    pub(crate) fn finish(self) -> VehicleRestrictionTable {
        self.table
    }
}

/// The edge-transition graph of `edges`, after checking that its
/// transitions (one per edge entering a node and edge leaving it) fit the
/// graph's 32-bit offsets.
pub(crate) fn build_edge_based_topology(
    node_count: usize,
    edges: &[DirectedEdge],
) -> Result<EdgeBasedTopology> {
    let mut out_degree = vec![0_u64; node_count];
    for edge in edges {
        out_degree[edge.from.0 as usize] += 1;
    }
    let transitions = edges
        .iter()
        .map(|edge| out_degree[edge.to.0 as usize])
        .sum::<u64>();
    checked_u32(
        usize::try_from(transitions).unwrap_or(usize::MAX),
        "edge transitions",
    )?;
    Ok(EdgeBasedTopology::build(
        node_count,
        edges.len(),
        |edge_index| (edges[edge_index].from.0, edges[edge_index].to.0),
    ))
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

/// Routable ways measured per parallel batch of the topology build.
const WAY_CHUNK: usize = 1 << 14;

/// One segment of a way between consecutive distinct nodes that both have
/// coordinates.
struct Segment {
    from_id: NodeId,
    to_id: NodeId,
    from_osm_id: i64,
    to_osm_id: i64,
    length_m: u32,
    profile: SegmentElevation,
}

/// The segments of `way`; `node_ids` are the source ids of `nodes`.
fn way_segments(
    way: &PendingWay,
    node_ids: &IdIndex,
    nodes: &[TopologyNode],
    dem: Option<&DemSampling<'_>>,
    structure_ways: &FxHashSet<i64>,
) -> Vec<Segment> {
    let mut segments = Vec::new();
    for pair in way.node_ids.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        if from == to {
            continue;
        }
        let (Some(from_index), Some(to_index)) = (node_ids.position(from), node_ids.position(to))
        else {
            continue;
        };
        let (from_node, to_node) = (&nodes[from_index], &nodes[to_index]);
        let (from_lon, from_lat, from_z) = (from_node.lon(), from_node.lat(), from_node.z());
        let (to_lon, to_lat, to_z) = (to_node.lon(), to_node.lat(), to_node.z());
        let length_m =
            distance_3d_meters(from_lon, from_lat, from_z, to_lon, to_lat, to_z).round() as u32;
        let profile = match dem {
            Some(sampling) => segment_elevation(
                sampling,
                (from_lon, from_lat, from_z),
                (to_lon, to_lat, to_z),
                structure_ways.contains(&way.osm_way_id),
            ),
            None => {
                let (ascent_m, descent_m) = ascent_descent_meters(from_z, to_z);
                SegmentElevation {
                    ascent_m,
                    descent_m,
                    ..SegmentElevation::default()
                }
            }
        };
        segments.push(Segment {
            from_id: NodeId(from_index as u32),
            to_id: NodeId(to_index as u32),
            from_osm_id: from,
            to_osm_id: to,
            length_m,
            profile,
        });
    }
    segments
}

/// Narrows a count or next id to the 32-bit ids topologies and CCH
/// hierarchies use, failing when the network exceeds them.
pub(crate) fn checked_u32(value: usize, what: &str) -> Result<u32> {
    u32::try_from(value).map_err(|_| {
        anyhow::anyhow!(
            "the network has more than {} {what}, the limit of 32-bit ids",
            u32::MAX
        )
    })
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
