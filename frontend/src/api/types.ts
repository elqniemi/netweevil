// Minimal typings for the parts of the API the console reads directly.
// Analysis results are handled generically (see geo/features.ts).

export interface LabeledPoint {
  id: string;
  lon: number;
  lat: number;
  z?: number;
}

export interface TopologyBounds {
  min_lon: number;
  min_lat: number;
  max_lon: number;
  max_lat: number;
}

export interface ProfileInfo {
  profile_id: string;
  label: string;
  mode: string;
  defaults_pack: string;
  source_path: string;
  profile_hash: string;
  created_at: string;
  edge_count: number | null;
  default_returns: unknown;
}

export interface TransitFeedInfo {
  feed_id: string;
  source_path: string;
  service_start_date: string;
  service_days: number;
  agency_timezone: string;
  time_origin_unix_s: number;
  stop_count: number;
  route_count: number;
  trip_count: number;
  connection_count: number;
  bound_stop_count: number;
  transfer_profile_ids: string[];
}

export interface ServiceInfo {
  workspace_root: string;
  dataset: {
    dataset_id: string;
    label: string;
    source_path: string;
    source_sha256: string;
    imported_at: string;
    topology_bounds: TopologyBounds | null;
    node_count: number | null;
    edge_count: number | null;
    turn_count: number | null;
    connected_components: {
      kind: string;
      component_count: number;
      largest_component_node_count: number;
      largest_component_edge_count: number;
    } | null;
  };
  default_profile_id: string;
  loaded_profiles: ProfileInfo[];
  loaded_transit_feeds: TransitFeedInfo[];
  capabilities: {
    analyses: string[];
    geometry: string[];
    breakdown_metrics: string[];
    connectivity_policies: string[];
    failure_modes: string[];
  };
  engine: { route_engine: string; batch_engine: string; acceleration: string };
}

export interface ApiErrorBody {
  error: string;
  diagnostics?: unknown[];
}

export interface SimulationFleetStatus {
  fleet_id: string;
  mode: string;
  profile_id: string;
  requested: number;
  dispatched: number;
  dispatch_failed: number;
  active: number;
  arrived: number;
  reroutes: number;
}

export interface SimulationStatus {
  state: "pending" | "dispatching" | "running" | "paused" | "completed" | "cancelled" | "failed" | string;
  sim_time_s: number;
  duration_s: number;
  tick_s: number;
  agents_total: number;
  agents_active: number;
  agents_arrived: number;
  agents_pending: number;
  agents_failed: number;
  frames_available: number;
  frame_interval_s: number;
  edge_bins_available: number;
  edge_stats_interval_s: number;
  global_speed_factor: number;
  wall_time_ms: number;
  message: string;
  fleets: SimulationFleetStatus[];
}

export interface SimulationInfo {
  simulation_id: string;
  scenario_id: string;
  created_at: string;
  status: SimulationStatus;
  error?: string;
  summary?: unknown;
}

export interface SimulationFrame {
  t: number;
  active_total: number;
  agents: {
    id: number[];
    fleet: string[];
    lon: number[];
    lat: number[];
    speed_mps: number[];
    edge: number[];
  };
}

export interface SimulationFramesResponse {
  simulation_id: string;
  state: string;
  sim_time_s: number;
  frame_interval_s: number;
  frames_available: number;
  frames: SimulationFrame[];
}
