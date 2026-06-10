//! Scenario schema for traffic simulations.
//!
//! A scenario is fully file-backed (YAML/TOML/JSON) so simulation runs stay
//! reproducible: the same scenario + dataset + profiles + seed always produce
//! the same result. CLI, API, and QGIS share this exact schema.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use netweevil_core::TravelMode;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationScenario {
    pub scenario: ScenarioHeader,
    #[serde(default)]
    pub time: TimeConfig,
    pub fleets: Vec<FleetConfig>,
    #[serde(default)]
    pub zones: Vec<ZoneConfig>,
    #[serde(default)]
    pub traffic: TrafficModelConfig,
    #[serde(default)]
    pub output: SimOutputConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioHeader {
    pub id: String,
    #[serde(default)]
    pub label: String,
    /// RNG seed: same seed -> identical run.
    #[serde(default = "default_seed")]
    pub seed: u64,
    #[serde(default)]
    pub description: String,
}

fn default_seed() -> u64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeConfig {
    /// Total simulated horizon in seconds.
    #[serde(default = "default_duration_s")]
    pub duration_s: f64,
    /// Engine tick in seconds. Smaller = finer movement, slower run.
    #[serde(default = "default_tick_s")]
    pub tick_s: f64,
    /// End early once every agent has arrived.
    #[serde(default = "default_true")]
    pub stop_when_all_arrived: bool,
}

impl Default for TimeConfig {
    fn default() -> Self {
        Self {
            duration_s: default_duration_s(),
            tick_s: default_tick_s(),
            stop_when_all_arrived: default_true(),
        }
    }
}

fn default_duration_s() -> f64 {
    3600.0
}

fn default_tick_s() -> f64 {
    1.0
}

fn default_true() -> bool {
    true
}

/// A group of agents dispatched with one travel mode / routing profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetConfig {
    pub fleet_id: String,
    /// Compiled profile to route and pace this fleet with. The travel mode
    /// comes from the profile.
    pub profile_id: String,
    pub agent_count: u32,
    #[serde(default)]
    pub demand: DemandConfig,
    #[serde(default)]
    pub departures: DepartureConfig,
    #[serde(default)]
    pub behavior: BehaviorConfig,
    #[serde(default)]
    pub speed: FleetSpeedConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DemandConfig {
    #[serde(default)]
    pub origins: EndpointDistribution,
    #[serde(default)]
    pub destinations: EndpointDistribution,
    /// Explicit OD pairs override origins/destinations when present; cycled
    /// when there are fewer pairs than agents.
    #[serde(default)]
    pub od_pairs: Vec<ExplicitOdPair>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplicitOdPair {
    pub origin: [f64; 2],
    pub destination: [f64; 2],
}

/// Where simulation endpoints are sampled from.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EndpointDistribution {
    /// Uniform random inside the dataset bounds (or an explicit bbox).
    #[default]
    RandomBounds,
    RandomBbox {
        /// [min_lon, min_lat, max_lon, max_lat]
        bbox: [f64; 4],
    },
    /// Uniform random inside a polygon (WGS84 outer ring).
    RandomPolygon { polygon: Vec<[f64; 2]> },
    /// Weighted choice from a fixed point set.
    Points { points: Vec<WeightedPoint> },
    /// Sample inside named scenario zones (kind=spawn/attract), weighted.
    Zones { zone_ids: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightedPoint {
    pub lon: f64,
    pub lat: f64,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default)]
    pub label: String,
}

fn default_weight() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DepartureConfig {
    /// Everyone departs in [start_s, end_s) uniformly.
    Uniform { start_s: f64, end_s: f64 },
    /// Normal distribution around a peak (clamped to >= 0).
    Peak { mean_s: f64, std_s: f64 },
    /// Everyone at the same instant.
    Instant { at_s: f64 },
    /// Poisson arrivals from start_s at rate_per_s.
    Poisson { start_s: f64, rate_per_s: f64 },
}

impl Default for DepartureConfig {
    fn default() -> Self {
        Self::Uniform {
            start_s: 0.0,
            end_s: 600.0,
        }
    }
}

