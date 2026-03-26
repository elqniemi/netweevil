use serde::{Deserialize, Serialize};

use crate::CacheBundleId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetAccelerationBundle {
    pub schema_version: u32,
    pub source_topology_bundle_id: CacheBundleId,
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
    pub upward_path_first_out: Vec<u32>,
    #[serde(default)]
    pub upward_path_edges: Vec<u32>,
    #[serde(default)]
    pub downward_first_out: Vec<u32>,
    #[serde(default)]
    pub downward_head: Vec<u32>,
    #[serde(default)]
    pub downward_path_first_out: Vec<u32>,
    #[serde(default)]
    pub downward_path_edges: Vec<u32>,
}
