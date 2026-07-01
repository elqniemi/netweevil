//! Fast-load sectioned bundle format.
//!
//! Bincode decodes every array element through the serde machinery, which
//! makes loading a continent-sized bundle a parse of tens of gigabytes. This
//! format keeps small/complex fields in a bincode header but stores the big
//! primitive arrays as raw little-endian sections, so reading them is one
//! `memcpy` per array from the memory-mapped file.
//!
//! Layout: an 8-byte magic (`NWSECB` + 2-digit version) followed by
//! length-prefixed sections (`u64` little-endian byte length + payload). The
//! section order is fixed per bundle type; readers fall back to plain
//! bincode when the magic is absent, so bundles written before this format
//! keep loading.

#[cfg(target_endian = "big")]
compile_error!("the sectioned bundle format stores raw little-endian arrays");

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use bytemuck::Pod;
use memmap2::Mmap;
use netweevil_core::{
    AccessMask, CacheBundleId, CompiledAcceleration, CompiledEdgeMetric, CompiledProfileBundle,
    CompiledTurnCostConfig, DatasetAccelerationBundle, EdgeId, EdgeProfileAttributes, HighwayClass,
    NodeId, RoadClass, RoutingEdge, SmoothnessClass, SurfaceClass, TopologyBundle,
    TopologyEdgeLayers, TopologyNode, TravelMode,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const MAGIC: &[u8; 8] = b"NWSECB01";

pub(crate) fn is_sectioned(bytes: &[u8]) -> bool {
    bytes.len() >= MAGIC.len() && &bytes[..MAGIC.len()] == MAGIC
}

struct SectionWriter<W: Write> {
    writer: W,
}

impl<W: Write> SectionWriter<W> {
    fn new(mut writer: W) -> Result<Self> {
        writer.write_all(MAGIC).context("writing bundle magic")?;
        Ok(Self { writer })
    }

    fn write_bincode<T: Serialize>(&mut self, value: &T) -> Result<()> {
        let bytes = bincode::serialize(value).context("serializing bundle header section")?;
        self.writer
            .write_all(&(bytes.len() as u64).to_le_bytes())
            .context("writing section length")?;
        self.writer.write_all(&bytes).context("writing section")
    }

    fn write_raw<T: Pod>(&mut self, values: &[T]) -> Result<()> {
        let bytes: &[u8] = bytemuck::cast_slice(values);
        self.writer
            .write_all(&(bytes.len() as u64).to_le_bytes())
            .context("writing section length")?;
        self.writer.write_all(bytes).context("writing raw section")
    }

    fn finish(mut self) -> Result<()> {
        self.writer.flush().context("flushing bundle")
    }
}

struct SectionReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> SectionReader<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self> {
        if !is_sectioned(bytes) {
            bail!("missing sectioned bundle magic");
        }
        Ok(Self {
            bytes,
            offset: MAGIC.len(),
        })
    }

    fn next_section(&mut self) -> Result<&'a [u8]> {
        let header_end = self.offset + 8;
        if header_end > self.bytes.len() {
            bail!("truncated bundle: missing section length");
        }
        let len = u64::from_le_bytes(self.bytes[self.offset..header_end].try_into().unwrap());
        let end = header_end + len as usize;
        if end > self.bytes.len() {
            bail!("truncated bundle: section extends past end of file");
        }
        self.offset = end;
        Ok(&self.bytes[header_end..end])
    }

    fn read_bincode<T: DeserializeOwned>(&mut self) -> Result<T> {
        bincode::deserialize(self.next_section()?).context("parsing bundle header section")
    }

    fn read_raw<T: Pod>(&mut self) -> Result<Vec<T>> {
        let bytes = self.next_section()?;
        if bytes.len() % size_of::<T>() != 0 {
            bail!("raw section length is not a multiple of the element size");
        }
        // Copies (mmap offsets are unaligned), which is the point: one
        // memcpy instead of per-element serde decoding.
        Ok(bytemuck::allocation::pod_collect_to_vec(bytes))
    }
}

fn create_writer(path: &Path) -> Result<SectionWriter<BufWriter<File>>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    SectionWriter::new(BufWriter::new(file))
}

