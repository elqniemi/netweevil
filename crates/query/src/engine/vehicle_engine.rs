//! Request vehicles on a prepared engine. A vehicle that excludes the
//! edges the engine's metric already excludes runs on the engine itself; one
//! that only excludes more edges joins the request's avoid set (and so its
//! cached engines and single-route fast path); any other vehicle gets a
//! cached engine derived for its exclusion set.

use std::sync::Arc;

use anyhow::Result;
use netweevil_core::VehicleParams;

use crate::vehicle::{VehicleChange, apply_vehicle, resolve_request_vehicle};
use crate::*;

/// How a request's avoid set and vehicle change the problem an engine
/// solves.
pub(crate) enum RequestAdjustment {
    Unchanged,
    /// Exclusions and penalties on this engine's metric.
    Avoid(ResolvedAvoid),
    /// A derived engine for the request's vehicle; the request (with its
    /// avoid set, if any) runs on that engine.
    Engine(Arc<PreparedRoutingEngine>),
}

impl PreparedRoutingEngine {
    pub(crate) fn request_adjustment(
        &self,
        avoid: &AvoidOptions,
        vehicle: Option<&VehicleParams>,
    ) -> Result<RequestAdjustment> {
        let resolved_avoid = |extra: &[u32]| -> Result<Option<ResolvedAvoid>> {
            let resolved = self.resolve_request_avoid(avoid)?;
            if extra.is_empty() {
                return Ok(resolved);
            }
            Ok(Some(resolved.unwrap_or_default().excluding(extra)))
        };
        let change = resolve_request_vehicle(self.topology(), self.metrics(), vehicle)?;
        Ok(match change {
            VehicleChange::Unchanged => match resolved_avoid(&[])? {
                Some(resolved) => RequestAdjustment::Avoid(resolved),
                None => RequestAdjustment::Unchanged,
            },
            VehicleChange::Excludes(edges) => match resolved_avoid(&edges)? {
                Some(resolved) => RequestAdjustment::Avoid(resolved),
                None => RequestAdjustment::Unchanged,
            },
            VehicleChange::Replaces { vehicle, edges } => {
                RequestAdjustment::Engine(self.vehicle_engine(vehicle, edges)?)
            }
        })
    }

    /// The engine to run a request with `avoid` and `vehicle` on instead of
    /// this one, or `None` when this engine serves it as is. Callers pass
    /// the unchanged request on: the target resolves both again, finds them
    /// applied, and a vehicle engine applies the avoid set in turn.
    pub(crate) fn request_target(
        &self,
        avoid: &AvoidOptions,
        vehicle: Option<&VehicleParams>,
        mode: EngineMode,
    ) -> Result<Option<Arc<PreparedRoutingEngine>>> {
        Ok(match self.request_adjustment(avoid, vehicle)? {
            RequestAdjustment::Unchanged => None,
            RequestAdjustment::Avoid(resolved) => Some(self.avoid_engine(&resolved, mode)?),
            RequestAdjustment::Engine(engine) => Some(engine),
        })
    }

    /// [`Self::request_target`] for a request's runtime options.
    pub(crate) fn temporal_target(
        &self,
        temporal: &TemporalRequestOptions,
        mode: EngineMode,
    ) -> Result<Option<Arc<PreparedRoutingEngine>>> {
        self.request_target(&temporal.avoid, temporal.vehicle.as_ref(), mode)
    }

    /// The cached engine whose metric excludes exactly `edges` for
    /// `vehicle`: the metric a profile compiled with that vehicle has.
    fn vehicle_engine(
        &self,
        vehicle: VehicleParams,
        edges: Vec<u32>,
    ) -> Result<Arc<PreparedRoutingEngine>> {
        let key = edges.clone();
        self.vehicle_engines.get_or_build(key, || {
            let started = std::time::Instant::now();
            let metrics = apply_vehicle(self.metrics(), vehicle, &edges);
            let engine = self.derive_engine(Arc::new(metrics), true)?;
            tracing::debug!(
                excluded_edges = edges.len(),
                prepare_ms = started.elapsed().as_secs_f64() * 1000.0,
                "request vehicle engine prepared"
            );
            Ok(engine)
        })
    }
}

#[cfg(test)]
mod tests;
