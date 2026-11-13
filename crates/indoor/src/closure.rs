//! Live closures: doors, edges, lifts, nodes, spaces or areas closed, or
//! slowed by a wait, for one query, without a rebuild.
//!
//! A closure list compiles into an `Overlay` (`Overlay::new`, cached per list by
//! `Router::overlay`): the affected arcs, sorted, each with its extra cost (INFINITY = closed),
//! and the triangles polygon closures cover. A* adds the extra cost to those arcs only; query
//! time walks (the route's first and last stretch) do not cross covered triangles. Without an
//! overlay nothing changes. Closures only add cost, so the heuristic stays admissible.
//!
//! The route reports the closures it had to respect (`ClosureHit`): the ones it passes with a
//! wait, and, when the search met a closed or slowed arc, the ones on the route the same query
//! takes without closures (one more search without geometry, only then).

use crate::funnel::{WalkBuf, measure};
use crate::geom::*;
use crate::graph::{ElemKind, FactKind, Graph, REV};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One closure of a route query (`closures`): what is closed, or slowed by `wait` seconds.
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Closure {
    /// Reported back in `ClosureHit`; free text.
    pub id: Option<String>,
    pub reason: Option<String>,
    /// Portal, vertical, edge, node or surface ids.
    pub elements: Vec<String>,
    /// Space ids: the walks over their surfaces (a closed room is not walked through; routes
    /// from or to a point in it still reach it).
    pub spaces: Vec<String>,
    /// An area (input coordinates): every link whose drawn line enters it.
    pub polygon: Option<Polygon>,
    /// The polygon's floor (all floors when absent).
    pub level: Option<String>,
    /// Seconds added per pass (per hop of a vertical, by length share along an edge) instead
    /// of closing.
    pub wait: Option<f64>,
}

/// `[[x, y], ...]` (one ring) or `[[[x, y], ...], ...]` (outer ring and holes).
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum Polygon {
    Ring(Vec<Vec<f64>>),
    Rings(Vec<Vec<Vec<f64>>>),
}

impl Polygon {
    fn rings(&self) -> Vec<Vec<P2>> {
        let ring = |r: &Vec<Vec<f64>>| -> Vec<P2> {
            r.iter()
                .filter(|p| p.len() >= 2)
                .map(|p| [p[0], p[1]])
                .collect()
        };
        match self {
            Polygon::Ring(r) => vec![ring(r)],
            Polygon::Rings(rs) => rs.iter().map(ring).collect(),
        }
        .into_iter()
        .filter(|r| r.len() >= 3)
        .collect()
    }
}

/// An area a single query keeps out of (`avoid` on a route query).
#[derive(Deserialize, Clone, Debug)]
pub struct Avoid {
    pub polygon: Polygon,
    #[serde(default)]
    pub level: Option<String>,
}

impl From<Avoid> for Closure {
    fn from(a: Avoid) -> Closure {
        Closure {
            polygon: Some(a.polygon),
            level: a.level,
            ..Closure::default()
        }
    }
}

/// A closure the route respected (`Route::closures`).
#[derive(Serialize, Clone, Debug)]
pub struct ClosureHit {
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `detour`: the route without closures goes through it, this one goes around.
    /// `wait`: the route passes it and waits `wait` seconds.
    pub effect: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<f64>,
    /// Where it is: the closed element, or the middle of the area.
    pub at: P3,
    pub level: Option<String>,
}

struct Item {
    id: Option<String>,
    reason: Option<String>,
    wait: Option<f64>,
    at: Option<P3>,
    level: Option<u32>,
}

struct Area {
    k: u32,
    rings: Vec<Vec<P2>>,
    bb: [f64; 4],
    level: Option<u32>,
}

/// A closure list compiled against one graph.
pub struct Overlay {
    /// (arc, extra cost, closure index), sorted by arc.
    arcs: Vec<(u32, f32, u32)>,
    /// (triangle, area index) for the triangles polygon closures cover, sorted.
    tris: Vec<(u32, u32)>,
    areas: Vec<Area>,
    items: Vec<Item>,
    /// Ids that name nothing in the graph.
    pub warnings: Vec<String>,
}

