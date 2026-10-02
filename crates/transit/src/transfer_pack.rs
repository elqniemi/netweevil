//! Packed network transfer tables.
//!
//! A transfer table stores, per directed stop pair, the walking time the
//! router needs plus the full street path (edge ids, 3D geometry, cost
//! components) that responses expose on transfer legs. Stored naively, the
//! paths dominate: every transfer repeats the shared walk out of its origin
//! station as `f64` triples, and every path repeats the component key strings.
//!
//! The packed form keeps the routing columns (target stop, straight-line
//! distance, walk time) as flat arrays and moves everything else into one
//! varint-coded block per origin stop:
//!
//! - Paths from one origin share long prefixes, so each block holds a trie of
//!   `(edge, geometry point)` steps and a transfer stores only its trie leaf
//!   and its first geometry point.
//! - Geometry points are deduplicated into one table-wide `f64` point pool and
//!   referenced by index, so coordinates stay bit-exact.
//! - Component maps reference a table-wide key dictionary; each value is
//!   stored in the shortest lossless form (zero, integer, `f32`, a repeat of
//!   an earlier value in the same path, or raw `f64`).
//!
//! Paths that do not fit the trie layout (geometry length other than edge
//! count + 1) fall back to explicit edge and point lists, so packing is
//! lossless for any input. Responses materialize a path from its block only
//! when a journey actually uses that transfer.

#[cfg(target_endian = "big")]
compile_error!("packed transfer tables store raw little-endian arrays");

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail, ensure};
use bytemuck::Pod;
use serde::{Deserialize, Serialize};

use crate::model::{
    TRANSIT_TRANSFER_TABLE_SCHEMA_VERSION, TransitNetworkTransfer, TransitStreetPath,
    TransitTransferTable,
};

const MAGIC: &[u8; 8] = b"NWTRXF02";
const NO_NODE: u32 = u32::MAX;

const TAG_ZERO: u8 = 0;
const TAG_INTEGER: u8 = 1;
const TAG_F32: u8 = 2;
const TAG_F64: u8 = 3;
const TAG_REPEAT: u8 = 4;
const TAG_NONE: u8 = 5;

/// Directed, profile-specific stop-to-stop network transfer table in packed
/// form. This is the persisted layout and the representation routers keep in
/// memory; [`PackedTransitTransferTable::to_table`] expands it losslessly.
#[derive(Debug, Clone, PartialEq)]
pub struct PackedTransitTransferTable {
    pub schema_version: u32,
    pub feed_id: String,
    pub source_sha256: String,
    pub stop_binding_sha256: Option<String>,
    pub dataset_id: String,
    pub profile_id: String,
    pub profile_hash: String,
    pub max_transfer_distance_m: f64,
    stop_ids: Vec<String>,
    component_keys: Vec<String>,
    key_sets: Vec<Vec<u16>>,
    /// Origin stop (index into `stop_ids`) of each block.
    group_from: Vec<u32>,
    /// Transfer range of each block; `len = groups + 1`.
    group_offsets: Vec<u32>,
    /// Byte range of each block in `blocks`; `len = groups + 1`.
    block_offsets: Vec<u64>,
    to_stop: Vec<u32>,
    straight_line_m: Vec<f64>,
    travel_time_s: Vec<u32>,
    points: Vec<[f64; 3]>,
    blocks: Vec<u8>,
}

/// One transfer's routing columns, with stop ids resolved.
pub(crate) struct PackedTransferEntry<'a> {
    pub(crate) transfer: u32,
    pub(crate) from_stop_id: &'a str,
    pub(crate) to_stop_id: &'a str,
    pub(crate) straight_line_distance_m: f64,
    pub(crate) travel_time_s: u32,
}

#[derive(Serialize, Deserialize)]
struct PackedHeader {
    schema_version: u32,
    feed_id: String,
    source_sha256: String,
    stop_binding_sha256: Option<String>,
    dataset_id: String,
    profile_id: String,
    profile_hash: String,
    max_transfer_distance_m: f64,
    stop_ids: Vec<String>,
    component_keys: Vec<String>,
    key_sets: Vec<Vec<u16>>,
}

