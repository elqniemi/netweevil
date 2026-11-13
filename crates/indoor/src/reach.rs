//! Reachability diagnostics: weak components and a one-to-all Dijkstra (no heuristic) from a
//! seed over one profile's arc costs.

use crate::cost::CompiledProfile;
use crate::geom::{MinHeap, NONE};
use crate::graph::{ElemKind, FactKind, Graph};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Serialize, Debug, Clone, Default)]
pub struct Reach {
    /// Weakly connected components over arcs usable by the profile (usable nodes only).
    pub components: usize,
    /// Nodes in the largest component.
    pub largest: usize,
    /// Seed nodes (the given `from`, else one node of the largest component).
    pub seed: Vec<u32>,
    pub nodes: usize,
    pub reached_nodes: usize,
    /// Largest finite cost (seconds) from the seed.
    pub max_cost: f64,
    pub spaces: usize,
    /// Space ids (surface `space`, else the surface id) with no reached walkable triangle.
    pub unreachable_spaces: Vec<String>,
    /// Named nodes (input nodes / edge `from`/`to` ids) not reached.
    pub unreachable_nodes: Vec<String>,
    /// Portal / vertical / edge elements with an end that is not reached.
    pub unreachable_portals: Vec<String>,
    pub unreachable_verticals: Vec<String>,
    pub unreachable_edges: Vec<String>,
    /// POIs with no reached access point.
    pub unreachable_pois: Vec<String>,
    /// Totals before capping the lists above.
    pub counts: ReachCounts,
    /// Cost per node from the seed (INFINITY = unreachable).
    #[serde(skip)]
    pub dist: Vec<f32>,
}

#[derive(Serialize, Debug, Clone, Default)]
pub struct ReachCounts {
    pub spaces: usize,
    pub nodes: usize,
    pub portals: usize,
    pub verticals: usize,
    pub edges: usize,
    pub pois: usize,
}

fn find(p: &mut [u32], mut x: u32) -> u32 {
    while p[x as usize] != x {
        p[x as usize] = p[p[x as usize] as usize];
        x = p[x as usize];
    }
    x
}

