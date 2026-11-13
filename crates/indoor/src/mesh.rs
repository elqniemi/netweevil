//! Per-level constrained Delaunay navmesh (spade), triangle search and straight walks.

use crate::geom::*;
use serde::{Deserialize, Serialize};
use spade::{ConstrainedDelaunayTriangulation, HasPosition, Point2, Triangulation};
use std::collections::HashMap;

/// All walkable triangles of all levels. Triangles are CCW; `tri_nbr[t][i]` is the triangle
/// across edge `(v[i], v[i+1])` when that edge is passable, else `NONE`.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Mesh {
    pub verts: Vec<P3>,
    /// Vertex touches a wall (non-passable edge): funnel portals shrink there.
    pub wall: Vec<bool>,
    pub tri_v: Vec<[u32; 3]>,
    /// z of each triangle corner, taken from the triangle's own surface (neighbouring surfaces
    /// at different heights share CDT vertices but not their z).
    pub tri_z: Vec<[f64; 3]>,
    pub tri_nbr: Vec<[u32; 3]>,
    /// Surface element index.
    pub tri_surf: Vec<u32>,
    pub tri_zone: Vec<u32>,
    pub tri_level: Vec<u32>,
    pub tri_c: Vec<P2>,
    /// Static channel weight (surface cost factor).
    pub tri_w: Vec<f32>,
    /// Steepest gradient of the triangle plane.
    pub tri_slope: Vec<f32>,
}

impl Mesh {
    #[inline]
    pub fn p(&self, v: u32) -> P2 {
        let q = self.verts[v as usize];
        [q[0], q[1]]
    }
    #[inline]
    pub fn pts(&self, t: u32) -> [P2; 3] {
        let v = self.tri_v[t as usize];
        [self.p(v[0]), self.p(v[1]), self.p(v[2])]
    }
    #[inline]
    pub fn contains(&self, t: u32, p: P2, tol: f64) -> bool {
        let [a, b, c] = self.pts(t);
        in_tri(p, a, b, c, tol)
    }
    /// Corners of `t` with their per-triangle z.
    #[inline]
    pub fn pts3(&self, t: u32) -> [P3; 3] {
        let (v, z) = (self.tri_v[t as usize], self.tri_z[t as usize]);
        [0, 1, 2].map(|k| {
            let q = self.verts[v[k] as usize];
            [q[0], q[1], z[k]]
        })
    }
    #[inline]
    pub fn z_at(&self, t: u32, p: P2) -> f64 {
        let z = self.tri_z[t as usize];
        if z[0] == z[1] && z[1] == z[2] {
            return z[0];
        }
        let [a, b, c] = self.pts3(t);
        bary_z(p, a, b, c)
    }
    pub fn area(&self, t: u32) -> f64 {
        let [a, b, c] = self.pts(t);
        0.5 * cross(a, b, c)
    }
    /// Edge index of `t` shared with neighbour `n`.
    #[inline]
    pub fn edge_to(&self, t: u32, n: u32) -> usize {
        let r = self.tri_nbr[t as usize];
        if r[0] == n {
            0
        } else if r[1] == n {
            1
        } else {
            2
        }
    }

    /// Walk the straight segment p -> q through passable triangles starting in `t`.
    /// Returns the triangle containing `q`, or None if the segment hits a wall.
    pub fn seg_inside(&self, t: u32, p: P2, q: P2) -> Option<u32> {
        self.trace(t, p, q, |_, _, _| {})
    }

    /// As `seg_inside`, calling `cross(point, from_tri, to_tri)` at every triangle change.
    pub fn trace(
        &self,
        mut t: u32,
        p: P2,
        q: P2,
        mut on_cross: impl FnMut(P2, u32, u32),
    ) -> Option<u32> {
        let mut prev = NONE;
        let tol = 1e-9 * dist(p, q).max(1.0);
        for _ in 0..100_000 {
            let pts = self.pts(t);
            if in_tri(q, pts[0], pts[1], pts[2], 1e-7) {
                return Some(t);
            }
            let mut next = NONE;
            for i in 0..3 {
                let (u, w) = (pts[i], pts[(i + 1) % 3]);
                if cross(u, w, q) >= -1e-9 * dist(u, w) {
                    continue;
                }
                let (s1, s2) = (cross(p, q, u), cross(p, q, w));
                if (s1 > tol && s2 > tol) || (s1 < -tol && s2 < -tol) {
                    continue;
                }
                let n = self.tri_nbr[t as usize][i];
                if n != NONE && n != prev {
                    let s = line_cross_t(p, q, u, w).unwrap_or(0.5).clamp(0.0, 1.0);
                    on_cross(lerp(u, w, s), t, n);
                    next = n;
                    break;
                }
            }
            if next == NONE {
                return None;
            }
            prev = t;
            t = next;
        }
        None
    }
}