impl Overlay {
    pub fn new(g: &Graph, list: &[Closure]) -> Overlay {
        let mut warnings = vec![];
        let mut items: Vec<Item> = list
            .iter()
            .map(|c| Item {
                id: c.id.clone(),
                reason: c.reason.clone(),
                wait: c.wait.filter(|w| w.is_finite() && *w >= 0.0),
                at: None,
                level: None,
            })
            .collect();
        let name = |c: &Closure| c.id.clone().unwrap_or_else(|| "(avoid)".into());
        // Element -> closures naming it (directly or through its space).
        let mut by_elem: HashMap<u32, Vec<u32>> = HashMap::new();
        let wanted: HashMap<&str, ()> = list
            .iter()
            .flat_map(|c| c.elements.iter().chain(&c.spaces))
            .map(|s| (s.as_str(), ()))
            .collect();
        if !wanted.is_empty() {
            let mut idx: HashMap<&str, Vec<u32>> = HashMap::new();
            let mut space: HashMap<&str, Vec<u32>> = HashMap::new();
            for (i, e) in g.elems.iter().enumerate() {
                if wanted.contains_key(e.id.as_str()) {
                    idx.entry(e.id.as_str()).or_default().push(i as u32);
                }
                if e.kind == ElemKind::Surface
                    && let Some(s) = g.str(e.space).filter(|s| wanted.contains_key(s))
                {
                    space.entry(s).or_default().push(i as u32);
                }
            }
            for (k, c) in list.iter().enumerate() {
                for (ids, map) in [(&c.elements, &idx), (&c.spaces, &space)] {
                    for id in ids {
                        match map.get(id.as_str()) {
                            Some(es) => {
                                for &e in es {
                                    by_elem.entry(e).or_default().push(k as u32);
                                }
                            }
                            None => warnings.push(format!("closure {}: unknown {id}", name(c))),
                        }
                    }
                }
            }
        }
        let areas: Vec<Area> = list
            .iter()
            .enumerate()
            .filter_map(|(k, c)| {
                let rings = c.polygon.as_ref()?.rings();
                if rings.is_empty() {
                    warnings.push(format!("closure {}: polygon needs 3 points", name(c)));
                    return None;
                }
                let level = match &c.level {
                    Some(l) => match g.level_idx(l) {
                        Some(i) => Some(i),
                        None => {
                            warnings.push(format!("closure {}: unknown level {l}", name(c)));
                            return None;
                        }
                    },
                    None => None,
                };
                let bb = bbox(rings[0].iter().copied());
                let n = rings[0].len() as f64;
                let (sx, sy) = rings[0]
                    .iter()
                    .fold((0.0, 0.0), |s, p| (s.0 + p[0], s.1 + p[1]));
                let z = level.map_or(0.0, |l| g.levels[l as usize].z);
                items[k].at = Some([sx / n, sy / n, z]);
                items[k].level = level;
                Some(Area {
                    k: k as u32,
                    rings,
                    bb,
                    level,
                })
            })
            .collect();

        // Triangles of the areas (query time walks go around them).
        let mut tris: Vec<(u32, u32)> = vec![];
        for (ai, a) in areas.iter().enumerate() {
            for (lv, grid) in g.tri_grid.iter().enumerate() {
                if a.level.is_some_and(|l| l != lv as u32) {
                    continue;
                }
                let base = g.level_tri[lv][0];
                grid.visit(a.bb, |k| {
                    let t = base + k;
                    if tri_meets(g.mesh.pts(t), a) {
                        tris.push((t, ai as u32));
                    }
                });
            }
        }
        tris.sort_unstable();
        tris.dedup();

        // Facts: their element, the surfaces they walk over, their line (walks: the taut line
        // through their triangles, measured only when one of them meets an area).
        let mut wb = WalkBuf::default();
        let mut edge_len: HashMap<u32, f32> = HashMap::new();
        for f in &g.facts {
            if f.kind == FactKind::Edge && by_elem.contains_key(&f.elem) {
                *edge_len.entry(f.elem).or_default() += f.len3;
            }
        }
        let cost = |k: u32, share: f32| -> f32 {
            match items[k as usize].wait {
                Some(w) => w as f32 * share,
                None => f32::INFINITY,
            }
        };
        let mut fact_x: Vec<(u32, f32, u32)> = vec![];
        let mut at: Vec<(u32, P3, u32)> = vec![];
        for (fi, f) in g.facts.iter().enumerate() {
            let mut x = 0.0f32;
            let mut first = u32::MAX;
            let mut add = |k: u32, c: f32, x: &mut f32| {
                *x += c;
                if first == u32::MAX || c.is_infinite() {
                    first = k;
                }
            };
            if let Some(ks) = by_elem.get(&f.elem) {
                let share = match f.kind {
                    FactKind::Edge => edge_len
                        .get(&f.elem)
                        .filter(|&&t| t > 0.0)
                        .map_or(1.0, |t| (f.len3 / t).min(1.0)),
                    _ => 1.0,
                };
                for &k in ks {
                    add(k, cost(k, share), &mut x);
                }
            }
            if matches!(f.kind, FactKind::Walk | FactKind::Attach) && !by_elem.is_empty() {
                let mut seen: Vec<u32> = vec![];
                for pc in &g.pieces[f.p0 as usize..f.p1 as usize] {
                    for &k in by_elem.get(&pc.elem).into_iter().flatten() {
                        if !seen.contains(&k) {
                            seen.push(k);
                            add(k, cost(k, 1.0), &mut x);
                        }
                    }
                }
            }
            if !areas.is_empty() {
                let chain = &g.chains[f.c0 as usize..f.c1 as usize];
                let line: &[P3] = if f.g1 > f.g0 {
                    &g.geom[f.g0 as usize..f.g1 as usize]
                } else if chain.iter().any(|&t| covered(&tris, t)) {
                    let (na, nb) = (g.nodes[f.na as usize].xyz, g.nodes[f.nb as usize].xyz);
                    measure(&g.mesh, chain, na, nb, &mut wb);
                    &wb.geom
                } else {
                    &[]
                };
                for a in &areas {
                    if a.level.is_none_or(|l| l == f.level) && line_enters(line, a) {
                        add(a.k, cost(a.k, 1.0), &mut x);
                    }
                }
            }
            if first != u32::MAX {
                fact_x.push((fi as u32, x, first));
                if f.elem != NONE && by_elem.contains_key(&f.elem) {
                    let line = &g.geom[f.g0 as usize..f.g1 as usize];
                    if let Some(&p) = line.get(line.len() / 2) {
                        at.push((f.elem, p, f.level));
                    }
                }
            }
        }
        // Arcs: their fact's extra, plus closed nodes (arcs in and out) and node waits (in).
        let nodes = by_elem
            .keys()
            .any(|&e| g.elems[e as usize].kind == ElemKind::Node);
        let mut arcs: Vec<(u32, f32, u32)> = vec![];
        let fx: HashMap<u32, (f32, u32)> = fact_x.iter().map(|&(f, x, k)| (f, (x, k))).collect();
        if !fx.is_empty() || nodes {
            for u in 0..g.nodes.len() as u32 {
                let tail = g.nodes[u as usize].elem;
                for a in g.out_arcs(u) {
                    let mut x = 0.0f32;
                    let mut first = u32::MAX;
                    if let Some(&(c, k)) = fx.get(&(g.arc_fact[a] & !REV)) {
                        x += c;
                        first = k;
                    }
                    if nodes {
                        let head = g.nodes[g.head[a] as usize].elem;
                        for &k in by_elem.get(&head).into_iter().flatten() {
                            let c = cost(k, 1.0);
                            x += c;
                            if first == u32::MAX || c.is_infinite() {
                                first = k;
                            }
                        }
                        for &k in by_elem.get(&tail).into_iter().flatten() {
                            if items[k as usize].wait.is_none() {
                                x = f32::INFINITY;
                                first = k;
                            }
                        }
                    }
                    if first != u32::MAX {
                        arcs.push((a as u32, x, first));
                    }
                }
            }
        }
        // Where each element closure is: its element's line, or the node.
        for (e, ks) in &by_elem {
            let el = &g.elems[*e as usize];
            let pos = at
                .iter()
                .find(|x| x.0 == *e)
                .map(|x| (x.1, x.2))
                .or_else(|| {
                    (el.kind == ElemKind::Node)
                        .then(|| g.nodes.iter().find(|n| n.elem == *e))
                        .flatten()
                        .map(|n| (n.xyz, n.level))
                });
            for &k in ks {
                let it = &mut items[k as usize];
                if it.at.is_none() {
                    if let Some((p, l)) = pos {
                        it.at = Some(p);
                        it.level = Some(l);
                    } else if el.kind == ElemKind::Surface
                        && let Some(&t) = g.space_tri.get(&el.id)
                    {
                        let c = g.mesh.tri_c[t as usize];
                        it.at = Some([c[0], c[1], g.mesh.tri_z[t as usize][0]]);
                        it.level = Some(g.mesh.tri_level[t as usize]);
                    }
                }
            }
        }
        Overlay {
            arcs,
            tris,
            areas,
            items,
            warnings,
        }
    }

