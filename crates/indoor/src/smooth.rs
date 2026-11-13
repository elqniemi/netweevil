//! Realistic walk geometry inside a zone.
//!
//! People keep away from walls in proportion to the room they have: in a corridor they walk
//! near the middle, in a hall they pass corners at a comfortable distance, and they leave and
//! enter doors roughly straight. The drawn walk is built in four steps:
//!
//! 1. door leads: from a door the walk first goes along the door's normal, to half the free
//!    depth in front of it (a corridor's centreline), at most `max(LEAD, clearance_max)`;
//! 2. a funnel whose portal ends at wall vertices are pulled in so that the path keeps the
//!    vertex's *adaptive clearance* `clamp(SHARE * free, clearance, clearance_max)` from the
//!    walls at that vertex (`free`: distance from the vertex to the nearest other wall);
//! 3. corners: `round` = an arc around each turn's wall vertex at its adaptive clearance (at
//!    least `corner_radius`: a bigger circle sits behind the vertex), joined by common
//!    tangents, plus a fillet where a door lead turns; `square` = the same tangent legs meeting
//!    in mitred corners (two corners for turns over 95 degrees); `sharp` = the funnel points;
//! 4. near-collinear points and micro-segments are dropped.
//!
//! Every step is validated against the zone's passable triangles. A failing corner is
//! downgraded (offset arc -> arc at the adaptive clearance -> arc at `clearance` -> funnel
//! point), so output never leaves walkable space.

use crate::funnel::{VInfo, WalkBuf, funnel, gates};
use crate::geom::*;
use crate::mesh::Mesh;
use crate::profile::{Corner, Profile};

/// Share of the local free width kept between a walk and a wall (0.45: corridors are walked in
/// their middle tenth).
pub const SHARE: f64 = 0.45;
/// Free width is measured up to this distance from a vertex (m).
pub const FREE_CAP: f64 = 6.0;
/// Door leads are at most `max(LEAD, clearance_max)` long (m).
pub const LEAD: f64 = 0.5;
/// Round turns get a radius of `TURN` times the free width around their vertex, within
/// [`corner_radius`, max(`corner_radius`, `TURN_MAX * clearance_max`)].
const TURN: f64 = 0.5;
const TURN_MAX: f64 = 3.0;
/// Turns flatter than this (radians) stay plain points.
const FLAT: f64 = 0.05;
/// Square corners turning more than this (radians, 95 degrees) become two corners.
const CHAMFER: f64 = 1.66;

fn locate_in(m: &Mesh, chain: &[u32], p: P2) -> Option<usize> {
    chain.iter().position(|&t| m.contains(t, p, 1e-7))
}

#[inline]
fn seg_dist(p: P2, a: P2, b: P2) -> f64 {
    closest_on_seg(p, a, b).2.sqrt()
}

/// Free space around wall vertex `v` (a corner of triangle `t`), cached in `wb.vinfo` for the
/// query (`WalkBuf::new_query`): the distance to the nearest wall edge of the zone not touching
/// `v` (at most `FREE_CAP`) and the directions of the wall edges that do.
pub fn vinfo(m: &Mesh, wb: &mut WalkBuf, t: u32, v: u32) -> VInfo {
    let key = (m as *const Mesh as usize, m.verts.len());
    if wb.vmesh != key || wb.vinfo.len() != m.verts.len() {
        wb.vinfo.clear();
        wb.vinfo.resize(m.verts.len(), VInfo::default());
        wb.vstamp.clear();
        wb.vstamp.resize(m.verts.len(), 0);
        wb.vmesh = key;
    }
    let cached = wb.vinfo[v as usize];
    if wb.vstamp[v as usize] == wb.vgen && !cached.free.is_nan() {
        return cached;
    }
    let pv = m.p(v);
    let mut info = VInfo::default();
    let mut best = FREE_CAP;
    wb.vstack.clear();
    wb.vseen.clear();
    wb.vstack.push(t);
    wb.vseen.push(t);
    while let Some(t) = wb.vstack.pop() {
        let tv = m.tri_v[t as usize];
        for i in 0..3 {
            let (u, w) = (tv[i], tv[(i + 1) % 3]);
            let n = m.tri_nbr[t as usize][i];
            let touches = u == v || w == v;
            if n == NONE {
                if touches {
                    let d = norm(sub(m.p(if u == v { w } else { u }), pv));
                    let k = info.ndirs as usize;
                    let seen = info.dirs[..k]
                        .iter()
                        .any(|e| dot([e[0] as f64, e[1] as f64], d) > 0.9999);
                    if k < 2 && !seen {
                        info.dirs[k] = [d[0] as f32, d[1] as f32];
                        info.ndirs += 1;
                    }
                } else if cross(m.p(u), m.p(w), pv) > 0.0 {
                    // Only walls facing v (v on their walkable side) bound its free space: the
                    // far side of a pillar does not.
                    best = best.min(seg_dist(pv, m.p(u), m.p(w)));
                }
            } else if wb.vseen.len() < 512 && !wb.vseen.contains(&n) {
                let d = if touches {
                    0.0
                } else {
                    seg_dist(pv, m.p(u), m.p(w))
                };
                if d < best {
                    wb.vseen.push(n);
                    wb.vstack.push(n);
                }
            }
        }
    }
    info.free = best as f32;
    wb.vinfo[v as usize] = info;
    wb.vstamp[v as usize] = wb.vgen;
    info
}