/// Generation-stamped Dijkstra over triangle adjacency (centroid distance x channel weight).
pub struct TriScratch {
    pub dist: Vec<f32>,
    pub parent: Vec<u32>,
    stamp: Vec<u32>,
    done: Vec<u32>,
    epoch: u32,
    heap: MinHeap,
}

impl TriScratch {
    pub fn new(n: usize) -> Self {
        TriScratch {
            dist: vec![0.0; n],
            parent: vec![0; n],
            stamp: vec![0; n],
            done: vec![0; n],
            epoch: 0,
            heap: MinHeap::new(),
        }
    }

    /// Run from `src` with per-triangle weight `w(t)` (infinite = blocked);
    /// `on_settle(t)` returns false to stop.
    pub fn run(
        &mut self,
        m: &Mesh,
        src: u32,
        w: impl Fn(u32) -> f32,
        on_settle: impl FnMut(u32) -> bool,
    ) {
        self.run_multi(m, &[src], w, on_settle)
    }

    /// As `run` from several source triangles at once (chains end at the nearest source).
    pub fn run_multi(
        &mut self,
        m: &Mesh,
        srcs: &[u32],
        w: impl Fn(u32) -> f32,
        mut on_settle: impl FnMut(u32) -> bool,
    ) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.stamp.fill(0);
            self.done.fill(0);
            self.epoch = 1;
        }
        let g = self.epoch;
        self.heap.clear();
        for &src in srcs {
            self.dist[src as usize] = 0.0;
            self.parent[src as usize] = src;
            self.stamp[src as usize] = g;
            self.heap.push(src as u64);
        }
        while let Some(k) = self.heap.pop() {
            let t = (k & 0xffff_ffff) as u32;
            let tu = t as usize;
            if self.done[tu] == g {
                continue;
            }
            self.done[tu] = g;
            if !on_settle(t) {
                return;
            }
            let d = self.dist[tu];
            let c = m.tri_c[tu];
            let wt = w(t).min(1e6);
            for &n in &m.tri_nbr[tu] {
                if n == NONE {
                    continue;
                }
                let nu = n as usize;
                if self.done[nu] == g {
                    continue;
                }
                let wn = w(n);
                if !wn.is_finite() {
                    continue;
                }
                let nd = d + (dist(c, m.tri_c[nu]) as f32) * 0.5 * (wt + wn);
                if self.stamp[nu] != g || nd < self.dist[nu] {
                    self.stamp[nu] = g;
                    self.dist[nu] = nd;
                    self.parent[nu] = t;
                    self.heap.push(((nd.to_bits() as u64) << 32) | n as u64);
                }
            }
        }
    }

    #[inline]
    pub fn reached(&self, t: u32) -> bool {
        self.stamp[t as usize] == self.epoch
    }

    /// Triangles from the root to `t`, appended to `out`.
    pub fn chain(&self, t: u32, out: &mut Vec<u32>) -> bool {
        if !self.reached(t) {
            return false;
        }
        let start = out.len();
        let mut c = t;
        loop {
            out.push(c);
            let p = self.parent[c as usize];
            if p == c {
                break;
            }
            c = p;
        }
        out[start..].reverse();
        true
    }
}

// ---------------------------------------------------------------------------
// Level meshing

#[derive(Clone, Copy)]
struct V {
    p: Point2<f64>,
    z: f64,
}

impl HasPosition for V {
    type Scalar = f64;
    fn position(&self) -> Point2<f64> {
        self.p
    }
}

pub struct SurfIn {
    pub elem: u32,
    pub polys: Vec<Vec<Vec<P3>>>,
    pub open: bool,
    pub space: u32,
}

#[derive(Default)]
pub struct LevelMesh {
    pub verts: Vec<P3>,
    pub tris: Vec<[u32; 3]>,
    /// Per-corner z from the triangle's surface.
    pub tz: Vec<[f64; 3]>,
    pub nbr: Vec<[u32; 3]>,
    /// Surface element.
    pub surf: Vec<u32>,
    pub wall: Vec<bool>,
    pub warnings: Vec<MeshWarn>,
}

