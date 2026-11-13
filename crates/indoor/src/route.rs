//! Query: locate -> attach -> A* -> unwind -> realistic geometry -> `Route`.

use crate::closure::{ClosureHit, Overlay};
use crate::cost::CompiledProfile;
use crate::funnel::measure;
use crate::geom::*;
use crate::graph::{FactKind, Graph};
use crate::locate::{Loc, Snap};
use crate::profile::{Corner, RouteMode};
use crate::search::QueryCtx;
use crate::smooth::{fillet, join_chain, realize};
use serde::Serialize;
use serde_json::value::RawValue;
use std::time::Instant;

mod costs;
mod matrix;
pub use costs::RouteCost;
pub(crate) use costs::costs;
pub(crate) use matrix::matrix;

/// Zone nodes considered when attaching a point (nearest by straight distance).
pub const ATTACH: usize = 24;
pub const MAX_SNAP: f64 = 6.0;
const NO_ACCESS_POINT: &str = "no usable access point (poi not on the graph for this profile)";
/// How far from a closed room's point `approach` looks for walkable space (m).
pub const APPROACH: f64 = 30.0;
/// How far a point in a closed scrap of a room moves to the room's open part (m).
const SCRAP: f64 = 1.5;
/// ... or to another open surface (m).
const SCRAP_ANY: f64 = 0.75;

#[derive(Serialize, Debug, Clone)]
pub struct Segment {
    /// `walk`, `edge`, the portal kind (`door`, `crossing`, ...) or the vertical kind (`stairs`, `lift`, ...)
    pub kind: String,
    /// walk | edge | portal | vertical | connector (short link from an edge node onto a surface)
    #[serde(rename = "type")]
    pub typ: &'static str,
    pub id: String,
    pub class: Option<String>,
    pub space: Option<String>,
    pub level: Option<String>,
    pub from_idx: usize,
    pub to_idx: usize,
    pub length: f64,
    pub duration: f64,
    /// The element's input `props`, passed through (null when none).
    pub props: Option<Box<RawValue>>,
    /// Access groups the element needs (any-of), when it is restricted (`access_groups`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<Vec<String>>,
    /// `ride` / `walk` for profiles with `on_foot` (bike): how this stretch is travelled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<&'static str>,
}

/// A run of segments travelled the same way (profiles with `on_foot`): `segments` is the
/// segment index range [s0, s1).
#[derive(Serialize, Debug, Clone)]
pub struct ModeLeg {
    pub mode: &'static str,
    pub from_idx: usize,
    pub to_idx: usize,
    pub s0: usize,
    pub s1: usize,
    pub length: f64,
    pub duration: f64,
}

#[derive(Serialize, Debug, Clone, Default)]
pub struct Stats {
    pub expanded: u32,
    pub micros: f64,
}

#[derive(Serialize, Debug, Clone, Default)]
pub struct Route {
    pub cost: f64,
    pub duration: f64,
    pub length: f64,
    pub climb: f64,
    pub descent: f64,
    pub coords: Vec<P3>,
    pub segments: Vec<Segment>,
    pub spaces: Vec<String>,
    pub warnings: Vec<String>,
    pub stats: Stats,
    /// The POI the route starts / ends at (POI and category locators only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_poi: Option<PoiHit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_poi: Option<PoiHit>,
    /// The unrouted first / last stretch into a room without doors (`approach` locators).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_approach: Option<Approach>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_approach: Option<Approach>,
    /// Ride / walk runs (profiles with `on_foot`; empty otherwise).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub legs: Vec<ModeLeg>,
    /// Closures the route respected (queries with `closures` / `avoid`, `closure.rs`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub closures: Vec<ClosureHit>,
}

/// A room the route could not enter (no way in for the profile): the route stops at `at`, the
/// nearest walkable point outside it, and `target` is the point in the room. `length` is the
/// straight horizontal distance between them and `duration` its walking time; neither is in
/// the route's own `length` / `duration` / `cost`.
#[derive(Serialize, Debug, Clone)]
pub struct Approach {
    pub space: Option<String>,
    pub at: P3,
    pub target: P3,
    pub level: Option<String>,
    pub length: f64,
    pub duration: f64,
}

/// Which POI (and which of its access points) a POI / category locator resolved to.
#[derive(Serialize, Debug, Clone)]
pub struct PoiHit {
    pub id: String,
    pub category: Option<String>,
    /// The access point used (graph node position).
    pub at: P3,
    pub level: Option<String>,
    /// How many POIs the locator offered (1 for `poi`).
    pub candidates: usize,
    pub props: Option<Box<RawValue>>,
}

#[derive(Clone, Debug)]
enum Leg {
    /// Query-time walk inside a zone (chain = ctx.pool[c0..c1]).
    Walk {
        c0: usize,
        c1: usize,
        a: P3,
        b: P3,
        la: P2,
        lb: P2,
        time: f32,
        /// Horizontal length of the taut walk the time was computed for.
        len: f32,
    },
    /// Part [f0, f1] of an arc (fractions by length along the arc direction).
    Arc {
        arc: u32,
        f0: f64,
        f1: f64,
    },
    Stay,
}

enum End {
    Tri(u32, P3),
    Edge(u32, f64, P3),
    Nodes(Vec<u32>),
    /// POI access points: (part, poi index); parts are `Nodes` or `Edge`.
    Pois(Vec<(End, u32)>),
}

impl End {
    /// (part, poi or NONE) for searching.
    fn parts(&self) -> Vec<(&End, u32)> {
        match self {
            End::Pois(v) => v.iter().map(|(e, i)| (e, *i)).collect(),
            e => vec![(e, NONE)],
        }
    }
    fn point(&self, g: &Graph) -> P3 {
        match self {
            End::Tri(_, p) | End::Edge(_, _, p) => *p,
            End::Nodes(ns) => g.nodes[ns[0] as usize].xyz,
            End::Pois(v) => v[0].0.point(g),
        }
    }
}

impl Graph {
    /// POIs a POI / category locator offers (indices into `pois`), in input order.
    pub fn poi_candidates(&self, loc: &Loc) -> Result<Vec<u32>, String> {
        self.poi_cands(loc, &|_| true)
    }

    /// `poi_candidates` for a profile: POIs it may not see (`access_groups`) are left out, and
    /// an explicit `poi` among them fails like a POI off the graph (nothing tells them apart).
    pub fn poi_candidates_for(&self, loc: &Loc, cp: &CompiledProfile) -> Result<Vec<u32>, String> {
        self.poi_cands(loc, &|i| cp.poi_ok(i))
    }

    fn poi_cands(&self, loc: &Loc, seen: &dyn Fn(u32) -> bool) -> Result<Vec<u32>, String> {
        let excluded = |i: u32| loc.exclude.contains(&self.pois[i as usize].id) || !seen(i);
        if let Some(id) = &loc.poi {
            let &i = self.poi_ids.get(id).ok_or(format!("unknown poi {id:?}"))?;
            if !seen(i) {
                return Err(NO_ACCESS_POINT.into());
            }
            return if excluded(i) {
                Err(format!("poi {id:?} is excluded"))
            } else {
                Ok(vec![i])
            };
        }
        let mut out = vec![];
        if !loc.pois.is_empty() {
            for id in &loc.pois {
                match self.poi_ids.get(id) {
                    Some(&i) if !excluded(i) && self.pois[i as usize].has_tags(&loc.tags) => {
                        if !out.contains(&i) {
                            out.push(i)
                        }
                    }
                    Some(_) => {}
                    None => return Err(format!("unknown poi {id:?}")),
                }
            }
        } else if let Some(cat) = &loc.category {
            for (i, p) in self.pois.iter().enumerate() {
                if p.in_category(cat) && p.has_tags(&loc.tags) && !excluded(i as u32) {
                    out.push(i as u32);
                }
            }
        }
        if out.is_empty() {
            let what = match &loc.category {
                Some(c) if loc.pois.is_empty() => format!("in category {c:?}"),
                _ => "of the given ids".to_string(),
            };
            let tags = if loc.tags.is_empty() {
                String::new()
            } else {
                format!(" with tags {:?}", loc.tags)
            };
            return Err(format!("no poi {what}{tags}"));
        }
        Ok(out)
    }

