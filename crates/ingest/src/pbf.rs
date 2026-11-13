//! Multi-pass, parallel OSM PBF reading.
//!
//! [`PbfFile::open`] walks the blob headers once to index every data blob
//! by file offset. Each pass then reads the blobs that can hold the
//! requested object kind in file order, decodes and filters them on all
//! cores, and hands the per-blob results to a sequential merge in file
//! order, so a pass sees objects in exactly the order a single-threaded
//! reader yields them. Files whose header declares `Sort.Type_then_ID`
//! (nodes, then ways, then relations) have the blob ranges of each kind
//! located by binary search when opened; otherwise the first pass records
//! which kinds every blob holds. Passes skip blobs without the kind they
//! read.

use std::fs::File;
use std::hash::Hasher;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use osmpbfreader::osmformat::PrimitiveBlock;
use osmpbfreader::{OsmObj, OsmPbfReader, blocks, primitive_block_from_blob};
use rayon::prelude::*;
use rustc_hash::FxHasher;
use serde::{Deserialize, Serialize};

/// The OSM object kinds a pass reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectKind {
    Node,
    Way,
    Relation,
}

impl ObjectKind {
    const fn bit(self) -> u8 {
        match self {
            Self::Node => 1,
            Self::Way => 2,
            Self::Relation => 4,
        }
    }

    #[cfg(test)]
    pub(crate) fn of(object: &OsmObj) -> Self {
        match object {
            OsmObj::Node(_) => Self::Node,
            OsmObj::Way(_) => Self::Way,
            OsmObj::Relation(_) => Self::Relation,
        }
    }
}

/// Every kind, for blobs not decoded yet.
const ALL_KINDS: u8 = 7;

/// A source of OSM objects that can be read one kind at a time, any number
/// of times.
pub(crate) trait OsmSource {
    /// Calls `extract` on every block of objects of `kind` (in parallel)
    /// and `merge` on the results in source order. `progress` receives the
    /// fraction of the pass done.
    fn read_pass<T, E, M>(
        &mut self,
        kind: ObjectKind,
        extract: E,
        merge: M,
        progress: &mut dyn FnMut(f64),
    ) -> Result<()>
    where
        T: Send,
        E: Fn(&mut dyn Iterator<Item = OsmObj>) -> T + Sync,
        M: FnMut(T);
}

/// Size and blob layout of a PBF file, to tell whether a file is the one a
/// dataset was imported from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PbfFingerprint {
    pub(crate) size: u64,
    /// Hash of the offset, type and size of every blob.
    pub(crate) blob_layout_hash: u64,
}

#[derive(Debug, Clone, Copy)]
struct BlobRef {
    /// Offset of the blob's length prefix.
    offset: u64,
    /// Length prefix, header and blob body.
    len: u64,
    kinds: u8,
}

/// An indexed OSM PBF file.
pub(crate) struct PbfFile {
    path: PathBuf,
    file: File,
    blobs: Vec<BlobRef>,
    fingerprint: PbfFingerprint,
}

/// Raw bytes decoded per batch and thread; bounds the memory of a pass.
const BATCH_BYTES_PER_THREAD: u64 = 2 << 20;
/// Largest blob header and blob body the PBF format allows.
const MAX_HEADER_BYTES: u64 = 64 << 10;
const MAX_BLOB_BYTES: u64 = 32 << 20;

