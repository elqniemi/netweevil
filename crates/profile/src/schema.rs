use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use netweevil_core::TravelMode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// Bump when compilation semantics change without a profile YAML or bundle
// layout change. This also invalidates transfer tables tied to profile hashes.
const COST_MODEL_REVISION: &[u8] = b"netweevil-profile-cost-v2\0";

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
    /// Optional walking-speed adjustment derived from the directed edge
    /// gradient. When omitted, the profile's ordinary speed rules are used
    /// unchanged.
    #[serde(default)]
    pub slope_model: Option<SlopeModelConfig>,
    /// Class-specific traversal models for stairs, lifts, escalators and
    /// other pedestrian facilities.
    #[serde(default)]
    pub facilities: FacilityConfig,
    /// Named quantities accumulated alongside the scalar routing objective.
    /// The expression language is deliberately small and edge-local; see
    /// [`CostComponentConfig::expression`].
    #[serde(default)]
    pub components: BTreeMap<String, CostComponentConfig>,
    /// Waiting policy used by exact time-dependent searches.
    #[serde(default)]
    pub temporal: TemporalProfileConfig,
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
        if let Some(slope_model) = &self.slope_model {
            slope_model.validate()?;
        }
        self.facilities.validate()?;
        for (name, component) in &self.components {
            if name.trim().is_empty() {
                bail!("component names must not be empty");
            }
            if component.expression.trim().is_empty() {
                bail!("component '{name}' must contain a non-empty expression");
            }
            if !component.weight.is_finite() || component.weight < 0.0 {
                bail!("component '{name}' weight must be finite and non-negative");
            }
        }
        if !self.temporal.max_wait_s.is_finite() || self.temporal.max_wait_s < 0.0 {
            bail!("temporal.max_wait_s must be finite and non-negative");
        }
        Ok(())
    }

    pub fn fingerprint(&self) -> Result<String> {
        let bytes = serde_yaml::to_string(self).context("serializing profile for hashing")?;
        let mut hash = Sha256::new();
        hash.update(COST_MODEL_REVISION);
        hash.update(bytes.as_bytes());
        Ok(hex::encode(hash.finalize()))
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

/// Directed-gradient walking model. Gradients are represented as rise/run
/// (for example `0.10` means a ten-percent uphill grade).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SlopeModelConfig {
    /// Tobler's hiking function, normalized so a flat edge keeps the speed
    /// selected by the ordinary profile rules. The normalization preserves
    /// custom pedestrian base speeds while retaining Tobler's asymmetric
    /// uphill/downhill response.
    Tobler {
        #[serde(default = "default_tobler_exponent")]
        exponent: f64,
        #[serde(default = "default_tobler_optimal_gradient")]
        optimal_gradient: f64,
        #[serde(default = "default_min_slope_speed_factor")]
        min_speed_factor: f64,
        #[serde(default = "default_max_slope_speed_factor")]
        max_speed_factor: f64,
    },
    /// Linear interpolation between gradient/speed-factor control points.
    /// Values outside the supplied range use the nearest endpoint.
    Piecewise { points: Vec<SlopePoint> },
}

impl SlopeModelConfig {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Tobler {
                exponent,
                optimal_gradient,
                min_speed_factor,
                max_speed_factor,
            } => {
                if !exponent.is_finite() || *exponent <= 0.0 {
                    bail!("slope_model.exponent must be finite and > 0");
                }
                if !optimal_gradient.is_finite() {
                    bail!("slope_model.optimal_gradient must be finite");
                }
                if !min_speed_factor.is_finite()
                    || !max_speed_factor.is_finite()
                    || *min_speed_factor <= 0.0
                    || *max_speed_factor < *min_speed_factor
                {
                    bail!("slope_model speed-factor bounds must be finite, positive, and ordered");
                }
            }
            Self::Piecewise { points } => {
                if points.is_empty() {
                    bail!("piecewise slope_model must contain at least one point");
                }
                let mut previous = f64::NEG_INFINITY;
                for (index, point) in points.iter().enumerate() {
                    if !point.gradient.is_finite()
                        || !point.speed_factor.is_finite()
                        || point.speed_factor <= 0.0
                    {
                        bail!(
                            "slope_model.points[{index}] must contain a finite gradient and a positive finite speed_factor"
                        );
                    }
                    if point.gradient <= previous {
                        bail!("piecewise slope_model gradients must be strictly increasing");
                    }
                    previous = point.gradient;
                }
            }
        }
        Ok(())
    }
}

fn default_tobler_exponent() -> f64 {
    3.5
}

fn default_tobler_optimal_gradient() -> f64 {
    0.05
}

fn default_min_slope_speed_factor() -> f64 {
    0.1
}

fn default_max_slope_speed_factor() -> f64 {
    2.0
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SlopePoint {
    pub gradient: f64,
    pub speed_factor: f64,
}

/// Facility classes are matched against any semantic or retained dataset
/// attribute. A class not present in this map falls back to normal edge
/// traversal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FacilityConfig {
    #[serde(default = "default_facility_attribute")]
    pub attribute: String,
    #[serde(default)]
    pub classes: BTreeMap<String, FacilityCost>,
}

impl Default for FacilityConfig {
    fn default() -> Self {
        Self {
            attribute: default_facility_attribute(),
            classes: BTreeMap::new(),
        }
    }
}