    fn poi_hit(&self, cp: &CompiledProfile, poi: u32, at: P3, _parts: usize, loc: &Loc) -> PoiHit {
        let p = &self.pois[poi as usize];
        let candidates = self
            .poi_candidates_for(loc, cp)
            .map(|v| v.len())
            .unwrap_or(1);
        PoiHit {
            id: p.id.clone(),
            category: p.category.clone(),
            at,
            level: self.level_at(at),
            candidates,
            props: if p.props.is_empty() {
                None
            } else {
                RawValue::from_string(p.props.clone()).ok()
            },
        }
    }

    /// Level whose z is nearest `p` (for display only).
    pub fn level_at(&self, p: P3) -> Option<String> {
        self.levels
            .iter()
            .min_by(|a, b| (a.z - p[2]).abs().total_cmp(&(b.z - p[2]).abs()))
            .map(|l| l.id.clone())
    }

    fn resolve(&self, loc: &Loc, cp: &CompiledProfile) -> Result<End, String> {
        if loc.is_poi() {
            let mut parts: Vec<(End, u32)> = vec![];
            for i in self.poi_candidates_for(loc, cp)? {
                let p = &self.pois[i as usize];
                let ns: Vec<u32> = p
                    .nodes
                    .iter()
                    .copied()
                    .filter(|&n| cp.usable[n as usize])
                    .collect();
                if !ns.is_empty() {
                    parts.push((End::Nodes(ns), i));
                }
                for sn in &p.snaps {
                    if cp.elem_usable(self.facts[sn.fact as usize].elem) {
                        parts.push((End::Edge(sn.fact, sn.s, sn.p), i));
                    }
                }
            }
            if parts.is_empty() {
                return Err(NO_ACCESS_POINT.into());
            }
            return Ok(End::Pois(parts));
        }
        let mode = cp.p.mode;
        // Outside the mixed mode, a node or portal with no arc of the mode cannot be an end.
        let in_mode = |ns: &[u32], what: String| -> Result<End, String> {
            if mode.is_mixed() || ns.iter().any(|&n| cp.usable[n as usize]) {
                Ok(End::Nodes(ns.to_vec()))
            } else {
                Err(format!("{what} has no usable link in mode {}", mode.name()))
            }
        };
        // Elements the query may not use (`access_groups`) are reported like unknown ones.
        if let Some(n) = &loc.node {
            let &i = self
                .names
                .get(n)
                .filter(|&&i| !cp.denies(self, self.nodes[i as usize].elem))
                .ok_or(format!("unknown node {n:?}"))?;
            return in_mode(&[i], format!("node {n:?}"));
        }
        if let Some(p) = &loc.portal {
            let ns = self
                .portals
                .get(p)
                .filter(|ns| !cp.denies(self, self.portal_elem(ns[0])))
                .ok_or(format!("unknown portal {p:?}"))?;
            return in_mode(ns, format!("portal {p:?}"));
        }
        // With `approach`, a space the graph does not have (not walkable) is its point `at`.
        let known =
            |s: &String| self.space_tri.contains_key(s) || !loc.approach || loc.at.is_none();
        if let Some(s) = loc.space.as_ref().filter(|s| known(s)) {
            if mode == RouteMode::EdgesOnly {
                return Err(format!(
                    "space {s:?} is a surface, and mode edges_only does not walk on surfaces"
                ));
            }
            let t = *self
                .space_tri
                .get(s)
                .ok_or(format!("unknown space {s:?}"))?;
            // `at` with `space`: that spot, snapped onto the space's own surface (never
            // through a wall into the next room); out of reach, the space's usual point.
            if let Some(at) = loc.at.as_ref().filter(|a| a.len() >= 2) {
                let level = self.mesh.tri_level[t as usize];
                let inside = |u: u32| {
                    let e = &self.elems[self.mesh.tri_surf[u as usize] as usize];
                    e.id == *s || (e.space != NONE && self.strings[e.space as usize] == *s)
                };
                let usable = |u: u32| inside(u) && cp.tri_weight(self, u).is_finite();
                let seen = |u: u32| inside(u) && !cp.tri_denied(self, u);
                let snap = self
                    .snap_within(at, level, MAX_SNAP, &usable)
                    .or_else(|| self.snap_within(at, level, MAX_SNAP, &seen));
                if let Some(Snap::Tri { tri, p }) = snap {
                    return Ok(self.out_of_scrap(cp, tri, p));
                }
            }
            if cp.tri_denied(self, t) {
                return Err(format!("unknown space {s:?}"));
            }
            let c = self.mesh.tri_c[t as usize];
            return Ok(End::Tri(t, [c[0], c[1], self.mesh.z_at(t, c)]));
        }
        let at = loc
            .at
            .as_ref()
            .ok_or("locator needs at, space, node or portal")?;
        if at.len() < 2 {
            return Err("at needs [x, y(, z)]".into());
        }
        let level = match &loc.level {
            Some(l) => Some(self.level_idx(l).ok_or(format!("unknown level {l:?}"))?),
            None => None,
        };
        let tri_ok = |t: u32| cp.tri_weight(self, t).is_finite();
        let edge_ok = |f: u32| cp.elem_usable(self.facts[f as usize].elem);
        // A room the graph leaves out (`approach` with an unknown space): look farther.
        let reach = if loc.space.is_some() {
            APPROACH
        } else {
            MAX_SNAP
        };
        let nowhere = || match mode {
            RouteMode::Mixed => format!("no walkable surface or edge within {reach} m of {at:?}"),
            RouteMode::EdgesOnly => {
                format!("no usable edge within {reach} m of {at:?} (mode edges_only)")
            }
            RouteMode::SurfacesOnly => {
                format!("no walkable surface within {reach} m of {at:?} (mode surfaces_only)")
            }
        };
        // A point in (or on) an area the query may not use is nowhere: it is neither snapped
        // out of it nor approached from outside.
        // So is a point that only lands on an element kind the mode excludes.
        let denied = |s: &Snap| match *s {
            Snap::Tri { tri, .. } => mode == RouteMode::EdgesOnly || cp.tri_denied(self, tri),
            Snap::Edge { fact, .. } => {
                mode == RouteMode::SurfacesOnly || cp.denies(self, self.facts[fact as usize].elem)
            }
        };
        if cp.granted != crate::access::ALL
            && matches!(
                self.snap_point(at, level, 0.0, |_| true, |_| false),
                Some(Snap::Tri { tri, .. }) if cp.tri_denied(self, tri)
            )
        {
            return Err(nowhere());
        }
        match self.snap_point(at, level, reach, tri_ok, edge_ok) {
            Some(s) if denied(&s) => Err(nowhere()),
            Some(Snap::Tri { tri, p }) => Ok(self.out_of_scrap(cp, tri, p)),
            Some(Snap::Edge { fact, s, p }) => Ok(End::Edge(fact, s, p)),
            None => Err(nowhere()),
        }
    }

    /// A point in a closed scrap of a room (a sliver cut off by overlapping spaces, with no
    /// way in or out, not the part holding the room's largest triangle) moves to the nearest
    /// open part of the same surface within `SCRAP` m, else to any open surface within
    /// `SCRAP_ANY` m. A room closed as a whole stays (that is `approach`'s business).
    fn out_of_scrap(&self, cp: &CompiledProfile, t: u32, p: P3) -> End {
        let z = self.mesh.tri_zone[t as usize];
        let open = |z: u32| cp.zone_open.get(z as usize).copied().unwrap_or(true);
        if open(z) || cp.zone_locked(z) {
            return End::Tri(t, p);
        }
        let surf = self.mesh.tri_surf[t as usize];
        let main = self.space_tri.get(&self.elems[surf as usize].id);
        if main.is_none_or(|&m| self.mesh.tri_zone[m as usize] == z) {
            return End::Tri(t, p);
        }
        let level = self.mesh.tri_level[t as usize];
        let usable =
            |u: u32| open(self.mesh.tri_zone[u as usize]) && cp.tri_weight(self, u).is_finite();
        let to = self
            .nearest_tri(level, xy(p), SCRAP, |u| {
                self.mesh.tri_surf[u as usize] == surf && usable(u)
            })
            .or_else(|| self.nearest_tri(level, xy(p), SCRAP_ANY, usable));
        match to {
            Some((u, q, _)) => {
                let q = self.inset(u, q);
                End::Tri(u, [q[0], q[1], self.mesh.z_at(u, q)])
            }
            None => End::Tri(t, p),
        }
    }
}