impl PbfFile {
    /// Opens `path` and indexes its data blobs.
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("opening OSM PBF {}", path.display()))?;
        let size = file
            .metadata()
            .with_context(|| format!("reading metadata of {}", path.display()))?
            .len();
        let mut reader = BufReader::new(file);
        let mut blobs = Vec::new();
        let mut layout = FxHasher::default();
        let mut offset = 0_u64;
        let mut header = Vec::new();
        let mut sorted_by_type = false;
        while offset < size {
            let mut prefix = [0_u8; 4];
            reader
                .read_exact(&mut prefix)
                .with_context(|| format!("reading blob header length at byte {offset}"))?;
            let header_len = u64::from(u32::from_be_bytes(prefix));
            if header_len > MAX_HEADER_BYTES {
                bail!(
                    "{}: blob header at byte {offset} is too large",
                    path.display()
                );
            }
            header.resize(header_len as usize, 0);
            reader
                .read_exact(&mut header)
                .with_context(|| format!("reading blob header at byte {offset}"))?;
            let (kind, body_len) = parse_blob_header(&header).with_context(|| {
                format!("{}: malformed blob header at byte {offset}", path.display())
            })?;
            if body_len > MAX_BLOB_BYTES {
                bail!("{}: blob at byte {offset} is too large", path.display());
            }
            let len = 4 + header_len + body_len;
            if offset + len > size {
                bail!("{}: truncated blob at byte {offset}", path.display());
            }
            if kind == "OSMHeader" {
                let mut body = vec![0_u8; body_len as usize];
                reader
                    .read_exact(&mut body)
                    .with_context(|| format!("reading the PBF header at byte {offset}"))?;
                sorted_by_type |= header_features(&body)
                    .with_context(|| format!("{}: malformed PBF header", path.display()))?
                    .iter()
                    .any(|feature| feature == "Sort.Type_then_ID");
            } else {
                reader
                    .seek_relative(body_len as i64)
                    .with_context(|| format!("skipping blob at byte {offset}"))?;
            }
            layout.write_u64(offset);
            layout.write(kind.as_bytes());
            layout.write_u64(body_len);
            if kind == "OSMData" {
                blobs.push(BlobRef {
                    offset,
                    len,
                    kinds: ALL_KINDS,
                });
            }
            offset += len;
        }
        let mut pbf = Self {
            path: path.to_path_buf(),
            file: reader.into_inner(),
            blobs,
            fingerprint: PbfFingerprint {
                size,
                blob_layout_hash: layout.finish(),
            },
        };
        if sorted_by_type {
            pbf.locate_kinds()?;
        }
        Ok(pbf)
    }

    /// Kinds of objects blob `index` holds.
    fn decode_kinds(&mut self, index: usize) -> Result<u8> {
        let bytes = self.read_blobs(&[index])?;
        let block = decode_blob(&bytes[0]).with_context(|| {
            format!(
                "decoding {} blob at byte {}",
                self.path.display(),
                self.blobs[index].offset
            )
        })?;
        Ok(block_kinds(&block))
    }

    /// Sets every blob's kinds in a file sorted by type, where the kinds
    /// change monotonically: nodes, then ways, then relations, with mixed
    /// blobs only at the transitions. Each boundary is found by binary
    /// search. Leaves the kinds unknown when a probed blob is empty.
    fn locate_kinds(&mut self) -> Result<()> {
        let mut probed = rustc_hash::FxHashMap::<usize, u8>::default();
        let mut kinds_of = |pbf: &mut Self, index: usize| -> Result<u8> {
            if let Some(&kinds) = probed.get(&index) {
                return Ok(kinds);
            }
            let kinds = pbf.decode_kinds(index)?;
            probed.insert(index, kinds);
            Ok(kinds)
        };
        // First blob for which `predicate(kinds)` holds; it holds for every
        // later blob.
        let mut first = |pbf: &mut Self, predicate: &dyn Fn(u8) -> bool| -> Result<Option<usize>> {
            let (mut low, mut high) = (0, pbf.blobs.len());
            while low < high {
                let middle = low + (high - low) / 2;
                let kinds = kinds_of(pbf, middle)?;
                if kinds == 0 {
                    return Ok(None);
                }
                if predicate(kinds) {
                    high = middle;
                } else {
                    low = middle + 1;
                }
            }
            Ok(Some(low))
        };
        let highest = |kinds: u8| 7 - kinds.leading_zeros() as u8;
        let lowest = |kinds: u8| kinds.trailing_zeros() as u8;
        let (way, relation) = (
            highest(ObjectKind::Way.bit()),
            highest(ObjectKind::Relation.bit()),
        );
        let (
            Some(first_with_ways),
            Some(first_with_relations),
            Some(first_without_nodes),
            Some(first_relations_only),
        ) = (
            first(self, &|kinds| highest(kinds) >= way)?,
            first(self, &|kinds| highest(kinds) >= relation)?,
            first(self, &|kinds| lowest(kinds) >= way)?,
            first(self, &|kinds| lowest(kinds) >= relation)?,
        )
        else {
            return Ok(());
        };
        for (index, blob) in self.blobs.iter_mut().enumerate() {
            blob.kinds = if index < first_without_nodes {
                ObjectKind::Node.bit()
            } else {
                0
            } | if (first_with_ways..first_relations_only).contains(&index) {
                ObjectKind::Way.bit()
            } else {
                0
            } | if index >= first_with_relations {
                ObjectKind::Relation.bit()
            } else {
                0
            };
        }
        Ok(())
    }

    pub(crate) fn fingerprint(&self) -> &PbfFingerprint {
        &self.fingerprint
    }

    /// Reads the blobs `selected` indexes, which are in file order, into
    /// one buffer per blob.
    fn read_blobs(&mut self, selected: &[usize]) -> Result<Vec<Vec<u8>>> {
        let mut position = None;
        selected
            .iter()
            .map(|&index| {
                let blob = self.blobs[index];
                if position != Some(blob.offset) {
                    self.file.seek(SeekFrom::Start(blob.offset))?;
                }
                let mut bytes = vec![0_u8; blob.len as usize];
                self.file.read_exact(&mut bytes)?;
                position = Some(blob.offset + blob.len);
                Ok(bytes)
            })
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("reading {}", self.path.display()))
    }
}

