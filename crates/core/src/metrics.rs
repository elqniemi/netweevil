use serde::{Deserialize, Serialize};

use crate::{CacheBundleId, EdgeId, TravelMode};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct CompiledTurnCostConfig {
    #[serde(default)]
    pub left_penalty_s: f64,
    #[serde(default)]
    pub right_penalty_s: f64,
    #[serde(default)]
    pub uturn_penalty_s: f64,
    #[serde(default)]
    pub traffic_signal_penalty_s: f64,
    #[serde(default)]
    pub roundabout_entry_penalty_s: f64,
    #[serde(default)]
    pub cost_time_weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledEdgeMetric {
    pub edge_id: EdgeId,
    #[serde(default)]
    pub travel_time_s: Option<f64>,
    #[serde(default)]
    pub generalized_cost: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledAcceleration {
    pub schema_version: u32,
    pub source_acceleration_bundle_id: CacheBundleId,
    pub algorithm: String,
    #[serde(default)]
    pub edge_order: Vec<u32>,
    #[serde(default)]
    pub edge_rank: Vec<u32>,
    #[serde(default)]
    pub upward_first_out: Vec<u32>,
    #[serde(default)]
    pub upward_head: Vec<u32>,
    #[serde(default)]
    pub upward_weight: Vec<f64>,
    #[serde(default)]
    pub upward_path_first_out: Vec<u32>,
    #[serde(default)]
    pub upward_path_edges: Vec<u32>,
    #[serde(default)]
    pub downward_first_out: Vec<u32>,
    #[serde(default)]
    pub downward_head: Vec<u32>,
    #[serde(default)]
    pub downward_weight: Vec<f64>,
    #[serde(default)]
    pub downward_path_first_out: Vec<u32>,
    #[serde(default)]
    pub downward_path_edges: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledProfileBundle {
    pub schema_version: u32,
    pub profile_id: String,
    pub profile_hash: String,
    #[serde(default)]
    pub mode: TravelMode,
    #[serde(default)]
    pub turn_costs: CompiledTurnCostConfig,
    pub source_topology_bundle_id: CacheBundleId,
    #[serde(default)]
    pub acceleration: Option<CompiledAcceleration>,
    pub edge_metrics: Vec<CompiledEdgeMetric>,
}