/// A meshing problem: surface element (NONE when not tied to one), message, location.
pub struct MeshWarn {
    pub elem: u32,
    pub msg: String,
    pub at: Option<P2>,
}

struct Snapper {
    snap: f64,
    pts: Vec<P3>,
    cells: HashMap<(i64, i64), Vec<u32>>,
}

impl Snapper {
    fn get(&mut self, p: P3) -> u32 {
        let (cx, cy) = (
            (p[0] / self.snap).floor() as i64,
            (p[1] / self.snap).floor() as i64,
        );
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(list) = self.cells.get(&(cx + dx, cy + dy)) {
                    for &i in list {
                        let q = self.pts[i as usize];
                        if (q[0] - p[0]).hypot(q[1] - p[1]) <= self.snap {
                            return i;
                        }
                    }
                }
            }
        }
        let i = self.pts.len() as u32;
        self.pts.push(p);
        self.cells.entry((cx, cy)).or_default().push(i);
        i
    }

    fn ring(&mut self, pts: &[P3], closed: bool) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::with_capacity(pts.len());
        for &p in pts {
            let i = self.get(p);
            if out.last() != Some(&i) {
                out.push(i);
            }
        }
        if closed {
            while out.len() > 1 && out.first() == out.last() {
                out.pop();
            }
        }
        out
    }
}

/// Height of a surface at a point: its least-squares plane when the rings are planar (flat
/// floors, ramps), else interpolation along the nearest ring segment (plane as fallback).
struct SurfZ {
    plane: [f64; 3],
    ring: Option<(Vec<(P3, P3)>, Grid)>,
}

impl SurfZ {
    fn new(polys: &[Vec<Vec<P3>>]) -> SurfZ {
        let pts: Vec<P3> = polys.iter().flatten().flatten().copied().collect();
        let n = pts.len().max(1) as f64;
        let m = pts
            .iter()
            .fold([0.0; 3], |a, p| [a[0] + p[0], a[1] + p[1], a[2] + p[2]]);
        let m = [m[0] / n, m[1] / n, m[2] / n];
        if pts.iter().all(|p| (p[2] - m[2]).abs() < 1e-9) {
            return SurfZ {
                plane: [0.0, 0.0, m[2]],
                ring: None,
            };
        }
        // Normal equations of z - mz = a (x - mx) + b (y - my), centred for stability.
        let (mut sxx, mut sxy, mut syy, mut sxz, mut syz) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for p in &pts {
            let (x, y, z) = (p[0] - m[0], p[1] - m[1], p[2] - m[2]);
            sxx += x * x;
            sxy += x * y;
            syy += y * y;
            sxz += x * z;
            syz += y * z;
        }
        let det = sxx * syy - sxy * sxy;
        let (a, b) = if det.abs() > 1e-12 * (sxx * syy).max(1e-30) {
            ((sxz * syy - syz * sxy) / det, (syz * sxx - sxz * sxy) / det)
        } else {
            (0.0, 0.0)
        };
        let plane = [a, b, m[2] - a * m[0] - b * m[1]];
        let off = |p: &P3| (p[2] - (plane[0] * p[0] + plane[1] * p[1] + plane[2])).abs();
        if pts.iter().all(|p| off(p) < 0.01) {
            return SurfZ { plane, ring: None };
        }
        let segs: Vec<(P3, P3)> = polys
            .iter()
            .flatten()
            .flat_map(|r| (0..r.len()).map(move |k| (r[k], r[(k + 1) % r.len()])))
            .collect();
        let grid = Grid::build(
            &segs
                .iter()
                .map(|(a, b)| bbox([xy(*a), xy(*b)]))
                .collect::<Vec<_>>(),
        );
        SurfZ {
            plane,
            ring: Some((segs, grid)),
        }
    }

    fn at(&self, p: P2, tol: f64) -> f64 {
        if let Some((segs, grid)) = &self.ring {
            let mut best: Option<(f64, f64)> = None;
            grid.visit([p[0] - tol, p[1] - tol, p[0] + tol, p[1] + tol], |k| {
                let (a, b) = segs[k as usize];
                let (_, t, d2) = closest_on_seg(p, xy(a), xy(b));
                if d2 <= tol * tol && best.is_none_or(|bb| d2 < bb.0) {
                    best = Some((d2, a[2] + (b[2] - a[2]) * t));
                }
            });
            if let Some((_, z)) = best {
                return z;
            }
        }
        self.plane[0] * p[0] + self.plane[1] * p[1] + self.plane[2]
    }
}