impl OsmSource for PbfFile {
    fn read_pass<T, E, M>(
        &mut self,
        kind: ObjectKind,
        extract: E,
        mut merge: M,
        progress: &mut dyn FnMut(f64),
    ) -> Result<()>
    where
        T: Send,
        E: Fn(&mut dyn Iterator<Item = OsmObj>) -> T + Sync,
        M: FnMut(T),
    {
        let selected = (0..self.blobs.len())
            .filter(|&index| self.blobs[index].kinds & kind.bit() != 0)
            .collect::<Vec<_>>();
        let total_bytes = selected
            .iter()
            .map(|&index| self.blobs[index].len)
            .sum::<u64>()
            .max(1);
        let batch_bytes = BATCH_BYTES_PER_THREAD * rayon::current_num_threads() as u64;
        let mut done_bytes = 0_u64;
        let mut start = 0;
        while start < selected.len() {
            let mut end = start;
            let mut bytes = 0;
            while end < selected.len() && (end == start || bytes < batch_bytes) {
                bytes += self.blobs[selected[end]].len;
                end += 1;
            }
            let batch = &selected[start..end];
            let raw = self.read_blobs(batch)?;
            let decoded = raw
                .par_iter()
                .zip(batch.par_iter())
                .map(|(bytes, &index)| -> Result<(u8, T)> {
                    let block = decode_blob(bytes).with_context(|| {
                        format!(
                            "decoding {} blob at byte {}",
                            self.path.display(),
                            self.blobs[index].offset
                        )
                    })?;
                    let kinds = block_kinds(&block);
                    let result = match kind {
                        ObjectKind::Node => extract(&mut blocks::nodes(&block).map(OsmObj::Node)),
                        ObjectKind::Way => extract(&mut blocks::ways(&block).map(OsmObj::Way)),
                        ObjectKind::Relation => {
                            extract(&mut blocks::relations(&block).map(OsmObj::Relation))
                        }
                    };
                    Ok((kinds, result))
                })
                .collect::<Vec<_>>();
            drop(raw);
            for (result, &index) in decoded.into_iter().zip(batch) {
                let (kinds, value) = result?;
                self.blobs[index].kinds = kinds;
                merge(value);
            }
            done_bytes += bytes;
            progress(done_bytes as f64 / total_bytes as f64);
            start = end;
        }
        Ok(())
    }
}