/// Clearance wanted at a vertex: `SHARE` of its free width, within [c, cmax].
#[inline]
fn want(info: &VInfo, c: f64, cmax: f64) -> f64 {
    (SHARE * info.free as f64).min(cmax).max(c)
}

/// How far to move along a portal (unit direction `pd` from the vertex) so that the point
/// keeps `d` from the walls at the vertex.
fn pull(info: &VInfo, d: f64, pd: P2) -> f64 {
    let mut t = d;
    for e in &info.dirs[..info.ndirs as usize] {
        let e = [e[0] as f64, e[1] as f64];
        if dot(pd, e) > 0.0 {
            let s = (pd[0] * e[1] - pd[1] * e[0]).abs();
            t = t.max(d / s.max(0.05));
        }
    }
    t
}

/// Free length (at most `max`) of the ray from `p` (in triangle `t`) along unit `dir` through
/// passable triangles.
fn ray(m: &Mesh, mut t: u32, p: P2, dir: P2, max: f64) -> f64 {
    let q = add(p, scale(dir, max));
    for _ in 0..10_000 {
        let pts = m.pts(t);
        if in_tri(q, pts[0], pts[1], pts[2], 1e-9) {
            return max;
        }
        // The ray leaves through the first edge whose outer side holds q.
        let mut exit: Option<(f64, usize)> = None;
        for i in 0..3 {
            let (u, w) = (pts[i], pts[(i + 1) % 3]);
            if cross(u, w, q) >= 0.0 {
                continue;
            }
            if let Some(s) = line_cross_t(u, w, p, q)
                && exit.is_none_or(|e| s < e.0)
            {
                exit = Some((s, i));
            }
        }
        let Some((s, i)) = exit else {
            return 0.0;
        };
        let n = m.tri_nbr[t as usize][i];
        if n == NONE {
            return (s * max).clamp(0.0, max);
        }
        t = n;
    }
    0.0
}

/// Lead point along `dir` from door point `p`: half the free depth in front of the door, at
/// most `cap`, shortened until the lead is walkable. Returns its index in the channel
/// (`from_end`: searched from the channel's end, index >= `min_k`; None: outside the channel,
/// which happens when both doors open from one long triangle against their wall), the point
/// and its triangle.
fn lead(
    m: &Mesh,
    chain: &[u32],
    p: P2,
    dir: P2,
    cap: f64,
    from_end: bool,
    min_k: usize,
) -> Option<(Option<usize>, P2, u32)> {
    if len(dir) < 0.5 || cap < 0.1 {
        return None;
    }
    let t0 = if from_end {
        chain[chain.len() - 1]
    } else {
        chain[0]
    };
    let mut l = (0.5 * ray(m, t0, p, dir, FREE_CAP)).min(cap);
    while l >= 0.1 {
        let q = add(p, scale(dir, l));
        if let Some(tq) = m.seg_inside(t0, p, q) {
            let k = if from_end {
                chain.iter().rposition(|&t| m.contains(t, q, 1e-7))
            } else {
                locate_in(m, chain, q)
            };
            match k {
                Some(k) if k >= min_k => return Some((Some(k), q, tq)),
                Some(_) => {}
                None => return Some((None, q, tq)),
            }
        }
        l *= 0.6;
    }
    None
}

/// Append `c` to `chain` (sharing the junction triangle) and cut any loop it creates.
pub(crate) fn join_chain(chain: &mut Vec<u32>, c: &[u32]) {
    for &t in c {
        if let Some(k) = chain.iter().rposition(|&x| x == t) {
            chain.truncate(k + 1);
        } else {
            chain.push(t);
        }
    }
}

/// Triangles the segment p -> q crosses, from `t` (holding p) on.
fn crossed(m: &Mesh, t: u32, p: P2, q: P2) -> Vec<u32> {
    let mut out = vec![t];
    m.trace(t, p, q, |_, _, to| out.push(to));
    out
}

/// Directed common tangent from circle (`o1`, `r1`) to circle (`o2`, `r2`). Radii are signed:
/// positive = the path goes around the centre counter-clockwise, negative = clockwise, zero =
/// a point. Returns the tangent points (leave the first, reach the second).
fn tangent(o1: P2, r1: f64, o2: P2, r2: f64) -> Option<(P2, P2)> {
    if r1 == 0.0 && r2 == 0.0 {
        return Some((o1, o2));
    }
    let d = sub(o2, o1);
    let l = len(d);
    let k = r2 - r1;
    if l < 1e-9 || k.abs() >= l * 0.999 {
        return None;
    }
    // Unit normal n (right of the travel direction) with d . n = -k.
    let (phi, beta) = (d[1].atan2(d[0]), (-k / l).acos());
    let (sn, cs) = (phi - beta).sin_cos();
    let n = [cs, sn];
    Some((add(o1, scale(n, r1)), add(o2, scale(n, r2))))
}

