import type { InputPoint, ResponseFormat, RouteInput, ToolId } from "../state/store";
import type { ServiceInfo } from "../api/types";
import { DEFAULT_TRANSIT_MODES, TRANSIT_MODE_OPTIONS, transitModeSelection } from "../state/transitModes";
import { parseClock } from "./clock";

export type FieldType = "number" | "text" | "select" | "multiselect" | "checkbox" | "datetime" | "json" | "list" | "clock";

export interface FieldOption {
  value: string;
  label: string;
}

export interface Field {
  key: string;
  label: string;
  type: FieldType;
  /** Default value when the form has no entry. */
  default?: unknown;
  options?: FieldOption[] | ((service: ServiceInfo | null) => FieldOption[]);
  step?: number;
  min?: number;
  hint?: string;
  placeholder?: string;
  /** Group heading rendered as a collapsible section. */
  group?: string;
  /** Field only rendered when this predicate holds for the current form. */
  when?: (form: Record<string, unknown>) => boolean;
}

export interface SlotDef {
  id: string;
  label: string;
  /** Maximum points; 1 = single point (replaced on click). */
  max?: number;
  /** Whether points in this slot carry a `kind` (waypoints). */
  kinds?: string[];
  /** Whether points carry a demand weight (betweenness). */
  weighted?: boolean;
  /** Whether points carry a service time and a time window (trip stops). */
  timed?: boolean;
  /** Whether points are resolved to the nearest OSM way (live traffic). */
  ways?: boolean;
  hint?: string;
}

/** A route with both ends placed, with its index in the shared routes list. */
export type StyleLayer = "line" | "point" | "polygon" | "network";

export interface PlacedRoute {
  route: RouteInput;
  index: number;
}

export interface BuildContext {
  form: Record<string, unknown>;
  points: (slot: string) => InputPoint[];
  /** Complete routes (origin and destination placed). */
  routes: PlacedRoute[];
  profileId: string | null;
  feedId: string | null;
  engineMode: string;
  service: ServiceInfo | null;
  viewportBbox: [number, number, number, number] | null;
  viewportZoom: number | null;
}

export interface ToolDef {
  id: ToolId;
  label: string;
  group: "street" | "batch" | "transit" | "simulation";
  path: string;
  method: "POST" | "GET";
  description: string;
  supportsGeojson: boolean;
  usesProfile: boolean;
  /** Whether the tool takes the shared start/end routes, its own point slots, the map view, or the GTFS editor. */
  input: "routes" | "slots" | "viewport" | "editor";
  /** Whether route vias (intermediate stops) are edited and used. */
  usesVias?: boolean;
  /** Run button text, when "Run <label>" does not fit. */
  runLabel?: string;
  /** Cheap enough to re-run live while points are dragged. */
  live?: boolean;
  /** Row set shown first in the table view, by result key (e.g. "pairs"). */
  tableKey?: string;
  /** Result layer kinds the tool draws, used to pick which style controls to show. */
  layers: StyleLayer[];
  slots: SlotDef[];
  fields: Field[];
  /** One request body for the whole run (slot tools, OD, scenario batch). */
  build?: (ctx: BuildContext) => unknown;
  /** One request body per placed route; all routes run concurrently. */
  buildEach?: (ctx: BuildContext, route: RouteInput, index: number) => unknown;
  /** Validation message when the request cannot be built yet. */
  ready: (ctx: BuildContext) => string | null;
}

// --- Shared field groups ---

const snapFields: Field[] = [
  { key: "snap_max_distance_m", label: "Snap distance (m)", type: "number", default: 500, min: 1, step: 50, group: "Snapping" },
  { key: "snap_z_window_m", label: "Elevation window (m)", type: "number", default: undefined, min: 0, step: 1, group: "Snapping", placeholder: "none" },
  {
    key: "snap_attribute_filters",
    label: "Attribute filters (JSON)",
    type: "json",
    default: "",
    group: "Snapping",
    placeholder: '{"indoor_location": "outdoor"}',
  },
];

const connectivityFields: Field[] = [
  {
    key: "disconnected",
    label: "Disconnected network",
    type: "select",
    default: "strict",
    group: "Connectivity",
    options: [
      { value: "strict", label: "strict" },
      { value: "ignore_unreachable", label: "ignore unreachable" },
      { value: "hop_origin_to_nearest_reachable_component", label: "hop origin" },
      { value: "hop_destination_to_nearest_reachable_component", label: "hop destination" },
      { value: "hop_either_end", label: "hop either end" },
    ],
  },
  { key: "max_hop_distance_m", label: "Max hop distance (m)", type: "number", min: 0, step: 100, group: "Connectivity", placeholder: "unbounded" },
];

const fallbackFields: Field[] = [
  { key: "auto_relax_unreachable", label: "Auto-relax when unreachable", type: "checkbox", default: false, group: "Fallback" },
  { key: "allow_reverse_oneway", label: "Allow reverse one-way", type: "checkbox", default: false, group: "Fallback" },
  { key: "allow_illegal_turn", label: "Allow illegal turns", type: "checkbox", default: false, group: "Fallback" },
  { key: "ignore_turn_restrictions", label: "Ignore turn restrictions", type: "checkbox", default: false, group: "Fallback" },
  { key: "allow_uturn_where_normally_forbidden", label: "Allow forbidden U-turns", type: "checkbox", default: false, group: "Fallback" },
];

const returnFields: Field[] = [
  {
    key: "geometry",
    label: "Geometry",
    type: "select",
    default: "full",
    group: "Returns",
    options: [
      { value: "full", label: "full" },
      { value: "segments", label: "segments" },
      { value: "none", label: "none (fast path)" },
    ],
  },
  { key: "segment_rows", label: "Segment rows", type: "checkbox", default: false, group: "Returns" },
  { key: "breakdowns", label: "Road type and surface breakdowns", type: "checkbox", default: false, group: "Returns" },
  { key: "penalty_breakdown", label: "Penalty breakdown", type: "checkbox", default: false, group: "Returns" },
  { key: "explain_cost_derivation", label: "Explain cost derivation", type: "checkbox", default: false, group: "Returns" },
  { key: "admin_areas", label: "Regions (countries, states, timezones)", type: "checkbox", default: false, group: "Returns" },
  { key: "elevation", label: "Elevation profile", type: "checkbox", default: false, group: "Returns", hint: "Heights need a dataset imported with a DEM; others report unknown heights." },
];

const temporalFields: Field[] = [
  { key: "departure_time", label: "Departure time", type: "datetime", group: "Time", placeholder: "2026-05-12T08:30:00+02:00" },
  { key: "arrive_by", label: "Arrive by", type: "datetime", group: "Time", placeholder: "2026-05-12T09:00:00+02:00" },
  {
    key: "scenario",
    label: "Scenario overlay (JSON)",
    type: "json",
    group: "Time",
    placeholder: '{"id": "closure", "features": [{"source_feature_id": 900002, "force_closed": true}]}',
    hint: "Per-feature overrides: force_closed, force_open, speed_factor, replace_rules, rules.",
  },
  { key: "holidays", label: "Holidays (comma-separated dates)", type: "list", group: "Time", placeholder: "2026-01-01, 2026-12-25" },
  {
    key: "overlays",
    label: "Overlays (JSON)",
    type: "json",
    group: "Time",
    placeholder: '[{"id": "shade", "rows": [{"feature_id": 900001, "t_start": "2026-07-10T09:30:00+08:00", "t_end": "2026-07-10T10:00:00+08:00", "shade_fraction": 0.75}]}]',
    hint: "List of overlay documents; each row is feature_id, t_start, t_end (RFC 3339) plus named numeric values. Later overlays take precedence.",
  },
  { key: "max_labels_per_state", label: "Max labels per state", type: "number", min: 1, step: 1, group: "Time", placeholder: "default" },
];

export const TRAFFIC_OPTIONS: FieldOption[] = [
  { value: "auto", label: "auto (historical + live)" },
  { value: "historical", label: "historical only" },
  { value: "time_dependent", label: "exact time-dependent (slow)" },
  { value: "off", label: "off" },
];

/** `traffic` of a timed request; batches cannot use the exact time-dependent engine. */
function trafficField(batch: boolean): Field {
  return {
    key: "traffic",
    label: "Traffic",
    type: "select",
    default: "auto",
    group: "Time",
    options: batch ? TRAFFIC_OPTIONS.filter((o) => o.value !== "time_dependent") : TRAFFIC_OPTIONS,
    hint: batch
      ? "Applies with a departure time: auto uses the departure-bucket speeds plus live updates, historical skips live updates."
      : "Applies with a departure time: auto uses the departure-bucket speeds plus live updates, historical skips live updates, exact integrates traffic along every edge.",
  };
}

const avoidFields: Field[] = [
  {
    key: "avoid",
    label: "Avoid (JSON)",
    type: "json",
    group: "Avoid",
    placeholder:
      '{"points": [{"lon": 6.56, "lat": 53.21, "radius_m": 25, "penalty_factor": 3.0}], "polygons": [{"rings": [[[6.55, 53.2], [6.57, 53.2], [6.57, 53.22], [6.55, 53.2]]]}], "source_feature_ids": [123456]}',
    hint: "points (radius_m defaults to 10), polygons (GeoJSON rings, outer first) and source_feature_ids. Omit penalty_factor to exclude matching edges entirely; a number in [1, 1000] multiplies their cost.",
  },
];

