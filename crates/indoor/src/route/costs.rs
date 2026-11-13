//! Costs between one point and many (`Router::costs_to`, `Router::costs_from`). Ends are
//! resolved and attached as `route_with` does. Towards many targets it is one one-to-many
//! search (`QueryCtx::many`); from many sources it is one search per source, as the graph
//! has no reverse arcs. Each target's path is unwound from the search tree to sum its
//! duration and length; no geometry is drawn.

use super::{End, Leg, Q, approach, matrix::attach};
use crate::cost::CompiledProfile;
use crate::geom::NONE;
use crate::graph::Graph;
use crate::locate::Loc;
use crate::search::QueryCtx;
use serde::{Deserialize, Serialize};

/// Cost, duration and length of the cheapest path between two points.
///
/// `cost` is the search cost (seconds with the profile's preferences), the same value that
/// `route` reports. `duration` and `length` are those of the taut path the search found:
/// `route` draws walks with clearance and rounded corners and scales walk durations to the
/// drawn line, so its `duration` and `length` can be a few percent larger.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct RouteCost {
    pub cost: f64,
    pub duration: f64,
    pub length: f64,
}

/// Costs from `one` to each of `many` (`forward`), or from each of `many` to `one`. A point
/// of `many` that cannot be located, or that has no path, is None. Errors when `one` cannot
/// be located.
pub(crate) fn costs(
    g: &Graph,
    cp: &CompiledProfile,
    ctx: &mut QueryCtx,
    one: &Loc,
    many: &[Loc],
    forward: bool,
) -> Result<Vec<Option<RouteCost>>, String> {
    ctx.wb.new_query();
    ctx.pool.clear();
    let side = if forward { "from" } else { "to" };
    let a = end_of(g, cp, one).map_err(|e| format!("{side}: {e}"))?;
    let ends: Vec<Option<End>> = many.iter().map(|l| end_of(g, cp, l).ok()).collect();
    let mut q = Q {
        g,
        cp,
        legs: vec![],
        ov: None,
    };
    let mut out = vec![None; many.len()];
    let mut tmp = vec![];
    if forward {
        let mut sources = vec![];
        attach(&mut q, ctx, &a, false, &mut sources);
        // (node, cost, target, leg), sorted by node for `many`.
        let mut ex: Vec<(u32, f32, u32, u32)> = vec![];
        let mut best = vec![f32::INFINITY; many.len()];
        let mut direct_leg = vec![NONE; many.len()];
        for (j, e) in ends.iter().enumerate() {
            let Some(e) = e else { continue };
            tmp.clear();
            attach(&mut q, ctx, e, true, &mut tmp);
            ex.extend(tmp.iter().map(|&(n, c, leg)| (n, c, j as u32, leg)));
            if let Some((c, leg)) = direct(&mut q, ctx, &a, e) {
                best[j] = c;
                direct_leg[j] = leg;
            }
        }
        ex.sort_unstable_by_key(|x| x.0);
        let exits: Vec<(u32, f32, u32)> = ex.iter().map(|x| (x.0, x.1, x.2)).collect();
        if !sources.is_empty() && !exits.is_empty() {
            let allow = cp.allowed_areas(&sources, &exits);
            ctx.many(g, cp, &sources, &exits, &mut best, &allow, None);
        } else {
            ctx.many_end.clear();
            ctx.many_end.resize(many.len(), (NONE, NONE));
        }
        let mut arcs = vec![];
        for (j, o) in out.iter_mut().enumerate() {
            let (node, k) = ctx.many_end[j];
            *o = summary(
                &q,
                ctx,
                best[j],
                node,
                ex.get(k as usize).map(|x| x.3),
                direct_leg[j],
                &mut arcs,
            );
        }
    } else {
        let mut ex: Vec<(u32, f32, u32, u32)> = vec![];
        attach(&mut q, ctx, &a, true, &mut tmp);
        ex.extend(tmp.iter().map(|&(n, c, leg)| (n, c, 0, leg)));
        ex.sort_unstable_by_key(|x| x.0);
        let exits: Vec<(u32, f32, u32)> = ex.iter().map(|x| (x.0, x.1, x.2)).collect();
        let mut sources = vec![];
        let mut arcs = vec![];
        for (i, e) in ends.iter().enumerate() {
            let Some(e) = e else { continue };
            sources.clear();
            attach(&mut q, ctx, e, false, &mut sources);
            let mut best = [f32::INFINITY];
            let mut direct_leg = NONE;
            if let Some((c, leg)) = direct(&mut q, ctx, e, &a) {
                best[0] = c;
                direct_leg = leg;
            }
            let mut end = (NONE, NONE);
            if !sources.is_empty() && !exits.is_empty() {
                let allow = cp.allowed_areas(&sources, &exits);
                ctx.many(g, cp, &sources, &exits, &mut best, &allow, None);
                end = ctx.many_end[0];
            }
            out[i] = summary(
                &q,
                ctx,
                best[0],
                end.0,
                ex.get(end.1 as usize).map(|x| x.3),
                direct_leg,
                &mut arcs,
            );
        }
    }
    Ok(out)
}