pub(crate) fn map_file(path: &Path) -> Result<Mmap> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    unsafe { Mmap::map(&file) }.with_context(|| format!("memory-mapping {}", path.display()))
}

/// `Option<f64>` encoded as a raw f64 with a NaN sentinel. Compiled values
/// are always finite or absent, so the sentinel is unambiguous.
fn option_to_raw(value: Option<f64>) -> f64 {
    value.unwrap_or(f64::NAN)
}

fn raw_to_option(value: f64) -> Option<f64> {
    (!value.is_nan()).then_some(value)
}

// --- Dataset acceleration bundle -----------------------------------------

#[derive(Serialize, Deserialize)]
struct AccelerationHeader {
    schema_version: u32,
    source_topology_bundle_id: CacheBundleId,
    algorithm: String,
    stats: netweevil_core::AccelerationBundleStats,
}

pub(crate) fn write_acceleration_sectioned(
    path: &Path,
    bundle: &DatasetAccelerationBundle,
) -> Result<()> {
    let mut writer = create_writer(path)?;
    writer.write_bincode(&"acceleration")?;
    writer.write_bincode(&AccelerationHeader {
        schema_version: bundle.schema_version,
        source_topology_bundle_id: bundle.source_topology_bundle_id.clone(),
        algorithm: bundle.algorithm.clone(),
        stats: bundle.stats.clone(),
    })?;
    writer.write_raw(&bundle.edge_order)?;
    writer.write_raw(&bundle.edge_rank)?;
    writer.write_raw(&bundle.upward_first_out)?;
    writer.write_raw(&bundle.upward_head)?;
    writer.write_raw(&bundle.downward_first_out)?;
    writer.write_raw(&bundle.downward_head)?;
    writer.finish()
}

pub(crate) fn read_acceleration_sectioned(bytes: &[u8]) -> Result<DatasetAccelerationBundle> {
    let mut reader = SectionReader::new(bytes)?;
    let tag: String = reader.read_bincode()?;
    if tag != "acceleration" {
        bail!("expected an acceleration bundle, found '{tag}'");
    }
    let header: AccelerationHeader = reader.read_bincode()?;
    Ok(DatasetAccelerationBundle {
        schema_version: header.schema_version,
        source_topology_bundle_id: header.source_topology_bundle_id,
        algorithm: header.algorithm,
        stats: header.stats,
        edge_order: reader.read_raw()?,
        edge_rank: reader.read_raw()?,
        upward_first_out: reader.read_raw()?,
        upward_head: reader.read_raw()?,
        downward_first_out: reader.read_raw()?,
        downward_head: reader.read_raw()?,
    })
}

// --- Compiled profile bundle ----------------------------------------------

#[derive(Serialize, Deserialize)]
struct CompiledProfileHeader {
    schema_version: u32,
    profile_id: String,
    profile_hash: String,
    mode: TravelMode,
    turn_costs: CompiledTurnCostConfig,
    source_topology_bundle_id: CacheBundleId,
    acceleration: Option<CompiledAccelerationHeader>,
}

#[derive(Serialize, Deserialize)]
struct CompiledAccelerationHeader {
    schema_version: u32,
    source_acceleration_bundle_id: CacheBundleId,
    algorithm: String,
}