    /// Extra cost of arc `a` (INFINITY = closed) and the closure causing it.
    #[inline]
    pub fn arc(&self, a: u32) -> Option<(f32, u32)> {
        self.arcs
            .binary_search_by_key(&a, |x| x.0)
            .ok()
            .map(|i| (self.arcs[i].1, self.arcs[i].2))
    }

    /// Finite extra seconds (waits) on arc `a`.
    #[inline]
    pub fn wait(&self, a: u32) -> f64 {
        match self.arc(a) {
            Some((x, _)) if x.is_finite() => x as f64,
            _ => 0.0,
        }
    }

    /// Triangle `t` is in a polygon closure that does not hold the query point `p` (a walk
    /// from a point in an area may leave it).
    #[inline]
    pub fn tri_closed(&self, t: u32, p: P2) -> bool {
        if !covered(&self.tris, t) {
            return false;
        }
        let i = self.tris.partition_point(|x| x.0 < t);
        self.tris[i..]
            .iter()
            .take_while(|x| x.0 == t)
            .any(|x| !in_rings(p, &self.areas[x.1 as usize].rings))
    }

    /// A query time walk on floor `level` (taut line `line`) from or to the query point `p`
    /// enters a polygon closure that `p` is not in (a walk out of an area is allowed).
    pub fn walk_closed(&self, line: &[P3], level: u32, p: P2) -> bool {
        self.areas.iter().any(|a| {
            a.level.is_none_or(|l| l == level) && !in_rings(p, &a.rings) && line_enters(line, a)
        })
    }

