//! Fast-load sectioned bundle format.
//!
//! Small and enum-heavy fields live in a bincode header section; the big
//! primitive arrays are stored as raw little-endian sections, so reading one
//! is a single `memcpy` from the memory-mapped file rather than a per-element
//! serde decode.
//!
//! Layout: the 8-byte magic [`CURRENT_MAGIC`] followed by length-prefixed
//! sections (`u64` little-endian byte length + payload). The section order is
//! fixed per bundle type.

#[cfg(target_endian = "big")]
compile_error!("the sectioned bundle format stores raw little-endian arrays");

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use bytemuck::Pod;
use memmap2::Mmap;
use netweevil_core::{
    ACCELERATION_BUNDLE_SCHEMA_VERSION, COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION, CacheBundleId,
    CompiledAcceleration, CompiledCostComponent, CompiledEdgeMetric, CompiledProfileBundle,
    CompiledTemporalProfile, CompiledTurnCostConfig, DatasetAccelerationBundle, EdgeId,
    EdgeProfileAttributes, NodeId, RoutingEdge, TOPOLOGY_BUNDLE_SCHEMA_VERSION, TopologyBundle,
    TopologyEdgeLayers, TopologyNode, TravelMode,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// The magic every bundle carries. Files that do not start with it are
/// rejected; there is no second format to fall back to.
const CURRENT_MAGIC: &[u8; 8] = b"NWSECB05";
const MAGIC_LEN: usize = CURRENT_MAGIC.len();

/// Error for a file that is not a current sectioned bundle.
pub(crate) fn unsupported_format_error(path: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "bundle {} uses an unsupported format; re-import the dataset and recompile profiles",
        path.display()
    )
}

struct SectionWriter<W: Write> {
    writer: W,
}