/// A locator resolved and, for a closed room (`approach`), moved to its nearest walkable
/// point outside, as `matrix` does.
fn end_of(g: &Graph, cp: &CompiledProfile, loc: &Loc) -> Result<End, String> {
    let mut e = g.resolve(loc, cp)?;
    let mut other = End::Nodes(vec![]);
    approach(g, cp, &mut e, loc, &mut other, &Loc::default())?;
    Ok(e)
}

/// The cheapest way between two ends that bypasses the graph: a walk inside one zone, or a
/// stretch of one edge. (cost, leg).
fn direct(q: &mut Q<'_>, ctx: &mut QueryCtx, a: &End, b: &End) -> Option<(f32, u32)> {
    let g = q.g;
    let mut best: Option<(f32, u32)> = None;
    for (pa, _) in a.parts() {
        for (pb, _) in b.parts() {
            let c = match (pa, pb) {
                (&End::Tri(t, p), &End::Tri(t2, p2))
                    if g.mesh.tri_zone[t as usize] == g.mesh.tri_zone[t2 as usize] =>
                {
                    let mut none = vec![];
                    q.attach_nodes(ctx, t, p, false, Some((t2, p2)), 0, &mut none)
                }
                (&End::Edge(fa, sa, _), &End::Edge(fb, sb, _)) if fa == fb => {
                    let fc = &g.facts[fa as usize];
                    let arc = if sb >= sa {
                        g.find_arc(fc.na, fa, false).map(|x| (x, sa, sb))
                    } else {
                        g.find_arc(fc.nb, fa, true).map(|x| (x, 1.0 - sa, 1.0 - sb))
                    };
                    arc.and_then(|(x, f0, f1)| {
                        let c = q.cp.arc_cost[x] * (f1 - f0) as f32;
                        c.is_finite().then(|| {
                            let leg = q.leg(Leg::Arc {
                                arc: x as u32,
                                f0,
                                f1,
                            });
                            (c, leg)
                        })
                    })
                }
                _ => None,
            };
            if let Some(c) = c.filter(|c| best.is_none_or(|b| c.0 < b.0)) {
                best = Some(c);
            }
        }
    }
    best
}

/// Duration and length of a target's path: the direct leg, or the source leg, the arcs from
/// the search tree and the exit leg.
fn summary(
    q: &Q<'_>,
    ctx: &QueryCtx,
    cost: f32,
    node: u32,
    exit_leg: Option<u32>,
    direct_leg: u32,
    arcs: &mut Vec<u32>,
) -> Option<RouteCost> {
    if !cost.is_finite() {
        return None;
    }
    let (g, cp) = (q.g, q.cp);
    let leg = |l: u32| -> (f64, f64) {
        match q.legs[l as usize] {
            Leg::Walk { time, len, .. } => (time as f64, len as f64),
            Leg::Arc { arc, f0, f1 } => {
                let fr = f1 - f0;
                let f = g.fact_of(arc as usize).0;
                (cp.arc_time[arc as usize] as f64 * fr, f.len3 as f64 * fr)
            }
            Leg::Stay => (0.0, 0.0),
        }
    };
    let (mut duration, mut length) = (0.0, 0.0);
    let mut add = |(d, l): (f64, f64)| {
        duration += d;
        length += l;
    };
    match exit_leg {
        Some(x) if node != NONE => {
            arcs.clear();
            let s = ctx.unwind(g, node, arcs);
            add(leg(ctx.src_leg[s as usize]));
            for &a in arcs.iter() {
                let f = g.fact_of(a as usize).0;
                add((cp.arc_time[a as usize] as f64, f.len3 as f64));
            }
            add(leg(x));
        }
        _ if direct_leg != NONE => add(leg(direct_leg)),
        _ => return None,
    }
    Some(RouteCost {
        cost: cost as f64,
        duration,
        length,
    })
}