    /// Nothing affected: the query runs as without closures.
    pub fn is_empty(&self) -> bool {
        self.arcs.is_empty() && self.tris.is_empty()
    }

    /// The closures a route respected: those its arcs (`used`) pass with a wait, and those
    /// the route without closures (`free`) passes that this one does not.
    pub fn hits(&self, g: &Graph, used: &[u32], free: &[u32]) -> Vec<ClosureHit> {
        let mut out: Vec<(u32, &'static str)> = vec![];
        for &a in used {
            if let Some((x, k)) = self.arc(a)
                && x.is_finite()
                && !out.iter().any(|o| o.0 == k)
            {
                out.push((k, "wait"));
            }
        }
        for &a in free {
            if let Some((_, k)) = self.arc(a)
                && !out.iter().any(|o| o.0 == k)
            {
                out.push((k, "detour"));
            }
        }
        out.iter()
            .map(|&(k, effect)| {
                let it = &self.items[k as usize];
                ClosureHit {
                    id: it.id.clone(),
                    reason: it.reason.clone(),
                    effect,
                    wait: if effect == "wait" { it.wait } else { None },
                    at: it.at.unwrap_or([0.0; 3]),
                    level: it
                        .level
                        .and_then(|l| g.levels.get(l as usize))
                        .map(|l| l.id.clone()),
                }
            })
            .collect()
    }
}

#[inline]
fn covered(tris: &[(u32, u32)], t: u32) -> bool {
    !tris.is_empty() && tris.binary_search_by_key(&t, |x| x.0).is_ok()
}

/// The polyline enters the area (a point inside, or a segment crossing its boundary).
fn line_enters(line: &[P3], a: &Area) -> bool {
    if line.is_empty() {
        return false;
    }
    let lb = bbox(line.iter().map(|p| xy(*p)));
    if lb[2] < a.bb[0] || lb[0] > a.bb[2] || lb[3] < a.bb[1] || lb[1] > a.bb[3] {
        return false;
    }
    if line.iter().any(|p| in_rings(xy(*p), &a.rings)) {
        return true;
    }
    line.windows(2)
        .any(|w| crosses_rings(xy(w[0]), xy(w[1]), &a.rings))
}

/// The triangle and the area overlap.
fn tri_meets(t: [P2; 3], a: &Area) -> bool {
    if t.iter().any(|&p| in_rings(p, &a.rings)) {
        return true;
    }
    if a.rings[0].iter().any(|&p| in_tri(p, t[0], t[1], t[2], 0.0)) {
        return true;
    }
    (0..3).any(|i| crosses_rings(t[i], t[(i + 1) % 3], &a.rings))
}

fn crosses_rings(p: P2, q: P2, rings: &[Vec<P2>]) -> bool {
    rings
        .iter()
        .any(|r| (0..r.len()).any(|i| segs_cross(p, q, r[i], r[(i + 1) % r.len()])))
}

fn segs_cross(p: P2, q: P2, a: P2, b: P2) -> bool {
    let (d1, d2) = (cross(p, q, a), cross(p, q, b));
    let (d3, d4) = (cross(a, b, p), cross(a, b, q));
    ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_and_rings() {
        let sq = vec![vec![[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]]];
        assert!(segs_cross([-1.0, 1.0], [3.0, 1.0], [0.0, 0.0], [0.0, 2.0]));
        assert!(!segs_cross([-1.0, 3.0], [3.0, 3.0], [0.0, 0.0], [0.0, 2.0]));
        assert!(crosses_rings([-1.0, 1.0], [3.0, 1.0], &sq));
        let a = Area {
            k: 0,
            bb: bbox(sq[0].iter().copied()),
            rings: sq,
            level: None,
        };
        // A big triangle around the small square meets it although no corner is inside.
        assert!(tri_meets([[-10.0, -10.0], [10.0, -10.0], [0.0, 10.0]], &a));
        assert!(!tri_meets([[5.0, 5.0], [6.0, 5.0], [5.0, 6.0]], &a));
    }
}
