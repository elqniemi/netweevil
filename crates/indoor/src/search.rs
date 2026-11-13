//! A* over the CSR graph with generation-stamped scratch held in a reusable `QueryCtx`.

use crate::closure::Overlay;
use crate::cost::CompiledProfile;
use crate::funnel::WalkBuf;
use crate::geom::{MinHeap, NONE};
use crate::graph::Graph;
use crate::mesh::TriScratch;
use std::f32::consts::PI;

/// Per-thread query scratch; sized for one graph, reused across queries.
pub struct QueryCtx {
    g: Vec<f32>,
    pub parent: Vec<u32>,
    stamp: Vec<u32>,
    closed: Vec<u32>,
    exit: Vec<f32>,
    pub exit_leg: Vec<u32>,
    exit_stamp: Vec<u32>,
    pub src_leg: Vec<u32>,
    epoch: u32,
    heap: MinHeap,
    pub tri: TriScratch,
    pub wb: WalkBuf,
    pub pool: Vec<u32>,
    pub cand: Vec<(f32, u32)>,
    /// Per target of the last `many`: (node, index into its `exits`) of the cheapest way
    /// through the graph; (NONE, NONE) when no graph path beat the target's direct candidate.
    pub many_end: Vec<(u32, u32)>,
}

pub struct SearchResult {
    pub cost: f32,
    /// Node where the path leaves the graph (NONE when the direct candidate won).
    pub end: u32,
    pub expanded: u32,
    /// The search met an arc a closure closes or slows (`closure.rs`).
    pub touched: bool,
}

impl QueryCtx {
    pub fn new(g: &Graph) -> QueryCtx {
        let n = g.nodes.len();
        QueryCtx {
            g: vec![0.0; n],
            parent: vec![NONE; n],
            stamp: vec![0; n],
            closed: vec![0; n],
            exit: vec![0.0; n],
            exit_leg: vec![NONE; n],
            exit_stamp: vec![0; n],
            src_leg: vec![NONE; n],
            epoch: 0,
            heap: MinHeap::new(),
            tri: TriScratch::new(g.mesh.tri_v.len()),
            wb: WalkBuf::default(),
            pool: vec![],
            cand: vec![],
            many_end: vec![],
        }
    }

