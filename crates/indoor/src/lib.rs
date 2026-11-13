//! Routing on walkable surfaces and on node/edge networks, indoors and outdoors, across levels.
//!
//! # Model
//!
//! An [`Input`] document describes a site in projected metres. **Surfaces** are walkable
//! polygons (rooms, corridors, plazas) on a **level**. Each level is triangulated into a
//! navigation mesh, and a route walks across it at any angle: a funnel finds the taut path
//! through the triangles and a smoothing pass keeps the drawn line away from walls. Surfaces
//! with `walls: true` are closed at their boundary. **Portals** (doors, gates, crossings) are
//! the openings in those walls, and **barriers** are lines that cut a surface (fences, kerbs)
//! that only portals pass. **Edges** are lines of a network (footways, roads, single indoor
//! lines) that join at shared vertices or at **nodes**; an edge end near a surface attaches to
//! it. **Verticals** (lifts, stairs, escalators, ramps) join stops on different levels.
//! **POIs** are destinations with one or more access points (a point, a node, a portal or a
//! space).
//!
//! [`compile`] turns an input into one [`Graph`]: a CSR graph of portal nodes whose arcs are
//! walks inside one mesh zone, edges, portal crossings and vertical hops. Input with only
//! edges routes along the edges, input with only surfaces routes on the mesh, and mixed input
//! does both. Graphs are saved and loaded with [`save_graph`] and [`load_graph`].
//!
//! # Profiles and modes
//!
//! A [`Profile`] sets the speed, factors per class, tag and kind (`null` forbids), penalties,
//! incline costs, width and step-free rules, and how walks are drawn. Built-ins are `walk`,
//! `wheelchair`, `fastest`, `bike` and `evacuation`. [`Router::profile`] compiles a profile
//! against the graph once and caches it. The profile's [`RouteMode`] limits the element kinds
//! a route may use: `mixed` (default) uses all of them, `edges_only` never walks across a
//! surface, and `surfaces_only` never travels along an edge. Access groups restrict elements
//! to queries that are granted one of their groups ([`Router::profile_access`]).
//!
//! # Queries
//!
//! A [`Router`] holds the graph and the compiled profile cache and can be shared across
//! threads; each thread uses its own [`QueryCtx`]. A [`Loc`] locates a query end by point and
//! level, space, node, portal, POI or POI category. [`Router::route`] returns a [`Route`] with
//! drawn coordinates and one [`Segment`] per walk, edge, portal or vertical. [`Router::costs_to`]
//! and [`Router::costs_from`] give costs between one point and many without drawing,
//! [`Router::route_stops`] routes through stops, [`Router::reach`] checks connectivity, and
//! [`Graph::elements`] lists portals, nodes, verticals and POIs with their graph positions.
//!
//! Functions return `Result<_, String>`; [`IntoAnyhow::anyhow`] converts such a result.

pub mod access;
pub mod closure;
pub mod cost;
pub mod elements;
pub mod funnel;
pub mod geom;
pub mod graph;
pub mod input;
pub mod io;
pub mod locate;
pub mod mesh;
pub mod multistop;
pub mod profile;
pub mod reach;
pub mod route;
pub mod search;
pub mod smooth;
pub mod tsp;

pub use closure::{Closure, Overlay};
pub use cost::CompiledProfile;
pub use elements::{Element, ElementKind, ElementPoint, Level};
pub use graph::{CompileReport, Graph, Warning, compile};
pub use input::Input;
pub use locate::Loc;
pub use multistop::Trip;
pub use profile::{Profile, RouteMode};
pub use reach::Reach;
pub use route::{Located, Route, RouteCost, Segment};
pub use search::QueryCtx;

use anyhow::Context;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

/// Compiled profiles kept per router (least recently used are dropped).
pub const PROFILE_CACHE: usize = 32;
/// Compiled closure lists kept per router (`Router::overlay`).
pub const OVERLAY_CACHE: usize = 8;

/// Conversion of the crate's `Result<T, String>` into `anyhow::Result<T>`.
pub trait IntoAnyhow<T> {
    fn anyhow(self) -> anyhow::Result<T>;
}