/// Zones an end lies in (surface points and nodes).
fn end_zones(g: &Graph, e: &End) -> Vec<u32> {
    match e {
        End::Tri(t, _) => vec![g.mesh.tri_zone[*t as usize]],
        End::Nodes(ns) => ns.iter().map(|&n| g.nodes[n as usize].zone).collect(),
        _ => vec![],
    }
}

/// A candidate approach point: (zone, triangle, point, distance).
type Cand = (u32, u32, P2, f64);

impl Graph {
    /// Usable triangles of `level` within `APPROACH` of `p` outside zone `own`: the nearest
    /// point of each zone.
    fn approach_cands(&self, cp: &CompiledProfile, level: u32, p: P2, own: u32) -> Vec<Cand> {
        let mut out: Vec<Cand> = vec![];
        let Some(grid) = self.tri_grid.get(level as usize) else {
            return out;
        };
        let base = self.level_tri[level as usize][0];
        let r = APPROACH;
        grid.visit([p[0] - r, p[1] - r, p[0] + r, p[1] + r], |k| {
            let t = base + k;
            let z = self.mesh.tri_zone[t as usize];
            if z == own || !cp.tri_weight(self, t).is_finite() {
                return;
            }
            let [a, b, c] = self.mesh.pts(t);
            let (q, d2) = closest_on_tri(p, a, b, c);
            let d = d2.sqrt();
            if d > r {
                return;
            }
            match out.iter_mut().find(|x| x.0 == z) {
                Some(x) if d < x.3 => *x = (z, t, q, d),
                Some(_) => {}
                None => out.push((z, t, q, d)),
            }
        });
        out
    }
}

/// `approach` ends. An end in a closed zone (no usable way in or out, and
/// not the other end's zone) moves to the nearest walkable point outside it: in an open zone,
/// or in the other end's zone; when both ends are closed and either finds no open zone, in the
/// zone nearest to both. A point off the walkable surfaces (a space the graph leaves out) keeps
/// its snap. Both report the stretch to the point asked for.
fn approach(
    g: &Graph,
    cp: &CompiledProfile,
    a: &mut End,
    la: &Loc,
    b: &mut End,
    lb: &Loc,
) -> Result<(Option<Approach>, Option<Approach>), String> {
    if !la.approach && !lb.approach {
        return Ok((None, None));
    }
    let (za, zb) = (end_zones(g, a), end_zones(g, b));
    let closed = |e: &End, other: &[u32], loc: &Loc| -> Option<(u32, P3, u32)> {
        let End::Tri(t, p) = e else { return None };
        let z = g.mesh.tri_zone[*t as usize];
        // Closed only for lack of access: not a room without doors, never approached.
        let open = cp.zone_open.get(z as usize).copied().unwrap_or(true) || cp.zone_locked(z);
        (loc.approach && !open && !other.contains(&z)).then_some((*t, *p, z))
    };
    let (ca, cb) = (closed(a, &zb, la), closed(b, &za, lb));
    let cands = |c: Option<(u32, P3, u32)>| -> Vec<Cand> {
        c.map(|(t, p, z)| g.approach_cands(cp, g.mesh.tri_level[t as usize], xy(p), z))
            .unwrap_or_default()
    };
    let (ka, kb) = (cands(ca), cands(cb));
    let near = |l: &[Cand], ok: &dyn Fn(u32) -> bool| -> Option<Cand> {
        l.iter()
            .filter(|c| ok(c.0))
            .min_by(|x, y| x.3.total_cmp(&y.3))
            .copied()
    };
    let open = |z: u32| cp.zone_open[z as usize];
    let (mut pa, mut pb) = (None, None);
    match (ca.is_some(), cb.is_some()) {
        (true, false) => pa = near(&ka, &|z| open(z) || zb.contains(&z)),
        (false, true) => pb = near(&kb, &|z| open(z) || za.contains(&z)),
        (true, true) => {
            pa = near(&ka, &open);
            pb = near(&kb, &open);
            if pa.is_none() || pb.is_none() {
                // One corridor with no way out: the zone nearest to both.
                let shared = ka
                    .iter()
                    .filter_map(|x| kb.iter().find(|y| y.0 == x.0).map(|y| (*x, *y)))
                    .min_by(|x, y| (x.0.3 + x.1.3).total_cmp(&(y.0.3 + y.1.3)));
                if let Some((x, y)) = shared {
                    (pa, pb) = (Some(x), Some(y));
                }
            }
        }
        (false, false) => {}
    }
    let side =
        |e: &mut End, loc: &Loc, c: Option<(u32, P3, u32)>, pick: Option<Cand>, what: &str| {
            let End::Tri(t0, p0) = *e else {
                return Ok(None);
            };
            // The point asked for, when the snap moved it off a room the graph leaves out.
            let off = loc.at.as_ref().filter(|a| {
                loc.approach
                    && a.len() >= 2
                    && loc
                        .space
                        .as_ref()
                        .is_none_or(|s| !g.space_tri.contains_key(s))
                    && dist([a[0], a[1]], xy(p0)) > 0.5
            });
            let target = off.map_or(p0, |a| [a[0], a[1], p0[2]]);
            let at = match (c, pick) {
                (Some(_), Some((_, t, q, _))) => {
                    let q = g.inset(t, q);
                    let at = [q[0], q[1], g.mesh.z_at(t, q)];
                    *e = End::Tri(t, at);
                    at
                }
                (Some(_), None) => {
                    return Err(format!(
                        "{what}: no way into its room, and no walkable space within {APPROACH} m"
                    ));
                }
                (None, _) if off.is_some() => p0,
                (None, _) => return Ok(None),
            };
            let surf = &g.elems[g.mesh.tri_surf[t0 as usize] as usize];
            let space = match &loc.space {
                Some(s) => Some(s.clone()),
                None if off.is_none() => g.str(surf.space).map(String::from),
                None => None,
            };
            let length = dist(xy(at), xy(target));
            let speed = cp.speed[g.mesh.tri_surf[t0 as usize] as usize].max(1e-3) as f64;
            Ok(Some(Approach {
                space,
                at,
                target,
                level: g
                    .levels
                    .get(g.mesh.tri_level[t0 as usize] as usize)
                    .map(|l| l.id.clone()),
                length,
                duration: length / speed,
            }))
        };
    let ra = side(a, la, ca, pa, "from")?;
    let rb = side(b, lb, cb, pb, "to")?;
    Ok((ra, rb))
}

struct Q<'a> {
    g: &'a Graph,
    cp: &'a CompiledProfile,
    legs: Vec<Leg>,
    /// Closures: walks at the ends go around polygon closures.
    ov: Option<&'a Overlay>,
}

impl<'a> Q<'a> {
    fn leg(&mut self, l: Leg) -> u32 {
        self.legs.push(l);
        (self.legs.len() - 1) as u32
    }

