//! Facts x profile -> per-arc f32 costs (seconds), cached per profile.

use crate::funnel::WalkStats;
use crate::geom::*;
use crate::graph::{ElemKind, Fact, FactKind, Graph, Piece};
use crate::profile::{Profile, RouteMode};
use rayon::prelude::*;

/// A profile resolved against one graph: per-element factors and per-arc costs.
#[derive(Clone)]
pub struct CompiledProfile {
    pub p: Profile,
    /// Preference factor per element (INFINITY = forbidden), and the same going up / down
    /// (`kind_factor_up` / `kind_factor_down`; equal to `factor` where none applies).
    pub factor: Vec<f32>,
    pub factor_up: Vec<f32>,
    pub factor_down: Vec<f32>,
    /// Walking speed per element (m/s): element override or profile speed.
    pub speed: Vec<f32>,
    /// Kind penalty per element (portal/vertical kind) or node penalty.
    pub kpen: Vec<f32>,
    /// Tag penalty per element (`tag_penalty`, preference only: added to costs, not durations).
    pub tpen: Vec<f32>,
    /// Total 3D length of each edge element (tag penalties are split over its pieces).
    pub edge_len: Vec<f32>,
    /// Cost (with preferences) and duration (real seconds) per arc.
    pub arc_cost: Vec<f32>,
    pub arc_time: Vec<f32>,
    /// Node has an arc usable by this profile (in or out).
    pub usable: Vec<bool>,
    /// Zone has a usable way in or out (door, vertical, edge, connector) for this profile;
    /// closed zones (rooms without doors) are left by `approach`.
    pub zone_open: Vec<bool>,
    /// Access groups granted (bit mask, `Graph::access_mask`); `u64::MAX` = unrestricted
    /// (`CompiledProfile::new`). Restricted variants come from `restrict` (`access.rs`).
    pub granted: u64,
    /// POIs not offered for lack of access (empty = none).
    pub poi_denied: Vec<bool>,
    /// Zones closed only for lack of access (empty = none): never `approach`ed.
    pub zone_locked: Vec<bool>,
    /// 1 / fastest admissible speed (0 disables the heuristic).
    pub inv_vmax: f32,
    /// Straight distance (m) that arcs faster than that speed (fixed `time`, zero-cost links)
    /// can cover in total; the heuristic subtracts it, so it stays admissible.
    pub reach: f32,
    pub max_grade: f64,
    /// Elements walked with the on-foot profile (`Profile::on_foot`): forbidden to this
    /// profile, allowed on foot. Their factors, speed, penalties, incline and grade limit are
    /// the on-foot profile's. Empty when the profile has no `on_foot`.
    pub foot: Vec<bool>,
    /// The resolved on-foot profile (drawing of walked walks, incline, grade limit).
    pub foot_p: Option<Box<Profile>>,
    /// `leave_tags`: per arc, the connected area of walked-without-the-bike elements it lies in
    /// (NONE elsewhere); per node, the area it touches. A search may use the arcs of the areas
    /// of its own sources and exits only (`allowed_areas`). Empty without `leave_tags`.
    pub arc_area: Vec<u32>,
    pub node_area: Vec<u32>,
}

/// Per-element lookups of one profile (`CompiledProfile::new`).
struct Elems {
    factor: Vec<f32>,
    factor_up: Vec<f32>,
    factor_down: Vec<f32>,
    speed: Vec<f32>,
    kpen: Vec<f32>,
    tpen: Vec<f32>,
}

fn lookup(map: &crate::profile::FactorMap, key: Option<&str>) -> Option<f64> {
    match key.and_then(|k| map.get(k)) {
        None => Some(1.0),
        Some(None) => None,
        Some(Some(x)) => Some(*x),
    }
}

/// Travel direction of a fact with signed rise `dz`: 1 up, -1 down, 0 level.
#[inline]
fn dir_of(dz: f64) -> i8 {
    if dz > 1e-3 {
        1
    } else if dz < -1e-3 {
        -1
    } else {
        0
    }
}