/// Per-agent awareness/behavior parameters. Values are fleet means; each
/// agent jitters them deterministically (seeded) by `heterogeneity`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorConfig {
    /// 0..1: how much of its own desired speed an agent keeps in moderate
    /// congestion (models overtaking). 0 = stuck with the flow.
    #[serde(default = "default_overtake")]
    pub overtake_eagerness: f64,
    /// 0..1: probability the agent reroutes when it detects a jam.
    #[serde(default = "default_reroute")]
    pub reroute_eagerness: f64,
    /// Congestion threshold that counts as a jam (effective speed below this
    /// fraction of free-flow on the current edge).
    #[serde(default = "default_jam_threshold")]
    pub jam_speed_threshold: f64,
    /// Minimum seconds between reroute attempts per agent.
    #[serde(default = "default_reroute_cooldown")]
    pub reroute_cooldown_s: f64,
    /// Stop at red traffic signals.
    #[serde(default = "default_true")]
    pub obey_traffic_signals: bool,
    /// 0..1: share of agents dispatched onto ranked alternative routes
    /// instead of the single best route. Note: alternative searches cost
    /// roughly an order of magnitude more than plain routes, and with fully
    /// random demand every agent is a unique pair, so high shares dominate
    /// dispatch time. Demand with shared endpoints (points/zones/OD pairs)
    /// dedupes this cost per unique pair.
    #[serde(default = "default_alternative_share")]
    pub alternative_route_share: f64,
    /// Max alternative routes requested when dispatching.
    #[serde(default = "default_max_alternatives")]
    pub max_alternatives: usize,
    /// Enforce edge storage capacity (queueing + spillback, no overlapping
    /// vehicles). Disable for "ghost" flows.
    #[serde(default = "default_true")]
    pub no_collision: bool,
    /// 0..1 relative jitter applied per agent to behavior parameters.
    #[serde(default = "default_heterogeneity")]
    pub heterogeneity: f64,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            overtake_eagerness: default_overtake(),
            reroute_eagerness: default_reroute(),
            jam_speed_threshold: default_jam_threshold(),
            reroute_cooldown_s: default_reroute_cooldown(),
            obey_traffic_signals: true,
            alternative_route_share: default_alternative_share(),
            max_alternatives: default_max_alternatives(),
            no_collision: true,
            heterogeneity: default_heterogeneity(),
        }
    }
}

fn default_overtake() -> f64 {
    0.4
}

fn default_reroute() -> f64 {
    0.3
}

fn default_jam_threshold() -> f64 {
    0.4
}

fn default_reroute_cooldown() -> f64 {
    120.0
}

fn default_alternative_share() -> f64 {
    0.25
}

fn default_max_alternatives() -> usize {
    3
}

