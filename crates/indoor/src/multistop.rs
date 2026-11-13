//! Multi-stop routes: from -> via stops -> to, in the given order or, with
//! `optimise`, in the order that costs least. The order comes from a cost matrix of one
//! one-to-many search per point (`route::matrix`) and `tsp.rs`; then each leg is routed in
//! full, exactly like a single route between its two stops.

use crate::closure::{ClosureHit, Overlay};
use crate::cost::CompiledProfile;
use crate::graph::Graph;
use crate::locate::Loc;
use crate::route::{Extra, Route, matrix, route_with};
use crate::search::QueryCtx;
use crate::tsp::best_order;
use serde::Serialize;
use std::time::Instant;

/// Most via stops in one query.
pub const MAX_VIA: usize = 50;

/// A multi-stop route: one leg per stop (each a whole `Route`), the totals and the order.
#[derive(Serialize, Debug, Clone, Default)]
pub struct Trip {
    pub cost: f64,
    pub duration: f64,
    pub length: f64,
    pub climb: f64,
    pub descent: f64,
    /// The via stops in visiting order (indices into the request's `via`).
    pub order: Vec<usize>,
    /// How the order was found with `optimise`: `exact` or `heuristic` (absent: as given).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optimised: Option<&'static str>,
    /// Leg k goes from stop k to stop k + 1 of from, the via stops in `order`, to.
    pub legs: Vec<Route>,
    pub warnings: Vec<String>,
    /// The closures any leg respected (each leg also lists its own).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub closures: Vec<ClosureHit>,
    pub stats: TripStats,
}

#[derive(Serialize, Debug, Clone, Default)]
pub struct TripStats {
    pub expanded: u32,
    pub micros: f64,
    /// Time of the cost matrix and the ordering (0 without `optimise`).
    pub matrix_micros: f64,
}

/// Route `from` -> `via` -> `to` (None: end at the last stop visited). `optimise`: visit the
/// via stops in the order that costs least, with `from` and `to` fixed.
pub fn route_stops(
    g: &Graph,
    cp: &CompiledProfile,
    ctx: &mut QueryCtx,
    from: &Loc,
    via: &[Loc],
    to: Option<&Loc>,
    optimise: bool,
    ov: Option<&Overlay>,
) -> Result<Trip, String> {
    if via.len() > MAX_VIA {
        return Err(format!(
            "too many via stops: {} (at most {MAX_VIA})",
            via.len()
        ));
    }
    let t0 = Instant::now();
    let n = via.len();
    let mut trip = Trip {
        order: (0..n).collect(),
        ..Trip::default()
    };
    let mut names: Vec<String> = vec!["from".into()];
    names.extend((0..n).map(|i| format!("via[{i}]")));
    names.push("to".into());
    if optimise && n > 1 {
        let mut locs = vec![from.clone()];
        locs.extend(via.iter().cloned());
        locs.extend(to.cloned());
        let (m, expanded) = matrix(g, cp, ctx, &locs, &names, to.is_some(), ov)?;
        // A stop the start cannot reach is reached by no order.
        if let Some(s) = (0..n).find(|&s| !m.at(0, s + 1).is_finite()) {
            return Err(format!("via[{s}]: no route"));
        }
        let (order, how) = best_order(&m, n, to.is_some());
        trip.order = order;
        trip.optimised = Some(how);
        trip.stats.expanded += expanded;
        trip.stats.matrix_micros = t0.elapsed().as_secs_f64() * 1e6;
    } else if optimise {
        trip.optimised = Some("exact");
    }
    // The points in visiting order, with their names for errors.
    let mut stops: Vec<(&Loc, &str)> = vec![(from, "from")];
    stops.extend(trip.order.iter().map(|&i| (&via[i], names[i + 1].as_str())));
    if let Some(t) = to {
        stops.push((t, "to"));
    }
    let extra = Extra {
        closures: ov,
        ..Extra::default()
    };
    // A category stop is the POI the leg into it chose: the next leg starts there.
    let mut chosen: Option<Loc> = None;
    for w in stops.windows(2) {
        let ((a, an), (b, bn)) = (w[0], w[1]);
        let a = chosen.take().unwrap_or_else(|| a.clone());
        let leg =
            route_with(g, cp, ctx, &a, b, &extra).map_err(|e| format!("{an} -> {bn}: {e}"))?;
        if b.is_poi() {
            chosen = leg.to_poi.as_ref().map(|p| Loc {
                poi: Some(p.id.clone()),
                ..Loc::default()
            });
        }
        add(&mut trip, leg);
    }
    trip.stats.micros = t0.elapsed().as_secs_f64() * 1e6;
    Ok(trip)
}

/// Leg totals into the trip; warnings and closures once each.
fn add(trip: &mut Trip, leg: Route) {
    trip.cost += leg.cost;
    trip.duration += leg.duration;
    trip.length += leg.length;
    trip.climb += leg.climb;
    trip.descent += leg.descent;
    trip.stats.expanded += leg.stats.expanded;
    for w in &leg.warnings {
        if !trip.warnings.contains(w) {
            trip.warnings.push(w.clone());
        }
    }
    for c in &leg.closures {
        if !trip
            .closures
            .iter()
            .any(|h| h.id == c.id && h.effect == c.effect)
        {
            trip.closures.push(c.clone());
        }
    }
    trip.legs.push(leg);
}