const alternativeFields: Field[] = [
  { key: "alt_max_routes", label: "Max routes (1 = best only)", type: "number", default: 1, min: 1, step: 1, group: "Alternatives" },
  { key: "alt_max_cost_ratio", label: "Max cost ratio", type: "number", default: 1.5, min: 1, step: 0.1, group: "Alternatives", when: (f) => Number(f.alt_max_routes ?? 1) > 1 },
  { key: "alt_min_jaccard_distance", label: "Min Jaccard distance", type: "number", default: 0.2, min: 0, step: 0.05, group: "Alternatives", when: (f) => Number(f.alt_max_routes ?? 1) > 1 },
  { key: "alt_max_search_attempts", label: "Max search attempts", type: "number", min: 1, step: 1, group: "Alternatives", placeholder: "default", when: (f) => Number(f.alt_max_routes ?? 1) > 1 },
];

// --- Helpers ---

export function fieldDefaults(tool: ToolDef): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const field of tool.fields) if (field.default !== undefined) out[field.key] = field.default;
  return out;
}

function num(form: Record<string, unknown>, key: string): number | undefined {
  const v = form[key];
  if (v === undefined || v === null || v === "") return undefined;
  const n = Number(v);
  return Number.isFinite(n) ? n : undefined;
}

function str(form: Record<string, unknown>, key: string): string | undefined {
  const v = form[key];
  if (v === undefined || v === null) return undefined;
  const s = String(v).trim();
  return s === "" ? undefined : s;
}

function bool(form: Record<string, unknown>, key: string): boolean {
  return form[key] === true || form[key] === "true";
}

function parseJson(form: Record<string, unknown>, key: string): unknown {
  const s = str(form, key);
  if (!s) return undefined;
  try {
    return JSON.parse(s);
  } catch {
    throw new Error(`${key} is not valid JSON`);
  }
}

function list(form: Record<string, unknown>, key: string): string[] {
  const s = str(form, key);
  if (!s) return [];
  return s
    .split(",")
    .map((x) => x.trim())
    .filter(Boolean);
}

function numberList(form: Record<string, unknown>, key: string): number[] {
  return list(form, key)
    .map(Number)
    .filter((n) => Number.isFinite(n));
}

function labeled(p: InputPoint) {
  const out: Record<string, unknown> = { id: p.id, lon: round(p.lon), lat: round(p.lat) };
  if (p.z !== undefined) out.z = p.z;
  return out;
}

function round(v: number) {
  return Math.round(v * 1e6) / 1e6;
}

function snap(form: Record<string, unknown>) {
  const out: Record<string, unknown> = { max_distance_m: num(form, "snap_max_distance_m") ?? 500 };
  const z = num(form, "snap_z_window_m");
  if (z !== undefined) out.z_window_m = z;
  const filters = parseJson(form, "snap_attribute_filters");
  if (filters) out.attribute_filters = filters;
  return out;
}

function connectivity(form: Record<string, unknown>) {
  const out: Record<string, unknown> = { disconnected: str(form, "disconnected") ?? "strict" };
  const hop = num(form, "max_hop_distance_m");
  if (hop !== undefined) out.max_hop_distance_m = hop;
  return out;
}

function fallback(form: Record<string, unknown>) {
  const out: Record<string, unknown> = {};
  for (const f of fallbackFields) if (bool(form, f.key)) out[f.key] = true;
  return out;
}

function returns(form: Record<string, unknown>) {
  const breakdown = bool(form, "breakdowns") ? ["time_s", "distance_m"] : [];
  const out: Record<string, unknown> = {
    geometry: str(form, "geometry") ?? "full",
    segment_rows: bool(form, "segment_rows"),
    road_type_breakdown: breakdown,
    surface_breakdown: breakdown,
    penalty_breakdown: bool(form, "penalty_breakdown"),
    explain_cost_derivation: bool(form, "explain_cost_derivation"),
  };
  if (bool(form, "admin_areas")) out.admin_areas = true;
  if (bool(form, "elevation")) out.elevation = true;
  return out;
}

function temporal(form: Record<string, unknown>) {
  const out: Record<string, unknown> = {};
  const dep = str(form, "departure_time");
  const arr = str(form, "arrive_by");
  if (dep) out.departure_time = dep;
  if (arr) out.arrive_by = arr;
  const scenario = parseJson(form, "scenario");
  if (scenario) out.scenario = scenario;
  const holidays = list(form, "holidays");
  if (holidays.length) out.holidays = holidays;
  const overlays = parseJson(form, "overlays");
  if (overlays) out.overlays = Array.isArray(overlays) ? overlays : [overlays];
  const labels = num(form, "max_labels_per_state");
  if (labels !== undefined) out.max_labels_per_state = labels;
  const traffic = str(form, "traffic");
  if (traffic && traffic !== "auto") out.traffic = traffic;
  return out;
}

/** Request-level avoid areas (`{avoid: ...}` or nothing), flattened next to the temporal fields. */
function avoid(form: Record<string, unknown>) {
  const value = parseJson(form, "avoid");
  return value ? { avoid: value } : {};
}

function alternatives(form: Record<string, unknown>) {
  const maxRoutes = num(form, "alt_max_routes") ?? 1;
  if (maxRoutes <= 1) return undefined;
  const out: Record<string, unknown> = {
    max_routes: maxRoutes,
    max_cost_ratio: num(form, "alt_max_cost_ratio") ?? 1.5,
    min_jaccard_distance: num(form, "alt_min_jaccard_distance") ?? 0.2,
  };
  const attempts = num(form, "alt_max_search_attempts");
  if (attempts !== undefined) out.max_search_attempts = attempts;
  return out;
}

function envelope(ctx: BuildContext, request: unknown, withEngine = true) {
  const out: Record<string, unknown> = {};
  if (ctx.profileId) out.profile_id = ctx.profileId;
  if (withEngine && ctx.engineMode !== "auto") out.engine_mode = ctx.engineMode;
  out.request = request;
  return out;
}

function needPoints(ctx: BuildContext, slot: string, min: number, label: string): string | null {
  return ctx.points(slot).length >= min ? null : `Place ${min === 1 ? "a" : `at least ${min}`} ${label} on the map`;
}

function needRoute(ctx: BuildContext): string | null {
  return ctx.routes.length ? null : "Click the map to place a start and an end point";
}

/** Route id from the form, suffixed with the route number when several routes run. */
function routeId(ctx: BuildContext, key: string, fallback: string, index: number) {
  const base = str(ctx.form, key) ?? fallback;
  return ctx.routes.length > 1 ? `${base}_${index + 1}` : base;
}

function pointSet(ctx: BuildContext, slot: string, opts: { geometry?: boolean; avoid?: boolean } = {}) {
  const form = ctx.form;
  return {
    points: ctx.points(slot).map(labeled),
    snap: snap(form),
    connectivity: connectivity(form),
    fallback: fallback(form),
    returns: { geometry: opts.geometry === false ? "none" : (str(form, "geometry") ?? "none") },
    ...temporal(form),
    ...(opts.avoid ? avoid(form) : {}),
  };
}

// --- Tool definitions ---

const routeFields: Field[] = [
  { key: "route_id", label: "Route id", type: "text", default: "console_route" },
  ...snapFields,
  ...connectivityFields,
  ...fallbackFields,
  ...returnFields,
  ...alternativeFields,
  ...temporalFields,
  trafficField(false),
  ...avoidFields,
  {
    key: "component_constraints",
    label: "Component constraints (JSON)",
    type: "json",
    group: "Multi-criteria",
    placeholder: '[{"component": "shade_fraction", "max_value": 0.4}]',
  },
  { key: "pareto", label: "Pareto options (JSON)", type: "json", group: "Multi-criteria", placeholder: '{"component": "shade_fraction", "max_routes": 4}' },
  {
    key: "profile_overrides",
    label: "Profile overrides (JSON)",
    type: "json",
    group: "Request-defined profile",
    placeholder: '{"preferences": {"use_highways": 0.2}, "turns": {"left_penalty_s": 12}}',
    hint: "Merged over the selected profile; the API compiles and caches the result.",
  },
  { key: "profile_inline", label: "Inline profile document (JSON)", type: "json", group: "Request-defined profile", placeholder: '{"profile": {"id": "...", "mode": "car", ...}}' },
];

const guidanceFields: Field[] = [
  {
    key: "language",
    label: "Language",
    type: "select",
    default: "en",
    group: "Guidance",
    options: [
      { value: "en", label: "English" },
      { value: "nl", label: "Nederlands" },
      { value: "de", label: "Deutsch" },
      { value: "fr", label: "Français" },
      { value: "fi", label: "Suomi" },
      { value: "zh-HK", label: "繁體中文（香港）" },
    ],
  },
  {
    key: "units",
    label: "Units",
    type: "select",
    default: "metric",
    group: "Guidance",
    options: [
      { value: "metric", label: "metric" },
      { value: "imperial", label: "imperial" },
    ],
  },
  {
    key: "driving_side",
    label: "Driving side",
    type: "select",
    default: "",
    group: "Guidance",
    options: [
      { value: "", label: "dataset default" },
      { value: "right", label: "right-hand traffic" },
      { value: "left", label: "left-hand traffic" },
    ],
  },
];