    /// Zone attachment of point `p` in `tri`: (node, cost, leg) per reachable nearby zone node.
    /// `to_point`: walks go node -> p (target side). `direct`: also walk to this point.
    fn attach_zone(
        &mut self,
        ctx: &mut QueryCtx,
        tri: u32,
        p: P3,
        to_point: bool,
        direct: Option<(u32, P3)>,
        out: &mut Vec<(u32, f32, u32)>,
    ) -> Option<(f32, u32)> {
        let zone = self.g.mesh.tri_zone[tri as usize];
        let direct = direct.filter(|d| self.g.mesh.tri_zone[d.0 as usize] == zone);
        // The nearest ATTACH usable nodes; if every one of them is blocked for the profile
        // (forbidden surfaces in between), all usable nodes of the zone.
        let start = out.len();
        let mut res = self.attach_nodes(ctx, tri, p, to_point, direct, ATTACH, out);
        if out.len() == start && self.g.zone_nodes(zone).len() > ATTACH {
            res = self.attach_nodes(ctx, tri, p, to_point, direct, usize::MAX, out);
        }
        res
    }

    fn attach_nodes(
        &mut self,
        ctx: &mut QueryCtx,
        tri: u32,
        p: P3,
        to_point: bool,
        direct: Option<(u32, P3)>,
        limit: usize,
        out: &mut Vec<(u32, f32, u32)>,
    ) -> Option<(f32, u32)> {
        let (g, cp) = (self.g, self.cp);
        let zone = g.mesh.tri_zone[tri as usize];
        ctx.cand.clear();
        for &n in g.zone_nodes(zone) {
            if cp.usable[n as usize] {
                let q = g.nodes[n as usize].xyz;
                ctx.cand.push((dist3(q, p) as f32, n));
            }
        }
        if ctx.cand.len() > limit {
            ctx.cand
                .select_nth_unstable_by(limit, |a, b| a.0.total_cmp(&b.0));
            ctx.cand.truncate(limit);
        }
        let mut remaining = ctx.cand.len() + direct.is_some() as usize;
        if remaining == 0 {
            return None;
        }
        let cand = std::mem::take(&mut ctx.cand);
        let ov = self.ov.filter(|o| !o.is_empty());
        let goal = direct.map_or(NONE, |d| d.0);
        ctx.tri.run(
            &g.mesh,
            tri,
            |t| match ov {
                Some(o) if t != tri && t != goal && o.tri_closed(t, xy(p)) => f32::INFINITY,
                _ => cp.tri_weight(g, t),
            },
            |t| {
                for c in &cand {
                    if g.nodes[c.1 as usize].tri == t {
                        remaining -= 1;
                    }
                }
                if direct.is_some_and(|d| d.0 == t) {
                    remaining -= 1;
                }
                remaining > 0
            },
        );
        let walk = |q: &mut Self,
                    ctx: &mut QueryCtx,
                    t_end: u32,
                    a: P3,
                    b: P3,
                    la: P2,
                    lb: P2,
                    reverse: bool|
         -> Option<(f32, u32)> {
            let c0 = ctx.pool.len();
            if !ctx.tri.chain(t_end, &mut ctx.pool) {
                return None;
            }
            if reverse {
                ctx.pool[c0..].reverse();
            }
            let st = measure(&g.mesh, &ctx.pool[c0..], a, b, &mut ctx.wb);
            let mut cost = cp.walk_cost(&ctx.wb.pieces, &st, true);
            // Polygon closures: big triangles can hold an area the chain did not go around.
            if q.ov.is_some_and(|o| {
                o.walk_closed(&ctx.wb.geom, g.mesh.tri_level[t_end as usize], xy(p))
            }) {
                cost = f64::INFINITY;
            }
            if !cost.is_finite() {
                ctx.pool.truncate(c0);
                return None;
            }
            let time = cp.walk_cost(&ctx.wb.pieces, &st, false) as f32;
            let len = st.len as f32;
            let c1 = ctx.pool.len();
            Some((
                cost as f32,
                q.leg(Leg::Walk {
                    c0,
                    c1,
                    a,
                    b,
                    la,
                    lb,
                    time,
                    len,
                }),
            ))
        };
        for c in &cand {
            let nd = &g.nodes[c.1 as usize];
            let lead = [nd.lead[0] as f64, nd.lead[1] as f64];
            let r = if to_point {
                walk(self, ctx, nd.tri, nd.xyz, p, lead, [0.0; 2], true)
            } else {
                walk(self, ctx, nd.tri, p, nd.xyz, [0.0; 2], lead, false)
            };
            if let Some((cost, leg)) = r {
                out.push((c.1, cost, leg));
            }
        }
        ctx.cand = cand;
        let d = direct?;
        walk(self, ctx, d.0, p, d.1, [0.0; 2], [0.0; 2], false)
    }

    /// Edge attachment: (node, cost, leg) for the reachable fact ends.
    fn attach_edge(&mut self, fact: u32, s: f64, to_point: bool, out: &mut Vec<(u32, f32, u32)>) {
        let g = self.g;
        let f = &g.facts[fact as usize];
        // On an end node: the node itself (other edges there may be allowed when this one isn't).
        let l = f.len3 as f64;
        for (node, d) in [(f.na, s * l), (f.nb, (1.0 - s) * l)] {
            if d < 1e-6 {
                let leg = self.leg(Leg::Stay);
                out.push((node, 0.0, leg));
            }
        }
        let start = out.len();
        let ab = g.find_arc(f.na, fact, false);
        let ba = g.find_arc(f.nb, fact, true);
        if !to_point {
            if let Some(a) = ab {
                let c = self.cp.arc_cost[a] * (1.0 - s) as f32;
                let leg = self.leg(Leg::Arc {
                    arc: a as u32,
                    f0: s,
                    f1: 1.0,
                });
                out.push((f.nb, c, leg));
            }
            if let Some(a) = ba {
                let c = self.cp.arc_cost[a] * s as f32;
                let leg = self.leg(Leg::Arc {
                    arc: a as u32,
                    f0: 1.0 - s,
                    f1: 1.0,
                });
                out.push((f.na, c, leg));
            }
        } else {
            if let Some(a) = ab {
                let c = self.cp.arc_cost[a] * s as f32;
                let leg = self.leg(Leg::Arc {
                    arc: a as u32,
                    f0: 0.0,
                    f1: s,
                });
                out.push((f.na, c, leg));
            }
            if let Some(a) = ba {
                let c = self.cp.arc_cost[a] * (1.0 - s) as f32;
                let leg = self.leg(Leg::Arc {
                    arc: a as u32,
                    f0: 0.0,
                    f1: 1.0 - s,
                });
                out.push((f.nb, c, leg));
            }
        }
        let mut k = start;
        while k < out.len() {
            if out[k].1.is_finite() {
                k += 1;
            } else {
                out.swap_remove(k);
            }
        }
    }
}

/// Polyline sub-range [f0, f1] by 3D length.
fn cut(pts: &[P3], f0: f64, f1: f64, out: &mut Vec<P3>) {
    if f0 <= 0.0 && f1 >= 1.0 {
        out.extend_from_slice(pts);
        return;
    }
    let total = poly_len3(pts);
    let (s0, s1) = (f0 * total, f1 * total);
    let mut acc = 0.0;
    let at = |k: usize, acc: f64, s: f64| {
        let l = dist3(pts[k], pts[k + 1]);
        lerp3(
            pts[k],
            pts[k + 1],
            if l > 0.0 {
                ((s - acc) / l).clamp(0.0, 1.0)
            } else {
                0.0
            },
        )
    };
    let mut started = false;
    for k in 0..pts.len() - 1 {
        let l = dist3(pts[k], pts[k + 1]);
        if !started && acc + l >= s0 {
            out.push(at(k, acc, s0));
            started = true;
        }
        if started {
            if acc + l >= s1 {
                out.push(at(k, acc, s1));
                return;
            }
            out.push(pts[k + 1]);
        }
        acc += l;
    }
    if !started {
        out.push(pts[pts.len() - 1]);
    }
}

/// Extra costs for POI ends, closures and whether to build the geometry (`route_with`).
pub(crate) struct Extra<'a> {
    /// (poi index, seconds) added to starting at / arriving at that POI.
    pub from: Vec<(u32, f32)>,
    pub to: Vec<(u32, f32)>,
    /// false: only search (cost, `from_poi` / `to_poi`); no coordinates or segments.
    pub geometry: bool,
    /// Closed and slowed elements and areas (`closure.rs`).
    pub closures: Option<&'a Overlay>,
}

