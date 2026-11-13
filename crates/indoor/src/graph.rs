//! The compiled graph (CSR over portal nodes) and `compile(Input) -> Graph`.

use crate::funnel::{WalkBuf, WalkStats, measure};
use crate::geom::*;
use crate::input::{Input, Node as InNode, Vtx, gen_id, props_str};
use crate::mesh::{LevelMesh, Mesh, SurfIn, TriScratch, build_level};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Instant;

/// High bit of `arc_fact`: the arc traverses its fact backwards.
pub const REV: u32 = 1 << 31;
/// Max nodes per zone connected all-pairs; bigger zones connect each node to its K nearest.
pub const K_ZONE: usize = 48;
/// Storey height assumed for verticals between levels without z (2D mode).
pub const STOREY: f64 = 3.5;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElemKind {
    Surface,
    Edge,
    Portal,
    Vertical,
    Node,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum FactKind {
    Walk,
    Attach,
    Edge,
    Portal,
    Vertical,
}

impl FactKind {
    pub fn name(self) -> &'static str {
        match self {
            FactKind::Walk | FactKind::Attach => "walk",
            FactKind::Edge => "edge",
            FactKind::Portal => "portal",
            FactKind::Vertical => "vertical",
        }
    }
}

/// An input element (surface, edge, portal, vertical, node) with its routing attributes.
/// Strings are interned in `Graph::strings`; `NaN` = unset.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Elem {
    pub kind: ElemKind,
    pub id: String,
    pub class: u32,
    /// Portal / vertical kind (door, crossing, lift, stairs, ...).
    pub sub: u32,
    pub tags: Vec<u32>,
    pub space: u32,
    pub level: u32,
    pub cost_factor: f32,
    pub speed: f32,
    pub time: f32,
    pub penalty: f32,
    pub width: f32,
    pub step_free: bool,
    pub wait: f32,
    pub props: String,
    /// Access groups needed (any-of; bit i = `Graph::access_groups[i]`, bit 63 = overflow,
    /// never granted except by `*`); 0 = public.
    pub access: u64,
}

/// Horizontal length, climb and descent walked on one element (forward direction).
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct Piece {
    pub elem: u32,
    pub len: f32,
    pub climb: f32,
    pub descent: f32,
}

/// Profile-independent facts of one link; arcs reference it forwards or reversed.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Fact {
    pub kind: FactKind,
    /// Edge / portal / vertical element (NONE for walks).
    pub elem: u32,
    pub zone: u32,
    pub level: u32,
    pub len3: f32,
    pub dz: f32,
    pub max_up: f32,
    pub max_down: f32,
    /// Fixed traversal time (NaN = none) and fixed penalty (seconds).
    pub time: f32,
    pub pen: f32,
    pub p0: u32,
    pub p1: u32,
    pub g0: u32,
    pub g1: u32,
    pub c0: u32,
    pub c1: u32,
    /// Heading of the first / last geometry segment.
    pub h0: f32,
    pub h1: f32,
    /// End nodes (geometry runs na -> nb).
    pub na: u32,
    pub nb: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Node {
    pub xyz: P3,
    pub level: u32,
    pub zone: u32,
    pub tri: u32,
    /// Node element (penalty / tags) or NONE.
    pub elem: u32,
    /// Direction into the zone for portal ends (walks lead in/out along it).
    pub lead: [f32; 2],
}

/// A compiled point of interest: its access points as graph nodes (deduplicated, input order).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PoiInfo {
    pub id: String,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub nodes: Vec<u32>,
    /// Point accesses off every surface, snapped onto the nearest edge (outdoor POIs).
    pub snaps: Vec<PoiSnap>,
    pub props: String,
    /// Access groups needed to be offered this POI (as `Elem::access`).
    pub access: u64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct PoiSnap {
    pub fact: u32,
    pub s: f64,
    pub p: P3,
}

