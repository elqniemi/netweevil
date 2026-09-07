use serde::{Deserialize, Serialize};

use crate::{CacheBundleId, EdgeId, TravelMode};

/// Schema version of [`CompiledProfileBundle`]. Readers accept this value only.
pub const COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION: u32 = 6;

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

/// Fixed-point CCH weights use 1/1024 of the objective unit. Binary scaling
/// keeps decoded sums exact. MAX is inaccessible; MAX-1 is finite overflow.
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
        scaled as u32
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
    pub profile_hash: String,
    pub mode: TravelMode,
    pub turn_costs: CompiledTurnCostConfig,
    pub components: Vec<CompiledCostComponent>,
    pub temporal: CompiledTemporalProfile,
    pub source_topology_bundle_id: CacheBundleId,
    pub acceleration: Option<CompiledAcceleration>,
    pub edge_metrics: Vec<CompiledEdgeMetric>,
}

#[cfg(test)]
mod fixed_point_tests {
    use super::*;

    #[test]
    fn fixed_point_rounding_and_path_sum() {
        assert_eq!(std::mem::size_of::<u32>() * 3, 12);
        assert_eq!(encode_cch_weight(0.5 / CCH_WEIGHT_SCALE), 1);
        assert_eq!(encode_cch_weight(0.0), 0);
        let values = [0.0001, 0.123456, 123.456789, 500.123456];
        let mut sum = 0;
        for value in values {
            let encoded = encode_cch_weight(value);
            assert!((decode_cch_weight(encoded) - value).abs() <= 0.5 / CCH_WEIGHT_SCALE);
            sum = add_cch_weights(sum, encoded);
        }
        assert!(
            (decode_cch_weight(sum) - values.iter().sum::<f64>()).abs()
                <= values.len() as f64 * 0.5 / CCH_WEIGHT_SCALE
        );
    }

    #[test]
    fn overflow_stays_distinct_from_inaccessible_and_never_wraps() {
        assert_eq!(encode_cch_weight(f64::INFINITY), CCH_WEIGHT_INFINITY);
        assert_eq!(encode_cch_weight(1e20), CCH_WEIGHT_OVERFLOW);
        assert_eq!(
            add_cch_weights(CCH_WEIGHT_OVERFLOW - 10, 20),
            CCH_WEIGHT_OVERFLOW
        );
        assert_eq!(add_cch_weights(CCH_WEIGHT_OVERFLOW, 5), CCH_WEIGHT_OVERFLOW);
        assert_eq!(add_cch_weights(CCH_WEIGHT_INFINITY, 0), CCH_WEIGHT_INFINITY);
        let largest = CCH_WEIGHT_OVERFLOW - 1;
        assert_eq!(encode_cch_weight(decode_cch_weight(largest)), largest);
    }
}
