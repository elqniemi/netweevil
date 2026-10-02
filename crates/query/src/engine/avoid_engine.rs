//! Avoid sets on a prepared engine: a small per-engine cache of engines whose
//! metric excludes or penalizes the avoided edges (CCH re-customized), plus
//! an exact fast path for single routes that never touch the avoid set.

use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::edge_spatial_index::EdgeSpatialIndex;
use crate::*;

/// Engines prepared for recent avoid sets, most recently used last. Each
/// slot is filled once; concurrent requests for the same set wait for the
/// first build instead of customizing twice.
#[derive(Default)]
pub(crate) struct AvoidEngineCache {
    entries: Mutex<Vec<(AvoidEngineKey, AvoidEngineSlot)>>,
}

type AvoidEngineSlot = Arc<Mutex<Option<Arc<PreparedRoutingEngine>>>>;

/// Resolved edges with factor bits, plus whether the engine needs the
/// `IgnoreMultiEdgeRestrictions` graph (skipped otherwise: building it is a
/// large share of the preparation time on restriction-rich networks).
type AvoidEngineKey = (Vec<(u32, u64)>, bool);

const AVOID_ENGINE_CACHE_CAPACITY: usize = 4;

/// Avoid sets of at most one edge in this many re-customize the base
/// engine's hierarchy; larger ones touch enough of it that a full
/// customization (parallel) is faster.
const RECUSTOMIZE_SHARE: usize = 200;

impl PreparedRoutingEngine {
    /// The engine that serves requests carrying `avoid`, or `None` when the
    /// set does not change the problem (empty, matching no routable edge, or
    /// already applied to this engine).
    pub(crate) fn avoid_target(
        &self,
        avoid: &AvoidOptions,
        mode: EngineMode,
    ) -> Result<Option<Arc<PreparedRoutingEngine>>> {
        match self.resolve_request_avoid(avoid)? {
            Some(resolved) => self.avoid_engine(&resolved, mode).map(Some),
            None => Ok(None),
        }
    }

    pub(crate) fn resolve_request_avoid(
        &self,
        avoid: &AvoidOptions,
    ) -> Result<Option<ResolvedAvoid>> {
        if avoid.is_empty() || self.avoid_applied {
            return Ok(None);
        }
        let resolved = resolve_avoid(
            self.topology.as_ref(),
            self.metrics.as_ref(),
            self.avoid_spatial_index(),
            avoid,
        )?;
        Ok((!resolved.is_empty()).then_some(resolved))
    }

    /// Routable edges for avoid resolution. The default graph's snapping
    /// index omits temporal materialized directions, which scenario requests
    /// can enable, so topologies with such edges get their own index.
    fn avoid_spatial_index(&self) -> &EdgeSpatialIndex {
        self.avoid_index
            .get_or_init(|| {
                let topology = self.topology.as_ref();
                let has_materialized = (0..topology.edge_count()).any(|edge| {
                    topology.routing_edge(edge).flags
                        & netweevil_core::EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION
                        != 0
                });
                has_materialized.then(|| {
                    let costs = self
                        .metrics
                        .edge_metrics
                        .iter()
                        .map(|metric| metric.generalized_cost.unwrap_or(f64::INFINITY))
                        .collect::<Vec<_>>();
                    EdgeSpatialIndex::build(topology, &costs)
                })
            })
            .as_ref()
            .unwrap_or(&self.default_routing_graph.edge_spatial_index)
    }

    pub(crate) fn avoid_engine(
        &self,
        resolved: &ResolvedAvoid,
        mode: EngineMode,
    ) -> Result<Arc<PreparedRoutingEngine>> {
        let with_ignore_mode_graph = mode == EngineMode::IgnoreMultiEdgeRestrictions
            && self.ignore_multi_edge_restrictions_graph.is_some();
        let key = (
            resolved
                .edges
                .iter()
                .map(|&(edge, factor)| (edge, factor.to_bits()))
                .collect::<Vec<_>>(),
            with_ignore_mode_graph,
        );
        let slot = {
            let mut entries = self
                .avoid_engines
                .entries
                .lock()
                .expect("avoid engine cache poisoned");
            let slot = match entries.iter().position(|(known, _)| *known == key) {
                Some(position) => entries.remove(position).1,
                None => {
                    if entries.len() >= AVOID_ENGINE_CACHE_CAPACITY {
                        entries.remove(0);
                    }
                    AvoidEngineSlot::default()
                }
            };
            entries.push((key, slot.clone()));
            slot
        };
        let mut slot = slot.lock().expect("avoid engine slot poisoned");
        if let Some(engine) = slot.as_ref() {
            return Ok(engine.clone());
        }
        let started = std::time::Instant::now();
        let metrics = Arc::new(resolved.apply(self.metrics.as_ref()));
        // Avoiding only raises or removes the costs of the avoided edges, so
        // a small avoid set re-customizes this engine's hierarchy instead of
        // customizing it from scratch.
        let mut ranked = (resolved.edges.len() <= self.topology.edge_count() / RECUSTOMIZE_SHARE)
            .then(|| {
                let changed = resolved
                    .edges
                    .iter()
                    .map(|&(edge, _)| edge)
                    .collect::<Vec<_>>();
                recustomize_changed_edges(
                    self.topology.as_ref(),
                    metrics.as_ref(),
                    &self.default_routing_graph,
                    &changed,
                )
            })
            .flatten();
        // Closing edges only removes adjacency, so a re-customized engine
        // filters this engine's graph instead of building one.
        let filtered = ranked.is_some().then(|| {
            self.default_routing_graph.with_closed_edges(
                self.topology.as_ref(),
                self.metrics.as_ref(),
                &metrics,
                &mut ranked,
            )
        });
        let mut engine = match filtered.flatten() {
            Some(graph) => {
                validate_execution_inputs(self.topology.as_ref(), metrics.as_ref())?;
                PreparedRoutingEngine::assemble(
                    self.topology.clone(),
                    metrics,
                    graph?,
                    true,
                    with_ignore_mode_graph,
                )
            }
            None => PreparedRoutingEngine::build(
                self.topology.clone(),
                metrics,
                self.dataset_acceleration(),
                with_ignore_mode_graph,
                ranked.map(CchWeights::Ranked),
            )?,
        };
        engine.avoid_applied = true;
        tracing::debug!(
            edges = resolved.edges.len(),
            prepare_ms = started.elapsed().as_secs_f64() * 1000.0,
            "avoid engine prepared"
        );
        let engine = Arc::new(engine);
        *slot = Some(engine.clone());
        Ok(engine)
    }

