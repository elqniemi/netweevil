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

// --- Workspace setup ---

export interface WorkspaceLocation {
  key: string;
  path: string;
  purpose: string;
}

export interface WorkspaceConfig {
  dataset_id: string;
  default_profile: string;
  profiles: string[];
  transit_feeds: string[];
  updated_at?: string | null;
}

export interface DatasetSummary {
  dataset_id: string;
  label: string;
  source_path: string;
  source_format: string;
  source_size_bytes: number;
  imported_at: string;
  node_count: number | null;
  edge_count: number | null;
  has_acceleration: boolean;
  compiled_profile_ids: string[];
  active: boolean;
}

export interface ProfileFileInfo {
  path: string;
  file_name: string;
  source: "workspace" | "examples";
  profile_id: string | null;
  label: string | null;
  mode: string | null;
  error: string | null;
  active: boolean;
  is_default: boolean;
}

export interface TransitFeedSummary {
  feed_id: string;
  label: string;
  source_path: string;
  imported_at: string;
  service_start_date: string;
  service_days: number;
  agency_timezone: string;
  stop_count: number;
  route_count: number;
  trip_count: number;
  bundle_path: string;
  loaded: boolean;
  scenario_id: string | null;
}

export interface UploadInfo {
  name: string;
  path: string;
  size_bytes: number;
  kind: "osm" | "overture" | "gpkg" | "gtfs" | "profile" | "other";
}

export interface JobRecord {
  job_id: string;
  kind: string;
  label: string;
  state: "running" | "done" | "failed";
  stage: string;
  message: string;
  percent?: number;
  started_at: string;
  finished_at?: string;
  error?: string;
  result?: Record<string, unknown>;
}

export interface WorkspaceInfo {
  root: string;
  state_dir: string;
  config_path: string;
  locations: WorkspaceLocation[];
  loaded: boolean;
  active: WorkspaceConfig | null;
  saved: WorkspaceConfig | null;
  datasets: DatasetSummary[];
  profiles: ProfileFileInfo[];
  transit_feeds: TransitFeedSummary[];
  uploads: UploadInfo[];
  jobs: JobRecord[];
  example_profiles_dir: string | null;
}

export interface ProfileTemplate {
  template_id: string;
  label: string;
  mode: string;
  description: string;
  yaml: string;
}

// --- GTFS editor ---

export type TransitMode = "tram" | "subway" | "rail" | "bus" | "ferry" | "cable_car" | "gondola" | "funicular" | "coach" | "air" | "other";

export interface ScenarioStop {
  stop_id: string;
  name: string;
  lon: number;
  lat: number;
}

export interface ScenarioLineStop {
  stop_id: string;
  travel_s?: number | null;
  dwell_s?: number | null;
}

export interface ScenarioHeadwayWindow {
  start: string;
  end: string;
  headway_min: number;
}

export interface ScenarioService {
  days: [boolean, boolean, boolean, boolean, boolean, boolean, boolean];
  windows: ScenarioHeadwayWindow[];
  departures: string[];
}

export interface ScenarioLine {
  line_id: string;
  short_name: string;
  long_name: string;
  mode: TransitMode;
  color?: string | null;
  headsign?: string | null;
  stops: ScenarioLineStop[];
  average_speed_kph: number;
  default_dwell_s: number;
  bidirectional: boolean;
  services: ScenarioService[];
}

export interface GtfsScenario {
  schema_version: number;
  scenario_id: string;
  label: string;
  base_feed_id: string | null;
  output_feed_id: string | null;
  agency: { name: string; url: string; timezone: string };
  service_start_date: string | null;
  service_days: number | null;
  stops: ScenarioStop[];
  lines: ScenarioLine[];
  removed_route_ids: string[];
  created_at: string;
  updated_at: string;
}

export interface ScenarioSummary {
  stop_count: number;
  line_count: number;
  trip_count: number;
  stop_time_count: number;
}

export interface BuiltFeedInfo {
  feed_id: string;
  bundle_path: string;
  built_at: string;
  loaded: boolean;
  stop_count: number;
  route_count: number;
  trip_count: number;
}

export interface ScenarioInfo {
  scenario_id: string;
  label: string;
  base_feed_id: string | null;
  output_feed_id: string;
  path: string;
  export_path: string;
  stop_count: number;
  line_count: number;
  updated_at: string;
  base_loaded: boolean;
  built: BuiltFeedInfo | null;
  summary: ScenarioSummary | null;
  validation_error: string | null;
}

export interface ScenarioResponse {
  info: ScenarioInfo;
  scenario: GtfsScenario;
}

export interface FeedStop {
  stop_id: string;
  name: string;
  lon: number;
  lat: number;
}

export interface FeedRoute {
  route_id: string;
  short_name: string;
  long_name: string;
  mode: TransitMode;
  trip_count: number;
}

export interface PatternStop extends FeedStop {
  arrival_offset_s: number;
  departure_offset_s: number;
}

export interface RoutePattern {
  route_id: string;
  headsign: string;
  run_count: number;
  stops: PatternStop[];
}

// --- Network explorer ---

export interface NetworkEdgesMeta {
  dataset_id: string;
  profile_id: string;
  compare_profile_id: string | null;
  edge_count: number;
  truncated: boolean;
  ranges: Record<string, [number, number]>;
}
