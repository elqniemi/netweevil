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

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use memmap2::Mmap;
use netweevil_core::{
    ACCELERATION_BUNDLE_SCHEMA_VERSION, COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION, CacheBundleId,
    CompiledAcceleration, CompiledCostComponent, CompiledEdgeMetric, CompiledProfileBundle,
    CompiledTemporalProfile, CompiledTurnCostConfig, DatasetAccelerationBundle,
    EDGE_NAME_BUNDLE_SCHEMA_VERSION, EdgeBasedTopology, EdgeGuidance, EdgeId, EdgeNameBundle,
    EdgePresentation, EdgeProfileAttributes, FeatureAttributeColumn, FeatureAttributeColumnData,
    FeatureAttributeDefinition, FeatureAttributeTable, NodeId, NodeSpatialIndex, RoutingEdge,
    TOPOLOGY_BUNDLE_SCHEMA_VERSION, TopologyBundle, TopologyEdgeLayers, TopologyNode, TravelMode,
};
use rayon::prelude::*;
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

/// Section length flag marking a deflate-compressed payload: a `u64`
/// little-endian inflated length followed by a raw deflate stream.
const DEFLATED: u64 = 1 << 63;
/// Sections smaller than this are never compressed.
const DEFLATE_MIN_BYTES: usize = 64 * 1024;
/// Deflated sections are split into independently compressed chunks of
/// this many inflated bytes, so one large section inflates on all cores.
/// Deflate's 32 KiB window makes the ratio loss negligible.
const DEFLATE_CHUNK_BYTES: usize = 1 << 20;