impl PoiInfo {
    /// `cat` equals the category or is a `/`-prefix of it (`toilet` matches `toilet/accessible`).
    pub fn in_category(&self, cat: &str) -> bool {
        let cat = cat.trim_end_matches('/');
        match &self.category {
            Some(c) => {
                cat.is_empty()
                    || c == cat
                    || (c.len() > cat.len()
                        && c.starts_with(cat)
                        && c.as_bytes()[cat.len()] == b'/')
            }
            None => cat.is_empty(),
        }
    }
    pub fn has_tags(&self, tags: &[String]) -> bool {
        tags.iter().all(|t| self.tags.contains(t))
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LevelInfo {
    pub id: String,
    pub z: f64,
    pub props: String,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Graph {
    pub levels: Vec<LevelInfo>,
    pub strings: Vec<String>,
    pub elems: Vec<Elem>,
    pub mesh: Mesh,
    /// Global triangle range per level.
    pub level_tri: Vec<[u32; 2]>,
    pub zone_level: Vec<u32>,
    pub zone_off: Vec<u32>,
    pub zone_nodes: Vec<u32>,
    pub nodes: Vec<Node>,
    pub origin: P3,
    pub node_xyz: Vec<[f32; 3]>,
    pub first_out: Vec<u32>,
    pub head: Vec<u32>,
    pub arc_fact: Vec<u32>,
    pub facts: Vec<Fact>,
    pub pieces: Vec<Piece>,
    pub geom: Vec<P3>,
    pub chains: Vec<u32>,
    pub tri_grid: Vec<Grid>,
    pub seg_grid: Grid,
    /// (edge fact, geometry index of the segment start).
    pub segs: Vec<[u32; 2]>,
    // Ordered maps: graph files are byte-for-byte deterministic.
    pub names: BTreeMap<String, u32>,
    pub portals: BTreeMap<String, [u32; 2]>,
    pub space_tri: BTreeMap<String, u32>,
    pub pois: Vec<PoiInfo>,
    pub poi_ids: BTreeMap<String, u32>,
    /// Access group names (bit i of `Elem::access` / `PoiInfo::access`), first appearance.
    pub access_groups: Vec<String>,
    /// Restricted elements / arcs / POIs, computed on first use (`access.rs`).
    #[serde(skip)]
    pub(crate) restricted: OnceLock<crate::access::Restricted>,
    /// Element props as raw JSON, parsed once on first use (route output splices them).
    #[serde(skip)]
    props_raw: OnceLock<Vec<Option<Box<RawValue>>>>,
}

#[derive(Serialize, Default, Debug, Clone)]
pub struct CompileReport {
    pub levels: usize,
    pub surfaces: usize,
    pub triangles: usize,
    pub zones: usize,
    pub nodes: usize,
    pub arcs: usize,
    pub facts: usize,
    pub millis: f64,
    pub timings: Vec<(String, f64)>,
    pub warnings: Vec<Warning>,
}

/// A compile problem tied to an input element where possible (`kind`: surface | edge | node |
/// portal | vertical | mesh), with a location for display. Never fatal.
#[derive(Serialize, Default, Debug, Clone)]
pub struct Warning {
    pub id: Option<String>,
    pub kind: &'static str,
    pub msg: String,
    pub at: Option<[f64; 2]>,
    pub level: Option<String>,
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.id {
            Some(id) => write!(f, "{} {id}: {}", self.kind, self.msg)?,
            None => write!(f, "{}: {}", self.kind, self.msg)?,
        }
        if let Some(l) = &self.level {
            write!(f, " [level {l}]")?;
        }
        if let Some(a) = self.at {
            write!(f, " at ({:.2}, {:.2})", a[0], a[1])?;
        }
        Ok(())
    }
}

impl Graph {
    #[inline]
    pub fn fact_of(&self, arc: usize) -> (&Fact, bool) {
        let f = self.arc_fact[arc];
        (&self.facts[(f & !REV) as usize], f & REV != 0)
    }
    pub fn str(&self, i: u32) -> Option<&str> {
        self.strings.get(i as usize).map(|s| s.as_str())
    }
    pub fn zone_nodes(&self, z: u32) -> &[u32] {
        &self.zone_nodes[self.zone_off[z as usize] as usize..self.zone_off[z as usize + 1] as usize]
    }
    pub fn out_arcs(&self, n: u32) -> std::ops::Range<usize> {
        self.first_out[n as usize] as usize..self.first_out[n as usize + 1] as usize
    }
    /// Arc of fact `fact` leaving node `tail` in the given direction.
    pub fn find_arc(&self, tail: u32, fact: u32, rev: bool) -> Option<usize> {
        let want = fact | if rev { REV } else { 0 };
        self.out_arcs(tail).find(|&a| self.arc_fact[a] == want)
    }
    pub fn num_arcs(&self) -> usize {
        self.head.len()
    }
    /// Props of element `e` (None when it has none).
    pub fn props(&self, e: u32) -> Option<Box<RawValue>> {
        self.props_raw
            .get_or_init(|| {
                self.elems
                    .iter()
                    .map(|e| match e.props.as_str() {
                        "" => None,
                        s => RawValue::from_string(s.to_string()).ok(),
                    })
                    .collect()
            })
            .get(e as usize)?
            .clone()
    }
}

// ---------------------------------------------------------------------------
// Compile

struct B {
    g: Graph,
    arcs: Vec<(u32, u32, u32)>,
    warnings: Vec<Warning>,
    strings: HashMap<String, u32>,
    level_map: HashMap<String, u32>,
    access: HashMap<String, u32>,
    /// Access groups beyond the 63 that get a bit (mapped to the reserved bit 63).
    access_overflow: Vec<String>,
}

impl B {
    fn s(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.strings.get(s) {
            return i;
        }
        let i = self.g.strings.len() as u32;
        self.g.strings.push(s.to_string());
        self.strings.insert(s.to_string(), i);
        i
    }
    fn os(&mut self, s: &Option<String>) -> u32 {
        match s {
            Some(s) => self.s(s),
            None => NONE,
        }
    }
    fn warn(
        &mut self,
        kind: &'static str,
        id: Option<&str>,
        msg: String,
        at: Option<P2>,
        lvl: u32,
    ) {
        let level = self.g.levels.get(lvl as usize).map(|l| l.id.clone());
        self.warnings.push(Warning {
            id: id.map(str::to_string),
            kind,
            msg,
            at,
            level,
        });
    }

    /// Access mask of a group list: each name gets a bit (first appearance); past 63 groups
    /// the reserved bit 63 (never granted by name: fail closed).
    fn access(&mut self, groups: &[String]) -> u64 {
        let mut m = 0u64;
        for g in groups {
            if let Some(&i) = self.access.get(g) {
                m |= 1 << i;
            } else if self.g.access_groups.len() < crate::access::MAX_GROUPS {
                let i = self.g.access_groups.len() as u32;
                self.g.access_groups.push(g.clone());
                self.access.insert(g.clone(), i);
                m |= 1 << i;
            } else {
                if !self.access_overflow.contains(g) {
                    self.access_overflow.push(g.clone());
                }
                m |= crate::access::OVERFLOW;
            }
        }
        m
    }

    fn tags(&mut self, t: &[String]) -> Vec<u32> {
        t.iter().map(|s| self.s(s)).collect()
    }

    fn add_level(&mut self, id: &str, z: f64, props: String) -> u32 {
        let i = self.g.levels.len() as u32;
        self.g.levels.push(LevelInfo {
            id: id.to_string(),
            z,
            props,
        });
        self.level_map.insert(id.to_string(), i);
        i
    }

    /// Explicit level id, else a level whose z matches (rounded to 0.1 m), else a new `z=..` level.
    fn level(&mut self, id: Option<&str>, z: Option<f64>) -> u32 {
        if let Some(id) = id {
            if let Some(&l) = self.level_map.get(id) {
                return l;
            }
            return self.add_level(id, z.unwrap_or(0.0), String::new());
        }
        let z = (z.unwrap_or(0.0) * 10.0).round() / 10.0;
        if let Some(l) = self.g.levels.iter().position(|l| (l.z - z).abs() < 0.05) {
            return l as u32;
        }
        self.add_level(&format!("z={z:.1}"), z, String::new())
    }

    /// Nearest level within 0.6 m in z (for level-less edge vertices).
    fn level_near(&self, z: f64) -> u32 {
        let mut best = (NONE, 0.6);
        for (i, l) in self.g.levels.iter().enumerate() {
            let d = (l.z - z).abs();
            if d < best.1 {
                best = (i as u32, d);
            }
        }
        best.0
    }

    fn lz(&self, l: u32) -> f64 {
        self.g.levels.get(l as usize).map(|l| l.z).unwrap_or(0.0)
    }

    fn elem(&mut self, e: Elem) -> u32 {
        self.g.elems.push(e);
        (self.g.elems.len() - 1) as u32
    }

    fn node(&mut self, xyz: P3, level: u32, zone: u32, tri: u32, elem: u32, lead: P2) -> u32 {
        self.g.nodes.push(Node {
            xyz,
            level,
            zone,
            tri,
            elem,
            lead: [lead[0] as f32, lead[1] as f32],
        });
        (self.g.nodes.len() - 1) as u32
    }

    /// Node anchored in the triangle containing / nearest to `p` on `level`.
    fn anchor(&mut self, level: u32, p: P2, max_d: f64, elem: u32) -> Option<(u32, f64)> {
        let (t, q, d) = match self.g.tri_at(level, p) {
            Some(t) => (t, p, 0.0),
            None => {
                let (t, q, d) = self.g.nearest_tri(level, p, max_d, |_| true)?;
                (t, self.g.inset(t, q), d)
            }
        };
        let z = self.g.mesh.z_at(t, q);
        let zone = self.g.mesh.tri_zone[t as usize];
        Some((
            self.node([q[0], q[1], z], level, zone, t, elem, [0.0, 0.0]),
            d,
        ))
    }

    fn fact(
        &mut self,
        kind: FactKind,
        elem: u32,
        zone: u32,
        level: u32,
        pieces: &[Piece],
        geom: &[P3],
        chain: &[u32],
        st: WalkStats,
    ) -> u32 {
        let g = &mut self.g;
        let (p0, g0, c0) = (
            g.pieces.len() as u32,
            g.geom.len() as u32,
            g.chains.len() as u32,
        );
        g.pieces.extend_from_slice(pieces);
        // Walks are re-derived from their channel at query time: no stored polyline.
        if kind != FactKind::Walk {
            g.geom.extend_from_slice(geom);
        }
        g.chains.extend_from_slice(chain);
        let n = geom.len();
        let (h0, h1) = if n >= 2 {
            (
                heading(xy(geom[0]), xy(geom[1])),
                heading(xy(geom[n - 2]), xy(geom[n - 1])),
            )
        } else {
            (0.0, 0.0)
        };
        g.facts.push(Fact {
            kind,
            elem,
            zone,
            level,
            len3: poly_len3(geom) as f32,
            dz: if n > 0 {
                (geom[n - 1][2] - geom[0][2]) as f32
            } else {
                0.0
            },
            max_up: st.max_up as f32,
            max_down: st.max_down as f32,
            time: f32::NAN,
            pen: 0.0,
            p0,
            p1: g.pieces.len() as u32,
            g0,
            g1: g.geom.len() as u32,
            c0,
            c1: g.chains.len() as u32,
            h0,
            h1,
            na: NONE,
            nb: NONE,
        });
        (g.facts.len() - 1) as u32
    }

    fn link(&mut self, a: u32, b: u32, fact: u32, fwd: bool, back: bool) {
        let f = &mut self.g.facts[fact as usize];
        f.na = a;
        f.nb = b;
        if fwd {
            self.arcs.push((a, b, fact));
        }
        if back {
            self.arcs.push((b, a, fact | REV));
        }
    }

    /// Straight connector a -> b walked on element `elem` (zone attachment).
    fn attach(&mut self, a: u32, b: u32, elem: u32) {
        let (pa, pb) = (self.g.nodes[a as usize].xyz, self.g.nodes[b as usize].xyz);
        // Connector across a short gap; z differences here are data offsets, not slopes.
        let l = dist3(pa, pb);
        let st = WalkStats {
            len: l,
            ..Default::default()
        };
        let piece = Piece {
            elem,
            len: l as f32,
            climb: 0.0,
            descent: 0.0,
        };
        let lvl = self.g.nodes[a as usize].level;
        let f = self.fact(
            FactKind::Attach,
            NONE,
            NONE,
            lvl,
            &[piece],
            &[pa, pb],
            &[],
            st,
        );
        self.link(a, b, f, true, true);
    }
}

fn grade_stats(l2: f64, dz: f64) -> WalkStats {
    let g = if l2 > 1e-6 {
        dz / l2
    } else if dz.abs() > 1e-6 {
        f64::INFINITY * dz.signum()
    } else {
        0.0
    };
    WalkStats {
        len: l2,
        max_up: g.max(0.0),
        max_down: (-g).max(0.0),
    }
}

fn f32o(v: Option<f64>) -> f32 {
    v.map(|x| x as f32).unwrap_or(f32::NAN)
}

fn vz(v: &Vtx, ez: Option<f64>, lz: f64) -> P3 {
    let x = v.first().copied().unwrap_or(0.0);
    let y = v.get(1).copied().unwrap_or(0.0);
    [x, y, v.get(2).copied().or(ez).unwrap_or(lz)]
}

struct Uf(Vec<u32>);
impl Uf {
    fn find(&mut self, mut x: u32) -> u32 {
        while self.0[x as usize] != x {
            let p = self.0[self.0[x as usize] as usize];
            self.0[x as usize] = p;
            x = p;
        }
        x
    }
    fn union(&mut self, a: u32, b: u32) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[b as usize] = a;
        }
    }
}

struct WalkRes {
    i: usize,
    j: usize,
    a: u32,
    b: u32,
    pieces: Vec<Piece>,
    geom: Vec<P3>,
    chain: Vec<u32>,
    st: WalkStats,
}

impl WalkRes {
    fn new(
        nodes: &[u32],
        i: usize,
        j: usize,
        chain: Vec<u32>,
        st: WalkStats,
        wb: &WalkBuf,
    ) -> Self {
        WalkRes {
            i,
            j,
            a: nodes[i],
            b: nodes[j],
            pieces: wb.pieces.clone(),
            geom: wb.geom.clone(),
            chain,
            st,
        }
    }
    /// Static weight: length x surface cost factor.
    fn weight(&self, g: &Graph) -> f32 {
        self.pieces
            .iter()
            .map(|p| p.len * g.elems[p.elem as usize].cost_factor)
            .sum()
    }
}

