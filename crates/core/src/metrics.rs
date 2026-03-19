use serde::{Deserialize, Serialize};

use crate::{CacheBundleId, EdgeId, TravelMode};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledEdgeMetric {
    pub edge_id: EdgeId,
    #[serde(default)]
    pub travel_time_s: Option<f64>,
    #[serde(default)]
    pub generalized_cost: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledProfileBundle {
    pub schema_version: u32,
    pub profile_id: String,
    pub profile_hash: String,
    #[serde(default)]
    pub mode: TravelMode,
    pub source_topology_bundle_id: CacheBundleId,
    pub edge_metrics: Vec<CompiledEdgeMetric>,
}
