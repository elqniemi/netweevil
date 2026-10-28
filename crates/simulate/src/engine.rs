//! Mesoscopic queue-based traffic engine.
//!
//! Agents traverse the directed edge graph with congestion-dependent speeds:
//! every edge has a storage capacity (PCU that physically fit) and an
//! outflow capacity (PCU/s). Speeds drop with density (Greenshields or BPR),
//! full downstream edges block transitions (spillback) when `no_collision`
//! is enabled, signal-flagged edges gate outflow on a fixed cycle, and
//! jammed agents reroute with live congested costs. The whole run is
//! deterministic for a given scenario seed.

use std::collections::{BTreeMap, BinaryHeap};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use rustc_hash::FxHashMap;

use anyhow::{Context, Result, bail};
use netweevil_core::TopologyBundle;
use netweevil_query::PreparedRoutingEngine;
use rayon::prelude::*;

use crate::control::{
    EdgeBin, EdgeBinStat, FleetStatus, Frame, SharedSimState, SimRunState, SimulationCommand,
    SimulationHandle, SimulationStatus,
};
use crate::demand::{
    AgentDraft, DispatchReport, DispatchedAgent, dispatch_fleet, plan_fleet, route_drafts,
};
use crate::network::{SimNetwork, build_sim_network, haversine_m, network_bounds_bbox};
use crate::output::{
    AgentTrajectory, EdgeUsage, FleetSummary, SimulationResult, SimulationSummary,
};
use crate::rng::{SimRng, SplitMix64};
use crate::scenario::{CongestionModel, FleetConfig, SimulationScenario};
use crate::zones::{PreparedZone, effects_at, prepare_zones};

const SPEED_FLOOR_MPS: f32 = 0.1;
const MAX_EDGE_CROSSINGS_PER_TICK: usize = 256;
const REROUTE_CHECK_INTERVAL_S: f64 = 5.0;
const ASTAR_MAX_SETTLED: usize = 120_000;
/// Sim-time window of departures routed ahead of the clock. Agents are
/// planned up front (cheap sampling) but routed lazily in these windows so
/// the simulation starts immediately instead of routing the whole demand
/// before tick 0.
const DISPATCH_HORIZON_S: f64 = 300.0;
/// How often the dispatcher routes the next departure window.
const DISPATCH_INTERVAL_S: f64 = 60.0;
/// Backoff between repeated forced reroute attempts (blocked next edge),
/// so permanently stuck agents don't burn an A* search every check round.
const FORCED_REROUTE_RETRY_S: f64 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentStatus {
    Pending,
    Active,
    Arrived,
    NeverDeparted,
}

struct Agent {
    fleet: u16,
    obey_signals: bool,
    no_collision: bool,
    desired_mult: f32,
    max_speed_mps: f32,
    overtake: f32,
    reroute_eagerness: f32,
    jam_threshold: f32,
    reroute_cooldown_s: f32,
    pcu: f32,
    depart_s: f64,
    origin: [f64; 2],
    destination: [f64; 2],
    route: Vec<u32>,
    route_pos: u32,
    pos_m: f32,
    speed_mps: f32,
    wait_s: f32,
    last_reroute_s: f64,
    force_reroute: bool,
    arrive_s: f64,
    status: AgentStatus,
    reroutes: u16,
    freeflow_time_s: f32,
    distance_m: f32,
    traj_edges: Vec<u32>,
    traj_enter_s: Vec<f32>,
}

impl Agent {
    fn from_dispatch(dispatch: DispatchedAgent) -> Self {
        Self {
            fleet: dispatch.fleet,
            obey_signals: dispatch.obey_signals,
            no_collision: dispatch.no_collision,
            desired_mult: dispatch.desired_speed_mult,
            max_speed_mps: dispatch.max_speed_mps,
            overtake: dispatch.overtake_eagerness,
            reroute_eagerness: dispatch.reroute_eagerness,
            jam_threshold: dispatch.jam_speed_threshold,
            reroute_cooldown_s: dispatch.reroute_cooldown_s,
            pcu: dispatch.pcu,
            depart_s: dispatch.depart_s,
            origin: dispatch.origin,
            destination: dispatch.destination,
            route: dispatch.route,
            route_pos: 0,
            pos_m: 0.0,
            speed_mps: 0.0,
            wait_s: 0.0,
            last_reroute_s: f64::NEG_INFINITY,
            force_reroute: false,
            arrive_s: f64::NAN,
            status: AgentStatus::Pending,
            reroutes: 0,
            freeflow_time_s: 0.0,
            distance_m: 0.0,
            traj_edges: Vec::new(),
            traj_enter_s: Vec::new(),
        }
    }

    /// Inert placeholder for a planned agent whose departure never fell
    /// inside the simulated window, so it was never routed.
    fn never_departed(fleet: u16, draft: &AgentDraft) -> Self {
        Self {
            fleet,
            obey_signals: false,
            no_collision: false,
            desired_mult: 1.0,
            max_speed_mps: f32::INFINITY,
            overtake: 0.0,
            reroute_eagerness: 0.0,
            jam_threshold: 0.0,
            reroute_cooldown_s: 0.0,
            pcu: 0.0,
            depart_s: draft.depart_s,
            origin: draft.origin,
            destination: draft.destination,
            route: Vec::new(),
            route_pos: 0,
            pos_m: 0.0,
            speed_mps: 0.0,
            wait_s: 0.0,
            last_reroute_s: f64::NEG_INFINITY,
            force_reroute: false,
            arrive_s: f64::NAN,
            status: AgentStatus::NeverDeparted,
            reroutes: 0,
            freeflow_time_s: 0.0,
            distance_m: 0.0,
            traj_edges: Vec::new(),
            traj_enter_s: Vec::new(),
        }
    }

    fn current_edge(&self) -> u32 {
        self.route[self.route_pos as usize]
    }

    fn on_last_edge(&self) -> bool {
        self.route_pos as usize + 1 >= self.route.len()
    }
}

struct FleetRuntime {
    config: FleetConfig,
    engine: Arc<PreparedRoutingEngine>,
    speed_table: usize,
    mode_bit: u16,
    mode_name: String,
    report: DispatchReport,
    arrived: u32,
    reroutes: u64,
    travel_time_sum: f64,
    distance_sum: f64,
    delay_sum: f64,
}

/// Per-edge dynamic state. Dense arrays sized to the edge count; per-tick
/// caches use tick stamps so nothing is cleared between ticks.
struct EdgeDynamics {
    occupancy_pcu: Vec<f32>,
    zone_speed: Vec<f32>,
    zone_capacity: Vec<f32>,
    background_pcu: Vec<f32>,
    closed_mask: Vec<u16>,
    cache_tick: Vec<u32>,
    cache_factor: Vec<f32>,
    cache_ratio: Vec<f32>,
    exit_tick: Vec<u32>,
    exits_pcu: Vec<f32>,
    traversals: Vec<u32>,
    vehicle_seconds: Vec<f32>,
    congested_seconds: Vec<f32>,
    signal_offset_s: Vec<f32>,
    bin_acc: Vec<BinAcc>,
    /// Edges with activity in the current stat bin (each pushed once).
    bin_touched: Vec<u32>,
}