/// Walks between the nodes of zone `z`: all pairs in small zones; in zones with more than
/// `K_ZONE + 1` nodes each node keeps its `K_ZONE` cheapest walks (candidates gathered by a
/// triangle Dijkstra, at most `2 K_ZONE`, nearest first), then components are joined by their
/// cheapest connecting walks so the zone stays connected.
fn zone_walks(g: &Graph, z: u32, ts: &mut TriScratch, wb: &mut WalkBuf) -> Vec<WalkRes> {
    let nodes = g.zone_nodes(z);
    let n = nodes.len();
    let mut out: Vec<WalkRes> = vec![];
    if n < 2 {
        return out;
    }
    let tri_of = |k: usize| g.nodes[nodes[k] as usize].tri;
    let pos = |k: usize| g.nodes[nodes[k] as usize].xyz;
    let mut by_tri: HashMap<u32, Vec<usize>> = HashMap::new();
    for k in 0..n {
        by_tri.entry(tri_of(k)).or_default().push(k);
    }
    let w = |t: u32| g.mesh.tri_w[t as usize];
    let walk = |ts: &TriScratch, i: usize, j: usize, wb: &mut WalkBuf| -> Option<WalkRes> {
        let mut chain = vec![];
        if !ts.chain(tri_of(j), &mut chain) {
            return None;
        }
        let st = measure(&g.mesh, &chain, pos(i), pos(j), wb);
        Some(WalkRes::new(nodes, i, j, chain, st, wb))
    };
    if n <= K_ZONE + 1 {
        for i in 0..n - 1 {
            let mut left = n - 1 - i;
            ts.run(&g.mesh, tri_of(i), w, |t| {
                if let Some(list) = by_tri.get(&t) {
                    left -= list.iter().filter(|&&j| j > i).count();
                }
                left > 0
            });
            for j in i + 1..n {
                if let Some(r) = walk(ts, i, j, wb) {
                    out.push(r);
                }
            }
        }
        return out;
    }
    let cap = K_ZONE;
    let mut index: HashMap<(usize, usize), usize> = HashMap::new();
    let mut spare: HashMap<(usize, usize), WalkRes> = HashMap::new();
    let mut targets: Vec<usize> = vec![];
    let mut cands: Vec<(f32, usize, Option<WalkRes>)> = vec![];
    for i in 0..n {
        targets.clear();
        let pi = pos(i);
        ts.run(&g.mesh, tri_of(i), w, |t| {
            if let Some(list) = by_tri.get(&t) {
                let start = targets.len();
                targets.extend(list.iter().copied().filter(|&j| j != i));
                let room = cap - start;
                if targets.len() - start > room {
                    // A big triangle holding many nodes: its nearest ones only.
                    targets[start..].select_nth_unstable_by(room, |&x, &y| {
                        dist3(pos(x), pi).total_cmp(&dist3(pos(y), pi))
                    });
                    targets.truncate(start + room);
                }
            }
            targets.len() < K_ZONE
        });
        cands.clear();
        for &j in &targets {
            let key = (i.min(j), i.max(j));
            if let Some(&k) = index.get(&key) {
                cands.push((out[k].weight(g), j, None));
            } else if let Some(r) = spare.remove(&key).or_else(|| walk(ts, i, j, wb)) {
                cands.push((r.weight(g), j, Some(r)));
            }
        }
        cands.sort_by(|x, y| x.0.total_cmp(&y.0));
        for (k, (_, j, r)) in cands.drain(..).enumerate() {
            let Some(r) = r else { continue };
            let key = (i.min(j), i.max(j));
            if k < K_ZONE {
                index.insert(key, out.len());
                out.push(r);
            } else {
                // Measured but not among i's cheapest: j may still want it.
                spare.insert(key, r);
            }
        }
    }
    // Connectivity: join the smallest component to its nearest node elsewhere until one is left.
    let mut uf = Uf((0..n as u32).collect());
    for r in &out {
        uf.union(r.i as u32, r.j as u32);
    }
    let mut srcs: Vec<u32> = vec![];
    loop {
        let mut size: HashMap<u32, usize> = HashMap::new();
        for k in 0..n as u32 {
            *size.entry(uf.find(k)).or_default() += 1;
        }
        if size.len() <= 1 {
            break;
        }
        let (&small, _) = size.iter().min_by_key(|&(&r, &c)| (c, r)).unwrap();
        let comp: Vec<bool> = (0..n as u32).map(|k| uf.find(k) == small).collect();
        srcs.clear();
        srcs.extend((0..n).filter(|&k| comp[k]).map(tri_of));
        srcs.sort_unstable();
        srcs.dedup();
        let mut hit = None;
        ts.run_multi(&g.mesh, &srcs, w, |t| {
            if let Some(list) = by_tri.get(&t) {
                hit = list.iter().copied().find(|&j| !comp[j]);
            }
            hit.is_none()
        });
        let Some(j) = hit else { break };
        let mut chain = vec![];
        ts.chain(tri_of(j), &mut chain);
        let pj = pos(j);
        let i = by_tri[&chain[0]]
            .iter()
            .copied()
            .filter(|&k| comp[k])
            .min_by(|&x, &y| dist3(pos(x), pj).total_cmp(&dist3(pos(y), pj)))
            .unwrap();
        let st = measure(&g.mesh, &chain, pos(i), pj, wb);
        out.push(WalkRes::new(nodes, i, j, chain, st, wb));
        uf.union(i as u32, j as u32);
    }
    out
}

/// Drop walks dominated by a two-walk path through another node of the zone (within 1% +
/// 5 cm), longest first and only via walks still kept, so connectivity is preserved. The
/// witness must also be no worse for any profile: it walks only surfaces of the direct walk
/// (or ones no profile tells apart from them), each at most 1% + 5 cm longer, with no steeper
/// grade and no more climb or descent.
fn prune(g: &Graph, sig: &[u32], nodes: &[u32], out: &mut Vec<WalkRes>) {
    let n = nodes.len();
    if n < 3 || out.len() < 3 {
        return;
    }
    let w: Vec<f32> = out.iter().map(|r| r.weight(g)).collect();
    // Per walk (flat, offsets in `eoff`): (surface signature, length) sorted by signature;
    // climb / descent (i -> j).
    let mut elen: Vec<(u32, f32)> = Vec::with_capacity(out.len());
    let mut eoff: Vec<u32> = Vec::with_capacity(out.len() + 1);
    let mut climb: Vec<[f32; 2]> = Vec::with_capacity(out.len());
    eoff.push(0);
    for r in out.iter() {
        let s0 = elen.len();
        let mut c = [0.0f32; 2];
        for p in &r.pieces {
            elen.push((sig[p.elem as usize], p.len));
            c[0] += p.climb;
            c[1] += p.descent;
        }
        let v = &mut elen[s0..];
        v.sort_unstable_by_key(|x| x.0);
        // Merge equal signatures in place.
        let mut m = s0;
        for k in s0..elen.len() {
            if k > s0 && elen[k].0 == elen[m - 1].0 {
                elen[m - 1].1 += elen[k].1;
            } else {
                elen[m] = elen[k];
                m += 1;
            }
        }
        elen.truncate(m);
        eoff.push(elen.len() as u32);
        climb.push(c);
    }
    let per = |k: usize| &elen[eoff[k] as usize..eoff[k + 1] as usize];
    // Adjacency for big zones (CSR, neighbours sorted): (neighbour, walk).
    let mut aoff = vec![0u32; n + 1];
    let mut adjv = vec![];
    if n > 2 * K_ZONE {
        for r in out.iter() {
            aoff[r.i + 1] += 1;
            aoff[r.j + 1] += 1;
        }
        for k in 1..=n {
            aoff[k] += aoff[k - 1];
        }
        let mut fillp = aoff.clone();
        adjv = vec![(0u32, 0u32); 2 * out.len()];
        for (k, r) in out.iter().enumerate() {
            for (a, b) in [(r.i, r.j), (r.j, r.i)] {
                adjv[fillp[a] as usize] = (b as u32, k as u32);
                fillp[a] += 1;
            }
        }
        for k in 0..n {
            adjv[aoff[k] as usize..aoff[k + 1] as usize].sort_unstable();
        }
    }
    let adj = |v: usize| &adjv[aoff[v] as usize..aoff[v + 1] as usize];
    let witness_ok = |k: usize, k1: usize, k2: usize, i: usize| -> bool {
        let grade = |k: usize| out[k].st.max_up.max(out[k].st.max_down);
        if grade(k1).max(grade(k2)) > grade(k) + 1e-6 {
            return false;
        }
        // Climb / descent along i -> v -> j.
        let along = |k: usize, from: usize| {
            if out[k].i == from {
                climb[k]
            } else {
                [climb[k][1], climb[k][0]]
            }
        };
        let v = if out[k1].i == i { out[k1].j } else { out[k1].i };
        let (c1, c2, d) = (along(k1, i), along(k2, v), along(k, i));
        if (0..2).any(|x| c1[x] + c2[x] > d[x] * 1.01 + 0.05) {
            return false;
        }
        // Per signature: witness length (both walks) within 1% + 5 cm of the direct one.
        let (direct, p1, p2) = (per(k), per(k1), per(k2));
        let len_in = |p: &[(u32, f32)], e: u32| {
            p.binary_search_by_key(&e, |y| y.0)
                .map(|y| p[y].1)
                .unwrap_or(0.0)
        };
        p1.iter().chain(p2).all(|&(e, _)| {
            let l = len_in(p1, e) + len_in(p2, e);
            match direct.binary_search_by_key(&e, |y| y.0) {
                Ok(y) => l <= direct[y].1 * 1.01 + 0.05,
                Err(_) => false,
            }
        })
    };
    let mut order: Vec<usize> = (0..out.len()).collect();
    order.sort_by(|&x, &y| w[y].total_cmp(&w[x]));
    let mut keep = vec![true; out.len()];
    let free = |v: usize| g.nodes[nodes[v] as usize].elem == NONE;
    if n <= 2 * K_ZONE {
        // Small zones: dense matrices (weight of the kept walk i-v, INFINITY if none).
        let mut m = vec![f32::INFINITY; n * n];
        let mut idx = vec![NONE; n * n];
        for (k, r) in out.iter().enumerate() {
            for (a, b) in [(r.i, r.j), (r.j, r.i)] {
                m[a * n + b] = w[k];
                idx[a * n + b] = k as u32;
            }
        }
        for k in order {
            let (i, j) = (out[k].i, out[k].j);
            let lim = w[k] * 1.01 + 0.05;
            let dominated = (0..n).any(|v| {
                m[i * n + v] + m[v * n + j] <= lim
                    && v != i
                    && v != j
                    && free(v)
                    && witness_ok(k, idx[i * n + v] as usize, idx[v * n + j] as usize, i)
            });
            if dominated {
                keep[k] = false;
                m[i * n + j] = f32::INFINITY;
                m[j * n + i] = f32::INFINITY;
            }
        }
    } else {
        // Big zones: merge the sorted neighbour lists of i and j.
        for k in order {
            let (i, j) = (out[k].i, out[k].j);
            let lim = w[k] * 1.01 + 0.05;
            let (ai, aj) = (adj(i), adj(j));
            let (mut x, mut y) = (0, 0);
            let mut dominated = false;
            while x < ai.len() && y < aj.len() && !dominated {
                let (vi, vj) = (ai[x].0, aj[y].0);
                if vi < vj {
                    x += 1;
                } else if vj < vi {
                    y += 1;
                } else {
                    let (k1, k2) = (ai[x].1 as usize, aj[y].1 as usize);
                    dominated = keep[k1]
                        && keep[k2]
                        && free(vi as usize)
                        && w[k1] + w[k2] <= lim
                        && witness_ok(k, k1, k2, i);
                    x += 1;
                    y += 1;
                }
            }
            if dominated {
                keep[k] = false;
            }
        }
    }
    let mut k = 0;
    out.retain(|_| {
        k += 1;
        keep[k - 1]
    });
}