/// Arc on circle (`o`, signed `r`) from `a` to `b`, appended to `out` (15 degree steps).
fn arc(o: P2, r: f64, a: P2, b: P2, out: &mut Vec<P2>) -> bool {
    let s = r.signum();
    let (a1, a2) = (
        (a[1] - o[1]).atan2(a[0] - o[0]),
        (b[1] - o[1]).atan2(b[0] - o[0]),
    );
    let sweep = ((a2 - a1) * s).rem_euclid(2.0 * std::f64::consts::PI);
    if sweep > 1.5 * std::f64::consts::PI {
        return false; // the tangents cross: the corner does not fit
    }
    let n = (sweep / (std::f64::consts::PI / 12.0)).ceil().max(1.0) as usize;
    for k in 0..=n {
        let (sn, cs) = (a1 + s * sweep * k as f64 / n as f64).sin_cos();
        out.push(add(o, scale([cs, sn], r.abs())));
    }
    true
}

/// Sweep (radians) of the arc `arc` would draw.
fn arc_sweep(o: P2, r: f64, a: P2, b: P2) -> f64 {
    let (a1, a2) = (
        (a[1] - o[1]).atan2(a[0] - o[0]),
        (b[1] - o[1]).atan2(b[0] - o[0]),
    );
    ((a2 - a1) * r.signum()).rem_euclid(2.0 * std::f64::consts::PI)
}

/// Fillet of corner `p` between `prev` and `next` with radius `r` (capped by half the legs).
pub(crate) fn fillet(prev: P2, p: P2, next: P2, r: f64, out: &mut Vec<P2>) {
    let (tin, tout) = (0.5 * dist(p, prev), 0.5 * dist(next, p));
    fillet_cap(prev, p, next, r, tin, tout, out);
}

/// Fillet of corner `p` with radius `r`, its tangent points at most `tin` before and `tout`
/// after `p`. Pushes `p` alone when there is no turn.
fn fillet_cap(prev: P2, p: P2, next: P2, r: f64, tin: f64, tout: f64, out: &mut Vec<P2>) {
    let (din, dout) = (norm(sub(p, prev)), norm(sub(next, p)));
    let th = dot(din, dout).clamp(-1.0, 1.0).acos();
    if th < 0.02 || r <= 0.0 {
        out.push(p);
        return;
    }
    let tan = (th * 0.5).tan();
    let t = (r * tan).min(tin).min(tout);
    let re = t / tan;
    let s = if cross([0.0, 0.0], din, dout) >= 0.0 {
        1.0
    } else {
        -1.0
    };
    let t1 = sub(p, scale(din, t));
    let c = add(t1, scale([-din[1], din[0]], re * s));
    let v = sub(t1, c);
    let n = (th / (std::f64::consts::PI / 12.0)).ceil().max(1.0) as usize;
    for k in 0..=n {
        let a = s * th * k as f64 / n as f64;
        let (sn, cs) = a.sin_cos();
        out.push(add(c, [v[0] * cs - v[1] * sn, v[0] * sn + v[1] * cs]));
    }
}

/// Point where line a->b meets line c->d (None when parallel).
#[inline]
fn meet(a: P2, b: P2, c: P2, d: P2) -> Option<P2> {
    line_cross_t(c, d, a, b).map(|s| lerp(a, b, s))
}

/// Deflection (radians) of the path a -> p -> b at p.
#[inline]
fn turn(a: P2, p: P2, b: P2) -> f64 {
    let (u, w) = (norm(sub(p, a)), norm(sub(b, p)));
    if len(u) < 0.5 || len(w) < 0.5 {
        return 0.0;
    }
    dot(u, w).clamp(-1.0, 1.0).acos()
}

/// Whether segment a-b passes closer than `d` to one of `pts`.
fn too_close(a: P2, b: P2, pts: &[P2], d: f64) -> bool {
    let (lo, hi) = (
        [a[0].min(b[0]) - d, a[1].min(b[1]) - d],
        [a[0].max(b[0]) + d, a[1].max(b[1]) + d],
    );
    pts.iter().any(|&q| {
        q[0] > lo[0] && q[0] < hi[0] && q[1] > lo[1] && q[1] < hi[1] && seg_dist(q, a, b) < d
    })
}

/// Index of the first segment of `pts` (walked from triangle `t`) that leaves walkable space.
fn first_bad(m: &Mesh, mut t: u32, pts: &[P2]) -> Option<usize> {
    for k in 0..pts.len().saturating_sub(1) {
        match m.seg_inside(t, pts[k], pts[k + 1]) {
            Some(t2) => t = t2,
            None => return Some(k),
        }
    }
    None
}

