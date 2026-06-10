use serde::{Deserialize, Serialize};

use crate::CacheBundleId;

/// Algorithm identifier for the current CCH acceleration bundle format.
pub const CCH_ALGORITHM: &str = "edge_based_cch_v2";

/// Schema version of [`DatasetAccelerationBundle`].
pub const ACCELERATION_BUNDLE_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AccelerationBundleStats {
    #[serde(default)]
    pub base_arc_count: u64,
    #[serde(default)]
    pub shortcut_arc_count: u64,
    #[serde(default)]
    pub total_arc_count: u64,
}

/// Metric-independent CCH topology built once per dataset.
///
/// Vertices are directed edge states; arcs are legal edge-to-edge transitions
/// plus the complete set of elimination shortcuts. Arcs are split by rank
/// direction into an upward CSR (`edge_rank[tail] < edge_rank[head]`) and a
/// downward CSR (`edge_rank[tail] > edge_rank[head]`); every CSR row is
/// sorted by head id so customization and unpacking can binary-search rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetAccelerationBundle {
    pub schema_version: u32,
    pub source_topology_bundle_id: CacheBundleId,
    pub algorithm: String,
    #[serde(default)]
    pub stats: AccelerationBundleStats,
    #[serde(default)]
    pub edge_order: Vec<u32>,
    #[serde(default)]
    pub edge_rank: Vec<u32>,
    #[serde(default)]
    pub upward_first_out: Vec<u32>,
    #[serde(default)]
    pub upward_head: Vec<u32>,
    #[serde(default)]
    pub downward_first_out: Vec<u32>,
    #[serde(default)]
    pub downward_head: Vec<u32>,
}

impl DatasetAccelerationBundle {
    /// Whether this bundle uses the current CCH algorithm and schema.
    pub fn is_current_format(&self) -> bool {
        self.schema_version == ACCELERATION_BUNDLE_SCHEMA_VERSION && self.algorithm == CCH_ALGORITHM
    }
}
