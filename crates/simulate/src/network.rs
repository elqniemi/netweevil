//! Static simulation network derived from a `TopologyBundle` plus one
//! compiled profile per fleet. Immutable during a run and shared (Arc) with
//! the live handle so frames can be resolved to coordinates at any time.

use std::sync::Arc;

use anyhow::{Result, bail};
use netweevil_core::{
    CompiledProfileBundle, EDGE_FLAG_TARGET_TRAFFIC_SIGNAL, RoadClass, TopologyBounds,
    TopologyBundle,
};

pub use netweevil_core::geo::haversine_meters as haversine_m;

/// Average passenger-car jam spacing used for edge storage capacity.
const JAM_SPACING_M_PER_PCU: f64 = 7.5;

const fn equivalent_lanes(class: RoadClass) -> f64 {
    match class {
        RoadClass::Motorway => 2.5,
        RoadClass::Trunk => 2.0,
        RoadClass::Primary => 1.6,
        RoadClass::Secondary => 1.3,
        RoadClass::Tertiary => 1.1,
        RoadClass::Residential => 1.0,
        RoadClass::Service => 0.8,
        RoadClass::Track => 0.6,
        RoadClass::Ferry => 1.0,
        RoadClass::Path => 0.5,
        RoadClass::Unknown => 1.0,
    }
}

/// Saturation flow per equivalent lane in PCU/s.
const fn saturation_flow_pcu_s(class: RoadClass) -> f64 {
    match class {
        RoadClass::Motorway => 0.60,
        RoadClass::Trunk => 0.55,
        RoadClass::Primary => 0.50,
        RoadClass::Secondary => 0.45,
        RoadClass::Tertiary => 0.42,
        RoadClass::Residential => 0.35,
        RoadClass::Service => 0.25,
        RoadClass::Track => 0.15,
        RoadClass::Ferry => 0.05,
        RoadClass::Path => 0.30,
        RoadClass::Unknown => 0.35,
    }
}

#[derive(Debug)]
pub struct SimNetwork {
    pub node_lon: Vec<f64>,
    pub node_lat: Vec<f64>,
    pub edge_from: Vec<u32>,
    pub edge_to: Vec<u32>,
    pub edge_length_m: Vec<f32>,
    pub edge_road_class: Vec<RoadClass>,
    /// Edge target node carries a traffic signal.
    pub edge_signal: Vec<bool>,
    /// Storage capacity in PCU (vehicles that physically fit on the edge).
    pub edge_storage_pcu: Vec<f32>,
    /// Outflow capacity in PCU/s.
    pub edge_flow_pcu_s: Vec<f32>,
    /// Allowed transitions edge -> next edges (turn restrictions respected
    /// when the topology bundle provides its turn table).
    pub trans_first: Vec<u32>,
    pub trans_edges: Vec<u32>,
    /// Node -> outgoing edge indices (spawning and reroute starts).
    pub node_first_out: Vec<u32>,
    pub node_edges: Vec<u32>,
    /// Free-flow speed in m/s per edge, per fleet (0 = not accessible).
    pub fleet_freeflow_mps: Vec<Vec<f32>>,
    /// Per fleet: fastest free-flow speed anywhere (A* heuristic bound).
    pub fleet_max_speed_mps: Vec<f64>,
    pub bounds: TopologyBounds,
}

impl SimNetwork {
    pub fn node_count(&self) -> usize {
        self.node_lon.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edge_from.len()
    }

    pub fn transitions(&self, edge: u32) -> &[u32] {
        let start = self.trans_first[edge as usize] as usize;
        let end = self.trans_first[edge as usize + 1] as usize;
        &self.trans_edges[start..end]
    }

    pub fn outgoing(&self, node: u32) -> &[u32] {
        let start = self.node_first_out[node as usize] as usize;
        let end = self.node_first_out[node as usize + 1] as usize;
        &self.node_edges[start..end]
    }