impl PackedTransitTransferTable {
    /// Packs an expanded table. Transfers are grouped by origin stop in order
    /// of first appearance; the order inside each origin is preserved.
    pub fn from_table(table: &TransitTransferTable) -> Result<Self> {
        let mut group_by_stop = HashMap::<&str, usize>::new();
        let mut groups = Vec::<(&str, Vec<&TransitNetworkTransfer>)>::new();
        for transfer in &table.transfers {
            let group = *group_by_stop
                .entry(transfer.from_stop_id.as_str())
                .or_insert_with(|| {
                    groups.push((transfer.from_stop_id.as_str(), Vec::new()));
                    groups.len() - 1
                });
            groups[group].1.push(transfer);
        }
        let mut packer = TransferPacker::new(TransferTableHeader {
            schema_version: table.schema_version,
            feed_id: table.feed_id.clone(),
            source_sha256: table.source_sha256.clone(),
            stop_binding_sha256: table.stop_binding_sha256.clone(),
            dataset_id: table.dataset_id.clone(),
            profile_id: table.profile_id.clone(),
            profile_hash: table.profile_hash.clone(),
            max_transfer_distance_m: table.max_transfer_distance_m,
        });
        for (from_stop_id, transfers) in groups {
            packer.push_group(
                from_stop_id,
                transfers.into_iter().map(|transfer| {
                    (
                        transfer.to_stop_id.as_str(),
                        transfer.straight_line_distance_m,
                        &transfer.path,
                    )
                }),
            )?;
        }
        Ok(packer.finish())
    }

    /// Expands every transfer back to its full street path.
    pub fn to_table(&self) -> Result<TransitTransferTable> {
        let mut transfers = Vec::with_capacity(self.len());
        for group in 0..self.group_count() {
            let from_stop_id = &self.stop_ids[self.group_from[group] as usize];
            let paths = self.decode_group(group, Decode::All)?;
            let start = self.group_offsets[group] as usize;
            for (local, path) in paths.into_iter().enumerate() {
                let transfer = start + local;
                transfers.push(TransitNetworkTransfer {
                    from_stop_id: from_stop_id.clone(),
                    to_stop_id: self.stop_ids[self.to_stop[transfer] as usize].clone(),
                    straight_line_distance_m: self.straight_line_m[transfer],
                    path,
                });
            }
        }
        Ok(TransitTransferTable {
            schema_version: self.schema_version,
            feed_id: self.feed_id.clone(),
            source_sha256: self.source_sha256.clone(),
            stop_binding_sha256: self.stop_binding_sha256.clone(),
            dataset_id: self.dataset_id.clone(),
            profile_id: self.profile_id.clone(),
            profile_hash: self.profile_hash.clone(),
            max_transfer_distance_m: self.max_transfer_distance_m,
            transfers,
        })
    }

    /// Number of directed transfers.
    pub fn len(&self) -> usize {
        self.to_stop.len()
    }

    pub fn is_empty(&self) -> bool {
        self.to_stop.is_empty()
    }

    /// Full street path of one transfer, as returned on transfer legs.
    pub fn path(&self, transfer: u32) -> Result<TransitStreetPath> {
        ensure!(
            (transfer as usize) < self.len(),
            "transfer {transfer} is outside the table"
        );
        let group = self
            .group_offsets
            .partition_point(|offset| *offset <= transfer)
            - 1;
        let local = (transfer - self.group_offsets[group]) as usize;
        self.decode_group(group, Decode::One(local))?
            .pop()
            .ok_or_else(|| anyhow!("transfer {transfer} is missing from its origin block"))
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = PackedTransferEntry<'_>> {
        (0..self.group_count()).flat_map(move |group| {
            let from_stop_id = self.stop_ids[self.group_from[group] as usize].as_str();
            (self.group_offsets[group]..self.group_offsets[group + 1]).map(move |transfer| {
                let index = transfer as usize;
                PackedTransferEntry {
                    transfer,
                    from_stop_id,
                    to_stop_id: self.stop_ids[self.to_stop[index] as usize].as_str(),
                    straight_line_distance_m: self.straight_line_m[index],
                    travel_time_s: self.travel_time_s[index],
                }
            })
        })
    }