impl Default for Extra<'_> {
    fn default() -> Self {
        Extra {
            from: vec![],
            to: vec![],
            geometry: true,
            closures: None,
        }
    }
}

fn extra_of(v: &[(u32, f32)], poi: u32) -> f32 {
    v.iter().find(|x| x.0 == poi).map_or(0.0, |x| x.1)
}

pub fn route(
    g: &Graph,
    cp: &CompiledProfile,
    ctx: &mut QueryCtx,
    from: &Loc,
    to: &Loc,
) -> Result<Route, String> {
    route_with(g, cp, ctx, from, to, &Extra::default())
}

/// `route` with closures in force (`closure.rs`); the route reports the ones it respected.
pub fn route_closed(
    g: &Graph,
    cp: &CompiledProfile,
    ctx: &mut QueryCtx,
    from: &Loc,
    to: &Loc,
    ov: &Overlay,
) -> Result<Route, String> {
    let extra = Extra {
        closures: Some(ov),
        ..Extra::default()
    };
    route_with(g, cp, ctx, from, to, &extra)
}

pub(crate) fn route_with(
    g: &Graph,
    cp: &CompiledProfile,
    ctx: &mut QueryCtx,
    from: &Loc,
    to: &Loc,
    extra: &Extra<'_>,
) -> Result<Route, String> {
    let t0 = Instant::now();
    ctx.wb.new_query();
    let mut a = g.resolve(from, cp).map_err(|e| format!("from: {e}"))?;
    let mut b = g.resolve(to, cp).map_err(|e| format!("to: {e}"))?;
    let (from_approach, to_approach) = approach(g, cp, &mut a, from, &mut b, to)?;
    let ov = extra.closures.filter(|o| !o.is_empty());
    let mut q = Q {
        g,
        cp,
        legs: vec![],
        ov,
    };
    ctx.pool.clear();
    let mut sources = vec![];
    let mut exits = vec![];
    let mut direct: Option<(f32, u32)> = None;
    // Legs created for each POI part: (first leg, end leg, poi, access point).
    let mut tagged: Vec<(u32, u32, u32, P3)> = vec![];
    // Source POI of an edge-to-edge direct candidate.
    let mut direct_from: (u32, P3) = (NONE, [0.0; 3]);
    let parts_a = a.parts();
    let parts_b = b.parts();
    for &(e, poi) in &parts_a {
        let l0 = q.legs.len() as u32;
        let s0 = sources.len();
        match e {
            End::Tri(t, p) => {
                let d = match (&b, parts_b.len()) {
                    (End::Tri(t2, p2), 1) => Some((*t2, *p2)),
                    _ => None,
                };
                direct = q.attach_zone(ctx, *t, *p, false, d, &mut sources);
            }
            End::Edge(f, s, _) => q.attach_edge(*f, *s, false, &mut sources),
            End::Nodes(ns) => {
                for &n in ns {
                    let leg = q.leg(Leg::Stay);
                    sources.push((n, 0.0, leg));
                }
            }
            End::Pois(_) => unreachable!(),
        }
        if poi != NONE {
            let x = extra_of(&extra.from, poi);
            for s in &mut sources[s0..] {
                s.1 += x;
            }
            tagged.push((l0, q.legs.len() as u32, poi, e.point(g)));
        }
    }
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for &(e, poi) in &parts_b {
        let l0 = q.legs.len() as u32;
        let x0 = exits.len();
        match e {
            End::Tri(t, p) => {
                q.attach_zone(ctx, *t, *p, true, None, &mut exits);
            }
            End::Edge(f, s, _) => {
                q.attach_edge(*f, *s, true, &mut exits);
                // Both ends on the same edge: walk along it directly.
                for &(ea, pa) in &parts_a {
                    let End::Edge(fa, sa, pa_at) = ea else {
                        continue;
                    };
                    if fa != f {
                        continue;
                    }
                    let fc = &g.facts[*f as usize];
                    let arc = if s >= sa {
                        g.find_arc(fc.na, *f, false).map(|x| (x, *sa, *s))
                    } else {
                        g.find_arc(fc.nb, *f, true).map(|x| (x, 1.0 - sa, 1.0 - s))
                    };
                    if let Some((x, f0, f1)) = arc {
                        let c = cp.arc_cost[x] * (f1 - f0) as f32
                            + extra_of(&extra.from, pa)
                            + extra_of(&extra.to, poi);
                        if c.is_finite() && direct.is_none_or(|d| c < d.0) {
                            let leg = q.leg(Leg::Arc {
                                arc: x as u32,
                                f0,
                                f1,
                            });
                            direct = Some((c, leg));
                            direct_from = (pa, *pa_at);
                        }
                    }
                }
            }
            End::Nodes(ns) => {
                for &n in ns {
                    let leg = q.leg(Leg::Stay);
                    exits.push((n, 0.0, leg));
                }
            }
            End::Pois(_) => unreachable!(),
        }
        let pts: Vec<P3> = match e {
            End::Nodes(ns) => ns.iter().map(|&n| g.nodes[n as usize].xyz).collect(),
            _ => vec![e.point(g)],
        };
        for p in pts {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        if poi != NONE {
            let x = extra_of(&extra.to, poi);
            for s in &mut exits[x0..] {
                s.1 += x;
            }
            tagged.push((l0, q.legs.len() as u32, poi, e.point(g)));
        }
    }
    // Heuristic centre: the targets' bounding box centre; the radius loop below makes every
    // exit's cost cover its distance beyond the radius.
    let center = [
        (lo[0] + hi[0]) * 0.5,
        (lo[1] + hi[1]) * 0.5,
        (lo[2] + hi[2]) * 0.5,
    ];
    let radius = 0.0f64;
    // Node targets reached directly from sources (same node) are handled by A* (g + 0 exit).
    let o = g.origin;
    let tc = [
        (center[0] - o[0]) as f32,
        (center[1] - o[1]) as f32,
        (center[2] - o[2]) as f32,
    ];
    let mut rad = radius as f32;
    for &(n, c, _) in &exits {
        let p = g.node_xyz[n as usize];
        let d = ((p[0] - tc[0]).powi(2) + (p[1] - tc[1]).powi(2) + (p[2] - tc[2]).powi(2)).sqrt();
        if cp.inv_vmax > 0.0 {
            rad = rad.max(d - c / cp.inv_vmax);
        }
    }
    let best0 = direct.map(|d| d.0).unwrap_or(f32::INFINITY);
    let allow = cp.allowed_areas(&sources, &exits);
    let res = ctx.astar(
        g,
        cp,
        &sources,
        &exits,
        tc,
        rad.max(0.0) + 1e-3,
        best0,
        &allow,
        ov,
    );
    if !res.cost.is_finite() {
        // Closures alone can be the reason: then say so.
        let closed = res.touched
            && ctx
                .astar(
                    g,
                    cp,
                    &sources,
                    &exits,
                    tc,
                    rad.max(0.0) + 1e-3,
                    best0,
                    &allow,
                    None,
                )
                .cost
                .is_finite();
        return Err(if closed {
            "no route: the way is closed (closures)".into()
        } else {
            "no route".into()
        });
    }
    let mut legs: Vec<Leg> = vec![];
    let mut ends = (NONE, NONE);
    let mut arcs = vec![];
    if res.end == NONE {
        legs.push(q.legs[direct.unwrap().1 as usize].clone());
    } else {
        let s = ctx.unwind(g, res.end, &mut arcs);
        ends = (s, res.end);
        legs.push(q.legs[ctx.src_leg[s as usize] as usize].clone());
        legs.extend(arcs.iter().map(|&a| Leg::Arc {
            arc: a,
            f0: 0.0,
            f1: 1.0,
        }));
        legs.push(q.legs[ctx.exit_leg[res.end as usize] as usize].clone());
    }
    let mut r = Route {
        cost: res.cost as f64,
        ..Default::default()
    };
    if extra.geometry {
        assemble(g, cp, ctx, &legs, ov, &mut r);
    }
    // Stay legs: the access node itself; edge snaps: the snapped point on the edge's level.
    let hit = |leg: u32, node: u32, loc: &Loc, n: usize| -> Option<PoiHit> {
        let &(_, _, poi, at) = tagged.iter().find(|t| leg >= t.0 && leg < t.1)?;
        let mut h = g.poi_hit(cp, poi, at, n, loc);
        if matches!(q.legs[leg as usize], Leg::Stay) {
            let nd = &g.nodes[node as usize];
            h.at = nd.xyz;
            h.level = g.levels.get(nd.level as usize).map(|l| l.id.clone());
        } else if let Leg::Arc { arc, .. } = q.legs[leg as usize] {
            let lv = g.fact_of(arc as usize).0.level;
            if let Some(l) = g.levels.get(lv as usize) {
                h.level = Some(l.id.clone());
            }
        }
        Some(h)
    };
    if res.end == NONE {
        let leg = direct.unwrap().1;
        if from.is_poi() && direct_from.0 != NONE {
            r.from_poi = Some(g.poi_hit(cp, direct_from.0, direct_from.1, parts_a.len(), from));
        }
        if to.is_poi() {
            r.to_poi = hit(leg, NONE, to, parts_b.len());
        }
    } else {
        if from.is_poi() {
            r.from_poi = hit(ctx.src_leg[ends.0 as usize], ends.0, from, parts_a.len());
        }
        if to.is_poi() {
            r.to_poi = hit(ctx.exit_leg[ends.1 as usize], ends.1, to, parts_b.len());
        }
    }
    r.from_approach = from_approach;
    r.to_approach = to_approach;
    if let Some(o) = ov {
        // Which closures made a difference: the route without them, searched only when the
        // search met one (it reuses the scratch, so after everything above).
        let mut free = vec![];
        if res.touched {
            let f = ctx.astar(
                g,
                cp,
                &sources,
                &exits,
                tc,
                rad.max(0.0) + 1e-3,
                best0,
                &allow,
                None,
            );
            if f.cost.is_finite() && f.end != NONE {
                ctx.unwind(g, f.end, &mut free);
            }
        }
        r.closures = o.hits(g, &arcs, &free);
    }
    if let Some(o) = extra.closures {
        r.warnings.extend(o.warnings.iter().cloned());
    }
    r.stats = Stats {
        expanded: res.expanded,
        micros: t0.elapsed().as_secs_f64() * 1e6,
    };
    Ok(r)
}

pub(crate) fn push_coords(coords: &mut Vec<P3>, pts: &[P3]) -> usize {
    let mut from = coords.len();
    let mut it = pts.iter();
    if let (Some(last), Some(first)) = (coords.last(), pts.first())
        && dist3(*last, *first) < 1e-6
    {
        from -= 1;
        it.next();
    }
    coords.extend(it);
    from
}

/// Per-surface walk segments; the walk's `time` is split by each step's walking time on its
/// surface (surface speed, incline costs), not by length.
fn walk_segments(
    g: &Graph,
    cp: &CompiledProfile,
    surf: &[u32],
    pts: &[P3],
    time: f64,
    level: u32,
    r: &mut Route,
) {
    if pts.len() < 2 {
        return;
    }
    let from = push_coords(&mut r.coords, pts);
    let step = |k: usize| -> f64 {
        let (a, b) = (pts[k], pts[k + 1]);
        let dz = b[2] - a[2];
        let (up, down, _) = cp.incline_of(surf[k]);
        (dist(xy(a), xy(b)) + up * dz.max(0.0) + down * (-dz).max(0.0))
            / cp.speed[surf[k] as usize].max(1e-6) as f64
    };
    let total: f64 = (0..pts.len() - 1).map(step).sum();
    let share = |k0: usize, k1: usize| -> f64 {
        if total > 1e-12 {
            (k0..k1).map(step).sum::<f64>() / total
        } else {
            poly_len3(&pts[k0..=k1]) / poly_len3(pts).max(1e-12)
        }
    };
    let surf_of = |k: usize| surf[k];
    let mut k0 = 0;
    let mut cur = surf_of(0);
    let n = pts.len() - 1;
    for k in 1..=n {
        let s = if k < n { surf_of(k) } else { NONE };
        let e0 = &g.elems[cur as usize];
        let same = s != NONE && {
            let e1 = &g.elems[s as usize];
            s == cur
                || (e1.space == e0.space
                    && e1.class == e0.class
                    && e0.space != NONE
                    && cp.is_foot(s) == cp.is_foot(cur))
        };
        if same {
            continue;
        }
        let l = poly_len3(&pts[k0..=k]);
        r.segments.push(Segment {
            kind: "walk".into(),
            typ: "walk",
            id: e0.id.clone(),
            class: g.str(e0.class).map(String::from),
            space: g.str(e0.space).map(String::from),
            level: g.levels.get(level as usize).map(|l| l.id.clone()),
            from_idx: from + k0,
            to_idx: from + k,
            length: l,
            duration: time * share(k0, k),
            props: g.props(cur),
            access: g.elem_access(cur),
            mode: cp.mode_of(cur),
        });
        k0 = k;
        cur = s;
    }
}

/// A zone walk to realize (query-time attach walks and compiled walk arcs alike).
struct ZWalk {
    zone: u32,
    level: u32,
    chain: Vec<u32>,
    a: P3,
    b: P3,
    la: P2,
    lb: P2,
    time: f64,
    /// Horizontal length of the taut walk(s) `time` was computed for.
    len: f64,
}

enum Item {
    Walk(ZWalk),
    Arc(u32, f64, f64),
}

fn assemble(
    g: &Graph,
    cp: &CompiledProfile,
    ctx: &mut QueryCtx,
    legs: &[Leg],
    ov: Option<&Overlay>,
    r: &mut Route,
) {
    // Waits of slowed elements (closures) are real time: they count in durations.
    let wait = |a: usize| ov.map_or(0.0, |o| o.wait(a as u32));
    // Legs -> items; consecutive walks in one zone merge into one walk over the joined channel,
    // so passing a zone node (door end, anchor) does not bend the path towards it.
    let mut items: Vec<Item> = vec![];
    for leg in legs {
        let w = match leg {
            Leg::Stay => continue,
            Leg::Walk {
                c0,
                c1,
                a,
                b,
                la,
                lb,
                time,
                len,
            } => {
                let chain = ctx.pool[*c0..*c1].to_vec();
                let t0 = chain[0] as usize;
                ZWalk {
                    zone: g.mesh.tri_zone[t0],
                    level: g.mesh.tri_level[t0],
                    chain,
                    a: *a,
                    b: *b,
                    la: *la,
                    lb: *lb,
                    time: *time as f64,
                    len: *len as f64,
                }
            }
            Leg::Arc { arc, f0, f1 } => {
                let (f, rev) = g.fact_of(*arc as usize);
                if f.kind != FactKind::Walk {
                    items.push(Item::Arc(*arc, *f0, *f1));
                    continue;
                }
                let mut chain = g.chains[f.c0 as usize..f.c1 as usize].to_vec();
                let (tail, head) = if rev {
                    chain.reverse();
                    (f.nb, f.na)
                } else {
                    (f.na, f.nb)
                };
                let (na, nb) = (&g.nodes[tail as usize], &g.nodes[head as usize]);
                let lead = |n: &crate::graph::Node| [n.lead[0] as f64, n.lead[1] as f64];
                ZWalk {
                    zone: f.zone,
                    level: f.level,
                    chain,
                    a: na.xyz,
                    b: nb.xyz,
                    la: lead(na),
                    lb: lead(nb),
                    time: cp.arc_time[*arc as usize] as f64 + wait(*arc as usize),
                    len: g.pieces[f.p0 as usize..f.p1 as usize]
                        .iter()
                        .map(|p| p.len as f64)
                        .sum(),
                }
            }
        };
        if let Some(Item::Walk(prev)) = items.last_mut()
            && prev.zone == w.zone
        {
            join_chain(&mut prev.chain, &w.chain);
            prev.b = w.b;
            prev.lb = w.lb;
            prev.time += w.time;
            prev.len += w.len;
            continue;
        }
        items.push(Item::Walk(w));
    }
    let mut pts: Vec<P3> = vec![];
    let mut surf: Vec<u32> = vec![];
    for item in &items {
        pts.clear();
        surf.clear();
        match item {
            Item::Walk(w) => {
                // Walked zones are drawn with the on-foot profile's clearance and corners.
                let drawp = match &cp.foot_p {
                    Some(fp) if cp.is_foot(g.mesh.tri_surf[w.chain[0] as usize]) => fp.as_ref(),
                    _ => &cp.p,
                };
                realize(
                    &g.mesh,
                    &w.chain,
                    w.a,
                    w.b,
                    w.la,
                    w.lb,
                    drawp,
                    &mut ctx.wb,
                    &mut pts,
                    &mut surf,
                );
                // The time was computed for the taut walk(s); the drawn walk keeps away from
                // walls and rounds corners (or skips a node when walks were merged), so the
                // duration follows its length.
                let drawn = poly_len2(&pts);
                let time = if w.len > 0.05 && drawn > 0.0 {
                    w.time * (drawn / w.len).clamp(0.5, 2.0)
                } else {
                    w.time
                };
                walk_segments(g, cp, &surf, &pts, time, w.level, r);
                r.duration += time;
            }
            Item::Arc(arc, f0, f1) => {
                let a = *arc as usize;
                let (f, rev) = g.fact_of(a);
                let frac = (f1 - f0).max(0.0);
                let time = (cp.arc_time[a] as f64 + wait(a)) * frac;
                r.duration += time;
                let mut geom: Vec<P3> = g.geom[f.g0 as usize..f.g1 as usize].to_vec();
                if rev {
                    geom.reverse();
                }
                cut(&geom, *f0, *f1, &mut pts);
                if poly_len3(&pts) < 1e-6 && (*f1 - *f0) < 1.0 {
                    continue; // zero-length partial edge (endpoint snapped onto a junction)
                }
                let from = push_coords(&mut r.coords, &pts);
                let mode_elem = if f.elem != NONE {
                    f.elem
                } else {
                    g.pieces[f.p0 as usize].elem
                };
                let access = g.elem_access(mode_elem);
                let (kind, typ, id, class, props, level) = if f.elem != NONE {
                    let e = &g.elems[f.elem as usize];
                    let typ = match f.kind {
                        FactKind::Edge => "edge",
                        FactKind::Portal => "portal",
                        FactKind::Vertical => "vertical",
                        _ => "walk",
                    };
                    let kind = if typ == "edge" {
                        "edge".to_string()
                    } else {
                        g.str(e.sub).unwrap_or(typ).to_string()
                    };
                    (
                        kind,
                        typ,
                        e.id.clone(),
                        g.str(e.class).map(String::from),
                        g.props(f.elem),
                        f.level,
                    )
                } else {
                    // Attach connector: walked on the surface (or portal) of its piece.
                    let pc = &g.pieces[f.p0 as usize];
                    let e = &g.elems[pc.elem as usize];
                    (
                        "connector".into(),
                        "connector",
                        e.id.clone(),
                        g.str(e.class).map(String::from),
                        None,
                        f.level,
                    )
                };
                let space = if f.elem == NONE {
                    g.str(g.elems[g.pieces[f.p0 as usize].elem as usize].space)
                        .map(String::from)
                } else {
                    None
                };
                r.segments.push(Segment {
                    kind,
                    typ,
                    id,
                    class,
                    space,
                    level: g.levels.get(level as usize).map(|l| l.id.clone()),
                    from_idx: from,
                    to_idx: r.coords.len() - 1,
                    length: poly_len3(&pts),
                    duration: time,
                    props,
                    access,
                    mode: cp.mode_of(mode_elem),
                });
            }
        }
    }

    junctions(g, cp, r);
    mode_legs(r);
    r.length = r.segments.iter().map(|s| s.length).sum();
    for w in r.coords.windows(2) {
        let dz = w[1][2] - w[0][2];
        if dz > 0.0 {
            r.climb += dz;
        } else {
            r.descent -= dz;
        }
    }
    for s in &r.segments {
        if let Some(sp) = &s.space
            && r.spaces.last() != Some(sp)
        {
            r.spaces.push(sp.clone());
        }
    }
}

/// Ride legs shorter than this between walks are walked (m).
const SHORT_RIDE: f64 = 25.0;

/// Profiles with `on_foot`: doors, stairs, lifts and connectors next to a walked stretch are
/// walked too (get off before the door), short rides between walks are walked, and runs of
/// one mode become the route's `legs`.
pub(crate) fn mode_legs(r: &mut Route) {
    r.legs.clear();
    let n = r.segments.len();
    if n == 0 || r.segments.iter().all(|s| s.mode.is_none()) {
        return;
    }
    let link = |s: &Segment| !matches!(s.typ, "walk" | "edge");
    let mut i = 0;
    while i < n {
        if !link(&r.segments[i]) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n && link(&r.segments[j]) {
            j += 1;
        }
        let walked = |k: Option<usize>| k.is_some_and(|k| r.segments[k].mode == Some("walk"));
        if walked(i.checked_sub(1)) || walked((j < n).then_some(j)) {
            for s in &mut r.segments[i..j] {
                s.mode = Some("walk");
            }
        }
        i = j;
    }
    let runs = |segs: &[Segment]| -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = vec![];
        for (k, s) in segs.iter().enumerate() {
            match out.last_mut() {
                Some(last) if segs[last.0].mode == s.mode => last.1 = k + 1,
                _ => out.push((k, k + 1)),
            }
        }
        out
    };
    let rs = runs(&r.segments);
    for (k, &(a, b)) in rs.iter().enumerate() {
        let len: f64 = r.segments[a..b].iter().map(|s| s.length).sum();
        if r.segments[a].mode == Some("ride") && len < SHORT_RIDE && k > 0 && k + 1 < rs.len() {
            for s in &mut r.segments[a..b] {
                s.mode = Some("walk");
            }
        }
    }
    for (a, b) in runs(&r.segments) {
        let segs = &r.segments[a..b];
        r.legs.push(ModeLeg {
            mode: segs[0].mode.unwrap_or("walk"),
            from_idx: segs[0].from_idx,
            to_idx: segs[segs.len() - 1].to_idx,
            s0: a,
            s1: b,
            length: segs.iter().map(|s| s.length).sum(),
            duration: segs.iter().map(|s| s.duration).sum(),
        });
    }
}