impl CompiledProfile {
    pub fn new(g: &Graph, p: &Profile) -> CompiledProfile {
        let ne = g.elems.len();
        let foot_p = p.foot().ok().flatten();
        let mut edge_len = vec![0.0f32; ne];
        if !p.tag_penalty.is_empty() || foot_p.as_ref().is_some_and(|f| !f.tag_penalty.is_empty()) {
            for f in &g.facts {
                if f.kind == FactKind::Edge && f.elem != NONE {
                    edge_len[f.elem as usize] += f.len3;
                }
            }
        }
        let Elems {
            mut factor,
            mut factor_up,
            mut factor_down,
            mut speed,
            mut kpen,
            mut tpen,
        } = Self::elems(g, p);
        // On foot where the profile may not go and the on-foot profile may.
        let mut foot = vec![];
        if let Some(fp) = &foot_p {
            let f = Self::elems(g, fp);
            foot = vec![false; ne];
            let any = |a: &[f32], b: &[f32], c: &[f32], i: usize| {
                a[i].is_finite() || b[i].is_finite() || c[i].is_finite()
            };
            for i in 0..ne {
                if !any(&factor, &factor_up, &factor_down, i)
                    && any(&f.factor, &f.factor_up, &f.factor_down, i)
                {
                    foot[i] = true;
                    factor[i] = f.factor[i];
                    factor_up[i] = f.factor_up[i];
                    factor_down[i] = f.factor_down[i];
                    speed[i] = f.speed[i];
                    kpen[i] = f.kpen[i];
                    tpen[i] = f.tpen[i];
                }
            }
        }
        // The mode closes whole element kinds, after the on-foot profile had its say.
        let closed = match p.mode {
            RouteMode::Mixed => None,
            RouteMode::EdgesOnly => Some(ElemKind::Surface),
            RouteMode::SurfacesOnly => Some(ElemKind::Edge),
        };
        if let Some(kind) = closed {
            for (i, e) in g.elems.iter().enumerate() {
                if e.kind == kind {
                    factor[i] = f32::INFINITY;
                    factor_up[i] = f32::INFINITY;
                    factor_down[i] = f32::INFINITY;
                    if let Some(f) = foot.get_mut(i) {
                        *f = false;
                    }
                }
            }
        }
        let mut cp = CompiledProfile {
            p: p.clone(),
            factor,
            factor_up,
            factor_down,
            speed,
            kpen,
            tpen,
            edge_len,
            arc_cost: vec![],
            arc_time: vec![],
            usable: vec![],
            zone_open: vec![],
            granted: u64::MAX,
            poi_denied: vec![],
            zone_locked: vec![],
            inv_vmax: 0.0,
            reach: 0.0,
            max_grade: p.incline.max.unwrap_or(f64::INFINITY),
            foot,
            foot_p: foot_p.map(Box::new),
            arc_area: vec![],
            node_area: vec![],
        };
        let nn = g.nodes.len();
        let both: Vec<(f32, f32)> = (0..nn as u32)
            .into_par_iter()
            .flat_map_iter(|u| {
                g.out_arcs(u)
                    .map(|a| (cp.arc_cost_of(g, a, true), cp.arc_cost_of(g, a, false)))
                    .collect::<Vec<_>>()
            })
            .collect();
        cp.arc_cost = both.iter().map(|x| x.0).collect();
        cp.arc_time = both.iter().map(|x| x.1).collect();
        cp.connectivity(g);
        cp.heuristic(g);
        cp.leave_areas(g);
        cp
    }