/// A turn of the funnel: funnel points `i0..=i1` all bend around wall vertex `v`.
#[derive(Clone, Copy)]
struct Turn {
    i0: usize,
    i1: usize,
    /// Wall vertex position (the point itself for the walk's ends); for turns around several
    /// nearby vertices their centroid.
    pv: P2,
    /// Last wall vertex of the turn.
    pl: P2,
    /// Distance from `pv` to the turn's farthest vertex (0 for one vertex).
    ext: f64,
    /// Adaptive clearance kept from the vertices.
    rho: f64,
    /// Turn radius wanted (>= rho): grows with the free space around the vertex.
    big: f64,
    /// +1: counter-clockwise around `pv`, -1: clockwise.
    s: f64,
    /// Outer bisector (away from `pv`).
    u: P2,
    /// Total deflection (radians).
    th: f64,
}

/// Turns sharper than this (radians) are not given a wider fillet.
const MAX_FILLET_TURN: f64 = 2.6;

/// Radius of a wide turn: the circle tangent to the two legs that pass the turn's vertices at
/// `rho`, as wide as the free space allows (`big`) while its apex keeps at least
/// `max(0.75 rho, c)` from the vertices. Gentle turns get wide arcs, sharp ones tight arcs.
fn fillet_radius(t: &Turn, c: f64) -> f64 {
    let re = t.rho + t.ext;
    let cs = (0.5 * t.th.min(MAX_FILLET_TURN)).cos();
    let amin = (0.75 * t.rho).max(c.min(t.rho)) + t.ext;
    let rmax = if 1.0 - cs < 1e-9 {
        f64::MAX
    } else {
        (re / cs - amin) / (1.0 / cs - 1.0)
    };
    t.big.min(rmax).max(re)
}

