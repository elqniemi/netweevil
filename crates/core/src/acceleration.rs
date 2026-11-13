use serde::{Deserialize, Serialize};

use crate::CacheBundleId;

/// Algorithm identifier for the current CCH acceleration bundle format.
pub const CCH_ALGORITHM: &str = "edge_based_cch_v2";

/// Schema version of [`DatasetAccelerationBundle`].
pub const ACCELERATION_BUNDLE_SCHEMA_VERSION: u32 = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AccelerationBundleStats {
    pub base_arc_count: u64,
    pub shortcut_arc_count: u64,
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
    pub stats: AccelerationBundleStats,
    /// Fingerprint of the arc topology ([`Self::compute_hierarchy_key`]).
    /// Customized weights record the key they were computed for, so weights
    /// of another hierarchy are never paired with this one.
    pub hierarchy_key: u64,
    pub edge_order: Vec<u32>,
    pub edge_rank: Vec<u32>,
    pub upward_first_out: Vec<u32>,
    pub upward_head: Vec<u32>,
    pub downward_first_out: Vec<u32>,
    pub downward_head: Vec<u32>,
}

impl DatasetAccelerationBundle {
    /// Whether this bundle uses the current CCH algorithm and schema.
    pub fn is_current_format(&self) -> bool {
        self.schema_version == ACCELERATION_BUNDLE_SCHEMA_VERSION && self.algorithm == CCH_ALGORITHM
    }

    /// A 64-bit fingerprint of the order and every arc array.
    pub fn compute_hierarchy_key(&self) -> u64 {
        let mut hash = FingerprintHasher::default();
        for column in [
            &self.edge_order,
            &self.edge_rank,
            &self.upward_first_out,
            &self.upward_head,
            &self.downward_first_out,
            &self.downward_head,
        ] {
            hash.write_u64(column.len() as u64);
            for &value in column.iter() {
                hash.write_u32(value);
            }
        }
        hash.finish()
    }
}

/// FNV-1a style mixing over 32-bit words, finished with a 64-bit avalanche.
struct FingerprintHasher(u64);

impl Default for FingerprintHasher {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl FingerprintHasher {
    fn write_u32(&mut self, value: u32) {
        self.0 = (self.0 ^ u64::from(value)).wrapping_mul(0x0000_0100_0000_01b3);
    }

    fn write_u64(&mut self, value: u64) {
        self.write_u32(value as u32);
        self.write_u32((value >> 32) as u32);
    }

    fn finish(&self) -> u64 {
        let mut value = self.0;
        value ^= value >> 33;
        value = value.wrapping_mul(0xff51_afd7_ed55_8ccd);
        value ^= value >> 33;
        value = value.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        value ^ (value >> 33)
    }
}