impl<T> IntoAnyhow<T> for Result<T, String> {
    fn anyhow(self) -> anyhow::Result<T> {
        self.map_err(anyhow::Error::msg)
    }
}

/// Parse an input document (JSON).
pub fn parse_input(json: &str) -> anyhow::Result<Input> {
    Input::from_json(json).anyhow()
}

/// Read a compiled graph file.
pub fn load_graph(path: &Path) -> anyhow::Result<Graph> {
    let p = path.to_str().context("graph path is not UTF-8")?;
    io::load(p).anyhow()
}

/// Write a compiled graph file.
pub fn save_graph(graph: &Graph, path: &Path) -> anyhow::Result<()> {
    let p = path.to_str().context("graph path is not UTF-8")?;
    io::save(graph, p).anyhow()
}

/// A profile from a built-in name, a JSON object or a path to a JSON file, validated.
pub fn resolve_profile(spec: &str) -> anyhow::Result<Profile> {
    Profile::resolve(spec).anyhow()
}

/// Profile cache entry: (profile JSON, granted access mask, costs).
type CachedProfile = (String, u64, Arc<CompiledProfile>);

/// A graph plus a per-profile cost cache. Share it across threads; give each thread its own
/// `QueryCtx` (`Router::ctx`).
pub struct Router {
    pub graph: Graph,
    /// Most recently used first. The unrestricted base of a profile is keyed with
    /// `access::ALL`.
    cache: Mutex<Vec<CachedProfile>>,
    /// Closure list JSON -> overlay; most recently used first.
    overlays: Mutex<Vec<(String, Arc<Overlay>)>>,
}

impl Router {
    pub fn new(graph: Graph) -> Router {
        Router {
            graph,
            cache: Mutex::new(vec![]),
            overlays: Mutex::new(vec![]),
        }
    }

    pub fn ctx(&self) -> QueryCtx {
        QueryCtx::new(&self.graph)
    }

    /// The levels of the graph with their z, in input order.
    pub fn levels(&self) -> Vec<Level> {
        self.graph.level_list()
    }

    /// Every element of `kind` with its graph positions (`Graph::elements`).
    pub fn elements(&self, kind: ElementKind) -> Vec<Element> {
        self.graph.elements(kind)
    }

    /// Costs for `p` for an anonymous query (no access groups granted: restricted elements
    /// and POIs are unusable). Same as `profile_access(p, 0)`; on a graph without
    /// `access_groups` this is the plain profile.
    pub fn profile(&self, p: &Profile) -> Arc<CompiledProfile> {
        self.profile_access(p, 0)
    }

    /// Costs for `p` for a query granted the access groups `granted`
    /// (`Graph::access_mask`; `access::ALL` = everything). Computed once per distinct
    /// (profile, mask) and cached (LRU, `PROFILE_CACHE`); a restricted variant is derived from
    /// the cached unrestricted profile (`CompiledProfile::restrict`), and is the very same
    /// `Arc` when the mask denies nothing.
    pub fn profile_access(&self, p: &Profile, granted: u64) -> Arc<CompiledProfile> {
        let key = serde_json::to_string(p).unwrap_or_default();
        if let Some(cp) = self.cached(&key, granted) {
            return cp;
        }
        let base = match self.cached(&key, access::ALL) {
            Some(b) => b,
            None => {
                let b = Arc::new(CompiledProfile::new(&self.graph, p));
                self.store(&key, access::ALL, b.clone());
                b
            }
        };
        if granted == access::ALL {
            return base;
        }
        let cp = match base.restrict(&self.graph, granted) {
            Some(r) => Arc::new(r),
            None => base,
        };
        self.store(&key, granted, cp.clone());
        cp
    }

    fn cached(&self, key: &str, granted: u64) -> Option<Arc<CompiledProfile>> {
        let mut c = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let k = c.iter().position(|e| e.0 == key && e.1 == granted)?;
        let e = c.remove(k);
        let cp = e.2.clone();
        c.insert(0, e);
        Some(cp)
    }