/// Walk a -> b through `chain` (triangles from a's to b's). `la`/`lb`: directions into the zone
/// at a / b (door normals; zero = none). Appends the 3D polyline to `out`.
pub fn realize(
    m: &Mesh,
    chain: &[u32],
    a: P3,
    b: P3,
    la: P2,
    lb: P2,
    p: &Profile,
    wb: &mut WalkBuf,
    out: &mut Vec<P3>,
    surf: &mut Vec<u32>,
) {
    let (a2, b2) = (xy(a), xy(b));
    let c = p.clearance.max(0.0);
    let cmax = p.clearance_max.max(c);
    let cap = cmax.max(LEAD).min(0.35 * dist(a2, b2));
    let last = chain.len() - 1;
    let ea = lead(m, chain, a2, la, cap, false, 0);
    let ka = ea.and_then(|e| e.0).unwrap_or(0);
    let eb = lead(m, chain, b2, lb, cap, true, ka);
    let kb = eb.and_then(|e| e.0).unwrap_or(last);
    let (sa, sb) = (ea.map_or(a2, |e| e.1), eb.map_or(b2, |e| e.1));
    // The funnel runs from lead to lead; a lead outside the channel joins it along the lead.
    let (out_a, out_b) = (
        matches!(ea, Some((None, ..))),
        matches!(eb, Some((None, ..))),
    );
    let mut ext: Vec<u32> = vec![];
    let sub_chain: &[u32] = if out_a || out_b {
        if out_a {
            let mut c = crossed(m, chain[0], a2, sa);
            c.reverse();
            join_chain(&mut ext, &c);
        }
        join_chain(&mut ext, &chain[ka..=kb]);
        if out_b {
            join_chain(&mut ext, &crossed(m, chain[last], b2, sb));
        }
        &ext
    } else {
        &chain[ka..=kb]
    };

    // Funnel through portals whose ends at wall vertices are pulled in to the adaptive
    // clearance (a portal narrower than both pulls closes to a point between them).
    gates(m, sub_chain, sa, sb, 0.0, &mut wb.gates);
    let mut gs = std::mem::take(&mut wb.gates);
    let ng = gs.len();
    for (j, g) in gs.iter_mut().enumerate().take(ng - 1).skip(1) {
        let w = dist(g.l, g.r);
        if w < 1e-9 {
            continue;
        }
        let t = sub_chain[j - 1];
        let pd = scale(sub(g.r, g.l), 1.0 / w);
        let mut need = |v: u32, pd: P2| -> f64 {
            if !m.wall[v as usize] {
                return 0.0;
            }
            let info = vinfo(m, wb, t, v);
            pull(&info, want(&info, c, cmax), pd)
        };
        let (tl, tr) = (need(g.lv, pd), need(g.rv, scale(pd, -1.0)));
        let (l0, r0) = (g.l, g.r);
        if tl + tr >= w {
            let q = lerp(l0, r0, tl / (tl + tr));
            (g.l, g.r) = (q, q);
        } else {
            g.l = lerp(l0, r0, tl / w);
            g.r = lerp(r0, l0, tr / w);
        }
    }
    wb.gates = gs;
    funnel(&wb.gates, &mut wb.pts, &mut wb.gi, &mut wb.corner);
    let base = std::mem::take(&mut wb.pts);
    // Wall vertices along the channel: arcs and legs keep `clearance` from them.
    let mut near: Vec<P2> = Vec::with_capacity(2 * wb.gates.len());
    for g in &wb.gates[1..wb.gates.len() - 1] {
        for v in [g.lv, g.rv] {
            if m.wall[v as usize] {
                let q = m.p(v);
                if near.last() != Some(&q) {
                    near.push(q);
                }
            }
        }
    }
    let n = base.len();

    // Turns: runs of funnel points around one wall vertex.
    let mut turns: Vec<Turn> = Vec::with_capacity(n);
    let point = |i: usize| Turn {
        i0: i,
        i1: i,
        pv: base[i],
        pl: base[i],
        ext: 0.0,
        rho: 0.0,
        big: 0.0,
        s: 0.0,
        u: [0.0; 2],
        th: 0.0,
    };
    turns.push(point(0));
    let mut i = 1;
    while i + 1 < n {
        let v = wb.corner[i];
        let mut e = i;
        while v != NONE && e + 2 < n && wb.corner[e + 1] == v {
            e += 1;
        }
        let mut tr = point(i);
        tr.i1 = e;
        if v != NONE && m.wall[v as usize] {
            let pv = m.p(v);
            let gk = (wb.gi[i] as usize).clamp(1, sub_chain.len());
            let info = vinfo(m, wb, sub_chain[gk - 1], v);
            let rho = base[i..=e]
                .iter()
                .fold(want(&info, c, cmax), |r, &q| r.min(dist(pv, q)));
            let big = (TURN * info.free as f64)
                .min(p.corner_radius.max(TURN_MAX * cmax))
                .max(p.corner_radius)
                .max(rho);
            let (din, dout) = (
                norm(sub(base[i], base[i - 1])),
                norm(sub(base[e + 1], base[e])),
            );
            let th: f64 = (i..=e)
                .map(|k| turn(base[k - 1], base[k], base[k + 1]))
                .sum();
            let s = if cross(base[i - 1], base[i], pv) >= 0.0 {
                1.0
            } else {
                -1.0
            };
            let w = sub(din, dout);
            let u = if len(w) > 1e-6 {
                norm(w)
            } else {
                scale([din[1], -din[0]], s)
            };
            tr = Turn {
                pv,
                pl: pv,
                rho,
                big,
                s,
                u,
                th,
                ..tr
            };
        }
        turns.push(tr);
        i = e + 1;
    }
    turns.push(point(n - 1));
    // Same-side turns around nearby vertices (a pillar, the end of a wall) become one turn
    // around their centroid, so their circles do not fight each other.
    let mut k = 1;
    while k + 2 < turns.len() {
        let (a, b) = (turns[k], turns[k + 1]);
        if a.rho > 0.0 && b.rho > 0.0 && a.s == b.s && dist(a.pl, b.pv) <= a.rho.max(b.rho) {
            let (i0, i1) = (a.i0, b.i1);
            let mut sum = [0.0; 2];
            let mut cnt = 0.0;
            let mut last = NONE;
            for &v in &wb.corner[i0..=i1] {
                if v != NONE && v != last {
                    sum = add(sum, m.p(v));
                    cnt += 1.0;
                    last = v;
                }
            }
            let pv = scale(sum, 1.0 / cnt);
            let ext = wb.corner[i0..=i1]
                .iter()
                .filter(|&&v| v != NONE)
                .map(|&v| dist(pv, m.p(v)))
                .fold(0.0, f64::max);
            let (din, dout) = (
                norm(sub(base[i0], base[i0 - 1])),
                norm(sub(base[i1 + 1], base[i1])),
            );
            let w = sub(din, dout);
            let rho = a.rho.min(b.rho);
            turns[k] = Turn {
                i0,
                i1,
                pv,
                pl: b.pl,
                ext,
                rho,
                big: a.big.max(b.big).max(rho + ext),
                s: a.s,
                u: if len(w) > 1e-6 { norm(w) } else { a.u },
                th: (i0..=i1)
                    .map(|i| turn(base[i - 1], base[i], base[i + 1]))
                    .sum(),
            };
            turns.remove(k + 1);
        } else {
            k += 1;
        }
    }
    let nt = turns.len();

    // Level per turn: round 3 = wide fillet of the legs (`fillet_radius`), 2 = arc of rho
    // around the vertex, 1 = arc of min(c, rho), 0 = funnel points;
    // square 2 / 1 = mitred around rho / min(c, rho), 0 = funnel points.
    let top = match p.corner {
        Corner::Round => 3u8,
        Corner::Square => 2,
        Corner::Sharp => 0,
    };
    let mut level: Vec<u8> = turns
        .iter()
        .map(|t| {
            if t.rho < 1e-3 || t.th < FLAT {
                0
            } else if top == 3 && fillet_radius(t, c) <= t.rho + t.ext + 1e-6 {
                2
            } else {
                top
            }
        })
        .collect();
    let circle = |k: usize, lv: u8| -> (P2, f64) {
        let t = &turns[k];
        match lv {
            3 => {
                let (re, r) = (t.rho + t.ext, fillet_radius(t, c));
                let cs = (0.5 * t.th.min(MAX_FILLET_TURN)).cos();
                (add(t.pv, scale(t.u, (re - r) / cs)), t.s * r)
            }
            2 => (t.pv, t.s * (t.rho + t.ext)),
            _ => (t.pv, t.s * (c.min(t.rho) + t.ext)),
        }
    };
    // Path items: (centre, signed radius, turn); a turn at level 0 gives its funnel points.
    let mut items: Vec<(P2, f64, usize)> = Vec::with_capacity(n);
    let mut legs: Vec<(P2, P2)> = Vec::with_capacity(n);
    let mut pts: Vec<P2> = Vec::with_capacity(n * 4);
    let mut owner: Vec<usize> = Vec::with_capacity(n * 4);
    // A circle the taut path does not touch (the legs turn the other way at it) is skipped;
    // 1 = skipped, 2 = was skipped but the shortcut failed validation.
    let mut skip: Vec<u8> = vec![0; nt];
    let mut ok = false;
    'outer: for _ in 0..5 * nt + 4 {
        items.clear();
        for (k, t) in turns.iter().enumerate() {
            if skip[k] == 1 {
                continue;
            }
            if level[k] == 0 {
                items.extend((t.i0..=t.i1).map(|i| (base[i], 0.0, k)));
            } else {
                let (o, r) = circle(k, level[k]);
                items.push((o, r, k));
            }
        }
        legs.clear();
        for w in items.windows(2) {
            match tangent(w[0].0, w[0].1, w[1].0, w[1].1) {
                Some(t) => legs.push(t),
                None => {
                    // Shrink the larger circle of the pair.
                    let (x, y) = if w[0].1.abs() >= w[1].1.abs() {
                        (w[0].2, w[1].2)
                    } else {
                        (w[1].2, w[0].2)
                    };
                    let k = if level[x] > 0 { x } else { y };
                    if level[k] == 0 {
                        break 'outer;
                    }
                    level[k] -= 1;
                    continue 'outer;
                }
            }
        }
        // A skipped circle must stay clear of the leg that replaced it.
        for (j, w) in items.windows(2).enumerate() {
            for k in w[0].2 + 1..w[1].2 {
                if skip[k] == 1 {
                    let (o, r) = circle(k, level[k].max(1));
                    if seg_dist(o, legs[j].0, legs[j].1) < 0.98 * r.abs() {
                        skip[k] = 2;
                        continue 'outer;
                    }
                }
            }
        }
        for (j, &(_, r, k)) in items.iter().enumerate() {
            if r != 0.0 && skip[k] == 0 {
                let (din, dout) = (
                    norm(sub(legs[j - 1].1, legs[j - 1].0)),
                    norm(sub(legs[j].1, legs[j].0)),
                );
                if (din[0] * dout[1] - din[1] * dout[0]) * r < -1e-6 {
                    skip[k] = 1;
                    continue 'outer;
                }
            }
        }
        pts.clear();
        owner.clear();
        for (j, &(o, r, k)) in items.iter().enumerate() {
            if r == 0.0 {
                pts.push(o);
            } else if p.corner == Corner::Round {
                // The arc turns the path by its sweep: much more than the funnel turned there
                // means the tangents wrapped around the circle.
                let n0 = pts.len();
                if arc_sweep(o, r, legs[j - 1].1, legs[j].0) > turns[k].th + 1.0
                    || !arc(o, r, legs[j - 1].1, legs[j].0, &mut pts)
                {
                    pts.truncate(n0);
                    level[k] -= 1;
                    continue 'outer;
                }
            } else {
                // Square: the legs meet in a mitre, or in two corners on the line tangent to
                // the circle at its apex when the turn is sharp.
                let ((p0, p1), (p2, p3)) = (legs[j - 1], legs[j]);
                let t = &turns[k];
                let near = |x: P2| dist(x, o) <= 4.0 * r.abs() + 1e-9;
                if t.th > CHAMFER {
                    let apex = add(o, scale(t.u, r.abs()));
                    let e = add(apex, [-t.u[1], t.u[0]]);
                    match (meet(p0, p1, apex, e), meet(apex, e, p2, p3)) {
                        (Some(x1), Some(x2)) if near(x1) && near(x2) => {
                            pts.push(x1);
                            pts.push(x2);
                        }
                        _ => pts.extend([p1, p2]),
                    }
                } else {
                    match meet(p0, p1, p2, p3) {
                        Some(x) if near(x) => pts.push(x),
                        _ => pts.extend([p1, p2]),
                    }
                }
            }
            owner.resize(pts.len(), k);
        }
        let bad = first_bad(m, sub_chain[0], &pts).or_else(|| {
            (0..pts.len() - 1).find(|&s| {
                (level[owner[s]] > 0 || level[owner[s + 1]] > 0)
                    && too_close(pts[s], pts[s + 1], &near, 0.9 * c)
            })
        });
        match bad {
            None => {
                ok = true;
                break;
            }
            Some(s) => {
                let (x, y) = (owner[s], owner[s + 1]);
                // Restore circles skipped between the two ends first.
                let mut restored = false;
                for f in &mut skip[x.min(y)..=x.max(y)] {
                    if *f == 1 {
                        *f = 2;
                        restored = true;
                    }
                }
                if restored {
                    continue;
                }
                let before: u32 = level.iter().map(|&l| l as u32).sum();
                for k in [x, y] {
                    level[k] = level[k].saturating_sub(1);
                }
                if level.iter().map(|&l| l as u32).sum::<u32>() == before {
                    // Both ends already plain: downgrade the neighbours too.
                    for k in [x.saturating_sub(1), (y + 1).min(nt - 1)] {
                        level[k] = level[k].saturating_sub(1);
                    }
                    if level.iter().map(|&l| l as u32).sum::<u32>() == before {
                        break;
                    }
                }
            }
        }
    }
    if !ok {
        pts.clear();
        pts.extend_from_slice(&base);
    }
    wb.pts = base;

    let mut full: Vec<P2> = Vec::with_capacity(pts.len() + 32);
    if ea.is_some() {
        full.push(a2);
    }
    for &q in &pts {
        if full.last().is_none_or(|&l| dist(l, q) > 1e-6) {
            full.push(q);
        }
    }
    if eb.is_some() {
        full.push(b2);
    }
    match p.corner {
        Corner::Round => lead_fillets(m, chain[0], &mut full, ea.is_some(), eb.is_some(), p),
        Corner::Square => orthogonalize(m, chain[0], &mut full),
        Corner::Sharp => {}
    }
    simplify(m, chain[0], &mut full);
    if ext.is_empty() {
        trace_out(m, chain, &full, a, b, out, surf);
    } else {
        let mut all = chain.to_vec();
        all.extend_from_slice(&ext);
        trace_out(m, &all, &full, a, b, out, surf);
    }
}