#[derive(Default, Clone, Copy)]
struct BinAcc {
    occ_sum: f32,
    factor_sum: f32,
    ticks: u32,
    entered: u32,
}

impl EdgeDynamics {
    fn new(network: &SimNetwork, signal_cycle_s: f64, seed: u64) -> Self {
        let edges = network.edge_count();
        let mut signal_offset_s = vec![0.0f32; edges];
        let mut mix = SplitMix64::new(seed ^ 0x51_67_4a_11);
        for (edge, offset) in signal_offset_s.iter_mut().enumerate() {
            if network.edge_signal[edge] {
                let hash = mix.next_u64();
                *offset = (hash % (signal_cycle_s.max(1.0) as u64 * 1000)) as f32 / 1000.0;
            }
        }
        Self {
            occupancy_pcu: vec![0.0; edges],
            zone_speed: vec![1.0; edges],
            zone_capacity: vec![1.0; edges],
            background_pcu: vec![0.0; edges],
            closed_mask: vec![0; edges],
            cache_tick: vec![u32::MAX; edges],
            cache_factor: vec![1.0; edges],
            cache_ratio: vec![0.0; edges],
            exit_tick: vec![u32::MAX; edges],
            exits_pcu: vec![0.0; edges],
            traversals: vec![0; edges],
            vehicle_seconds: vec![0.0; edges],
            congested_seconds: vec![0.0; edges],
            signal_offset_s,
            bin_acc: vec![BinAcc::default(); edges],
            bin_touched: Vec::new(),
        }
    }

    /// Dense per-edge bin accumulator; records first-touch per bin so the
    /// flush only visits edges with activity.
    fn bin_acc_mut(&mut self, edge: usize) -> &mut BinAcc {
        let untouched = {
            let acc = &self.bin_acc[edge];
            acc.ticks == 0 && acc.entered == 0
        };
        if untouched {
            self.bin_touched.push(edge as u32);
        }
        &mut self.bin_acc[edge]
    }

    fn apply_zones(
        &mut self,
        network: &SimNetwork,
        zones: &[PreparedZone],
        global_background: f64,
    ) {
        for edge in 0..network.edge_count() {
            let (lon, lat) = network.edge_midpoint(edge);
            let effects = effects_at(zones, lon, lat);
            self.zone_speed[edge] = effects.speed_factor;
            self.zone_capacity[edge] = effects.capacity_factor.max(0.01);
            let load = (effects.background_load as f64 + global_background).clamp(0.0, 1.0);
            self.background_pcu[edge] =
                load as f32 * network.edge_storage_pcu[edge] * self.zone_capacity[edge];
            self.closed_mask[edge] = effects.closed_mode_mask;
        }
    }
}

pub struct SimulationRunner {
    scenario: SimulationScenario,
    network: Arc<SimNetwork>,
    profile_order: Vec<String>,
    profiles: BTreeMap<String, Arc<PreparedRoutingEngine>>,
    fleets: Vec<FleetRuntime>,
    zones: Vec<PreparedZone>,
    shared: Arc<SharedSimState>,
}

impl SimulationRunner {
    /// Build a runner. `profiles` must contain every profile referenced by a
    /// fleet; extra profiles are also loaded into the network so mid-run
    /// `SpawnFleet` commands can use them.
    pub fn new(
        scenario: SimulationScenario,
        topology: Arc<TopologyBundle>,
        profiles: BTreeMap<String, Arc<PreparedRoutingEngine>>,
    ) -> Result<Self> {
        scenario.validate()?;
        for fleet in &scenario.fleets {
            if !profiles.contains_key(&fleet.profile_id) {
                bail!(
                    "fleet '{}' references profile '{}' which is not loaded (available: {})",
                    fleet.fleet_id,
                    fleet.profile_id,
                    profiles.keys().cloned().collect::<Vec<_>>().join(", ")
                );
            }
        }

        let profile_order: Vec<String> = profiles.keys().cloned().collect();
        let metrics: Vec<_> = profile_order
            .iter()
            .map(|id| profiles[id].metrics_arc())
            .collect();
        let network = build_sim_network(topology.as_ref(), &metrics)
            .context("building simulation network")?;

        let mut fleets = Vec::with_capacity(scenario.fleets.len());
        for fleet_config in &scenario.fleets {
            fleets.push(make_fleet_runtime(
                fleet_config.clone(),
                &profiles,
                &profile_order,
            )?);
        }

        let zones = prepare_zones(&scenario.zones);
        let status = SimulationStatus::initial(
            scenario.time.duration_s,
            scenario.time.tick_s,
            scenario.output.frame_interval_s,
            scenario.output.edge_stats_interval_s,
        );
        let shared = SharedSimState::new(status);
        let _ = shared.network.set(network.clone());

        Ok(Self {
            scenario,
            network,
            profile_order,
            profiles,
            fleets,
            zones,
            shared,
        })
    }

    pub fn handle(&self) -> SimulationHandle {
        SimulationHandle {
            shared: self.shared.clone(),
        }
    }

    pub fn network(&self) -> Arc<SimNetwork> {
        self.network.clone()
    }

    /// Run the simulation to completion (blocking). Status, frames, and edge
    /// statistics stream into the shared handle while running.
    pub fn run(mut self) -> Result<SimulationResult> {
        let wall_start = Instant::now();
        let result = self.run_inner(wall_start);
        match &result {
            Ok(_) => {}
            Err(error) => {
                let mut status = self.shared.status.write().expect("status lock");
                status.state = SimRunState::Failed;
                status.message = format!("simulation failed: {error:#}");
                status.wall_time_ms = wall_start.elapsed().as_millis() as u64;
            }
        }
        result
    }