/// Triangulate one level: surface rings and barriers become constraints, triangles are
/// classified by the smallest containing surface (centroid test), passability per edge.
pub fn build_level(surfs: &[SurfIn], barriers: &[Vec<P3>], snap: f64, level_z: f64) -> LevelMesh {
    let mut out = LevelMesh::default();
    let mut sn = Snapper {
        snap: snap.max(1e-9),
        pts: vec![],
        cells: HashMap::new(),
    };
    // (surface local index, rings (snapped 2D), |area|)
    let mut polys: Vec<(u32, Vec<Vec<P2>>, f64)> = vec![];
    let mut rings: Vec<Vec<u32>> = vec![];
    for (si, s) in surfs.iter().enumerate() {
        for poly in &s.polys {
            let mut rr: Vec<Vec<P2>> = vec![];
            let mut area = 0.0;
            for (k, ring) in poly.iter().enumerate() {
                let r = sn.ring(ring, true);
                if r.len() < 3 {
                    if k == 0 {
                        out.warnings.push(MeshWarn {
                            elem: s.elem,
                            msg: "degenerate ring skipped".into(),
                            at: ring.first().map(|v| xy(*v)),
                        });
                    }
                    continue;
                }
                let r2: Vec<P2> = r.iter().map(|&i| xy(sn.pts[i as usize])).collect();
                let a = ring_area(&r2).abs();
                area += if k == 0 { a } else { -a };
                if k > 0 && rr.is_empty() {
                    break;
                }
                rr.push(r2);
                rings.push(r);
            }
            if !rr.is_empty() {
                polys.push((si as u32, rr, area));
            }
        }
    }
    let bars: Vec<Vec<u32>> = barriers
        .iter()
        .map(|b| sn.ring(b, false))
        .filter(|r| r.len() >= 2)
        .collect();
    let mut segs: Vec<([u32; 2], bool)> = vec![];
    for r in &rings {
        for k in 0..r.len() {
            segs.push(([r[k], r[(k + 1) % r.len()]], false));
        }
    }
    for b in &bars {
        for w in b.windows(2) {
            segs.push(([w[0], w[1]], true));
        }
    }
    let segs = node_segments(&mut sn, segs);
    let bar_pairs: std::collections::HashSet<(u32, u32)> = segs
        .iter()
        .filter(|s| s.1)
        .map(|s| (s.0[0].min(s.0[1]), s.0[0].max(s.0[1])))
        .collect();

    let mut cdt: ConstrainedDelaunayTriangulation<V> = ConstrainedDelaunayTriangulation::new();
    let handles: Vec<_> = sn
        .pts
        .iter()
        .map(|p| {
            cdt.insert(V {
                p: Point2::new(p[0], p[1]),
                z: p[2],
            })
            .ok()
        })
        .collect();
    let mut idx_of = vec![NONE; sn.pts.len() + 1];
    let mut skipped = 0;
    let mut skipped_at: Option<P2> = None;
    for (k, h) in handles.iter().enumerate() {
        if let Some(h) = h {
            if h.index() >= idx_of.len() {
                idx_of.resize(h.index() + 1, NONE);
            }
            idx_of[h.index()] = k as u32;
        }
    }
    for (s, _) in &segs {
        if let (Some(ha), Some(hb)) = (handles[s[0] as usize], handles[s[1] as usize])
            && ha != hb
            && !cdt.exists_constraint(ha, hb)
            && cdt.try_add_constraint(ha, hb).is_empty()
        {
            skipped += 1;
            skipped_at.get_or_insert_with(|| {
                lerp(xy(sn.pts[s[0] as usize]), xy(sn.pts[s[1] as usize]), 0.5)
            });
        }
    }
    if skipped > 0 {
        out.warnings.push(MeshWarn {
            elem: NONE,
            msg: format!("{skipped} boundary segments conflicted after noding and were skipped"),
            at: skipped_at,
        });
    }
    let _ = &idx_of;
    // Vertices (fill z of split points from their neighbours).
    let nv = cdt.num_vertices();
    out.verts = vec![[0.0; 3]; nv];
    for v in cdt.vertices() {
        let p = v.position();
        let mut z = v.data().z;
        if z.is_nan() {
            let (mut s, mut n) = (0.0, 0.0);
            for e in v.out_edges() {
                let zz = e.to().data().z;
                if !zz.is_nan() {
                    s += zz;
                    n += 1.0;
                }
            }
            z = if n > 0.0 { s / n } else { level_z };
        }
        out.verts[v.fix().index()] = [p.x, p.y, z];
    }

    // Classification grid over polygons.
    let pgrid = Grid::build(
        &polys
            .iter()
            .map(|(_, rr, _)| bbox(rr[0].iter().copied()))
            .collect::<Vec<_>>(),
    );
    let bar_segs: Vec<(P2, P2)> = segs
        .iter()
        .filter(|s| s.1)
        .map(|s| (xy(sn.pts[s.0[0] as usize]), xy(sn.pts[s.0[1] as usize])))
        .collect();
    let bgrid = Grid::build(
        &bar_segs
            .iter()
            .map(|(a, b)| bbox([*a, *b]))
            .collect::<Vec<_>>(),
    );
    let on_barrier = |p: P2| -> bool {
        let mut hit = false;
        bgrid.visit([p[0] - 1e-6, p[1] - 1e-6, p[0] + 1e-6, p[1] + 1e-6], |k| {
            let (a, b) = bar_segs[k as usize];
            if closest_on_seg(p, a, b).2 < 1e-12 {
                hit = true;
            }
        });
        hit
    };

    let mut surf_z: Vec<Option<SurfZ>> = (0..surfs.len()).map(|_| None).collect();
    let mut fmap = vec![NONE; cdt.num_all_faces()];
    let mut faces = vec![];
    for f in cdt.inner_faces() {
        let vs = f.vertices();
        let id = [
            vs[0].fix().index() as u32,
            vs[1].fix().index() as u32,
            vs[2].fix().index() as u32,
        ];
        let p: [P2; 3] = id.map(|i| xy(out.verts[i as usize]));
        let c = [
            (p[0][0] + p[1][0] + p[2][0]) / 3.0,
            (p[0][1] + p[1][1] + p[2][1]) / 3.0,
        ];
        let mut best = (NONE, f64::INFINITY);
        for &k in pgrid.at(c) {
            let (si, rr, area) = &polys[k as usize];
            if *area < best.1 && in_rings(c, rr) {
                best = (*si, *area);
            }
        }
        if best.0 == NONE {
            continue;
        }
        let tri = if cross(p[0], p[1], p[2]) >= 0.0 {
            id
        } else {
            [id[0], id[2], id[1]]
        };
        fmap[f.fix().index()] = out.tris.len() as u32;
        let sz = surf_z[best.0 as usize]
            .get_or_insert_with(|| SurfZ::new(&surfs[best.0 as usize].polys));
        out.tz
            .push(tri.map(|v| sz.at(xy(out.verts[v as usize]), 2.0 * snap + 1e-9)));
        out.tris.push(tri);
        out.surf.push(best.0);
        faces.push(f.fix());
    }

    out.nbr = vec![[NONE; 3]; out.tris.len()];
    out.wall = vec![false; nv];
    for (t, fh) in faces.iter().enumerate() {
        let f = cdt.face(*fh);
        let tri = out.tris[t];
        for e in f.adjacent_edges() {
            let (a, b) = (e.from().fix().index() as u32, e.to().fix().index() as u32);
            let i = (0..3).find(|&i| {
                let (u, w) = (tri[i], tri[(i + 1) % 3]);
                (u == a && w == b) || (u == b && w == a)
            });
            let Some(i) = i else { continue };
            let n = e
                .rev()
                .face()
                .as_inner()
                .map(|x| fmap[x.fix().index()])
                .unwrap_or(NONE);
            let mut pass = n != NONE;
            if pass && !bar_segs.is_empty() && cdt.is_constraint_edge(e.fix().as_undirected()) {
                let (pa, pb) = (xy(out.verts[a as usize]), xy(out.verts[b as usize]));
                if bar_pairs.contains(&(a.min(b), a.max(b)))
                    || (on_barrier(lerp(pa, pb, 0.5)) && on_barrier(pa) && on_barrier(pb))
                {
                    pass = false;
                }
            }
            if pass {
                let (s1, s2) = (out.surf[t], out.surf[n as usize]);
                let (a1, a2) = (&surfs[s1 as usize], &surfs[s2 as usize]);
                pass =
                    s1 == s2 || (a1.open && a2.open) || (a1.space != NONE && a1.space == a2.space);
            }
            if pass {
                out.nbr[t][i] = n;
            } else {
                out.wall[tri[i] as usize] = true;
                out.wall[tri[(i + 1) % 3] as usize] = true;
            }
        }
    }
    for s in out.surf.iter_mut() {
        *s = surfs[*s as usize].elem;
    }
    out
}

