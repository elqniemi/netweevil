//! Simulation endpoints: create/run simulations in the background, stream
//! status + agent-position frames + edge congestion while running, accept
//! mid-run control commands, and serve full results for playback.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::response::{IntoResponse, Response};
use netweevil_query::PreparedRoutingEngine;
use netweevil_simulate::{
    SimulationCommand, SimulationHandle, SimulationResult, SimulationRunner, SimulationScenario,
    SimulationStatus, edge_bins_to_geojson, edge_usage_to_geojson, frames_to_temporal_geojson,
    trajectory_to_geojson,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::{ApiError, ApiState, geojson_response};

pub struct SimulationEntry {
    pub simulation_id: String,
    pub scenario_id: String,
    pub created_at: String,
    pub handle: SimulationHandle,
    pub result: Arc<OnceLock<Result<Arc<SimulationResult>, String>>>,
}

#[derive(Default)]
pub struct SimulationRegistry {
    counter: AtomicU64,
    entries: Mutex<BTreeMap<String, Arc<SimulationEntry>>>,
}

impl SimulationRegistry {
    fn insert(&self, entry: Arc<SimulationEntry>) {
        self.entries
            .lock()
            .expect("simulation registry lock poisoned")
            .insert(entry.simulation_id.clone(), entry);
    }

    fn get(&self, simulation_id: &str) -> Option<Arc<SimulationEntry>> {
        self.entries
            .lock()
            .expect("simulation registry lock poisoned")
            .get(simulation_id)
            .cloned()
    }

    fn remove(&self, simulation_id: &str) -> Option<Arc<SimulationEntry>> {
        self.entries
            .lock()
            .expect("simulation registry lock poisoned")
            .remove(simulation_id)
    }

    fn list(&self) -> Vec<Arc<SimulationEntry>> {
        self.entries
            .lock()
            .expect("simulation registry lock poisoned")
            .values()
            .cloned()
            .collect()
    }

    fn next_id(&self, scenario_id: &str) -> String {
        let number = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        format!("sim-{number:04}-{scenario_id}")
    }
}

#[derive(Debug, Deserialize)]
pub struct SimulationCreateRequest {
    pub scenario: SimulationScenario,
}

#[derive(Debug, Serialize)]
pub struct SimulationCreateResponse {
    pub simulation_id: String,
    pub status: SimulationStatus,
}

#[derive(Debug, Serialize)]
pub struct SimulationInfo {
    pub simulation_id: String,
    pub scenario_id: String,
    pub created_at: String,
    pub status: SimulationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Value>,
}

fn entry_info(entry: &SimulationEntry, include_summary: bool) -> SimulationInfo {
    let status = entry.handle.status();
    let (error, summary) = match entry.result.get() {
        Some(Ok(result)) => (
            None,
            include_summary.then(|| {
                json!({
                    "summary": result.summary,
                    "fleets": result.fleets,
                    "edge_usage_count": result.edge_usage.len(),
                    "trajectory_count": result
                        .trajectories
                        .as_ref()
                        .map(|trajectories| trajectories.len())
                        .unwrap_or(0),
                })
            }),
        ),
        Some(Err(error)) => (Some(error.clone()), None),
        None => (None, None),
    };
    SimulationInfo {
        simulation_id: entry.simulation_id.clone(),
        scenario_id: entry.scenario_id.clone(),
        created_at: entry.created_at.clone(),
        status,
        error,
        summary,
    }
}

pub async fn create_simulation(
    State(state): State<ApiState>,
    Json(payload): Json<SimulationCreateRequest>,
) -> Result<Json<SimulationCreateResponse>, ApiError> {
    let scenario = payload.scenario;
    scenario
        .validate()
        .map_err(|error| ApiError::bad_request(error.to_string()))?;

    let service = state.service.clone();
    let mut profiles: BTreeMap<String, Arc<PreparedRoutingEngine>> = BTreeMap::new();
    for (profile_id, profile) in &service.profiles {
        profiles.insert(profile_id.clone(), Arc::clone(&profile.engine));
    }
    let topology = service.topology.clone();
    let scenario_id = scenario.scenario.id.clone();
    let simulation_id = state.simulations.next_id(&scenario_id);

    info!(
        endpoint = "simulation",
        simulation_id = %simulation_id,
        scenario_id = %scenario_id,
        fleets = scenario.fleets.len(),
        "creating simulation"
    );

    // Building the runner compiles the simulation network (CPU bound).
    let runner =
        tokio::task::spawn_blocking(move || SimulationRunner::new(scenario, topology, profiles))
            .await
            .map_err(|error| ApiError::internal(format!("simulation build task failed: {error}")))?
            .map_err(|error| ApiError::bad_request(format!("{error:#}")))?;

    let handle = runner.handle();
    let entry = Arc::new(SimulationEntry {
        simulation_id: simulation_id.clone(),
        scenario_id,
        created_at: netweevil_report::now_rfc3339().unwrap_or_else(|_| "unknown".to_string()),
        handle: handle.clone(),
        result: Arc::new(OnceLock::new()),
    });
    state.simulations.insert(entry.clone());

    let result_slot = entry.result.clone();
    let task_id = simulation_id.clone();
    tokio::task::spawn_blocking(move || {
        let outcome = runner.run();
        match outcome {
            Ok(result) => {
                info!(simulation_id = %task_id, agents = result.summary.agents_total,
                    arrived = result.summary.agents_arrived,
                    wall_ms = result.summary.wall_time_ms, "simulation finished");
                let _ = result_slot.set(Ok(Arc::new(result)));
            }
            Err(error) => {
                warn!(simulation_id = %task_id, %error, "simulation failed");
                let _ = result_slot.set(Err(format!("{error:#}")));
            }
        }
    });

    Ok(Json(SimulationCreateResponse {
        simulation_id,
        status: handle.status(),
    }))
}

pub async fn list_simulations(State(state): State<ApiState>) -> Json<Vec<SimulationInfo>> {
    let mut infos: Vec<SimulationInfo> = state
        .simulations
        .list()
        .iter()
        .map(|entry| entry_info(entry, false))
        .collect();
    infos.sort_by(|a, b| a.simulation_id.cmp(&b.simulation_id));
    Json(infos)
}

fn lookup(state: &ApiState, simulation_id: &str) -> Result<Arc<SimulationEntry>, ApiError> {
    state
        .simulations
        .get(simulation_id)
        .ok_or_else(|| ApiError::not_found(format!("unknown simulation '{simulation_id}'")))
}

pub async fn get_simulation(
    State(state): State<ApiState>,
    AxumPath(simulation_id): AxumPath<String>,
) -> Result<Json<SimulationInfo>, ApiError> {
    let entry = lookup(&state, &simulation_id)?;
    Ok(Json(entry_info(&entry, true)))
}

pub async fn delete_simulation(
    State(state): State<ApiState>,
    AxumPath(simulation_id): AxumPath<String>,
) -> Result<Json<Value>, ApiError> {
    let entry = lookup(&state, &simulation_id)?;
    entry.handle.send(SimulationCommand::Cancel);
    state.simulations.remove(&simulation_id);
    Ok(Json(
        json!({"simulation_id": simulation_id, "removed": true}),
    ))
}

pub async fn control_simulation(
    State(state): State<ApiState>,
    AxumPath(simulation_id): AxumPath<String>,
    Json(command): Json<SimulationCommand>,
) -> Result<Json<SimulationInfo>, ApiError> {
    let entry = lookup(&state, &simulation_id)?;
    if entry.handle.is_finished() && !matches!(command, SimulationCommand::Cancel) {
        return Err(ApiError::bad_request(format!(
            "simulation '{simulation_id}' has finished; commands are no longer accepted"
        )));
    }
    info!(simulation_id = %simulation_id, command = ?command, "simulation control");
    entry.handle.send(command);
    // Give the engine a moment to apply on the next tick before reporting.
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    Ok(Json(entry_info(&entry, false)))
}

#[derive(Debug, Deserialize, Default)]
pub struct FrameQuery {
    pub start_s: Option<f64>,
    pub end_s: Option<f64>,
    /// Cap agents per frame (deterministic stride subsample).
    pub max_agents: Option<usize>,
    /// "json" (columnar, default) or "geojson" (last frame only).
    pub format: Option<String>,
}

pub async fn simulation_frames(
    State(state): State<ApiState>,
    AxumPath(simulation_id): AxumPath<String>,
    Query(query): Query<FrameQuery>,
) -> Result<Response, ApiError> {
    let entry = lookup(&state, &simulation_id)?;
    let network = entry
        .handle
        .network()
        .ok_or_else(|| ApiError::internal("simulation network unavailable"))?;
    let status = entry.handle.status();
    let start_s = query.start_s.unwrap_or(0.0);
    let end_s = query.end_s.unwrap_or(f64::MAX);
    let frames = entry.handle.frames_in_range(start_s, end_s);
    let fleet_ids: Vec<String> = status
        .fleets
        .iter()
        .map(|fleet| fleet.fleet_id.clone())
        .collect();
    let max_agents = query.max_agents.unwrap_or(usize::MAX).max(1);

    if query.format.as_deref() == Some("geojson") {
        let empty;
        let frame = match frames.last() {
            Some(frame) => frame.as_ref(),
            None => {
                empty = netweevil_simulate::Frame {
                    t: start_s,
                    active_total: 0,
                    agent_id: Vec::new(),
                    fleet: Vec::new(),
                    edge: Vec::new(),
                    fraction: Vec::new(),
                    speed_mps: Vec::new(),
                };
                &empty
            }
        };
        let geojson = netweevil_simulate::frame_to_geojson(frame, &network, &fleet_ids);
        return geojson_response(geojson);
    }

    let mut frame_payloads = Vec::with_capacity(frames.len());
    for frame in &frames {
        let stride = frame.agent_id.len().div_ceil(max_agents).max(1);
        let mut ids = Vec::new();
        let mut fleets = Vec::new();
        let mut lons = Vec::new();
        let mut lats = Vec::new();
        let mut speeds = Vec::new();
        let mut edges = Vec::new();
        for slot in (0..frame.agent_id.len()).step_by(stride) {
            let (lon, lat) =
                network.position_on_edge(frame.edge[slot], frame.fraction[slot] as f64);
            ids.push(frame.agent_id[slot]);
            fleets.push(
                fleet_ids
                    .get(frame.fleet[slot] as usize)
                    .cloned()
                    .unwrap_or_else(|| frame.fleet[slot].to_string()),
            );
            lons.push((lon * 1e7).round() / 1e7);
            lats.push((lat * 1e7).round() / 1e7);
            speeds.push((frame.speed_mps[slot] * 10.0).round() / 10.0);
            edges.push(frame.edge[slot]);
        }
        frame_payloads.push(json!({
            "t": frame.t,
            "active_total": frame.active_total,
            "agents": {
                "id": ids,
                "fleet": fleets,
                "lon": lons,
                "lat": lats,
                "speed_mps": speeds,
                "edge": edges,
            }
        }));
    }

    let body = json!({
        "simulation_id": simulation_id,
        "state": status.state,
        "sim_time_s": status.sim_time_s,
        "frame_interval_s": status.frame_interval_s,
        "frames_available": status.frames_available,
        "frames": frame_payloads,
    });
    Ok(Json(body).into_response())
}

#[derive(Debug, Deserialize, Default)]
pub struct EdgeStatsQuery {
    pub start_s: Option<f64>,
    pub end_s: Option<f64>,
    /// Skip edges quieter than this mean occupancy (PCU).
    pub min_occupancy: Option<f64>,
    /// "bins" (time window, default) or "summary" (whole run busy segments).
    pub mode: Option<String>,
    pub min_traversals: Option<u32>,
}

pub async fn simulation_edges(
    State(state): State<ApiState>,
    AxumPath(simulation_id): AxumPath<String>,
    Query(query): Query<EdgeStatsQuery>,
) -> Result<Response, ApiError> {
    let entry = lookup(&state, &simulation_id)?;
    let network = entry
        .handle
        .network()
        .ok_or_else(|| ApiError::internal("simulation network unavailable"))?;

    let geojson = if query.mode.as_deref() == Some("summary") {
        let result = match entry.result.get() {
            Some(Ok(result)) => result.clone(),
            Some(Err(error)) => return Err(ApiError::bad_request(error.clone())),
            None => {
                return Err(ApiError::bad_request(
                    "summary edge usage is available after the simulation completes; \
                     use mode=bins while running",
                ));
            }
        };
        edge_usage_to_geojson(
            &result.edge_usage,
            &network,
            query.min_traversals.unwrap_or(1),
        )
    } else {
        let bins = entry.handle.edge_bins_in_range(
            query.start_s.unwrap_or(0.0),
            query.end_s.unwrap_or(f64::MAX),
        );
        edge_bins_to_geojson(&bins, &network, query.min_occupancy.unwrap_or(0.0))
    };
    geojson_response(geojson)
}

#[derive(Debug, Deserialize, Default)]
pub struct TemporalQuery {
    pub start_s: Option<f64>,
    pub end_s: Option<f64>,
    pub max_agents: Option<usize>,
    /// Reference instant for frame timestamps (ISO, default 2026-01-01T00:00:00).
    pub base_datetime: Option<String>,
}

pub async fn simulation_temporal(
    State(state): State<ApiState>,
    AxumPath(simulation_id): AxumPath<String>,
    Query(query): Query<TemporalQuery>,
) -> Result<Response, ApiError> {
    let entry = lookup(&state, &simulation_id)?;
    let network = entry
        .handle
        .network()
        .ok_or_else(|| ApiError::internal("simulation network unavailable"))?;
    let status = entry.handle.status();
    let fleet_ids: Vec<String> = status
        .fleets
        .iter()
        .map(|fleet| fleet.fleet_id.clone())
        .collect();
    let mut frames = entry.handle.frames_in_range(
        query.start_s.unwrap_or(0.0),
        query.end_s.unwrap_or(f64::MAX),
    );
    // Subsample agents per frame when requested.
    if let Some(max_agents) = query.max_agents {
        let max_agents = max_agents.max(1);
        frames = frames
            .iter()
            .map(|frame| {
                let stride = frame.agent_id.len().div_ceil(max_agents).max(1);
                if stride == 1 {
                    return frame.clone();
                }
                let keep: Vec<usize> = (0..frame.agent_id.len()).step_by(stride).collect();
                Arc::new(netweevil_simulate::Frame {
                    t: frame.t,
                    active_total: frame.active_total,
                    agent_id: keep.iter().map(|&slot| frame.agent_id[slot]).collect(),
                    fleet: keep.iter().map(|&slot| frame.fleet[slot]).collect(),
                    edge: keep.iter().map(|&slot| frame.edge[slot]).collect(),
                    fraction: keep.iter().map(|&slot| frame.fraction[slot]).collect(),
                    speed_mps: keep.iter().map(|&slot| frame.speed_mps[slot]).collect(),
                })
            })
            .collect();
    }
    let base = query
        .base_datetime
        .unwrap_or_else(|| "2026-01-01T00:00:00".to_string());
    let geojson = frames_to_temporal_geojson(&frames, &network, &fleet_ids, &base);
    geojson_response(geojson)
}

pub async fn simulation_agent(
    State(state): State<ApiState>,
    AxumPath((simulation_id, agent_id)): AxumPath<(String, u32)>,
) -> Result<Json<Value>, ApiError> {
    let entry = lookup(&state, &simulation_id)?;
    let network = entry
        .handle
        .network()
        .ok_or_else(|| ApiError::internal("simulation network unavailable"))?;
    let result = match entry.result.get() {
        Some(Ok(result)) => result.clone(),
        Some(Err(error)) => return Err(ApiError::bad_request(error.clone())),
        None => {
            return Err(ApiError::bad_request(
                "agent trajectories are available after the simulation completes",
            ));
        }
    };
    let trajectories = result.trajectories.as_ref().ok_or_else(|| {
        ApiError::bad_request("this simulation was run with store_trajectories=false")
    })?;
    let trajectory = trajectories
        .iter()
        .find(|trajectory| trajectory.agent_id == agent_id)
        .ok_or_else(|| ApiError::not_found(format!("unknown agent {agent_id}")))?;
    Ok(Json(trajectory_to_geojson(trajectory, &network)))
}