    /// Exact single-route fast path: route on this engine between the
    /// candidates the avoid engine would snap to, and accept the result when
    /// it provably equals the avoid engine's. Removing or penalizing edges
    /// never makes a path cheaper, so a best path that touches no avoided
    /// edge stays optimal once they are avoided. The candidate-pair rule of
    /// [`route_between_candidates`] (first routable physical attachment wins)
    /// is preserved when every pair it may try before the chosen one shares
    /// the chosen pair's attachment. Returns `None` when the fast path does
    /// not apply.
    pub(crate) fn try_route_avoiding(
        &self,
        request: &RouteRequest,
        resolved: &ResolvedAvoid,
        edge_names: Option<&[String]>,
        mode: EngineMode,
    ) -> Result<Option<RouteResult>> {
        if has_failure_modes(&request.fallback)
            || auto_relaxation_requested(&request.fallback)
            || request.temporal.requires_exact_labels()
            || request.alternatives.max_routes > 1
            || request.connectivity.disconnected != DisconnectedNetworkMode::Strict
        {
            return Ok(None);
        }
        let (routing_graph, _) = self.routing_graph_for_mode(mode);
        let topology = self.topology.as_ref();
        let search_snap = SnapOptions {
            max_distance_m: request
                .connectivity
                .max_hop_distance_m
                .unwrap_or(request.snap.max_distance_m)
                .max(request.snap.max_distance_m),
            ..request.snap.clone()
        };
        let excluded = |edge: usize| resolved.excludes(edge);
        let (Ok(origins), Ok(destinations)) = (
            snap_candidates_excluding(
                topology,
                routing_graph,
                &request.origin,
                &search_snap,
                true,
                &excluded,
            ),
            snap_candidates_excluding(
                topology,
                routing_graph,
                &request.destination,
                &search_snap,
                false,
                &excluded,
            ),
        ) else {
            return Ok(None);
        };
        let Ok((origin, destination, path, _)) = route_between_candidates(
            topology,
            self.metrics.as_ref(),
            routing_graph,
            request.snap.max_distance_m,
            &request.connectivity,
            &request.fallback,
            &origins,
            &destinations,
        ) else {
            return Ok(None);
        };
        if path.edge_indexes.iter().any(|&edge| resolved.affects(edge))
            || !chosen_pair_leads_its_attachment(
                request.snap.max_distance_m,
                &request.connectivity,
                &origins,
                &destinations,
                &origin,
                &destination,
            )
        {
            return Ok(None);
        }
        execute_route_with_candidates(
            topology,
            self.metrics.as_ref(),
            routing_graph,
            &request.route_id,
            request.snap.max_distance_m,
            &request.connectivity,
            &request.fallback,
            &request.returns,
            &request.alternatives,
            &origins,
            &destinations,
            edge_names,
        )
        .map(Some)
    }
}

/// Whether every candidate pair that [`route_between_candidates`] would try
/// before the chosen pair has the chosen pair's physical attachment at both
/// ends. Then no other attachment can win once avoided edges disappear.
fn chosen_pair_leads_its_attachment(
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    origins: &[SnappedPoint],
    destinations: &[SnappedPoint],
    chosen_origin: &SnappedPoint,
    chosen_destination: &SnappedPoint,
) -> bool {
    let chosen = (
        snap_cache_key(chosen_origin),
        snap_cache_key(chosen_destination),
    );
    for origin in origins {
        for destination in destinations {
            if (snap_cache_key(origin), snap_cache_key(destination)) == chosen {
                return true;
            }
            let tried = !same_edge_reverse_pair(origin, destination)
                && hop_info_for_pair(origin, destination, snap_max_distance_m, connectivity)
                    .is_some();
            if tried
                && !(same_physical_attachment(origin, chosen_origin)
                    && same_physical_attachment(destination, chosen_destination))
            {
                return false;
            }
        }
    }
    false
}