/** `directions_options` of /v1/directions. */
function directionsOptions(form: Record<string, unknown>) {
  const out: Record<string, unknown> = {};
  const language = str(form, "language");
  if (language) out.language = language;
  const units = str(form, "units");
  if (units) out.units = units;
  const side = str(form, "driving_side");
  if (side) out.driving_side = side;
  return out;
}

function buildRoute(ctx: BuildContext, route: RouteInput, index: number, directions: boolean) {
  const form = ctx.form;
  const request: Record<string, unknown> = {
    route_id: routeId(ctx, "route_id", directions ? "console_directions" : "console_route", index),
    origin: labeled(route.origin!),
    destination: labeled(route.destination!),
    snap: snap(form),
    connectivity: connectivity(form),
    fallback: fallback(form),
    returns: returns(form),
    ...avoid(form),
  };
  Object.assign(request, temporal(form));
  if (!directions) {
    const alt = alternatives(form);
    if (alt) request.alternatives = alt;
    const constraints = parseJson(form, "component_constraints");
    if (constraints) request.constraints = constraints;
    const pareto = parseJson(form, "pareto");
    if (pareto) request.pareto = pareto;
  }
  const body = envelope(ctx, request, !directions) as Record<string, unknown>;
  const overrides = parseJson(form, "profile_overrides");
  if (overrides) body.profile_overrides = overrides;
  const inline = parseJson(form, "profile_inline");
  if (inline) body.profile = inline;
  if (directions) body.directions_options = directionsOptions(form);
  return body;
}

const routeTool: ToolDef = {
  id: "route",
  label: "Route",
  group: "street",
  path: "/v1/route",
  layers: ["line", "point"],
  method: "POST",
  description: "Point-to-point street route with alternatives, time dependence, fallbacks and request-defined profiles.",
  supportsGeojson: true,
  usesProfile: true,
  input: "routes",
  live: true,
  tableKey: "alternatives",
  slots: [],
  fields: routeFields,
  buildEach: (ctx, route, index) => buildRoute(ctx, route, index, false),
  ready: needRoute,
};

const directionsTool: ToolDef = {
  id: "directions",
  label: "Directions",
  group: "street",
  path: "/v1/directions",
  layers: ["line", "point"],
  method: "POST",
  description: "Turn-by-turn maneuvers with lanes, signposts and spoken instructions in six languages, for static or time-dependent routes.",
  supportsGeojson: true,
  usesProfile: true,
  input: "routes",
  live: true,
  tableKey: "maneuvers",
  slots: [],
  fields: [{ key: "route_id", label: "Route id", type: "text", default: "console_directions" }, ...guidanceFields, ...snapFields, ...connectivityFields, ...fallbackFields, ...avoidFields, ...temporalFields],
  buildEach: (ctx, route, index) => buildRoute(ctx, route, index, true),
  ready: needRoute,
};

const locateTool: ToolDef = {
  id: "locate",
  label: "Locate",
  group: "street",
  path: "/v1/locate",
  layers: ["point"],
  method: "POST",
  description: "Profile-aware snap candidates for each point: snapped position, edge, fraction, component and distance.",
  supportsGeojson: false,
  usesProfile: true,
  input: "slots",
  live: true,
  tableKey: "points",
  slots: [{ id: "points", label: "Points", hint: "Each click adds a point." }],
  fields: [
    {
      key: "direction",
      label: "Direction",
      type: "select",
      default: "origin",
      options: [
        { value: "origin", label: "origin (outgoing)" },
        { value: "destination", label: "destination (incoming)" },
      ],
    },
    ...snapFields,
  ],
  build: (ctx) => ({
    ...(ctx.profileId ? { profile_id: ctx.profileId } : {}),
    request: { points: ctx.points("points").map(labeled), snap: snap(ctx.form), direction: str(ctx.form, "direction") ?? "origin" },
  }),
  ready: (ctx) => needPoints(ctx, "points", 1, "point"),
};

const waypointsTool: ToolDef = {
  id: "waypoints",
  label: "Waypoints",
  group: "street",
  path: "/v1/waypoints",
  layers: ["line", "point"],
  method: "POST",
  description: "Ordered multi-stop route. Break stops split legs; through stops keep turn history. Optionally optimise the stop order.",
  supportsGeojson: true,
  usesProfile: true,
  input: "routes",
  usesVias: true,
  live: true,
  tableKey: "legs",
  slots: [],
  fields: [
    { key: "route_id", label: "Route id", type: "text", default: "console_waypoints" },
    { key: "optimize_order", label: "Optimise stop order (Held-Karp, up to 16 breaks)", type: "checkbox", default: false },
    ...snapFields,
    ...returnFields,
    ...temporalFields.slice(0, 1),
    ...avoidFields,
  ],
  buildEach: (ctx, route, index) => {
    const pts = [route.origin!, ...route.vias, route.destination!];
    const waypoints = pts.map((p, i) => {
      const entry: Record<string, unknown> = { point: labeled(p) };
      if (i > 0 && i < pts.length - 1 && p.kind) entry.kind = p.kind;
      return entry;
    });
    const request: Record<string, unknown> = {
      route_id: routeId(ctx, "route_id", "console_waypoints", index),
      waypoints,
      snap: snap(ctx.form),
      optimize_order: bool(ctx.form, "optimize_order"),
      returns: returns(ctx.form),
      ...temporal(ctx.form),
      ...avoid(ctx.form),
    };
    return envelope(ctx, request, false);
  },
  ready: needRoute,
};

const odTool: ToolDef = {
  id: "od",
  label: "OD pairs",
  group: "batch",
  path: "/v1/od",
  layers: ["line", "point"],
  method: "POST",
  description: "Batch of origin-destination pairs sharing snap, connectivity and return settings.",
  supportsGeojson: true,
  usesProfile: true,
  input: "routes",
  live: true,
  tableKey: "pairs",
  slots: [],
  fields: [
    ...snapFields,
    ...connectivityFields,
    ...fallbackFields,
    {
      key: "geometry",
      label: "Geometry",
      type: "select",
      default: "full",
      group: "Returns",
      options: [
        { value: "full", label: "full" },
        { value: "none", label: "none (fast path)" },
      ],
    },
    ...alternativeFields,
    ...temporalFields,
    trafficField(true),
    ...avoidFields,
  ],
  build: (ctx) => {
    const pairs = ctx.routes.map(({ route, index }) => ({ pair_id: `pair_${index + 1}`, origin: labeled(route.origin!), destination: labeled(route.destination!) }));
    const request: Record<string, unknown> = {
      pairs,
      snap: snap(ctx.form),
      connectivity: connectivity(ctx.form),
      fallback: fallback(ctx.form),
      returns: { geometry: str(ctx.form, "geometry") ?? "full" },
      ...temporal(ctx.form),
      ...avoid(ctx.form),
    };
    const alt = alternatives(ctx.form);
    if (alt) request.alternatives = alt;
    return envelope(ctx, request);
  },
  ready: needRoute,
};

const matrixTool: ToolDef = {
  id: "matrix",
  label: "Matrix",
  group: "batch",
  path: "/v1/matrix",
  layers: ["line", "point"],
  tableKey: "cells",
  live: true,
  method: "POST",
  description: "Many-to-many travel time and distance matrix between two point sets.",
  supportsGeojson: true,
  usesProfile: true,
  input: "slots",
  slots: [
    { id: "origins", label: "Origins" },
    { id: "destinations", label: "Destinations" },
  ],
  fields: [
    ...snapFields,
    ...connectivityFields,
    ...fallbackFields,
    {
      key: "geometry",
      label: "Geometry",
      type: "select",
      default: "none",
      group: "Returns",
      options: [
        { value: "none", label: "none (fast path)" },
        { value: "full", label: "full (draws every cell)" },
      ],
    },
    ...temporalFields,
    trafficField(true),
    ...avoidFields,
  ],
  build: (ctx) =>
    envelope(ctx, {
      origins: pointSet(ctx, "origins", { avoid: true }),
      destinations: pointSet(ctx, "destinations"),
    }),
  ready: (ctx) => needPoints(ctx, "origins", 1, "origin") ?? needPoints(ctx, "destinations", 1, "destination"),
};

