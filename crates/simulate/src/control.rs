//! Live simulation state shared between the running engine and its callers:
//! status, mid-run control commands, and the append-only frame/stat store
//! that makes playback possible while the simulation is still running.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::network::SimNetwork;
use crate::scenario::{FleetConfig, ZoneConfig};

/// Commands accepted while a simulation is running (or paused).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum SimulationCommand {
    Pause,
    Resume,
    Cancel,
    /// Scale every edge speed globally (e.g. weather). 1.0 restores normal.
    SetGlobalSpeedFactor {
        factor: f64,
    },
    /// Add a zone mid-run (closure, speed limit, capacity cut, background
    /// traffic...). Takes effect on the next tick.
    AddZone {
        zone: ZoneConfig,
    },
    RemoveZone {
        zone_id: String,
    },
    /// Dispatch an extra fleet of agents mid-run. Departure times are
    /// interpreted relative to the current simulation time.
    SpawnFleet {
        fleet: FleetConfig,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimRunState {
    Pending,
    Dispatching,
    Running,
    Paused,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetStatus {
    pub fleet_id: String,
    pub mode: String,
    pub profile_id: String,
    pub requested: u32,
    pub dispatched: u32,
    pub dispatch_failed: u32,
    pub active: u32,
    pub arrived: u32,
    pub reroutes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationStatus {
    pub state: SimRunState,
    pub sim_time_s: f64,
    pub duration_s: f64,
    pub tick_s: f64,
    pub agents_total: u32,
    pub agents_active: u32,
    pub agents_arrived: u32,
    pub agents_pending: u32,
    pub agents_failed: u32,
    pub frames_available: u32,
    pub frame_interval_s: f64,
    pub edge_bins_available: u32,
    pub edge_stats_interval_s: f64,
    pub global_speed_factor: f64,
    pub wall_time_ms: u64,
    pub message: String,
    pub fleets: Vec<FleetStatus>,
}

impl SimulationStatus {
    pub fn initial(duration_s: f64, tick_s: f64, frame_interval_s: f64, bin_s: f64) -> Self {
        Self {
            state: SimRunState::Pending,
            sim_time_s: 0.0,
            duration_s,
            tick_s,
            agents_total: 0,
            agents_active: 0,
            agents_arrived: 0,
            agents_pending: 0,
            agents_failed: 0,
            frames_available: 0,
            frame_interval_s,
            edge_bins_available: 0,
            edge_stats_interval_s: bin_s,
            global_speed_factor: 1.0,
            wall_time_ms: 0,
            message: String::new(),
            fleets: Vec::new(),
        }
    }
}

/// One stored snapshot of agent positions. Positions are kept as
/// (edge, fraction) so coordinates resolve exactly against the network at
/// serve time and the store stays compact at high agent counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub t: f64,
    /// Total active agents at this time (>= sampled count below).
    pub active_total: u32,
    pub agent_id: Vec<u32>,
    pub fleet: Vec<u16>,
    pub edge: Vec<u32>,
    pub fraction: Vec<f32>,
    pub speed_mps: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeBinStat {
    pub edge: u32,
    /// Mean simultaneous PCU on the edge over the bin.
    pub mean_occupancy_pcu: f32,
    /// Mean congestion factor (1 = free flow) over ticks the edge was active.
    pub mean_speed_factor: f32,
    /// Vehicles that entered the edge during the bin.
    pub entered: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeBin {
    pub start_s: f64,
    pub end_s: f64,
    pub stats: Vec<EdgeBinStat>,
}

#[derive(Debug, Default)]
pub struct LiveStore {
    pub frames: Vec<Arc<Frame>>,
    pub edge_bins: Vec<Arc<EdgeBin>>,
}

pub struct SharedSimState {
    pub status: RwLock<SimulationStatus>,
    pub commands: Mutex<Vec<SimulationCommand>>,
    pub live: RwLock<LiveStore>,
    pub network: OnceLock<Arc<SimNetwork>>,
    pub cancelled: AtomicBool,
}

impl SharedSimState {
    pub fn new(status: SimulationStatus) -> Arc<Self> {
        Arc::new(Self {
            status: RwLock::new(status),
            commands: Mutex::new(Vec::new()),
            live: RwLock::new(LiveStore::default()),
            network: OnceLock::new(),
            cancelled: AtomicBool::new(false),
        })
    }
}

/// Cloneable handle for observing and steering a simulation.
#[derive(Clone)]
pub struct SimulationHandle {
    pub shared: Arc<SharedSimState>,
}

impl SimulationHandle {
    pub fn status(&self) -> SimulationStatus {
        self.shared
            .status
            .read()
            .expect("simulation status lock poisoned")
            .clone()
    }

    pub fn send(&self, command: SimulationCommand) {
        if matches!(command, SimulationCommand::Cancel) {
            self.shared.cancelled.store(true, Ordering::Relaxed);
        }
        self.shared
            .commands
            .lock()
            .expect("simulation command lock poisoned")
            .push(command);
    }

    pub fn network(&self) -> Option<Arc<SimNetwork>> {
        self.shared.network.get().cloned()
    }

    /// Frames whose timestamp falls inside [start_s, end_s].
    pub fn frames_in_range(&self, start_s: f64, end_s: f64) -> Vec<Arc<Frame>> {
        let live = self
            .shared
            .live
            .read()
            .expect("simulation live store lock poisoned");
        live.frames
            .iter()
            .filter(|frame| frame.t >= start_s - 1e-9 && frame.t <= end_s + 1e-9)
            .cloned()
            .collect()
    }

    pub fn edge_bins_in_range(&self, start_s: f64, end_s: f64) -> Vec<Arc<EdgeBin>> {
        let live = self
            .shared
            .live
            .read()
            .expect("simulation live store lock poisoned");
        live.edge_bins
            .iter()
            .filter(|bin| bin.end_s >= start_s && bin.start_s <= end_s)
            .cloned()
            .collect()
    }

    pub fn is_finished(&self) -> bool {
        matches!(
            self.status().state,
            SimRunState::Completed | SimRunState::Cancelled | SimRunState::Failed
        )
    }
}