    fn group_count(&self) -> usize {
        self.group_from.len()
    }

    /// Checks every array bound and decodes every block once, so a damaged
    /// file fails at load rather than in the middle of a response.
    fn validate(&self) -> Result<()> {
        let groups = self.group_count();
        ensure!(
            self.group_offsets.len() == groups + 1 && self.block_offsets.len() == groups + 1,
            "packed transfer table has inconsistent group offsets"
        );
        ensure!(
            self.group_offsets.first() == Some(&0)
                && self.block_offsets.first() == Some(&0)
                && self.group_offsets.windows(2).all(|pair| pair[0] <= pair[1])
                && self.block_offsets.windows(2).all(|pair| pair[0] <= pair[1])
                && self.group_offsets.last().copied() == Some(self.to_stop.len() as u32)
                && self.block_offsets.last().copied() == Some(self.blocks.len() as u64),
            "packed transfer table offsets are not monotone or do not cover the data"
        );
        ensure!(
            self.straight_line_m.len() == self.len() && self.travel_time_s.len() == self.len(),
            "packed transfer table columns have different lengths"
        );
        let stop_count = self.stop_ids.len() as u32;
        ensure!(
            self.group_from.iter().all(|stop| *stop < stop_count)
                && self.to_stop.iter().all(|stop| *stop < stop_count),
            "packed transfer table references an unknown stop"
        );
        ensure!(
            self.key_sets
                .iter()
                .flatten()
                .all(|key| { (*key as usize) < self.component_keys.len() }),
            "packed transfer table references an unknown component key"
        );
        ensure!(
            self.points.iter().flatten().all(|value| value.is_finite()),
            "packed transfer table has non-finite geometry"
        );
        for group in 0..groups {
            self.decode_group(group, Decode::Validate)
                .with_context(|| format!("decoding transfer block {group}"))?;
        }
        Ok(())
    }

    fn decode_group(&self, group: usize, mode: Decode) -> Result<Vec<TransitStreetPath>> {
        let start = usize::try_from(self.block_offsets[group])?;
        let end = usize::try_from(self.block_offsets[group + 1])?;
        let transfer_start = self.group_offsets[group] as usize;
        let transfer_count = self.group_offsets[group + 1] as usize - transfer_start;
        let mut cursor = Cursor::new(&self.blocks[start..end]);

        let node_count = cursor.length()?;
        let mut parent = Vec::with_capacity(node_count);
        let mut edge = Vec::with_capacity(node_count);
        let mut point = Vec::with_capacity(node_count);
        for index in 0..node_count {
            // Head: zigzag edge delta, then flags for an explicit parent
            // (default: the previous node) and an explicit point (default:
            // the parent's point + 1, the next fresh pool entry).
            let head = cursor.varint()?;
            let code = if head & 1 == 1 { cursor.varint()? } else { 1 };
            let (parent_node, base_edge, base_point) = if code == 0 {
                (NO_NODE, 0, 0)
            } else {
                let parent_node = (index as u64)
                    .checked_sub(code)
                    .ok_or_else(|| anyhow!("trie node {index} has a forward parent"))?
                    as usize;
                (parent_node as u32, edge[parent_node], point[parent_node])
            };
            parent.push(parent_node);
            edge.push(apply_delta(base_edge, head >> 2)?);
            let point_id = if head & 2 == 2 {
                cursor.delta_u32(base_point)?
            } else {
                base_point
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("trie node {index} point overflows"))?
            };
            ensure!(
                (point_id as usize) < self.points.len(),
                "trie node {index} references an unknown point"
            );
            point.push(point_id);
        }