const accessibilityTool: ToolDef = {
  id: "accessibility",
  label: "Accessibility",
  group: "batch",
  path: "/v1/accessibility",
  layers: ["point"],
  tableKey: "rows",
  method: "POST",
  description: "Per-origin counts of reachable destinations within travel-time thresholds, by category.",
  supportsGeojson: false,
  usesProfile: true,
  input: "slots",
  slots: [
    { id: "origins", label: "Origins" },
    { id: "destinations", label: "Destinations (one category)" },
  ],
  fields: [
    { key: "category_id", label: "Category id", type: "text", default: "destinations" },
    { key: "thresholds_s", label: "Thresholds (s, comma-separated)", type: "text", default: "300, 600, 900" },
    { key: "max_travel_time_s", label: "Max travel time (s)", type: "number", default: 900, min: 1, step: 60 },
    ...snapFields,
    ...connectivityFields,
    ...temporalFields,
    ...avoidFields,
  ],
  build: (ctx) =>
    envelope(ctx, {
      origins: pointSet(ctx, "origins", { geometry: false, avoid: true }),
      categories: [
        {
          category_id: str(ctx.form, "category_id") ?? "destinations",
          destinations: pointSet(ctx, "destinations", { geometry: false }),
        },
      ],
      thresholds_s: numberList(ctx.form, "thresholds_s"),
      max_travel_time_s: num(ctx.form, "max_travel_time_s") ?? 900,
    }),
  ready: (ctx) => needPoints(ctx, "origins", 1, "origin") ?? needPoints(ctx, "destinations", 1, "destination"),
};

const serviceAreaFields: Field[] = [
  { key: "analysis_id", label: "Analysis id", type: "text", default: "console_service_area" },
  { key: "thresholds", label: "Thresholds (comma-separated)", type: "text", default: "300, 600, 900" },
  {
    key: "metric",
    label: "Threshold metric",
    type: "select",
    default: "travel_time_s",
    options: [
      { value: "travel_time_s", label: "travel time (s)" },
      { value: "distance_m", label: "distance (m)" },
    ],
  },
  {
    key: "output_mode",
    label: "Output",
    type: "select",
    default: "polygon",
    options: [
      { value: "polygon", label: "polygon" },
      { value: "network", label: "network" },
      { value: "both", label: "both" },
    ],
  },
  {
    key: "band_mode",
    label: "Bands",
    type: "select",
    default: "cumulative",
    options: [
      { value: "cumulative", label: "cumulative" },
      { value: "ring", label: "ring" },
      { value: "none", label: "none" },
    ],
  },
  {
    key: "boundary_mode",
    label: "Boundary",
    type: "select",
    default: "overlap",
    options: [
      { value: "overlap", label: "overlap" },
      { value: "cut_at_boundary", label: "cut at boundary" },
    ],
  },
  {
    key: "multi_origin_mode",
    label: "Multiple origins",
    type: "select",
    default: "merge",
    options: [
      { value: "merge", label: "merge" },
      { value: "overlap", label: "overlap" },
      { value: "cut", label: "cut" },
    ],
  },
  {
    key: "direction",
    label: "Direction",
    type: "select",
    default: "outbound",
    options: [
      { value: "outbound", label: "from origins" },
      { value: "inbound", label: "to origins" },
    ],
    hint: "To origins: every place that reaches an origin within the thresholds.",
  },
  { key: "hull_aggressiveness", label: "Hull aggressiveness", type: "number", default: 1, min: 0, step: 0.1, group: "Polygon" },
  { key: "simplification_tolerance_m", label: "Simplification (m)", type: "number", min: 0, step: 5, group: "Polygon", placeholder: "none" },
  { key: "cell_size_m", label: "Cell size (m)", type: "number", min: 1, step: 10, group: "Polygon", placeholder: "auto" },
  { key: "denoise", label: "Denoise (fraction of largest part)", type: "number", min: 0, step: 0.1, group: "Polygon", placeholder: "keep all", hint: "0..1; 1 keeps only the largest part" },
  { key: "min_part_area_m2", label: "Min part area (m²)", type: "number", min: 0, step: 1000, group: "Polygon", placeholder: "keep all", hint: "Drops polygon parts smaller than this." },
  { key: "min_hole_area_m2", label: "Min hole area (m²)", type: "number", min: 0, step: 1000, group: "Polygon", placeholder: "keep all", hint: "Fills holes smaller than this." },
  { key: "segments", label: "Return segments", type: "checkbox", default: false, group: "Returns" },
  { key: "max_features", label: "Max features", type: "number", min: 1, step: 100, group: "Returns", placeholder: "default" },
  { key: "max_geometry_points", label: "Max geometry points", type: "number", min: 1, step: 1000, group: "Returns", placeholder: "default" },
  ...snapFields,
  ...connectivityFields,
  ...fallbackFields,
];

function serviceAreaRequest(ctx: BuildContext) {
  const form = ctx.form;
  const metric = str(form, "metric") ?? "travel_time_s";
  const polygon: Record<string, unknown> = { hull_aggressiveness: num(form, "hull_aggressiveness") ?? 1 };
  const simplification = num(form, "simplification_tolerance_m");
  if (simplification !== undefined) polygon.simplification_tolerance_m = simplification;
  const cell = num(form, "cell_size_m");
  if (cell !== undefined) polygon.cell_size_m = cell;
  const denoise = num(form, "denoise");
  if (denoise !== undefined) polygon.denoise = denoise;
  const minPart = num(form, "min_part_area_m2");
  if (minPart !== undefined) polygon.min_part_area_m2 = minPart;
  const minHole = num(form, "min_hole_area_m2");
  if (minHole !== undefined) polygon.min_hole_area_m2 = minHole;
  const returns: Record<string, unknown> = { geometry: true, attributes: true, per_threshold_summary: true, diagnostics: true, segments: bool(form, "segments") };
  const maxFeatures = num(form, "max_features");
  if (maxFeatures !== undefined) returns.max_features = maxFeatures;
  const maxPoints = num(form, "max_geometry_points");
  if (maxPoints !== undefined) returns.max_geometry_points = maxPoints;
  return {
    analysis_id: str(form, "analysis_id") ?? "console_service_area",
    origins: ctx.points("origins").map(labeled),
    thresholds: numberList(form, "thresholds").map((limit, i) => ({ id: `t${i + 1}_${limit}`, limit, metric })),
    snap: snap(form),
    connectivity: connectivity(form),
    fallback: fallback(form),
    output_mode: str(form, "output_mode") ?? "polygon",
    band_mode: str(form, "band_mode") ?? "cumulative",
    boundary_mode: str(form, "boundary_mode") ?? "overlap",
    multi_origin_mode: str(form, "multi_origin_mode") ?? "merge",
    direction: str(form, "direction") ?? "outbound",
    polygon,
    returns,
    ...temporal(form),
    ...avoid(form),
  };
}

const serviceAreaTool: ToolDef = {
  id: "service_area",
  label: "Service area",
  group: "batch",
  path: "/v1/service-area",
  layers: ["polygon", "network", "point"],
  tableKey: "features",
  method: "POST",
  description: "Isochrone or isodistance polygons and reachable network from one or more origins.",
  supportsGeojson: true,
  usesProfile: true,
  input: "slots",
  slots: [{ id: "origins", label: "Origins" }],
  fields: [...serviceAreaFields, ...temporalFields, trafficField(true), ...avoidFields],
  build: (ctx) => envelope(ctx, serviceAreaRequest(ctx), false),
  ready: (ctx) => needPoints(ctx, "origins", 1, "origin"),
};

const serviceAreaSequenceTool: ToolDef = {
  id: "service_area_sequence",
  label: "Area sequence",
  group: "batch",
  path: "/v1/service-area-sequence",
  layers: ["polygon", "network", "point"],
  tableKey: "features",
  method: "POST",
  description: "Time-dependent service areas replayed over a range of departure times.",
  supportsGeojson: true,
  usesProfile: true,
  input: "slots",
  slots: [{ id: "origins", label: "Origins" }],
  fields: [
    { key: "sequence_id", label: "Sequence id", type: "text", default: "console_sequence" },
    { key: "start_time", label: "Start time", type: "datetime", default: "2026-05-12T07:00:00+02:00" },
    { key: "end_time", label: "End time", type: "datetime", default: "2026-05-12T09:00:00+02:00" },
    { key: "step_s", label: "Step (s)", type: "number", default: 1800, min: 60, step: 300 },
    { key: "departure_times", label: "Explicit departure times (comma-separated, overrides range)", type: "text", placeholder: "" },
    ...serviceAreaFields,
    ...temporalFields.slice(2),
    ...avoidFields,
  ],
  build: (ctx) => {
    const request = serviceAreaRequest(ctx);
    const explicit = list(ctx.form, "departure_times");
    const body: Record<string, unknown> = {
      sequence_id: str(ctx.form, "sequence_id") ?? "console_sequence",
      request,
    };
    if (explicit.length) body.departure_times = explicit;
    else {
      body.start_time = str(ctx.form, "start_time");
      body.end_time = str(ctx.form, "end_time");
      body.step_s = num(ctx.form, "step_s") ?? 1800;
    }
    return envelope(ctx, body, false);
  },
  ready: (ctx) => needPoints(ctx, "origins", 1, "origin"),
};