/// Round the turn at the end of a door lead (`full[1]` / `full[n - 2]`): the walk leaves the
/// door straight and swings onto its next leg. Fillets that leave walkable space are dropped.
fn lead_fillets(m: &Mesh, t0: u32, full: &mut Vec<P2>, start: bool, end: bool, p: &Profile) {
    let mut tmp: Vec<P2> = Vec::with_capacity(full.len() + 16);
    // Try both, then each one alone.
    for (fs, fe) in [(start, end), (start, false), (false, end)] {
        if !(fs || fe) {
            continue;
        }
        let n = full.len();
        if n < 3 || (fs && fe && n < 4) {
            continue;
        }
        tmp.clear();
        tmp.push(full[0]);
        let mut k = 1;
        if fs {
            let (a, q, nx) = (full[0], full[1], full[2]);
            let l = dist(a, q);
            let tout = if fe && n == 4 { 0.45 } else { 0.5 } * dist(q, nx);
            fillet_cap(a, q, nx, l.max(p.corner_radius), 0.6 * l, tout, &mut tmp);
            k = 2;
        }
        let stop = if fe { n - 2 } else { n - 1 };
        tmp.extend_from_slice(&full[k..stop]);
        if fe {
            let (pr, q, b) = (full[n - 3], full[n - 2], full[n - 1]);
            let l = dist(q, b);
            let tin = if fs && n == 4 { 0.45 } else { 0.5 } * dist(pr, q);
            fillet_cap(pr, q, b, l.max(p.corner_radius), tin, 0.6 * l, &mut tmp);
            tmp.push(b);
        } else {
            tmp.push(full[n - 1]);
        }
        if first_bad(m, t0, &tmp).is_none() {
            std::mem::swap(full, &mut tmp);
            return;
        }
    }
}