    fn run_inner(&mut self, wall_start: Instant) -> Result<SimulationResult> {
        let seed = self.scenario.scenario.seed;
        let tick_s = self.scenario.time.tick_s;
        let duration_s = self.scenario.time.duration_s;
        let bbox = network_bounds_bbox(&self.network);

        self.update_status(|status| {
            status.state = SimRunState::Dispatching;
            status.message = "planning demand".to_string();
        });

        // Plan every fleet's demand up front (cheap sampling only); the
        // expensive routing happens lazily in departure-time windows so the
        // simulation starts immediately.
        let mut draft_queues: Vec<Vec<AgentDraft>> = Vec::with_capacity(self.fleets.len());
        let mut next_agent_id: u32 = 0;
        for (fleet_index, fleet) in self.fleets.iter().enumerate() {
            let mut drafts = plan_fleet(
                &fleet.config,
                fleet_index as u16,
                &fleet.engine,
                bbox,
                &self.zones,
                seed,
                next_agent_id,
                0.0,
            )
            .with_context(|| format!("planning fleet '{}'", fleet.config.fleet_id))?;
            next_agent_id = next_agent_id.saturating_add(drafts.len() as u32);
            // Latest departures first so routed windows split off the back.
            drafts.sort_by(|a, b| {
                b.depart_s
                    .partial_cmp(&a.depart_s)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            draft_queues.push(drafts);
        }
        let planned_total: usize = draft_queues.iter().map(Vec::len).sum();

        let mut dynamics =
            EdgeDynamics::new(&self.network, self.scenario.traffic.signal_cycle_s, seed);
        dynamics.apply_zones(
            &self.network,
            &self.zones,
            self.scenario.traffic.background_load,
        );

        // Pending agents sorted by departure (latest first so we pop the back).
        let mut agents: Vec<Agent> = Vec::new();
        let mut pending: Vec<u32> = Vec::new();
        let mut spawn_retry: Vec<u32> = Vec::new();
        let mut active: Vec<u32> = Vec::new();

        // Route the first departure window before tick 0.
        self.update_status(|status| {
            status.state = SimRunState::Dispatching;
            status.message = format!("routing initial departures ({planned_total} agents planned)");
        });
        self.dispatch_window(
            DISPATCH_HORIZON_S,
            &mut draft_queues,
            &mut agents,
            &mut pending,
            bbox,
        )?;
        let mut next_dispatch_t = DISPATCH_INTERVAL_S;

        let mut global_speed_factor = 1.0f64;
        let mut event_rng = SimRng::derive(seed, 0xE7E27);
        let mut reroute_candidates: Vec<u32> = Vec::new();

        let frame_interval = self.scenario.output.frame_interval_s;
        let bin_interval = self.scenario.output.edge_stats_interval_s;
        let mut next_frame_t = 0.0f64;
        let mut bin_start_t = 0.0f64;
        let reroute_check_ticks = (REROUTE_CHECK_INTERVAL_S / tick_s).round().max(1.0) as u64;
        let reroute_budget_per_round = (self.scenario.traffic.reroute_budget_per_s as f64
            * REROUTE_CHECK_INTERVAL_S)
            .round()
            .max(1.0) as usize;

        self.update_status(|status| {
            status.state = SimRunState::Running;
            status.agents_total = planned_total as u32;
            status.message = "running".to_string();
        });
        self.publish_fleet_status(&agents, &active);

        let total_ticks = (duration_s / tick_s).ceil() as u64;
        let mut tick: u64 = 0;
        let mut last_status_update = Instant::now();
        let mut still_active: Vec<u32> = Vec::new();

        while tick <= total_ticks {
            let t = tick as f64 * tick_s;

            // Drain control commands; block here while paused.
            loop {
                let commands: Vec<SimulationCommand> = {
                    let mut inbox = self.shared.commands.lock().expect("command lock");
                    std::mem::take(&mut *inbox)
                };
                for command in commands {
                    self.apply_command(
                        command,
                        t,
                        &mut agents,
                        &mut pending,
                        &mut dynamics,
                        &mut global_speed_factor,
                        bbox,
                        seed,
                    );
                }
                if self.shared.cancelled.load(Ordering::Relaxed) {
                    break;
                }
                let paused = {
                    let status = self.shared.status.read().expect("status lock");
                    status.state == SimRunState::Paused
                };
                if !paused {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
                self.update_status(|status| {
                    status.wall_time_ms = wall_start.elapsed().as_millis() as u64;
                });
            }
            if self.shared.cancelled.load(Ordering::Relaxed) {
                break;
            }

            // Route the next departure window of planned agents.
            if t + 1e-9 >= next_dispatch_t && draft_queues.iter().any(|queue| !queue.is_empty()) {
                self.dispatch_window(
                    t + DISPATCH_HORIZON_S,
                    &mut draft_queues,
                    &mut agents,
                    &mut pending,
                    bbox,
                )?;
                next_dispatch_t = t + DISPATCH_INTERVAL_S;
            }

            // Spawn agents whose departure time has come.
            let mut retry = std::mem::take(&mut spawn_retry);
            for agent_index in retry.drain(..) {
                self.try_spawn(
                    agent_index,
                    t,
                    &mut agents,
                    &mut active,
                    &mut spawn_retry,
                    &mut dynamics,
                );
            }
            while let Some(&agent_index) = pending.last() {
                if agents[agent_index as usize].depart_s > t {
                    break;
                }
                pending.pop();
                self.try_spawn(
                    agent_index,
                    t,
                    &mut agents,
                    &mut active,
                    &mut spawn_retry,
                    &mut dynamics,
                );
            }

            // Move active agents.
            let check_reroutes = tick.is_multiple_of(reroute_check_ticks);
            still_active.clear();
            still_active.reserve(active.len());
            for &agent_index in &active {
                let arrived = self.move_agent(
                    agent_index,
                    t,
                    tick_s,
                    tick as u32,
                    global_speed_factor,
                    &mut agents,
                    &mut dynamics,
                    check_reroutes,
                    &mut reroute_candidates,
                    &mut event_rng,
                );
                if arrived {
                    let agent = &agents[agent_index as usize];
                    let fleet = &mut self.fleets[agent.fleet as usize];
                    fleet.arrived += 1;
                    let travel_time = agent.arrive_s - agent.depart_s;
                    fleet.travel_time_sum += travel_time;
                    fleet.distance_sum += agent.distance_m as f64;
                    fleet.delay_sum += (travel_time - agent.freeflow_time_s as f64).max(0.0);
                } else {
                    still_active.push(agent_index);
                }
            }
            std::mem::swap(&mut active, &mut still_active);

            // Reroute jammed agents with live congested costs (budgeted).
            if check_reroutes && !reroute_candidates.is_empty() {
                let candidates = std::mem::take(&mut reroute_candidates);
                self.process_reroutes(
                    t,
                    candidates,
                    reroute_budget_per_round,
                    &mut agents,
                    &dynamics,
                );
            }

            // Frames.
            while t + 1e-9 >= next_frame_t {
                let frame = self.capture_frame(next_frame_t.min(t.max(0.0)), &agents, &active);
                let mut live = self.shared.live.write().expect("live store lock");
                live.frames.push(Arc::new(frame));
                let frames_available = live.frames.len() as u32;
                drop(live);
                self.update_status(|status| status.frames_available = frames_available);
                next_frame_t += frame_interval;
            }

            // Edge stat bins.
            if t + 1e-9 >= bin_start_t + bin_interval {
                let bin = flush_bin(&mut dynamics, bin_start_t, t, tick_s);
                let mut live = self.shared.live.write().expect("live store lock");
                live.edge_bins.push(Arc::new(bin));
                let bins_available = live.edge_bins.len() as u32;
                drop(live);
                self.update_status(|status| status.edge_bins_available = bins_available);
                bin_start_t = t;
            }

            // Status heartbeat.
            if last_status_update.elapsed().as_millis() >= 200 {
                let active_count = active.len() as u32;
                let unrouted: usize = draft_queues.iter().map(Vec::len).sum();
                let pending_count = (pending.len() + spawn_retry.len() + unrouted) as u32;
                let arrived: u32 = self.fleets.iter().map(|fleet| fleet.arrived).sum();
                let wall = wall_start.elapsed().as_millis() as u64;
                self.update_status(|status| {
                    status.sim_time_s = t;
                    status.agents_active = active_count;
                    status.agents_pending = pending_count;
                    status.agents_arrived = arrived;
                    status.global_speed_factor = global_speed_factor;
                    status.wall_time_ms = wall;
                });
                self.publish_fleet_status(&agents, &active);
                last_status_update = Instant::now();
            }

            if self.scenario.time.stop_when_all_arrived
                && active.is_empty()
                && pending.is_empty()
                && spawn_retry.is_empty()
                && draft_queues.iter().all(Vec::is_empty)
                && tick > 0
            {
                break;
            }
            tick += 1;
        }

        let final_t = (tick as f64 * tick_s).min(duration_s);
        let cancelled = self.shared.cancelled.load(Ordering::Relaxed);

        // Flush the trailing edge-stat bin.
        if final_t > bin_start_t + 1e-9 {
            let bin = flush_bin(&mut dynamics, bin_start_t, final_t, tick_s);
            let mut live = self.shared.live.write().expect("live store lock");
            live.edge_bins.push(Arc::new(bin));
        }

        // Pending agents never made it onto the network.
        for agent_index in pending.iter().chain(spawn_retry.iter()) {
            agents[*agent_index as usize].status = AgentStatus::NeverDeparted;
        }

        // Planned agents whose departure window never arrived were never
        // routed; record them as never-departed without paying for routing.
        for (fleet_index, queue) in draft_queues.into_iter().enumerate() {
            self.fleets[fleet_index].report.requested += queue.len() as u32;
            for draft in queue {
                agents.push(Agent::never_departed(fleet_index as u16, &draft));
            }
        }

        let result = self.build_result(
            agents,
            &active,
            &dynamics,
            final_t,
            wall_start.elapsed().as_millis() as u64,
            cancelled,
        );

        let frames_available;
        let bins_available;
        {
            let live = self.shared.live.read().expect("live store lock");
            frames_available = live.frames.len() as u32;
            bins_available = live.edge_bins.len() as u32;
        }
        self.update_status(|status| {
            status.state = if cancelled {
                SimRunState::Cancelled
            } else {
                SimRunState::Completed
            };
            status.sim_time_s = final_t;
            status.agents_active = result.summary.agents_unfinished;
            status.agents_arrived = result.summary.agents_arrived;
            status.agents_pending = 0;
            status.frames_available = frames_available;
            status.edge_bins_available = bins_available;
            status.wall_time_ms = result.summary.wall_time_ms;
            status.message = if cancelled {
                "cancelled".to_string()
            } else {
                "completed".to_string()
            };
        });

        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_command(
        &mut self,
        command: SimulationCommand,
        t: f64,
        agents: &mut Vec<Agent>,
        pending: &mut Vec<u32>,
        dynamics: &mut EdgeDynamics,
        global_speed_factor: &mut f64,
        bbox: [f64; 4],
        seed: u64,
    ) {
        match command {
            SimulationCommand::Pause => {
                self.update_status(|status| {
                    if status.state == SimRunState::Running {
                        status.state = SimRunState::Paused;
                        status.message = "paused".to_string();
                    }
                });
            }
            SimulationCommand::Resume => {
                self.update_status(|status| {
                    if status.state == SimRunState::Paused {
                        status.state = SimRunState::Running;
                        status.message = "running".to_string();
                    }
                });
            }
            SimulationCommand::Cancel => {
                self.shared.cancelled.store(true, Ordering::Relaxed);
            }
            SimulationCommand::SetGlobalSpeedFactor { factor } => {
                *global_speed_factor = factor.clamp(0.05, 5.0);
            }
            SimulationCommand::AddZone { zone } => {
                self.zones
                    .retain(|existing| existing.zone_id != zone.zone_id);
                self.zones
                    .extend(prepare_zones(std::slice::from_ref(&zone)));
                self.scenario.zones.retain(|z| z.zone_id != zone.zone_id);
                self.scenario.zones.push(zone);
                dynamics.apply_zones(
                    &self.network,
                    &self.zones,
                    self.scenario.traffic.background_load,
                );
            }
            SimulationCommand::RemoveZone { zone_id } => {
                self.zones.retain(|zone| zone.zone_id != zone_id);
                self.scenario.zones.retain(|zone| zone.zone_id != zone_id);
                dynamics.apply_zones(
                    &self.network,
                    &self.zones,
                    self.scenario.traffic.background_load,
                );
            }
            SimulationCommand::SpawnFleet { fleet } => {
                match make_fleet_runtime(fleet, &self.profiles, &self.profile_order) {
                    Ok(mut runtime) => {
                        let fleet_index = self.fleets.len() as u16;
                        match dispatch_fleet(
                            &runtime.config,
                            fleet_index,
                            &runtime.engine,
                            bbox,
                            &self.zones,
                            seed ^ (agents.len() as u64) << 17,
                            agents.len() as u32,
                            t,
                        ) {
                            Ok((dispatched, report)) => {
                                let added = report.requested;
                                runtime.report = report;
                                let mut new_indices: Vec<u32> = Vec::new();
                                for dispatch in dispatched {
                                    new_indices.push(agents.len() as u32);
                                    agents.push(Agent::from_dispatch(dispatch));
                                }
                                pending.extend(new_indices);
                                pending.sort_by(|a, b| {
                                    agents[*b as usize]
                                        .depart_s
                                        .partial_cmp(&agents[*a as usize].depart_s)
                                        .unwrap_or(std::cmp::Ordering::Equal)
                                        .then(b.cmp(a))
                                });
                                self.fleets.push(runtime);
                                self.update_status(|status| status.agents_total += added);
                            }
                            Err(error) => {
                                self.update_status(|status| {
                                    status.message =
                                        format!("spawn_fleet dispatch failed: {error:#}");
                                });
                            }
                        }
                    }
                    Err(error) => {
                        self.update_status(|status| {
                            status.message = format!("spawn_fleet rejected: {error:#}");
                        });
                    }
                }
            }
        }
    }

    /// Route every planned draft departing before `horizon_end_s` and queue
    /// the resulting agents for spawning. Draft queues are sorted with the
    /// latest departure first, so each window splits off the queue tail.
    fn dispatch_window(
        &mut self,
        horizon_end_s: f64,
        draft_queues: &mut [Vec<AgentDraft>],
        agents: &mut Vec<Agent>,
        pending: &mut Vec<u32>,
        bbox: [f64; 4],
    ) -> Result<()> {
        let mut appended = false;
        for (fleet_index, queue) in draft_queues.iter_mut().enumerate() {
            if self.shared.cancelled.load(Ordering::Relaxed) {
                break;
            }
            let split = queue.partition_point(|draft| draft.depart_s > horizon_end_s);
            if split == queue.len() {
                continue;
            }
            let mut window = queue.split_off(split);
            let fleet = &self.fleets[fleet_index];
            let (dispatched, report) = route_drafts(
                &fleet.config,
                fleet_index as u16,
                &fleet.engine,
                bbox,
                &self.zones,
                &mut window,
            )
            .with_context(|| format!("dispatching fleet '{}'", fleet.config.fleet_id))?;
            merge_dispatch_report(&mut self.fleets[fleet_index].report, report);
            for dispatch in dispatched {
                pending.push(agents.len() as u32);
                agents.push(Agent::from_dispatch(dispatch));
                appended = true;
            }
        }
        if appended {
            pending.sort_by(|a, b| {
                agents[*b as usize]
                    .depart_s
                    .partial_cmp(&agents[*a as usize].depart_s)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.cmp(a))
            });
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn try_spawn(
        &mut self,
        agent_index: u32,
        t: f64,
        agents: &mut [Agent],
        active: &mut Vec<u32>,
        spawn_retry: &mut Vec<u32>,
        dynamics: &mut EdgeDynamics,
    ) {
        let agent = &mut agents[agent_index as usize];
        if agent.route.is_empty() {
            agent.status = AgentStatus::NeverDeparted;
            return;
        }
        let first_edge = agent.route[0] as usize;
        let mode_bit = self.fleets[agent.fleet as usize].mode_bit;
        if dynamics.closed_mask[first_edge] & mode_bit != 0 {
            spawn_retry.push(agent_index);
            return;
        }
        if agent.no_collision {
            let storage =
                self.network.edge_storage_pcu[first_edge] * dynamics.zone_capacity[first_edge];
            let load = dynamics.occupancy_pcu[first_edge] + dynamics.background_pcu[first_edge];
            if load + agent.pcu > storage {
                spawn_retry.push(agent_index);
                return;
            }
        }
        dynamics.occupancy_pcu[first_edge] += agent.pcu;
        dynamics.traversals[first_edge] += 1;
        dynamics.bin_acc_mut(first_edge).entered += 1;
        agent.status = AgentStatus::Active;
        agent.route_pos = 0;
        agent.pos_m = 0.0;
        agent.traj_edges.push(first_edge as u32);
        agent.traj_enter_s.push(t as f32);
        active.push(agent_index);
    }

    /// Returns true when the agent arrived this tick.
    #[allow(clippy::too_many_arguments)]
    fn move_agent(
        &mut self,
        agent_index: u32,
        t: f64,
        dt: f64,
        tick: u32,
        global_speed_factor: f64,
        agents: &mut [Agent],
        dynamics: &mut EdgeDynamics,
        check_reroutes: bool,
        reroute_candidates: &mut Vec<u32>,
        event_rng: &mut SimRng,
    ) -> bool {
        let agent = &mut agents[agent_index as usize];
        let fleet = &self.fleets[agent.fleet as usize];
        let speed_table = &self.network.fleet_freeflow_mps[fleet.speed_table];
        let mode_bit = fleet.mode_bit;

        let mut edge = agent.current_edge() as usize;
        let (factor, ratio) = edge_factor(
            &self.network,
            dynamics,
            edge,
            tick,
            dt,
            &self.scenario.traffic,
        );

        let freeflow = speed_table[edge];
        let v_free = if freeflow > 0.0 {
            freeflow * dynamics.zone_speed[edge] * global_speed_factor as f32
        } else {
            // The agent is on an edge its profile cannot traverse (should not
            // happen via routing, but stay robust): crawl across.
            1.0
        };
        // Overtaking recovers part of the desired speed while space remains.
        let space = (1.0 - ratio).max(0.0);
        let factor_agent = factor + agent.overtake * (1.0 - factor) * space * space;
        let v_desired = (v_free * agent.desired_mult).min(agent.max_speed_mps);
        let mut v = (v_desired * factor_agent).max(SPEED_FLOOR_MPS);

        let mut remaining = v * dt as f32;
        let mut blocked = false;
        let mut crossings = 0usize;
        let mut arrived = false;

        loop {
            let length = self.network.edge_length_m[edge];
            if agent.pos_m + remaining < length {
                agent.pos_m += remaining;
                agent.distance_m += remaining;
                break;
            }
            // Time at which the edge end is reached within this tick.
            let distance_to_end = length - agent.pos_m;
            let time_to_end = (dt as f32 - (remaining - distance_to_end) / v).max(0.0);

            if agent.on_last_edge() {
                // Arrival at the end of the final edge.
                dynamics.occupancy_pcu[edge] = (dynamics.occupancy_pcu[edge] - agent.pcu).max(0.0);
                agent.distance_m += distance_to_end;
                if speed_table[edge] > 0.0 {
                    // The whole final edge was traversed (earlier ticks
                    // advanced pos_m without crediting free-flow time).
                    agent.freeflow_time_s += length / speed_table[edge];
                }
                agent.arrive_s = t + time_to_end as f64;
                agent.status = AgentStatus::Arrived;
                agent.speed_mps = 0.0;
                arrived = true;
                break;
            }

            let next = agent.route[agent.route_pos as usize + 1] as usize;

            // Mid-route closure (zone added after dispatch): force reroute.
            if dynamics.closed_mask[next] & mode_bit != 0 {
                agent.pos_m = length;
                agent.wait_s += dt as f32;
                agent.force_reroute = true;
                blocked = true;
                break;
            }
            // Traffic signal gate at the end of the current edge.
            if agent.obey_signals && signal_red(&self.network, dynamics, edge, t, &self.scenario) {
                agent.pos_m = length;
                agent.wait_s += dt as f32;
                blocked = true;
                break;
            }
            // Outflow capacity of the current edge.
            let flow_cap =
                self.network.edge_flow_pcu_s[edge] * dynamics.zone_capacity[edge] * dt as f32;
            let exited = if dynamics.exit_tick[edge] == tick {
                dynamics.exits_pcu[edge]
            } else {
                0.0
            };
            if exited + agent.pcu > flow_cap.max(agent.pcu) {
                agent.pos_m = length;
                agent.wait_s += dt as f32;
                blocked = true;
                break;
            }
            // Storage of the next edge (spillback).
            if agent.no_collision {
                let storage = self.network.edge_storage_pcu[next] * dynamics.zone_capacity[next];
                let load = dynamics.occupancy_pcu[next] + dynamics.background_pcu[next];
                if load + agent.pcu > storage {
                    agent.pos_m = length;
                    agent.wait_s += dt as f32;
                    blocked = true;
                    break;
                }
            }

            // Transition.
            remaining -= distance_to_end;
            dynamics.occupancy_pcu[edge] = (dynamics.occupancy_pcu[edge] - agent.pcu).max(0.0);
            dynamics.occupancy_pcu[next] += agent.pcu;
            if dynamics.exit_tick[edge] == tick {
                dynamics.exits_pcu[edge] += agent.pcu;
            } else {
                dynamics.exit_tick[edge] = tick;
                dynamics.exits_pcu[edge] = agent.pcu;
            }
            dynamics.traversals[next] += 1;
            dynamics.bin_acc_mut(next).entered += 1;
            agent.distance_m += distance_to_end;
            if speed_table[edge] > 0.0 {
                agent.freeflow_time_s += length / speed_table[edge];
            }
            let cross_t = t + time_to_end as f64;
            agent.route_pos += 1;
            agent.pos_m = 0.0;
            agent.traj_edges.push(next as u32);
            agent.traj_enter_s.push(cross_t as f32);
            edge = next;

            // Recompute pace on the new edge for the remaining tick time.
            let (next_factor, next_ratio) = edge_factor(
                &self.network,
                dynamics,
                edge,
                tick,
                dt,
                &self.scenario.traffic,
            );
            let next_free = if speed_table[edge] > 0.0 {
                speed_table[edge] * dynamics.zone_speed[edge] * global_speed_factor as f32
            } else {
                1.0
            };
            let next_space = (1.0 - next_ratio).max(0.0);
            let next_factor_agent =
                next_factor + agent.overtake * (1.0 - next_factor) * next_space * next_space;
            let next_v = ((next_free * agent.desired_mult).min(agent.max_speed_mps)
                * next_factor_agent)
                .max(SPEED_FLOOR_MPS);
            remaining = remaining / v * next_v;
            v = next_v;

            crossings += 1;
            if crossings >= MAX_EDGE_CROSSINGS_PER_TICK {
                break;
            }
        }

        if arrived {
            return true;
        }

        let agent = &mut agents[agent_index as usize];
        if !blocked {
            agent.wait_s = 0.0;
            agent.speed_mps = v;
        } else {
            agent.speed_mps = 0.0;
        }

        // Jam detection for rerouting.
        if check_reroutes && !agent.on_last_edge() {
            let slow = factor < agent.jam_threshold;
            let waiting = agent.wait_s > REROUTE_CHECK_INTERVAL_S as f32;
            let cooldown_over = t - agent.last_reroute_s >= agent.reroute_cooldown_s as f64;
            // Forced reroutes (blocked next edge) skip the regular cooldown
            // but still back off between attempts: a stuck agent must not
            // pay for an A* search every check round.
            let forced_ready = agent.force_reroute
                && (agent.last_reroute_s == f64::NEG_INFINITY
                    || t - agent.last_reroute_s >= FORCED_REROUTE_RETRY_S);
            if forced_ready
                || (!agent.force_reroute
                    && (slow || waiting)
                    && cooldown_over
                    && event_rng.chance(agent.reroute_eagerness as f64))
            {
                reroute_candidates.push(agent_index);
            }
        }
        false
    }

    /// Recompute routes for jammed agents using live congested costs. The
    /// searches only read `dynamics`, so they run in parallel; route swaps
    /// are applied sequentially in candidate order to stay deterministic.
    fn process_reroutes(
        &mut self,
        t: f64,
        candidates: Vec<u32>,
        budget: usize,
        agents: &mut [Agent],
        dynamics: &EdgeDynamics,
    ) {
        struct RerouteJob {
            agent_index: u32,
            current: u32,
            goal: u32,
            speed_table: usize,
            mode_bit: u16,
            forced: bool,
        }

        let mut jobs: Vec<RerouteJob> = Vec::new();
        for agent_index in candidates {
            if jobs.len() >= budget {
                break;
            }
            let agent = &mut agents[agent_index as usize];
            if agent.status != AgentStatus::Active || agent.on_last_edge() {
                agent.force_reroute = false;
                continue;
            }
            let fleet = &self.fleets[agent.fleet as usize];
            jobs.push(RerouteJob {
                agent_index,
                current: agent.current_edge(),
                goal: *agent.route.last().expect("route not empty"),
                speed_table: fleet.speed_table,
                mode_bit: fleet.mode_bit,
                forced: agent.force_reroute,
            });
        }

        let network = &self.network;
        let traffic = &self.scenario.traffic;
        let new_paths: Vec<Option<Vec<u32>>> = jobs
            .par_iter()
            .map_init(AstarScratch::default, |scratch, job| {
                astar_route(
                    network,
                    dynamics,
                    traffic,
                    job.speed_table,
                    job.mode_bit,
                    job.current,
                    job.goal,
                    scratch,
                )
            })
            .collect();

        for (job, new_path) in jobs.into_iter().zip(new_paths) {
            let agent = &mut agents[job.agent_index as usize];
            agent.force_reroute = false;
            agent.last_reroute_s = t;

            let Some(new_path) = new_path else {
                continue;
            };

            // Keep the old route unless the new one is meaningfully better
            // (forced reroutes always switch — the old route is blocked).
            if !job.forced {
                let old_cost = remaining_route_cost(
                    network,
                    dynamics,
                    traffic,
                    job.speed_table,
                    &agent.route[agent.route_pos as usize..],
                );
                let new_cost =
                    remaining_route_cost(network, dynamics, traffic, job.speed_table, &new_path);
                if new_cost >= old_cost * 0.95 {
                    continue;
                }
            }

            // new_path starts at the current edge.
            let mut route = Vec::with_capacity(agent.route_pos as usize + new_path.len());
            route.extend_from_slice(&agent.route[..agent.route_pos as usize]);
            route.extend_from_slice(&new_path);
            agent.route_pos = agent.route_pos.min((route.len() - 1) as u32);
            agent.route = route;
            agent.reroutes += 1;
            self.fleets[agent.fleet as usize].reroutes += 1;
        }
    }

    fn capture_frame(&self, t: f64, agents: &[Agent], active: &[u32]) -> Frame {
        let cap = self.scenario.output.frame_max_agents.max(1) as usize;
        let stride = active.len().div_ceil(cap).max(1);
        let mut frame = Frame {
            t,
            active_total: active.len() as u32,
            agent_id: Vec::new(),
            fleet: Vec::new(),
            edge: Vec::new(),
            fraction: Vec::new(),
            speed_mps: Vec::new(),
        };
        for (slot, &agent_index) in active.iter().enumerate() {
            if slot % stride != 0 {
                continue;
            }
            let agent = &agents[agent_index as usize];
            let edge = agent.current_edge();
            let length = self.network.edge_length_m[edge as usize].max(1.0);
            frame.agent_id.push(agent_index);
            frame.fleet.push(agent.fleet);
            frame.edge.push(edge);
            frame.fraction.push((agent.pos_m / length).clamp(0.0, 1.0));
            frame.speed_mps.push(agent.speed_mps);
        }
        frame
    }

    fn publish_fleet_status(&self, agents: &[Agent], active: &[u32]) {
        let mut per_fleet_active = vec![0u32; self.fleets.len()];
        for &agent_index in active {
            per_fleet_active[agents[agent_index as usize].fleet as usize] += 1;
        }
        let fleets: Vec<FleetStatus> = self
            .fleets
            .iter()
            .enumerate()
            .map(|(index, fleet)| FleetStatus {
                fleet_id: fleet.config.fleet_id.clone(),
                mode: fleet.mode_name.clone(),
                profile_id: fleet.config.profile_id.clone(),
                requested: fleet.report.requested,
                dispatched: fleet.report.dispatched,
                dispatch_failed: fleet.report.failed,
                active: per_fleet_active[index],
                arrived: fleet.arrived,
                reroutes: fleet.reroutes,
            })
            .collect();
        self.update_status(|status| status.fleets = fleets.clone());
    }

    fn update_status(&self, update: impl FnOnce(&mut SimulationStatus)) {
        let mut status = self.shared.status.write().expect("status lock");
        update(&mut status);
    }

    fn build_result(
        &self,
        agents: Vec<Agent>,
        active: &[u32],
        dynamics: &EdgeDynamics,
        final_t: f64,
        wall_time_ms: u64,
        cancelled: bool,
    ) -> SimulationResult {
        let mut edge_usage = Vec::new();
        for edge in 0..self.network.edge_count() {
            if dynamics.traversals[edge] == 0 && dynamics.vehicle_seconds[edge] <= 0.0 {
                continue;
            }
            edge_usage.push(EdgeUsage {
                edge: edge as u32,
                traversals: dynamics.traversals[edge],
                vehicle_seconds: dynamics.vehicle_seconds[edge],
                congested_seconds: dynamics.congested_seconds[edge],
            });
        }

        let mut fleet_summaries = Vec::with_capacity(self.fleets.len());
        for fleet in &self.fleets {
            let arrived = fleet.arrived as f64;
            fleet_summaries.push(FleetSummary {
                fleet_id: fleet.config.fleet_id.clone(),
                profile_id: fleet.config.profile_id.clone(),
                mode: fleet.mode_name.clone(),
                requested: fleet.report.requested,
                dispatched: fleet.report.dispatched,
                dispatch_failed: fleet.report.failed,
                dispatch_errors: fleet.report.sample_errors.clone(),
                arrived: fleet.arrived,
                reroutes: fleet.reroutes,
                mean_travel_time_s: (arrived > 0.0).then(|| fleet.travel_time_sum / arrived),
                mean_distance_m: (arrived > 0.0).then(|| fleet.distance_sum / arrived),
                mean_delay_s: (arrived > 0.0).then(|| fleet.delay_sum / arrived),
            });
        }

        let arrived_total: u32 = self.fleets.iter().map(|fleet| fleet.arrived).sum();
        let dispatch_failed: u32 = self.fleets.iter().map(|fleet| fleet.report.failed).sum();
        let never_departed = agents
            .iter()
            .filter(|agent| agent.status == AgentStatus::NeverDeparted)
            .count() as u32;

        let trajectories = self.scenario.output.store_trajectories.then(|| {
            agents
                .iter()
                .enumerate()
                .filter(|(_, agent)| !agent.traj_edges.is_empty())
                .map(|(agent_index, agent)| AgentTrajectory {
                    agent_id: agent_index as u32,
                    fleet: agent.fleet,
                    fleet_id: self.fleets[agent.fleet as usize].config.fleet_id.clone(),
                    depart_s: agent.depart_s,
                    arrive_s: agent.arrive_s.is_finite().then_some(agent.arrive_s),
                    origin: agent.origin,
                    destination: agent.destination,
                    edges: agent.traj_edges.clone(),
                    enter_s: agent.traj_enter_s.clone(),
                    distance_m: agent.distance_m,
                    freeflow_time_s: agent.freeflow_time_s,
                    reroutes: agent.reroutes,
                })
                .collect()
        });

        let (frames, edge_bins) = {
            let live = self.shared.live.read().expect("live store lock");
            (live.frames.clone(), live.edge_bins.clone())
        };

        let total_reroutes: u64 = self.fleets.iter().map(|fleet| fleet.reroutes).sum();
        SimulationResult {
            scenario: self.scenario.clone(),
            summary: SimulationSummary {
                simulated_s: final_t,
                wall_time_ms,
                cancelled,
                agents_total: agents.len() as u32,
                agents_arrived: arrived_total,
                agents_unfinished: active.len() as u32,
                agents_never_departed: never_departed,
                dispatch_failed,
                total_reroutes,
            },
            fleets: fleet_summaries,
            trajectories,
            frames,
            edge_bins,
            edge_usage,
        }
    }
}

fn merge_dispatch_report(into: &mut DispatchReport, report: DispatchReport) {
    into.requested += report.requested;
    into.dispatched += report.dispatched;
    into.failed += report.failed;
    for error in report.sample_errors {
        if into.sample_errors.len() >= 5 {
            break;
        }
        into.sample_errors.push(error);
    }
}

fn make_fleet_runtime(
    config: FleetConfig,
    profiles: &BTreeMap<String, Arc<PreparedRoutingEngine>>,
    profile_order: &[String],
) -> Result<FleetRuntime> {
    let engine = profiles
        .get(&config.profile_id)
        .with_context(|| {
            format!(
                "fleet '{}' references unknown profile '{}'",
                config.fleet_id, config.profile_id
            )
        })?
        .clone();
    let speed_table = profile_order
        .iter()
        .position(|id| id == &config.profile_id)
        .context("profile missing from speed tables")?;
    let mode = engine.metrics().mode;
    Ok(FleetRuntime {
        config,
        engine,
        speed_table,
        mode_bit: mode.access_bit(),
        mode_name: format!("{mode:?}").to_lowercase(),
        report: DispatchReport::default(),
        arrived: 0,
        reroutes: 0,
        travel_time_sum: 0.0,
        distance_sum: 0.0,
        delay_sum: 0.0,
    })
}

/// Congestion factor + density ratio for an edge at a tick (cached per tick,
/// also feeds the per-bin statistics exactly once per tick).
fn edge_factor(
    network: &SimNetwork,
    dynamics: &mut EdgeDynamics,
    edge: usize,
    tick: u32,
    dt: f64,
    traffic: &crate::scenario::TrafficModelConfig,
) -> (f32, f32) {
    if dynamics.cache_tick[edge] == tick {
        return (dynamics.cache_factor[edge], dynamics.cache_ratio[edge]);
    }
    let storage = (network.edge_storage_pcu[edge] * dynamics.zone_capacity[edge]).max(0.1);
    let load = dynamics.occupancy_pcu[edge] + dynamics.background_pcu[edge];
    let ratio = (load / storage).max(0.0);
    let factor = congestion_factor(ratio, traffic);
    dynamics.cache_tick[edge] = tick;
    dynamics.cache_factor[edge] = factor;
    dynamics.cache_ratio[edge] = ratio;

    // Statistics: occupancy integral + congestion bookkeeping once per tick.
    dynamics.vehicle_seconds[edge] += dynamics.occupancy_pcu[edge] * dt as f32;
    if factor < 0.5 {
        dynamics.congested_seconds[edge] += dt as f32;
    }
    let acc = dynamics.bin_acc_mut(edge);
    acc.occ_sum += load;
    acc.factor_sum += factor;
    acc.ticks += 1;
    (factor, ratio)
}

pub(crate) fn congestion_factor(ratio: f32, traffic: &crate::scenario::TrafficModelConfig) -> f32 {
    let min_factor = traffic.min_speed_factor as f32;
    match traffic.model {
        CongestionModel::Greenshields => (1.0 - ratio).clamp(min_factor, 1.0),
        CongestionModel::Bpr => {
            let alpha = traffic.bpr_alpha as f32;
            let beta = traffic.bpr_beta as f32;
            (1.0 / (1.0 + alpha * ratio.powf(beta))).clamp(min_factor, 1.0)
        }
    }
}

fn signal_red(
    network: &SimNetwork,
    dynamics: &EdgeDynamics,
    edge: usize,
    t: f64,
    scenario: &SimulationScenario,
) -> bool {
    if !network.edge_signal[edge] {
        return false;
    }
    let cycle = scenario.traffic.signal_cycle_s.max(1.0);
    let phase = (t + dynamics.signal_offset_s[edge] as f64) % cycle;
    phase > cycle * scenario.traffic.signal_green_share
}

/// Live congested traversal cost of an edge in seconds (for rerouting).
fn live_edge_cost(
    network: &SimNetwork,
    dynamics: &EdgeDynamics,
    traffic: &crate::scenario::TrafficModelConfig,
    speed_table: usize,
    edge: usize,
) -> Option<f32> {
    let freeflow = network.fleet_freeflow_mps[speed_table][edge];
    if freeflow <= 0.0 {
        return None;
    }
    let storage = (network.edge_storage_pcu[edge] * dynamics.zone_capacity[edge]).max(0.1);
    let load = dynamics.occupancy_pcu[edge] + dynamics.background_pcu[edge];
    let factor = congestion_factor(load / storage, traffic);
    let speed = (freeflow * dynamics.zone_speed[edge] * factor).max(SPEED_FLOOR_MPS);
    let mut cost = network.edge_length_m[edge] / speed;
    if network.edge_signal[edge] {
        // Expected wait at a red signal.
        let red_share = 1.0 - traffic.signal_green_share as f32;
        cost += red_share * red_share * traffic.signal_cycle_s as f32 / 2.0;
    }
    Some(cost)
}

fn remaining_route_cost(
    network: &SimNetwork,
    dynamics: &EdgeDynamics,
    traffic: &crate::scenario::TrafficModelConfig,
    speed_table: usize,
    edges: &[u32],
) -> f32 {
    edges
        .iter()
        .map(|edge| {
            live_edge_cost(network, dynamics, traffic, speed_table, *edge as usize)
                .unwrap_or(3600.0)
        })
        .sum()
}

#[derive(PartialEq)]
struct HeapEntry {
    estimate: f32,
    cost: f32,
    edge: u32,
}

impl Eq for HeapEntry {}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Min-heap on the A* estimate.
        other
            .estimate
            .partial_cmp(&self.estimate)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| other.edge.cmp(&self.edge))
    }
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Search state reused across A* calls on the same worker thread so repeated
/// reroutes don't reallocate (and rehash) fresh maps every time.
#[derive(Default)]
struct AstarScratch {
    best: FxHashMap<u32, f32>,
    parent: FxHashMap<u32, u32>,
    heap: BinaryHeap<HeapEntry>,
}

/// A* over the edge graph with live congested costs. Returns the edge path
/// starting at `start_edge` and ending at `goal_edge` (inclusive).
#[allow(clippy::too_many_arguments)]
fn astar_route(
    network: &SimNetwork,
    dynamics: &EdgeDynamics,
    traffic: &crate::scenario::TrafficModelConfig,
    speed_table: usize,
    mode_bit: u16,
    start_edge: u32,
    goal_edge: u32,
    scratch: &mut AstarScratch,
) -> Option<Vec<u32>> {
    if start_edge == goal_edge {
        return Some(vec![start_edge]);
    }
    let goal_node = network.edge_to[goal_edge as usize] as usize;
    let goal_lon = network.node_lon[goal_node];
    let goal_lat = network.node_lat[goal_node];
    let max_speed = network.fleet_max_speed_mps[speed_table].max(1.0);

    let heuristic = |edge: u32| -> f32 {
        let node = network.edge_to[edge as usize] as usize;
        (haversine_m(
            network.node_lon[node],
            network.node_lat[node],
            goal_lon,
            goal_lat,
        ) / max_speed) as f32
    };

    let AstarScratch { best, parent, heap } = scratch;
    best.clear();
    parent.clear();
    heap.clear();
    best.insert(start_edge, 0.0);
    heap.push(HeapEntry {
        estimate: heuristic(start_edge),
        cost: 0.0,
        edge: start_edge,
    });
    let mut settled = 0usize;

    while let Some(entry) = heap.pop() {
        if entry.edge == goal_edge {
            let mut path = vec![goal_edge];
            let mut cursor = goal_edge;
            while let Some(&previous) = parent.get(&cursor) {
                path.push(previous);
                cursor = previous;
            }
            path.reverse();
            return Some(path);
        }
        if let Some(&known) = best.get(&entry.edge)
            && entry.cost > known + 1e-6
        {
            continue;
        }
        settled += 1;
        if settled > ASTAR_MAX_SETTLED {
            return None;
        }
        for &next in network.transitions(entry.edge) {
            let next_usize = next as usize;
            if dynamics.closed_mask[next_usize] & mode_bit != 0 {
                continue;
            }
            let Some(edge_cost) =
                live_edge_cost(network, dynamics, traffic, speed_table, next_usize)
            else {
                continue;
            };
            let cost = entry.cost + edge_cost;
            if best.get(&next).is_none_or(|&known| cost < known - 1e-6) {
                best.insert(next, cost);
                parent.insert(next, entry.edge);
                heap.push(HeapEntry {
                    estimate: cost + heuristic(next),
                    cost,
                    edge: next,
                });
            }
        }
    }
    None
}

fn flush_bin(dynamics: &mut EdgeDynamics, start_s: f64, end_s: f64, tick_s: f64) -> EdgeBin {
    let ticks_in_bin = (((end_s - start_s) / tick_s).round() as u32).max(1);
    let mut touched = std::mem::take(&mut dynamics.bin_touched);
    touched.sort_unstable();
    let stats: Vec<EdgeBinStat> = touched
        .into_iter()
        .map(|edge| {
            let acc = std::mem::take(&mut dynamics.bin_acc[edge as usize]);
            EdgeBinStat {
                edge,
                mean_occupancy_pcu: acc.occ_sum / ticks_in_bin as f32,
                mean_speed_factor: if acc.ticks > 0 {
                    acc.factor_sum / acc.ticks as f32
                } else {
                    1.0
                },
                entered: acc.entered,
            }
        })
        .collect();
    EdgeBin {
        start_s,
        end_s,
        stats,
    }
}
