use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::{CacheBundleId, TemporalRuleSet, TravelMode, VehicleParams};

/// Schema version of [`CompiledProfileBundle`]. Readers accept this value
/// only, so bumping it also makes cached bundles recompile; bump it when
/// compilation semantics change.
pub const COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION: u32 = 10;

/// Schema version of [`CompiledAcceleration`] customization data.
pub const COMPILED_ACCELERATION_SCHEMA_VERSION: u32 = 4;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct CompiledTurnCostConfig {
    pub left_penalty_s: f64,
    pub right_penalty_s: f64,
    pub uturn_penalty_s: f64,
    pub traffic_signal_penalty_s: f64,
    pub roundabout_entry_penalty_s: f64,
    pub cost_time_weight: f64,
    /// Generalized-cost penalty for entering an edge whose access is
    /// destination-only for the profile's mode from one that is not.
    pub destination_only_penalty_s: f64,
}

/// Metrics of one edge, at the edge's index in
/// [`CompiledProfileBundle::edge_metrics`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledEdgeMetric {
    pub travel_time_s: Option<f64>,
    pub generalized_cost: Option<f64>,
}

/// One named, edge-aligned accounting column emitted by profile
/// compilation. Values are stored separately from the scalar objective so a
/// shortest path can report its complete cost vector without changing the
/// fast static search representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledCostComponent {
    pub name: String,
    /// Contribution of this component to generalized cost.
    pub weight: f64,
    /// Per-edge values aligned with [`CompiledProfileBundle::edge_metrics`].
    /// Shared, so profiles with equal columns can hold one copy.
    pub edge_values: std::sync::Arc<[f32]>,
    /// Components based on `travel_time` scale with a temporal speed factor
    /// (but not with waiting before an edge opens).
    pub scales_with_travel_time: bool,
    /// Optional continuous temporal overlay multiplied into the value.
    pub overlay_name: Option<String>,
    /// When true, multiply by `1 - overlay` instead of `overlay`.
    pub invert_overlay: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledTemporalProfile {
    pub allow_wait: bool,
    pub max_wait_s: f64,
    /// Dataset edge schedules resolved for this profile.
    pub rules: CompiledTemporalRules,
}

impl Default for CompiledTemporalProfile {
    fn default() -> Self {
        Self {
            allow_wait: false,
            max_wait_s: default_max_wait_s(),
            rules: CompiledTemporalRules::default(),
        }
    }
}

/// Per-profile replacements for dataset edge schedules that carry
/// posted-speed rules ([`crate::TemporalEffect::MaxSpeed`]): rules of other
/// modes are dropped and posted speeds become the profile's speed factors.
/// Other edges use their dataset rule set. Sparse, sorted by edge index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CompiledTemporalRules {
    pub rule_sets: Vec<TemporalRuleSet>,
    pub edges: Vec<u32>,
    pub edge_rule_sets: Vec<u32>,
}

impl CompiledTemporalRules {
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    pub fn for_edge(&self, edge_index: usize) -> Option<&TemporalRuleSet> {
        let position = self.edges.binary_search(&(edge_index as u32)).ok()?;
        self.rule_sets
            .get(*self.edge_rule_sets.get(position)? as usize)
    }

    pub fn validate(&self, edge_count: usize) -> Result<(), String> {
        if self.edges.len() != self.edge_rule_sets.len() {
            return Err("compiled temporal rule columns differ in length".to_string());
        }
        if self.edges.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("compiled temporal rule edges are not strictly sorted".to_string());
        }
        if self
            .edges
            .last()
            .is_some_and(|edge| *edge as usize >= edge_count)
        {
            return Err("compiled temporal rules reference a missing edge".to_string());
        }
        if self
            .edge_rule_sets
            .iter()
            .any(|id| *id as usize >= self.rule_sets.len())
        {
            return Err("compiled temporal rules reference a missing rule set".to_string());
        }
        Ok(())
    }
}

fn default_max_wait_s() -> f64 {
    3_600.0
}

/// Fixed-point CCH weights use 1/1024 of the objective unit. Binary scaling
/// keeps decoded sums exact. MAX is inaccessible; MAX-1 is finite overflow.
/// Positive weights occupy at least one tick so rounding cannot create free
/// physical traversals. Exact zero remains available for coincident connectors.
pub const CCH_WEIGHT_SCALE: f64 = 1024.0;
pub const CCH_WEIGHT_INFINITY: u32 = u32::MAX;
pub const CCH_WEIGHT_OVERFLOW: u32 = u32::MAX - 1;

pub fn encode_cch_weight(weight: f64) -> u32 {
    if !weight.is_finite() {
        return CCH_WEIGHT_INFINITY;
    }
    let scaled = (weight.max(0.0) * CCH_WEIGHT_SCALE).round();
    if scaled >= CCH_WEIGHT_OVERFLOW as f64 {
        CCH_WEIGHT_OVERFLOW
    } else {
        (scaled as u32).max(u32::from(weight > 0.0))
    }
}