/// Square corners: a diagonal leg between two (nearly) perpendicular legs becomes an L along
/// them, one between two parallel legs a perpendicular step half way, where that stays
/// walkable.
fn orthogonalize(m: &Mesh, t0: u32, full: &mut Vec<P2>) {
    let mut tmp: Vec<P2> = Vec::with_capacity(full.len() + 8);
    let mut k = 1;
    while k + 2 < full.len() {
        let (p0, p, q, q1) = (full[k - 1], full[k], full[k + 1], full[k + 2]);
        let (din, dout, d) = (norm(sub(p, p0)), norm(sub(q1, q)), sub(q, p));
        let l = len(d);
        if l < 0.5 || len(din) < 0.5 || len(dout) < 0.5 {
            k += 1;
            continue;
        }
        let dd = scale(d, 1.0 / l);
        if dot(dd, din).abs() > 0.985 || dot(dd, dout).abs() > 0.985 {
            k += 1;
            continue; // already along one of them
        }
        let mut ins: [P2; 2] = [[0.0; 2]; 2];
        let mut ni = 0;
        if (din[0] * dout[1] - din[1] * dout[0]).abs() > 0.95 {
            if let Some(x) = meet(p, add(p, din), q, add(q, dout))
                && dot(sub(x, p), din) > 0.0
                && dot(sub(q, x), dout) > 0.0
            {
                ins[0] = x;
                ni = 1;
            }
        } else if dot(din, dout) > 0.95 {
            let along = dot(d, din);
            if along > 0.0 {
                let m1 = add(p, scale(din, 0.5 * along));
                ins = [m1, add(m1, sub(d, scale(din, along)))];
                ni = 2;
            }
        }
        if ni == 0 {
            k += 1;
            continue;
        }
        tmp.clear();
        tmp.extend_from_slice(&full[..=k]);
        tmp.extend_from_slice(&ins[..ni]);
        tmp.extend_from_slice(&full[k + 1..]);
        if first_bad(m, t0, &tmp).is_none() {
            std::mem::swap(full, &mut tmp);
            k += ni + 1;
        } else {
            k += 1;
        }
    }
}