    fn store(&self, key: &str, granted: u64, cp: Arc<CompiledProfile>) {
        let mut c = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(k) = c.iter().position(|e| e.0 == key && e.1 == granted) {
            c.remove(k);
        }
        c.insert(0, (key.to_string(), granted, cp));
        c.truncate(PROFILE_CACHE);
    }

    /// The overlay of a closure list (`closure.rs`), compiled once per distinct list and
    /// cached (LRU, `OVERLAY_CACHE`), as the same live closures tend to come with every query.
    pub fn overlay(&self, list: &[Closure]) -> Arc<Overlay> {
        let key = serde_json::to_string(list).unwrap_or_default();
        let mut c = self.overlays.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(k) = c.iter().position(|e| e.0 == key) {
            let e = c.remove(k);
            let ov = e.1.clone();
            c.insert(0, e);
            return ov;
        }
        drop(c);
        let ov = Arc::new(Overlay::new(&self.graph, list));
        let mut c = self.overlays.lock().unwrap_or_else(PoisonError::into_inner);
        c.insert(0, (key, ov.clone()));
        c.truncate(OVERLAY_CACHE);
        ov
    }

    /// `route` with closures in force: closed elements and areas are not used, waits are
    /// added; the route lists the closures it respected (`Route::closures`).
    pub fn route_closed(
        &self,
        ctx: &mut QueryCtx,
        p: &CompiledProfile,
        from: &Loc,
        to: &Loc,
        ov: &Overlay,
    ) -> Result<Route, String> {
        route::route_closed(&self.graph, p, ctx, from, to, ov)
    }

    /// The cheapest route from `from` to `to` for profile `p`, with drawn geometry.
    pub fn route(
        &self,
        ctx: &mut QueryCtx,
        p: &CompiledProfile,
        from: &Loc,
        to: &Loc,
    ) -> Result<Route, String> {
        route::route(&self.graph, p, ctx, from, to)
    }

    /// Costs from `from` to each of `targets`, in one one-to-many search. One-way elements
    /// are followed in their direction. A target that cannot be located or reached is None;
    /// the error is for a `from` that cannot be located. See [`RouteCost`].
    pub fn costs_to(
        &self,
        ctx: &mut QueryCtx,
        p: &CompiledProfile,
        from: &Loc,
        targets: &[Loc],
    ) -> Result<Vec<Option<RouteCost>>, String> {
        route::costs(&self.graph, p, ctx, from, targets, true)
    }

    /// Costs from each of `sources` to `to` (one search per source). One-way elements are
    /// followed in their direction. A source that cannot be located or has no path is None;
    /// the error is for a `to` that cannot be located. See [`RouteCost`].
    pub fn costs_from(
        &self,
        ctx: &mut QueryCtx,
        p: &CompiledProfile,
        sources: &[Loc],
        to: &Loc,
    ) -> Result<Vec<Option<RouteCost>>, String> {
        route::costs(&self.graph, p, ctx, to, sources, false)
    }

    /// A multi-stop route `from` -> `via` -> `to` (None: end at the last stop), in the given
    /// order or with `optimise` the cheapest one: `multistop.rs`. `ov`: closures in force.
    pub fn route_stops(
        &self,
        ctx: &mut QueryCtx,
        p: &CompiledProfile,
        from: &Loc,
        via: &[Loc],
        to: Option<&Loc>,
        optimise: bool,
        ov: Option<&Overlay>,
    ) -> Result<Trip, String> {
        multistop::route_stops(&self.graph, p, ctx, from, via, to, optimise, ov)
    }

    /// Where `loc` snaps for profile `p` (no search).
    pub fn locate(&self, p: &CompiledProfile, loc: &Loc) -> Result<Located, String> {
        route::locate(&self.graph, p, loc)
    }

    /// Reachability from `from` (node / portal / space id, None = largest component): see
    /// `reach::reach`. Id lists capped at `cap`.
    pub fn reach(
        &self,
        p: &CompiledProfile,
        from: Option<&str>,
        cap: usize,
    ) -> Result<Reach, String> {
        reach::reach(&self.graph, p, from, cap)
    }
}
