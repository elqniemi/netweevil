use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use netweevil_core::TravelMode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileDocument {
    pub profile: ProfileHeader,
    #[serde(default)]
    pub cost: CostConfig,
    #[serde(default)]
    pub speeds: SpeedsConfig,
    #[serde(default)]
    pub speed_rules: Vec<SpeedRule>,
    #[serde(default)]
    pub exclude_rules: Vec<ExcludeRule>,
    #[serde(default)]
    pub factors: Vec<FactorRule>,
    #[serde(default)]
    pub direction: DirectionConfig,
    #[serde(default)]
    pub turns: TurnConfig,
    #[serde(default)]
    pub ferry: FerryConfig,
    #[serde(default)]
    pub preferences: PreferencesConfig,
    #[serde(default)]
    pub returns: ReturnConfig,
}

impl ProfileDocument {
    pub fn validate(&self) -> Result<()> {
        if self.profile.id.trim().is_empty() {
            bail!("profile.id must not be empty");
        }
        if self.profile.defaults_pack.trim().is_empty() {
            bail!("profile.defaults_pack must not be empty");
        }
        if self.cost.distance_weight < 0.0 || self.cost.time_weight < 0.0 {
            bail!("cost weights must be non-negative");
        }
        for (index, rule) in self.speed_rules.iter().enumerate() {
            if rule.r#match.tags.is_empty() {
                bail!("speed_rules[{index}] must contain at least one match tag");
            }
            if rule.speed_kph <= 0.0 {
                bail!("speed_rules[{index}] speed_kph must be > 0");
            }
        }
        for (index, rule) in self.exclude_rules.iter().enumerate() {
            if rule.r#match.tags.is_empty() {
                bail!("exclude_rules[{index}] must contain at least one match tag");
            }
        }
        for (index, factor) in self.factors.iter().enumerate() {
            if factor.r#match.tags.is_empty() {
                bail!("factors[{index}] must contain at least one match tag");
            }
            if factor.speed_factor <= 0.0 {
                bail!("factors[{index}] speed_factor must be > 0");
            }
        }
        Ok(())
    }

    pub fn fingerprint(&self) -> Result<String> {
        let bytes = serde_yaml::to_string(self).context("serializing profile for hashing")?;
        Ok(hex::encode(Sha256::digest(bytes.as_bytes())))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileHeader {
    pub id: String,
    pub label: String,
    pub mode: TravelMode,
    pub defaults_pack: String,
    #[serde(default)]
    pub extends: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostConfig {
    #[serde(default)]
    pub objective: Objective,
    #[serde(default)]
    pub distance_weight: f64,
    #[serde(default = "default_time_weight")]
    pub time_weight: f64,
}

impl Default for CostConfig {
    fn default() -> Self {
        Self {
            objective: Objective::Fastest,
            distance_weight: 0.0,
            time_weight: 1.0,
        }
    }
}

fn default_time_weight() -> f64 {
    1.0
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Objective {
    #[default]
    Fastest,
    Shortest,
    Generalized,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SpeedsConfig {
    /// How posted speed limits from the source data (OSM `maxspeed`,
    /// Overture `speed_limits`) combine with class/profile speeds for
    /// motorized modes.
    #[serde(default)]
    pub posted_limits: PostedLimitPolicy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PostedLimitPolicy {
    /// The posted limit replaces the class/profile speed wherever the
    /// source data carries one; profile speeds fill the gaps.
    Prefer,
    /// The posted limit caps the class/profile speed (default): travel is
    /// never assumed faster than the legal limit, but profile speeds that
    /// are already lower win.
    #[default]
    Cap,
    /// Posted limits are ignored; only class/profile speeds apply.
    Ignore,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TagMatch {
    #[serde(flatten)]
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeedRule {
    #[serde(rename = "match")]
    pub r#match: TagMatch,
    pub speed_kph: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExcludeRule {
    #[serde(rename = "match")]
    pub r#match: TagMatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorRule {
    #[serde(rename = "match")]
    pub r#match: TagMatch,
    pub speed_factor: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DirectionConfig {
    #[serde(default)]
    pub ignore_plain_oneway_for_foot: bool,
    #[serde(default)]
    pub allow_bicycle_contraflow_on_roads: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TurnConfig {
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FerryConfig {
    #[serde(default = "default_true")]
    pub allow: bool,
    #[serde(default = "default_use_ferry")]
    pub use_ferry: f64,
    #[serde(default)]
    pub boarding_cost_s: f64,
    #[serde(default)]
    pub infer_duration_when_missing: bool,
    #[serde(default = "default_ferry_speed")]
    pub default_speed_kph: f64,
}

impl Default for FerryConfig {
    fn default() -> Self {
        Self {
            allow: true,
            use_ferry: 0.5,
            boarding_cost_s: 0.0,
            infer_duration_when_missing: true,
            default_speed_kph: 20.0,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_use_ferry() -> f64 {
    0.5
}

fn default_ferry_speed() -> f64 {
    20.0
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PreferencesConfig {
    #[serde(default = "default_pref")]
    pub use_highways: f64,
    #[serde(default = "default_pref")]
    pub use_tolls: f64,
    #[serde(default = "default_pref")]
    pub use_tracks: f64,
    #[serde(default)]
    pub service_penalty_s: f64,
}

fn default_pref() -> f64 {
    0.5
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReturnConfig {
    #[serde(default)]
    pub geometry: ReturnGeometry,
    #[serde(default)]
    pub segment_rows: bool,
    #[serde(default)]
    pub road_type_breakdown: Vec<BreakdownMetric>,
    #[serde(default)]
    pub surface_breakdown: Vec<BreakdownMetric>,
    #[serde(default)]
    pub penalty_breakdown: bool,
    #[serde(default)]
    pub explain_cost_derivation: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReturnGeometry {
    #[default]
    None,
    Full,
    Segments,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakdownMetric {
    TimeS,
    DistanceM,
}