        let mut paths = Vec::new();
        let mut previous_first_point = 0_u32;
        let mut values = Vec::new();
        for local in 0..transfer_count {
            let keep = match mode {
                Decode::All => true,
                Decode::One(wanted) => local == wanted,
                Decode::Validate => false,
            };
            let mut edge_path = Vec::new();
            let mut geometry = Vec::new();
            match cursor.varint()? {
                0 => {
                    let edge_count = cursor.length()?;
                    let mut previous = 0;
                    for _ in 0..edge_count {
                        previous = cursor.delta_u32(previous)?;
                        if keep {
                            edge_path.push(previous);
                        }
                    }
                    let point_count = cursor.length()?;
                    let mut previous = 0;
                    for _ in 0..point_count {
                        previous = cursor.delta_u32(previous)?;
                        let value = self
                            .points
                            .get(previous as usize)
                            .ok_or_else(|| anyhow!("raw path references an unknown point"))?;
                        if keep {
                            geometry.push(*value);
                        }
                    }
                }
                code => {
                    let first = cursor.delta_u32(previous_first_point)?;
                    previous_first_point = first;
                    let first_point = self
                        .points
                        .get(first as usize)
                        .ok_or_else(|| anyhow!("path references an unknown first point"))?;
                    if code >= 2 {
                        let leaf = usize::try_from(code - 2)?;
                        ensure!(leaf < node_count, "path references an unknown trie node");
                        if keep {
                            let mut node = leaf as u32;
                            while node != NO_NODE {
                                edge_path.push(edge[node as usize]);
                                geometry.push(self.points[point[node as usize] as usize]);
                                node = parent[node as usize];
                            }
                            edge_path.reverse();
                            geometry.push(*first_point);
                            geometry.reverse();
                        }
                    } else if keep {
                        geometry.push(*first_point);
                    }
                }
            }

            let key_set = self
                .key_sets
                .get(cursor.length()?)
                .ok_or_else(|| anyhow!("path references an unknown component key set"))?;
            cursor.values(1 + key_set.len(), &mut values)?;
            let distance_m = values[0];
            if let Some(distance) = distance_m {
                ensure!(
                    distance.is_finite() && distance >= 0.0,
                    "path has an invalid distance"
                );
            }
            let mut components = BTreeMap::new();
            for (key, value) in key_set.iter().zip(&values[1..]) {
                let value = value.ok_or_else(|| anyhow!("component value is missing"))?;
                ensure!(value.is_finite(), "path has a non-finite component");
                if keep {
                    components.insert(self.component_keys[*key as usize].clone(), value);
                }
            }
            if keep {
                paths.push(TransitStreetPath {
                    travel_time_s: self.travel_time_s[transfer_start + local],
                    distance_m,
                    edge_path,
                    geometry,
                    components,
                });
                if matches!(mode, Decode::One(_)) {
                    return Ok(paths);
                }
            }
        }
        ensure!(
            cursor.is_empty(),
            "transfer block has trailing bytes after its paths"
        );
        Ok(paths)
    }
}

#[derive(Clone, Copy)]
enum Decode {
    Validate,
    One(usize),
    All,
}

pub(crate) struct TransferTableHeader {
    pub(crate) schema_version: u32,
    pub(crate) feed_id: String,
    pub(crate) source_sha256: String,
    pub(crate) stop_binding_sha256: Option<String>,
    pub(crate) dataset_id: String,
    pub(crate) profile_id: String,
    pub(crate) profile_hash: String,
    pub(crate) max_transfer_distance_m: f64,
}

/// Incremental builder: origin blocks are appended one at a time, so a table
/// build never has to hold every expanded path at once.
pub(crate) struct TransferPacker {
    table: PackedTransitTransferTable,
    stop_lookup: HashMap<String, u32>,
    key_lookup: HashMap<String, u16>,
    key_set_lookup: HashMap<Vec<u16>, u32>,
    point_lookup: HashMap<[u64; 3], u32>,
}