pub fn decode_cch_weight(weight: u32) -> f64 {
    if weight >= CCH_WEIGHT_OVERFLOW {
        f64::INFINITY
    } else {
        weight as f64 / CCH_WEIGHT_SCALE
    }
}

pub fn add_cch_weights(left: u32, right: u32) -> u32 {
    if left == CCH_WEIGHT_INFINITY || right == CCH_WEIGHT_INFINITY {
        CCH_WEIGHT_INFINITY
    } else {
        left.saturating_add(right).min(CCH_WEIGHT_OVERFLOW)
    }
}

/// Three fixed-point metrics per arc, occupying 12 bytes. Path unpacking
/// finds a matching lower triangle in the shared topology.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledAcceleration {
    pub schema_version: u32,
    pub source_acceleration_bundle_id: CacheBundleId,
    pub algorithm: String,
    pub upward_weight: Vec<u32>,
    pub downward_weight: Vec<u32>,
    /// Travel-time weight sets (seconds, time-based turn penalties) over the
    /// same arcs, used for fixed-point one-to-all sweeps on time-limited
    /// isochrones. Empty when the compiled profile carries no such weights,
    /// in which case time-limited searches run plain Dijkstra.
    pub time_upward_weight: Vec<u32>,
    pub time_downward_weight: Vec<u32>,
    /// Distance weight sets (metres, no turn costs) over the same arcs, for
    /// distance-limited isochrones. Empty when the compiled profile carries
    /// no such weights, in which case distance-limited searches run plain
    /// Dijkstra.
    pub distance_upward_weight: Vec<u32>,
    pub distance_downward_weight: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledProfileBundle {
    pub schema_version: u32,
    pub profile_id: String,
    /// Identifies these metrics among the bundles of one process, for caches
    /// of structures derived from them. Each compile or read gets a fresh
    /// key; it is not persisted.
    #[serde(skip, default = "fresh_metric_key")]
    pub metric_key: String,
    pub mode: TravelMode,
    pub turn_costs: CompiledTurnCostConfig,
    pub components: Vec<CompiledCostComponent>,
    pub temporal: CompiledTemporalProfile,
    pub source_topology_bundle_id: CacheBundleId,
    pub acceleration: Option<CompiledAcceleration>,
    pub edge_metrics: Vec<CompiledEdgeMetric>,
    /// Edges the profile's vehicle excludes, with the metrics they would
    /// otherwise have, so requests with other vehicle parameters can
    /// re-enable them.
    #[serde(default)]
    pub vehicle: CompiledVehicleExclusions,
}

/// A [`CompiledProfileBundle::metric_key`] no other bundle of this process
/// carries.
pub fn fresh_metric_key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("metrics-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// The profile's vehicle and the edges its physical restrictions exclude
/// although the profile could otherwise traverse them, each with the metric
/// and cost component values it would have. Excluded edges carry no metric
/// in [`CompiledProfileBundle::edge_metrics`] and zero component values, as
/// any untraversable edge does; this table lets a request whose vehicle
/// passes some of them restore their metrics without recompiling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CompiledVehicleExclusions {
    /// The profile's `vehicle` block; request vehicles override its fields.
    pub vehicle: Option<VehicleParams>,
    /// Excluded edge indexes, strictly increasing.
    pub edges: Vec<u32>,
    pub travel_time_s: Vec<f64>,
    pub generalized_cost: Vec<f64>,
    /// `component_values[c][i]` is the value of component `c` of
    /// [`CompiledProfileBundle::components`] on `edges[i]`.
    pub component_values: Vec<Vec<f32>>,
}

impl CompiledVehicleExclusions {
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// Position of `edge_index` in [`Self::edges`].
    pub fn position(&self, edge_index: usize) -> Option<usize> {
        let edge = u32::try_from(edge_index).ok()?;
        self.edges.binary_search(&edge).ok()
    }

    pub fn validate(&self, edge_count: usize, component_count: usize) -> Result<(), String> {
        let len = self.edges.len();
        if self.travel_time_s.len() != len
            || self.generalized_cost.len() != len
            || self.component_values.len() != component_count
            || self
                .component_values
                .iter()
                .any(|values| values.len() != len)
        {
            return Err("vehicle exclusion columns differ in length".to_string());
        }
        if self.edges.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("vehicle exclusion edges are not strictly sorted".to_string());
        }
        if self
            .edges
            .last()
            .is_some_and(|edge| *edge as usize >= edge_count)
        {
            return Err("vehicle exclusions reference a missing edge".to_string());
        }
        if let Some(vehicle) = &self.vehicle {
            vehicle.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