    pub fn edge_midpoint(&self, edge: usize) -> (f64, f64) {
        let from = self.edge_from[edge] as usize;
        let to = self.edge_to[edge] as usize;
        (
            (self.node_lon[from] + self.node_lon[to]) / 2.0,
            (self.node_lat[from] + self.node_lat[to]) / 2.0,
        )
    }

    /// Coordinates at a fraction (0..1) along an edge (straight line between
    /// endpoint nodes — matches how the routing engine reports geometry when
    /// detailed way geometry is not stored).
    pub fn position_on_edge(&self, edge: u32, fraction: f64) -> (f64, f64) {
        let fraction = fraction.clamp(0.0, 1.0);
        let from = self.edge_from[edge as usize] as usize;
        let to = self.edge_to[edge as usize] as usize;
        (
            self.node_lon[from] + (self.node_lon[to] - self.node_lon[from]) * fraction,
            self.node_lat[from] + (self.node_lat[to] - self.node_lat[from]) * fraction,
        )
    }

    pub fn edge_heading_deg(&self, edge: u32) -> f64 {
        let from = self.edge_from[edge as usize] as usize;
        let to = self.edge_to[edge as usize] as usize;
        let d_lon =
            (self.node_lon[to] - self.node_lon[from]) * self.node_lat[from].to_radians().cos();
        let d_lat = self.node_lat[to] - self.node_lat[from];
        let heading = d_lon.atan2(d_lat).to_degrees();
        if heading < 0.0 {
            heading + 360.0
        } else {
            heading
        }
    }
}

