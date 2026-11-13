//! Access-controlled routing (`access_groups`).
//!
//! Every element and POI carries an access mask (bit i = `Graph::access_groups[i]`, 0 =
//! public). A query is granted a mask; an element is denied when it is restricted and shares
//! no bit with it (any-of). `CompiledProfile::restrict` derives a per-mask profile from the
//! unrestricted one by patching only the arcs that touch restricted elements, so access needs
//! no rebuild and no full cost recompute per user.

use crate::cost::CompiledProfile;
use crate::geom::NONE;
use crate::graph::Graph;

/// Access groups that get their own bit; more share the reserved bit 63.
pub const MAX_GROUPS: usize = 63;
/// The reserved bit of groups beyond `MAX_GROUPS`: never granted by name (fail closed).
pub const OVERFLOW: u64 = 1 << 63;
/// Every group (`"*"`): trusted full access, including the overflow bit.
pub const ALL: u64 = u64::MAX;
/// Name reported for the overflow bit.
pub const OVERFLOW_NAME: &str = "@overflow";

/// Restricted elements of a graph and the arcs whose cost depends on them (computed once).
#[derive(Default, Debug)]
pub struct Restricted {
    pub elems: Vec<u32>,
    pub arcs: Vec<u32>,
    pub pois: Vec<u32>,
}

/// Element / POI with mask `access` denied to a query granted `granted`.
#[inline]
pub fn denied(access: u64, granted: u64) -> bool {
    access != 0 && access & granted == 0
}

impl Graph {
    /// Mask of granted group names: unknown names are ignored, `"*"` grants everything.
    pub fn access_mask<S: AsRef<str>>(&self, granted: &[S]) -> u64 {
        let mut m = 0u64;
        for s in granted {
            let s = s.as_ref();
            if s == "*" {
                return ALL;
            }
            if let Some(i) = self.access_groups.iter().position(|g| g == s) {
                m |= 1 << i;
            }
        }
        m
    }

    /// Group names of a mask (bit 63: `@overflow`).
    pub fn access_names(&self, mask: u64) -> Vec<String> {
        (0..64)
            .filter(|i| mask & (1u64 << i) != 0)
            .map(|i| {
                self.access_groups
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| OVERFLOW_NAME.to_string())
            })
            .collect()
    }

    /// Group names element `e` needs (None when public).
    pub fn elem_access(&self, e: u32) -> Option<Vec<String>> {
        let a = self.elems.get(e as usize)?.access;
        (a != 0).then(|| self.access_names(a))
    }

    /// Any element or POI restricted.
    pub fn has_access_rules(&self) -> bool {
        let r = self.restricted();
        !r.elems.is_empty() || !r.pois.is_empty()
    }

    /// Portal element of a portal end node (NONE if none).
    pub fn portal_elem(&self, n: u32) -> u32 {
        self.out_arcs(n)
            .map(|a| self.fact_of(a).0)
            .find(|f| f.kind == crate::graph::FactKind::Portal)
            .map_or(NONE, |f| f.elem)
    }

    pub(crate) fn restricted(&self) -> &Restricted {
        self.restricted.get_or_init(|| {
            let restricted: Vec<bool> = self.elems.iter().map(|e| e.access != 0).collect();
            let elems: Vec<u32> = (0..self.elems.len() as u32)
                .filter(|&e| restricted[e as usize])
                .collect();
            let pois: Vec<u32> = (0..self.pois.len() as u32)
                .filter(|&i| self.pois[i as usize].access != 0)
                .collect();
            let mut arcs = vec![];
            if !elems.is_empty() {
                let r = |e: u32| e != NONE && restricted[e as usize];
                for a in 0..self.num_arcs() {
                    let (f, _) = self.fact_of(a);
                    if r(f.elem)
                        || r(self.nodes[self.head[a] as usize].elem)
                        || self.pieces[f.p0 as usize..f.p1 as usize]
                            .iter()
                            .any(|p| r(p.elem))
                    {
                        arcs.push(a as u32);
                    }
                }
            }
            Restricted { elems, arcs, pois }
        })
    }
}

impl CompiledProfile {
    /// This profile for a query granted `granted` (`Graph::access_mask`): denied elements get
    /// INFINITY factors, the arcs touching restricted elements are re-costed, `usable` /
    /// `zone_open` recomputed, denied POIs flagged. The heuristic is kept (costs only grow, so
    /// it stays admissible). None when nothing is denied (use `self`). `self` should be the
    /// unrestricted profile; on a restricted one the denials add up.
    pub fn restrict(&self, g: &Graph, granted: u64) -> Option<CompiledProfile> {
        if granted == ALL {
            return None;
        }
        let r = g.restricted();
        let elems: Vec<u32> = r
            .elems
            .iter()
            .copied()
            .filter(|&e| denied(g.elems[e as usize].access, granted))
            .collect();
        let pois: Vec<u32> = r
            .pois
            .iter()
            .copied()
            .filter(|&i| denied(g.pois[i as usize].access, granted))
            .collect();
        if elems.is_empty() && pois.is_empty() {
            return None;
        }
        let mut cp = self.clone();
        cp.granted = self.granted & granted;
        for &e in &elems {
            let e = e as usize;
            cp.factor[e] = f32::INFINITY;
            cp.factor_up[e] = f32::INFINITY;
            cp.factor_down[e] = f32::INFINITY;
        }
        if !elems.is_empty() {
            for &a in &r.arcs {
                let a = a as usize;
                cp.arc_cost[a] = cp.arc_cost_of(g, a, true);
                cp.arc_time[a] = cp.arc_cost_of(g, a, false);
            }
            cp.connectivity(g);
            let locked: Vec<bool> = cp
                .zone_open
                .iter()
                .enumerate()
                .map(|(z, &o)| !o && self.zone_open.get(z).copied().unwrap_or(false))
                .collect();
            if locked.iter().any(|&l| l) {
                cp.zone_locked = locked;
            }
        }
        if !pois.is_empty() {
            let mut pd = if cp.poi_denied.is_empty() {
                vec![false; g.pois.len()]
            } else {
                cp.poi_denied.clone()
            };
            for i in pois {
                pd[i as usize] = true;
            }
            cp.poi_denied = pd;
        }
        Some(cp)
    }

    /// Element `e` denied to this profile's query (false for NONE / unrestricted profiles).
    #[inline]
    pub fn denies(&self, g: &Graph, e: u32) -> bool {
        self.granted != ALL
            && e != NONE
            && g.elems
                .get(e as usize)
                .is_some_and(|x| denied(x.access, self.granted))
    }

    /// Triangle `t` lies on a surface denied to this query.
    #[inline]
    pub fn tri_denied(&self, g: &Graph, t: u32) -> bool {
        self.granted != ALL && self.denies(g, g.mesh.tri_surf[t as usize])
    }

    /// POI `i` offered to this query.
    #[inline]
    pub fn poi_ok(&self, i: u32) -> bool {
        !self.poi_denied.get(i as usize).copied().unwrap_or(false)
    }

    /// Zone closed only for lack of access (never approached).
    #[inline]
    pub fn zone_locked(&self, z: u32) -> bool {
        self.zone_locked.get(z as usize).copied().unwrap_or(false)
    }
}