impl TransferPacker {
    pub(crate) fn new(header: TransferTableHeader) -> Self {
        Self {
            table: PackedTransitTransferTable {
                schema_version: header.schema_version,
                feed_id: header.feed_id,
                source_sha256: header.source_sha256,
                stop_binding_sha256: header.stop_binding_sha256,
                dataset_id: header.dataset_id,
                profile_id: header.profile_id,
                profile_hash: header.profile_hash,
                max_transfer_distance_m: header.max_transfer_distance_m,
                stop_ids: Vec::new(),
                component_keys: Vec::new(),
                key_sets: Vec::new(),
                group_from: Vec::new(),
                group_offsets: vec![0],
                block_offsets: vec![0],
                to_stop: Vec::new(),
                straight_line_m: Vec::new(),
                travel_time_s: Vec::new(),
                points: Vec::new(),
                blocks: Vec::new(),
            },
            stop_lookup: HashMap::new(),
            key_lookup: HashMap::new(),
            key_set_lookup: HashMap::new(),
            point_lookup: HashMap::new(),
        }
    }

    fn stop(&mut self, stop_id: &str) -> u32 {
        if let Some(index) = self.stop_lookup.get(stop_id) {
            return *index;
        }
        let index = self.table.stop_ids.len() as u32;
        self.table.stop_ids.push(stop_id.to_string());
        self.stop_lookup.insert(stop_id.to_string(), index);
        index
    }

    fn point(&mut self, value: [f64; 3]) -> u32 {
        let next = self.table.points.len() as u32;
        let id = *self
            .point_lookup
            .entry(value.map(f64::to_bits))
            .or_insert(next);
        if id == next {
            self.table.points.push(value);
        }
        id
    }

    fn key_set(&mut self, components: &BTreeMap<String, f64>) -> Result<u32> {
        let mut keys = Vec::with_capacity(components.len());
        for key in components.keys() {
            let id = match self.key_lookup.get(key) {
                Some(id) => *id,
                None => {
                    let id = u16::try_from(self.table.component_keys.len())
                        .context("transfer table has too many distinct component keys")?;
                    self.table.component_keys.push(key.clone());
                    self.key_lookup.insert(key.clone(), id);
                    id
                }
            };
            keys.push(id);
        }
        if let Some(id) = self.key_set_lookup.get(&keys) {
            return Ok(*id);
        }
        let id = self.table.key_sets.len() as u32;
        self.table.key_sets.push(keys.clone());
        self.key_set_lookup.insert(keys, id);
        Ok(id)
    }

