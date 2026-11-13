//! Simple stupid funnel over a triangle channel, optionally with clearance-shrunk portals,
//! and exact per-triangle measurement of the resulting walk (length, climb, surfaces).

use crate::geom::*;
use crate::graph::Piece;
use crate::mesh::Mesh;

/// A portal of the channel as seen when walking forward: `l` on the left, `r` on the right.
#[derive(Clone, Copy, Debug)]
pub struct Gate {
    pub l: P2,
    pub r: P2,
    pub lv: u32,
    pub rv: u32,
}

/// Channel portals from `start` to `end` (first/last gates are the degenerate end points).
/// Portal ends at wall vertices are pulled in by `clearance` (at most 45% of the portal).
pub fn gates(m: &Mesh, chain: &[u32], start: P2, end: P2, clearance: f64, out: &mut Vec<Gate>) {
    out.clear();
    out.push(Gate {
        l: start,
        r: start,
        lv: NONE,
        rv: NONE,
    });
    for w in chain.windows(2) {
        let i = m.edge_to(w[0], w[1]);
        let v = m.tri_v[w[0] as usize];
        // Leaving a CCW triangle across p -> q: q is on the left, p on the right.
        let (rv, lv) = (v[i], v[(i + 1) % 3]);
        let (mut l, mut r) = (m.p(lv), m.p(rv));
        if clearance > 0.0 {
            let wdt = dist(l, r);
            if wdt > 1e-9 {
                let s = (clearance / wdt).min(0.45);
                let (l0, r0) = (l, r);
                if m.wall[lv as usize] {
                    l = lerp(l0, r0, s);
                }
                if m.wall[rv as usize] {
                    r = lerp(r0, l0, s);
                }
            }
        }
        out.push(Gate { l, r, lv, rv });
    }
    out.push(Gate {
        l: end,
        r: end,
        lv: NONE,
        rv: NONE,
    });
}

#[inline]
fn same(a: P2, b: P2) -> bool {
    (a[0] - b[0]).abs() < 1e-12 && (a[1] - b[1]).abs() < 1e-12
}

/// String pulling. Outputs points, the gate index of each point and the mesh vertex each
/// interior turn wraps around (`NONE` for the end points).
pub fn funnel(gates: &[Gate], pts: &mut Vec<P2>, gi: &mut Vec<u32>, corner: &mut Vec<u32>) {
    pts.clear();
    gi.clear();
    corner.clear();
    let n = gates.len();
    let mut apex = gates[0].l;
    let (mut left, mut right) = (gates[0].l, gates[0].r);
    let (mut li, mut ri) = (0usize, 0usize);
    pts.push(apex);
    gi.push(0);
    corner.push(NONE);
    let mut i = 1;
    while i < n {
        let (l, r) = (gates[i].l, gates[i].r);
        if cross(apex, right, r) >= 0.0 {
            if same(apex, right) || cross(apex, left, r) < 0.0 {
                right = r;
                ri = i;
            } else {
                apex = left;
                if !same(apex, *pts.last().unwrap()) {
                    pts.push(apex);
                    gi.push(li as u32);
                    corner.push(gates[li].lv);
                }
                let ai = li;
                left = apex;
                right = apex;
                ri = ai;
                i = ai + 1;
                continue;
            }
        }
        if cross(apex, left, l) <= 0.0 {
            if same(apex, left) || cross(apex, right, l) > 0.0 {
                left = l;
                li = i;
            } else {
                apex = right;
                if !same(apex, *pts.last().unwrap()) {
                    pts.push(apex);
                    gi.push(ri as u32);
                    corner.push(gates[ri].rv);
                }
                let ai = ri;
                left = apex;
                right = apex;
                li = ai;
                i = ai + 1;
                continue;
            }
        }
        i += 1;
    }
    let end = gates[n - 1].l;
    pts.push(end);
    gi.push((n - 1) as u32);
    corner.push(NONE);
}

#[derive(Default)]
pub struct WalkBuf {
    pub gates: Vec<Gate>,
    pub pts: Vec<P2>,
    pub gi: Vec<u32>,
    pub corner: Vec<u32>,
    pub pieces: Vec<Piece>,
    pub geom: Vec<P3>,
    /// Query-time wall clearance per mesh vertex (lazily filled; see `smooth::vinfo`).
    pub vinfo: Vec<VInfo>,
    /// Identity of the mesh `vinfo` belongs to (address, vertex count).
    pub vmesh: (usize, usize),
    /// `vinfo[v]` belongs to this query when `vstamp[v] == vgen` (`new_query`).
    pub vstamp: Vec<u32>,
    pub vgen: u32,
    /// Scratch of the vertex clearance search.
    pub vstack: Vec<u32>,
    pub vseen: Vec<u32>,
}