/// Drop points closer than 2 cm to the previous one and points where the path turns by less
/// than 1.5 degrees and less than 1 cm, as long as the shortcut stays walkable.
fn simplify(m: &Mesh, t0: u32, pts: &mut Vec<P2>) {
    let n = pts.len();
    if n < 3 {
        return;
    }
    let mut keep: Vec<P2> = Vec::with_capacity(n);
    keep.push(pts[0]);
    let mut t = t0;
    for i in 1..n - 1 {
        let (p, q, r) = (*keep.last().unwrap(), pts[i], pts[i + 1]);
        let drop = dist(p, q) < 0.02 || (turn(p, q, r) < 0.026 && seg_dist(q, p, r) < 0.01);
        if drop && m.seg_inside(t, p, r).is_some() {
            continue;
        }
        match m.seg_inside(t, p, q) {
            Some(t2) => t = t2,
            None => return, // the input itself is not walkable from here: leave it as is
        }
        keep.push(q);
    }
    keep.push(pts[n - 1]);
    *pts = keep;
}

/// 3D polyline of `full` (2D) with points added where the walk changes surface or crosses
/// sloped triangles; `surf[k]` is the surface element of segment k -> k+1.
fn trace_out(
    m: &Mesh,
    chain: &[u32],
    full: &[P2],
    a: P3,
    b: P3,
    out: &mut Vec<P3>,
    surf: &mut Vec<u32>,
) {
    let start = out.len();
    let tri_of = |q: P2| -> u32 {
        locate_in(m, chain, q).map(|k| chain[k]).unwrap_or_else(|| {
            *chain
                .iter()
                .min_by(|&&x, &&y| {
                    dist(m.tri_c[x as usize], q).total_cmp(&dist(m.tri_c[y as usize], q))
                })
                .unwrap()
        })
    };
    let n = full.len();
    let mut t = chain[0];
    out.push(a);
    surf.push(m.tri_surf[t as usize]);
    let mut cr: Vec<(P2, u32, u32)> = vec![];
    for k in 0..n.saturating_sub(1) {
        let (p, q) = (full[k], full[k + 1]);
        cr.clear();
        let end = m.trace(t, p, q, |x, f, to| {
            let (sf, st) = (m.tri_surf[f as usize], m.tri_surf[to as usize]);
            if sf != st || m.tri_slope[f as usize] > 1e-4 || m.tri_slope[to as usize] > 1e-4 {
                cr.push((x, f, to));
            }
        });
        match end {
            Some(te) => {
                for &(x, f, to) in &cr {
                    let (z0, z1) = (m.z_at(f, x), m.z_at(to, x));
                    out.push([x[0], x[1], z0]);
                    surf.push(m.tri_surf[to as usize]);
                    if (z1 - z0).abs() > 1e-9 {
                        // Step between surfaces of different heights.
                        out.push([x[0], x[1], z1]);
                        surf.push(m.tri_surf[to as usize]);
                    }
                }
                t = te;
            }
            None => t = tri_of(q),
        }
        let z = if k + 2 == n { b[2] } else { m.z_at(t, q) };
        out.push([q[0], q[1], z]);
        surf.push(m.tri_surf[t as usize]);
    }
    // Drop zero-length steps (keep the surface of the segment that follows).
    let mut w = start;
    for r in start..out.len() {
        if r == start || dist3(out[r], out[w - 1]) > 1e-9 {
            out[w] = out[r];
            surf[w] = surf[r];
            w += 1;
        } else {
            surf[w - 1] = surf[r];
        }
    }
    out.truncate(w);
    surf.truncate(w);
}
