//! netweevil-simulate: fast mesoscopic agent-based traffic simulation on the
//! shared routing network.
//!
//! Agents are dispatched per fleet (one travel mode / compiled profile each)
//! from configurable origin/destination distributions, routed through the
//! exact `PreparedRoutingEngine`, then moved tick-by-tick over the edge graph
//! with density-dependent speeds, edge storage/outflow capacities, traffic
//! signal gates, spillback queueing, and live congested rerouting.
//!
//! Runs stream status, agent-position frames, and per-edge congestion
//! statistics through a [`SimulationHandle`] while running, accept mid-run
//! [`SimulationCommand`]s (pause/resume, global speed factor, zone changes,
//! extra fleets), and finish with a reproducible [`SimulationResult`].

mod control;
mod demand;
mod engine;
mod network;
mod output;
mod rng;
mod scenario;
mod zones;

pub use control::{
    EdgeBin, EdgeBinStat, FleetStatus, Frame, SimRunState, SimulationCommand, SimulationHandle,
    SimulationStatus,
};
pub use demand::{DispatchReport, DispatchedAgent};
pub use engine::SimulationRunner;
pub use network::{SimNetwork, build_sim_network, haversine_m, network_bounds_bbox};
pub use output::{
    AgentPosition, AgentTrajectory, EdgeUsage, FleetSummary, SimulationResult, SimulationSummary,
    edge_bins_to_geojson, edge_usage_to_geojson, frame_to_geojson, frames_to_temporal_geojson,
    trajectory_position_at, trajectory_to_geojson, trapezoidal_distance,
};
pub use rng::SimRng;
pub use scenario::{
    BehaviorConfig, CongestionModel, DemandConfig, DepartureConfig, EndpointDistribution,
    ExplicitOdPair, FleetConfig, FleetSpeedConfig, PositionModel, ScenarioHeader, SimOutputConfig,
    SimulationScenario, TimeConfig, TrafficModelConfig, WeightedPoint, ZoneConfig, ZoneEffect,
    load_scenario,
};
pub use zones::{PreparedPolygon, PreparedZone, prepare_zones};
