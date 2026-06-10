//! Simulation results, position-at-time interpolation, and GeoJSON export.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::control::{EdgeBin, Frame};
use crate::network::SimNetwork;
use crate::scenario::{PositionModel, SimulationScenario};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationSummary {
    pub simulated_s: f64,
    pub wall_time_ms: u64,
    pub cancelled: bool,
    pub agents_total: u32,
    pub agents_arrived: u32,
    pub agents_unfinished: u32,
    pub agents_never_departed: u32,
    pub dispatch_failed: u32,
    pub total_reroutes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetSummary {
    pub fleet_id: String,
    pub profile_id: String,
    pub mode: String,
    pub requested: u32,
    pub dispatched: u32,
    pub dispatch_failed: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dispatch_errors: Vec<String>,
    pub arrived: u32,
    pub reroutes: u64,
    pub mean_travel_time_s: Option<f64>,
    pub mean_distance_m: Option<f64>,
    pub mean_delay_s: Option<f64>,
}

/// Compact agent trajectory: edge index + entry timestamp pairs. The exit
/// time of edge i is the entry time of edge i+1 (or `arrive_s` for the last).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTrajectory {
    pub agent_id: u32,
    pub fleet: u16,
    pub fleet_id: String,
    pub depart_s: f64,
    pub arrive_s: Option<f64>,
    pub origin: [f64; 2],
    pub destination: [f64; 2],
    pub edges: Vec<u32>,
    pub enter_s: Vec<f32>,
    pub distance_m: f32,
    pub freeflow_time_s: f32,
    pub reroutes: u16,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EdgeUsage {
    pub edge: u32,
    pub traversals: u32,
    pub vehicle_seconds: f32,
    pub congested_seconds: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    pub scenario: SimulationScenario,
    pub summary: SimulationSummary,
    pub fleets: Vec<FleetSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectories: Option<Vec<AgentTrajectory>>,
    pub frames: Vec<Arc<Frame>>,
    pub edge_bins: Vec<Arc<EdgeBin>>,
    pub edge_usage: Vec<EdgeUsage>,
}

/// Distance covered after `elapsed` seconds on an edge crossed in `duration`
/// seconds with `length` metres, under a trapezoidal speed profile that
/// accelerates from rest, cruises, then decelerates to rest.
///
/// The cruise speed solves `length = v*duration - v^2/2 * (1/a + 1/d)`; when
/// the discriminant is negative the edge is too short to reach a cruise
/// phase and the profile degenerates to linear.
pub fn trapezoidal_distance(
    elapsed: f64,
    duration: f64,
    length: f64,
    accel_mps2: f64,
    decel_mps2: f64,
) -> f64 {
    if duration <= 0.0 || length <= 0.0 {
        return 0.0;
    }
    let elapsed = elapsed.clamp(0.0, duration);
    let a = accel_mps2.max(0.05);
    let d = decel_mps2.max(0.05);
    let k = 0.5 * (1.0 / a + 1.0 / d);
    let discriminant = duration * duration - 4.0 * k * length;
    if discriminant <= 0.0 {
        // Cannot fit a trapezoid: fall back to constant speed.
        return length * elapsed / duration;
    }
    let v_cruise = (duration - discriminant.sqrt()) / (2.0 * k);
    if !(v_cruise.is_finite() && v_cruise > 0.0) {
        return length * elapsed / duration;
    }
    let t_accel = v_cruise / a;
    let t_decel = v_cruise / d;
    let t_cruise = (duration - t_accel - t_decel).max(0.0);
    if elapsed <= t_accel {
        0.5 * a * elapsed * elapsed
    } else if elapsed <= t_accel + t_cruise {
        0.5 * a * t_accel * t_accel + v_cruise * (elapsed - t_accel)
    } else {
        let into_decel = (elapsed - t_accel - t_cruise).min(t_decel);
        0.5 * a * t_accel * t_accel + v_cruise * t_cruise + v_cruise * into_decel
            - 0.5 * d * into_decel * into_decel
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AgentPosition {
    pub lon: f64,
    pub lat: f64,
    pub edge: u32,
    pub fraction: f64,
    pub speed_mps: f64,
    pub heading_deg: f64,
}

/// Exact position of an agent at simulation time `t`, interpolated from its
/// trajectory timestamps. Returns None when the agent is not on the network
/// at `t`.
pub fn trajectory_position_at(
    trajectory: &AgentTrajectory,
    t: f64,
    network: &SimNetwork,
    model: PositionModel,
    accel_mps2: f64,
    decel_mps2: f64,
) -> Option<AgentPosition> {
    if trajectory.edges.is_empty() || t < trajectory.depart_s {
        return None;
    }
    if let Some(arrive_s) = trajectory.arrive_s
        && t > arrive_s
    {
        return None;
    }
    // Binary search the edge whose [enter, exit) interval contains t.
    let enter = &trajectory.enter_s;
    let mut low = 0usize;
    let mut high = enter.len();
    while low + 1 < high {
        let mid = (low + high) / 2;
        if (enter[mid] as f64) <= t {
            low = mid;
        } else {
            high = mid;
        }
    }
    let index = low;
    let edge = trajectory.edges[index];
    let enter_t = enter[index] as f64;
    let exit_t = if index + 1 < enter.len() {
        enter[index + 1] as f64
    } else if let Some(arrive_s) = trajectory.arrive_s {
        arrive_s
    } else {
        // Still on its last known edge in a running simulation.
        return position_from_fraction(network, edge, 0.0, 0.0);
    };
    let duration = (exit_t - enter_t).max(1e-6);
    let length = network.edge_length_m[edge as usize] as f64;
    let elapsed = (t - enter_t).clamp(0.0, duration);
    let (distance, speed) = match model {
        PositionModel::Linear => (length * elapsed / duration, length / duration),
        PositionModel::Trapezoidal => {
            let distance = trapezoidal_distance(elapsed, duration, length, accel_mps2, decel_mps2);
            // Approximate instantaneous speed numerically.
            let epsilon = (duration / 200.0).max(1e-3);
            let ahead =
                trapezoidal_distance(elapsed + epsilon, duration, length, accel_mps2, decel_mps2);
            (distance, ((ahead - distance) / epsilon).max(0.0))
        }
    };
    position_from_fraction(network, edge, (distance / length).clamp(0.0, 1.0), speed)
}

fn position_from_fraction(
    network: &SimNetwork,
    edge: u32,
    fraction: f64,
    speed: f64,
) -> Option<AgentPosition> {
    if edge as usize >= network.edge_count() {
        return None;
    }
    let (lon, lat) = network.position_on_edge(edge, fraction);
    Some(AgentPosition {
        lon,
        lat,
        edge,
        fraction,
        speed_mps: speed,
        heading_deg: network.edge_heading_deg(edge),
    })
}

/// GeoJSON FeatureCollection of agent positions for one frame.
pub fn frame_to_geojson(frame: &Frame, network: &SimNetwork, fleet_ids: &[String]) -> Value {
    let mut features = Vec::with_capacity(frame.agent_id.len());
    for slot in 0..frame.agent_id.len() {
        let edge = frame.edge[slot];
        let (lon, lat) = network.position_on_edge(edge, frame.fraction[slot] as f64);
        let fleet_index = frame.fleet[slot] as usize;
        features.push(json!({
            "type": "Feature",
            "geometry": {"type": "Point", "coordinates": [lon, lat]},
            "properties": {
                "agent_id": frame.agent_id[slot],
                "fleet": fleet_ids.get(fleet_index).cloned()
                    .unwrap_or_else(|| fleet_index.to_string()),
                "edge_id": edge,
                "speed_mps": frame.speed_mps[slot],
                "speed_kph": frame.speed_mps[slot] * 3.6,
                "heading_deg": network.edge_heading_deg(edge),
                "t": frame.t,
            }
        }));
    }
    json!({
        "type": "FeatureCollection",
        "analysis_kind": "simulation_frame",
        "t": frame.t,
        "active_total": frame.active_total,
        "features": features,
    })
}

/// GeoJSON of per-edge congestion over a set of bins (averaged when an edge
/// appears in several bins). Only edges that saw traffic are included.
pub fn edge_bins_to_geojson(
    bins: &[Arc<EdgeBin>],
    network: &SimNetwork,
    min_mean_occupancy: f64,
) -> Value {
    use std::collections::HashMap;
    #[derive(Default)]
    struct Acc {
        occ_sum: f64,
        factor_sum: f64,
        entered: u64,
        bins: u32,
    }
    let mut by_edge: HashMap<u32, Acc> = HashMap::new();
    for bin in bins {
        for stat in &bin.stats {
            let acc = by_edge.entry(stat.edge).or_default();
            acc.occ_sum += stat.mean_occupancy_pcu as f64;
            acc.factor_sum += stat.mean_speed_factor as f64;
            acc.entered += stat.entered as u64;
            acc.bins += 1;
        }
    }
    let mut edges: Vec<(u32, Acc)> = by_edge.into_iter().collect();
    edges.sort_by_key(|(edge, _)| *edge);
    let mut features = Vec::new();
    for (edge, acc) in edges {
        let mean_occ = acc.occ_sum / acc.bins.max(1) as f64;
        if mean_occ < min_mean_occupancy && acc.entered == 0 {
            continue;
        }
        let mean_factor = acc.factor_sum / acc.bins.max(1) as f64;
        let from = network.edge_from[edge as usize] as usize;
        let to = network.edge_to[edge as usize] as usize;
        features.push(json!({
            "type": "Feature",
            "geometry": {"type": "LineString", "coordinates": [
                [network.node_lon[from], network.node_lat[from]],
                [network.node_lon[to], network.node_lat[to]],
            ]},
            "properties": {
                "edge_id": edge,
                "road_class": format!("{:?}", network.edge_road_class[edge as usize])
                    .to_lowercase(),
                "length_m": network.edge_length_m[edge as usize],
                "mean_occupancy_pcu": mean_occ,
                "mean_speed_factor": mean_factor,
                "congestion_level": (1.0 - mean_factor).clamp(0.0, 1.0),
                "vehicles_entered": acc.entered,
            }
        }));
    }
    json!({
        "type": "FeatureCollection",
        "analysis_kind": "simulation_edges",
        "features": features,
    })
}

/// GeoJSON of whole-run busy segments from sparse edge usage totals.
pub fn edge_usage_to_geojson(
    usage: &[EdgeUsage],
    network: &SimNetwork,
    min_traversals: u32,
) -> Value {
    let mut features = Vec::new();
    for entry in usage {
        if entry.traversals < min_traversals {
            continue;
        }
        let edge = entry.edge as usize;
        let from = network.edge_from[edge] as usize;
        let to = network.edge_to[edge] as usize;
        let congestion_share = if entry.vehicle_seconds > 0.0 {
            (entry.congested_seconds / entry.vehicle_seconds.max(entry.congested_seconds))
                .clamp(0.0, 1.0)
        } else {
            0.0
        };
        features.push(json!({
            "type": "Feature",
            "geometry": {"type": "LineString", "coordinates": [
                [network.node_lon[from], network.node_lat[from]],
                [network.node_lon[to], network.node_lat[to]],
            ]},
            "properties": {
                "edge_id": entry.edge,
                "road_class": format!("{:?}", network.edge_road_class[edge]).to_lowercase(),
                "length_m": network.edge_length_m[edge],
                "traversals": entry.traversals,
                "vehicle_seconds": entry.vehicle_seconds,
                "congested_seconds": entry.congested_seconds,
                "congestion_share": congestion_share,
            }
        }));
    }
    json!({
        "type": "FeatureCollection",
        "analysis_kind": "simulation_edge_usage",
        "features": features,
    })
}

/// Temporal GeoJSON for the native QGIS temporal controller: one point per
/// agent per frame with an ISO-8601 `datetime` property derived from an
/// arbitrary reference date.
pub fn frames_to_temporal_geojson(
    frames: &[Arc<Frame>],
    network: &SimNetwork,
    fleet_ids: &[String],
    base_datetime_iso: &str,
) -> Value {
    let mut features = Vec::new();
    for frame in frames {
        for slot in 0..frame.agent_id.len() {
            let edge = frame.edge[slot];
            let (lon, lat) = network.position_on_edge(edge, frame.fraction[slot] as f64);
            let fleet_index = frame.fleet[slot] as usize;
            features.push(json!({
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [lon, lat]},
                "properties": {
                    "agent_id": frame.agent_id[slot],
                    "fleet": fleet_ids.get(fleet_index).cloned()
                        .unwrap_or_else(|| fleet_index.to_string()),
                    "speed_kph": frame.speed_mps[slot] * 3.6,
                    "t": frame.t,
                    "datetime": offset_iso_datetime(base_datetime_iso, frame.t),
                }
            }));
        }
    }
    json!({
        "type": "FeatureCollection",
        "analysis_kind": "simulation_temporal",
        "features": features,
    })
}

/// Offset an ISO `YYYY-MM-DDTHH:MM:SS` instant by whole seconds without
/// pulling in a datetime dependency (good enough for sub-day simulations).
fn offset_iso_datetime(base_iso: &str, offset_s: f64) -> String {
    let (date, time) = base_iso.split_once('T').unwrap_or((base_iso, "00:00:00"));
    let parts: Vec<u64> = time
        .trim_end_matches('Z')
        .split(':')
        .filter_map(|part| part.parse::<u64>().ok())
        .collect();
    let base_seconds = match parts.as_slice() {
        [h, m, s] => h * 3600 + m * 60 + s,
        [h, m] => h * 3600 + m * 60,
        _ => 0,
    };
    let total = base_seconds + offset_s.max(0.0) as u64;
    let days = total / 86_400;
    let seconds = total % 86_400;
    let day_suffix = if days > 0 {
        format!("+{days}d")
    } else {
        String::new()
    };
    format!(
        "{date}T{:02}:{:02}:{:02}{day_suffix}",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

/// GeoJSON line + timestamps for a single agent (inspection/debugging).
pub fn trajectory_to_geojson(trajectory: &AgentTrajectory, network: &SimNetwork) -> Value {
    let mut coordinates = Vec::with_capacity(trajectory.edges.len() + 1);
    for (index, edge) in trajectory.edges.iter().enumerate() {
        let from = network.edge_from[*edge as usize] as usize;
        if index == 0 {
            coordinates.push(json!([network.node_lon[from], network.node_lat[from]]));
        }
        let to = network.edge_to[*edge as usize] as usize;
        coordinates.push(json!([network.node_lon[to], network.node_lat[to]]));
    }
    json!({
        "type": "Feature",
        "geometry": {"type": "LineString", "coordinates": coordinates},
        "properties": {
            "agent_id": trajectory.agent_id,
            "fleet_id": trajectory.fleet_id,
            "depart_s": trajectory.depart_s,
            "arrive_s": trajectory.arrive_s,
            "distance_m": trajectory.distance_m,
            "freeflow_time_s": trajectory.freeflow_time_s,
            "reroutes": trajectory.reroutes,
            "edge_count": trajectory.edges.len(),
            "enter_s": trajectory.enter_s,
            "edges": trajectory.edges,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trapezoid_covers_full_length() {
        let distance = trapezoidal_distance(20.0, 20.0, 100.0, 2.0, 2.5);
        assert!((distance - 100.0).abs() < 0.5, "got {distance}");
    }

    #[test]
    fn trapezoid_monotonic() {
        let mut last = 0.0;
        for step in 0..=100 {
            let t = step as f64 * 0.2;
            let d = trapezoidal_distance(t, 20.0, 100.0, 2.0, 2.5);
            assert!(d + 1e-9 >= last, "not monotonic at t={t}");
            last = d;
        }
    }

    #[test]
    fn trapezoid_slow_start_and_end() {
        // First second should cover less ground than a middle second.
        let first = trapezoidal_distance(1.0, 20.0, 100.0, 2.0, 2.5);
        let middle = trapezoidal_distance(10.0, 20.0, 100.0, 2.0, 2.5)
            - trapezoidal_distance(9.0, 20.0, 100.0, 2.0, 2.5);
        assert!(first < middle, "first={first} middle={middle}");
    }

    #[test]
    fn degenerate_trapezoid_falls_back_to_linear() {
        // Too little time to accelerate: linear interpolation.
        let d = trapezoidal_distance(0.5, 1.0, 500.0, 1.0, 1.0);
        assert!((d - 250.0).abs() < 1.0);
    }

    #[test]
    fn iso_offset_rolls_over_minutes() {
        assert_eq!(
            offset_iso_datetime("2026-01-01T08:00:00", 75.0),
            "2026-01-01T08:01:15"
        );
    }
}