/// Compressed section layout: `u64` inflated length, `u32` chunk count,
/// one `u32` compressed length per chunk, then the raw deflate chunks.
fn deflate_chunked(section: &[u8]) -> std::io::Result<Vec<u8>> {
    let chunks = section
        .par_chunks(DEFLATE_CHUNK_BYTES)
        .map(|chunk| {
            let mut encoder = flate2::write::DeflateEncoder::new(
                Vec::with_capacity(chunk.len() / 2 + 16),
                flate2::Compression::default(),
            );
            encoder.write_all(chunk)?;
            encoder.finish()
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut out =
        Vec::with_capacity(12 + 4 * chunks.len() + chunks.iter().map(Vec::len).sum::<usize>());
    out.extend_from_slice(&(section.len() as u64).to_le_bytes());
    out.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
    for chunk in &chunks {
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
    }
    for chunk in &chunks {
        out.extend_from_slice(chunk);
    }
    Ok(out)
}

fn inflate_chunked(payload: &[u8]) -> Result<Vec<u8>> {
    let mut cursor = payload;
    let mut take = |len: usize| -> Result<&[u8]> {
        if cursor.len() < len {
            bail!("truncated compressed bundle section");
        }
        let (head, tail) = cursor.split_at(len);
        cursor = tail;
        Ok(head)
    };
    let length = usize::try_from(u64::from_le_bytes(take(8)?.try_into().unwrap()))
        .context("compressed section too large")?;
    let chunk_count = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
    // Deflate expands at most ~1032:1, which also bounds the allocation
    // below for corrupt lengths.
    if chunk_count != length.div_ceil(DEFLATE_CHUNK_BYTES) || length / 1032 > payload.len() {
        bail!("compressed bundle section has an inconsistent chunk table");
    }
    let lengths = (0..chunk_count)
        .map(|_| Ok(u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize))
        .collect::<Result<Vec<_>>>()?;
    let chunks = lengths
        .into_iter()
        .map(&mut take)
        .collect::<Result<Vec<_>>>()?;
    let mut inflated = vec![0_u8; length];
    inflated
        .par_chunks_mut(DEFLATE_CHUNK_BYTES)
        .zip(chunks)
        .try_for_each(|(output, chunk)| -> Result<()> {
            let mut decoder = flate2::read::DeflateDecoder::new(chunk);
            decoder
                .read_exact(output)
                .context("inflating bundle section")?;
            if decoder.read(&mut [0_u8])? != 0 {
                bail!("compressed bundle chunk is longer than expected");
            }
            Ok(())
        })?;
    Ok(inflated)
}

/// Collects encoded sections and writes them, deflating large sections in
/// parallel when that saves at least a tenth of their size.
struct SectionWriter {
    sections: Vec<Vec<u8>>,
}

impl SectionWriter {
    fn new() -> Self {
        Self {
            sections: Vec::new(),
        }
    }

    fn write_section(&mut self, bytes: Vec<u8>) -> Result<()> {
        self.sections.push(bytes);
        Ok(())
    }

    fn write_bincode<T: Serialize>(&mut self, value: &T) -> Result<()> {
        let bytes = bincode::serialize(value).context("serializing bundle header section")?;
        self.write_section(bytes)
    }

    fn write_u32s(&mut self, values: &[u32]) -> Result<()> {
        self.write_section(codec::encode_u32s(values))
    }

    fn write_i64s(&mut self, values: &[i64]) -> Result<()> {
        self.write_section(codec::encode_i64s(values))
    }

    fn write_f64s(&mut self, values: &[f64]) -> Result<()> {
        self.write_section(codec::encode_f64s(values))
    }

    fn write_f32s(&mut self, values: &[f32]) -> Result<()> {
        self.write_section(codec::encode_f32s(values))
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
        self.write_section(table)?;
        self.write_u32s(&indexes)
    }

    fn finish_into(self, writer: &mut impl Write) -> Result<()> {
        let framed: Vec<(u64, Vec<u8>)> = self
            .sections
            .into_par_iter()
            .map(|section| {
                if section.len() >= DEFLATE_MIN_BYTES {
                    let compressed = deflate_chunked(&section)?;
                    if compressed.len() * 10 <= section.len() * 9 {
                        return Ok((compressed.len() as u64 | DEFLATED, compressed));
                    }
                }
                Ok((section.len() as u64, section))
            })
            .collect::<std::io::Result<_>>()
            .context("compressing bundle sections")?;
        writer
            .write_all(CURRENT_MAGIC)
            .context("writing bundle magic")?;
        for (header, payload) in framed {
            writer
                .write_all(&header.to_le_bytes())
                .context("writing section length")?;
            writer.write_all(&payload).context("writing section")?;
        }
        writer.flush().context("flushing bundle")
    }

    fn finish(self, path: &Path) -> Result<()> {
        crate::write_atomically(path, |writer| self.finish_into(writer))
    }
}

/// Reads sections in order. Deflated sections are all inflated up front,
/// in parallel, and each buffer is released once its section is consumed.
struct SectionReader<'a> {
    sections: std::vec::IntoIter<Cow<'a, [u8]>>,
}

impl<'a> SectionReader<'a> {
    fn new(bytes: &'a [u8], path: &Path) -> Result<Self> {
        if bytes.len() < MAGIC_LEN || &bytes[..MAGIC_LEN] != CURRENT_MAGIC {
            return Err(unsupported_format_error(path));
        }
        let mut framed = Vec::new();
        let mut offset = MAGIC_LEN;
        while offset < bytes.len() {
            let header_end = offset + 8;
            if header_end > bytes.len() {
                bail!("truncated bundle: missing section length");
            }
            let header = u64::from_le_bytes(bytes[offset..header_end].try_into().unwrap());
            let end = usize::try_from(header & !DEFLATED)
                .ok()
                .and_then(|len| header_end.checked_add(len))
                .filter(|end| *end <= bytes.len());
            let Some(end) = end else {
                bail!("truncated bundle: section extends past end of file");
            };
            framed.push((header & DEFLATED != 0, &bytes[header_end..end]));
            offset = end;
        }
        let sections = framed
            .into_par_iter()
            .map(|(deflated, payload)| {
                if !deflated {
                    return Ok(Cow::Borrowed(payload));
                }
                let inflated = inflate_chunked(payload)?;
                Ok(Cow::Owned(inflated))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            sections: sections.into_iter(),
        })
    }

    /// Moves the next `count` sections into their own reader, so they can
    /// be decoded on another thread.
    fn take(&mut self, count: usize) -> Result<SectionReader<'a>> {
        let sections = (0..count)
            .map(|_| self.next_section())
            .collect::<Result<Vec<_>>>()?;
        Ok(SectionReader {
            sections: sections.into_iter(),
        })
    }

    fn next_section(&mut self) -> Result<Cow<'a, [u8]>> {
        self.sections
            .next()
            .context("truncated bundle: missing section")
    }

    fn read_bincode<T: DeserializeOwned>(&mut self) -> Result<T> {
        bincode::deserialize(&self.next_section()?).context("parsing bundle header section")
    }

    fn read_u32s(&mut self) -> Result<Vec<u32>> {
        codec::decode_u32s(&self.next_section()?)
    }

    fn read_i64s(&mut self) -> Result<Vec<i64>> {
        codec::decode_i64s(&self.next_section()?)
    }

    fn read_f64s(&mut self) -> Result<Vec<f64>> {
        codec::decode_f64s(&self.next_section()?)
    }

    fn read_f32s(&mut self) -> Result<Vec<f32>> {
        codec::decode_f32s(&self.next_section()?)
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

pub(crate) fn has_current_magic(path: &Path) -> bool {
    let mut magic = [0_u8; MAGIC_LEN];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .is_ok_and(|()| &magic == CURRENT_MAGIC)
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
    let mut writer = SectionWriter::new();
    encode_acceleration(&mut writer, bundle)?;
    writer.finish(path)
}

fn encode_acceleration(
    writer: &mut SectionWriter,
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
    writer.write_section(codec::encode_sorted_rows(
        &bundle.upward_first_out,
        &bundle.upward_head,
    ))?;
    writer.write_u32s(&bundle.downward_first_out)?;
    writer.write_section(codec::encode_sorted_rows(
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
    let order_section = reader.next_section()?;
    let rank_section = match reader.read_bincode::<Stored<()>>()? {
        Stored::Derived => None,
        Stored::Explicit(()) => Some(reader.next_section()?),
    };
    let upward = (reader.next_section()?, reader.next_section()?);
    let downward = (reader.next_section()?, reader.next_section()?);
    let csr = |(first_out, heads): (Cow<'_, [u8]>, Cow<'_, [u8]>)| -> Result<_> {
        let first_out = codec::decode_u32s(&first_out)?;
        let heads = codec::decode_sorted_rows(&first_out, &heads)?;
        Ok((first_out, heads))
    };
    let (order, (upward, downward)) = rayon::join(
        || -> Result<_> {
            let edge_order = codec::decode_u32s(&order_section)?;
            let edge_rank = match rank_section {
                None => invert_permutation(&edge_order)
                    .context("CCH edge order is not a permutation")?,
                Some(section) => codec::decode_u32s(&section)?,
            };
            Ok((edge_order, edge_rank))
        },
        || rayon::join(|| csr(upward), || csr(downward)),
    );
    let (edge_order, edge_rank) = order?;
    let (upward_first_out, upward_head) = upward?;
    let (downward_first_out, downward_head) = downward?;
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

// --- Edge-name bundle ----------------------------------------------------
//
// Names and guidance strings are front-coded tables; each guidance record
// field is a column of `string index + 1` (0 for absent), and the per-edge
// record index is a varint column that deflates to almost nothing on the
// many edges without guidance tags.

const GUIDANCE_FIELDS: usize = 8;

fn guidance_fields(record: &EdgeGuidance) -> [Option<u32>; GUIDANCE_FIELDS] {
    [
        record.reference,
        record.turn_lanes,
        record.destination,
        record.destination_ref,
        record.destination_street,
        record.junction_ref,
        record.junction_name,
        record.exit_to,
    ]
}

pub(crate) fn write_edge_names_sectioned(path: &Path, bundle: &EdgeNameBundle) -> Result<()> {
    let mut writer = SectionWriter::new();
    encode_edge_names(&mut writer, bundle)?;
    writer.finish(path)
}

fn encode_edge_names(writer: &mut SectionWriter, bundle: &EdgeNameBundle) -> Result<()> {
    bundle
        .validate(None)
        .map_err(|error| anyhow::anyhow!("invalid edge-name bundle: {error}"))?;
    writer.write_bincode(&"edge_names")?;
    writer.write_bincode(&EDGE_NAME_BUNDLE_SCHEMA_VERSION)?;
    writer.write_section(codec::encode_strings(&bundle.names))?;
    writer.write_section(codec::encode_strings(&bundle.strings))?;
    for field in 0..GUIDANCE_FIELDS {
        let column = bundle
            .guidance_records
            .iter()
            .map(|record| guidance_fields(record)[field].map_or(0, |index| index + 1))
            .collect::<Vec<_>>();
        writer.write_u32s(&column)?;
    }
    writer.write_u32s(&bundle.edge_guidance)
}

pub(crate) fn read_edge_names_sectioned(bytes: &[u8], path: &Path) -> Result<EdgeNameBundle> {
    let mut reader = SectionReader::new(bytes, path)?;
    let tag: String = reader.read_bincode()?;
    if tag != "edge_names" {
        bail!("expected an edge-name bundle, found '{tag}'");
    }
    if reader.read_bincode::<u32>()? != EDGE_NAME_BUNDLE_SCHEMA_VERSION {
        return Err(unsupported_format_error(path));
    }
    let names = codec::decode_strings(&reader.next_section()?)?;
    let strings = codec::decode_strings(&reader.next_section()?)?;
    let columns = (0..GUIDANCE_FIELDS)
        .map(|_| reader.read_u32s())
        .collect::<Result<Vec<_>>>()?;
    let record_count = columns[0].len();
    let field = |column: &[u32], record: usize| -> Result<Option<u32>> {
        let value = *column
            .get(record)
            .context("guidance record columns differ in length")?;
        Ok(value.checked_sub(1))
    };
    let guidance_records = (0..record_count)
        .map(|record| {
            Ok(EdgeGuidance {
                reference: field(&columns[0], record)?,
                turn_lanes: field(&columns[1], record)?,
                destination: field(&columns[2], record)?,
                destination_ref: field(&columns[3], record)?,
                destination_street: field(&columns[4], record)?,
                junction_ref: field(&columns[5], record)?,
                junction_name: field(&columns[6], record)?,
                exit_to: field(&columns[7], record)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if columns.iter().any(|column| column.len() != record_count) {
        bail!("guidance record columns differ in length");
    }
    let bundle = EdgeNameBundle {
        names,
        strings,
        guidance_records,
        edge_guidance: reader.read_u32s()?,
    };
    bundle
        .validate(None)
        .map_err(|error| anyhow::anyhow!("invalid edge-name bundle: {error}"))?;
    Ok(bundle)
}

// --- Compiled profile bundle ----------------------------------------------
//
// Only per-edge metrics are persisted. Customized CCH weights are a pure
// function of these metrics, the topology and the dataset hierarchy, and are
// rebuilt when an engine loads the profile (see
// `netweevil_profile::customize_acceleration`); the reader returns the
// acceleration header with empty weight arrays.

#[derive(Serialize, Deserialize)]
struct CompiledProfileHeader {
    schema_version: u32,
    profile_id: String,
    profile_hash: String,
    mode: TravelMode,
    turn_costs: CompiledTurnCostConfig,
    source_topology_bundle_id: CacheBundleId,
    acceleration: Option<CompiledAccelerationHeader>,
    temporal: CompiledTemporalProfile,
    components: Vec<CompiledCostComponentHeader>,
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
    let mut writer = SectionWriter::new();
    encode_compiled_profile(&mut writer, bundle)?;
    writer.finish(path)
}

fn encode_compiled_profile(
    writer: &mut SectionWriter,
    bundle: &CompiledProfileBundle,
) -> Result<()> {
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
        temporal: bundle.temporal,
        components: bundle
            .components
            .iter()
            .map(|component| CompiledCostComponentHeader {
                name: component.name.clone(),
                weight: component.weight,
                scales_with_travel_time: component.scales_with_travel_time,
                overlay_name: component.overlay_name.clone(),
                invert_overlay: component.invert_overlay,
            })
            .collect(),
    })?;
    writer.write_u32s(
        &bundle
            .edge_metrics
            .iter()
            .map(|metric| metric.edge_id.0)
            .collect::<Vec<_>>(),
    )?;
    // Travel times and costs derive from a few speed classes and integer
    // lengths, so (time, cost) pairs repeat heavily.
    writer.write_interned(
        &bundle
            .edge_metrics
            .iter()
            .map(|metric| (metric.travel_time_s, metric.generalized_cost))
            .collect::<Vec<_>>(),
    )?;
    for component in &bundle.components {
        ensure_len(
            "compiled profile component",
            component.edge_values.len(),
            bundle.edge_metrics.len(),
        )?;
        writer.write_interned(&component.edge_values)?;
    }
    Ok(())
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

    let edge_ids = reader.read_u32s()?;
    let metrics: Vec<(Option<f64>, Option<f64>)> = reader.read_interned()?;
    ensure_len("compiled edge metrics", metrics.len(), edge_ids.len())?;
    let edge_metrics: Vec<CompiledEdgeMetric> = edge_ids
        .into_iter()
        .zip(metrics)
        .map(
            |(edge_id, (travel_time_s, generalized_cost))| CompiledEdgeMetric {
                edge_id: EdgeId(edge_id),
                travel_time_s,
                generalized_cost,
            },
        )
        .collect();

    let mut components = Vec::with_capacity(header.components.len());
    for component in header.components {
        let edge_values: Vec<f32> = reader.read_interned()?;
        ensure_len(
            "compiled profile component",
            edge_values.len(),
            edge_metrics.len(),
        )?;
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
        temporal: header.temporal,
        source_topology_bundle_id: header.source_topology_bundle_id,
        acceleration: header
            .acceleration
            .map(|acceleration| CompiledAcceleration {
                schema_version: acceleration.schema_version,
                source_acceleration_bundle_id: acceleration.source_acceleration_bundle_id,
                algorithm: acceleration.algorithm,
                upward_weight: Vec::new(),
                downward_weight: Vec::new(),
                time_upward_weight: Vec::new(),
                time_downward_weight: Vec::new(),
                distance_upward_weight: Vec::new(),
                distance_downward_weight: Vec::new(),
            }),
        edge_metrics,
    })
}

// --- Source snapshot bundle -------------------------------------------------

#[derive(Serialize, Deserialize)]
struct SourceSnapshotHeader {
    schema_version: u32,
    source_topology_bundle_id: CacheBundleId,
    names: Vec<String>,
}

pub(crate) fn write_source_snapshot_sectioned<W: Serialize, R: Serialize, E: Serialize>(
    path: &Path,
    snapshot: &crate::SourceSnapshot<W, R, E>,
) -> Result<()> {
    let other_nodes = snapshot.other_node_ids.len();
    ensure_len(
        "other node longitudes",
        snapshot.other_node_lon_e7.len(),
        other_nodes,
    )?;
    ensure_len(
        "other node latitudes",
        snapshot.other_node_lat_e7.len(),
        other_nodes,
    )?;
    let ways = snapshot.way_ids.len();
    ensure_len("way node counts", snapshot.way_node_counts.len(), ways)?;
    ensure_len("way names", snapshot.way_name_indexes.len(), ways)?;
    ensure_len("way attributes", snapshot.way_attributes.len(), ways)?;
    let node_refs = snapshot
        .way_node_counts
        .iter()
        .map(|count| *count as usize)
        .sum::<usize>();
    ensure_len("way node ids", snapshot.way_node_ids.len(), node_refs)?;

    let mut writer = SectionWriter::new();
    writer.write_bincode(&"source_snapshot")?;
    writer.write_bincode(&SourceSnapshotHeader {
        schema_version: snapshot.schema_version,
        source_topology_bundle_id: snapshot.source_topology_bundle_id.clone(),
        names: snapshot.names.clone(),
    })?;
    let widen = |values: &[i32]| {
        values
            .iter()
            .map(|value| i64::from(*value))
            .collect::<Vec<_>>()
    };
    writer.write_i64s(&snapshot.topology_node_ids)?;
    writer.write_i64s(&snapshot.other_node_ids)?;
    writer.write_i64s(&widen(&snapshot.other_node_lon_e7))?;
    writer.write_i64s(&widen(&snapshot.other_node_lat_e7))?;
    writer.write_i64s(&snapshot.traffic_signal_node_ids)?;
    writer.write_i64s(&snapshot.way_ids)?;
    writer.write_u32s(&snapshot.way_node_counts)?;
    writer.write_i64s(&snapshot.way_node_ids)?;
    writer.write_i64s(
        &snapshot
            .way_name_indexes
            .iter()
            .map(|index| option_u32_to_i64(*index))
            .collect::<Vec<_>>(),
    )?;
    // Classified way records repeat heavily across a network.
    writer.write_interned(&snapshot.way_attributes)?;
    writer.write_bincode(&snapshot.restrictions)?;
    writer.write_i64s(&snapshot.other_way_ids)?;
    writer.write_i64s(&snapshot.other_relation_ids)?;
    writer.write_bincode(&snapshot.extras)?;
    writer.finish(path)
}

pub(crate) fn read_source_snapshot_sectioned<W, R, E>(
    bytes: &[u8],
    path: &Path,
) -> Result<crate::SourceSnapshot<W, R, E>>
where
    W: DeserializeOwned + Clone,
    R: DeserializeOwned,
    E: DeserializeOwned,
{
    let mut reader = SectionReader::new(bytes, path)?;
    let tag: String = reader.read_bincode()?;
    if tag != "source_snapshot" {
        bail!("expected a source snapshot bundle, found '{tag}'");
    }
    let header: SourceSnapshotHeader = reader.read_bincode()?;
    if header.schema_version != crate::SOURCE_SNAPSHOT_SCHEMA_VERSION {
        return Err(unsupported_format_error(path));
    }
    let narrow = |values: Vec<i64>| -> Result<Vec<i32>> {
        values
            .into_iter()
            .map(|value| i32::try_from(value).context("source coordinate out of range"))
            .collect()
    };
    let topology_node_ids = reader.read_i64s()?;
    let other_node_ids = reader.read_i64s()?;
    let other_node_lon_e7 = narrow(reader.read_i64s()?)?;
    let other_node_lat_e7 = narrow(reader.read_i64s()?)?;
    ensure_len(
        "other node longitudes",
        other_node_lon_e7.len(),
        other_node_ids.len(),
    )?;
    ensure_len(
        "other node latitudes",
        other_node_lat_e7.len(),
        other_node_ids.len(),
    )?;
    let traffic_signal_node_ids = reader.read_i64s()?;
    let way_ids = reader.read_i64s()?;
    let way_node_counts = reader.read_u32s()?;
    let way_node_ids = reader.read_i64s()?;
    let way_name_indexes = reader
        .read_i64s()?
        .into_iter()
        .map(i64_to_option_u32)
        .collect::<Result<Vec<_>>>()?;
    let way_attributes: Vec<W> = reader.read_interned()?;
    for (name, len) in [
        ("way node counts", way_node_counts.len()),
        ("way names", way_name_indexes.len()),
        ("way attributes", way_attributes.len()),
    ] {
        ensure_len(name, len, way_ids.len())?;
    }
    let node_refs = way_node_counts
        .iter()
        .map(|count| *count as usize)
        .sum::<usize>();
    ensure_len("way node ids", way_node_ids.len(), node_refs)?;
    if way_name_indexes
        .iter()
        .flatten()
        .any(|index| *index as usize >= header.names.len())
    {
        bail!("source snapshot way name index out of range");
    }
    let restrictions: Vec<R> = reader.read_bincode()?;
    let other_way_ids = reader.read_i64s()?;
    let other_relation_ids = reader.read_i64s()?;
    let extras: E = reader.read_bincode()?;
    Ok(crate::SourceSnapshot {
        schema_version: header.schema_version,
        source_topology_bundle_id: header.source_topology_bundle_id,
        topology_node_ids,
        other_node_ids,
        other_node_lon_e7,
        other_node_lat_e7,
        traffic_signal_node_ids,
        way_ids,
        way_node_counts,
        way_node_ids,
        way_name_indexes,
        names: header.names,
        way_attributes,
        restrictions,
        other_way_ids,
        other_relation_ids,
        extras,
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
    values
        .iter()
        .map(|value| u32::from(value.is_some()))
        .collect()
}

fn write_feature_attributes(
    writer: &mut SectionWriter,
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
            FeatureAttributeColumnData::String(values)
            | FeatureAttributeColumnData::Json(values) => {
                let coded: Vec<i64> = values
                    .iter()
                    .map(|value| option_u32_to_i64(*value))
                    .collect();
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
    let mut writer = SectionWriter::new();
    encode_topology(&mut writer, bundle)?;
    writer.finish(path)
}

fn encode_topology(writer: &mut SectionWriter, bundle: &TopologyBundle) -> Result<()> {
    let layers = &bundle.edge_layers;
    let edge_count = layers.routing.len();
    ensure_len("edge profile layer", layers.profile.len(), edge_count)?;
    ensure_len(
        "edge presentation layer",
        layers.presentation.len(),
        edge_count,
    )?;
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
    writer.write_u32s(
        &bundle
            .nodes
            .iter()
            .map(|node| node.node_id.0)
            .collect::<Vec<_>>(),
    )?;
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
    writer.write_f32s(
        &routing
            .iter()
            .map(|edge| edge.descent_m)
            .collect::<Vec<_>>(),
    )?;
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
        writer.write_section(codec::encode_sorted_rows(
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

    // Nodes, routing columns and the profile/presentation layers are
    // independent: decode the three groups concurrently.
    let mut node_reader = reader.take(5)?;
    let mut routing_reader = reader.take(11)?;
    let mut layer_reader = reader.take(5)?;
    let (nodes, (routing, layers)) = rayon::join(
        move || -> Result<_> {
            let reader = &mut node_reader;
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

            Ok((nodes, spatial_index))
        },
        move || {
            rayon::join(
                move || -> Result<_> {
                    let reader = &mut routing_reader;
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

                    Ok(routing)
                },
                move || -> Result<_> {
                    let reader = &mut layer_reader;
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

                    Ok((profile, presentation))
                },
            )
        },
    );
    let (nodes, spatial_index) = nodes?;
    let routing = routing?;
    let (profile, presentation) = layers?;

    let explicit_transitions = match reader.read_bincode::<Stored<()>>()? {
        Stored::Derived => None,
        Stored::Explicit(()) => Some(reader.take(4)?),
    };
    let (edge_based_topology, rest) = rayon::join(
        || -> Result<_> {
            Ok(match explicit_transitions {
                None => derived_edge_based_topology(node_count, &routing)
                    .context("edge endpoints reference missing nodes")?,
                Some(mut reader) => {
                    let node_first_out = reader.read_u32s()?;
                    let node_edge_order = reader.read_u32s()?;
                    let edge_transition_first_out = reader.read_u32s()?;
                    let edge_transition_edges = codec::decode_sorted_rows(
                        &edge_transition_first_out,
                        &reader.next_section()?,
                    )?;
                    EdgeBasedTopology {
                        node_first_out,
                        node_edge_order,
                        edge_transition_first_out,
                        edge_transition_edges,
                    }
                }
            })
        },
        || -> Result<_> {
            let node_component_ids = reader.read_u32s()?;
            let edge_component_ids = match reader.read_bincode::<Stored<()>>()? {
                Stored::Derived => derived_edge_component_ids(&node_component_ids, &routing)
                    .context("edge components reference missing nodes")?,
                Stored::Explicit(()) => reader.read_u32s()?,
            };
            let feature_attributes = read_feature_attributes(&mut reader)?;
            let temporal_rule_sets = reader.read_bincode()?;
            Ok((
                node_component_ids,
                edge_component_ids,
                feature_attributes,
                temporal_rule_sets,
            ))
        },
    );
    let edge_based_topology = edge_based_topology?;
    let (node_component_ids, edge_component_ids, feature_attributes, temporal_rule_sets) = rest?;

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
mod tests;
