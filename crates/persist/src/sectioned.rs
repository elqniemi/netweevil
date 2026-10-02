//! Compact sectioned bundle format.
//!
//! Small and enum-heavy fields live in bincode sections; numeric columns use
//! the lossless encodings in [`crate::codec`] (constant, identity, delta and
//! varint, fixed-point coordinates, interned records). Structures that can be
//! rebuilt from other columns (edge-transition CSR, spatial grid, edge
//! components, CCH ranks) are recomputed at load whenever the writer verified
//! that the rebuild reproduces them exactly; otherwise they are stored.
//!
//! Layout: the 8-byte magic [`CURRENT_MAGIC`] followed by length-prefixed
//! sections (`u64` little-endian byte length + payload). The section order is
//! fixed per bundle type.

#[cfg(target_endian = "big")]
compile_error!("the sectioned bundle format stores raw little-endian arrays");

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use bytemuck::Pod;
use memmap2::Mmap;
use netweevil_core::{
    ACCELERATION_BUNDLE_SCHEMA_VERSION, COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION, CacheBundleId,
    CompiledAcceleration, CompiledCostComponent, CompiledEdgeMetric, CompiledProfileBundle,
    CompiledTemporalProfile, CompiledTurnCostConfig, DatasetAccelerationBundle, EdgeBasedTopology,
    EdgeId, EdgePresentation, EdgeProfileAttributes, FeatureAttributeColumn,
    FeatureAttributeColumnData, FeatureAttributeDefinition, FeatureAttributeTable, NodeId,
    NodeSpatialIndex, RoutingEdge, TOPOLOGY_BUNDLE_SCHEMA_VERSION, TopologyBundle,
    TopologyEdgeLayers, TopologyNode, TravelMode,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::codec;

/// The magic every bundle carries. Files that do not start with it are
/// rejected; there is no second format to fall back to.
const CURRENT_MAGIC: &[u8; 8] = b"NWSECB06";
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

    fn write_section(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer
            .write_all(&(bytes.len() as u64).to_le_bytes())
            .context("writing section length")?;
        self.writer.write_all(bytes).context("writing section")
    }

    fn write_bincode<T: Serialize>(&mut self, value: &T) -> Result<()> {
        let bytes = bincode::serialize(value).context("serializing bundle header section")?;
        self.write_section(&bytes)
    }

    fn write_raw<T: Pod>(&mut self, values: &[T]) -> Result<()> {
        self.write_section(bytemuck::cast_slice(values))
    }

    fn write_u32s(&mut self, values: &[u32]) -> Result<()> {
        self.write_section(&codec::encode_u32s(values))
    }

    fn write_i64s(&mut self, values: &[i64]) -> Result<()> {
        self.write_section(&codec::encode_i64s(values))
    }

    fn write_f64s(&mut self, values: &[f64]) -> Result<()> {
        self.write_section(&codec::encode_f64s(values))
    }

    fn write_f32s(&mut self, values: &[f32]) -> Result<()> {
        self.write_section(&codec::encode_f32s(values))
    }

    /// Writes `values` as a bincode table of distinct records plus a
    /// per-element index column.
    fn write_interned<T: Serialize>(&mut self, values: &[T]) -> Result<()> {
        let mut lookup: HashMap<Vec<u8>, u32> = HashMap::new();
        let mut table_bytes = Vec::new();
        let mut table_len = 0_u64;
        let mut indexes = Vec::with_capacity(values.len());
        let mut scratch = Vec::new();
        for value in values {
            scratch.clear();
            bincode::serialize_into(&mut scratch, value).context("serializing interned record")?;
            let index = match lookup.get(scratch.as_slice()) {
                Some(index) => *index,
                None => {
                    let index = u32::try_from(table_len).context("too many distinct records")?;
                    table_bytes.extend_from_slice(&scratch);
                    table_len += 1;
                    lookup.insert(scratch.clone(), index);
                    index
                }
            };
            indexes.push(index);
        }
        // A bincode `Vec<T>` is its u64 length followed by the elements.
        let mut table = Vec::with_capacity(table_bytes.len() + 8);
        table.extend_from_slice(&table_len.to_le_bytes());
        table.extend_from_slice(&table_bytes);
        self.write_section(&table)?;
        self.write_u32s(&indexes)
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
        let end = usize::try_from(len)
            .ok()
            .and_then(|len| header_end.checked_add(len))
            .filter(|end| *end <= self.bytes.len());
        let Some(end) = end else {
            bail!("truncated bundle: section extends past end of file");
        };
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
        // Copies (mmap offsets are unaligned): one memcpy per array.
        Ok(bytemuck::allocation::pod_collect_to_vec(bytes))
    }

    fn read_u32s(&mut self) -> Result<Vec<u32>> {
        codec::decode_u32s(self.next_section()?)
    }

    fn read_i64s(&mut self) -> Result<Vec<i64>> {
        codec::decode_i64s(self.next_section()?)
    }

    fn read_f64s(&mut self) -> Result<Vec<f64>> {
        codec::decode_f64s(self.next_section()?)
    }

    fn read_f32s(&mut self) -> Result<Vec<f32>> {
        codec::decode_f32s(self.next_section()?)
    }

    fn read_interned<T: DeserializeOwned + Clone>(&mut self) -> Result<Vec<T>> {
        let table: Vec<T> = self.read_bincode()?;
        let indexes = self.read_u32s()?;
        indexes
            .into_iter()
            .map(|index| {
                table
                    .get(index as usize)
                    .cloned()
                    .context("interned record index out of range")
            })
            .collect()
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

/// `Option<u32>` as `i64` with `-1` for `None`, which the varint codecs
/// store in one byte.
fn option_u32_to_i64(value: Option<u32>) -> i64 {
    value.map_or(-1, i64::from)
}

fn i64_to_option_u32(value: i64) -> Result<Option<u32>> {
    if value == -1 {
        return Ok(None);
    }
    u32::try_from(value)
        .map(Some)
        .context("optional u32 column value out of range")
}

fn ensure_len(name: &str, actual: usize, expected: usize) -> Result<()> {
    if actual != expected {
        bail!("bundle column '{name}' has {actual} values, expected {expected}");
    }
    Ok(())
}

/// A structure the reader can rebuild from other sections. `Derived` is
/// written only when the writer checked that the rebuild is identical.
#[derive(Serialize, Deserialize)]
enum Stored<T> {
    Derived,
    Explicit(T),
}

// --- Dataset acceleration bundle -----------------------------------------

#[derive(Serialize, Deserialize)]
struct AccelerationHeader {
    schema_version: u32,
    source_topology_bundle_id: CacheBundleId,
    algorithm: String,
    stats: netweevil_core::AccelerationBundleStats,
}

/// Inverse permutation of `order`, or `None` when `order` is not a
/// permutation of `0..order.len()`.
fn invert_permutation(order: &[u32]) -> Option<Vec<u32>> {
    let mut rank = vec![u32::MAX; order.len()];
    for (position, &item) in order.iter().enumerate() {
        let slot = rank.get_mut(item as usize)?;
        if *slot != u32::MAX {
            return None;
        }
        *slot = position as u32;
    }
    Some(rank)
}

pub(crate) fn write_acceleration_sectioned(
    path: &Path,
    bundle: &DatasetAccelerationBundle,
) -> Result<()> {
    let mut writer = create_writer(path)?;
    encode_acceleration(&mut writer, bundle)?;
    writer.finish()
}

fn encode_acceleration<W: Write>(
    writer: &mut SectionWriter<W>,
    bundle: &DatasetAccelerationBundle,
) -> Result<()> {
    writer.write_bincode(&"acceleration")?;
    writer.write_bincode(&AccelerationHeader {
        schema_version: bundle.schema_version,
        source_topology_bundle_id: bundle.source_topology_bundle_id.clone(),
        algorithm: bundle.algorithm.clone(),
        stats: bundle.stats.clone(),
    })?;
    writer.write_u32s(&bundle.edge_order)?;
    if invert_permutation(&bundle.edge_order).as_ref() == Some(&bundle.edge_rank) {
        writer.write_bincode(&Stored::<()>::Derived)?;
    } else {
        writer.write_bincode(&Stored::Explicit(()))?;
        writer.write_u32s(&bundle.edge_rank)?;
    }
    writer.write_u32s(&bundle.upward_first_out)?;
    writer.write_section(&codec::encode_sorted_rows(
        &bundle.upward_first_out,
        &bundle.upward_head,
    ))?;
    writer.write_u32s(&bundle.downward_first_out)?;
    writer.write_section(&codec::encode_sorted_rows(
        &bundle.downward_first_out,
        &bundle.downward_head,
    ))
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
    let edge_order = reader.read_u32s()?;
    let edge_rank = match reader.read_bincode::<Stored<()>>()? {
        Stored::Derived => {
            invert_permutation(&edge_order).context("CCH edge order is not a permutation")?
        }
        Stored::Explicit(()) => reader.read_u32s()?,
    };
    let upward_first_out = reader.read_u32s()?;
    let upward_head = codec::decode_sorted_rows(&upward_first_out, reader.next_section()?)?;
    let downward_first_out = reader.read_u32s()?;
    let downward_head = codec::decode_sorted_rows(&downward_first_out, reader.next_section()?)?;
    Ok(DatasetAccelerationBundle {
        schema_version: header.schema_version,
        source_topology_bundle_id: header.source_topology_bundle_id,
        algorithm: header.algorithm,
        stats: header.stats,
        edge_order,
        edge_rank,
        upward_first_out,
        upward_head,
        downward_first_out,
        downward_head,
    })
}

// --- Compiled profile bundle ----------------------------------------------

/// `Option<f64>` encoded as a raw f64 with a NaN sentinel. Compiled values
/// are always finite or absent, so the sentinel is unambiguous.
fn option_to_raw(value: Option<f64>) -> f64 {
    value.unwrap_or(f64::NAN)
}

fn raw_to_option(value: f64) -> Option<f64> {
    (!value.is_nan()).then_some(value)
}


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
    node_count: u64,
    edge_count: u64,
}

/// Column kind of one feature-attribute column; the data follows in
/// codec sections.
#[derive(Serialize, Deserialize)]
enum FeatureColumnKind {
    Boolean,
    Integer,
    Float,
    String,
    Json,
}

#[derive(Serialize, Deserialize)]
struct FeatureTableHeader {
    row_count: u32,
    strings: Vec<String>,
    columns: Vec<(FeatureAttributeDefinition, FeatureColumnKind)>,
}

fn present_flags<T>(values: &[Option<T>]) -> Vec<u32> {
    values.iter().map(|value| u32::from(value.is_some())).collect()
}

fn write_feature_attributes<W: Write>(
    writer: &mut SectionWriter<W>,
    table: &FeatureAttributeTable,
) -> Result<()> {
    writer.write_bincode(&FeatureTableHeader {
        row_count: table.row_count,
        strings: table.strings.clone(),
        columns: table
            .columns
            .iter()
            .map(|column| {
                let kind = match column.data {
                    FeatureAttributeColumnData::Boolean(_) => FeatureColumnKind::Boolean,
                    FeatureAttributeColumnData::Integer(_) => FeatureColumnKind::Integer,
                    FeatureAttributeColumnData::Float(_) => FeatureColumnKind::Float,
                    FeatureAttributeColumnData::String(_) => FeatureColumnKind::String,
                    FeatureAttributeColumnData::Json(_) => FeatureColumnKind::Json,
                };
                (column.definition.clone(), kind)
            })
            .collect(),
    })?;
    for column in &table.columns {
        match &column.data {
            FeatureAttributeColumnData::Boolean(values) => {
                let coded: Vec<u32> = values
                    .iter()
                    .map(|value| value.map_or(0, |value| 1 + u32::from(value)))
                    .collect();
                writer.write_u32s(&coded)?;
            }
            FeatureAttributeColumnData::Integer(values) => {
                writer.write_u32s(&present_flags(values))?;
                writer.write_i64s(&values.iter().flatten().copied().collect::<Vec<_>>())?;
            }
            FeatureAttributeColumnData::Float(values) => {
                writer.write_u32s(&present_flags(values))?;
                writer.write_f64s(&values.iter().flatten().copied().collect::<Vec<_>>())?;
            }
            FeatureAttributeColumnData::String(values) | FeatureAttributeColumnData::Json(values) => {
                let coded: Vec<i64> = values.iter().map(|value| option_u32_to_i64(*value)).collect();
                writer.write_i64s(&coded)?;
            }
        }
    }
    Ok(())
}

fn read_present<T>(flags: &[u32], values: Vec<T>) -> Result<Vec<Option<T>>> {
    let mut values = values.into_iter();
    let column = flags
        .iter()
        .map(|flag| match flag {
            0 => Ok(None),
            1 => values
                .next()
                .map(Some)
                .context("feature attribute column is missing values"),
            _ => bail!("invalid feature attribute presence flag"),
        })
        .collect::<Result<Vec<_>>>()?;
    if values.next().is_some() {
        bail!("feature attribute column has surplus values");
    }
    Ok(column)
}

fn read_feature_attributes(reader: &mut SectionReader<'_>) -> Result<FeatureAttributeTable> {
    let header: FeatureTableHeader = reader.read_bincode()?;
    let mut columns = Vec::with_capacity(header.columns.len());
    for (definition, kind) in header.columns {
        let data = match kind {
            FeatureColumnKind::Boolean => FeatureAttributeColumnData::Boolean(
                reader
                    .read_u32s()?
                    .into_iter()
                    .map(|value| match value {
                        0 => Ok(None),
                        1 => Ok(Some(false)),
                        2 => Ok(Some(true)),
                        _ => bail!("invalid boolean feature attribute"),
                    })
                    .collect::<Result<_>>()?,
            ),
            FeatureColumnKind::Integer => {
                let flags = reader.read_u32s()?;
                FeatureAttributeColumnData::Integer(read_present(&flags, reader.read_i64s()?)?)
            }
            FeatureColumnKind::Float => {
                let flags = reader.read_u32s()?;
                FeatureAttributeColumnData::Float(read_present(&flags, reader.read_f64s()?)?)
            }
            FeatureColumnKind::String | FeatureColumnKind::Json => {
                let values = reader
                    .read_i64s()?
                    .into_iter()
                    .map(i64_to_option_u32)
                    .collect::<Result<Vec<_>>>()?;
                if matches!(kind, FeatureColumnKind::String) {
                    FeatureAttributeColumnData::String(values)
                } else {
                    FeatureAttributeColumnData::Json(values)
                }
            }
        };
        columns.push(FeatureAttributeColumn { definition, data });
    }
    Ok(FeatureAttributeTable {
        row_count: header.row_count,
        strings: header.strings,
        columns,
    })
}

fn derived_edge_based_topology(
    node_count: usize,
    routing: &[RoutingEdge],
) -> Option<EdgeBasedTopology> {
    routing
        .iter()
        .all(|edge| (edge.from.0 as usize) < node_count && (edge.to.0 as usize) < node_count)
        .then(|| {
            EdgeBasedTopology::build(node_count, routing.len(), |edge_index| {
                (routing[edge_index].from.0, routing[edge_index].to.0)
            })
        })
}

fn derived_edge_component_ids(
    node_component_ids: &[u32],
    routing: &[RoutingEdge],
) -> Option<Vec<u32>> {
    routing
        .iter()
        .map(|edge| node_component_ids.get(edge.from.0 as usize).copied())
        .collect()
}

pub(crate) fn write_topology_sectioned(path: &Path, bundle: &TopologyBundle) -> Result<()> {
    let mut writer = create_writer(path)?;
    encode_topology(&mut writer, bundle)?;
    writer.finish()
}

fn encode_topology<W: Write>(writer: &mut SectionWriter<W>, bundle: &TopologyBundle) -> Result<()> {
    let layers = &bundle.edge_layers;
    let edge_count = layers.routing.len();
    ensure_len("edge profile layer", layers.profile.len(), edge_count)?;
    ensure_len("edge presentation layer", layers.presentation.len(), edge_count)?;
    writer.write_bincode(&"topology")?;
    writer.write_bincode(&TopologyHeader {
        schema_version: bundle.schema_version,
        source_path: bundle.source_path.clone(),
        source_sha256: bundle.source_sha256.clone(),
        turn_restrictions: bundle.turn_restrictions.clone(),
        names: bundle.names.clone(),
        node_count: bundle.nodes.len() as u64,
        edge_count: edge_count as u64,
    })?;

    // Nodes: ids are normally their index, coordinates OSM decimicro
    // degrees, elevation absent; the codecs collapse all three.
    let column = |f: fn(&TopologyNode) -> f64| bundle.nodes.iter().map(f).collect::<Vec<_>>();
    writer.write_u32s(&bundle.nodes.iter().map(|node| node.node_id.0).collect::<Vec<_>>())?;
    writer.write_f64s(&column(|node| node.lon))?;
    writer.write_f64s(&column(|node| node.lat))?;
    writer.write_f64s(&column(|node| node.z))?;
    let spatial_index = match &bundle.spatial_index {
        None => Some(None),
        Some(index) if NodeSpatialIndex::build(&bundle.nodes).as_ref() == Some(index) => None,
        Some(index) => Some(Some(index.clone())),
    };
    match spatial_index {
        None => writer.write_bincode(&Stored::<Option<NodeSpatialIndex>>::Derived)?,
        Some(index) => writer.write_bincode(&Stored::Explicit(index))?,
    }

    // Routing layer.
    let routing = &layers.routing;
    let u32s = |f: fn(&RoutingEdge) -> u32| routing.iter().map(f).collect::<Vec<_>>();
    let i64s = |f: fn(&RoutingEdge) -> i64| routing.iter().map(f).collect::<Vec<_>>();
    writer.write_u32s(&u32s(|edge| edge.edge_id.0))?;
    writer.write_u32s(&u32s(|edge| edge.from.0))?;
    writer.write_u32s(&u32s(|edge| edge.to.0))?;
    writer.write_i64s(&i64s(|edge| edge.source_way_id))?;
    writer.write_u32s(&u32s(|edge| edge.length_m))?;
    writer.write_f32s(&routing.iter().map(|edge| edge.ascent_m).collect::<Vec<_>>())?;
    writer.write_f32s(&routing.iter().map(|edge| edge.descent_m).collect::<Vec<_>>())?;
    writer.write_u32s(&u32s(|edge| edge.flags))?;
    writer.write_u32s(&u32s(|edge| edge.feature_row))?;
    writer.write_i64s(&i64s(|edge| i64::from(edge.source_direction)))?;
    writer.write_i64s(&i64s(|edge| option_u32_to_i64(edge.temporal_rule_id)))?;

    // Profile attributes repeat along ways and across classes: intern them.
    writer.write_interned(&layers.profile)?;
    // Presentation layer.
    let presentation = &layers.presentation;
    writer.write_i64s(
        &presentation
            .iter()
            .map(|edge| option_u32_to_i64(edge.name_index))
            .collect::<Vec<_>>(),
    )?;
    writer.write_i64s(
        &presentation
            .iter()
            .map(|edge| edge.geometry_offset as i64)
            .collect::<Vec<_>>(),
    )?;
    writer.write_u32s(
        &presentation
            .iter()
            .map(|edge| edge.geometry_len)
            .collect::<Vec<_>>(),
    )?;

    let ebt = &bundle.edge_based_topology;
    if derived_edge_based_topology(bundle.nodes.len(), routing).as_ref() == Some(ebt) {
        writer.write_bincode(&Stored::<()>::Derived)?;
    } else {
        writer.write_bincode(&Stored::Explicit(()))?;
        writer.write_u32s(&ebt.node_first_out)?;
        writer.write_u32s(&ebt.node_edge_order)?;
        writer.write_u32s(&ebt.edge_transition_first_out)?;
        writer.write_section(&codec::encode_sorted_rows(
            &ebt.edge_transition_first_out,
            &ebt.edge_transition_edges,
        ))?;
    }
    writer.write_u32s(&bundle.node_component_ids)?;
    if derived_edge_component_ids(&bundle.node_component_ids, routing).as_ref()
        == Some(&bundle.edge_component_ids)
    {
        writer.write_bincode(&Stored::<()>::Derived)?;
    } else {
        writer.write_bincode(&Stored::Explicit(()))?;
        writer.write_u32s(&bundle.edge_component_ids)?;
    }
    write_feature_attributes(writer, &bundle.feature_attributes)?;
    writer.write_bincode(&bundle.temporal_rule_sets)
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
    let node_count = usize::try_from(header.node_count).context("node count overflow")?;
    let edge_count = usize::try_from(header.edge_count).context("edge count overflow")?;

    let node_ids = reader.read_u32s()?;
    let lons = reader.read_f64s()?;
    let lats = reader.read_f64s()?;
    let elevations = reader.read_f64s()?;
    for (name, len) in [
        ("node ids", node_ids.len()),
        ("longitudes", lons.len()),
        ("latitudes", lats.len()),
        ("elevations", elevations.len()),
    ] {
        ensure_len(name, len, node_count)?;
    }
    let nodes: Vec<TopologyNode> = (0..node_count)
        .map(|index| TopologyNode {
            node_id: NodeId(node_ids[index]),
            lon: lons[index],
            lat: lats[index],
            z: elevations[index],
        })
        .collect();
    drop((node_ids, lons, lats, elevations));
    let spatial_index = match reader.read_bincode::<Stored<Option<NodeSpatialIndex>>>()? {
        Stored::Derived => Some(
            NodeSpatialIndex::build(&nodes).context("derived spatial index needs nodes")?,
        ),
        Stored::Explicit(index) => index,
    };

    let edge_ids = reader.read_u32s()?;
    let froms = reader.read_u32s()?;
    let tos = reader.read_u32s()?;
    let way_ids = reader.read_i64s()?;
    let lengths = reader.read_u32s()?;
    let ascents = reader.read_f32s()?;
    let descents = reader.read_f32s()?;
    let flags = reader.read_u32s()?;
    let feature_rows = reader.read_u32s()?;
    let source_directions = reader.read_i64s()?;
    let temporal_rule_ids = reader.read_i64s()?;
    for (name, len) in [
        ("edge ids", edge_ids.len()),
        ("edge tails", froms.len()),
        ("edge heads", tos.len()),
        ("way ids", way_ids.len()),
        ("lengths", lengths.len()),
        ("ascents", ascents.len()),
        ("descents", descents.len()),
        ("flags", flags.len()),
        ("feature rows", feature_rows.len()),
        ("source directions", source_directions.len()),
        ("temporal rule ids", temporal_rule_ids.len()),
    ] {
        ensure_len(name, len, edge_count)?;
    }
    let routing = (0..edge_count)
        .map(|index| {
            Ok(RoutingEdge {
                edge_id: EdgeId(edge_ids[index]),
                from: NodeId(froms[index]),
                to: NodeId(tos[index]),
                source_way_id: way_ids[index],
                length_m: lengths[index],
                ascent_m: ascents[index],
                descent_m: descents[index],
                feature_row: feature_rows[index],
                source_direction: i8::try_from(source_directions[index])
                    .context("source direction out of range")?,
                temporal_rule_id: i64_to_option_u32(temporal_rule_ids[index])?,
                flags: flags[index],
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let profile: Vec<EdgeProfileAttributes> = reader.read_interned()?;
    ensure_len("edge profile layer", profile.len(), edge_count)?;
    let name_indexes = reader.read_i64s()?;
    let geometry_offsets = reader.read_i64s()?;
    let geometry_lens = reader.read_u32s()?;
    ensure_len("edge names", name_indexes.len(), edge_count)?;
    ensure_len("geometry offsets", geometry_offsets.len(), edge_count)?;
    ensure_len("geometry lengths", geometry_lens.len(), edge_count)?;
    let presentation = (0..edge_count)
        .map(|index| {
            Ok(EdgePresentation {
                name_index: i64_to_option_u32(name_indexes[index])?,
                geometry_offset: geometry_offsets[index] as u64,
                geometry_len: geometry_lens[index],
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let edge_based_topology = match reader.read_bincode::<Stored<()>>()? {
        Stored::Derived => derived_edge_based_topology(node_count, &routing)
            .context("edge endpoints reference missing nodes")?,
        Stored::Explicit(()) => {
            let node_first_out = reader.read_u32s()?;
            let node_edge_order = reader.read_u32s()?;
            let edge_transition_first_out = reader.read_u32s()?;
            let edge_transition_edges = codec::decode_sorted_rows(
                &edge_transition_first_out,
                reader.next_section()?,
            )?;
            EdgeBasedTopology {
                node_first_out,
                node_edge_order,
                edge_transition_first_out,
                edge_transition_edges,
            }
        }
    };
    let node_component_ids = reader.read_u32s()?;
    let edge_component_ids = match reader.read_bincode::<Stored<()>>()? {
        Stored::Derived => derived_edge_component_ids(&node_component_ids, &routing)
            .context("edge components reference missing nodes")?,
        Stored::Explicit(()) => reader.read_u32s()?,
    };
    let feature_attributes = read_feature_attributes(&mut reader)?;
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
        spatial_index,
        node_component_ids,
        edge_component_ids,
        feature_attributes,
        temporal_rule_sets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topology() -> TopologyBundle {
        let mut bundle = TopologyBundle {
            schema_version: TOPOLOGY_BUNDLE_SCHEMA_VERSION,
            source_path: "city.osm.pbf".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    lon: 1_141_000_000_f64 * 1e-7,
                    lat: 223_000_000_f64 * 1e-7,
                    z: f64::NAN,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    lon: 114.2,
                    lat: 22.4,
                    z: f64::NAN,
                },
            ],
            edge_layers: TopologyEdgeLayers {
                routing: vec![
                    RoutingEdge {
                        from: NodeId(0),
                        to: NodeId(1),
                        source_way_id: 42,
                        length_m: 100,
                        ..RoutingEdge::default()
                    },
                    RoutingEdge {
                        edge_id: EdgeId(1),
                        from: NodeId(1),
                        to: NodeId(0),
                        source_way_id: 42,
                        length_m: 100,
                        temporal_rule_id: Some(3),
                        source_direction: -1,
                        ..RoutingEdge::default()
                    },
                ],
                profile: vec![EdgeProfileAttributes::default(); 2],
                presentation: vec![
                    EdgePresentation {
                        name_index: Some(0),
                        ..EdgePresentation::default()
                    },
                    EdgePresentation::default(),
                ],
            },
            turn_restrictions: Vec::new(),
            names: Vec::new(),
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
            node_component_ids: vec![0, 0],
            edge_component_ids: vec![0, 0],
            feature_attributes: FeatureAttributeTable::default(),
            temporal_rule_sets: Vec::new(),
        };
        bundle.edge_based_topology = EdgeBasedTopology::build(2, 2, |index| {
            let edge = bundle.edge_layers.routing[index];
            (edge.from.0, edge.to.0)
        });
        bundle.spatial_index = NodeSpatialIndex::build(&bundle.nodes);
        bundle
    }

    fn topology_bytes(bundle: &TopologyBundle) -> Vec<u8> {
        let mut writer = SectionWriter::new(Vec::new()).unwrap();
        encode_topology(&mut writer, bundle).unwrap();
        writer.writer
    }

    #[test]
    fn derived_and_explicit_structures_round_trip() {
        let bundle = topology();
        let read = read_topology_sectioned(&topology_bytes(&bundle), Path::new("t.bin"))
            .expect("current topology reads");
        assert_eq!(read.edge_based_topology, bundle.edge_based_topology);
        assert_eq!(read.spatial_index, bundle.spatial_index);
        assert_eq!(read.nodes[0].lon.to_bits(), bundle.nodes[0].lon.to_bits());
        assert_eq!(read.nodes[1].lat.to_bits(), bundle.nodes[1].lat.to_bits());
        assert!(read.nodes[1].z.is_nan());
        assert_eq!(read.routing_edge(1).temporal_rule_id, Some(3));
        assert_eq!(read.routing_edge(1).source_direction, -1);
        assert_eq!(read.edge_presentation(0).name_index, Some(0));

        // Structures that do not match their rebuild are stored verbatim.
        let mut edited = bundle.clone();
        edited.edge_based_topology.edge_transition_edges.reverse();
        edited.edge_component_ids = vec![5, 6];
        if let Some(index) = edited.spatial_index.as_mut() {
            index.columns += 1;
        }
        let read = read_topology_sectioned(&topology_bytes(&edited), Path::new("t.bin"))
            .expect("explicit structures read");
        assert_eq!(read.edge_based_topology, edited.edge_based_topology);
        assert_eq!(read.edge_component_ids, vec![5, 6]);
        assert_eq!(read.spatial_index, edited.spatial_index);
    }

    #[test]
    fn rejects_an_unrecognized_magic_or_schema() {
        let mut bytes = topology_bytes(&topology());
        bytes[..8].copy_from_slice(b"INVALID!");
        let error = read_topology_sectioned(&bytes, Path::new("topology.bin"))
            .expect_err("an unrecognized magic is rejected");
        assert!(error.to_string().contains("re-import the dataset"), "{error}");

        let mut stale = topology();
        stale.schema_version -= 1;
        let error = read_topology_sectioned(&topology_bytes(&stale), Path::new("topology.bin"))
            .expect_err("a mismatched schema version is rejected");
        assert!(error.to_string().contains("re-import the dataset"), "{error}");
    }
}