type Hit = Option<(u32, P2, f64)>;

/// The two sides of a portal centred at `c` on level `lvl`: (score, normal, hit along the
/// normal, hit against it). Score < 100: two zones (or the two named spaces); 100..1000: one
/// side only; >= 1000: both sides in one zone; 1e9: nothing walkable. Read-only, so portals
/// are searched in parallel.
fn portal_sides(
    g: &Graph,
    lvl: u32,
    c: P2,
    given: Option<P2>,
    spaces: &[String],
    max_d: f64,
) -> (f64, P2, Hit, Hit) {
    let in_space = |t: u32, sp: &str| -> bool {
        let e = &g.elems[g.mesh.tri_surf[t as usize] as usize];
        sp == e.id || (e.space != NONE && g.strings[e.space as usize] == sp)
    };
    let tri_ok = |t: u32| spaces.is_empty() || spaces.iter().any(|s| in_space(t, s));
    let zone_of = |h: &Hit| h.map(|x| g.mesh.tri_zone[x.0 as usize]);
    // First triangle along `dir` accepted by `ok` (probing past rejected ones up to max_d).
    // Triangle at q, trying the previous step's triangle first.
    let tri_near = |last: &mut u32, q: P2| -> Option<u32> {
        if *last != NONE && g.mesh.contains(*last, q, 1e-9) {
            return Some(*last);
        }
        let t = g.tri_at(lvl, q);
        *last = t.unwrap_or(NONE);
        t
    };
    let probe = |dir: P2, ok: &dyn Fn(u32) -> bool| -> Hit {
        let (mut d, mut last) = (0.05, NONE);
        while d <= max_d + 1e-9 {
            let q = add(c, scale(dir, d));
            if let Some(t) = tri_near(&mut last, q)
                && ok(t)
            {
                return Some((t, q, d));
            }
            d += 0.025;
        }
        None
    };
    let two = spaces.len() >= 2 && spaces[0] != spaces[1];
    // With two named spaces both sides are always one in each of them.
    let score = |s: &(Hit, Hit)| -> f64 {
        match s {
            (Some(a), Some(bb)) if two || zone_of(&s.0) != zone_of(&s.1) => a.2 + bb.2,
            // One side only (exterior door: the other end dangles) beats both sides in
            // one zone, which would make the portal a useless loop.
            (Some(a), None) | (None, Some(a)) => 100.0 + a.2,
            (Some(a), Some(bb)) => 1000.0 + a.2 + bb.2,
            _ => 1e9,
        }
    };
    // Sides along +n / -n: different zones if any (a door segment drawn inside one room
    // keeps probing through it); with two named spaces, one side in each.
    let sides = |n: P2| -> (Hit, Hit) {
        let m = scale(n, -1.0);
        if two {
            // Usually the nearest triangles on the two sides are one in each space; if
            // both are in the same one, look further for the other space on either side.
            let (fp, fm) = (probe(n, &tri_ok), probe(m, &tri_ok));
            let (Some(a), Some(bb)) = (fp, fm) else {
                return (fp, fm);
            };
            let k = in_space(a.0, &spaces[1]) as usize;
            if in_space(bb.0, &spaces[k]) == in_space(a.0, &spaces[k]) {
                let other = &spaces[1 - k];
                let alt = [
                    (probe(n, &|t| in_space(t, other)), fm),
                    (fp, probe(m, &|t| in_space(t, other))),
                ];
                return alt
                    .into_iter()
                    .filter(|x| x.0.is_some() && x.1.is_some())
                    .min_by(|x, y| score(x).total_cmp(&score(y)))
                    .unwrap_or((fp, None));
            }
            return (fp, fm);
        }
        let (fp, fm) = (probe(n, &tri_ok), probe(m, &tri_ok));
        let (zp, zm) = (zone_of(&fp), zone_of(&fm));
        if fp.is_none() || fm.is_none() || zp != zm {
            return (fp, fm);
        }
        let other =
            |z: Option<u32>| move |t: u32| tri_ok(t) && Some(g.mesh.tri_zone[t as usize]) != z;
        [
            (fp, probe(m, &other(zp))),
            (probe(n, &other(zm)), fm),
            (fp, fm),
        ]
        .into_iter()
        .min_by(|x, y| score(x).total_cmp(&score(y)))
        .unwrap()
    };
    let around: Vec<P2> = (0..12)
        .map(|k| (k as f64 * std::f64::consts::PI / 12.0).sin_cos())
        .map(|(s, c)| [c, s])
        .collect();
    let mut best: Option<(f64, P2, Hit, Hit)> = None;
    // The given normal first; if it does not reach two zones (a door placed at a room
    // corner, a normal along a thin room), the 12 directions are tried as well. Those keep
    // the side of the given normal (oneway semantics).
    for (k, mut n) in given.iter().chain(around.iter()).copied().enumerate() {
        if k == 1 && given.is_some() && best.as_ref().is_some_and(|bb| bb.0 < 100.0) {
            break;
        }
        if let (true, Some(gn)) = (k > 0, given)
            && dot(n, gn) < 0.0
        {
            n = scale(n, -1.0);
        }
        let (fp, fm) = sides(n);
        let sc = score(&(fp, fm));
        if best.as_ref().is_none_or(|bb| sc < bb.0) {
            best = Some((sc, n, fp, fm));
        }
    }
    best.unwrap()
}

/// A POI's access list (`at` + `level` alone = one point access).
fn poi_access(p: &crate::input::Poi) -> Vec<crate::input::Access> {
    if !p.access.is_empty() {
        return p.access.clone();
    }
    match &p.at {
        Some(at) => vec![crate::input::Access {
            at: Some(at.clone()),
            level: p.level.clone(),
            ..Default::default()
        }],
        None => vec![],
    }
}

