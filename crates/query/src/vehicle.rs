//! Request vehicle parameters: per-request height, weight, hazmat and
//! similar overrides of the profile's `vehicle` block.
//!
//! A compiled profile already excludes the edges its own vehicle may not
//! use and keeps their metrics aside ([`CompiledVehicleExclusions`]). A
//! request vehicle is merged over the profile's and resolved against the
//! dataset's restriction table into the set of otherwise traversable edges
//! it excludes. Equal sets reuse the engine as is; a superset only removes
//! edges (handled like an avoid set); any other set restores and removes
//! edge metrics, giving exactly the metric a profile compiled with that
//! vehicle would have.

use std::hash::{Hash, Hasher};

use anyhow::{Result, bail};
use netweevil_core::{
    CompiledProfileBundle, CompiledVehicleExclusions, TopologyBundle, VehicleParams,
};

/// How a request vehicle changes an engine's metric.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum VehicleChange {
    /// The vehicle excludes exactly the edges the metric already excludes.
    Unchanged,
    /// The vehicle excludes every edge the metric excludes plus these
    /// (sorted) edges.
    Excludes(Vec<u32>),
    /// The vehicle passes some excluded edges: `edges` is its complete
    /// exclusion set for `vehicle`, the merged parameters.
    Replaces {
        vehicle: VehicleParams,
        edges: Vec<u32>,
    },
}

/// Resolves `request` (merged over the profile's vehicle) against the
/// edges `metrics` excludes for its vehicle.
pub(crate) fn resolve_request_vehicle(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: Option<&VehicleParams>,
) -> Result<VehicleChange> {
    let Some(request) = request else {
        return Ok(VehicleChange::Unchanged);
    };
    request.validate().map_err(anyhow::Error::msg)?;
    let current = &metrics.vehicle;
    let vehicle = current.vehicle.unwrap_or_default().merged(request);
    let table = &topology.edge_layers.vehicle_restrictions;
    if table.is_empty() || current.vehicle == Some(vehicle) {
        return Ok(VehicleChange::Unchanged);
    }
    // Only edges the profile could otherwise traverse matter: the rest
    // carry no metric either way.
    let edges = table
        .excluded_edges(&table.excluded_restrictions(&vehicle, metrics.mode.access_bit()))
        .into_iter()
        .filter(|&edge| {
            current.position(edge as usize).is_some()
                || metrics.edge_metrics[edge as usize]
                    .generalized_cost
                    .is_some()
        })
        .collect::<Vec<_>>();
    if edges == current.edges {
        return Ok(VehicleChange::Unchanged);
    }
    if current
        .edges
        .iter()
        .all(|edge| edges.binary_search(edge).is_ok())
    {
        let added = edges
            .into_iter()
            .filter(|edge| current.edges.binary_search(edge).is_err())
            .collect();
        return Ok(VehicleChange::Excludes(added));
    }
    Ok(VehicleChange::Replaces { vehicle, edges })
}

/// `metrics` with the profile's vehicle exclusions replaced by `edges`
/// (from [`VehicleChange::Replaces`]): previously excluded edges regain
/// their metrics and component values, `edges` lose theirs. Acceleration
/// weights are dropped so the engine customizes them for the new metric.
pub(crate) fn apply_vehicle(
    metrics: &CompiledProfileBundle,
    vehicle: VehicleParams,
    edges: &[u32],
) -> CompiledProfileBundle {
    let mut derived = metrics.clone();
    restore_vehicle_exclusions(&mut derived);
    derived.vehicle.vehicle = Some(vehicle);
    exclude_vehicle_edges(&mut derived, edges);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    derived.vehicle.edges.hash(&mut hasher);
    derived.profile_hash = format!("{}+vehicle-{:016x}", metrics.profile_hash, hasher.finish());
    clear_acceleration_weights(&mut derived);
    derived
}