/// Split segments at mutual crossings and at vertices within `snap` of their interior, so that
/// the CDT never sees crossing or near-overlapping constraints. New points go through the
/// snapper (merging with anything within `snap`). Two passes catch crossings created by snapping.
fn node_segments(sn: &mut Snapper, mut segs: Vec<([u32; 2], bool)>) -> Vec<([u32; 2], bool)> {
    let tol = sn.snap;
    for _ in 0..2 {
        let boxes: Vec<[f64; 4]> = segs
            .iter()
            .map(|s| {
                let b = bbox([xy(sn.pts[s.0[0] as usize]), xy(sn.pts[s.0[1] as usize])]);
                [b[0] - tol, b[1] - tol, b[2] + tol, b[3] + tol]
            })
            .collect();
        let grid = Grid::build(&boxes);
        let mut splits: Vec<Vec<(f64, u32)>> = vec![vec![]; segs.len()];
        let mut cand: Vec<u32> = vec![];
        let mut any = false;
        for i in 0..segs.len() {
            let [a, b] = segs[i].0;
            if a == b {
                continue;
            }
            cand.clear();
            grid.visit(boxes[i], |j| cand.push(j));
            cand.sort_unstable();
            cand.dedup();
            for &j in &cand {
                let j = j as usize;
                if j <= i {
                    continue;
                }
                let [c, d] = segs[j].0;
                if c == d {
                    continue;
                }
                let (pa, pb, pc, pd) = (
                    xy(sn.pts[a as usize]),
                    xy(sn.pts[b as usize]),
                    xy(sn.pts[c as usize]),
                    xy(sn.pts[d as usize]),
                );
                // Vertices of one segment lying on the other.
                for (v, pv, k, p0, p1, va, vb) in [
                    (c, pc, i, pa, pb, a, b),
                    (d, pd, i, pa, pb, a, b),
                    (a, pa, j, pc, pd, c, d),
                    (b, pb, j, pc, pd, c, d),
                ] {
                    if v == va || v == vb {
                        continue;
                    }
                    let (_, t, d2) = closest_on_seg(pv, p0, p1);
                    if d2 <= tol * tol && t > 0.0 && t < 1.0 {
                        splits[k].push((t, v));
                        any = true;
                    }
                }
                // Proper crossing.
                if a == c || a == d || b == c || b == d {
                    continue;
                }
                let (o1, o2) = (cross(pa, pb, pc), cross(pa, pb, pd));
                let (o3, o4) = (cross(pc, pd, pa), cross(pc, pd, pb));
                if ((o1 > 0.0 && o2 < 0.0) || (o1 < 0.0 && o2 > 0.0))
                    && ((o3 > 0.0 && o4 < 0.0) || (o3 < 0.0 && o4 > 0.0))
                {
                    let t = o3 / (o3 - o4);
                    let s2 = o1 / (o1 - o2);
                    let x = lerp(pa, pb, t);
                    let z =
                        sn.pts[a as usize][2] + (sn.pts[b as usize][2] - sn.pts[a as usize][2]) * t;
                    let v = sn.get([x[0], x[1], z]);
                    if v != a && v != b {
                        splits[i].push((t, v));
                        any = true;
                    }
                    if v != c && v != d {
                        splits[j].push((s2, v));
                        any = true;
                    }
                }
            }
        }
        if !any {
            break;
        }
        let mut out = Vec::with_capacity(segs.len() * 2);
        for (i, (s, bar)) in segs.iter().enumerate() {
            let sp = &mut splits[i];
            sp.sort_by(|x, y| x.0.total_cmp(&y.0));
            let mut prev = s[0];
            for &(_, v) in sp.iter().chain(std::iter::once(&(1.0, s[1]))) {
                if v != prev {
                    out.push(([prev, v], *bar));
                    prev = v;
                }
            }
        }
        segs = out;
    }
    // Deduplicate (barrier flag wins).
    let mut seen: HashMap<(u32, u32), usize> = HashMap::new();
    let mut out: Vec<([u32; 2], bool)> = vec![];
    for (s, bar) in segs {
        if s[0] == s[1] {
            continue;
        }
        let key = (s[0].min(s[1]), s[0].max(s[1]));
        match seen.get(&key) {
            Some(&k) => out[k].1 |= bar,
            None => {
                seen.insert(key, out.len());
                out.push((s, bar));
            }
        }
    }
    out
}