/// Longest leg removed as a U-turn spike next to an edge or connector (m).
const SPIKE: f64 = 1.0;

/// Clean-up at segment junctions (graph nodes):
/// - U-turn spikes: where the path enters and leaves a node with a heading change over 150°
///   (attached to a node and came back), the junction point is dropped so the shorter leg's
///   back-and-forth disappears, if the shortcut stays valid: inside the zone's walkable
///   triangles for a walk, or a spike of at most `SPIKE` m next to an edge or connector.
///   Portals and verticals (stair flights turn back for real) are never touched.
/// - Round corners (`corner: round`) between two edges: a fillet of at most `corner_radius`
///   deviation, its tangent points at most half way along the adjacent legs.
fn junctions(g: &Graph, cp: &CompiledProfile, r: &mut Route) {
    if r.segments.len() < 2 {
        return;
    }
    // Streams over the segments: `cur` is segment k (already trimmed at its start by junction
    // k - 1), `next` segment k + 1; junction k may change both, then `cur` is emitted.
    let segs = std::mem::take(&mut r.segments);
    let old = std::mem::take(&mut r.coords);
    r.coords.reserve(old.len() + 16);
    r.segments.reserve(segs.len());
    let trimmable = |t: &str| matches!(t, "walk" | "edge" | "connector");
    let n = segs.len();
    let mut cur: Vec<P3> = old[segs[0].from_idx..=segs[0].to_idx].to_vec();
    let mut next: Vec<P3> = Vec::with_capacity(cur.len());
    let (mut cur_t, mut next_t) = (false, false);
    let mut arc: Vec<P2> = vec![];
    let mut arc3: Vec<P3> = vec![];
    let mut carry = 0.0;
    let mut segs = segs.into_iter().peekable();
    for _ in 0..n {
        let sa = segs.next().unwrap();
        next.clear();
        if let Some(sb) = segs.peek() {
            next.extend_from_slice(&old[sb.from_idx..=sb.to_idx]);
            next_t = false;
            'j: {
                if sa.to_idx != sb.from_idx || !trimmable(sa.typ) || !trimmable(sb.typ) {
                    break 'j;
                }
                let (na, nb) = (cur.len(), next.len());
                if na < 2 || nb < 2 {
                    break 'j;
                }
                let (a, j, b) = (cur[na - 2], cur[na - 1], next[1]);
                let (din, dout) = (sub(xy(j), xy(a)), sub(xy(b), xy(j)));
                let (la, lb) = (len(din), len(dout));
                if la < 1e-6 || lb < 1e-6 {
                    break 'j;
                }
                let cos = dot(din, dout) / (la * lb);
                if cos < -0.866 {
                    // The long leg's segment gets the shortcut (from the far end of the short
                    // leg).
                    let (short_is_cur, long, from, to) = if la <= lb {
                        (true, sb, a, b)
                    } else {
                        (false, &sa, b, a)
                    };
                    let ok = match long.typ {
                        "walk" => long
                            .level
                            .as_deref()
                            .and_then(|l| g.level_idx(l))
                            .and_then(|l| g.tri_at(l, xy(from)))
                            .and_then(|t| g.mesh.seg_inside(t, xy(from), xy(to)))
                            .is_some(),
                        _ => la.min(lb) <= SPIKE,
                    };
                    if !ok {
                        break 'j;
                    }
                    if short_is_cur {
                        cur.pop();
                        next[0] = a;
                    } else {
                        next.remove(0);
                        *cur.last_mut().unwrap() = b;
                    }
                    cur_t = true;
                    next_t = true;
                } else if cp.p.corner == Corner::Round
                    && sa.typ == "edge"
                    && sb.typ == "edge"
                    && cos < 0.9986
                {
                    // Fillet; radius reduced so the arc stays within corner_radius of the
                    // corner.
                    let th = cos.clamp(-1.0, 1.0).acos();
                    let dev = (0.5 * th).tan() * (0.25 * th).tan();
                    let rad = cp.p.corner_radius * (1.0 / dev.max(1e-12)).min(1.0);
                    arc.clear();
                    fillet(xy(a), xy(j), xy(b), rad, &mut arc);
                    if arc.len() < 2 {
                        break 'j;
                    }
                    // z along the legs at the tangent points, linear over the arc.
                    let zon = |p: P2, q: P3, s: P3| {
                        let l = dist(xy(q), xy(s));
                        if l > 1e-12 {
                            q[2] + (s[2] - q[2]) * dist(xy(q), p) / l
                        } else {
                            q[2]
                        }
                    };
                    let m = arc.len() - 1;
                    let (z0, z1) = (zon(arc[0], j, a), zon(arc[m], j, b));
                    arc3.clear();
                    arc3.extend(
                        arc.iter()
                            .enumerate()
                            .map(|(i, p)| [p[0], p[1], z0 + (z1 - z0) * i as f64 / m as f64]),
                    );
                    let mid = m / 2;
                    cur.pop();
                    cur.extend_from_slice(&arc3[..=mid]);
                    next.splice(0..1, arc3[mid..].iter().copied());
                    cur_t = true;
                    next_t = true;
                }
            }
        }
        // Emit segment k; one left without length hands its duration to a neighbour.
        let mut s = sa;
        if cur_t && (cur.len() < 2 || poly_len3(&cur) < 1e-9) {
            match r.segments.last_mut() {
                Some(prev) => prev.duration += s.duration,
                None => carry += s.duration,
            }
        } else {
            s.duration += std::mem::take(&mut carry);
            s.from_idx = push_coords(&mut r.coords, &cur);
            s.to_idx = r.coords.len() - 1;
            if cur_t {
                s.length = poly_len3(&cur);
            }
            r.segments.push(s);
        }
        std::mem::swap(&mut cur, &mut next);
        cur_t = next_t;
    }
    if let Some(last) = r.segments.last_mut() {
        last.duration += carry;
    }
}

