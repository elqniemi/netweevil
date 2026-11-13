//! Multi-pass, parallel OSM PBF reading.
//!
//! [`PbfFile::open`] walks the blob headers once to index every data blob
//! by file offset. Each pass then reads the blobs that can hold the
//! requested object kind in file order, decodes and filters them on all
//! cores, and hands the per-blob results to a sequential merge in file
//! order, so a pass sees objects in exactly the order a single-threaded
//! reader yields them. The first pass records which kinds every blob holds,
//! so later passes skip blobs without the kind they read.

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
const BATCH_BYTES_PER_THREAD: u64 = 8 << 20;
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
            reader
                .seek_relative(body_len as i64)
                .with_context(|| format!("skipping blob at byte {offset}"))?;
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
        Ok(Self {
            path: path.to_path_buf(),
            file: reader.into_inner(),
            blobs,
            fingerprint: PbfFingerprint {
                size,
                blob_layout_hash: layout.finish(),
            },
        })
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

/// The `type` and `datasize` fields of a `BlobHeader` message.
fn parse_blob_header(bytes: &[u8]) -> Option<(String, u64)> {
    let mut cursor = bytes;
    let mut kind = None;
    let mut body_len = None;
    while !cursor.is_empty() {
        let key = read_varint(&mut cursor)?;
        match (key >> 3, key & 7) {
            (field, 0) => {
                let value = read_varint(&mut cursor)?;
                if field == 3 {
                    body_len = Some(value);
                }
            }
            (field, 2) => {
                let len = usize::try_from(read_varint(&mut cursor)?).ok()?;
                if len > cursor.len() {
                    return None;
                }
                let (value, rest) = cursor.split_at(len);
                if field == 1 {
                    kind = Some(String::from_utf8(value.to_vec()).ok()?);
                }
                cursor = rest;
            }
            (_, 1) => cursor = cursor.get(8..)?,
            (_, 5) => cursor = cursor.get(4..)?,
            _ => return None,
        }
    }
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

/// A sorted set of OSM ids with an index for O(1) bucket lookup: ids are
/// bucketed by their high bits, so a lookup binary-searches only the ids
/// that share a bucket. Uses 8 bytes per id plus a fixed 16 MiB table.
#[derive(Debug, Clone, Default)]
pub(crate) struct IdIndex {
    ids: Vec<i64>,
    /// `buckets[b]..buckets[b + 1]` are the positions of ids in bucket `b`.
    buckets: Vec<u64>,
}

/// Ids below this are bucketed; OSM ids stay far below it. Others fall
/// back to a binary search over all ids.
const BUCKETED_ID_LIMIT: i64 = 1 << 37;
const BUCKET_SHIFT: u32 = 16;

impl IdIndex {
    /// Indexes `ids`, sorting them and dropping repeats.
    pub(crate) fn new(mut ids: Vec<i64>) -> Self {
        ids.par_sort_unstable();
        ids.dedup();
        ids.shrink_to_fit();
        let bucket_count = (BUCKETED_ID_LIMIT >> BUCKET_SHIFT) as usize;
        let mut buckets = Vec::with_capacity(bucket_count + 1);
        let mut position = ids.partition_point(|id| *id < 0);
        for bucket in 0..=bucket_count as i64 {
            let start = bucket << BUCKET_SHIFT;
            while position < ids.len() && ids[position] < start {
                position += 1;
            }
            buckets.push(position as u64);
        }
        Self { ids, buckets }
    }

    pub(crate) fn len(&self) -> usize {
        self.ids.len()
    }

    pub(crate) fn ids(&self) -> &[i64] {
        &self.ids
    }

    /// Position of `id` among the sorted ids.
    pub(crate) fn position(&self, id: i64) -> Option<usize> {
        if (0..BUCKETED_ID_LIMIT).contains(&id) {
            let bucket = (id >> BUCKET_SHIFT) as usize;
            let start = self.buckets[bucket] as usize;
            let end = self.buckets[bucket + 1] as usize;
            return self.ids[start..end]
                .binary_search(&id)
                .ok()
                .map(|offset| start + offset);
        }
        self.ids.binary_search(&id).ok()
    }

    pub(crate) fn contains(&self, id: i64) -> bool {
        self.position(id).is_some()
    }
}

#[cfg(test)]
mod tests;
