use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use netweevil_core::{
    AdminBoundarySource, ConnectedComponentKind, ConnectedComponentsMeta, DemImportSummary,
    DirectedEdge, EDGE_FLAG_ROUNDABOUT, EDGE_FLAG_TARGET_TRAFFIC_SIGNAL, EdgeBasedTopology, EdgeId,
    EdgeNameBundle, NodeId, TOPOLOGY_BUNDLE_SCHEMA_VERSION, TopologyBundle, TopologyBundleMeta,
    TopologyElevation, TopologyNode,
};
use std::collections::BTreeMap;

use rustc_hash::FxHashMap;

use crate::admin::{AdminIndex, read_boundaries_file};
use crate::dem::{DemRaster, LonLatBounds};
use crate::elevation::{DemSampling, SegmentElevation, assign_node_elevations, segment_elevation};
use crate::import::{
    DatasetImportProgress, DatasetImportStage, DemImportOptions, PercentReporter, emit_progress,
};
use crate::restrictions::build_turn_restrictions;
use crate::scan::{PendingWay, ScanOutput, scan_osm};
use crate::timezones::assign_timezones;
use netweevil_core::SourceFormat;

/// Optional inputs that enrich nodes and edges beyond the network source.
#[derive(Debug, Clone, Default)]
pub(crate) struct EnrichmentInputs {
    /// Boundaries file (GeoJSON or OSM PBF) replacing the extract's admin
    /// relations, with its SHA-256.
    pub(crate) admin_boundaries: Option<(PathBuf, String)>,
    /// DEM to sample, with its SHA-256.
    pub(crate) dem: Option<(DemImportOptions, String)>,
}

/// Topology plus what the import needs to persist alongside it.
pub(crate) struct BuiltTopology {
    pub(crate) bundle: TopologyBundle,
    pub(crate) names: EdgeNameBundle,
    pub(crate) meta: TopologyBundleMeta,
    /// The cropped DEM, for the runtime elevation raster.
    pub(crate) dem: Option<DemRaster>,
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn build_topology_bundle(
    source_paths: &[PathBuf],
    source_size_bytes: u64,
    source_sha256: &str,
    source_format: SourceFormat,
    mapping: Option<&crate::GeoPackageMapping>,
    enrichment: &EnrichmentInputs,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<BuiltTopology> {
    let source_path = source_paths
        .first()
        .ok_or_else(|| anyhow::anyhow!("dataset import requires at least one source"))?;
    let scan = match source_format {
        SourceFormat::OsmPbf => scan_osm(
            source_path,
            source_size_bytes,
            enrichment.admin_boundaries.is_none(),
            progress,
        )?,
        SourceFormat::OvertureParquet => {
            crate::overture::scan_overture(source_path, source_size_bytes, progress)?
        }
        SourceFormat::GeoPackage => crate::geopackage::scan_geopackages(
            source_paths,
            mapping.ok_or_else(|| anyhow::anyhow!("GeoPackage import requires a mapping"))?,
            progress,
        )?,
    };
    let mut warnings = Vec::new();
    let bounds = node_bounds(&scan);
    let dem = match &enrichment.dem {
        None => None,
        Some(_) if source_format == SourceFormat::GeoPackage => bail!(
            "DEM sampling applies to OSM and Overture sources; GeoPackage imports keep their surveyed heights"
        ),
        Some((options, _)) => {
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
        Some((path, sha256)) => {
            let (boundaries, incomplete) = read_boundaries_file(path)?;
            (
                AdminBoundarySource::File {
                    path: path.display().to_string(),
                    sha256: sha256.clone(),
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
    let (mut bundle, names, meta) = build_topology(
        source_path,
        source_sha256,
        scan,
        sampling.as_ref(),
        progress,
    )?;
    if let (Some(sampling), Some((options, sha256))) = (&sampling, &enrichment.dem)
        && let Some(summary) = bundle.elevation.dem.as_mut()
    {
        summary.source =
            sampling
                .dem
                .source_info(&options.path, sha256, options.vertical_datum.clone());
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

    bundle.source_path = source_paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(";");
    Ok(BuiltTopology {
        bundle,
        names,
        meta,
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
    source_sha256: &str,
    scan: ScanOutput,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<(TopologyBundle, EdgeNameBundle, TopologyBundleMeta)> {
    build_topology(source_path, source_sha256, scan, None, progress)
}

/// Builds the topology; with `dem`, node heights come from the DEM and every
/// edge carries a densified ascent/descent and its steepest grades.
pub(crate) fn build_topology(
    source_path: &Path,
    source_sha256: &str,
    scan: ScanOutput,
    dem: Option<&DemSampling<'_>>,
    progress: &mut impl FnMut(DatasetImportProgress),
) -> Result<(TopologyBundle, EdgeNameBundle, TopologyBundleMeta)> {
    let ScanOutput {
        pending_ways,
        restriction_candidates,
        mut node_coords,
        traffic_signal_nodes,
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
    let mut edge_grades = Vec::new();
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
            segments.push((from_id, to_id, from, to, length_m, profile));
        }

        if segments.is_empty() {
            skipped_way_count += 1;
            continue;
        }

        let total_length_m = segments
            .iter()
            .map(|(_, _, _, _, length_m, _)| *length_m as u64)
            .sum();
        let segment_count = segments.len() as u32;
        for (from_id, to_id, from_osm_id, to_osm_id, length_m, profile) in segments {
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

            if way.forward_access_mask.0 != 0 {
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
                    temporal_rule_id: way.temporal_rule_id,
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
                edge_grades.push(profile);
            }

            if way.reverse_access_mask.0 != 0 {
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
                    temporal_rule_id: way.temporal_rule_id,
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
                edge_grades.push(profile.reversed());
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

    let mut edge_layers = netweevil_core::TopologyEdgeLayers::from_directed_edges(&edges);
    for (profile, highway) in edge_layers.profile.iter_mut().zip(edge_highways) {
        profile.highway = highway;
    }
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
        regions: Default::default(),
        elevation: match (dem, node_elevation_stats) {
            (Some(sampling), Some(stats)) => TopologyElevation {
                dem: Some(DemImportSummary {
                    source: sampling.dem.source_info(source_path, "", None),
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