const betweennessTool: ToolDef = {
  id: "betweenness",
  label: "Betweenness",
  group: "batch",
  path: "/v1/betweenness",
  layers: ["line"],
  tableKey: "edges",
  method: "POST",
  description: "Demand-weighted edge betweenness from all origin-destination combinations.",
  supportsGeojson: true,
  usesProfile: true,
  input: "slots",
  slots: [
    { id: "origins", label: "Origins", weighted: true },
    { id: "destinations", label: "Destinations", weighted: true },
  ],
  fields: [
    { key: "analysis_id", label: "Analysis id", type: "text", default: "console_betweenness" },
    { key: "max_od_pairs", label: "Max OD pairs", type: "number", default: 10000, min: 1, step: 1000 },
    { key: "include_zero", label: "Include zero-score edges", type: "checkbox", default: false },
    ...snapFields,
    ...temporalFields,
    ...avoidFields,
  ],
  build: (ctx) => {
    const weighted = (p: InputPoint) => ({ ...labeled(p), weight: p.weight ?? 1 });
    return envelope(
      ctx,
      {
        analysis_id: str(ctx.form, "analysis_id") ?? "console_betweenness",
        origins: ctx.points("origins").map(weighted),
        destinations: ctx.points("destinations").map(weighted),
        snap: snap(ctx.form),
        max_od_pairs: num(ctx.form, "max_od_pairs") ?? 10000,
        include_zero: bool(ctx.form, "include_zero"),
        ...temporal(ctx.form),
        ...avoid(ctx.form),
      },
      false,
    );
  },
  ready: (ctx) => needPoints(ctx, "origins", 1, "origin") ?? needPoints(ctx, "destinations", 1, "destination"),
};

const scenarioBatchTool: ToolDef = {
  id: "scenario_batch",
  label: "Scenario batch",
  group: "batch",
  path: "/v1/scenario-batch",
  layers: ["line", "polygon", "point"],
  tableKey: "routes",
  method: "POST",
  description: "Baseline versus scenario cases (inline overlay, top-k closures, attribute group) for routes and service areas in one call. Leave cases blank for a baseline-only run; edit the JSON to add OD, matrix or betweenness analyses.",
  supportsGeojson: true,
  usesProfile: true,
  input: "routes",
  slots: [{ id: "origins", label: "Service-area origins", hint: "Optional; routes come from the shared start/end list." }],
  fields: [
    { key: "batch_id", label: "Batch id", type: "text", default: "console_batch" },
    { key: "departure_time", label: "Departure time", type: "datetime", default: "2026-05-12T08:30:00+02:00" },
    {
      key: "scenario_overlay",
      label: "Scenario overlay (JSON, blank = baseline only)",
      type: "json",
      placeholder: '{"id": "closure", "features": [{"source_feature_id": 900002, "force_closed": true}]}',
    },
    {
      key: "ranking",
      label: "Feature ranking (JSON)",
      type: "json",
      group: "Top-k closures",
      placeholder: '[{"source_feature_id": 900002, "score": 9.0}, {"source_feature_id": 900001, "score": 4.5}]',
      hint: "The highest-scoring features are closed; scores of repeated features are summed.",
    },
    { key: "top_k", label: "Closures to apply", type: "number", default: 3, min: 1, step: 1, group: "Top-k closures" },
    { key: "group_attribute", label: "Attribute", type: "text", group: "Attribute group closure", placeholder: "bridge" },
    { key: "group_value", label: "Value", type: "text", group: "Attribute group closure", placeholder: "yes" },
    { key: "thresholds", label: "Service-area thresholds (s)", type: "text", default: "600" },
    ...snapFields,
  ],
  build: (ctx) => {
    const form = ctx.form;
    const scenarios: Record<string, unknown>[] = [];
    const overlay = parseJson(form, "scenario_overlay");
    if (overlay) scenarios.push({ id: "overlay_case", overlay });
    const ranking = parseJson(form, "ranking");
    const topK = num(form, "top_k") ?? 3;
    if (ranking) scenarios.push({ id: `top_${topK}_closures`, top_k: { ranking, count: topK } });
    const attribute = str(form, "group_attribute");
    const value = str(form, "group_value");
    if (attribute && value) scenarios.push({ id: `close_${attribute}_${value}`, group: { attribute, value } });
    const routes = ctx.routes.map(({ route, index }) => ({
      route_id: `console_route_${index + 1}`,
      origin: labeled(route.origin!),
      destination: labeled(route.destination!),
      snap: snap(form),
      returns: { geometry: "full" },
    }));
    const serviceAreas: unknown[] = [];
    const origins = ctx.points("origins");
    if (origins.length) {
      serviceAreas.push({
        analysis_id: "console_service_area",
        origins: origins.map(labeled),
        thresholds: numberList(form, "thresholds").map((limit) => ({ limit, metric: "travel_time_s" })),
        snap: snap(form),
        output_mode: "polygon",
        returns: { geometry: true },
      });
    }
    return envelope(
      ctx,
      {
        batch_id: str(form, "batch_id") ?? "console_batch",
        departure_time: str(form, "departure_time"),
        scenarios,
        routes,
        service_areas: serviceAreas,
        accessibility: [],
        od: [],
        matrices: [],
        betweenness: [],
      },
      false,
    );
  },
  ready: (ctx) => (ctx.routes.length || ctx.points("origins").length ? null : "Place a start and end point, or service-area origins"),
};

const accessModeOptions: FieldOption[] = [
  { value: "walk", label: "walk" },
  { value: "bicycle", label: "bicycle" },
  { value: "car", label: "car" },
  { value: "walk,bicycle", label: "walk + bicycle" },
  { value: "walk,car", label: "walk + car" },
  { value: "walk,bicycle,car", label: "all" },
];

const transitModeFields = (): Field[] => [
  { key: "datetime", label: "Date and time (feed timezone)", type: "datetime", default: "2026-05-12T08:30:00+02:00", group: "Time" },
  { key: "arrive_by_flag", label: "Treat time as arrival", type: "checkbox", default: false, group: "Time" },
  { key: "search_window_s", label: "Search window (s)", type: "number", default: 3600, min: 60, step: 300, group: "Time" },
  { key: "access", label: "Access modes", type: "select", default: "walk", options: accessModeOptions, group: "Street access" },
  { key: "egress", label: "Egress modes", type: "select", default: "walk", options: accessModeOptions, group: "Street access" },
  {
    key: "street_access",
    label: "Street access model",
    type: "select",
    default: "straight_line",
    group: "Street access",
    options: [
      { value: "straight_line", label: "straight line" },
      { value: "network", label: "network (uses loaded street profiles)" },
    ],
  },
  { key: "walk_speed_kph", label: "Walk speed (km/h)", type: "number", min: 0.5, step: 0.5, group: "Street access", placeholder: "default" },
  { key: "max_access_distance_m", label: "Max walk access (m)", type: "number", min: 0, step: 100, group: "Street access", placeholder: "default" },
  { key: "max_endpoint_snap_distance_m", label: "Max marker snap distance (m)", type: "number", default: 50, min: 0, step: 5, group: "Street access", hint: "Maximum distance from a marker to the street network. This is separate from the walking distance to a transit stop. Dashed amber connections are unsurveyed estimates, not routed paths." },
  { key: "max_transfer_distance_m", label: "Max transfer distance (m)", type: "number", min: 0, step: 50, group: "Street access", placeholder: "default" },
  {
    key: "transfer_profile_id",
    label: "Transfer profile",
    type: "select",
    group: "Street access",
    default: "",
    options: (service) => [
      { value: "", label: "none" },
      ...(service?.loaded_transit_feeds.flatMap((f) => f.transfer_profile_ids.map((id) => ({ value: id, label: id }))) ?? []),
    ],
  },
  { key: "max_transfers", label: "Max transfers", type: "number", min: 0, step: 1, group: "Transit", placeholder: "default" },
  { key: "transit_modes", label: "Allowed transit modes", type: "multiselect", default: DEFAULT_TRANSIT_MODES, options: TRANSIT_MODE_OPTIONS, group: "Transit", hint: "Checked modes are allowed. Unchecked modes are excluded. MTR uses Subway; light rail uses Tram." },
  { key: "board_slack_s", label: "Boarding slack (s)", type: "number", min: 0, step: 30, group: "Transit", placeholder: "default" },
  { key: "transfer_slack_s", label: "Transfer slack (s)", type: "number", min: 0, step: 30, group: "Transit", placeholder: "default" },
];

function transitModes(form: Record<string, unknown>) {
  const out: Record<string, unknown> = {};
  const access = list(form, "access");
  const egress = list(form, "egress");
  if (access.length) out.access = access;
  if (egress.length) out.egress = egress;
  if (access.join() !== egress.join()) out.mixed_access_egress = true;
  const streetAccess = str(form, "street_access");
  if (streetAccess) out.street_access = streetAccess;
  for (const key of ["walk_speed_kph", "max_access_distance_m", "max_endpoint_snap_distance_m", "max_transfer_distance_m", "max_transfers", "board_slack_s", "transfer_slack_s"]) {
    const v = num(form, key);
    if (v !== undefined) out[key] = v;
  }
  if (num(form, "max_access_distance_m") !== undefined) out.max_egress_distance_m = num(form, "max_access_distance_m");
  // Send [] explicitly when every mode is unchecked; omitting the field
  // would re-enable the API's default modes.
  out.transit = transitModeSelection(form.transit_modes);
  const transfer = str(form, "transfer_profile_id");
  if (transfer) out.transfer_profile_id = transfer;
  return out;
}