/// Reachability from `from` (node, portal or space id; None = the largest component) under
/// profile `cp`. Id lists are capped at `cap` entries (`counts` has the totals).
pub fn reach(
    g: &Graph,
    cp: &CompiledProfile,
    from: Option<&str>,
    cap: usize,
) -> Result<Reach, String> {
    let nn = g.nodes.len();
    // Components (union-find over usable arcs, undirected).
    let mut par: Vec<u32> = (0..nn as u32).collect();
    for u in 0..nn as u32 {
        for a in g.out_arcs(u) {
            if cp.arc_cost[a].is_finite() {
                let (x, y) = (find(&mut par, u), find(&mut par, g.head[a]));
                if x != y {
                    par[x as usize] = y;
                }
            }
        }
    }
    let mut size = vec![0u32; nn];
    for n in 0..nn as u32 {
        if cp.usable[n as usize] {
            let r = find(&mut par, n);
            size[r as usize] += 1;
        }
    }
    let components = size.iter().filter(|&&s| s > 0).count();
    let big = (0..nn).max_by_key(|&r| size[r]).filter(|&r| size[r] > 0);
    let largest = big.map(|r| size[r] as usize).unwrap_or(0);

    // Seeds.
    let mut seed_zone = NONE;
    let seed: Vec<u32> = match from.filter(|s| !s.is_empty()) {
        Some(id) => {
            if let Some(&n) = g.names.get(id) {
                vec![n]
            } else if let Some(p) = g.portals.get(id) {
                p.to_vec()
            } else if let Some(&t) = g.space_tri.get(id) {
                seed_zone = g.mesh.tri_zone[t as usize];
                g.zone_nodes(seed_zone).to_vec()
            } else {
                return Err(format!("unknown node, portal or space {id:?}"));
            }
        }
        None => match big {
            Some(r) => (0..nn as u32)
                .find(|&n| cp.usable[n as usize] && find(&mut par, n) == r as u32)
                .into_iter()
                .collect(),
            None => vec![],
        },
    };

    // Dijkstra.
    let mut dist = vec![f32::INFINITY; nn];
    let mut heap = MinHeap::new();
    for &s in &seed {
        dist[s as usize] = 0.0;
        heap.push(s as u64);
    }
    while let Some(k) = heap.pop() {
        let d = f32::from_bits((k >> 32) as u32);
        let u = (k & 0xffff_ffff) as u32;
        if d > dist[u as usize] {
            continue;
        }
        for a in g.out_arcs(u) {
            let nd = d + cp.arc_cost[a];
            let v = g.head[a] as usize;
            if nd < dist[v] {
                dist[v] = nd;
                heap.push(((nd.to_bits() as u64) << 32) | v as u64);
            }
        }
    }
    let reached = |n: u32| dist[n as usize].is_finite();
    let reached_nodes = dist.iter().filter(|d| d.is_finite()).count();
    let max_cost = dist
        .iter()
        .filter(|d| d.is_finite())
        .fold(0.0f32, |m, &d| m.max(d)) as f64;

    // Spaces: reached if one of their walkable triangles lies in a zone with a reached node.
    let nz = g.zone_level.len();
    let mut zone_ok = vec![false; nz];
    if seed_zone != NONE {
        zone_ok[seed_zone as usize] = true;
    }
    for (n, nd) in g.nodes.iter().enumerate() {
        if nd.zone != NONE && dist[n].is_finite() {
            zone_ok[nd.zone as usize] = true;
        }
    }
    let key = |e: u32| -> &str {
        let el = &g.elems[e as usize];
        if el.space != NONE {
            &g.strings[el.space as usize]
        } else {
            &el.id
        }
    };
    let mut all: BTreeSet<&str> = BTreeSet::new();
    let mut ok: BTreeSet<&str> = BTreeSet::new();
    for t in 0..g.mesh.tri_v.len() as u32 {
        let e = g.mesh.tri_surf[t as usize];
        let k = key(e);
        all.insert(k);
        if zone_ok[g.mesh.tri_zone[t as usize] as usize] && cp.tri_weight(g, t).is_finite() {
            ok.insert(k);
        }
    }
    let unreachable_spaces: Vec<String> = all
        .iter()
        .filter(|k| !ok.contains(*k))
        .map(|k| k.to_string())
        .collect();

    let unreachable_nodes: Vec<String> = g
        .names
        .iter()
        .filter(|&(_, &n)| !reached(n))
        .map(|(k, _)| k.clone())
        .collect();

    // Elements: every end of every fact reached.
    let mut elem_ok = vec![true; g.elems.len()];
    let mut elem_seen = vec![false; g.elems.len()];
    for f in &g.facts {
        if f.elem == NONE
            || !matches!(
                f.kind,
                FactKind::Edge | FactKind::Portal | FactKind::Vertical
            )
        {
            continue;
        }
        elem_seen[f.elem as usize] = true;
        if !reached(f.na) || !reached(f.nb) {
            elem_ok[f.elem as usize] = false;
        }
    }
    let (mut up, mut uv, mut ue) = (vec![], vec![], vec![]);
    for (i, e) in g.elems.iter().enumerate() {
        let list = match e.kind {
            ElemKind::Portal => &mut up,
            ElemKind::Vertical => &mut uv,
            ElemKind::Edge => &mut ue,
            _ => continue,
        };
        // Elements without any fact (dropped at compile) count as unreachable.
        if !elem_seen[i] || !elem_ok[i] {
            list.push(e.id.clone());
        }
    }
    let unreachable_pois: Vec<String> = g
        .pois
        .iter()
        .enumerate()
        .filter(|&(i, p)| {
            !cp.poi_ok(i as u32)
                || !p.nodes.iter().any(|&n| reached(n))
                    && !p.snaps.iter().any(|sn| {
                        let f = &g.facts[sn.fact as usize];
                        cp.elem_usable(f.elem) && (reached(f.na) || reached(f.nb))
                    })
        })
        .map(|(_, p)| p.id.clone())
        .collect();
    let counts = ReachCounts {
        pois: unreachable_pois.len(),
        spaces: unreachable_spaces.len(),
        nodes: unreachable_nodes.len(),
        portals: up.len(),
        verticals: uv.len(),
        edges: ue.len(),
    };
    let capped = |mut v: Vec<String>| {
        v.truncate(cap);
        v
    };
    Ok(Reach {
        components,
        largest,
        seed,
        nodes: nn,
        reached_nodes,
        max_cost,
        spaces: all.len(),
        unreachable_spaces: capped(unreachable_spaces),
        unreachable_nodes: capped(unreachable_nodes),
        unreachable_portals: capped(up),
        unreachable_verticals: capped(uv),
        unreachable_edges: capped(ue),
        unreachable_pois: capped(unreachable_pois),
        counts,
        dist,
    })
}