impl WalkBuf {
    /// Forget the clearances of earlier queries: a vertex's clearance depends on the triangle
    /// its search starts from, so a cached one would make a route depend on the queries before
    /// it.
    pub fn new_query(&mut self) {
        self.vgen = self.vgen.wrapping_add(1);
        if self.vgen == 0 {
            self.vstamp.fill(0);
            self.vgen = 1;
        }
    }
}

/// Free space around a wall vertex: distance to the nearest wall not touching it (capped)
/// and the directions of up to two walls that do touch it. `free` is NaN until computed.
#[derive(Clone, Copy, Debug)]
pub struct VInfo {
    pub free: f32,
    pub dirs: [[f32; 2]; 2],
    pub ndirs: u8,
}

impl Default for VInfo {
    fn default() -> Self {
        VInfo {
            free: f32::NAN,
            dirs: [[0.0; 2]; 2],
            ndirs: 0,
        }
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct WalkStats {
    pub len: f64,
    pub max_up: f64,
    pub max_down: f64,
}

/// Geodesic walk a -> b through `chain`: apex polyline (with z) into `buf.geom` and per-surface
/// pieces into `buf.pieces`. Segments are split where they cross channel portals, so each
/// sub-segment lies in one triangle and climb/descent/grade are exact on sloped meshes.
pub fn measure(m: &Mesh, chain: &[u32], a: P3, b: P3, buf: &mut WalkBuf) -> WalkStats {
    gates(m, chain, xy(a), xy(b), 0.0, &mut buf.gates);
    funnel(&buf.gates, &mut buf.pts, &mut buf.gi, &mut buf.corner);
    buf.pieces.clear();
    buf.geom.clear();
    let mut st = WalkStats::default();
    let n = buf.pts.len();
    let zof = |k: usize| -> f64 {
        if k == 0 {
            a[2]
        } else if k == n - 1 {
            b[2]
        } else if buf.corner[k] != NONE {
            // Corner on the portal between chain[gi - 1] and chain[gi]: z of the incoming side.
            let gk = (buf.gi[k] as usize).clamp(1, chain.len());
            m.z_at(chain[gk - 1], buf.pts[k])
        } else {
            a[2]
        }
    };
    let emit = |p0: P3, p1: P3, tri: u32, pieces: &mut Vec<Piece>, st: &mut WalkStats| {
        let l2 = dist(xy(p0), xy(p1));
        let dz = p1[2] - p0[2];
        st.len += l2;
        if l2 > 0.05 {
            let g = dz / l2;
            st.max_up = st.max_up.max(g);
            st.max_down = st.max_down.max(-g);
        }
        let elem = m.tri_surf[tri as usize];
        let (cl, de) = (dz.max(0.0) as f32, (-dz).max(0.0) as f32);
        match pieces.last_mut() {
            Some(p) if p.elem == elem => {
                p.len += l2 as f32;
                p.climb += cl;
                p.descent += de;
            }
            _ => pieces.push(Piece {
                elem,
                len: l2 as f32,
                climb: cl,
                descent: de,
            }),
        }
    };
    for k in 0..n {
        let p = buf.pts[k];
        buf.geom.push([p[0], p[1], zof(k)]);
    }
    for k in 0..n - 1 {
        let (g0, g1) = (buf.gi[k] as usize, buf.gi[k + 1] as usize);
        let (p0, p1) = (buf.pts[k], buf.pts[k + 1]);
        let mut cur = buf.geom[k];
        for j in g0 + 1..g1 {
            let gt = buf.gates[j];
            let t = line_cross_t(p0, p1, gt.l, gt.r)
                .unwrap_or(0.5)
                .clamp(0.0, 1.0);
            let q = lerp(gt.l, gt.r, t);
            let next = [q[0], q[1], m.z_at(chain[j - 1], q)];
            emit(cur, next, chain[j - 1], &mut buf.pieces, &mut st);
            cur = next;
            // Surfaces of different heights meeting at an open boundary: a step.
            let (t0, t1) = (chain[j - 1] as usize, chain[j] as usize);
            let z2 = if m.tri_surf[t0] == m.tri_surf[t1] {
                cur[2]
            } else {
                m.z_at(chain[j], q)
            };
            if (z2 - cur[2]).abs() > 1e-9 {
                let next = [q[0], q[1], z2];
                emit(cur, next, chain[j], &mut buf.pieces, &mut st);
                cur = next;
            }
        }
        emit(
            cur,
            buf.geom[k + 1],
            chain[g1.max(1) - 1],
            &mut buf.pieces,
            &mut st,
        );
    }
    st
}
