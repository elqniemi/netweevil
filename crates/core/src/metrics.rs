use serde::{Deserialize, Serialize};

use crate::{CacheBundleId, EdgeId, TravelMode};

/// Schema version of [`CompiledProfileBundle`]. Readers accept this value only.
pub const COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION: u32 = 5;

/// Schema version of [`CompiledAcceleration`] customization data.
pub const COMPILED_ACCELERATION_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct CompiledTurnCostConfig {
    pub left_penalty_s: f64,
    pub right_penalty_s: f64,
    pub uturn_penalty_s: f64,
    pub traffic_signal_penalty_s: f64,
    pub roundabout_entry_penalty_s: f64,
    pub cost_time_weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledEdgeMetric {
    pub edge_id: EdgeId,
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
    pub edge_values: Vec<f32>,
    /// Components based on `travel_time` scale with a temporal speed factor
    /// (but not with waiting before an edge opens).
    pub scales_with_travel_time: bool,
    /// Optional continuous temporal overlay multiplied into the value.
    pub overlay_name: Option<String>,
    /// When true, multiply by `1 - overlay` instead of `overlay`.
    pub invert_overlay: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CompiledTemporalProfile {
    pub allow_wait: bool,
    pub max_wait_s: f64,
}

impl Default for CompiledTemporalProfile {
    fn default() -> Self {
        Self {
            allow_wait: false,
            max_wait_s: default_max_wait_s(),
        }
    }
}

fn default_max_wait_s() -> f64 {
    3_600.0
}

/// Sentinel for arcs whose weight comes from the base transition rather than
/// a lower triangle; such arcs unpack directly to their head edge state.
pub const NO_MIDDLE: u32 = u32::MAX;

/// Per-profile CCH customization aligned with a [`crate::DatasetAccelerationBundle`].
///
/// `upward_middle`/`downward_middle` record, per arc, the edge state whose
/// contraction produced the winning lower triangle (or [`NO_MIDDLE`] when the
/// base transition wins) so query-time unpacking can recurse without storing
/// full path expansions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledAcceleration {
    pub schema_version: u32,
    pub source_acceleration_bundle_id: CacheBundleId,
    pub algorithm: String,
    pub upward_weight: Vec<f64>,
    pub upward_middle: Vec<u32>,
    pub downward_weight: Vec<f64>,
    pub downward_middle: Vec<u32>,
    /// Travel-time weight sets (seconds, time-based turn penalties) over the
    /// same arcs, used for exact one-to-all sweeps on time-limited
    /// isochrones. Empty when the compiled profile carries no such weights,
    /// in which case time-limited searches run plain Dijkstra.
    pub time_upward_weight: Vec<f64>,
    pub time_downward_weight: Vec<f64>,
    /// Distance weight sets (metres, no turn costs) over the same arcs, for
    /// distance-limited isochrones. Empty when the compiled profile carries
    /// no such weights, in which case distance-limited searches run plain
    /// Dijkstra.
    pub distance_upward_weight: Vec<f64>,
    pub distance_downward_weight: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledProfileBundle {
    pub schema_version: u32,
    pub profile_id: String,
    pub profile_hash: String,
    pub mode: TravelMode,
    pub turn_costs: CompiledTurnCostConfig,
    pub components: Vec<CompiledCostComponent>,
    pub temporal: CompiledTemporalProfile,
    pub source_topology_bundle_id: CacheBundleId,
    pub acceleration: Option<CompiledAcceleration>,
    pub edge_metrics: Vec<CompiledEdgeMetric>,
}