function transitTime(form: Record<string, unknown>) {
  return {
    datetime: str(form, "datetime") ?? "2026-05-12T08:30:00+02:00",
    arrive_by: bool(form, "arrive_by_flag"),
    search_window_s: num(form, "search_window_s") ?? 3600,
  };
}

const transitRouteTool: ToolDef = {
  id: "transit_route",
  label: "Transit route",
  group: "transit",
  path: "/v1/transit-route",
  layers: ["line", "point"],
  tableKey: "legs",
  method: "POST",
  description: "GTFS itinerary with walk, bicycle or car access and egress legs.",
  supportsGeojson: true,
  usesProfile: false,
  input: "routes",
  live: true,
  slots: [],
  fields: [
    { key: "route_id", label: "Route id", type: "text", default: "console_transit" },
    ...transitModeFields(),
    { key: "include_geometry", label: "Include geometry", type: "checkbox", default: true, group: "Returns" },
    {
      key: "walking_geometry",
      label: "Walking geometry",
      type: "select",
      default: "straight_line",
      group: "Returns",
      options: [
        { value: "straight_line", label: "straight line" },
        { value: "network", label: "network (needs a foot profile)" },
      ],
    },
    { key: "include_stops", label: "Include stops", type: "checkbox", default: true, group: "Returns" },
    { key: "include_stop_segments", label: "Include stop segments", type: "checkbox", default: false, group: "Returns" },
    { key: "alt_max_routes", label: "Alternatives (1 = best only)", type: "number", default: 1, min: 1, step: 1, group: "Alternatives" },
    { key: "alt_max_time_ratio", label: "Max time ratio", type: "number", default: 1.5, min: 1, step: 0.1, group: "Alternatives", when: (f) => Number(f.alt_max_routes ?? 1) > 1 },
    {
      key: "pedestrian_profile_id",
      label: "Pedestrian profile (network walking geometry)",
      type: "select",
      default: "",
      group: "Street profiles",
      options: (service) => [{ value: "", label: "auto" }, ...(service?.loaded_profiles.filter((p) => p.mode === "foot").map((p) => ({ value: p.profile_id, label: p.profile_id })) ?? [])],
    },
    {
      key: "access_profile_id",
      label: "Access profile (bicycle/car legs)",
      type: "select",
      default: "",
      group: "Street profiles",
      options: (service) => [{ value: "", label: "auto" }, ...(service?.loaded_profiles.map((p) => ({ value: p.profile_id, label: p.profile_id })) ?? [])],
    },
    {
      key: "egress_profile_id",
      label: "Egress profile (bicycle/car legs)",
      type: "select",
      default: "",
      group: "Street profiles",
      options: (service) => [{ value: "", label: "auto" }, ...(service?.loaded_profiles.map((p) => ({ value: p.profile_id, label: p.profile_id })) ?? [])],
    },
  ],
  buildEach: (ctx, route, index) => {
    const form = ctx.form;
    const body: Record<string, unknown> = { feed_id: ctx.feedId ?? "" };
    for (const key of ["pedestrian_profile_id", "access_profile_id", "egress_profile_id"]) {
      const v = str(form, key);
      if (v) body[key] = v;
    }
    const maxRoutes = num(form, "alt_max_routes") ?? 1;
    body.request = {
      route_id: routeId(ctx, "route_id", "console_transit", index),
      origin: labeled(route.origin!),
      destination: labeled(route.destination!),
      time: transitTime(form),
      modes: transitModes(form),
      returns: {
        include_geometry: bool(form, "include_geometry"),
        walking_geometry: str(form, "walking_geometry") ?? "straight_line",
        include_stops: bool(form, "include_stops"),
        include_stop_segments: bool(form, "include_stop_segments"),
      },
      ...(maxRoutes > 1 ? { alternatives: { max_routes: maxRoutes, max_time_ratio: num(form, "alt_max_time_ratio") ?? 1.5 } } : {}),
    };
    return body;
  },
  ready: (ctx) => (!ctx.feedId ? "No transit feed loaded in the API" : needRoute(ctx)),
};

const transitDirectionsTool: ToolDef = {
  ...transitRouteTool,
  id: "transit_directions",
  label: "Transit directions",
  path: "/v1/transit-directions",
  tableKey: "directions",
  description: "Journey instructions for walking, boarding, riding, transfers and arrival. Route geometry, stops and intermediate stop segments are always included.",
  supportsGeojson: false,
  fields: transitRouteTool.fields.filter(field => !["include_geometry", "include_stops", "include_stop_segments"].includes(field.key)),
  buildEach: (ctx, route, index) => transitRouteTool.buildEach!({ ...ctx, form: { ...ctx.form, include_geometry: true, include_stops: true, include_stop_segments: true } }, route, index),
};

const transitServiceAreaTool: ToolDef = {
  id: "transit_service_area",
  label: "Transit reach",
  group: "transit",
  path: "/v1/transit-service-area",
  layers: ["point", "line", "network", "polygon"],
  tableKey: "stops",
  method: "POST",
  description: "Reachable transit stops, or a street isochrone that continues from stops until the total travel time runs out.",
  supportsGeojson: true,
  usesProfile: false,
  input: "slots",
  slots: [{ id: "origins", label: "Origins" }],
  fields: [
    { key: "analysis_id", label: "Analysis id", type: "text", default: "console_transit_reach" },
    {
      key: "catchment_mode", label: "Catchment mode", type: "select", default: "stops",
      options: [{ value: "stops", label: "Reachable stops" }, { value: "street_isochrone", label: "Street isochrone" }],
      hint: "Street isochrones include direct street travel and use the remaining time after transit. Load a street profile for each chosen mode.",
    },
    { key: "max_travel_time_s", label: "Max travel time (s)", type: "number", default: 1800, min: 60, step: 300 },
    ...transitModeFields().map((field) => field.key === "street_access" ? { ...field, when: (form: Record<string, unknown>) => form.catchment_mode !== "street_isochrone" } : field),
    { key: "include_stops", label: "Include stops", type: "checkbox", default: true, group: "Returns" },
    { key: "include_stop_segments", label: "Include stop segments", type: "checkbox", default: true, group: "Returns" },
    { key: "include_geometry", label: "Include geometry", type: "checkbox", default: true, group: "Returns" },
    { key: "max_stops", label: "Max stops", type: "number", min: 1, step: 500, group: "Returns", placeholder: "default" },
  ],
  build: (ctx) => {
    const form = ctx.form;
    const returns: Record<string, unknown> = {
      include_stops: bool(form, "include_stops"),
      include_stop_segments: bool(form, "include_stop_segments"),
      include_geometry: bool(form, "include_geometry"),
    };
    const maxStops = num(form, "max_stops");
    if (maxStops !== undefined) returns.max_stops = maxStops;
    return {
      feed_id: ctx.feedId ?? "",
      request: {
        analysis_id: str(form, "analysis_id") ?? "console_transit_reach",
        catchment_mode: str(form, "catchment_mode") ?? "stops",
        origins: ctx.points("origins").map(labeled),
        time: transitTime(form),
        modes: { ...transitModes(form), ...(form.catchment_mode === "street_isochrone" ? { street_access: "network" } : {}) },
        max_travel_time_s: num(form, "max_travel_time_s") ?? 1800,
        returns,
      },
    };
  },
  ready: (ctx) => (!ctx.feedId ? "No transit feed loaded in the API" : needPoints(ctx, "origins", 1, "origin")),
};