/// Moves the metrics and component values of `edges` (sorted) into a
/// fresh exclusion table of `metrics`, keeping its vehicle. Edges without
/// a metric are skipped: they are untraversable either way.
pub(crate) fn exclude_vehicle_edges(metrics: &mut CompiledProfileBundle, edges: &[u32]) {
    let mut exclusions = CompiledVehicleExclusions {
        vehicle: metrics.vehicle.vehicle,
        edges: Vec::with_capacity(edges.len()),
        travel_time_s: Vec::with_capacity(edges.len()),
        generalized_cost: Vec::with_capacity(edges.len()),
        component_values: vec![Vec::with_capacity(edges.len()); metrics.components.len()],
    };
    for &edge in edges {
        let metric = &mut metrics.edge_metrics[edge as usize];
        let (Some(travel_time_s), Some(generalized_cost)) =
            (metric.travel_time_s, metric.generalized_cost)
        else {
            continue;
        };
        metric.travel_time_s = None;
        metric.generalized_cost = None;
        exclusions.edges.push(edge);
        exclusions.travel_time_s.push(travel_time_s);
        exclusions.generalized_cost.push(generalized_cost);
        for (values, component) in exclusions
            .component_values
            .iter_mut()
            .zip(&mut metrics.components)
        {
            values.push(std::mem::take(&mut component.edge_values[edge as usize]));
        }
    }
    metrics.vehicle = exclusions;
}

/// Moves every vehicle exclusion of `metrics` back into its edge metrics
/// and component columns, leaving an empty exclusion table for the same
/// vehicle. Traffic engines derive their speeds from this unrestricted
/// metric and exclude the edges again afterwards.
pub(crate) fn restore_vehicle_exclusions(metrics: &mut CompiledProfileBundle) {
    let exclusions = std::mem::take(&mut metrics.vehicle);
    for (position, &edge) in exclusions.edges.iter().enumerate() {
        let metric = &mut metrics.edge_metrics[edge as usize];
        metric.travel_time_s = Some(exclusions.travel_time_s[position]);
        metric.generalized_cost = Some(exclusions.generalized_cost[position]);
        for (component, values) in metrics
            .components
            .iter_mut()
            .zip(&exclusions.component_values)
        {
            component.edge_values[edge as usize] = values[position];
        }
    }
    metrics.vehicle.vehicle = exclusions.vehicle;
}

pub(crate) fn clear_acceleration_weights(metrics: &mut CompiledProfileBundle) {
    if let Some(acceleration) = metrics.acceleration.as_mut() {
        for weights in [
            &mut acceleration.upward_weight,
            &mut acceleration.downward_weight,
            &mut acceleration.time_upward_weight,
            &mut acceleration.time_downward_weight,
            &mut acceleration.distance_upward_weight,
            &mut acceleration.distance_downward_weight,
        ] {
            *weights = Vec::new();
        }
    }
}

/// The one vehicle of a request whose parts (matrix sides, accessibility
/// categories) may each name one; parts that name one must agree.
pub(crate) fn single_request_vehicle<'a>(
    vehicles: impl IntoIterator<Item = Option<&'a VehicleParams>>,
) -> Result<Option<&'a VehicleParams>> {
    let mut chosen = None;
    for vehicle in vehicles.into_iter().flatten() {
        match chosen {
            None => chosen = Some(vehicle),
            Some(existing) if existing == vehicle => {}
            Some(_) => bail!("every part of a request must use the same vehicle parameters"),
        }
    }
    Ok(chosen)
}

/// One-shot entry points build a plain graph per call; request vehicles
/// need the cached engines of [`crate::PreparedRoutingEngine`].
pub(crate) fn ensure_no_vehicle<'a>(
    vehicles: impl IntoIterator<Item = Option<&'a VehicleParams>>,
) -> Result<()> {
    if vehicles.into_iter().any(|vehicle| vehicle.is_some()) {
        bail!("request vehicle parameters require a PreparedRoutingEngine");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