/// Where a locator resolved to (for diagnostics / the `debug` route option).
#[derive(Serialize, Debug, Clone, Default)]
pub struct Located {
    /// surface | edge | node | portal | poi
    pub kind: &'static str,
    /// Snapped point (surface / edge) or the first node's position.
    pub at: Option<P3>,
    /// Horizontal distance from the requested `at` (when one was given).
    pub snap_dist: Option<f64>,
    pub level: Option<String>,
    pub zone: Option<u32>,
    pub tri: Option<u32>,
    /// Surface element id and its space / class (surface hits).
    pub surface: Option<String>,
    pub space: Option<String>,
    pub class: Option<String>,
    /// Edge element id and fraction along the snapped graph edge (edge hits).
    pub edge: Option<String>,
    pub edge_frac: Option<f64>,
    /// Graph nodes the locator starts from (node / portal locators).
    pub nodes: Vec<u32>,
}

/// Resolve a locator as `route` does, without searching.
pub fn locate(g: &Graph, cp: &CompiledProfile, loc: &Loc) -> Result<Located, String> {
    let lvl = |l: u32| g.levels.get(l as usize).map(|x| x.id.clone());
    let mut out = match g.resolve(loc, cp)? {
        End::Tri(t, p) => {
            let e = &g.elems[g.mesh.tri_surf[t as usize] as usize];
            Located {
                kind: "surface",
                at: Some(p),
                level: lvl(g.mesh.tri_level[t as usize]),
                zone: Some(g.mesh.tri_zone[t as usize]),
                tri: Some(t),
                surface: Some(e.id.clone()),
                space: g.str(e.space).map(str::to_string),
                class: g.str(e.class).map(str::to_string),
                ..Default::default()
            }
        }
        End::Edge(f, s, p) => {
            let fc = &g.facts[f as usize];
            let e = &g.elems[fc.elem as usize];
            Located {
                kind: "edge",
                at: Some(p),
                level: lvl(fc.level),
                edge: Some(e.id.clone()),
                edge_frac: Some(s),
                class: g.str(e.class).map(str::to_string),
                nodes: vec![fc.na, fc.nb],
                ..Default::default()
            }
        }
        End::Nodes(ns) => {
            let n0 = &g.nodes[ns[0] as usize];
            Located {
                kind: if loc.node.is_some() { "node" } else { "portal" },
                at: Some(n0.xyz),
                level: lvl(n0.level),
                zone: (n0.zone != NONE).then_some(n0.zone),
                tri: (n0.tri != NONE).then_some(n0.tri),
                nodes: ns,
                ..Default::default()
            }
        }
        End::Pois(parts) => {
            // First part's position; every access node of the candidates.
            let at = parts[0].0.point(g);
            let mut nodes = vec![];
            for (e, _) in &parts {
                match e {
                    End::Nodes(ns) => nodes.extend(ns),
                    End::Edge(f, _, _) => {
                        let fc = &g.facts[*f as usize];
                        nodes.extend([fc.na, fc.nb]);
                    }
                    _ => {}
                }
            }
            Located {
                kind: "poi",
                at: Some(at),
                level: g.level_at(at),
                nodes,
                ..Default::default()
            }
        }
    };
    if let (Some(a), Some(p)) = (&loc.at, out.at)
        && loc.node.is_none()
        && loc.portal.is_none()
        && loc.space.is_none()
        && !loc.is_poi()
        && a.len() >= 2
    {
        out.snap_dist = Some(dist([a[0], a[1]], xy(p)));
    }
    Ok(out)
}