fn default_heterogeneity() -> f64 {
    0.15
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetSpeedConfig {
    /// Mean multiplier on profile speeds (agent heterogeneity around it).
    #[serde(default = "default_speed_mult")]
    pub speed_multiplier_mean: f64,
    #[serde(default = "default_speed_std")]
    pub speed_multiplier_std: f64,
    /// Hard cap regardless of edge speeds.
    #[serde(default)]
    pub max_speed_kph: Option<f64>,
    /// Used by the trapezoidal position model and spawn ramp-up.
    #[serde(default = "default_accel")]
    pub acceleration_mps2: f64,
    #[serde(default = "default_decel")]
    pub deceleration_mps2: f64,
    /// Passenger-car-units this fleet's vehicles occupy on an edge.
    /// Defaults by mode when omitted (car 1.0, hgv 2.0, bicycle 0.3, foot 0.1).
    #[serde(default)]
    pub pcu: Option<f64>,
}

impl Default for FleetSpeedConfig {
    fn default() -> Self {
        Self {
            speed_multiplier_mean: default_speed_mult(),
            speed_multiplier_std: default_speed_std(),
            max_speed_kph: None,
            acceleration_mps2: default_accel(),
            deceleration_mps2: default_decel(),
            pcu: None,
        }
    }
}

fn default_speed_mult() -> f64 {
    1.0
}

fn default_speed_std() -> f64 {
    0.08
}

fn default_accel() -> f64 {
    2.0
}

fn default_decel() -> f64 {
    2.5
}

pub fn default_pcu_for_mode(mode: TravelMode) -> f64 {
    match mode {
        TravelMode::Car => 1.0,
        TravelMode::Hgv => 2.0,
        TravelMode::Bicycle => 0.3,
        TravelMode::Foot => 0.1,
        TravelMode::Transit => 2.0,
    }
}

/// Polygon zones drawn in QGIS (or written in the scenario file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneConfig {
    pub zone_id: String,
    #[serde(default)]
    pub label: String,
    /// WGS84 outer ring, closed or open (closure handled internally).
    pub polygon: Vec<[f64; 2]>,
    pub effect: ZoneEffect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ZoneEffect {
    /// Edges inside are closed for the listed modes (empty = all modes).
    NoAccess {
        #[serde(default)]
        modes: Vec<TravelMode>,
    },
    /// Multiply edge speeds inside (e.g. 0.5 = half speed).
    SpeedFactor { factor: f64 },
    /// Multiply storage/flow capacity inside (e.g. 0.5 = lane closures).
    CapacityFactor { factor: f64 },
    /// Standing background traffic inside, as a fraction of storage
    /// capacity (0..1). Creates congestion without simulated agents.
    HighTraffic { background_load: f64 },
    /// Origin sampling zone for EndpointDistribution::Zones.
    Spawn {
        #[serde(default = "default_weight")]
        weight: f64,
    },
    /// Destination sampling zone for EndpointDistribution::Zones.
    Attract {
        #[serde(default = "default_weight")]
        weight: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CongestionModel {
    /// v = v_free * max(min_speed_factor, 1 - density_ratio)
    #[default]
    Greenshields,
    /// v = v_free / (1 + alpha * density_ratio^beta)
    Bpr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrafficModelConfig {
    #[serde(default)]
    pub model: CongestionModel,
    #[serde(default = "default_bpr_alpha")]
    pub bpr_alpha: f64,
    #[serde(default = "default_bpr_beta")]
    pub bpr_beta: f64,
    /// Congested speed never drops below this fraction of free-flow.
    #[serde(default = "default_min_speed_factor")]
    pub min_speed_factor: f64,
    /// Global standing background load on every edge (0..1 of storage).
    #[serde(default)]
    pub background_load: f64,
    /// Traffic signal cycle length and green share for signal-flagged edges.
    #[serde(default = "default_signal_cycle")]
    pub signal_cycle_s: f64,
    #[serde(default = "default_green_share")]
    pub signal_green_share: f64,
    /// Max reroute computations per simulated second (cost control).
    #[serde(default = "default_reroute_budget")]
    pub reroute_budget_per_s: u32,
}

impl Default for TrafficModelConfig {
    fn default() -> Self {
        Self {
            model: CongestionModel::default(),
            bpr_alpha: default_bpr_alpha(),
            bpr_beta: default_bpr_beta(),
            min_speed_factor: default_min_speed_factor(),
            background_load: 0.0,
            signal_cycle_s: default_signal_cycle(),
            signal_green_share: default_green_share(),
            reroute_budget_per_s: default_reroute_budget(),
        }
    }
}

fn default_bpr_alpha() -> f64 {
    1.0
}

fn default_bpr_beta() -> f64 {
    4.0
}

fn default_min_speed_factor() -> f64 {
    0.05
}

fn default_signal_cycle() -> f64 {
    60.0
}

fn default_green_share() -> f64 {
    0.55
}

fn default_reroute_budget() -> u32 {
    80
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PositionModel {
    /// Constant speed across each edge between its enter/exit timestamps.
    #[default]
    Linear,
    /// Accelerate from the edge entry, cruise, decelerate to the exit so
    /// interpolated positions have smooth speed ramps.
    Trapezoidal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimOutputConfig {
    /// Interval between stored agent-position frames.
    #[serde(default = "default_snapshot_interval")]
    pub frame_interval_s: f64,
    /// Max agents stored per frame (deterministic stride sample above this).
    #[serde(default = "default_frame_max_agents")]
    pub frame_max_agents: u32,
    /// Interval for per-edge congestion statistics bins.
    #[serde(default = "default_edge_bin")]
    pub edge_stats_interval_s: f64,
    /// Keep full per-agent trajectories in the result.
    #[serde(default = "default_true")]
    pub store_trajectories: bool,
    #[serde(default)]
    pub position_model: PositionModel,
}

impl Default for SimOutputConfig {
    fn default() -> Self {
        Self {
            frame_interval_s: default_snapshot_interval(),
            frame_max_agents: default_frame_max_agents(),
            edge_stats_interval_s: default_edge_bin(),
            store_trajectories: true,
            position_model: PositionModel::default(),
        }
    }
}

fn default_snapshot_interval() -> f64 {
    5.0
}

fn default_frame_max_agents() -> u32 {
    20_000
}

fn default_edge_bin() -> f64 {
    60.0
}

impl SimulationScenario {
    pub fn validate(&self) -> Result<()> {
        if self.scenario.id.trim().is_empty() {
            bail!("scenario.id must not be empty");
        }
        if self.fleets.is_empty() {
            bail!("scenario must define at least one fleet");
        }
        if !(self.time.tick_s > 0.0) || self.time.tick_s > 60.0 {
            bail!("time.tick_s must be in (0, 60]");
        }
        if !(self.time.duration_s > 0.0) {
            bail!("time.duration_s must be positive");
        }
        let mut fleet_ids = std::collections::BTreeSet::new();
        for fleet in &self.fleets {
            if fleet.fleet_id.trim().is_empty() {
                bail!("fleet_id must not be empty");
            }
            if !fleet_ids.insert(fleet.fleet_id.clone()) {
                bail!("duplicate fleet_id '{}'", fleet.fleet_id);
            }
            if fleet.profile_id.trim().is_empty() {
                bail!("fleet '{}' is missing profile_id", fleet.fleet_id);
            }
            for value in [
                ("overtake_eagerness", fleet.behavior.overtake_eagerness),
                ("reroute_eagerness", fleet.behavior.reroute_eagerness),
                ("jam_speed_threshold", fleet.behavior.jam_speed_threshold),
                (
                    "alternative_route_share",
                    fleet.behavior.alternative_route_share,
                ),
                ("heterogeneity", fleet.behavior.heterogeneity),
            ] {
                if !(0.0..=1.0).contains(&value.1) {
                    bail!(
                        "fleet '{}': behavior.{} must be within [0, 1]",
                        fleet.fleet_id,
                        value.0
                    );
                }
            }
            if fleet.speed.speed_multiplier_mean <= 0.0 {
                bail!(
                    "fleet '{}': speed.speed_multiplier_mean must be positive",
                    fleet.fleet_id
                );
            }
            if let EndpointDistribution::RandomBbox { bbox } = &fleet.demand.origins
                && (bbox[0] >= bbox[2] || bbox[1] >= bbox[3])
            {
                bail!("fleet '{}': origins bbox is degenerate", fleet.fleet_id);
            }
            if let EndpointDistribution::RandomBbox { bbox } = &fleet.demand.destinations
                && (bbox[0] >= bbox[2] || bbox[1] >= bbox[3])
            {
                bail!(
                    "fleet '{}': destinations bbox is degenerate",
                    fleet.fleet_id
                );
            }
        }
        let mut zone_ids = std::collections::BTreeSet::new();
        for zone in &self.zones {
            if zone.zone_id.trim().is_empty() {
                bail!("zone_id must not be empty");
            }
            if !zone_ids.insert(zone.zone_id.clone()) {
                bail!("duplicate zone_id '{}'", zone.zone_id);
            }
            if zone.polygon.len() < 3 {
                bail!("zone '{}' polygon needs at least 3 vertices", zone.zone_id);
            }
        }
        if !(self.traffic.signal_green_share > 0.0 && self.traffic.signal_green_share <= 1.0) {
            bail!("traffic.signal_green_share must be in (0, 1]");
        }
        if !(self.output.frame_interval_s > 0.0) {
            bail!("output.frame_interval_s must be positive");
        }
        if !(self.output.edge_stats_interval_s > 0.0) {
            bail!("output.edge_stats_interval_s must be positive");
        }
        Ok(())
    }
}

/// Load a scenario from YAML/TOML/JSON based on extension.
pub fn load_scenario(path: impl AsRef<Path>) -> Result<SimulationScenario> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading scenario file {}", path.display()))?;
    let scenario: SimulationScenario = match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "yml" | "yaml" => serde_yaml::from_str(&raw)
            .with_context(|| format!("parsing YAML scenario {}", path.display()))?,
        "toml" => toml::from_str(&raw)
            .with_context(|| format!("parsing TOML scenario {}", path.display()))?,
        "json" => serde_json::from_str(&raw)
            .with_context(|| format!("parsing JSON scenario {}", path.display()))?,
        other => bail!("unsupported scenario extension '{other}' (use yml/yaml/toml/json)"),
    };
    scenario.validate()?;
    Ok(scenario)
}
