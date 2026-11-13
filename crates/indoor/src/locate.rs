//! Point location: uniform grids over triangles (per level) and edge segments.

use crate::geom::*;
use crate::graph::Graph;
use serde::{Deserialize, Serialize};

/// A query endpoint.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Loc {
    pub at: Option<Vec<f64>>,
    pub level: Option<String>,
    pub space: Option<String>,
    pub node: Option<String>,
    pub portal: Option<String>,
    /// A POI id: the best of its access points.
    pub poi: Option<String>,
    /// Any of these POIs (the best one by route cost).
    pub pois: Vec<String>,
    /// The nearest POI (by route cost) in this category (`/`-prefix match) ...
    pub category: Option<String>,
    /// ... carrying all of these tags (also filters `pois`).
    pub tags: Vec<String>,
    /// POI ids never chosen (k-nearest by repeated queries).
    pub exclude: Vec<String>,
    /// Rooms without doors: a point in a closed zone (no way in or out) is reached from the
    /// nearest walkable point outside it.
    pub approach: bool,
}

impl Loc {
    /// Whether this locator names POIs (`poi`, `pois` or `category`).
    pub fn is_poi(&self) -> bool {
        self.poi.is_some() || !self.pois.is_empty() || self.category.is_some()
    }
}

impl Loc {
    /// `"x,y[,z]|level=L1"`, `"space=room_12"`, `"node=n1"`, `"portal=d1"`, `"poi=p1"`,
    /// `"category=toilet|tag=wheelchair"`; `approach` (a bare word) sets `approach`.
    pub fn parse(s: &str) -> Result<Loc, String> {
        let mut l = Loc::default();
        for part in s.split('|').map(str::trim).filter(|p| !p.is_empty()) {
            if part == "approach" {
                l.approach = true;
            } else if let Some((k, v)) = part.split_once('=') {
                let v = v.trim().to_string();
                match k.trim() {
                    "level" => l.level = Some(v),
                    "space" => l.space = Some(v),
                    "node" => l.node = Some(v),
                    "portal" => l.portal = Some(v),
                    "poi" => l.poi = Some(v),
                    "category" => l.category = Some(v),
                    "tag" => l.tags.push(v),
                    k => return Err(format!("unknown locator key {k:?}")),
                }
            } else {
                let c: Result<Vec<f64>, _> =
                    part.split(',').map(|x| x.trim().parse::<f64>()).collect();
                let c = c.map_err(|e| format!("bad coordinates {part:?}: {e}"))?;
                if c.len() < 2 || c.len() > 3 {
                    return Err(format!("bad coordinates {part:?}"));
                }
                l.at = Some(c);
            }
        }
        Ok(l)
    }
}

/// Where a point landed.
#[derive(Clone, Copy, Debug)]
pub enum Snap {
    Tri {
        tri: u32,
        p: P3,
    },
    /// On edge fact `fact` at fraction `s` (by 3D length) of its geometry.
    Edge {
        fact: u32,
        s: f64,
        p: P3,
    },
}

impl Graph {
    pub fn level_idx(&self, id: &str) -> Option<u32> {
        self.levels
            .iter()
            .position(|l| l.id == id)
            .map(|i| i as u32)
    }

    /// Triangle of `level` containing `p`.
    pub fn tri_at(&self, level: u32, p: P2) -> Option<u32> {
        let grid = self.tri_grid.get(level as usize)?;
        let base = self.level_tri[level as usize][0];
        for &k in grid.at(p) {
            let t = base + k;
            if self.mesh.contains(t, p, 1e-9) {
                return Some(t);
            }
        }
        None
    }

    /// Closest triangle of `level` within `max_d` accepted by `ok` (point on it, distance).
    pub fn nearest_tri(
        &self,
        level: u32,
        p: P2,
        max_d: f64,
        ok: impl Fn(u32) -> bool,
    ) -> Option<(u32, P2, f64)> {
        let grid = self.tri_grid.get(level as usize)?;
        let base = self.level_tri[level as usize][0];
        let mut best: Option<(u32, P2, f64)> = None;
        grid.visit(
            [p[0] - max_d, p[1] - max_d, p[0] + max_d, p[1] + max_d],
            |k| {
                let t = base + k;
                if !ok(t) {
                    return;
                }
                let [a, b, c] = self.mesh.pts(t);
                let (q, d2) = closest_on_tri(p, a, b, c);
                let d = d2.sqrt();
                if d <= max_d && best.is_none_or(|b| d < b.2) {
                    best = Some((t, q, d));
                }
            },
        );
        best
    }

    /// Nudge a boundary point a hair into its triangle so that it locates robustly.
    pub fn inset(&self, t: u32, q: P2) -> P2 {
        let c = self.mesh.tri_c[t as usize];
        let d = dist(q, c);
        if d < 1e-9 {
            q
        } else {
            lerp(q, c, (1e-4 / d).min(0.5))
        }
    }