impl<W: Write> SectionWriter<W> {
    fn new(mut writer: W) -> Result<Self> {
        writer
            .write_all(CURRENT_MAGIC)
            .context("writing bundle magic")?;
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
    fn new(bytes: &'a [u8], path: &Path) -> Result<Self> {
        if bytes.len() < MAGIC_LEN || &bytes[..MAGIC_LEN] != CURRENT_MAGIC {
            return Err(unsupported_format_error(path));
        }
        Ok(Self {
            bytes,
            offset: MAGIC_LEN,
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

pub(crate) fn read_acceleration_sectioned(
    bytes: &[u8],
    path: &Path,
) -> Result<DatasetAccelerationBundle> {
    let mut reader = SectionReader::new(bytes, path)?;
    let tag: String = reader.read_bincode()?;
    if tag != "acceleration" {
        bail!("expected an acceleration bundle, found '{tag}'");
    }
    let header: AccelerationHeader = reader.read_bincode()?;
    if header.schema_version != ACCELERATION_BUNDLE_SCHEMA_VERSION {
        return Err(unsupported_format_error(path));
    }
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

#[derive(Serialize, Deserialize)]
struct CompiledCostComponentHeader {
    name: String,
    weight: f64,
    scales_with_travel_time: bool,
    overlay_name: Option<String>,
    invert_overlay: bool,
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

    static EMPTY_U32: &[u32] = &[];
    match bundle.acceleration.as_ref() {
        Some(acceleration) => {
            writer.write_raw(&acceleration.upward_weight)?;
            writer.write_raw(&acceleration.downward_weight)?;
            writer.write_raw(&acceleration.time_upward_weight)?;
            writer.write_raw(&acceleration.time_downward_weight)?;
            writer.write_raw(&acceleration.distance_upward_weight)?;
            writer.write_raw(&acceleration.distance_downward_weight)?;
        }
        None => {
            for _ in 0..6 {
                writer.write_raw(EMPTY_U32)?;
            }
        }
    }
    writer.write_bincode(&bundle.temporal)?;
    writer.write_bincode(
        &bundle
            .components
            .iter()
            .map(|component| CompiledCostComponentHeader {
                name: component.name.clone(),
                weight: component.weight,
                scales_with_travel_time: component.scales_with_travel_time,
                overlay_name: component.overlay_name.clone(),
                invert_overlay: component.invert_overlay,
            })
            .collect::<Vec<_>>(),
    )?;
    for component in &bundle.components {
        writer.write_raw(&component.edge_values)?;
    }
    writer.finish()
}

pub(crate) fn read_compiled_profile_sectioned(
    bytes: &[u8],
    path: &Path,
) -> Result<CompiledProfileBundle> {
    let mut reader = SectionReader::new(bytes, path)?;
    let tag: String = reader.read_bincode()?;
    if tag != "compiled_profile" {
        bail!("expected a compiled profile bundle, found '{tag}'");
    }
    let header: CompiledProfileHeader = reader.read_bincode()?;
    if header.schema_version != COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION {
        return Err(unsupported_format_error(path));
    }

    let edge_ids: Vec<u32> = reader.read_raw()?;
    let travel_times: Vec<f64> = reader.read_raw()?;
    let costs: Vec<f64> = reader.read_raw()?;
    if edge_ids.len() != travel_times.len() || edge_ids.len() != costs.len() {
        bail!("compiled profile metric sections have inconsistent lengths");
    }
    let edge_metrics: Vec<CompiledEdgeMetric> = edge_ids
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

    let upward_weight: Vec<u32> = reader.read_raw()?;
    let downward_weight: Vec<u32> = reader.read_raw()?;
    let time_upward_weight: Vec<u32> = reader.read_raw()?;
    let time_downward_weight: Vec<u32> = reader.read_raw()?;
    let distance_upward_weight: Vec<u32> = reader.read_raw()?;
    let distance_downward_weight: Vec<u32> = reader.read_raw()?;
    let acceleration = header
        .acceleration
        .map(|acceleration| CompiledAcceleration {
            schema_version: acceleration.schema_version,
            source_acceleration_bundle_id: acceleration.source_acceleration_bundle_id,
            algorithm: acceleration.algorithm,
            upward_weight,
            downward_weight,
            time_upward_weight,
            time_downward_weight,
            distance_upward_weight,
            distance_downward_weight,
        });
    let temporal: CompiledTemporalProfile = reader.read_bincode()?;
    let component_headers: Vec<CompiledCostComponentHeader> = reader.read_bincode()?;
    let mut components = Vec::with_capacity(component_headers.len());
    for component in component_headers {
        let edge_values: Vec<f32> = reader.read_raw()?;
        if edge_values.len() != edge_metrics.len() {
            bail!(
                "compiled profile component '{}' has {} values for {} edge metrics",
                component.name,
                edge_values.len(),
                edge_metrics.len()
            );
        }
        components.push(CompiledCostComponent {
            name: component.name,
            weight: component.weight,
            edge_values,
            scales_with_travel_time: component.scales_with_travel_time,
            overlay_name: component.overlay_name,
            invert_overlay: component.invert_overlay,
        });
    }

    Ok(CompiledProfileBundle {
        schema_version: header.schema_version,
        profile_id: header.profile_id,
        profile_hash: header.profile_hash,
        mode: header.mode,
        turn_costs: header.turn_costs,
        components,
        temporal,
        source_topology_bundle_id: header.source_topology_bundle_id,
        acceleration,
        edge_metrics,
    })
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
    let layers = &bundle.edge_layers;
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
    let lons: Vec<f64> = bundle.nodes.iter().map(|node| node.lon).collect();
    let lats: Vec<f64> = bundle.nodes.iter().map(|node| node.lat).collect();
    let elevations: Vec<f64> = bundle.nodes.iter().map(|node| node.z).collect();
    writer.write_raw(&node_ids)?;
    writer.write_raw(&lons)?;
    writer.write_raw(&lats)?;
    writer.write_raw(&elevations)?;

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
    let ascents: Vec<f32> = layers.routing.iter().map(|edge| edge.ascent_m).collect();
    let descents: Vec<f32> = layers.routing.iter().map(|edge| edge.descent_m).collect();
    let flags: Vec<u32> = layers.routing.iter().map(|edge| edge.flags).collect();
    let feature_rows: Vec<u32> = layers.routing.iter().map(|edge| edge.feature_row).collect();
    let source_directions: Vec<i8> = layers
        .routing
        .iter()
        .map(|edge| edge.source_direction)
        .collect();
    let temporal_rule_ids: Vec<u32> = layers
        .routing
        .iter()
        .map(|edge| edge.temporal_rule_id.unwrap_or(u32::MAX))
        .collect();
    writer.write_raw(&edge_ids)?;
    writer.write_raw(&froms)?;
    writer.write_raw(&tos)?;
    writer.write_raw(&way_ids)?;
    writer.write_raw(&lengths)?;
    writer.write_raw(&ascents)?;
    writer.write_raw(&descents)?;
    writer.write_raw(&flags)?;
    writer.write_raw(&feature_rows)?;
    writer.write_raw(&source_directions)?;
    writer.write_raw(&temporal_rule_ids)?;

    // Attribute layers stay bincode: they are enum-heavy and comparatively
    // small next to the numeric arrays.
    writer.write_bincode(&layers.profile)?;
    writer.write_bincode(&layers.presentation)?;

    writer.write_raw(&bundle.edge_based_topology.node_first_out)?;
    writer.write_raw(&bundle.edge_based_topology.node_edge_order)?;
    writer.write_raw(&bundle.edge_based_topology.edge_transition_first_out)?;
    writer.write_raw(&bundle.edge_based_topology.edge_transition_edges)?;
    writer.write_raw(&bundle.node_component_ids)?;
    writer.write_raw(&bundle.edge_component_ids)?;
    writer.write_bincode(&bundle.feature_attributes)?;
    writer.write_bincode(&bundle.temporal_rule_sets)?;
    writer.finish()
}

pub(crate) fn read_topology_sectioned(bytes: &[u8], path: &Path) -> Result<TopologyBundle> {
    let mut reader = SectionReader::new(bytes, path)?;
    let tag: String = reader.read_bincode()?;
    if tag != "topology" {
        bail!("expected a topology bundle, found '{tag}'");
    }
    let header: TopologyHeader = reader.read_bincode()?;
    if header.schema_version != TOPOLOGY_BUNDLE_SCHEMA_VERSION {
        return Err(unsupported_format_error(path));
    }

    let node_ids: Vec<u32> = reader.read_raw()?;
    let lons: Vec<f64> = reader.read_raw()?;
    let lats: Vec<f64> = reader.read_raw()?;
    let elevations: Vec<f64> = reader.read_raw()?;
    if node_ids.len() != lons.len()
        || node_ids.len() != lats.len()
        || node_ids.len() != elevations.len()
    {
        bail!("topology node sections have inconsistent lengths");
    }
    let nodes = node_ids
        .into_iter()
        .zip(lons)
        .zip(lats)
        .zip(elevations)
        .map(|(((node_id, lon), lat), z)| TopologyNode {
            node_id: NodeId(node_id),
            lon,
            lat,
            z,
        })
        .collect();

    let edge_ids: Vec<u32> = reader.read_raw()?;
    let froms: Vec<u32> = reader.read_raw()?;
    let tos: Vec<u32> = reader.read_raw()?;
    let way_ids: Vec<i64> = reader.read_raw()?;
    let lengths: Vec<u32> = reader.read_raw()?;
    let ascents: Vec<f32> = reader.read_raw()?;
    let descents: Vec<f32> = reader.read_raw()?;
    let flags: Vec<u32> = reader.read_raw()?;
    let feature_rows: Vec<u32> = reader.read_raw()?;
    let source_directions: Vec<i8> = reader.read_raw()?;
    let temporal_rule_ids: Vec<u32> = reader.read_raw()?;
    if edge_ids.len() != froms.len()
        || edge_ids.len() != tos.len()
        || edge_ids.len() != way_ids.len()
        || edge_ids.len() != lengths.len()
        || edge_ids.len() != ascents.len()
        || edge_ids.len() != descents.len()
        || edge_ids.len() != flags.len()
        || edge_ids.len() != feature_rows.len()
        || edge_ids.len() != source_directions.len()
        || edge_ids.len() != temporal_rule_ids.len()
    {
        bail!("topology routing-edge sections have inconsistent lengths");
    }
    let routing = (0..edge_ids.len())
        .map(|index| RoutingEdge {
            edge_id: EdgeId(edge_ids[index]),
            from: NodeId(froms[index]),
            to: NodeId(tos[index]),
            source_way_id: way_ids[index],
            length_m: lengths[index],
            ascent_m: ascents[index],
            descent_m: descents[index],
            feature_row: feature_rows[index],
            source_direction: source_directions[index],
            temporal_rule_id: (temporal_rule_ids[index] != u32::MAX)
                .then_some(temporal_rule_ids[index]),
            flags: flags[index],
        })
        .collect();

    let profile: Vec<EdgeProfileAttributes> = reader.read_bincode()?;
    let presentation = reader.read_bincode()?;

    let edge_based_topology = netweevil_core::EdgeBasedTopology {
        node_first_out: reader.read_raw()?,
        node_edge_order: reader.read_raw()?,
        edge_transition_first_out: reader.read_raw()?,
        edge_transition_edges: reader.read_raw()?,
    };
    let node_component_ids = reader.read_raw()?;
    let edge_component_ids = reader.read_raw()?;
    let feature_attributes = reader.read_bincode()?;
    let temporal_rule_sets = reader.read_bincode()?;

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
        turn_restrictions: header.turn_restrictions,
        names: header.names,
        edge_based_topology,
        spatial_index: header.spatial_index,
        node_component_ids,
        edge_component_ids,
        feature_attributes,
        temporal_rule_sets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use netweevil_core::EdgePresentation;

    fn push_bincode<T: Serialize>(bytes: &mut Vec<u8>, value: &T) {
        let section = bincode::serialize(value).expect("section serializes");
        bytes.extend_from_slice(&(section.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&section);
    }

    fn push_raw<T: bytemuck::Pod>(bytes: &mut Vec<u8>, value: &[T]) {
        let section = bytemuck::cast_slice(value);
        bytes.extend_from_slice(&(section.len() as u64).to_le_bytes());
        bytes.extend_from_slice(section);
    }

    /// Builds a topology bundle whose header carries `schema_version`.
    fn topology_bytes(magic: &[u8; 8], schema_version: u32) -> Vec<u8> {
        let mut bytes = magic.to_vec();
        push_bincode(&mut bytes, &"topology");
        push_bincode(
            &mut bytes,
            &TopologyHeader {
                schema_version,
                source_path: "city.osm.pbf".to_string(),
                source_sha256: "abc".to_string(),
                turn_restrictions: Vec::new(),
                names: Vec::new(),
                spatial_index: None,
            },
        );
        push_raw(&mut bytes, &[0_u32, 1]);
        push_raw(&mut bytes, &[114.1_f64, 114.2]);
        push_raw(&mut bytes, &[22.3_f64, 22.4]);
        push_raw(&mut bytes, &[0.0_f64, 0.0]);
        push_raw(&mut bytes, &[0_u32]);
        push_raw(&mut bytes, &[0_u32]);
        push_raw(&mut bytes, &[1_u32]);
        push_raw(&mut bytes, &[42_i64]);
        push_raw(&mut bytes, &[100_u32]);
        push_raw(&mut bytes, &[0.0_f32]);
        push_raw(&mut bytes, &[0.0_f32]);
        push_raw(&mut bytes, &[0_u32]);
        push_raw(&mut bytes, &[netweevil_core::NO_FEATURE_ROW]);
        push_raw(&mut bytes, &[0_i8]);
        push_raw(&mut bytes, &[u32::MAX]);
        push_bincode(&mut bytes, &vec![EdgeProfileAttributes::default()]);
        push_bincode(&mut bytes, &vec![EdgePresentation::default()]);
        for _ in 0..6 {
            push_raw::<u32>(&mut bytes, &[]);
        }
        push_bincode(
            &mut bytes,
            &netweevil_core::FeatureAttributeTable::default(),
        );
        push_bincode(&mut bytes, &Vec::<netweevil_core::TemporalRuleSet>::new());
        bytes
    }

    #[test]
    fn reads_a_current_topology_bundle() {
        let bytes = topology_bytes(CURRENT_MAGIC, TOPOLOGY_BUNDLE_SCHEMA_VERSION);
        let bundle = read_topology_sectioned(&bytes, Path::new("topology.bin"))
            .expect("current topology reads");
        assert_eq!(bundle.nodes.len(), 2);
        assert_eq!(bundle.edge_count(), 1);
        assert_eq!(bundle.routing_edge(0).length_m, 100);
    }

    #[test]
    fn rejects_an_unrecognized_magic() {
        let bytes = topology_bytes(b"INVALID!", TOPOLOGY_BUNDLE_SCHEMA_VERSION);
        let error = read_topology_sectioned(&bytes, Path::new("topology.bin"))
            .expect_err("an unrecognized magic is rejected");
        assert!(
            error.to_string().contains("re-import the dataset"),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_mismatched_schema_version() {
        let bytes = topology_bytes(CURRENT_MAGIC, TOPOLOGY_BUNDLE_SCHEMA_VERSION - 1);
        let error = read_topology_sectioned(&bytes, Path::new("topology.bin"))
            .expect_err("a mismatched schema version is rejected");
        assert!(
            error.to_string().contains("re-import the dataset"),
            "{error}"
        );
    }
}