pub(crate) fn write_compiled_profile_sectioned(
    path: &Path,
    bundle: &CompiledProfileBundle,
) -> Result<()> {
    let mut writer = create_writer(path)?;
    writer.write_bincode(&"compiled_profile")?;
    writer.write_bincode(&CompiledProfileHeader {
        schema_version: bundle.schema_version,
        profile_id: bundle.profile_id.clone(),
        profile_hash: bundle.profile_hash.clone(),
        mode: bundle.mode,
        turn_costs: bundle.turn_costs,
        source_topology_bundle_id: bundle.source_topology_bundle_id.clone(),
        acceleration: bundle
            .acceleration
            .as_ref()
            .map(|acceleration| CompiledAccelerationHeader {
                schema_version: acceleration.schema_version,
                source_acceleration_bundle_id: acceleration.source_acceleration_bundle_id.clone(),
                algorithm: acceleration.algorithm.clone(),
            }),
    })?;

    let edge_ids: Vec<u32> = bundle
        .edge_metrics
        .iter()
        .map(|metric| metric.edge_id.0)
        .collect();
    let travel_times: Vec<f64> = bundle
        .edge_metrics
        .iter()
        .map(|metric| option_to_raw(metric.travel_time_s))
        .collect();
    let costs: Vec<f64> = bundle
        .edge_metrics
        .iter()
        .map(|metric| option_to_raw(metric.generalized_cost))
        .collect();
    writer.write_raw(&edge_ids)?;
    writer.write_raw(&travel_times)?;
    writer.write_raw(&costs)?;

    static EMPTY_F64: &[f64] = &[];
    static EMPTY_U32: &[u32] = &[];
    match bundle.acceleration.as_ref() {
        Some(acceleration) => {
            writer.write_raw(&acceleration.upward_weight)?;
            writer.write_raw(&acceleration.upward_middle)?;
            writer.write_raw(&acceleration.downward_weight)?;
            writer.write_raw(&acceleration.downward_middle)?;
        }
        None => {
            writer.write_raw(EMPTY_F64)?;
            writer.write_raw(EMPTY_U32)?;
            writer.write_raw(EMPTY_F64)?;
            writer.write_raw(EMPTY_U32)?;
        }
    }
    writer.finish()
}

pub(crate) fn read_compiled_profile_sectioned(bytes: &[u8]) -> Result<CompiledProfileBundle> {
    let mut reader = SectionReader::new(bytes)?;
    let tag: String = reader.read_bincode()?;
    if tag != "compiled_profile" {
        bail!("expected a compiled profile bundle, found '{tag}'");
    }
    let header: CompiledProfileHeader = reader.read_bincode()?;

    let edge_ids: Vec<u32> = reader.read_raw()?;
    let travel_times: Vec<f64> = reader.read_raw()?;
    let costs: Vec<f64> = reader.read_raw()?;
    if edge_ids.len() != travel_times.len() || edge_ids.len() != costs.len() {
        bail!("compiled profile metric sections have inconsistent lengths");
    }
    let edge_metrics = edge_ids
        .into_iter()
        .zip(travel_times)
        .zip(costs)
        .map(
            |((edge_id, travel_time_s), generalized_cost)| CompiledEdgeMetric {
                edge_id: EdgeId(edge_id),
                travel_time_s: raw_to_option(travel_time_s),
                generalized_cost: raw_to_option(generalized_cost),
            },
        )
        .collect();

    let upward_weight: Vec<f64> = reader.read_raw()?;
    let upward_middle: Vec<u32> = reader.read_raw()?;
    let downward_weight: Vec<f64> = reader.read_raw()?;
    let downward_middle: Vec<u32> = reader.read_raw()?;
    let acceleration = header
        .acceleration
        .map(|acceleration| CompiledAcceleration {
            schema_version: acceleration.schema_version,
            source_acceleration_bundle_id: acceleration.source_acceleration_bundle_id,
            algorithm: acceleration.algorithm,
            upward_weight,
            upward_middle,
            downward_weight,
            downward_middle,
        });

    Ok(CompiledProfileBundle {
        schema_version: header.schema_version,
        profile_id: header.profile_id,
        profile_hash: header.profile_hash,
        mode: header.mode,
        turn_costs: header.turn_costs,
        source_topology_bundle_id: header.source_topology_bundle_id,
        acceleration,
        edge_metrics,
    })
}

/// Profile-layer layout of topology bundles written before schema 10.
#[derive(Serialize, Deserialize)]
struct LegacyEdgeProfileAttributes {
    duration_s: Option<f64>,
    road_class: RoadClass,
    highway: HighwayClass,
    surface: SurfaceClass,
    smoothness: SmoothnessClass,
    access_mask: AccessMask,
    is_toll: bool,
}