const simulationTool: ToolDef = {
  id: "simulation",
  label: "Simulation",
  group: "simulation",
  path: "/v1/simulation",
  layers: ["line", "point"],
  method: "POST",
  description: "Agent-based traffic simulation: create a scenario, watch agents move, add zones mid-run, and inspect congestion.",
  supportsGeojson: false,
  usesProfile: true,
  input: "slots",
  slots: [
    { id: "destinations", label: "Destinations (weighted)", weighted: true, hint: "Optional; agents head here. Empty = random." },
    { id: "zone", label: "Zone corners", hint: "Optional; 3+ corners form a slow zone." },
  ],
  fields: [
    { key: "scenario_id", label: "Scenario id", type: "text", default: "console_scenario" },
    { key: "agent_count", label: "Agents", type: "number", default: 500, min: 1, step: 100 },
    { key: "duration_s", label: "Duration (s)", type: "number", default: 1800, min: 60, step: 300 },
    { key: "tick_s", label: "Tick (s)", type: "number", default: 1, min: 0.1, step: 0.5 },
    {
      key: "departures",
      label: "Departures",
      type: "select",
      default: "uniform",
      options: [
        { value: "uniform", label: "uniform over first 10 minutes" },
        { value: "peak", label: "peak around 5 minutes" },
        { value: "instant", label: "all at once" },
      ],
    },
    { key: "origins_mode", label: "Origins", type: "select", default: "viewport", options: [{ value: "viewport", label: "random in current map view" }, { value: "bounds", label: "random anywhere in dataset" }] },
    { key: "zone_factor", label: "Zone speed factor", type: "number", default: 0.5, min: 0.05, step: 0.05 },
    { key: "background_load", label: "Global background load (0-1)", type: "number", default: 0, min: 0, step: 0.05 },
    { key: "reroute_eagerness", label: "Reroute eagerness", type: "number", default: 0.3, min: 0, step: 0.1, group: "Behaviour" },
    { key: "alternative_route_share", label: "Alternative route share", type: "number", default: 0.25, min: 0, step: 0.05, group: "Behaviour" },
    { key: "no_collision", label: "Queueing and spillback", type: "checkbox", default: true, group: "Behaviour" },
    { key: "frame_interval_s", label: "Frame interval (s)", type: "number", default: 5, min: 1, step: 1, group: "Output" },
    { key: "frame_max_agents", label: "Max agents per frame", type: "number", default: 5000, min: 100, step: 500, group: "Output" },
    { key: "edge_stats_interval_s", label: "Edge stats interval (s)", type: "number", default: 60, min: 10, step: 10, group: "Output" },
    { key: "store_trajectories", label: "Store trajectories", type: "checkbox", default: true, group: "Output" },
  ],
  build: (ctx) => {
    const form = ctx.form;
    const bbox = ctx.viewportBbox;
    const origins =
      str(form, "origins_mode") === "viewport" && bbox
        ? { kind: "random_bbox", bbox: bbox.map(round) }
        : { kind: "random_bounds" };
    const destinationPoints = ctx.points("destinations");
    const destinations = destinationPoints.length
      ? { kind: "points", points: destinationPoints.map((p) => ({ lon: round(p.lon), lat: round(p.lat), weight: p.weight ?? 1, label: p.id })) }
      : origins;
    const departuresKind = str(form, "departures") ?? "uniform";
    const departures =
      departuresKind === "peak"
        ? { kind: "peak", mean_s: 300, std_s: 120 }
        : departuresKind === "instant"
          ? { kind: "instant", at_s: 0 }
          : { kind: "uniform", start_s: 0, end_s: 600 };
    const zoneCorners = ctx.points("zone");
    const zones =
      zoneCorners.length >= 3
        ? [
            {
              zone_id: "console_zone",
              label: "Console slow zone",
              polygon: zoneCorners.map((p) => [round(p.lon), round(p.lat)]),
              effect: { kind: "speed_factor", factor: num(form, "zone_factor") ?? 0.5 },
            },
          ]
        : [];
    return {
      scenario: {
        scenario: { id: str(form, "scenario_id") ?? "console_scenario", label: "Console scenario", seed: 1 },
        time: { duration_s: num(form, "duration_s") ?? 1800, tick_s: num(form, "tick_s") ?? 1, stop_when_all_arrived: true },
        fleets: [
          {
            fleet_id: "fleet_1",
            profile_id: ctx.profileId ?? ctx.service?.default_profile_id ?? "",
            agent_count: num(form, "agent_count") ?? 500,
            demand: { origins, destinations },
            departures,
            behavior: {
              reroute_eagerness: num(form, "reroute_eagerness") ?? 0.3,
              alternative_route_share: num(form, "alternative_route_share") ?? 0.25,
              no_collision: bool(form, "no_collision"),
            },
          },
        ],
        zones,
        traffic: { background_load: num(form, "background_load") ?? 0 },
        output: {
          frame_interval_s: num(form, "frame_interval_s") ?? 5,
          frame_max_agents: num(form, "frame_max_agents") ?? 5000,
          edge_stats_interval_s: num(form, "edge_stats_interval_s") ?? 60,
          store_trajectories: bool(form, "store_trajectories"),
        },
      },
    };
  },
  ready: () => null,
};

// --- Trip optimization ---

export const TRIP_MODE_OPTIONS: FieldOption[] = [
  { value: "roundtrip", label: "roundtrip (back to the first stop)" },
  { value: "fixed_start", label: "start at the first stop, end anywhere" },
  { value: "fixed_end", label: "start anywhere, end at the last stop" },
  { value: "fixed_endpoints", label: "first stop to last stop" },
  { value: "open", label: "open (any start and end)" },
];

/** One `TripStop`: the point plus its own or the default service time, and its window. */
export function tripStop(p: InputPoint, defaultServiceS: number) {
  const stop: Record<string, unknown> = { point: labeled(p) };
  const service = p.service_s ?? defaultServiceS;
  if (service > 0) stop.service_s = service;
  if (p.earliest_s !== undefined || p.latest_s !== undefined) {
    const earliest = p.earliest_s ?? 0;
    if (p.latest_s !== undefined && p.latest_s < earliest) throw new Error(`Stop ${p.id}: the window closes before it opens`);
    stop.time_window = p.latest_s !== undefined ? { earliest_s: earliest, latest_s: p.latest_s } : { earliest_s: earliest };
  }
  return stop;
}

function tripRequest(ctx: BuildContext) {
  const form = ctx.form;
  const defaultService = num(form, "service_s") ?? 0;
  const optimizer: Record<string, unknown> = {};
  for (const key of ["seed", "max_iterations", "time_budget_ms", "exact_max_stops"]) {
    const v = num(form, key);
    if (v !== undefined) optimizer[key] = v;
  }
  const exact = optimizer.exact_max_stops as number | undefined;
  if (exact !== undefined && exact > 18) throw new Error("Exact up to (stops) must be at most 18");
  const request: Record<string, unknown> = {
    trip_id: str(form, "trip_id") ?? "console_trip",
    stops: ctx.points("stops").map((p) => tripStop(p, defaultService)),
    mode: str(form, "mode") ?? "roundtrip",
    departure_s: parseClock(form.departure) ?? 0,
    allow_late: bool(form, "allow_late"),
    snap: snap(form),
    returns: returns(form),
  };
  if (Object.keys(optimizer).length) request.optimizer = optimizer;
  return request;
}

const tripTool: ToolDef = {
  id: "trip",
  label: "Trip",
  group: "street",
  path: "/v1/trip",
  layers: ["line", "point"],
  method: "POST",
  description: "Visits every stop in the cheapest directed order: exact up to 16 free stops, local search above. Stops may carry service times and time windows.",
  supportsGeojson: true,
  usesProfile: true,
  input: "slots",
  live: true,
  tableKey: "visits",
  slots: [{ id: "stops", label: "Stops", timed: true, hint: "Each click adds a stop. Letters keep the input order; numbers on the result show the visit order. The first stop is the start and the last the end, where the mode fixes them." }],
  fields: [
    { key: "trip_id", label: "Trip id", type: "text", default: "console_trip" },
    { key: "mode", label: "Mode", type: "select", default: "roundtrip", options: TRIP_MODE_OPTIONS },
    { key: "departure", label: "Departure", type: "clock", default: "08:00", hint: "Clock of the stop windows; the trip may not start earlier." },
    { key: "service_s", label: "Service time per stop (s)", type: "number", default: 0, min: 0, step: 60, hint: "Used for stops without their own service time." },
    { key: "allow_late", label: "Allow late arrivals", type: "checkbox", default: false, hint: "Return the least-late order instead of an error when no order meets every window." },
    { key: "seed", label: "Seed", type: "number", min: 0, step: 1, group: "Optimizer", placeholder: "0" },
    { key: "max_iterations", label: "Max iterations", type: "number", min: 1, step: 1000, group: "Optimizer", placeholder: "size-dependent" },
    { key: "time_budget_ms", label: "Time budget (ms)", type: "number", min: 1, step: 100, group: "Optimizer", placeholder: "none", hint: "A time budget can make orders differ between runs." },
    { key: "exact_max_stops", label: "Exact up to (stops)", type: "number", min: 0, step: 1, group: "Optimizer", placeholder: "16", hint: "Held-Karp below this many free stops, at most 18." },
    ...snapFields,
    returnFields[0],
    ...returnFields.slice(-2),
  ],
  build: (ctx) => envelope(ctx, tripRequest(ctx), false),
  ready: (ctx) => needPoints(ctx, "stops", 2, "stops"),
};

// --- Height probe ---

const heightTool: ToolDef = {
  id: "height",
  label: "Heights",
  group: "street",
  path: "/v1/height",
  layers: ["line", "point"],
  method: "POST",
  description: "Terrain heights from the dataset's DEM at clicked points, or a height profile along the line through them. Needs a dataset imported with --dem.",
  supportsGeojson: false,
  usesProfile: false,
  input: "slots",
  live: true,
  tableKey: "heights",
  slots: [{ id: "probe", label: "Probe points", hint: "Each click samples the DEM. In profile mode the points form a line." }],
  fields: [
    {
      key: "probe_mode",
      label: "Mode",
      type: "select",
      default: "points",
      options: [
        { value: "points", label: "height at each point" },
        { value: "profile", label: "profile along the line" },
      ],
    },
    { key: "resample_distance_m", label: "Sample every (m)", type: "number", min: 1, step: 5, placeholder: "vertices only", hint: "Resamples the line at even intervals (at least 1 m)." },
  ],
  build: (ctx) => {
    const profile = str(ctx.form, "probe_mode") === "profile";
    const body: Record<string, unknown> = { points: ctx.points("probe").map((p) => [round(p.lon), round(p.lat)]) };
    if (profile) body.range = true;
    const resample = num(ctx.form, "resample_distance_m");
    if (resample !== undefined) {
      if (resample < 1) throw new Error("Sample spacing must be at least 1 m");
      body.resample_distance_m = resample;
    }
    return body;
  },
  ready: (ctx) => needPoints(ctx, "probe", str(ctx.form, "probe_mode") === "profile" ? 2 : 1, str(ctx.form, "probe_mode") === "profile" ? "line points" : "probe point"),
};