    /// Appends one origin's transfers in their final order. Origins without
    /// transfers add nothing, not even a stop id.
    pub(crate) fn push_group<'p>(
        &mut self,
        from_stop_id: &str,
        transfers: impl IntoIterator<Item = (&'p str, f64, &'p TransitStreetPath)>,
    ) -> Result<()> {
        let mut transfers = transfers.into_iter().peekable();
        if transfers.peek().is_none() {
            return Ok(());
        }
        let from = self.stop(from_stop_id);
        let mut nodes = Vec::<(u32, u32, u32)>::new();
        let mut node_lookup = HashMap::<(u32, u32, u32), u32>::new();
        let mut records = Vec::<u8>::new();
        let mut previous_first_point = 0_u32;
        let mut count = 0_u32;
        for (to_stop_id, straight_line_distance_m, path) in transfers {
            validate_network_path(from_stop_id, to_stop_id, path)?;
            let to = self.stop(to_stop_id);
            self.table.to_stop.push(to);
            self.table.straight_line_m.push(straight_line_distance_m);
            self.table.travel_time_s.push(path.travel_time_s);
            count += 1;

            if !path.geometry.is_empty() && path.geometry.len() == path.edge_path.len() + 1 {
                let first = self.point(path.geometry[0]);
                let mut node = NO_NODE;
                for (edge, value) in path.edge_path.iter().zip(&path.geometry[1..]) {
                    // A node is keyed by its parent, so equal prefixes share nodes.
                    let key = (node, *edge, self.point(*value));
                    node = match node_lookup.get(&key) {
                        Some(existing) => *existing,
                        None => {
                            let created = nodes.len() as u32;
                            nodes.push(key);
                            node_lookup.insert(key, created);
                            created
                        }
                    };
                }
                put_varint(
                    &mut records,
                    if node == NO_NODE { 1 } else { node as u64 + 2 },
                );
                put_delta(&mut records, previous_first_point, first);
                previous_first_point = first;
            } else {
                put_varint(&mut records, 0);
                put_varint(&mut records, path.edge_path.len() as u64);
                let mut previous = 0;
                for edge in &path.edge_path {
                    put_delta(&mut records, previous, *edge);
                    previous = *edge;
                }
                put_varint(&mut records, path.geometry.len() as u64);
                let mut previous = 0;
                for value in &path.geometry {
                    let point = self.point(*value);
                    put_delta(&mut records, previous, point);
                    previous = point;
                }
            }

            let key_set = self.key_set(&path.components)?;
            put_varint(&mut records, u64::from(key_set));
            let values = std::iter::once(path.distance_m)
                .chain(path.components.values().map(|value| Some(*value)))
                .collect::<Vec<_>>();
            put_values(&mut records, &values);
        }

        let block = &mut self.table.blocks;
        put_varint(block, nodes.len() as u64);
        for (index, (parent, edge, point)) in nodes.iter().enumerate() {
            let (parent_code, parent_edge, parent_point) = if *parent == NO_NODE {
                (0, 0, 0)
            } else {
                let (_, parent_edge, parent_point) = nodes[*parent as usize];
                (u64::from(index as u32 - parent), parent_edge, parent_point)
            };
            let explicit_parent = parent_code != 1;
            let explicit_point = parent_point.checked_add(1) != Some(*point);
            put_varint(
                block,
                (zigzag(i64::from(*edge) - i64::from(parent_edge)) << 2)
                    | (u64::from(explicit_point) << 1)
                    | u64::from(explicit_parent),
            );
            if explicit_parent {
                put_varint(block, parent_code);
            }
            if explicit_point {
                put_delta(block, parent_point, *point);
            }
        }
        block.extend_from_slice(&records);
        self.table.group_from.push(from);
        let transfer_end = self
            .table
            .group_offsets
            .last()
            .copied()
            .unwrap_or_default()
            .checked_add(count)
            .context("transfer table has more than u32::MAX transfers")?;
        self.table.group_offsets.push(transfer_end);
        self.table.block_offsets.push(block.len() as u64);
        Ok(())
    }

    pub(crate) fn finish(self) -> PackedTransitTransferTable {
        self.table
    }
}

pub(crate) fn validate_network_path(
    from_stop_id: &str,
    to_stop_id: &str,
    path: &TransitStreetPath,
) -> Result<()> {
    if path
        .distance_m
        .is_some_and(|distance| !distance.is_finite() || distance < 0.0)
    {
        bail!("network transfer '{from_stop_id} -> {to_stop_id}' has an invalid distance");
    }
    if path
        .geometry
        .iter()
        .flatten()
        .any(|value| !value.is_finite())
    {
        bail!("network transfer '{from_stop_id} -> {to_stop_id}' has non-finite geometry");
    }
    if path.components.values().any(|value| !value.is_finite()) {
        bail!("network transfer '{from_stop_id} -> {to_stop_id}' has a non-finite component");
    }
    Ok(())
}