/// Compile an input into a routable graph. Never fails: problems become warnings.
pub fn compile(input: &Input) -> (Graph, CompileReport) {
    let t0 = Instant::now();
    let mut timings = vec![];
    let mut lap = |name: &str, t: &mut Instant| {
        timings.push((name.to_string(), t.elapsed().as_secs_f64() * 1e3));
        *t = Instant::now();
    };
    let mut tl = Instant::now();
    let snap = input.defaults.snap.max(1e-6);
    let attach_d = input.defaults.edge_attach.max(0.0);
    let mut b = B {
        g: Graph::default(),
        arcs: vec![],
        warnings: vec![],
        strings: HashMap::new(),
        level_map: HashMap::new(),
        access: HashMap::new(),
        access_overflow: vec![],
    };
    for l in &input.levels {
        if !b.level_map.contains_key(&l.id) {
            b.add_level(&l.id, l.z, props_str(&l.props));
        }
    }

    // 1. Surfaces and barriers grouped by level.
    let mut level_surfs: Vec<Vec<SurfIn>> = vec![];
    let mut level_bars: Vec<Vec<Vec<P3>>> = vec![];
    let mut open_surf: Vec<bool> = vec![];
    let mut surf_outer: Vec<Vec<Vec<P2>>> = vec![];
    for (i, s) in input.surfaces.iter().enumerate() {
        let polys: Vec<&Vec<Vec<Vtx>>> = match (&s.polygon, &s.multipolygon) {
            (Some(p), _) => vec![p],
            (None, Some(m)) => m.iter().collect(),
            _ => {
                let id = gen_id(&s.id, "surface", i);
                b.warn("surface", Some(&id), "no polygon".into(), None, NONE);
                continue;
            }
        };
        let z0 = s.z.or_else(|| {
            polys
                .first()
                .and_then(|p| p.first())
                .and_then(|r| r.first())
                .and_then(|v| v.get(2).copied())
        });
        let lvl = b.level(s.level.as_deref(), z0);
        let lz = b.lz(lvl);
        let polys: Vec<Vec<Vec<P3>>> = polys
            .iter()
            .map(|p| {
                p.iter()
                    .map(|r| r.iter().map(|v| vz(v, s.z, lz)).collect())
                    .collect()
            })
            .collect();
        let (class, tags, space) = (b.os(&s.class), b.tags(&s.tags), b.os(&s.space));
        let access = b.access(&s.access_groups);
        let elem = b.elem(Elem {
            kind: ElemKind::Surface,
            id: gen_id(&s.id, "surface", i),
            class,
            sub: NONE,
            tags,
            space,
            level: lvl,
            cost_factor: s.cost_factor as f32,
            speed: f32o(s.speed),
            time: f32::NAN,
            penalty: 0.0,
            width: f32::NAN,
            step_free: true,
            wait: 0.0,
            props: props_str(&s.props),
            access,
        });
        while level_surfs.len() <= lvl as usize {
            level_surfs.push(vec![]);
        }
        open_surf.resize(elem as usize + 1, false);
        open_surf[elem as usize] = !s.walls;
        surf_outer.resize(elem as usize + 1, vec![]);
        surf_outer[elem as usize] = polys
            .iter()
            .filter_map(|p| p.first())
            .map(|r| r.iter().map(|v| xy(*v)).collect())
            .collect();
        level_surfs[lvl as usize].push(SurfIn {
            elem,
            polys,
            open: !s.walls,
            space,
        });
    }
    for br in &input.barriers {
        let z0 =
            br.z.or_else(|| br.line.first().and_then(|v| v.get(2).copied()));
        let lvl = b.level(br.level.as_deref(), z0);
        while level_bars.len() <= lvl as usize {
            level_bars.push(vec![]);
        }
        // Barriers carry no height of their own: vertices without z take it from the surface
        // they cut (NaN here), never from the level.
        level_bars[lvl as usize].push(br.line.iter().map(|v| vz(v, br.z, f64::NAN)).collect());
    }
    let nlev = b.g.levels.len();
    level_surfs.resize_with(nlev, Vec::new);
    level_bars.resize_with(nlev, Vec::new);

    // 2. Mesh per level (parallel).
    let lzs: Vec<f64> = b.g.levels.iter().map(|l| l.z).collect();
    let meshes: Vec<LevelMesh> = (0..nlev)
        .into_par_iter()
        .map(|l| {
            if level_surfs[l].is_empty() {
                LevelMesh::default()
            } else {
                build_level(&level_surfs[l], &level_bars[l], snap, lzs[l])
            }
        })
        .collect();
    lap("mesh", &mut tl);
    {
        let g = &mut b.g;
        let m = &mut g.mesh;
        for (l, lm) in meshes.into_iter().enumerate() {
            let (v0, t0) = (m.verts.len() as u32, m.tri_v.len() as u32);
            for w in lm.warnings {
                b.warnings.push(Warning {
                    id: (w.elem != NONE).then(|| g.elems[w.elem as usize].id.clone()),
                    kind: if w.elem != NONE { "surface" } else { "mesh" },
                    msg: w.msg,
                    at: w.at,
                    level: Some(g.levels[l].id.clone()),
                });
            }
            m.verts.extend(lm.verts);
            m.wall.extend(lm.wall);
            for (k, tri) in lm.tris.iter().enumerate() {
                m.tri_v.push(tri.map(|v| v + v0));
                m.tri_z.push(lm.tz[k]);
                m.tri_nbr
                    .push(lm.nbr[k].map(|n| if n == NONE { NONE } else { n + t0 }));
                m.tri_surf.push(lm.surf[k]);
                m.tri_level.push(l as u32);
                m.tri_w
                    .push(g.elems[lm.surf[k] as usize].cost_factor.max(0.01));
            }
            g.level_tri.push([t0, m.tri_v.len() as u32]);
        }
        m.tri_slope = (0..m.tri_v.len() as u32)
            .map(|t| {
                let [a, b, c] = m.pts3(t);
                let (u, w) = (
                    [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
                    [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
                );
                let n = [
                    u[1] * w[2] - u[2] * w[1],
                    u[2] * w[0] - u[0] * w[2],
                    u[0] * w[1] - u[1] * w[0],
                ];
                (n[0].hypot(n[1]) / n[2].abs().max(1e-12)) as f32
            })
            .collect();
        m.tri_c = (0..m.tri_v.len() as u32)
            .map(|t| {
                let [a, b, c] = m.pts(t);
                [(a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0]
            })
            .collect();
        // Zones: connected components over passable edges.
        let nt = m.tri_v.len();
        m.tri_zone = vec![NONE; nt];
        let mut stack = vec![];
        for s in 0..nt {
            if m.tri_zone[s] != NONE {
                continue;
            }
            let z = g.zone_level.len() as u32;
            g.zone_level.push(m.tri_level[s]);
            m.tri_zone[s] = z;
            stack.push(s as u32);
            while let Some(t) = stack.pop() {
                for &n in &m.tri_nbr[t as usize] {
                    if n != NONE && m.tri_zone[n as usize] == NONE {
                        m.tri_zone[n as usize] = z;
                        stack.push(n);
                    }
                }
            }
        }
        g.tri_grid = g
            .level_tri
            .par_iter()
            .map(|r| {
                let boxes: Vec<[f64; 4]> = (r[0]..r[1]).map(|t| bbox(g.mesh.pts(t))).collect();
                Grid::build(&boxes)
            })
            .collect();
        // Space / surface id -> largest triangle.
        let mut best: HashMap<String, (u32, f64)> = HashMap::new();
        for t in 0..nt as u32 {
            let e = &g.elems[g.mesh.tri_surf[t as usize] as usize];
            let a = g.mesh.area(t);
            let mut keys = vec![e.id.clone()];
            if e.space != NONE {
                keys.push(g.strings[e.space as usize].clone());
            }
            for k in keys {
                let ent = best.entry(k).or_insert((t, -1.0));
                if a > ent.1 {
                    *ent = (t, a);
                }
            }
        }
        g.space_tri = best.into_iter().map(|(k, v)| (k, v.0)).collect();
    }
    lap("zones", &mut tl);

    // 3. Portals: elements (serial), side search (parallel), nodes and facts (serial).
    struct PortalJob {
        i: usize,
        id: String,
        lvl: u32,
        c: P2,
        given: Option<P2>,
        pz: Option<f64>,
        elem: u32,
    }
    let mut jobs: Vec<PortalJob> = vec![];
    for (i, p) in input.portals.iter().enumerate() {
        let id = gen_id(&p.id, "portal", i);
        let pz = p.z.or_else(|| {
            p.segment
                .as_ref()
                .and_then(|s| s.first())
                .or(p.at.as_ref())
                .and_then(|v| v.get(2).copied())
        });
        let lvl = b.level(p.level.as_deref(), pz);
        let (c, tdir, seg_w) = match (&p.segment, &p.at) {
            (Some(s), _) if s.len() >= 2 => {
                let (a, bb) = ([s[0][0], s[0][1]], [s[1][0], s[1][1]]);
                (lerp(a, bb, 0.5), Some(norm(sub(bb, a))), Some(dist(a, bb)))
            }
            (_, Some(at)) if at.len() >= 2 => ([at[0], at[1]], None, None),
            _ => {
                b.warn("portal", Some(&id), "needs segment or at".into(), None, lvl);
                continue;
            }
        };
        let width = p.width.or(seg_w).unwrap_or(0.9);
        let (kind, tags) = (b.s(p.kind.as_deref().unwrap_or("door")), b.tags(&p.tags));
        let access = b.access(&p.access_groups);
        let elem = b.elem(Elem {
            kind: ElemKind::Portal,
            id: id.clone(),
            class: NONE,
            sub: kind,
            tags,
            space: NONE,
            level: lvl,
            cost_factor: 1.0,
            speed: f32::NAN,
            time: f32::NAN,
            penalty: p.penalty as f32,
            width: width as f32,
            step_free: p.step_free,
            wait: 0.0,
            props: props_str(&p.props),
            access,
        });
        let given: Option<P2> = match (&p.normal, tdir) {
            (Some(n), _) if n.len() >= 2 => Some(norm([n[0], n[1]])),
            (_, Some(t)) => Some([t[1], -t[0]]),
            _ => None,
        };
        jobs.push(PortalJob {
            i,
            id,
            lvl,
            c,
            given,
            pz,
            elem,
        });
    }
    let found: Vec<(f64, P2, Hit, Hit)> = jobs
        .par_iter()
        .map(|j| {
            let spaces = &input.portals[j.i].spaces;
            portal_sides(&b.g, j.lvl, j.c, j.given, spaces, 0.5 + snap)
        })
        .collect();
    let mut dangling: Vec<u32> = vec![];
    let mut dangling_elem: Vec<u32> = vec![];
    for (job, (score, n, fp, fm)) in jobs.into_iter().zip(found) {
        let PortalJob {
            i,
            id,
            lvl,
            c,
            pz,
            elem,
            ..
        } = job;
        let p = &input.portals[i];
        let two = p.spaces.len() >= 2 && p.spaces[0] != p.spaces[1];
        if score >= 1e9 {
            b.warn(
                "portal",
                Some(&id),
                "no walkable side found".into(),
                Some(c),
                lvl,
            );
            continue;
        }
        if (100.0..1000.0).contains(&score) && two {
            b.warn(
                "portal",
                Some(&id),
                "one side not found".into(),
                Some(c),
                lvl,
            );
        } else if score >= 1000.0 {
            b.warn(
                "portal",
                Some(&id),
                "both sides in the same zone".into(),
                Some(c),
                lvl,
            );
        }
        let mut side = |b: &mut B, hit: Option<(u32, P2, f64)>, dir: P2| -> u32 {
            match hit {
                Some((t, q, _)) => {
                    let z = b.g.mesh.z_at(t, q);
                    let zone = b.g.mesh.tri_zone[t as usize];
                    b.node([q[0], q[1], z], lvl, zone, t, NONE, dir)
                }
                None => {
                    let q = add(c, scale(dir, 0.05));
                    let z = pz.unwrap_or(b.lz(lvl));
                    let nd = b.node([q[0], q[1], z], lvl, NONE, NONE, NONE, dir);
                    dangling.push(nd);
                    dangling_elem.push(elem);
                    nd
                }
            }
        };
        let plus = side(&mut b, fp, n);
        let minus = side(&mut b, fm, scale(n, -1.0));
        let (pa, pb) = (b.g.nodes[minus as usize].xyz, b.g.nodes[plus as usize].xyz);
        let l = dist(xy(pa), xy(pb));
        // Height differences under 0.1 m across a door are data offsets, not slopes.
        let dz = if (pb[2] - pa[2]).abs() < 0.1 {
            0.0
        } else {
            pb[2] - pa[2]
        };
        let piece = Piece {
            elem,
            len: l as f32,
            climb: dz.max(0.0) as f32,
            descent: (-dz).max(0.0) as f32,
        };
        let f = b.fact(
            FactKind::Portal,
            elem,
            NONE,
            lvl,
            &[piece],
            &[pa, pb],
            &[],
            grade_stats(l.max(0.5), dz),
        );
        b.g.facts[f as usize].pen = p.penalty as f32;
        let ow = p.oneway.as_deref().unwrap_or("");
        b.link(minus, plus, f, ow != "backward", ow != "forward");
        b.g.portals.insert(id, [minus, plus]);
    }
    lap("portals", &mut tl);

    // 4. Edges: cluster vertices (snap / shared ids), split at shared vertices.
    struct EV {
        p: P3,
        level: u32,
        /// Level given explicitly (else derived from z; joins any level at that z).
        explicit: bool,
        end: bool,
    }
    let mut evs: Vec<EV> = vec![];
    let mut edge_v0: Vec<usize> = vec![];
    let mut names: Vec<(String, usize)> = vec![];
    let mut edge_elem: Vec<u32> = vec![];
    for (i, e) in input.edges.iter().enumerate() {
        edge_v0.push(evs.len());
        let elvl = e.level.as_deref().map(|l| b.level(Some(l), e.z));
        let lz = elvl.map(|l| b.lz(l)).unwrap_or(0.0);
        let n = e.line.len();
        for (k, v) in e.line.iter().enumerate() {
            let p = vz(v, e.z, lz);
            let level = elvl.unwrap_or_else(|| b.level_near(p[2]));
            evs.push(EV {
                p,
                level,
                explicit: elvl.is_some(),
                end: k == 0 || k + 1 == n,
            });
        }
        if n >= 2 {
            if let Some(f) = &e.from {
                names.push((f.clone(), edge_v0[i]));
            }
            if let Some(t) = &e.to {
                names.push((t.clone(), edge_v0[i] + n - 1));
            }
        }
        let (class, tags) = (b.os(&e.class), b.tags(&e.tags));
        let access = b.access(&e.access_groups);
        edge_elem.push(b.elem(Elem {
            kind: ElemKind::Edge,
            id: gen_id(&e.id, "edge", i),
            class,
            sub: NONE,
            tags,
            space: NONE,
            level: elvl.unwrap_or(NONE),
            cost_factor: e.cost_factor as f32,
            speed: f32o(e.speed),
            time: f32o(e.time),
            penalty: e.penalty as f32,
            width: f32o(e.width),
            step_free: e.step_free,
            wait: 0.0,
            props: props_str(&e.props),
            access,
        }));
    }
    edge_v0.push(evs.len());
    // POI access points given as a point or a space become nodes named `{poi}@{k}`.
    let mut synth: Vec<InNode> = vec![];
    for (i, p) in input.pois.iter().enumerate() {
        let pid = gen_id(&p.id, "poi", i);
        for (k, a) in poi_access(p).iter().enumerate() {
            let at = match (&a.at, &a.space) {
                (Some(at), _) => Some((at.clone(), a.level.clone())),
                (None, Some(sp)) => b.g.space_tri.get(sp).map(|&t| {
                    let c = b.g.mesh.tri_c[t as usize];
                    let lvl = b.g.levels[b.g.mesh.tri_level[t as usize] as usize]
                        .id
                        .clone();
                    (vec![c[0], c[1], b.g.mesh.z_at(t, c)], Some(lvl))
                }),
                _ => None,
            };
            if let Some((at, level)) = at {
                synth.push(InNode {
                    id: Some(format!("{pid}@{k}")),
                    at: Some(at),
                    level,
                    ..Default::default()
                });
            }
        }
    }
    let all_nodes: Vec<&InNode> = input.nodes.iter().chain(synth.iter()).collect();
    let mut node_elem: Vec<(usize, u32)> = vec![];
    for (i, n) in all_nodes.iter().enumerate() {
        let id = gen_id(&n.id, "node", i);
        let tags = b.tags(&n.tags);
        let access = b.access(&n.access_groups);
        let lvl = n.level.as_deref().map(|l| b.level(Some(l), None));
        let elem = b.elem(Elem {
            kind: ElemKind::Node,
            id: id.clone(),
            class: NONE,
            sub: NONE,
            tags,
            space: NONE,
            level: lvl.unwrap_or(NONE),
            cost_factor: 1.0,
            speed: f32::NAN,
            time: f32::NAN,
            penalty: n.penalty as f32,
            width: f32::NAN,
            step_free: true,
            wait: 0.0,
            props: props_str(&n.props),
            access,
        });
        if let Some(at) = &n.at {
            let p = vz(at, None, lvl.map(|l| b.lz(l)).unwrap_or(0.0));
            let level = lvl.unwrap_or_else(|| b.level_near(p[2]));
            names.push((id.clone(), evs.len()));
            node_elem.push((evs.len(), elem));
            evs.push(EV {
                p,
                level,
                explicit: lvl.is_some(),
                end: true,
            });
        } else {
            // Position comes from the edges naming it.
            node_elem.push((usize::MAX, elem));
            names.push((id.clone(), usize::MAX));
        }
    }
    // Coordinate clusters: within snap, |dz| <= 0.5, and the same level unless one side's level
    // is only derived from z. A cluster takes the explicit level of any member.
    let mut cluster = vec![0u32; evs.len()];
    let mut reps: Vec<usize> = vec![];
    let mut cl_level: Vec<(u32, bool)> = vec![];
    {
        let mut cells: HashMap<(i64, i64), Vec<u32>> = HashMap::new();
        for (k, v) in evs.iter().enumerate() {
            let (cx, cy) = (
                (v.p[0] / snap).floor() as i64,
                (v.p[1] / snap).floor() as i64,
            );
            let mut found = NONE;
            'o: for dx in -1..=1 {
                for dy in -1..=1 {
                    if let Some(list) = cells.get(&(cx + dx, cy + dy)) {
                        for &c in list {
                            let q = evs[reps[c as usize]].p;
                            let (cl, cx_) = cl_level[c as usize];
                            let lvl_ok = !(v.explicit && cx_) || cl == v.level;
                            if lvl_ok
                                && (q[0] - v.p[0]).hypot(q[1] - v.p[1]) <= snap
                                && (q[2] - v.p[2]).abs() <= 0.5
                            {
                                found = c;
                                break 'o;
                            }
                        }
                    }
                }
            }
            if found == NONE {
                found = reps.len() as u32;
                reps.push(k);
                cl_level.push((v.level, v.explicit));
                cells.entry((cx, cy)).or_default().push(found);
            } else if v.explicit && !cl_level[found as usize].1 {
                cl_level[found as usize] = (v.level, true);
            }
            cluster[k] = found;
        }
    }
    let mut uf = Uf((0..reps.len() as u32).collect());
    let mut name_cluster: HashMap<String, u32> = HashMap::new();
    for (name, k) in &names {
        if *k == usize::MAX {
            continue;
        }
        let c = cluster[*k];
        match name_cluster.get(name) {
            Some(&o) => uf.union(o, c),
            None => {
                name_cluster.insert(name.clone(), c);
            }
        }
    }
    let mut count = vec![0u32; reps.len()];
    let mut is_node = vec![false; reps.len()];
    for (k, v) in evs.iter().enumerate() {
        let r = uf.find(cluster[k]) as usize;
        count[r] += 1;
        if v.end {
            is_node[r] = true;
        }
    }
    let mut cnode = vec![NONE; reps.len()];
    let mut edge_nodes: Vec<u32> = vec![];
    let mut derived: Vec<u32> = vec![];
    let mut get_node = |b: &mut B, k: usize, uf: &mut Uf| -> u32 {
        let r = uf.find(cluster[k]) as usize;
        if cnode[r] == NONE {
            let v = &evs[reps[r]];
            cnode[r] = b.node(v.p, cl_level[r].0, NONE, NONE, NONE, [0.0, 0.0]);
            if !cl_level[r].1 {
                derived.push(cnode[r]);
            }
            edge_nodes.push(cnode[r]);
        }
        cnode[r]
    };
    // Edge element of each edge node (first edge reaching it), for warnings.
    let mut node_edge: Vec<u32> = vec![];
    for (i, e) in input.edges.iter().enumerate() {
        let (v0, v1) = (edge_v0[i], edge_v0[i + 1]);
        if v1 - v0 < 2 {
            let id = gen_id(&e.id, "edge", i);
            let at = e
                .line
                .first()
                .filter(|v| v.len() >= 2)
                .map(|v| [v[0], v[1]]);
            let lvl = if v1 > v0 { evs[v0].level } else { NONE };
            b.warn("edge", Some(&id), "needs >= 2 vertices".into(), at, lvl);
            continue;
        }
        let elem = edge_elem[i];
        let mut note = |n: u32| {
            if node_edge.len() <= n as usize {
                node_edge.resize(n as usize + 1, NONE);
            }
            if node_edge[n as usize] == NONE {
                node_edge[n as usize] = elem;
            }
        };
        let pts: Vec<P3> = evs[v0..v1].iter().map(|v| v.p).collect();
        let total = poly_len3(&pts).max(1e-12);
        let mut start = 0usize;
        let mut a = get_node(&mut b, v0, &mut uf);
        note(a);
        for k in 1..pts.len() {
            let r = uf.find(cluster[v0 + k]) as usize;
            if k + 1 < pts.len() && !(is_node[r] || count[r] > 1) {
                continue;
            }
            let bn = get_node(&mut b, v0 + k, &mut uf);
            note(bn);
            let mut geom: Vec<P3> = pts[start..=k].to_vec();
            geom[0] = b.g.nodes[a as usize].xyz;
            *geom.last_mut().unwrap() = b.g.nodes[bn as usize].xyz;
            let l2 = poly_len2(&geom);
            if l2 < 1e-9 && (geom[0][2] - geom[geom.len() - 1][2]).abs() < 1e-9 {
                start = k;
                a = bn;
                continue;
            }
            let (mut climb, mut descent, mut st) = (
                0.0,
                0.0,
                WalkStats {
                    len: l2,
                    ..Default::default()
                },
            );
            let len3 = poly_len3(&geom);
            let plen = len3;
            let rise = (geom[geom.len() - 1][2] - geom[0][2]).abs();
            if l2 < 0.05_f64.max(0.1 * rise) {
                // Vertical-only link (floor change in a line network): walked length, no grade.
            } else if let Some(inc) = e.incline {
                climb = inc.max(0.0) * l2;
                descent = (-inc).max(0.0) * l2;
                st.max_up = inc.max(0.0);
                st.max_down = (-inc).max(0.0);
            } else {
                for w in geom.windows(2) {
                    let s = grade_stats(dist(xy(w[0]), xy(w[1])), w[1][2] - w[0][2]);
                    let dz = w[1][2] - w[0][2];
                    climb += dz.max(0.0);
                    descent += (-dz).max(0.0);
                    st.max_up = st.max_up.max(s.max_up);
                    st.max_down = st.max_down.max(s.max_down);
                }
            }
            let piece = Piece {
                elem,
                len: plen as f32,
                climb: climb as f32,
                descent: descent as f32,
            };
            let (la, lb) = (b.g.nodes[a as usize].level, b.g.nodes[bn as usize].level);
            let f = b.fact(
                FactKind::Edge,
                elem,
                NONE,
                if la == lb { la } else { NONE },
                &[piece],
                &geom,
                &[],
                st,
            );
            let frac = (poly_len3(&geom) / total) as f32;
            let fe = &mut b.g.facts[f as usize];
            if let Some(t) = e.time {
                fe.time = t as f32 * frac;
            }
            fe.pen = e.penalty as f32 * frac;
            b.link(a, bn, f, true, !e.oneway);
            start = k;
            a = bn;
        }
    }
    for &(k, elem) in &node_elem {
        if k != usize::MAX {
            let n = get_node(&mut b, k, &mut uf);
            b.g.nodes[n as usize].elem = elem;
        }
    }
    for (name, k) in &names {
        let c = if *k == usize::MAX {
            name_cluster.get(name).copied()
        } else {
            Some(cluster[*k])
        };
        if let Some(c) = c {
            let r = uf.find(c) as usize;
            if cnode[r] != NONE {
                b.g.names.insert(name.clone(), cnode[r]);
            }
        }
    }
    for (k, (name, pos)) in names.iter().enumerate() {
        if *pos == usize::MAX && !b.g.names.contains_key(name) {
            b.warn(
                "node",
                Some(name),
                format!("no position (no `at` and not referenced by an edge) [{k}]"),
                None,
                NONE,
            );
        }
    }
    for (i, n) in all_nodes.iter().enumerate() {
        let id = gen_id(&n.id, "node", i);
        if let (None, Some(&nd)) = (&n.at, b.g.names.get(&id)) {
            b.g.nodes[nd as usize].elem = node_elem[i].1;
        }
    }
    // Attach edge nodes to zones on their level (or to dangling portal ends).
    let dgrid = Grid::build(
        &dangling
            .iter()
            .map(|&d| bbox([xy(b.g.nodes[d as usize].xyz)]))
            .collect::<Vec<_>>(),
    );
    let derived: HashSet<u32> = derived.into_iter().collect();
    // A node joined by id across explicit levels (e.g. an outdoor way's end and a connector
    // edge on a room level) keeps one level; it also attaches on each other member level.
    let mut extra: HashMap<u32, Vec<u32>> = HashMap::new();
    for (k, v) in evs.iter().enumerate() {
        let n = cnode[uf.find(cluster[k]) as usize];
        if v.explicit && n != NONE && v.level != b.g.nodes[n as usize].level {
            let ls = extra.entry(n).or_default();
            if !ls.contains(&v.level) {
                ls.push(v.level);
            }
        }
    }
    // Surfaces a node outside every triangle may attach to directly: open ones (walls=false),
    // surfaces within `snap` (on the boundary), and any surface whose outer ring contains the
    // node (it lies in a hole: a stairwell or atrium void). Walled rooms are otherwise entered through portals only; a node inside a
    // room (room link, entrance edge ending in the room) attaches to that room.
    let is_open = |b: &B, t: u32, p: P2| {
        let e = b.g.mesh.tri_surf[t as usize] as usize;
        open_surf[e]
            || b.g.mesh.contains(t, p, snap)
            || surf_outer[e]
                .iter()
                .any(|r| in_rings(p, std::slice::from_ref(r)))
    };
    for &n in &edge_nodes {
        let mut nd = b.g.nodes[n as usize].clone();
        let p = xy(nd.xyz);
        for &l in extra.get(&n).map(|v| v.as_slice()).unwrap_or(&[]) {
            let hit = match b.g.tri_at(l, p) {
                Some(t) => Some((t, p)),
                None => {
                    b.g.nearest_tri(l, p, attach_d, |t| is_open(&b, t, p))
                        .map(|(t, q, _)| (t, b.g.inset(t, q)))
                }
            };
            if let Some((t, q)) = hit {
                let z = b.g.mesh.z_at(t, q);
                let zone = b.g.mesh.tri_zone[t as usize];
                let an = b.node([q[0], q[1], z], l, zone, t, NONE, [0.0, 0.0]);
                let surf = b.g.mesh.tri_surf[t as usize];
                b.attach(n, an, surf);
            }
        }
        if derived.contains(&n) {
            // Level only derived from z: attach on whichever level within 0.6 m has a surface here.
            let mut best: Option<(f64, u32)> = None;
            for (l, info) in b.g.levels.iter().enumerate() {
                if (info.z - nd.xyz[2]).abs() > 0.6 {
                    continue;
                }
                let d = if b.g.tri_at(l as u32, p).is_some() {
                    0.0
                } else {
                    b.g.nearest_tri(l as u32, p, attach_d, |_| true)
                        .map(|x| x.2)
                        .unwrap_or(f64::INFINITY)
                };
                if d.is_finite() && best.is_none_or(|bb| d < bb.0) {
                    best = Some((d, l as u32));
                }
            }
            if let Some((_, l)) = best {
                nd.level = l;
                b.g.nodes[n as usize].level = l;
            }
        }
        if nd.level == NONE {
            continue;
        }
        if let Some(t) = b.g.tri_at(nd.level, p) {
            let z = b.g.mesh.z_at(t, p);
            let zone = b.g.mesh.tri_zone[t as usize];
            if (z - nd.xyz[2]).abs() <= 0.02 {
                let node = &mut b.g.nodes[n as usize];
                node.tri = t;
                node.zone = zone;
            } else {
                let an = b.node([p[0], p[1], z], nd.level, zone, t, NONE, [0.0, 0.0]);
                let surf = b.g.mesh.tri_surf[t as usize];
                b.attach(n, an, surf);
            }
            continue;
        }
        // Exterior door end (dangling portal side) nearby: enter through the door.
        let mut best = (NONE, attach_d);
        dgrid.visit(
            [
                p[0] - attach_d,
                p[1] - attach_d,
                p[0] + attach_d,
                p[1] + attach_d,
            ],
            |k| {
                let d = dangling[k as usize];
                let dn = &b.g.nodes[d as usize];
                let dd = dist(xy(dn.xyz), p);
                if dn.level == nd.level && dd <= best.1 {
                    best = (d, dd);
                }
            },
        );
        if best.0 != NONE {
            // The connector is walked as part of the portal.
            let k = dangling.iter().position(|&d| d == best.0).unwrap();
            b.attach(n, best.0, dangling_elem[k]);
            continue;
        }
        if let Some((t, q, _)) =
            b.g.nearest_tri(nd.level, p, attach_d, |t| is_open(&b, t, p))
        {
            let q = b.g.inset(t, q);
            let z = b.g.mesh.z_at(t, q);
            let an = b.node(
                [q[0], q[1], z],
                nd.level,
                b.g.mesh.tri_zone[t as usize],
                t,
                NONE,
                [0.0, 0.0],
            );
            let surf = b.g.mesh.tri_surf[t as usize];
            b.attach(n, an, surf);
            continue;
        }
        if let Some((t, _, d)) = b.g.nearest_tri(nd.level, p, attach_d, |_| true) {
            let sid = b.g.elems[b.g.mesh.tri_surf[t as usize] as usize].id.clone();
            let eid = node_edge
                .get(n as usize)
                .filter(|&&e| e != NONE)
                .map(|&e| b.g.elems[e as usize].id.clone());
            b.warn(
                "edge",
                eid.as_deref(),
                format!("end {d:.2} m from walled surface {sid}, not attached (no portal)"),
                Some(p),
                nd.level,
            );
        }
    }
    lap("edges", &mut tl);

    // 5. Verticals.
    let ngrid = Grid::build(
        &edge_nodes
            .iter()
            .map(|&d| bbox([xy(b.g.nodes[d as usize].xyz)]))
            .collect::<Vec<_>>(),
    );
    for (i, v) in input.verticals.iter().enumerate() {
        let id = gen_id(&v.id, "vertical", i);
        let kind_s = v.kind.clone().unwrap_or_else(|| "stairs".into());
        let step_free = v
            .step_free
            .unwrap_or(!matches!(kind_s.as_str(), "stairs" | "escalator" | "steps"));
        let (kind, tags) = (b.s(&kind_s), b.tags(&v.tags));
        let access = b.access(&v.access_groups);
        let elem = b.elem(Elem {
            kind: ElemKind::Vertical,
            id: id.clone(),
            class: NONE,
            sub: kind,
            tags,
            space: NONE,
            level: NONE,
            cost_factor: 1.0,
            speed: f32o(v.speed),
            time: f32o(v.time),
            penalty: 0.0,
            width: f32o(v.width),
            step_free,
            wait: v.wait.unwrap_or(if kind_s == "lift" { 30.0 } else { 0.0 }) as f32,
            props: props_str(&v.props),
            access,
        });
        let mut stops: Vec<u32> = vec![];
        for st in &v.stops {
            let lvl = b.level(st.level.as_deref(), st.at.get(2).copied());
            let p = [
                st.at.first().copied().unwrap_or(0.0),
                st.at.get(1).copied().unwrap_or(0.0),
            ];
            let n = if let Some(t) = b.g.tri_at(lvl, p) {
                let z = st.at.get(2).copied().unwrap_or_else(|| b.g.mesh.z_at(t, p));
                b.node(
                    [p[0], p[1], z],
                    lvl,
                    b.g.mesh.tri_zone[t as usize],
                    t,
                    NONE,
                    [0.0, 0.0],
                )
            } else {
                let mut best = (NONE, attach_d.max(snap));
                ngrid.visit(
                    [p[0] - best.1, p[1] - best.1, p[0] + best.1, p[1] + best.1],
                    |k| {
                        let d = edge_nodes[k as usize];
                        let dn = &b.g.nodes[d as usize];
                        let dd = dist(xy(dn.xyz), p);
                        if dn.level == lvl && dd <= best.1 {
                            best = (d, dd);
                        }
                    },
                );
                if best.0 != NONE {
                    best.0
                } else if let Some((n, _)) = b.anchor(lvl, p, attach_d.max(1.0), NONE) {
                    n
                } else {
                    let msg = format!(
                        "stop on level {} not on any surface or edge",
                        b.g.levels[lvl as usize].id
                    );
                    b.warn("vertical", Some(&id), msg, Some(p), lvl);
                    let z = st.at.get(2).copied().unwrap_or(b.lz(lvl));
                    b.node([p[0], p[1], z], lvl, NONE, NONE, NONE, [0.0, 0.0])
                }
            };
            stops.push(n);
        }
        if stops.len() < 2 {
            let at = v
                .stops
                .first()
                .filter(|s| s.at.len() >= 2)
                .map(|s| [s.at[0], s.at[1]]);
            b.warn("vertical", Some(&id), "needs >= 2 stops".into(), at, NONE);
            continue;
        }
        let pairs: Vec<(usize, usize)> = if kind_s == "lift" {
            (0..stops.len())
                .flat_map(|a| (a + 1..stops.len()).map(move |c| (a, c)))
                .collect()
        } else {
            (0..stops.len() - 1).map(|a| (a, a + 1)).collect()
        };
        let dir = v.direction.as_deref().unwrap_or("both");
        for (x, y) in pairs {
            let (na, nb) = (stops[x], stops[y]);
            let (pa, pb) = (b.g.nodes[na as usize].xyz, b.g.nodes[nb as usize].xyz);
            let geom: Vec<P3> = match &v.line {
                Some(line) if line.len() >= 2 && v.stops.len() == 2 => {
                    let mut g: Vec<P3> = line.iter().map(|q| vz(q, None, pa[2])).collect();
                    g[0] = pa;
                    *g.last_mut().unwrap() = pb;
                    g
                }
                _ => vec![pa, pb],
            };
            let l2 = poly_len2(&geom);
            // 2D mode (stops at the same z): the rise is the level distance times a default
            // storey height, upwards in stop order (first -> last).
            let dz = if (pb[2] - pa[2]).abs() > 1e-6 {
                pb[2] - pa[2]
            } else {
                let (la, lb) = (b.g.nodes[na as usize].level, b.g.nodes[nb as usize].level);
                let dl = la.abs_diff(lb) as usize;
                (if dl > 0 { dl } else { y - x }) as f64 * STOREY
            };
            let piece = Piece {
                elem,
                len: l2 as f32,
                climb: dz.max(0.0) as f32,
                descent: (-dz).max(0.0) as f32,
            };
            let f = b.fact(
                FactKind::Vertical,
                elem,
                NONE,
                NONE,
                &[piece],
                &geom,
                &[],
                grade_stats(l2, dz),
            );
            let fe = &mut b.g.facts[f as usize];
            if let Some(t) = v.time {
                fe.time = t as f32;
            }
            if (pb[2] - pa[2]).abs() <= 1e-6 {
                fe.dz = dz as f32;
                fe.len3 = l2.hypot(dz) as f32;
            }
            let up_ok = |d: f64| match dir {
                "up" => d >= 0.0,
                "down" => d <= 0.0,
                _ => true,
            };
            b.link(na, nb, f, up_ok(dz), up_ok(-dz));
        }
    }
    lap("verticals", &mut tl);

    // 6. Intra-zone walks (parallel over zones).
    let nz = b.g.zone_level.len();
    let mut zcount = vec![0u32; nz + 1];
    for n in &b.g.nodes {
        if n.zone != NONE {
            zcount[n.zone as usize + 1] += 1;
        }
    }
    for z in 1..=nz {
        zcount[z] += zcount[z - 1];
    }
    let mut fill = zcount.clone();
    b.g.zone_nodes = vec![0; zcount[nz] as usize];
    for (i, n) in b.g.nodes.iter().enumerate() {
        if n.zone != NONE {
            b.g.zone_nodes[fill[n.zone as usize] as usize] = i as u32;
            fill[n.zone as usize] += 1;
        }
    }
    b.g.zone_off = zcount;
    // Surfaces that no profile can tell apart (same class, tags, factor, speed, ...) share a
    // signature; pruning compares walks per signature.
    let sig: Vec<u32> = {
        // (class, sub, sorted tags, cost_factor, speed, width, step_free)
        type Sig = (u32, u32, Vec<u32>, u32, u32, u32, bool);
        let mut ids: HashMap<Sig, u32> = HashMap::new();
        b.g.elems
            .iter()
            .map(|e| {
                let mut tags = e.tags.clone();
                tags.sort_unstable();
                let key = (
                    e.class,
                    e.sub,
                    tags,
                    e.cost_factor.to_bits(),
                    e.speed.to_bits(),
                    e.width.to_bits(),
                    e.step_free,
                );
                let n = ids.len() as u32;
                *ids.entry(key).or_insert(n)
            })
            .collect()
    };
    let ntri = b.g.mesh.tri_v.len();
    let g = &b.g;
    let walks: Vec<Vec<WalkRes>> = (0..nz as u32)
        .into_par_iter()
        .map_init(
            || (TriScratch::new(ntri), WalkBuf::default()),
            |(ts, wb), z| {
                let mut out = zone_walks(g, z, ts, wb);
                prune(g, &sig, g.zone_nodes(z), &mut out);
                out
            },
        )
        .collect();
    for (z, list) in walks.into_iter().enumerate() {
        let lvl = b.g.zone_level[z];
        for w in list {
            let f = b.fact(
                FactKind::Walk,
                NONE,
                z as u32,
                lvl,
                &w.pieces,
                &w.geom,
                &w.chain,
                w.st,
            );
            b.link(w.a, w.b, f, true, true);
        }
    }
    lap("walks", &mut tl);

    // 7. Freeze into CSR.
    let nn = b.g.nodes.len();
    let mut first = vec![0u32; nn + 1];
    for a in &b.arcs {
        first[a.0 as usize + 1] += 1;
    }
    for i in 1..=nn {
        first[i] += first[i - 1];
    }
    let mut pos = first.clone();
    let na = b.arcs.len();
    b.g.head = vec![0; na];
    b.g.arc_fact = vec![0; na];
    for &(u, v, f) in &b.arcs {
        let k = pos[u as usize] as usize;
        b.g.head[k] = v;
        b.g.arc_fact[k] = f;
        pos[u as usize] += 1;
    }
    b.g.first_out = first;
    b.g.origin = b.g.nodes.first().map(|n| n.xyz).unwrap_or([0.0; 3]);
    let o = b.g.origin;
    b.g.node_xyz =
        b.g.nodes
            .iter()
            .map(|n| {
                [
                    (n.xyz[0] - o[0]) as f32,
                    (n.xyz[1] - o[1]) as f32,
                    (n.xyz[2] - o[2]) as f32,
                ]
            })
            .collect();
    // Edge segment index.
    let mut boxes = vec![];
    for (fi, f) in b.g.facts.iter().enumerate() {
        if f.kind == FactKind::Edge {
            for gi in f.g0..f.g1 - 1 {
                b.g.segs.push([fi as u32, gi]);
                boxes.push(bbox([
                    xy(b.g.geom[gi as usize]),
                    xy(b.g.geom[gi as usize + 1]),
                ]));
            }
        }
    }
    b.g.seg_grid = Grid::build(&boxes);
    lap("csr", &mut tl);

    // POIs: access points -> graph nodes (or edge snaps).
    let poi_snap = input.defaults.poi_snap.max(0.0);
    for (i, p) in input.pois.iter().enumerate() {
        let pid = gen_id(&p.id, "poi", i);
        let mut nodes: Vec<u32> = vec![];
        let mut snaps: Vec<PoiSnap> = vec![];
        let at0 = p.at.as_ref().map(|v| [v[0], v[1]]);
        for (k, a) in poi_access(p).iter().enumerate() {
            let found: Vec<u32> = if a.at.is_some() || a.space.is_some() {
                b.g.names
                    .get(&format!("{pid}@{k}"))
                    .map(|&n| vec![n])
                    .unwrap_or_default()
            } else if let Some(n) = &a.node {
                b.g.names.get(n).map(|&n| vec![n]).unwrap_or_default()
            } else if let Some(pt) = &a.portal {
                b.g.portals
                    .get(pt)
                    .map(|ns| ns.to_vec())
                    .unwrap_or_default()
            } else {
                vec![]
            };
            let what = a
                .node
                .as_deref()
                .or(a.portal.as_deref())
                .or(a.space.as_deref())
                .unwrap_or("point");
            if found.is_empty() {
                b.warn(
                    "poi",
                    Some(&pid),
                    format!("access {k} ({what}) not found"),
                    at0,
                    NONE,
                );
            }
            for n in found {
                let nd = &b.g.nodes[n as usize];
                let linked = nd.zone != NONE || !b.g.out_arcs(n).is_empty();
                if !linked && a.at.is_some() {
                    // Off every surface: snap onto the nearest edge (any class; profiles that
                    // may not use it get no access here).
                    let (p3, lvl) = (nd.xyz, nd.level);
                    let lv = (lvl != NONE).then_some(lvl);
                    if let Some((fact, s, q, _)) =
                        b.g.nearest_edge(p3, false, lv, poi_snap, &|_| true)
                    {
                        snaps.push(PoiSnap { fact, s, p: q });
                        continue;
                    }
                }
                if !linked {
                    let (at, lvl) = (xy(nd.xyz), nd.level);
                    b.warn(
                        "poi",
                        Some(&pid),
                        format!("access {k} ({what}) not on any walkable surface or edge (within {poi_snap} m)"),
                        Some(at),
                        lvl,
                    );
                    continue;
                }
                if !nodes.contains(&n) {
                    nodes.push(n);
                }
            }
        }
        if b.g.poi_ids.contains_key(&pid) {
            b.warn(
                "poi",
                Some(&pid),
                "duplicate id (later one ignored)".into(),
                at0,
                NONE,
            );
            continue;
        }
        b.g.poi_ids.insert(pid.clone(), b.g.pois.len() as u32);
        let access = b.access(&p.access_groups);
        b.g.pois.push(PoiInfo {
            id: pid,
            category: p.category.clone(),
            tags: p.tags.clone(),
            nodes,
            snaps,
            props: props_str(&p.props),
            access,
        });
    }
    lap("pois", &mut tl);
    if !b.access_overflow.is_empty() {
        let n = b.access_overflow.len();
        let names = b.access_overflow[..n.min(5)].join(", ");
        b.warn(
            "input",
            None,
            format!(
                "more than {} access groups: {n} more ({names}{}) share one reserved group that is never granted by name",
                crate::access::MAX_GROUPS,
                if n > 5 { ", ..." } else { "" }
            ),
            None,
            NONE,
        );
    }

    let g = b.g;
    let report = CompileReport {
        levels: g.levels.len(),
        surfaces: input.surfaces.len(),
        triangles: g.mesh.tri_v.len(),
        zones: g.zone_level.len(),
        nodes: g.nodes.len(),
        arcs: g.head.len(),
        facts: g.facts.len(),
        millis: t0.elapsed().as_secs_f64() * 1e3,
        timings,
        warnings: b.warnings,
    };
    (g, report)
}
