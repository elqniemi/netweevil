use serde::{Deserialize, Serialize};

use crate::CacheBundleId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccelerationBuildProfile {
    Compact,
    #[default]
    Balanced,
    Aggressive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccelerationBuildSettings {
    #[serde(default)]
    pub profile: AccelerationBuildProfile,
    #[serde(default = "default_max_shortcut_path_len")]
    pub max_shortcut_path_len: u32,
    #[serde(default = "default_max_shortcuts_per_contracted_edge")]
    pub max_shortcuts_per_contracted_edge: u32,
    #[serde(default = "default_max_shortcut_budget_per_edge")]
    pub max_shortcut_budget_per_edge: u32,
}

impl Default for AccelerationBuildSettings {
    fn default() -> Self {
        Self::for_profile(AccelerationBuildProfile::Balanced)
    }
}

impl AccelerationBuildSettings {
    pub fn for_profile(profile: AccelerationBuildProfile) -> Self {
        match profile {
            AccelerationBuildProfile::Compact => Self {
                profile,
                max_shortcut_path_len: 12,
                max_shortcuts_per_contracted_edge: 32,
                max_shortcut_budget_per_edge: 1,
            },
            AccelerationBuildProfile::Balanced => Self {
                profile,
                max_shortcut_path_len: default_max_shortcut_path_len(),
                max_shortcuts_per_contracted_edge: default_max_shortcuts_per_contracted_edge(),
                max_shortcut_budget_per_edge: default_max_shortcut_budget_per_edge(),
            },
            AccelerationBuildProfile::Aggressive => Self {
                profile,
                max_shortcut_path_len: 96,
                max_shortcuts_per_contracted_edge: 2_048,
                max_shortcut_budget_per_edge: 8,
            },
        }
    }

    pub fn normalized(self) -> Self {
        Self {
            profile: self.profile,
            max_shortcut_path_len: self.max_shortcut_path_len.max(1),
            max_shortcuts_per_contracted_edge: self.max_shortcuts_per_contracted_edge,
            max_shortcut_budget_per_edge: self.max_shortcut_budget_per_edge,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AccelerationBundleStats {
    #[serde(default)]
    pub base_arc_count: u64,
    #[serde(default)]
    pub shortcut_arc_count: u64,
    #[serde(default)]
    pub total_arc_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetAccelerationBundle {
    pub schema_version: u32,
    pub source_topology_bundle_id: CacheBundleId,
    pub algorithm: String,
    #[serde(default)]
    pub build_settings: AccelerationBuildSettings,
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

const fn default_max_shortcut_path_len() -> u32 {
    64
}

const fn default_max_shortcuts_per_contracted_edge() -> u32 {
    1_024
}

const fn default_max_shortcut_budget_per_edge() -> u32 {
    4
}