    fn next_gen(&mut self) -> u32 {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.stamp.fill(0);
            self.closed.fill(0);
            self.exit_stamp.fill(0);
            self.epoch = 1;
        }
        self.epoch
    }

    #[inline]
    fn gv(&self, n: usize) -> f32 {
        if self.stamp[n] == self.epoch {
            self.g[n]
        } else {
            f32::INFINITY
        }
    }

    /// `sources`/`exits`: (node, cost, leg id). `target`: centre (graph-local coords) and
    /// radius such that every exit's cost covers its distance beyond the radius. `best0`: cost of
    /// a direct candidate that bypasses the graph. `allow`: the `leave_tags` areas the search
    /// may walk in (`CompiledProfile::allowed_areas`; ignored without `leave_tags`). `ov`:
    /// extra costs of closed and slowed arcs (`closure.rs`).
    pub fn astar(
        &mut self,
        g: &Graph,
        cp: &CompiledProfile,
        sources: &[(u32, f32, u32)],
        exits: &[(u32, f32, u32)],
        target: [f32; 3],
        radius: f32,
        best0: f32,
        allow: &[u32],
        ov: Option<&Overlay>,
    ) -> SearchResult {
        let epoch = self.next_gen();
        self.heap.clear();
        let inv = cp.inv_vmax;
        let radius = radius + cp.reach;
        let h = |n: usize| -> f32 {
            let p = g.node_xyz[n];
            let d = ((p[0] - target[0]).powi(2)
                + (p[1] - target[1]).powi(2)
                + (p[2] - target[2]).powi(2))
            .sqrt();
            (d - radius).max(0.0) * inv
        };
        for &(n, c, leg) in exits {
            let nu = n as usize;
            if self.exit_stamp[nu] != epoch || c < self.exit[nu] {
                self.exit_stamp[nu] = epoch;
                self.exit[nu] = c;
                self.exit_leg[nu] = leg;
            }
        }
        for &(n, c, leg) in sources {
            let nu = n as usize;
            if c < self.gv(nu) {
                self.stamp[nu] = epoch;
                self.g[nu] = c;
                self.parent[nu] = NONE;
                self.src_leg[nu] = leg;
                self.heap
                    .push((((c + h(nu)).to_bits() as u64) << 32) | n as u64);
            }
        }
        let turn = cp.p.turn_penalty as f32;
        // `leave_tags` areas: only those of the ends (`CompiledProfile::allowed_areas`).
        let gate = !cp.arc_area.is_empty();
        let mut best = best0;
        let mut end = NONE;
        let mut expanded = 0u32;
        let mut touched = false;
        while let Some(k) = self.heap.pop() {
            let f = f32::from_bits((k >> 32) as u32);
            if f >= best {
                break;
            }
            let u = (k & 0xffff_ffff) as usize;
            if self.closed[u] == epoch {
                continue;
            }
            self.closed[u] = epoch;
            expanded += 1;
            let gu = self.g[u];
            if self.exit_stamp[u] == epoch && gu + self.exit[u] < best {
                best = gu + self.exit[u];
                end = u as u32;
            }
            let in_head = if turn > 0.0 && self.parent[u] != NONE {
                let (fa, rev) = g.fact_of(self.parent[u] as usize);
                Some(if rev { fa.h0 + PI } else { fa.h1 })
            } else {
                None
            };
            for a in g.out_arcs(u as u32) {
                if gate && cp.arc_area[a] != NONE && !allow.contains(&cp.arc_area[a]) {
                    continue;
                }
                let mut c = cp.arc_cost[a];
                if let Some(x) = ov.and_then(|o| o.arc(a as u32)) {
                    touched = true;
                    c += x.0;
                }
                if !c.is_finite() {
                    continue;
                }
                let v = g.head[a] as usize;
                if let Some(hin) = in_head {
                    let (fa, rev) = g.fact_of(a);
                    let hout = if rev { fa.h1 + PI } else { fa.h0 };
                    let mut d = (hout - hin).rem_euclid(2.0 * PI);
                    if d > PI {
                        d = 2.0 * PI - d;
                    }
                    c += turn * d;
                }
                let ng = gu + c;
                if ng < self.gv(v) {
                    // Reopen: only happens where the heuristic is not consistent (`reach`).
                    self.closed[v] = 0;
                    self.stamp[v] = epoch;
                    self.g[v] = ng;
                    self.parent[v] = a as u32;
                    self.heap
                        .push((((ng + h(v)).to_bits() as u64) << 32) | v as u64);
                }
            }
        }
        SearchResult {
            cost: best,
            end,
            expanded,
            touched,
        }
    }

    /// One-to-many: a Dijkstra from `sources` that stops when every target is settled.
    /// `exits`: (node, cost, target) sorted by node; `best`: per target, the cost of a direct
    /// candidate (INFINITY: none), lowered to the cheapest way found. Same arc costs, turn
    /// penalty, `allow` and closures as `astar`. `src_leg` and `many_end` record the source
    /// leg per node and the end per target, so `unwind` gives each target's path.
    pub fn many(
        &mut self,
        g: &Graph,
        cp: &CompiledProfile,
        sources: &[(u32, f32, u32)],
        exits: &[(u32, f32, u32)],
        best: &mut [f32],
        allow: &[u32],
        ov: Option<&Overlay>,
    ) -> u32 {
        let epoch = self.next_gen();
        self.heap.clear();
        for &(n, _, _) in exits {
            self.exit_stamp[n as usize] = epoch;
        }
        self.many_end.clear();
        self.many_end.resize(best.len(), (NONE, NONE));
        for &(n, c, leg) in sources {
            let nu = n as usize;
            if c < self.gv(nu) {
                self.stamp[nu] = epoch;
                self.g[nu] = c;
                self.parent[nu] = NONE;
                self.src_leg[nu] = leg;
                self.heap.push(((c.to_bits() as u64) << 32) | n as u64);
            }
        }
        let turn = cp.p.turn_penalty as f32;
        let gate = !cp.arc_area.is_empty();
        let worst = |b: &[f32]| b.iter().copied().fold(0.0f32, f32::max);
        let mut bound = worst(best);
        let mut expanded = 0u32;
        while let Some(k) = self.heap.pop() {
            let f = f32::from_bits((k >> 32) as u32);
            if f >= bound {
                break;
            }
            let u = (k & 0xffff_ffff) as usize;
            if self.closed[u] == epoch {
                continue;
            }
            self.closed[u] = epoch;
            expanded += 1;
            let f = self.g[u];
            if self.exit_stamp[u] == epoch {
                let k0 = exits.partition_point(|e| (e.0 as usize) < u);
                for (i, &(_, c, t)) in exits[k0..]
                    .iter()
                    .enumerate()
                    .take_while(|e| e.1.0 as usize == u)
                {
                    if f + c < best[t as usize] {
                        best[t as usize] = f + c;
                        self.many_end[t as usize] = (u as u32, (k0 + i) as u32);
                        bound = worst(best);
                    }
                }
            }
            let in_head = if turn > 0.0 && self.parent[u] != NONE {
                let (fa, rev) = g.fact_of(self.parent[u] as usize);
                Some(if rev { fa.h0 + PI } else { fa.h1 })
            } else {
                None
            };
            for a in g.out_arcs(u as u32) {
                if gate && cp.arc_area[a] != NONE && !allow.contains(&cp.arc_area[a]) {
                    continue;
                }
                let mut c = cp.arc_cost[a];
                if let Some(x) = ov.and_then(|o| o.arc(a as u32)) {
                    c += x.0;
                }
                if !c.is_finite() {
                    continue;
                }
                if let Some(hin) = in_head {
                    let (fa, rev) = g.fact_of(a);
                    let hout = if rev { fa.h1 + PI } else { fa.h0 };
                    let mut d = (hout - hin).rem_euclid(2.0 * PI);
                    if d > PI {
                        d = 2.0 * PI - d;
                    }
                    c += turn * d;
                }
                let v = g.head[a] as usize;
                let ng = f + c;
                if ng < self.gv(v) {
                    self.closed[v] = 0;
                    self.stamp[v] = epoch;
                    self.g[v] = ng;
                    self.parent[v] = a as u32;
                    self.heap.push(((ng.to_bits() as u64) << 32) | v as u64);
                }
            }
        }
        expanded
    }

    /// Arcs from the source to `end`, in travel order (appended to `out`); returns the source node.
    pub fn unwind(&self, g: &Graph, end: u32, out: &mut Vec<u32>) -> u32 {
        let start = out.len();
        let mut n = end;
        while self.parent[n as usize] != NONE {
            let a = self.parent[n as usize];
            out.push(a);
            // Tail of arc a: the node whose CSR range contains it.
            n = g.first_out.partition_point(|&x| x <= a) as u32 - 1;
        }
        out[start..].reverse();
        n
    }
}
