//! Routing profile schema and compiler. Profiles are YAML documents
//! describing mode, access rules, speeds, turn costs, and generalized-cost
//! weights; compilation turns them into per-edge metrics and customized CCH
//! weights for a specific dataset.

mod compile;
mod components;
mod edge_cost;
mod load;
mod progress;
mod schema;
mod turns;

pub use compile::{
    compile_profile_bundle, compile_profile_bundle_with_acceleration,
    compile_profile_bundle_with_acceleration_with_progress,
};
pub use load::load_profile;
pub use progress::{ProfileCompileProgress, ProfileCompileStage};
pub use schema::{
    BreakdownMetric, CostComponentConfig, CostConfig, DirectionConfig, ExcludeRule, FacilityConfig,
    FacilityCost, FactorRule, FerryConfig, Objective, PostedLimitPolicy, PreferencesConfig,
    ProfileDocument, ProfileHeader, ReturnConfig, ReturnGeometry, SlopeModelConfig, SlopePoint,
    SpeedRule, SpeedsConfig, TagMatch, TemporalProfileConfig, TurnConfig,
};