/// Objects held in memory, read in blocks of a few objects so tests cover
/// the per-block merge.
#[cfg(test)]
pub(crate) struct ObjectList(pub(crate) Vec<OsmObj>);

#[cfg(test)]
impl OsmSource for ObjectList {
    fn read_pass<T, E, M>(
        &mut self,
        kind: ObjectKind,
        extract: E,
        mut merge: M,
        progress: &mut dyn FnMut(f64),
    ) -> Result<()>
    where
        T: Send,
        E: Fn(&mut dyn Iterator<Item = OsmObj>) -> T + Sync,
        M: FnMut(T),
    {
        let objects = self
            .0
            .iter()
            .filter(|object| ObjectKind::of(object) == kind)
            .cloned()
            .collect::<Vec<_>>();
        let results = objects
            .par_chunks(3)
            .map(|chunk| extract(&mut chunk.iter().cloned()))
            .collect::<Vec<_>>();
        results.into_iter().for_each(&mut merge);
        progress(1.0);
        Ok(())
    }
}

fn decode_blob(framed: &[u8]) -> Result<PrimitiveBlock> {
    let blob = OsmPbfReader::new(Cursor::new(framed))
        .blobs()
        .next()
        .context("missing blob")??;
    Ok(primitive_block_from_blob(&blob)?)
}

fn block_kinds(block: &PrimitiveBlock) -> u8 {
    block.primitivegroup.iter().fold(0, |kinds, group| {
        let nodes = !group.nodes.is_empty()
            || group
                .dense
                .as_ref()
                .is_some_and(|dense| !dense.id.is_empty());
        kinds
            | if nodes { ObjectKind::Node.bit() } else { 0 }
            | if group.ways.is_empty() {
                0
            } else {
                ObjectKind::Way.bit()
            }
            | if group.relations.is_empty() {
                0
            } else {
                ObjectKind::Relation.bit()
            }
    })
}

/// The required and optional features of an `OSMHeader` blob.
fn header_features(blob: &[u8]) -> Option<Vec<String>> {
    let mut block = None;
    for_each_field(blob, |field, value| match (field, value) {
        (1, FieldValue::Bytes(raw)) => {
            block = Some(raw.to_vec());
            Some(())
        }
        (3, FieldValue::Bytes(zlib)) => {
            let mut inflated = Vec::new();
            flate2::read::ZlibDecoder::new(zlib)
                .read_to_end(&mut inflated)
                .ok()?;
            block = Some(inflated);
            Some(())
        }
        _ => Some(()),
    })?;
    let mut features = Vec::new();
    for_each_field(&block?, |field, value| {
        if let (4 | 5, FieldValue::Bytes(feature)) = (field, value) {
            features.push(String::from_utf8(feature.to_vec()).ok()?);
        }
        Some(())
    })?;
    Some(features)
}

enum FieldValue<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

/// Calls `visit` on every field of a protobuf message; `None` when the
/// message is malformed or `visit` returns `None`.
fn for_each_field<'a>(
    mut cursor: &'a [u8],
    mut visit: impl FnMut(u64, FieldValue<'a>) -> Option<()>,
) -> Option<()> {
    while !cursor.is_empty() {
        let key = read_varint(&mut cursor)?;
        match key & 7 {
            0 => visit(key >> 3, FieldValue::Varint(read_varint(&mut cursor)?))?,
            2 => {
                let len = usize::try_from(read_varint(&mut cursor)?).ok()?;
                if len > cursor.len() {
                    return None;
                }
                let (value, rest) = cursor.split_at(len);
                cursor = rest;
                visit(key >> 3, FieldValue::Bytes(value))?;
            }
            1 => cursor = cursor.get(8..)?,
            5 => cursor = cursor.get(4..)?,
            _ => return None,
        }
    }
    Some(())
}