impl From<&EdgeProfileAttributes> for LegacyEdgeProfileAttributes {
    fn from(value: &EdgeProfileAttributes) -> Self {
        Self {
            duration_s: value.duration_s,
            road_class: value.road_class,
            highway: value.highway,
            surface: value.surface,
            smoothness: value.smoothness,
            access_mask: value.access_mask,
            is_toll: value.is_toll,
        }
    }
}

impl From<LegacyEdgeProfileAttributes> for EdgeProfileAttributes {
    fn from(value: LegacyEdgeProfileAttributes) -> Self {
        Self {
            duration_s: value.duration_s,
            road_class: value.road_class,
            highway: value.highway,
            surface: value.surface,
            smoothness: value.smoothness,
            access_mask: value.access_mask,
            is_toll: value.is_toll,
            max_speed_kph: None,
            lanes: None,
        }
    }
}

// --- Topology bundle --------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct TopologyHeader {
    schema_version: u32,
    source_path: String,
    source_sha256: String,
    turn_restrictions: Vec<netweevil_core::TurnRestriction>,
    names: Vec<String>,
    spatial_index: Option<netweevil_core::NodeSpatialIndex>,
}

pub(crate) fn write_topology_sectioned(path: &Path, bundle: &TopologyBundle) -> Result<()> {
    // Legacy in-memory bundles carry AoS `edges`; convert to the SoA layers
    // without cloning the rest of the bundle.
    let converted_layers;
    let layers: &TopologyEdgeLayers =
        if bundle.edge_layers.routing.is_empty() && !bundle.edges.is_empty() {
            converted_layers = TopologyEdgeLayers::from_directed_edges(&bundle.edges);
            &converted_layers
        } else {
            &bundle.edge_layers
        };

    let mut writer = create_writer(path)?;
    writer.write_bincode(&"topology")?;
    writer.write_bincode(&TopologyHeader {
        schema_version: bundle.schema_version,
        source_path: bundle.source_path.clone(),
        source_sha256: bundle.source_sha256.clone(),
        turn_restrictions: bundle.turn_restrictions.clone(),
        names: bundle.names.clone(),
        spatial_index: bundle.spatial_index.clone(),
    })?;

    // Nodes as columnar raw arrays.
    let node_ids: Vec<u32> = bundle.nodes.iter().map(|node| node.node_id.0).collect();
    let osm_node_ids: Vec<i64> = bundle.nodes.iter().map(|node| node.osm_node_id).collect();
    let lons: Vec<f64> = bundle.nodes.iter().map(|node| node.lon).collect();
    let lats: Vec<f64> = bundle.nodes.iter().map(|node| node.lat).collect();
    writer.write_raw(&node_ids)?;
    writer.write_raw(&osm_node_ids)?;
    writer.write_raw(&lons)?;
    writer.write_raw(&lats)?;

    // Routing edges as columnar raw arrays.
    let edge_ids: Vec<u32> = layers.routing.iter().map(|edge| edge.edge_id.0).collect();
    let froms: Vec<u32> = layers.routing.iter().map(|edge| edge.from.0).collect();
    let tos: Vec<u32> = layers.routing.iter().map(|edge| edge.to.0).collect();
    let way_ids: Vec<i64> = layers
        .routing
        .iter()
        .map(|edge| edge.source_way_id)
        .collect();
    let lengths: Vec<u32> = layers.routing.iter().map(|edge| edge.length_m).collect();
    let flags: Vec<u32> = layers.routing.iter().map(|edge| edge.flags).collect();
    writer.write_raw(&edge_ids)?;
    writer.write_raw(&froms)?;
    writer.write_raw(&tos)?;
    writer.write_raw(&way_ids)?;
    writer.write_raw(&lengths)?;
    writer.write_raw(&flags)?;

    // Attribute layers stay bincode: they are enum-heavy and comparatively
    // small next to the numeric arrays. Bundles carrying a pre-10 schema
    // version keep the pre-10 field layout so version-gated reads stay
    // consistent.
    if bundle.schema_version >= 10 {
        writer.write_bincode(&layers.profile)?;
    } else {
        let legacy: Vec<LegacyEdgeProfileAttributes> = layers
            .profile
            .iter()
            .map(LegacyEdgeProfileAttributes::from)
            .collect();
        writer.write_bincode(&legacy)?;
    }
    writer.write_bincode(&layers.presentation)?;

    writer.write_raw(&bundle.edge_based_topology.node_first_out)?;
    writer.write_raw(&bundle.edge_based_topology.node_edge_order)?;
    writer.write_raw(&bundle.edge_based_topology.edge_transition_first_out)?;
    writer.write_raw(&bundle.edge_based_topology.edge_transition_edges)?;
    writer.write_raw(&bundle.node_component_ids)?;
    writer.write_raw(&bundle.edge_component_ids)?;
    writer.finish()
}

