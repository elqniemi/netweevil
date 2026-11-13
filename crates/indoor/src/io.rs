//! Binary graph files: magic + format version + schema fingerprint + bincode payload.

use crate::graph::Graph;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};

const MAGIC: &[u8; 4] = b"NWIG";
pub const FORMAT: u32 = 1;

/// Serialized layout of `Graph` (field names in order, nested structs in brackets). Update it
/// whenever a serialized struct changes; its fingerprint is checked on load, so stale graph
/// files are rejected with a clear message instead of a bincode error or garbage.
const SCHEMA: &str = "Graph{levels[LevelInfo{id,z,props}],strings,\
elems[Elem{kind,id,class,sub,tags,space,level,cost_factor,speed,time,penalty,width,step_free,wait,props,access}],\
mesh{verts,wall,tri_v,tri_z,tri_nbr,tri_surf,tri_zone,tri_level,tri_c,tri_w,tri_slope},\
level_tri,zone_level,zone_off,zone_nodes,nodes[Node{xyz,level,zone,tri,elem,lead}],origin,node_xyz,\
first_out,head,arc_fact,facts[Fact{kind,elem,zone,level,len3,dz,max_up,max_down,time,pen,p0,p1,g0,g1,\
c0,c1,h0,h1,na,nb}],pieces[Piece{elem,len,climb,descent}],geom,chains,tri_grid,seg_grid,segs,\
names:btree,portals:btree,space_tri:btree,\
pois[PoiInfo{id,category,tags,nodes,snaps[PoiSnap{fact,s,p}],props,access}],poi_ids:btree,\
access_groups}";

/// FNV-1a hash of `SCHEMA`.
pub const fn schema_fingerprint() -> u64 {
    let b = SCHEMA.as_bytes();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < b.len() {
        h ^= b[i] as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    h
}

pub const SCHEMA_FINGERPRINT: u64 = schema_fingerprint();

pub fn save(g: &Graph, path: &str) -> Result<(), String> {
    let f = File::create(path).map_err(|e| format!("{path}: {e}"))?;
    let mut w = BufWriter::with_capacity(1 << 20, f);
    w.write_all(MAGIC)
        .and_then(|_| w.write_all(&FORMAT.to_le_bytes()))
        .and_then(|_| w.write_all(&SCHEMA_FINGERPRINT.to_le_bytes()))
        .map_err(|e| e.to_string())?;
    bincode::serialize_into(&mut w, g).map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())
}

pub fn load(path: &str) -> Result<Graph, String> {
    let f = File::open(path).map_err(|e| format!("{path}: {e}"))?;
    read(BufReader::with_capacity(1 << 20, f), path)
}

/// A graph file already in memory.
pub fn from_bytes(b: &[u8]) -> Result<Graph, String> {
    read(b, "graph")
}

/// A graph file from `r`; `path` names it in errors.
pub fn read(mut r: impl Read, path: &str) -> Result<Graph, String> {
    let mut head = [0u8; 8];
    r.read_exact(&mut head)
        .map_err(|e| format!("{path}: {e}"))?;
    if &head[..4] != MAGIC {
        return Err(format!("{path}: not a NetWeevil indoor graph"));
    }
    let v = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
    if v != FORMAT {
        return Err(format!(
            "{path}: graph format {v}, expected {FORMAT} (rebuild it)"
        ));
    }
    let mut fp = [0u8; 8];
    r.read_exact(&mut fp).map_err(|e| format!("{path}: {e}"))?;
    if u64::from_le_bytes(fp) != SCHEMA_FINGERPRINT {
        return Err(format!(
            "{path}: graph schema {:016x}, expected {SCHEMA_FINGERPRINT:016x} (rebuild it)",
            u64::from_le_bytes(fp)
        ));
    }
    bincode::deserialize_from(r).map_err(|e| format!("{path}: {e}"))
}