impl FacilityConfig {
    fn validate(&self) -> Result<()> {
        if !self.classes.is_empty() && self.attribute.trim().is_empty() {
            bail!("facilities.attribute must not be empty when classes are configured");
        }
        for (class, cost) in &self.classes {
            if class.trim().is_empty() {
                bail!("facility class names must not be empty");
            }
            cost.validate(class)?;
        }
        Ok(())
    }
}

fn default_facility_attribute() -> String {
    "feature_type".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FacilityCost {
    Stairs {
        #[serde(default = "default_stair_vertical_speed")]
        vertical_speed_mps: f64,
        #[serde(default)]
        boarding_penalty_s: f64,
        #[serde(default)]
        burden_per_vertical_m: f64,
        #[serde(default = "default_stair_burden_component")]
        burden_component: String,
    },
    Escalator {
        #[serde(default = "default_escalator_speed")]
        conveyor_speed_mps: f64,
        #[serde(default)]
        walking_speed_mps: f64,
        #[serde(default)]
        boarding_penalty_s: f64,
    },
    Lift {
        #[serde(default)]
        wait_s: f64,
        #[serde(default = "default_lift_vertical_speed")]
        vertical_speed_mps: f64,
        #[serde(default = "default_lift_wait_component")]
        wait_component: String,
    },
    Travelator {
        #[serde(default = "default_travelator_speed")]
        conveyor_speed_mps: f64,
        #[serde(default)]
        walking_speed_mps: f64,
        #[serde(default)]
        boarding_penalty_s: f64,
    },
    Ramp {
        #[serde(default = "default_ramp_speed_factor")]
        speed_factor: f64,
        #[serde(default)]
        boarding_penalty_s: f64,
    },
}

impl FacilityCost {
    fn validate(&self, class: &str) -> Result<()> {
        let positive = |name: &str, value: f64| -> Result<()> {
            if !value.is_finite() || value <= 0.0 {
                bail!("facility class '{class}' {name} must be finite and > 0");
            }
            Ok(())
        };
        let non_negative = |name: &str, value: f64| -> Result<()> {
            if !value.is_finite() || value < 0.0 {
                bail!("facility class '{class}' {name} must be finite and non-negative");
            }
            Ok(())
        };
        match self {
            Self::Stairs {
                vertical_speed_mps,
                boarding_penalty_s,
                burden_per_vertical_m,
                burden_component,
            } => {
                positive("vertical_speed_mps", *vertical_speed_mps)?;
                non_negative("boarding_penalty_s", *boarding_penalty_s)?;
                non_negative("burden_per_vertical_m", *burden_per_vertical_m)?;
                if *burden_per_vertical_m > 0.0 && burden_component.trim().is_empty() {
                    bail!("facility class '{class}' burden_component must not be empty");
                }
            }
            Self::Escalator {
                conveyor_speed_mps,
                walking_speed_mps,
                boarding_penalty_s,
            }
            | Self::Travelator {
                conveyor_speed_mps,
                walking_speed_mps,
                boarding_penalty_s,
            } => {
                positive("conveyor_speed_mps", *conveyor_speed_mps)?;
                non_negative("walking_speed_mps", *walking_speed_mps)?;
                non_negative("boarding_penalty_s", *boarding_penalty_s)?;
            }
            Self::Lift {
                wait_s,
                vertical_speed_mps,
                wait_component,
            } => {
                non_negative("wait_s", *wait_s)?;
                positive("vertical_speed_mps", *vertical_speed_mps)?;
                if wait_component.trim().is_empty() {
                    bail!("facility class '{class}' wait_component must not be empty");
                }
            }
            Self::Ramp {
                speed_factor,
                boarding_penalty_s,
            } => {
                positive("speed_factor", *speed_factor)?;
                non_negative("boarding_penalty_s", *boarding_penalty_s)?;
            }
        }
        Ok(())
    }
}

fn default_stair_vertical_speed() -> f64 {
    0.35
}

fn default_stair_burden_component() -> String {
    "stair_burden".to_string()
}

fn default_escalator_speed() -> f64 {
    0.5
}

fn default_lift_vertical_speed() -> f64 {
    1.5
}

fn default_lift_wait_component() -> String {
    "lift_wait".to_string()
}

fn default_travelator_speed() -> f64 {
    0.75
}

fn default_ramp_speed_factor() -> f64 {
    0.8
}

/// Named component definition. Supported expressions are a base quantity
/// (`travel_time`, `distance`, `ascent`, or `descent`) optionally multiplied
/// by an attribute predicate (`covered == false`) and/or an overlay term
/// (`overlay:shade_fraction` or `1 - overlay:shade_fraction`). Parentheses
/// and either `*` or the multiplication sign are accepted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostComponentConfig {
    pub expression: String,
    #[serde(default)]
    pub weight: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TemporalProfileConfig {
    #[serde(default)]
    pub allow_wait: bool,
    #[serde(default = "default_max_wait_s")]
    pub max_wait_s: f64,
}

impl Default for TemporalProfileConfig {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_invalidates_metrics_from_unversioned_cost_model() {
        let profile: ProfileDocument = serde_yaml::from_str(include_str!(
            "../../../examples/profiles/pedestrian_multilayer.yml"
        ))
        .unwrap();
        let yaml = serde_yaml::to_string(&profile).unwrap();
        let legacy_hash = hex::encode(Sha256::digest(yaml.as_bytes()));
        let hash = profile.fingerprint().unwrap();
        assert_ne!(hash, legacy_hash);
        assert_eq!(hash, profile.fingerprint().unwrap());
        let mut changed = profile;
        changed.profile.label.push_str(" changed");
        assert_ne!(hash, changed.fingerprint().unwrap());
    }
}
