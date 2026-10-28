//! Minimal OSM PBF writer for import fixtures: uncompressed blobs, plain
//! (non-dense) nodes, granularity 100 so decimicro coordinates round-trip.

use std::io::Write;
use std::path::Path;

use osmpbfreader::{OsmId, OsmObj, Tags};

/// Objects per primitive block, the limit the format recommends.
const BLOCK_OBJECTS: usize = 8_000;

/// Writes `objects` in the given order, one object kind per block.
pub(crate) fn write_pbf(path: &Path, objects: &[OsmObj]) {
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).expect("PBF file creates"));
    let mut header = Vec::new();
    bytes_field(&mut header, 4, b"OsmSchema-V0.6");
    write_blob(&mut file, "OSMHeader", &header);

    let mut start = 0;
    while start < objects.len() {
        let kind = std::mem::discriminant(&objects[start]);
        let mut end = start;
        while end < objects.len()
            && end - start < BLOCK_OBJECTS
            && std::mem::discriminant(&objects[end]) == kind
        {
            end += 1;
        }
        write_blob(&mut file, "OSMData", &primitive_block(&objects[start..end]));
        start = end;
    }
    file.flush().expect("PBF file flushes");
}

fn write_blob(out: &mut impl Write, kind: &str, block: &[u8]) {
    let mut blob = Vec::new();
    bytes_field(&mut blob, 1, block);
    varint_field(&mut blob, 2, block.len() as u64);
    let mut blob_header = Vec::new();
    bytes_field(&mut blob_header, 1, kind.as_bytes());
    varint_field(&mut blob_header, 3, blob.len() as u64);
    out.write_all(&(blob_header.len() as u32).to_be_bytes())
        .expect("PBF writes");
    out.write_all(&blob_header).expect("PBF writes");
    out.write_all(&blob).expect("PBF writes");
}

/// One primitive block holding one group of same-kind objects.
fn primitive_block(objects: &[OsmObj]) -> Vec<u8> {
    let mut strings = StringTable::default();
    let mut group = Vec::new();
    for object in objects {
        let mut message = Vec::new();
        match object {
            OsmObj::Node(node) => {
                varint_field(&mut message, 1, zigzag(node.id.0));
                tag_fields(&mut message, &node.tags, &mut strings);
                varint_field(&mut message, 8, zigzag(i64::from(node.decimicro_lat)));
                varint_field(&mut message, 9, zigzag(i64::from(node.decimicro_lon)));
                bytes_field(&mut group, 1, &message);
            }
            OsmObj::Way(way) => {
                varint_field(&mut message, 1, way.id.0 as u64);
                tag_fields(&mut message, &way.tags, &mut strings);
                let mut previous = 0;
                packed_field(
                    &mut message,
                    8,
                    way.nodes.iter().map(|node| {
                        let delta = node.0 - previous;
                        previous = node.0;
                        zigzag(delta)
                    }),
                );
                bytes_field(&mut group, 3, &message);
            }
            OsmObj::Relation(relation) => {
                varint_field(&mut message, 1, relation.id.0 as u64);
                tag_fields(&mut message, &relation.tags, &mut strings);
                packed_field(
                    &mut message,
                    8,
                    relation
                        .refs
                        .iter()
                        .map(|member| u64::from(strings.index(&member.role))),
                );
                let mut previous = 0;
                packed_field(
                    &mut message,
                    9,
                    relation.refs.iter().map(|member| {
                        let id = member.member.inner_id();
                        let delta = id - previous;
                        previous = id;
                        zigzag(delta)
                    }),
                );
                packed_field(
                    &mut message,
                    10,
                    relation.refs.iter().map(|member| match member.member {
                        OsmId::Node(_) => 0,
                        OsmId::Way(_) => 1,
                        OsmId::Relation(_) => 2,
                    }),
                );
                bytes_field(&mut group, 4, &message);
            }
        }
    }
    let mut table = Vec::new();
    for string in &strings.strings {
        bytes_field(&mut table, 1, string.as_bytes());
    }
    let mut block = Vec::new();
    bytes_field(&mut block, 1, &table);
    bytes_field(&mut block, 2, &group);
    block
}

#[derive(Default)]
struct StringTable {
    strings: Vec<String>,
    lookup: std::collections::HashMap<String, u32>,
}

impl StringTable {
    fn index(&mut self, value: &str) -> u32 {
        if self.strings.is_empty() {
            // Index 0 is reserved for the empty string.
            self.strings.push(String::new());
        }
        if let Some(&index) = self.lookup.get(value) {
            return index;
        }
        let index = self.strings.len() as u32;
        self.strings.push(value.to_string());
        self.lookup.insert(value.to_string(), index);
        index
    }
}

fn tag_fields(message: &mut Vec<u8>, tags: &Tags, strings: &mut StringTable) {
    let pairs = tags
        .iter()
        .map(|(key, value)| (strings.index(key), strings.index(value)))
        .collect::<Vec<_>>();
    packed_field(message, 2, pairs.iter().map(|(key, _)| u64::from(*key)));
    packed_field(message, 3, pairs.iter().map(|(_, value)| u64::from(*value)));
}

fn zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn varint_field(out: &mut Vec<u8>, field: u64, value: u64) {
    varint(out, field << 3);
    varint(out, value);
}

fn bytes_field(out: &mut Vec<u8>, field: u64, bytes: &[u8]) {
    varint(out, (field << 3) | 2);
    varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn packed_field(out: &mut Vec<u8>, field: u64, values: impl Iterator<Item = u64>) {
    let mut payload = Vec::new();
    for value in values {
        varint(&mut payload, value);
    }
    if !payload.is_empty() {
        bytes_field(out, field, &payload);
    }
}