pub(crate) fn read_topology_sectioned(bytes: &[u8]) -> Result<TopologyBundle> {
    let mut reader = SectionReader::new(bytes)?;
    let tag: String = reader.read_bincode()?;
    if tag != "topology" {
        bail!("expected a topology bundle, found '{tag}'");
    }
    let header: TopologyHeader = reader.read_bincode()?;

    let node_ids: Vec<u32> = reader.read_raw()?;
    let osm_node_ids: Vec<i64> = reader.read_raw()?;
    let lons: Vec<f64> = reader.read_raw()?;
    let lats: Vec<f64> = reader.read_raw()?;
    if node_ids.len() != osm_node_ids.len()
        || node_ids.len() != lons.len()
        || node_ids.len() != lats.len()
    {
        bail!("topology node sections have inconsistent lengths");
    }
    let nodes = node_ids
        .into_iter()
        .zip(osm_node_ids)
        .zip(lons)
        .zip(lats)
        .map(|(((node_id, osm_node_id), lon), lat)| TopologyNode {
            node_id: NodeId(node_id),
            osm_node_id,
            lon,
            lat,
        })
        .collect();

    let edge_ids: Vec<u32> = reader.read_raw()?;
    let froms: Vec<u32> = reader.read_raw()?;
    let tos: Vec<u32> = reader.read_raw()?;
    let way_ids: Vec<i64> = reader.read_raw()?;
    let lengths: Vec<u32> = reader.read_raw()?;
    let flags: Vec<u32> = reader.read_raw()?;
    if edge_ids.len() != froms.len()
        || edge_ids.len() != tos.len()
        || edge_ids.len() != way_ids.len()
        || edge_ids.len() != lengths.len()
        || edge_ids.len() != flags.len()
    {
        bail!("topology routing-edge sections have inconsistent lengths");
    }
    let routing = edge_ids
        .into_iter()
        .zip(froms)
        .zip(tos)
        .zip(way_ids)
        .zip(lengths)
        .zip(flags)
        .map(
            |(((((edge_id, from), to), source_way_id), length_m), flags)| RoutingEdge {
                edge_id: EdgeId(edge_id),
                from: NodeId(from),
                to: NodeId(to),
                source_way_id,
                length_m,
                flags,
            },
        )
        .collect();

    // Schema 10 added max_speed/lanes to the profile layer; bincode is not
    // self-describing, so bundles written before that decode through the old
    // field layout.
    let profile: Vec<EdgeProfileAttributes> = if header.schema_version >= 10 {
        reader.read_bincode()?
    } else {
        reader
            .read_bincode::<Vec<LegacyEdgeProfileAttributes>>()?
            .into_iter()
            .map(EdgeProfileAttributes::from)
            .collect()
    };
    let presentation = reader.read_bincode()?;

    Ok(TopologyBundle {
        schema_version: header.schema_version,
        source_path: header.source_path,
        source_sha256: header.source_sha256,
        nodes,
        edge_layers: TopologyEdgeLayers {
            routing,
            profile,
            presentation,
        },
        edges: Vec::new(),
        turn_restrictions: header.turn_restrictions,
        names: header.names,
        edge_based_topology: netweevil_core::EdgeBasedTopology {
            node_first_out: reader.read_raw()?,
            node_edge_order: reader.read_raw()?,
            edge_transition_first_out: reader.read_raw()?,
            edge_transition_edges: reader.read_raw()?,
        },
        spatial_index: header.spatial_index,
        node_component_ids: reader.read_raw()?,
        edge_component_ids: reader.read_raw()?,
    })
}