pub fn build_sim_network(
    topology: &TopologyBundle,
    fleet_metrics: &[Arc<CompiledProfileBundle>],
) -> Result<Arc<SimNetwork>> {
    let node_count = topology.nodes.len();
    let edge_count = topology.edge_count();
    if node_count == 0 || edge_count == 0 {
        bail!("topology bundle has no nodes or edges");
    }

    let mut node_lon = Vec::with_capacity(node_count);
    let mut node_lat = Vec::with_capacity(node_count);
    let mut min_lon = f64::INFINITY;
    let mut min_lat = f64::INFINITY;
    let mut max_lon = f64::NEG_INFINITY;
    let mut max_lat = f64::NEG_INFINITY;
    for node in &topology.nodes {
        node_lon.push(node.lon);
        node_lat.push(node.lat);
        min_lon = min_lon.min(node.lon);
        min_lat = min_lat.min(node.lat);
        max_lon = max_lon.max(node.lon);
        max_lat = max_lat.max(node.lat);
    }

    let mut edge_from = Vec::with_capacity(edge_count);
    let mut edge_to = Vec::with_capacity(edge_count);
    let mut edge_length_m = Vec::with_capacity(edge_count);
    let mut edge_road_class = Vec::with_capacity(edge_count);
    let mut edge_signal = Vec::with_capacity(edge_count);
    let mut edge_storage_pcu = Vec::with_capacity(edge_count);
    let mut edge_flow_pcu_s = Vec::with_capacity(edge_count);
    for index in 0..edge_count {
        let routing = topology.routing_edge(index);
        let profile = topology.edge_profile(index);
        if routing.from.0 as usize >= node_count || routing.to.0 as usize >= node_count {
            bail!("edge {index} references a node outside the topology");
        }
        let length = (routing.length_m as f64).max(1.0);
        let lanes = equivalent_lanes(profile.road_class);
        edge_from.push(routing.from.0);
        edge_to.push(routing.to.0);
        edge_length_m.push(length as f32);
        edge_road_class.push(profile.road_class);
        edge_signal.push(routing.flags & EDGE_FLAG_TARGET_TRAFFIC_SIGNAL != 0);
        edge_storage_pcu.push((length * lanes / JAM_SPACING_M_PER_PCU).max(1.0) as f32);
        edge_flow_pcu_s.push((lanes * saturation_flow_pcu_s(profile.road_class)) as f32);
    }

    // Node -> outgoing edges (CSR), rebuilt locally so the network is usable
    // even when the bundle's edge-based topology is absent.
    let mut out_degree = vec![0u32; node_count + 1];
    for from in &edge_from {
        out_degree[*from as usize + 1] += 1;
    }
    for index in 1..out_degree.len() {
        out_degree[index] += out_degree[index - 1];
    }
    let node_first_out = out_degree.clone();
    let mut cursor = node_first_out.clone();
    let mut node_edges = vec![0u32; edge_count];
    for (edge, from) in edge_from.iter().enumerate() {
        let slot = cursor[*from as usize];
        node_edges[slot as usize] = edge as u32;
        cursor[*from as usize] += 1;
    }

    // Edge transitions: prefer the bundle's precomputed turn table.
    let ebt = &topology.edge_based_topology;
    let (trans_first, trans_edges) = if ebt.edge_transition_first_out.len() == edge_count + 1 {
        (
            ebt.edge_transition_first_out.clone(),
            ebt.edge_transition_edges.clone(),
        )
    } else {
        // Fallback: every outgoing edge at the target node, skipping the
        // immediate U-turn when another option exists.
        let mut first = Vec::with_capacity(edge_count + 1);
        let mut edges = Vec::new();
        first.push(0u32);
        for edge in 0..edge_count {
            let to = edge_to[edge];
            let from = edge_from[edge];
            let candidates_start = node_first_out[to as usize] as usize;
            let candidates_end = node_first_out[to as usize + 1] as usize;
            let candidates = &node_edges[candidates_start..candidates_end];
            let non_uturn = candidates
                .iter()
                .filter(|next| edge_to[**next as usize] != from)
                .count();
            for next in candidates {
                let is_uturn = edge_to[*next as usize] == from;
                if is_uturn && non_uturn > 0 {
                    continue;
                }
                edges.push(*next);
            }
            first.push(edges.len() as u32);
        }
        (first, edges)
    };

    // Per-fleet free-flow speeds from compiled metrics.
    let mut fleet_freeflow_mps = Vec::with_capacity(fleet_metrics.len());
    let mut fleet_max_speed_mps = Vec::with_capacity(fleet_metrics.len());
    for metrics in fleet_metrics {
        if metrics.edge_metrics.len() < edge_count {
            bail!(
                "compiled profile '{}' covers {} edges but the topology has {}",
                metrics.profile_id,
                metrics.edge_metrics.len(),
                edge_count
            );
        }
        let mut speeds = Vec::with_capacity(edge_count);
        let mut max_speed = 0.0f64;
        for (edge, metric) in metrics.edge_metrics.iter().take(edge_count).enumerate() {
            let speed = match metric.travel_time_s {
                Some(time) if time > 0.0 => {
                    let speed = edge_length_m[edge] as f64 / time;
                    max_speed = max_speed.max(speed);
                    speed as f32
                }
                _ => 0.0,
            };
            speeds.push(speed);
        }
        if max_speed <= 0.0 {
            bail!(
                "compiled profile '{}' has no traversable edges",
                metrics.profile_id
            );
        }
        fleet_freeflow_mps.push(speeds);
        fleet_max_speed_mps.push(max_speed);
    }

    let bounds = TopologyBounds {
        min_lon,
        min_lat,
        max_lon,
        max_lat,
    };

    Ok(Arc::new(SimNetwork {
        node_lon,
        node_lat,
        edge_from,
        edge_to,
        edge_length_m,
        edge_road_class,
        edge_signal,
        edge_storage_pcu,
        edge_flow_pcu_s,
        trans_first,
        trans_edges,
        node_first_out,
        node_edges,
        fleet_freeflow_mps,
        fleet_max_speed_mps,
        bounds,
    }))
}

/// Convenience wrapper so callers don't depend on field order.
pub fn network_bounds_bbox(network: &SimNetwork) -> [f64; 4] {
    [
        network.bounds.min_lon,
        network.bounds.min_lat,
        network.bounds.max_lon,
        network.bounds.max_lat,
    ]
}