/// Writes the packed binary layout, or the expanded JSON document for paths
/// ending in `.json`.
pub fn write_packed_transit_transfer_table(
    path: impl AsRef<Path>,
    table: &PackedTransitTransferTable,
) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
        let raw = serde_json::to_string_pretty(&table.to_table()?)?;
        return fs::write(path, raw).with_context(|| format!("writing {}", path.display()));
    }
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, file);
    let header = PackedHeader {
        schema_version: table.schema_version,
        feed_id: table.feed_id.clone(),
        source_sha256: table.source_sha256.clone(),
        stop_binding_sha256: table.stop_binding_sha256.clone(),
        dataset_id: table.dataset_id.clone(),
        profile_id: table.profile_id.clone(),
        profile_hash: table.profile_hash.clone(),
        max_transfer_distance_m: table.max_transfer_distance_m,
        stop_ids: table.stop_ids.clone(),
        component_keys: table.component_keys.clone(),
        key_sets: table.key_sets.clone(),
    };
    let mut write = || -> Result<()> {
        writer.write_all(MAGIC)?;
        write_section(&mut writer, &bincode::serialize(&header)?)?;
        write_section(&mut writer, bytemuck::cast_slice(&table.group_from))?;
        write_section(&mut writer, bytemuck::cast_slice(&table.group_offsets))?;
        write_section(&mut writer, bytemuck::cast_slice(&table.block_offsets))?;
        write_section(&mut writer, bytemuck::cast_slice(&table.to_stop))?;
        write_section(&mut writer, bytemuck::cast_slice(&table.straight_line_m))?;
        write_section(&mut writer, bytemuck::cast_slice(&table.travel_time_s))?;
        write_section(&mut writer, bytemuck::cast_slice(&table.points))?;
        write_section(&mut writer, &table.blocks)?;
        writer.flush()?;
        Ok(())
    };
    write().with_context(|| format!("writing transit transfer table {}", path.display()))
}

pub(crate) fn read_packed_file(path: impl AsRef<Path>) -> Result<PackedTransitTransferTable> {
    let path = path.as_ref();
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    decode_file(&bytes).with_context(|| {
        format!(
            "parsing transit transfer table {}; rebuild it with `netweevil transit transfers build`",
            path.display()
        )
    })
}

fn decode_file(bytes: &[u8]) -> Result<PackedTransitTransferTable> {
    ensure!(
        bytes.starts_with(MAGIC),
        "unsupported transfer table format"
    );
    let mut sections = Sections {
        bytes,
        offset: MAGIC.len(),
    };
    let header: PackedHeader = bincode::deserialize(sections.next()?)?;
    if header.schema_version != TRANSIT_TRANSFER_TABLE_SCHEMA_VERSION {
        bail!(
            "unsupported transit transfer table schema version {}; expected {}",
            header.schema_version,
            TRANSIT_TRANSFER_TABLE_SCHEMA_VERSION
        );
    }
    let table = PackedTransitTransferTable {
        schema_version: header.schema_version,
        feed_id: header.feed_id,
        source_sha256: header.source_sha256,
        stop_binding_sha256: header.stop_binding_sha256,
        dataset_id: header.dataset_id,
        profile_id: header.profile_id,
        profile_hash: header.profile_hash,
        max_transfer_distance_m: header.max_transfer_distance_m,
        stop_ids: header.stop_ids,
        component_keys: header.component_keys,
        key_sets: header.key_sets,
        group_from: sections.raw()?,
        group_offsets: sections.raw()?,
        block_offsets: sections.raw()?,
        to_stop: sections.raw()?,
        straight_line_m: sections.raw()?,
        travel_time_s: sections.raw()?,
        points: sections.raw()?,
        blocks: sections.next()?.to_vec(),
    };
    ensure!(
        sections.offset == bytes.len(),
        "transfer table has trailing bytes"
    );
    table.validate()?;
    Ok(table)
}

fn write_section(writer: &mut impl Write, bytes: &[u8]) -> Result<()> {
    writer.write_all(&(bytes.len() as u64).to_le_bytes())?;
    writer.write_all(bytes)?;
    Ok(())
}

struct Sections<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Sections<'a> {
    fn next(&mut self) -> Result<&'a [u8]> {
        let length_end = self.offset + 8;
        let length = self
            .bytes
            .get(self.offset..length_end)
            .ok_or_else(|| anyhow!("transfer table is truncated"))?;
        let length = usize::try_from(u64::from_le_bytes(length.try_into()?))?;
        let end = length_end
            .checked_add(length)
            .ok_or_else(|| anyhow!("transfer table section is too large"))?;
        let section = self
            .bytes
            .get(length_end..end)
            .ok_or_else(|| anyhow!("transfer table is truncated"))?;
        self.offset = end;
        Ok(section)
    }

    fn raw<T: Pod>(&mut self) -> Result<Vec<T>> {
        let section = self.next()?;
        ensure!(
            section.len() % std::mem::size_of::<T>() == 0,
            "transfer table section has a partial element"
        );
        Ok(bytemuck::pod_collect_to_vec(section))
    }
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