    /// Nearest edge-fact segment (3D distance when `p` has z, else 2D), optionally on `level`.
    pub fn nearest_edge(
        &self,
        p: P3,
        has_z: bool,
        level: Option<u32>,
        max_d: f64,
        ok: &dyn Fn(u32) -> bool,
    ) -> Option<(u32, f64, P3, f64)> {
        let mut best: Option<(u32, u32, f64, P3, f64)> = None;
        self.seg_grid.visit(
            [p[0] - max_d, p[1] - max_d, p[0] + max_d, p[1] + max_d],
            |k| {
                let [fact, gi] = self.segs[k as usize];
                let f = &self.facts[fact as usize];
                if !ok(fact) {
                    return;
                }
                if let Some(l) = level
                    && f.level != l
                    && f.level != NONE
                {
                    return;
                }
                let (a, b) = (self.geom[gi as usize], self.geom[gi as usize + 1]);
                let (q, t, d2) = closest_on_seg(xy(p), xy(a), xy(b));
                let z = a[2] + (b[2] - a[2]) * t;
                let d = if has_z {
                    (d2 + (z - p[2]).powi(2)).sqrt()
                } else {
                    d2.sqrt()
                };
                if d <= max_d && best.is_none_or(|bb| d < bb.4) {
                    best = Some((fact, gi, t, [q[0], q[1], z], d));
                }
            },
        );
        let (fact, gi, t, q, d) = best?;
        let f = &self.facts[fact as usize];
        let pts = &self.geom[f.g0 as usize..f.g1 as usize];
        let total = poly_len3(pts).max(1e-12);
        let k = (gi - f.g0) as usize;
        let before = poly_len3(&pts[..=k]) + dist3(pts[k], pts[k + 1]) * t;
        Some((fact, (before / total).clamp(0.0, 1.0), q, d))
    }

    /// Snap a coordinate: containing triangle, else the nearer of nearest triangle / edge
    /// within `max_snap`. Only triangles accepted by `tri_ok` and edge facts accepted by
    /// `edge_ok` (usable by the profile) are considered; if none is in reach, any is.
    /// Without level and z, the level whose surface is nearest z = 0 wins (ground floor).
    pub fn snap_point(
        &self,
        at: &[f64],
        level: Option<u32>,
        max_snap: f64,
        tri_ok: impl Fn(u32) -> bool,
        edge_ok: impl Fn(u32) -> bool,
    ) -> Option<Snap> {
        self.snap_with(at, level, max_snap, &tri_ok, &edge_ok)
            .or_else(|| self.snap_with(at, level, max_snap, &|_| true, &|_| true))
    }

    /// Snap onto the triangles of `level` accepted by `tri_ok` only (no edges, no fallback):
    /// a point inside one space, kept there.
    pub fn snap_within(
        &self,
        at: &[f64],
        level: u32,
        max_snap: f64,
        tri_ok: &dyn Fn(u32) -> bool,
    ) -> Option<Snap> {
        self.snap_with(&at[..2], Some(level), max_snap, tri_ok, &|_| false)
    }

    fn snap_with(
        &self,
        at: &[f64],
        level: Option<u32>,
        max_snap: f64,
        tri_ok: &dyn Fn(u32) -> bool,
        edge_ok: &dyn Fn(u32) -> bool,
    ) -> Option<Snap> {
        let p2 = [at[0], at[1]];
        let has_z = at.len() > 2;
        let levels: Vec<u32> = match level {
            Some(l) => vec![l],
            None => (0..self.levels.len() as u32).collect(),
        };
        // Height mismatch: to the given z, else (no level, no z) to the ground (z = 0).
        let zdiff = |t: u32| {
            if has_z {
                (self.mesh.z_at(t, p2) - at[2]).abs()
            } else if level.is_none() {
                self.mesh.z_at(t, p2).abs()
            } else {
                0.0
            }
        };
        // Containing triangle (closest in z when the level is not given).
        let mut best: Option<(u32, f64)> = None;
        for &l in &levels {
            if let Some(t) = self.tri_at(l, p2).filter(|&t| tri_ok(t)) {
                let dz = zdiff(t);
                if best.is_none_or(|b| dz < b.1) {
                    best = Some((t, dz));
                }
            }
        }
        if let Some((t, dz)) = best
            && (level.is_some() || !has_z || dz < 2.0)
        {
            return Some(Snap::Tri {
                tri: t,
                p: [p2[0], p2[1], self.mesh.z_at(t, p2)],
            });
        }
        // Nearest triangle: 2D distance (+ z mismatch when z is given; ground first otherwise).
        let zw = if has_z { 1.0 } else { 1e-6 };
        let mut tri_best: Option<(u32, P2, f64, f64)> = None;
        for &l in &levels {
            if let Some(r) = self.nearest_tri(l, p2, max_snap, tri_ok) {
                let d = r.2 + zw * zdiff(r.0);
                if tri_best.is_none_or(|b| d < b.3) {
                    tri_best = Some((r.0, r.1, r.2 + if has_z { zdiff(r.0) } else { 0.0 }, d));
                }
            }
        }
        let edge_best = self.nearest_edge(
            [p2[0], p2[1], if has_z { at[2] } else { 0.0 }],
            has_z,
            level,
            max_snap,
            edge_ok,
        );
        match (tri_best, edge_best) {
            (Some(t), e) if e.is_none_or(|e| t.2 <= e.3) => {
                let q = self.inset(t.0, t.1);
                Some(Snap::Tri {
                    tri: t.0,
                    p: [q[0], q[1], self.mesh.z_at(t.0, q)],
                })
            }
            (_, Some((fact, s, p, _))) => Some(Snap::Edge { fact, s, p }),
            _ => None,
        }
    }
}