    /// `arc_area` / `node_area` from `leave_tags`: walked elements carrying one of them are
    /// "leave" elements; nodes on a leave arc are area nodes; areas are the components of
    /// area nodes over leave arcs and arcs between two area nodes (indoor doors, stairs).
    fn leave_areas(&mut self, g: &Graph) {
        let p = &self.p;
        if self.foot.is_empty() || p.leave_tags.is_empty() {
            return;
        }
        let leave: Vec<bool> = g
            .elems
            .iter()
            .enumerate()
            .map(|(i, e)| {
                self.foot[i]
                    && p.leave_tags.iter().any(|t| {
                        g.str(e.class) == Some(t.as_str())
                            || g.str(e.sub) == Some(t.as_str())
                            || e.tags.iter().any(|&x| g.str(x) == Some(t.as_str()))
                    })
            })
            .collect();
        let is_leave = |a: usize| -> bool {
            let (f, _) = g.fact_of(a);
            (f.elem != NONE && leave[f.elem as usize])
                || g.pieces[f.p0 as usize..f.p1 as usize]
                    .iter()
                    .any(|pc| pc.elem != NONE && leave[pc.elem as usize])
        };
        let nn = g.nodes.len();
        let mut arc_leave = vec![false; g.num_arcs()];
        // Nodes on a leave surface (a room's only door end has no walk arc) and on leave arcs.
        let mut area_node: Vec<bool> = g
            .nodes
            .iter()
            .map(|n| n.tri != NONE && leave[g.mesh.tri_surf[n.tri as usize] as usize])
            .collect();
        for u in 0..nn as u32 {
            for a in g.out_arcs(u) {
                if is_leave(a) {
                    arc_leave[a] = true;
                    area_node[u as usize] = true;
                    area_node[g.head[a] as usize] = true;
                }
            }
        }
        let mut parent: Vec<u32> = (0..nn as u32).collect();
        fn find(p: &mut [u32], mut x: u32) -> u32 {
            while p[x as usize] != x {
                p[x as usize] = p[p[x as usize] as usize];
                x = p[x as usize];
            }
            x
        }
        for u in 0..nn as u32 {
            for a in g.out_arcs(u) {
                let v = g.head[a];
                if arc_leave[a] || (area_node[u as usize] && area_node[v as usize]) {
                    let (ru, rv) = (find(&mut parent, u), find(&mut parent, v));
                    if ru != rv {
                        parent[ru as usize] = rv;
                    }
                }
            }
        }
        self.node_area = (0..nn as u32)
            .map(|n| {
                if area_node[n as usize] {
                    find(&mut parent, n)
                } else {
                    NONE
                }
            })
            .collect();
        let mut arc_area = vec![NONE; g.num_arcs()];
        for u in 0..nn as u32 {
            for a in g.out_arcs(u) {
                if arc_leave[a] {
                    arc_area[a] = self.node_area[u as usize];
                }
            }
        }
        self.arc_area = arc_area;
    }

    /// Areas a search from `sources` to `exits` may walk in (`arc_area`).
    pub fn allowed_areas(
        &self,
        sources: &[(u32, f32, u32)],
        exits: &[(u32, f32, u32)],
    ) -> Vec<u32> {
        if self.node_area.is_empty() {
            return vec![];
        }
        let mut out: Vec<u32> = sources
            .iter()
            .chain(exits)
            .map(|s| self.node_area[s.0 as usize])
            .filter(|&a| a != NONE)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Factors, speeds and penalties of every element for one profile.
    fn elems(g: &Graph, p: &Profile) -> Elems {
        let ne = g.elems.len();
        let mut factor = vec![1.0f32; ne];
        let mut factor_up = vec![1.0f32; ne];
        let mut factor_down = vec![1.0f32; ne];
        let mut speed = vec![p.speed as f32; ne];
        let mut kpen = vec![0.0f32; ne];
        let mut tpen = vec![0.0f32; ne];
        for (i, e) in g.elems.iter().enumerate() {
            let class = g.str(e.class);
            let sub = g.str(e.sub);
            let tags: Vec<&str> = e.tags.iter().filter_map(|&t| g.str(t)).collect();
            let has = |k: &str| class == Some(k) || sub == Some(k) || tags.contains(&k);
            let mut common = Some(e.cost_factor as f64);
            let mut mul = |x: Option<f64>| common = common.and_then(|a| x.map(|b| a * b));
            for t in &tags {
                mul(lookup(&p.tag_factor, Some(t)));
            }
            let (cls, knd) = (lookup(&p.class_factor, class), lookup(&p.kind_factor, sub));
            let mut ok = !p.avoid_tags.iter().any(|t| has(t));
            if matches!(e.kind, ElemKind::Surface | ElemKind::Edge) {
                ok &= p.require_tags.iter().all(|t| has(t));
            }
            ok &= !(p.step_free && !e.step_free);
            if let Some(mw) = p.min_width {
                ok &= !(e.width.is_finite() && (e.width as f64) < mw);
            }
            // Directional factors replace the kind factor (verticals) or the class factor
            // (edges) of their key.
            let key = match e.kind {
                ElemKind::Vertical => sub,
                ElemKind::Edge => class,
                _ => None,
            };
            let with = |map: &crate::profile::FactorMap| -> f32 {
                let (c, k) = match key.filter(|k| map.contains_key(*k)) {
                    Some(k) if e.kind == ElemKind::Vertical => (cls, lookup(map, Some(k))),
                    Some(k) => (lookup(map, Some(k)), knd),
                    None => (cls, knd),
                };
                match (common, c, k) {
                    (Some(a), Some(b), Some(d))
                        if ok && (a * b * d).is_finite() && a * b * d >= 0.0 =>
                    {
                        (a * b * d) as f32
                    }
                    _ => f32::INFINITY,
                }
            };
            factor[i] = with(&crate::profile::FactorMap::new());
            factor_up[i] = with(&p.kind_factor_up);
            factor_down[i] = with(&p.kind_factor_down);
            if let Some(cap) = p
                .tag_speed
                .iter()
                .filter(|(k, _)| has(k))
                .map(|(_, v)| *v)
                .reduce(f64::min)
            {
                speed[i] = speed[i].min(cap as f32);
            }
            if e.speed.is_finite() && e.speed > 0.0 && e.kind != ElemKind::Vertical {
                speed[i] = e.speed;
            }
            if !p.tag_penalty.is_empty() && e.kind != ElemKind::Surface {
                tpen[i] = tags
                    .iter()
                    .filter_map(|t| p.tag_penalty.get(*t))
                    .sum::<f64>() as f32;
            }
            kpen[i] = match e.kind {
                ElemKind::Node => e.penalty.max(0.0),
                _ => sub
                    .and_then(|s| p.kind_penalty.get(s))
                    .copied()
                    .unwrap_or(0.0) as f32,
            };
        }
        Elems {
            factor,
            factor_up,
            factor_down,
            speed,
            kpen,
            tpen,
        }
    }

    /// Element `e` is walked with the on-foot profile.
    #[inline]
    pub fn is_foot(&self, e: u32) -> bool {
        e != NONE && self.foot.get(e as usize).copied().unwrap_or(false)
    }

    /// Segment mode of element `e`: None without `on_foot`, else `walk` / `ride`.
    pub fn mode_of(&self, e: u32) -> Option<&'static str> {
        if self.foot.is_empty() {
            None
        } else if self.is_foot(e) {
            Some("walk")
        } else {
            Some("ride")
        }
    }

