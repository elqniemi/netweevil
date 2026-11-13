//! Cost matrix of a multi-stop route (`multistop.rs`): one one-to-many search per point
//! (`QueryCtx::many`) instead of a search per pair. Each point is resolved and attached once,
//! as a source and as a target, the same way `route_with` attaches its ends; two points in
//! one zone also get the direct walk between them. The legs are routed in full afterwards,
//! so the matrix only has to order the stops.

use super::{End, Leg, Q, approach};
use crate::closure::Overlay;
use crate::cost::CompiledProfile;
use crate::graph::Graph;
use crate::locate::Loc;
use crate::search::QueryCtx;
use crate::tsp::Matrix;

/// Costs between `locs` (row-major; INFINITY: no way, and on the diagonal). `names` label
/// the points in errors. With `end`, the last point is only a target and the first only a
/// source. Returns the matrix and the nodes the searches expanded.
pub(crate) fn matrix(
    g: &Graph,
    cp: &CompiledProfile,
    ctx: &mut QueryCtx,
    locs: &[Loc],
    names: &[String],
    end: bool,
    ov: Option<&Overlay>,
) -> Result<(Matrix, u32), String> {
    let k = locs.len();
    ctx.wb.new_query();
    ctx.pool.clear();
    let ov = ov.filter(|o| !o.is_empty());
    let mut q = Q {
        g,
        cp,
        legs: vec![],
        ov,
    };
    let mut ends = Vec::with_capacity(k);
    for (loc, name) in locs.iter().zip(names) {
        let mut e = g.resolve(loc, cp).map_err(|e| format!("{name}: {e}"))?;
        // A closed room (`approach`): its nearest walkable point outside, as from elsewhere.
        let mut other = End::Nodes(vec![]);
        approach(g, cp, &mut e, loc, &mut other, &Loc::default())
            .map_err(|e| format!("{name}: {e}"))?;
        ends.push(e);
    }
    // Every point but the start as a target: (node, cost, target), sorted by node.
    let mut exits = vec![];
    for (j, e) in ends.iter().enumerate().skip(1) {
        let x0 = exits.len();
        attach(&mut q, ctx, e, true, &mut exits);
        for x in &mut exits[x0..] {
            x.2 = j as u32;
        }
    }
    exits.sort_unstable_by_key(|x| x.0);
    let rows = if end { k - 1 } else { k };
    let mut m = Matrix {
        k,
        c: vec![f32::INFINITY; k * k],
    };
    let mut expanded = 0;
    let mut sources = vec![];
    for i in 0..rows {
        sources.clear();
        attach(&mut q, ctx, &ends[i], false, &mut sources);
        let row = &mut m.c[i * k..(i + 1) * k];
        // Points in the same zone: the direct walk (the search only sees zone nodes).
        if let End::Tri(t, p) = ends[i] {
            for (j, e) in ends.iter().enumerate().skip(1) {
                if let (true, &End::Tri(t2, p2)) = (j != i, e)
                    && g.mesh.tri_zone[t as usize] == g.mesh.tri_zone[t2 as usize]
                {
                    let mut none = vec![];
                    if let Some((c, _)) =
                        q.attach_nodes(ctx, t, p, false, Some((t2, p2)), 0, &mut none)
                    {
                        row[j] = c;
                    }
                }
            }
        }
        let allow = cp.allowed_areas(&sources, &exits);
        expanded += ctx.many(g, cp, &sources, &exits, row, &allow, ov);
        row[i] = f32::INFINITY;
    }
    Ok((m, expanded))
}

/// Attachment of an end as a source (`to_point` false) or a target, as in `route_with`.
pub(super) fn attach(
    q: &mut Q<'_>,
    ctx: &mut QueryCtx,
    e: &End,
    to_point: bool,
    out: &mut Vec<(u32, f32, u32)>,
) {
    for (part, _) in e.parts() {
        match part {
            End::Tri(t, p) => {
                q.attach_zone(ctx, *t, *p, to_point, None, out);
            }
            End::Edge(f, s, _) => q.attach_edge(*f, *s, to_point, out),
            End::Nodes(ns) => {
                for &n in ns {
                    let leg = q.leg(Leg::Stay);
                    out.push((n, 0.0, leg));
                }
            }
            End::Pois(_) => unreachable!(),
        }
    }
}