/// The `type` and `datasize` fields of a `BlobHeader` message.
fn parse_blob_header(bytes: &[u8]) -> Option<(String, u64)> {
    let mut kind = None;
    let mut body_len = None;
    for_each_field(bytes, |field, value| {
        match (field, value) {
            (1, FieldValue::Bytes(value)) => kind = Some(String::from_utf8(value.to_vec()).ok()?),
            (3, FieldValue::Varint(value)) => body_len = Some(value),
            _ => {}
        }
        Some(())
    })?;
    Some((kind?, body_len?))
}

fn read_varint(cursor: &mut &[u8]) -> Option<u64> {
    let mut value = 0_u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = cursor.split_first()?;
        *cursor = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// A sorted set of OSM ids with a bucket table over their high bits, so a
/// lookup binary-searches only the few ids that share a bucket. Uses
/// 8 bytes per id plus about half a byte per id for the table.
#[derive(Debug, Clone, Default)]
pub(crate) struct IdIndex {
    ids: Vec<i64>,
    /// Ids before this position are negative and not bucketed.
    first_bucketed: usize,
    shift: u32,
    /// `buckets[b]..buckets[b + 1]` are the positions of the ids `x` with
    /// `x >> shift == b`.
    buckets: Vec<u64>,
}

/// Ids per bucket the table aims for.
const IDS_PER_BUCKET: usize = 16;

impl IdIndex {
    /// Indexes `ids`, sorting them and dropping repeats.
    pub(crate) fn new(mut ids: Vec<i64>) -> Self {
        ids.par_sort_unstable();
        ids.dedup();
        Self::from_sorted(ids)
    }

    /// Indexes ids that are sorted and unique.
    pub(crate) fn from_sorted(mut ids: Vec<i64>) -> Self {
        debug_assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        ids.shrink_to_fit();
        let first_bucketed = ids.partition_point(|id| *id < 0);
        let Some(&max) = ids.last().filter(|max| **max >= 0) else {
            return Self {
                ids,
                first_bucketed,
                shift: 0,
                buckets: Vec::new(),
            };
        };
        let target_buckets = ((ids.len() - first_bucketed) / IDS_PER_BUCKET)
            .max(1)
            .next_power_of_two();
        let shift = (64 - max.leading_zeros()).saturating_sub(target_buckets.trailing_zeros());
        let bucket_count = (max >> shift) as usize + 1;
        let mut buckets = Vec::with_capacity(bucket_count + 1);
        let mut position = first_bucketed;
        for bucket in 0..=bucket_count as i128 {
            let start = bucket << shift;
            while position < ids.len() && i128::from(ids[position]) < start {
                position += 1;
            }
            buckets.push(position as u64);
        }
        Self {
            ids,
            first_bucketed,
            shift,
            buckets,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.ids.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub(crate) fn ids(&self) -> &[i64] {
        &self.ids
    }

    /// Position of `id` among the sorted ids.
    pub(crate) fn position(&self, id: i64) -> Option<usize> {
        if id < 0 {
            return self.ids[..self.first_bucketed].binary_search(&id).ok();
        }
        let bucket = (id >> self.shift) as usize;
        if bucket + 1 >= self.buckets.len() {
            return None;
        }
        let start = self.buckets[bucket] as usize;
        let end = self.buckets[bucket + 1] as usize;
        self.ids[start..end]
            .binary_search(&id)
            .ok()
            .map(|offset| start + offset)
    }

    pub(crate) fn contains(&self, id: i64) -> bool {
        self.position(id).is_some()
    }
}

#[cfg(test)]
mod tests;