    /// Incline coefficients (up, down) and grade limit of element `e`.
    #[inline]
    pub(crate) fn incline_of(&self, e: u32) -> (f64, f64, f64) {
        match &self.foot_p {
            Some(f) if self.is_foot(e) => (
                f.incline.up,
                f.incline.down,
                f.incline.max.unwrap_or(f64::INFINITY),
            ),
            _ => (self.p.incline.up, self.p.incline.down, self.max_grade),
        }
    }

    /// `usable` and `zone_open` from `arc_cost`.
    pub(crate) fn connectivity(&mut self, g: &Graph) {
        let (nn, cp) = (g.nodes.len(), self);
        cp.usable = vec![false; nn];
        cp.zone_open = vec![false; g.zone_off.len().saturating_sub(1)];
        for u in 0..nn as u32 {
            for a in g.out_arcs(u) {
                if cp.arc_cost[a].is_finite() {
                    cp.usable[u as usize] = true;
                    cp.usable[g.head[a] as usize] = true;
                    if g.fact_of(a).0.kind != FactKind::Walk {
                        for n in [u, g.head[a]] {
                            let z = g.nodes[n as usize].zone;
                            if let Some(o) = cp.zone_open.get_mut(z as usize) {
                                *o = true;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Heuristic speed: the fastest physical motion of the profile (walking on surfaces and
    /// edges, stairs), so that `straight / vmax` underestimates every such arc (consistent).
    /// Arcs still faster than that (fixed `time`, zero-cost links, lifts with no wait) are not
    /// allowed to inflate vmax: their excess straight distance is summed into `reach`, which
    /// the heuristic subtracts. A* reopens nodes, so it stays exact either way.
    fn heuristic(&mut self, g: &Graph) {
        let mut vmax = 0.0f64;
        for (i, e) in g.elems.iter().enumerate() {
            let f = self.factor[i]
                .min(self.factor_up[i])
                .min(self.factor_down[i]) as f64;
            if !f.is_finite() {
                continue;
            }
            let v = match e.kind {
                ElemKind::Surface => self.speed[i] as f64,
                ElemKind::Edge if !e.time.is_finite() => self.speed[i] as f64,
                ElemKind::Vertical
                    if !e.time.is_finite()
                        && !matches!(g.str(e.sub), Some("lift" | "escalator")) =>
                {
                    let vs = if e.speed.is_finite() && e.speed > 0.0 {
                        e.speed as f64
                    } else {
                        1.0
                    };
                    self.speed[i] as f64 * vs
                }
                _ => continue,
            };
            vmax = vmax.max(v / f.max(1e-9));
        }
        if !(vmax.is_finite() && vmax > 0.0) {
            return;
        }
        // f32 rounding of costs: shave a little.
        let inv = 0.999 / vmax;
        let mut excess = vec![0.0f64; g.facts.len()];
        for u in 0..g.nodes.len() as u32 {
            let pu = g.node_xyz[u as usize];
            for a in g.out_arcs(u) {
                let c = self.arc_cost[a] as f64;
                if !c.is_finite() {
                    continue;
                }
                let pv = g.node_xyz[g.head[a] as usize];
                let d = (((pu[0] - pv[0]).powi(2)
                    + (pu[1] - pv[1]).powi(2)
                    + (pu[2] - pv[2]).powi(2)) as f64)
                    .sqrt();
                let x = d - c / inv;
                if x > 0.0 {
                    let k = (g.arc_fact[a] & !crate::graph::REV) as usize;
                    excess[k] = excess[k].max(x);
                }
            }
        }
        let reach: f64 = excess.iter().sum();
        self.inv_vmax = inv as f32;
        self.reach = if reach > 0.0 {
            (reach * 1.001 + 1e-3) as f32
        } else {
            0.0
        };
    }

    #[inline]
    pub fn ok(&self, e: u32) -> bool {
        self.factor[e as usize].is_finite()
    }

    /// Element usable in some direction (level, up or down).
    pub fn elem_usable(&self, e: u32) -> bool {
        let e = e as usize;
        e >= self.factor.len()
            || self.factor[e].is_finite()
            || self.factor_up[e].is_finite()
            || self.factor_down[e].is_finite()
    }

    /// Factor of element `e` travelled in direction `dir` (1 up, -1 down, 0 level); with
    /// `pref == false` only whether it is allowed (1 or INFINITY).
    #[inline]
    fn fac(&self, e: u32, pref: bool, dir: i8) -> f64 {
        let f = match dir {
            1 => self.factor_up[e as usize],
            -1 => self.factor_down[e as usize],
            _ => self.factor[e as usize],
        };
        if !f.is_finite() {
            f64::INFINITY
        } else if pref {
            f as f64
        } else {
            1.0
        }
    }

    /// Walking time over pieces (forward, or reversed), INFINITY if forbidden / too steep.
    pub fn pieces_cost(
        &self,
        pieces: &[Piece],
        max_up: f64,
        max_down: f64,
        rev: bool,
        pref: bool,
        dir: i8,
    ) -> f64 {
        let (mu, md) = if rev {
            (max_down, max_up)
        } else {
            (max_up, max_down)
        };
        // Grade limit: of every profile (riding / on foot) with a piece here.
        let lim = if self.foot.is_empty() {
            self.max_grade
        } else {
            pieces
                .iter()
                .map(|pc| self.incline_of(pc.elem).2)
                .fold(f64::INFINITY, f64::min)
        };
        if mu > lim || md > lim {
            return f64::INFINITY;
        }
        let mut t = 0.0;
        for pc in pieces {
            let (up, down, _) = self.incline_of(pc.elem);
            let (cl, de) = if rev {
                (pc.descent, pc.climb)
            } else {
                (pc.climb, pc.descent)
            };
            let eff = pc.len as f64 + up * cl as f64 + down * de as f64;
            t += eff / self.speed[pc.elem as usize] as f64 * self.fac(pc.elem, pref, dir);
        }
        t
    }

    pub fn walk_cost(&self, pieces: &[Piece], st: &WalkStats, pref: bool) -> f64 {
        self.pieces_cost(pieces, st.max_up, st.max_down, false, pref, 0)
    }

    pub fn fact_cost(&self, g: &Graph, f: &Fact, rev: bool, pref: bool) -> f64 {
        let pieces = &g.pieces[f.p0 as usize..f.p1 as usize];
        let dir = match f.kind {
            FactKind::Vertical | FactKind::Edge => dir_of(if rev { -f.dz } else { f.dz } as f64),
            _ => 0,
        };
        if f.elem != NONE && !self.fac(f.elem, false, dir).is_finite() {
            return f64::INFINITY;
        }
        match f.kind {
            FactKind::Vertical => {
                let e = &g.elems[f.elem as usize];
                let fac = self.fac(f.elem, pref, dir);
                let dz = f.dz.abs() as f64;
                let vs = |d: f64| {
                    if e.speed.is_finite() && e.speed > 0.0 {
                        e.speed as f64
                    } else {
                        d
                    }
                };
                let t = if f.time.is_finite() {
                    f.time as f64
                } else {
                    match g.str(e.sub) {
                        Some("lift") => e.wait as f64 + dz / vs(1.0),
                        Some("escalator") => e.wait as f64 + dz / vs(0.5),
                        Some("ramp") => {
                            let (mu, md) = if rev {
                                (f.max_down, f.max_up)
                            } else {
                                (f.max_up, f.max_down)
                            };
                            let (up, down, lim) = self.incline_of(f.elem);
                            if mu as f64 > lim || md as f64 > lim {
                                return f64::INFINITY;
                            }
                            let pc = &pieces[0];
                            let (cl, de) = if rev {
                                (pc.descent, pc.climb)
                            } else {
                                (pc.climb, pc.descent)
                            };
                            (pc.len as f64 + up * cl as f64 + down * de as f64)
                                / (self.speed[f.elem as usize] as f64 * vs(1.0))
                        }
                        _ => f.len3 as f64 / (self.speed[f.elem as usize] as f64 * vs(1.0)),
                    }
                };
                t * fac + self.kpen[f.elem as usize] as f64 + self.tag_pen(f, pref)
            }
            _ => {
                let mut t = if f.time.is_finite() {
                    f.time as f64 * self.fac(f.elem, pref, dir)
                } else {
                    self.pieces_cost(pieces, f.max_up as f64, f.max_down as f64, rev, pref, dir)
                };
                t += f.pen as f64;
                if f.elem != NONE {
                    t += self.kpen[f.elem as usize] as f64;
                }
                t + self.tag_pen(f, pref)
            }
        }
    }

    /// Tag penalty of a fact (portal, vertical hop, edge piece by length share); 0 for durations.
    #[inline]
    fn tag_pen(&self, f: &Fact, pref: bool) -> f64 {
        if !pref || f.elem == NONE || self.tpen.is_empty() {
            return 0.0;
        }
        let tp = self.tpen[f.elem as usize];
        if tp == 0.0 {
            return 0.0;
        }
        match f.kind {
            FactKind::Portal | FactKind::Vertical => tp as f64,
            FactKind::Edge => {
                let total = self.edge_len[f.elem as usize];
                if total > 0.0 {
                    (tp * (f.len3 / total).min(1.0)) as f64
                } else {
                    tp as f64
                }
            }
            _ => 0.0,
        }
    }

    pub(crate) fn arc_cost_of(&self, g: &Graph, a: usize, pref: bool) -> f32 {
        let (f, rev) = g.fact_of(a);
        let mut c = self.fact_cost(g, f, rev, pref);
        let e = g.nodes[g.head[a] as usize].elem;
        if e != NONE {
            c = if self.ok(e) {
                c + self.kpen[e as usize] as f64
                    + if pref {
                        self.tpen[e as usize] as f64
                    } else {
                        0.0
                    }
            } else {
                f64::INFINITY
            };
        }
        c as f32
    }

    /// Profile channel weight of a triangle (INFINITY = not walkable for this profile).
    #[inline]
    pub fn tri_weight(&self, g: &Graph, t: u32) -> f32 {
        let s = g.mesh.tri_surf[t as usize];
        let lim = if self.foot.is_empty() {
            self.max_grade
        } else {
            self.incline_of(s).2
        };
        if (g.mesh.tri_slope[t as usize] as f64) > lim + 1e-3 {
            return f32::INFINITY;
        }
        self.factor[s as usize]
    }
}