// --- Live traffic ---

/** Body of `POST /v1/traffic/live`: one update per distinct clicked way. */
export function liveTrafficBody(form: Record<string, unknown>, points: InputPoint[]) {
  const action = str(form, "action") ?? "close";
  const direction = str(form, "traffic_direction") ?? "both";
  const ttl = num(form, "ttl_s");
  const speed = num(form, "speed_kph");
  if (action === "slow" && (speed === undefined || speed <= 0)) throw new Error("Set a speed above 0 km/h for the slowdown");
  if (ttl !== undefined && ttl <= 0) throw new Error("Validity must be positive");
  const seen = new Set<string>();
  const updates: Record<string, unknown>[] = [];
  for (const p of points) {
    if (p.way_id === undefined) continue;
    const key = `${p.way_id}:${direction}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const update: Record<string, unknown> = { feature_id: p.way_id };
    if (direction !== "both") update.direction = direction;
    if (action === "clear") update.clear = true;
    else {
      if (action === "slow") update.speed_kph = speed;
      else update.closed = true;
      if (ttl !== undefined) update.ttl_s = ttl;
    }
    updates.push(update);
  }
  return { updates, replace: bool(form, "replace") };
}

const trafficTool: ToolDef = {
  id: "traffic",
  label: "Live traffic",
  group: "street",
  path: "/v1/traffic/live?wait=true",
  runLabel: "Post live updates",
  layers: ["point"],
  method: "POST",
  description: "Closes or slows the clicked roads with live traffic updates, or clears them. Timed car, HGV and transit-vehicle requests with traffic auto use them until they expire.",
  supportsGeojson: false,
  usesProfile: false,
  input: "slots",
  slots: [{ id: "traffic_ways", label: "Roads", ways: true, hint: "Each click selects the OSM way nearest to it." }],
  fields: [
    {
      key: "action",
      label: "Update",
      type: "select",
      default: "close",
      options: [
        { value: "close", label: "close" },
        { value: "slow", label: "slow down" },
        { value: "clear", label: "clear live state" },
      ],
    },
    { key: "speed_kph", label: "Speed (km/h)", type: "number", default: 15, min: 1, step: 5, when: (f) => (f.action ?? "close") === "slow" },
    {
      key: "traffic_direction",
      label: "Direction",
      type: "select",
      default: "both",
      options: [
        { value: "both", label: "both directions" },
        { value: "forward", label: "forward (along the way)" },
        { value: "backward", label: "backward" },
      ],
    },
    { key: "ttl_s", label: "Valid for (s)", type: "number", default: 1800, min: 1, step: 300, when: (f) => (f.action ?? "close") !== "clear" },
    { key: "replace", label: "Replace all live entries", type: "checkbox", default: false, hint: "Otherwise updates merge with the current live state." },
  ],
  build: (ctx) => liveTrafficBody(ctx.form, ctx.points("traffic_ways")),
  ready: (ctx) => {
    const points = ctx.points("traffic_ways");
    if (!points.length) return "Click roads on the map to select them";
    if (points.some((p) => p.way_id === undefined)) return "Looking up the clicked roads…";
    return null;
  },
};

export const NETWORK_COLOR_BY: FieldOption[] = [
  { value: "road_class", label: "road class" },
  { value: "highway", label: "highway tag" },
  { value: "speed_kph", label: "profile speed (km/h)" },
  { value: "travel_time_s", label: "profile travel time (s)" },
  { value: "cost_per_km", label: "profile cost per km" },
  { value: "allowed", label: "allowed by profile" },
  { value: "max_speed_kph", label: "posted speed limit" },
  { value: "surface", label: "surface" },
  { value: "smoothness", label: "smoothness" },
  { value: "grade_pct", label: "gradient (%)" },
  { value: "elevation_m", label: "elevation (source metres)" },
  { value: "elevation_known", label: "elevation coverage" },
  { value: "structure", label: "structure / underground" },
  { value: "pedestrian_kind", label: "pedestrian connection type" },
  { value: "level", label: "source level" },
  { value: "indoor_location", label: "indoor / outdoor" },
  { value: "access", label: "source access by travel mode" },
  { value: "direction", label: "one-way / two-way" },
  { value: "component", label: "connected component" },
  { value: "temporal", label: "has opening hours" },
];

export const NETWORK_COMPARE_BY: FieldOption[] = [
  { value: "delta_speed_kph", label: "speed difference (B − A, km/h)" },
  { value: "time_ratio", label: "time ratio (B / A)" },
  { value: "delta_cost", label: "cost difference (B − A)" },
  { value: "allowed_diff", label: "allowed by A / B / both" },
];

const networkTool: ToolDef = {
  id: "network",
  label: "Network explorer",
  group: "street",
  path: "/v1/network/edges",
  method: "POST",
  description: "Shows the directed edges in the current view with their source attributes and what the selected profile makes of them (speed, travel time, cost, access). Pick a second profile to see where they differ.",
  supportsGeojson: false,
  usesProfile: true,
  input: "viewport",
  live: true,
  layers: ["line"],
  slots: [],
  fields: [
    { key: "color_by", label: "Colour by", type: "select", default: "speed_kph", options: NETWORK_COLOR_BY },
    {
      key: "compare_profile_id",
      label: "Compare with profile",
      type: "select",
      default: "",
      options: (service) => [{ value: "", label: "none" }, ...(service?.loaded_profiles ?? []).map((p) => ({ value: p.profile_id, label: `${p.profile_id} (${p.mode})` }))],
    },
    { key: "compare_by", label: "Compare colour", type: "select", default: "delta_speed_kph", options: NETWORK_COMPARE_BY, when: (form) => !!form.compare_profile_id },
    { key: "only_allowed", label: "Hide edges the profile excludes", type: "checkbox", default: false },
    { key: "max_edges", label: "Max edges", type: "number", default: 25000, min: 100, step: 5000, group: "Limits" },
    { key: "min_zoom", label: "Minimum zoom", type: "number", default: 12, min: 6, step: 1, group: "Limits", hint: "Below this zoom nothing is requested." },
  ],
  build: (ctx) => ({
    bbox: ctx.viewportBbox,
    profile_id: ctx.profileId,
    compare_profile_id: ctx.form.compare_profile_id ? String(ctx.form.compare_profile_id) : null,
    max_edges: Number(ctx.form.max_edges ?? 25000),
  }),
  ready: (ctx) => {
    if (!ctx.viewportBbox) return "Move the map to an area first.";
    const minZoom = Number(ctx.form.min_zoom ?? 12);
    const zoom = ctx.viewportZoom ?? 0;
    if (zoom < minZoom - 0.5) return `Zoom in to at least zoom ${minZoom} (now ${zoom.toFixed(1)}), or lower the minimum zoom.`;
    return null;
  },
};

const transitEditorTool: ToolDef = {
  id: "transit_editor",
  label: "GTFS editor",
  group: "transit",
  path: "/v1/gtfs-editor/scenarios",
  method: "POST",
  description: "Draw new transit lines on the map (or express variants of existing routes), give them a timetable, and build them into a routable feed: on top of an imported feed or from scratch. The imported feed stays untouched and the scenario can be reverted.",
  supportsGeojson: false,
  usesProfile: false,
  input: "editor",
  layers: ["line", "point"],
  slots: [],
  fields: [],
  ready: () => "Use the editor panel.",
};

export const TOOLS: ToolDef[] = [
  routeTool,
  directionsTool,
  locateTool,
  waypointsTool,
  tripTool,
  heightTool,
  trafficTool,
  odTool,
  matrixTool,
  accessibilityTool,
  serviceAreaTool,
  serviceAreaSequenceTool,
  betweennessTool,
  scenarioBatchTool,
  networkTool,
  transitRouteTool,
  transitDirectionsTool,
  transitServiceAreaTool,
  transitEditorTool,
  simulationTool,
];

export const TOOL_BY_ID: Record<ToolId, ToolDef> = Object.fromEntries(TOOLS.map((t) => [t.id, t])) as Record<ToolId, ToolDef>;

export const GROUP_LABELS: Record<ToolDef["group"], string> = {
  street: "Street routing",
  batch: "Batch analysis",
  transit: "Transit",
  simulation: "Simulation",
};

export function requestPath(tool: ToolDef, format: ResponseFormat): string {
  return tool.supportsGeojson && format === "geojson" ? `${tool.path}?format=geojson` : tool.path;
}