fn put_delta(out: &mut Vec<u8>, base: u32, value: u32) {
    put_varint(out, zigzag(i64::from(value) - i64::from(base)));
}

fn apply_delta(base: u32, zigzagged: u64) -> Result<u32> {
    let delta = ((zigzagged >> 1) as i64) ^ -((zigzagged & 1) as i64);
    u32::try_from(i64::from(base) + delta)
        .map_err(|_| anyhow!("transfer block delta leaves the u32 range"))
}

/// Stores optional `f64`s with a 4-bit tag each (two per byte) followed by
/// the payloads. Every encoding reproduces the exact bit pattern.
fn put_values(out: &mut Vec<u8>, values: &[Option<f64>]) {
    let mut tags = Vec::with_capacity(values.len());
    let mut payload = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let Some(value) = *value else {
            tags.push(TAG_NONE);
            continue;
        };
        let bits = value.to_bits();
        if bits == 0 {
            tags.push(TAG_ZERO);
        } else if let Some(earlier) = values[..index]
            .iter()
            .position(|other| other.is_some_and(|other| other.to_bits() == bits))
        {
            tags.push(TAG_REPEAT);
            put_varint(&mut payload, earlier as u64);
        } else if value > 0.0 && value < 9_007_199_254_740_992.0 && value.fract() == 0.0 {
            tags.push(TAG_INTEGER);
            put_varint(&mut payload, value as u64);
        } else if f64::from(value as f32).to_bits() == bits {
            tags.push(TAG_F32);
            payload.extend_from_slice(&(value as f32).to_le_bytes());
        } else {
            tags.push(TAG_F64);
            payload.extend_from_slice(&value.to_le_bytes());
        }
    }
    for pair in tags.chunks(2) {
        out.push(pair[0] | pair.get(1).map_or(0, |tag| tag << 4));
    }
    out.extend_from_slice(&payload);
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self.offset + length;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| anyhow!("transfer block is truncated"))?;
        self.offset = end;
        Ok(bytes)
    }

    fn varint(&mut self) -> Result<u64> {
        let mut value = 0_u64;
        for shift in (0..64).step_by(7) {
            let byte = self.take(1)?[0];
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        bail!("transfer block has an overlong varint")
    }

    fn length(&mut self) -> Result<usize> {
        let value = self.varint()?;
        let length = usize::try_from(value)?;
        // Every encoded element takes at least one byte; reject lengths that
        // could not fit so damaged data cannot trigger huge allocations.
        ensure!(
            length <= self.bytes.len(),
            "transfer block declares an impossible length"
        );
        Ok(length)
    }

    fn delta_u32(&mut self, base: u32) -> Result<u32> {
        apply_delta(base, self.varint()?)
    }

    fn values(&mut self, count: usize, out: &mut Vec<Option<f64>>) -> Result<()> {
        let tags = self.take(count.div_ceil(2))?;
        out.clear();
        for index in 0..count {
            let tag = (tags[index / 2] >> ((index % 2) * 4)) & 0x0f;
            let value = match tag {
                TAG_NONE => None,
                TAG_ZERO => Some(0.0),
                TAG_REPEAT => {
                    let earlier = usize::try_from(self.varint()?)?;
                    ensure!(earlier < index, "repeated value points forward");
                    out[earlier]
                }
                TAG_INTEGER => Some(self.varint()? as f64),
                TAG_F32 => Some(f64::from(f32::from_le_bytes(self.take(4)?.try_into()?))),
                TAG_F64 => Some(f64::from_le_bytes(self.take(8)?.try_into()?)),
                other => bail!("transfer block has unknown value tag {other}"),
            };
            out.push(value);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
