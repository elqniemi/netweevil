import { useSyncExternalStore } from "react";
import type { Timing } from "../api/client";
import type { FeedStop, GtfsScenario, LabeledPoint, ScenarioInfo, ServiceInfo } from "../api/types";
import type { FeatureCollection } from "../geo/features";

export type ToolId =
  | "route"
  | "directions"
  | "locate"
  | "waypoints"
  | "od"
  | "matrix"
  | "accessibility"
  | "service_area"
  | "service_area_sequence"
  | "betweenness"
  | "scenario_batch"
  | "transit_route"
  | "transit_service_area"
  | "transit_editor"
  | "network"
  | "simulation";

export type Basemap = "positron" | "dark" | "osm";
export type ResponseFormat = "json" | "geojson";

/** A map-placed input point. `kind` is used by waypoint vias (break/through). */
export interface InputPoint extends LabeledPoint {
  kind?: string;
  weight?: number;
}

export type RouteEnd = "origin" | "destination";

/**
 * One start/end pair shared by every route-like tool (route, directions,
 * waypoints, OD, transit, scenario batch). Several routes can be placed at
 * once; every route tool runs them all. `vias` are only used by waypoints.
 */
export interface RouteInput {
  id: string;
  origin: InputPoint | null;
  destination: InputPoint | null;
  vias: InputPoint[];
}

/** One request/response of a run. Multi-route tools produce one per route. */
export interface RunRecord {
  label: string;
  /** Index into `routes` when the run belongs to one route. */
  routeIndex: number | null;
  /** Profile the run used when comparing profiles. */
  profile: string | null;
  /** Hue index shared by the map features and the results table. */
  colorIndex: number;
  requestBody: unknown;
  response: unknown;
  rawText: string;
  timing: Timing;
  features: FeatureCollection;
  error?: string;
}

export type PolygonPalette = "blue" | "heat" | "green" | "grey" | "single";

/** User-adjustable styling of every result layer and the input markers. */
export interface MapStyle {
  /** Multiplier on route, leg and pair line widths. */
  lineScale: number;
  /** Multiplier on line opacity, 0..1. */
  lineOpacity: number;
  /** Fixed colour for lines, or null for automatic per-route/per-kind colours. */
  lineColor: string | null;
  /** Multiplier on result point radii. */
  pointScale: number;
  /** Fixed colour for result points, or null for automatic colours. */
  pointColor: string | null;
  /** Multiplier on A/B and slot marker size. */
  markerScale: number;
  polygonPalette: PolygonPalette;
  /** Colour used by the "single" palette. */
  polygonColor: string;
  /** Fill opacity of reach polygons, 0..1. */
  polygonOpacity: number;
  /** Outline width of reach polygons in px. */
  polygonOutline: number;
  /** Width of reach network lines in px. */
  networkWidth: number;
}

export const DEFAULT_MAP_STYLE: MapStyle = {
  lineScale: 1,
  lineOpacity: 1,
  lineColor: null,
  pointScale: 1,
  pointColor: null,
  markerScale: 1,
  polygonPalette: "blue",
  polygonColor: "#1F5FBF",
  polygonOpacity: 0.22,
  polygonOutline: 1.5,
  networkWidth: 2,
};

export interface ResultRecord {
  tool: ToolId;
  path: string;
  format: ResponseFormat;
  runs: RunRecord[];
  /** All run features merged, each tagged with `_run` (run index). */
  features: FeatureCollection;
  /** Wall time from first request start to last response, ms. */
  wallMs: number;
  /** Set when nothing was sent (request could not be built). */
  error?: string;
  /** Whether the run came from live updating rather than the Run button. */
  live: boolean;
}

/** Working state of the GTFS editor; the document is saved through the API. */
export interface EditorState {
  /** Scenario opened in the editor (persisted so a reload returns to it). */
  scenarioId: string | null;
  /** Working copy of the scenario document, null while loading. */
  scenario: GtfsScenario | null;
  info: ScenarioInfo | null;
  dirty: boolean;
  /** Index of the line being edited; map clicks append stops to it. */
  activeLine: number | null;
  /** Whether map clicks add stops ("add") or only select ("select"). */
  mode: "select" | "add";
  /** Stops of the base feed inside the current view. */
  baseStops: FeedStop[];
  /** Stop the user last clicked (scenario or base), for the side panel. */
  selectedStop: string | null;
}

export interface AppState {
  apiBase: string;
  tool: ToolId;
  profileId: string | null;
  feedId: string | null;
  format: ResponseFormat;
  basemap: Basemap;
  /** Re-run route tools automatically when their inputs change. */
  live: boolean;
  mapStyle: MapStyle;
  /** Extra profiles to run alongside the selected one (route tools). */
  compareProfiles: string[];
  /** Asks the map to fly somewhere (place search). */
  flyRequest: { lon: number; lat: number; zoom: number; n: number } | null;
  /** Points keyed by slot id; slots are shared between tools that use them. */
  points: Record<string, InputPoint[]>;
  /** Active point slot per tool (which slot a map click feeds). */
  activeSlot: Partial<Record<ToolId, string>>;
  routes: RouteInput[];
  activeRoute: number;
  activeEnd: RouteEnd;
  /** Form values per tool. */
  form: Partial<Record<ToolId, Record<string, unknown>>>;
  /** Raw JSON overrides per tool (null = generated from the form). */
  raw: Partial<Record<ToolId, string | null>>;
  service: ServiceInfo | null;
  result: ResultRecord | null;
  /** Run selected in the results panel (highlighted on the map). */
  selectedRun: number | null;
  running: boolean;
  timings: Timing[];
  hudOpen: boolean;
  resultsTab: "summary" | "steps" | "table" | "json";
  /** Whether the left sidebar (settings and results) is shown. */
  sidebarOpen: boolean;
  /** Whether the Setup flow overlay is shown. */
  setupOpen: boolean;
  editor: EditorState;
  /** Whether the settings part of the tool panel is expanded. */
  settingsOpen: boolean;
  hoverInfo: { x: number; y: number; props: Record<string, unknown> } | null;
  viewport: { bbox: [number, number, number, number] | null; zoom: number | null };
  /** Bumps whenever the map should fit to the current result (or one run). */
  fitRequest: number;
  fitRun: number | null;
  simulation: {
    selectedId: string | null;
    frameIndex: number;
    playing: boolean;
    showEdges: boolean;
    agentFeatures: FeatureCollection | null;
    edgeFeatures: FeatureCollection | null;
  };
}

let pointCounter = 0;
export function nextPointId(prefix: string): string {
  pointCounter += 1;
  return `${prefix}_${pointCounter}`;
}

export function emptyRoute(id?: string): RouteInput {
  return { id: id ?? nextPointId("r"), origin: null, destination: null, vias: [] };
}

const initialState: AppState = {
  apiBase: "",
  tool: "route",
  profileId: null,
  feedId: null,
  format: "json",
  basemap: "positron",
  live: true,
  mapStyle: DEFAULT_MAP_STYLE,
  compareProfiles: [],
  flyRequest: null,
  points: {},
  activeSlot: {},
  routes: [{ id: "r1", origin: null, destination: null, vias: [] }],
  activeRoute: 0,
  activeEnd: "origin",
  form: {},
  raw: {},
  service: null,
  result: null,
  selectedRun: null,
  running: false,
  timings: [],
  hudOpen: false,
  resultsTab: "summary",
  sidebarOpen: true,
  setupOpen: false,
  editor: { scenarioId: null, scenario: null, info: null, dirty: false, activeLine: null, mode: "add", baseStops: [], selectedStop: null },
  settingsOpen: true,
  hoverInfo: null,
  viewport: { bbox: null, zoom: null },
  fitRequest: 0,
  fitRun: null,
  simulation: {
    selectedId: null,
    frameIndex: 0,
    playing: false,
    showEdges: true,
    agentFeatures: null,
    edgeFeatures: null,
  },
};

// Declared before `state` because loadPersisted runs at module evaluation.
const PERSIST_KEY = "netweevil-console-v2";
const LEGACY_KEY = "netweevil-console-v1";
let persistTimer: number | null = null;

let state: AppState = loadPersisted(initialState);
const listeners = new Set<() => void>();

function emit() {
  for (const listener of listeners) listener();
}

export function getState(): AppState {
  return state;
}

export function setState(patch: Partial<AppState> | ((prev: AppState) => Partial<AppState>)) {
  const next = typeof patch === "function" ? patch(state) : patch;
  state = { ...state, ...next };
  emit();
  schedulePersist();
}

export function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useStore<T>(selector: (s: AppState) => T): T {
  return useSyncExternalStore(subscribe, () => selector(state), () => selector(state));
}

// --- Persistence of the lightweight parts (points, routes, forms, settings). ---

function schedulePersist() {
  if (persistTimer !== null) return;
  persistTimer = window.setTimeout(() => {
    persistTimer = null;
    try {
      const { apiBase, tool, profileId, feedId, format, basemap, live, mapStyle, compareProfiles, points, activeSlot, routes, activeRoute, activeEnd, form, raw, hudOpen, sidebarOpen, settingsOpen } = state;
      localStorage.setItem(
        PERSIST_KEY,
        JSON.stringify({ apiBase, tool, profileId, feedId, format, basemap, live, mapStyle, compareProfiles, points, activeSlot, routes, activeRoute, activeEnd, form, raw, hudOpen, sidebarOpen, settingsOpen, editorScenarioId: state.editor.scenarioId }),
      );
    } catch {
      // Storage may be unavailable; the console still works without it.
    }
  }, 250);
}

function loadPersisted(base: AppState): AppState {
  try {
    const stored = localStorage.getItem(PERSIST_KEY);
    if (stored) {
      const parsed = JSON.parse(stored) as Partial<AppState> & { editorScenarioId?: string | null };
      const routes = Array.isArray(parsed.routes) && parsed.routes.length ? parsed.routes : base.routes;
      const { editorScenarioId, ...rest } = parsed;
      return {
        ...base,
        ...rest,
        editor: { ...base.editor, scenarioId: typeof editorScenarioId === "string" ? editorScenarioId : null },
        routes,
        activeRoute: Math.min(parsed.activeRoute ?? 0, routes.length - 1),
        mapStyle: { ...DEFAULT_MAP_STYLE, ...(parsed.mapStyle ?? {}) },
        compareProfiles: Array.isArray(parsed.compareProfiles) ? parsed.compareProfiles : [],
        flyRequest: null,
        service: null,
        result: null,
        running: false,
        timings: [],
      };
    }
    const legacy = localStorage.getItem(LEGACY_KEY);
    if (legacy) return migrateLegacy(base, JSON.parse(legacy) as Record<string, unknown>);
    return base;
  } catch {
    return base;
  }
}

/** Converts per-tool point slots from the first console version into shared slots and routes. */
function migrateLegacy(base: AppState, legacy: Record<string, unknown>): AppState {
  const oldPoints = (legacy.points ?? {}) as Record<string, InputPoint[]>;
  const points: Record<string, InputPoint[]> = {};
  const routes: RouteInput[] = [];
  const pairFrom = (tool: string) => {
    const origin = oldPoints[`${tool}:origin`]?.[0];
    const destination = oldPoints[`${tool}:destination`]?.[0];
    if (origin && destination && routes.length === 0) routes.push({ id: "r1", origin, destination, vias: [] });
  };
  for (const tool of ["route", "directions", "transit_route", "scenario_batch"]) pairFrom(tool);
  const od = oldPoints["od:od"] ?? [];
  if (routes.length === 0) for (let i = 0; i + 1 < od.length; i += 2) routes.push({ id: `r${routes.length + 1}`, origin: od[i], destination: od[i + 1], vias: [] });
  for (const [key, list] of Object.entries(oldPoints)) {
    const slot = key.split(":")[1];
    if (!slot || slot === "origin" || slot === "destination" || slot === "od") continue;
    if (list.length && !(points[slot]?.length)) points[slot] = list;
  }
  const { apiBase, tool, profileId, feedId, format, basemap, form, raw, hudOpen } = legacy as Partial<AppState>;
  return {
    ...base,
    ...(apiBase !== undefined ? { apiBase } : {}),
    ...(tool !== undefined ? { tool } : {}),
    ...(profileId !== undefined ? { profileId } : {}),
    ...(feedId !== undefined ? { feedId } : {}),
    ...(format !== undefined ? { format } : {}),
    ...(basemap !== undefined ? { basemap } : {}),
    ...(form !== undefined ? { form } : {}),
    ...(raw !== undefined ? { raw } : {}),
    ...(hudOpen !== undefined ? { hudOpen } : {}),
    points,
    routes: routes.length ? routes : base.routes,
  };
}

// --- Slot point helpers (shared between tools) ---

export function getPoints(slot: string): InputPoint[] {
  return state.points[slot] ?? [];
}

export function setPoints(slot: string, points: InputPoint[]) {
  setState((prev) => ({ points: { ...prev.points, [slot]: points } }));
}

export function addPoint(slot: string, point: InputPoint, max?: number) {
  const current = getPoints(slot);
  let next: InputPoint[];
  if (max === 1) next = [point];
  else if (max !== undefined && current.length >= max) next = [...current.slice(1), point];
  else next = [...current, point];
  setPoints(slot, next);
}

export function updatePoint(slot: string, index: number, patch: Partial<InputPoint>) {
  const current = getPoints(slot);
  if (!current[index]) return;
  const next = current.slice();
  next[index] = { ...current[index], ...patch };
  setPoints(slot, next);
}

export function removePoint(slot: string, index: number) {
  setPoints(
    slot,
    getPoints(slot).filter((_, i) => i !== index),
  );
}

export function clearSlots(slots: string[]) {
  setState((prev) => {
    const points = { ...prev.points };
    for (const slot of slots) delete points[slot];
    return { points };
  });
}

// --- Route helpers ---

export function getRoutes(): RouteInput[] {
  return state.routes;
}

/** Routes with both ends placed, keeping their index in `routes`. */
export function completeRoutes(routes: RouteInput[] = state.routes): { route: RouteInput; index: number }[] {
  const out: { route: RouteInput; index: number }[] = [];
  routes.forEach((route, index) => {
    if (route.origin && route.destination) out.push({ route, index });
  });
  return out;
}

export function setRoutes(routes: RouteInput[], active?: number) {
  const list = routes.length ? routes : [emptyRoute()];
  setState((prev) => ({ routes: list, activeRoute: Math.min(active ?? prev.activeRoute, list.length - 1) }));
}

export function addRoute(): number {
  const routes = [...state.routes, emptyRoute()];
  setState({ routes, activeRoute: routes.length - 1, activeEnd: "origin" });
  return routes.length - 1;
}

export function removeRoute(index: number) {
  const routes = state.routes.filter((_, i) => i !== index);
  setRoutes(routes, state.activeRoute > index ? state.activeRoute - 1 : state.activeRoute);
}

export function setActiveRoute(index: number, end?: RouteEnd) {
  setState((prev) => ({ activeRoute: Math.max(0, Math.min(index, prev.routes.length - 1)), ...(end ? { activeEnd: end } : {}) }));
}

export function setRouteEnd(index: number, end: RouteEnd, point: InputPoint | null) {
  setState((prev) => {
    const routes = prev.routes.slice();
    if (!routes[index]) return {};
    routes[index] = { ...routes[index], [end]: point };
    return { routes };
  });
}

export function moveRouteEnd(index: number, end: RouteEnd, lon: number, lat: number) {
  const route = state.routes[index];
  const current = route?.[end];
  if (!current) return;
  setRouteEnd(index, end, { ...current, lon, lat });
}

export function swapRoute(index: number) {
  const route = state.routes[index];
  if (!route) return;
  const routes = state.routes.slice();
  routes[index] = { ...route, origin: route.destination, destination: route.origin, vias: route.vias.slice().reverse() };
  setState({ routes });
}

export function addVia(index: number, point: InputPoint) {
  setState((prev) => {
    const routes = prev.routes.slice();
    if (!routes[index]) return {};
    routes[index] = { ...routes[index], vias: [...routes[index].vias, point] };
    return { routes };
  });
}

export function updateVia(index: number, via: number, patch: Partial<InputPoint>) {
  setState((prev) => {
    const routes = prev.routes.slice();
    const route = routes[index];
    if (!route || !route.vias[via]) return {};
    const vias = route.vias.slice();
    vias[via] = { ...vias[via], ...patch };
    routes[index] = { ...route, vias };
    return { routes };
  });
}

export function removeVia(index: number, via: number) {
  setState((prev) => {
    const routes = prev.routes.slice();
    const route = routes[index];
    if (!route) return {};
    routes[index] = { ...route, vias: route.vias.filter((_, i) => i !== via) };
    return { routes };
  });
}

/**
 * Places a clicked point into the active route the way a maps app does:
 * fill the missing end (origin first), otherwise replace the active end.
 * Returns the end that was set.
 */
export function placeRoutePoint(lon: number, lat: number, viaMode = false): RouteEnd | "via" {
  const index = Math.min(state.activeRoute, state.routes.length - 1);
  const route = state.routes[index];
  const point = (prefix: string) => ({ id: nextPointId(prefix), lon, lat });
  if (!route.origin) {
    setRouteEnd(index, "origin", point("origin"));
    setState({ activeEnd: "destination" });
    return "origin";
  }
  if (!route.destination) {
    setRouteEnd(index, "destination", point("destination"));
    setState({ activeEnd: "destination" });
    return "destination";
  }
  if (viaMode) {
    addVia(index, { ...point("via"), kind: "break" });
    return "via";
  }
  setRouteEnd(index, state.activeEnd, { ...route[state.activeEnd]!, lon, lat });
  return state.activeEnd;
}

export function clearRoutes() {
  setState({ routes: [emptyRoute()], activeRoute: 0, activeEnd: "origin" });
}

/** Removes every placed point and route, and the current result. */
export function clearAll() {
  clearMap();
  setState({ routes: [emptyRoute()], activeRoute: 0, activeEnd: "origin", points: {} });
}

/** Removes drawn results and simulation overlays, keeping the placed points. */
export function clearMap() {
  setState((prev) => ({
    result: null,
    selectedRun: null,
    hoverInfo: null,
    simulation: { ...prev.simulation, agentFeatures: null, edgeFeatures: null, playing: false },
  }));
}

export function setMapStyle(patch: Partial<MapStyle>) {
  setState((prev) => ({ mapStyle: { ...prev.mapStyle, ...patch } }));
}

export function flyTo(lon: number, lat: number, zoom = 14) {
  setState((prev) => ({ flyRequest: { lon, lat, zoom, n: (prev.flyRequest?.n ?? 0) + 1 } }));
}

// --- Forms ---

export function getForm(tool: ToolId): Record<string, unknown> {
  return state.form[tool] ?? {};
}

export function setFormValue(tool: ToolId, key: string, value: unknown) {
  setState((prev) => ({ form: { ...prev.form, [tool]: { ...(prev.form[tool] ?? {}), [key]: value } } }));
}

export function resetForm(tool: ToolId) {
  setState((prev) => {
    const form = { ...prev.form };
    delete form[tool];
    const raw = { ...prev.raw };
    delete raw[tool];
    return { form, raw };
  });
}

export function pushTiming(timing: Timing) {
  setState((prev) => ({ timings: [...prev.timings.slice(-499), timing] }));
}

/** Asks the map to fit the whole result, or one run of it. */
export function requestFit(run: number | null = null) {
  setState((prev) => ({ fitRequest: prev.fitRequest + 1, fitRun: run }));
}


// --- GTFS editor helpers ---

export function setEditor(patch: Partial<EditorState> | ((prev: EditorState) => Partial<EditorState>)) {
  setState((prev) => ({ editor: { ...prev.editor, ...(typeof patch === "function" ? patch(prev.editor) : patch) } }));
}

/** Replaces the working scenario and marks it dirty (unsaved). */
export function updateScenario(mutate: (scenario: GtfsScenario) => GtfsScenario) {
  setState((prev) => {
    if (!prev.editor.scenario) return {};
    return { editor: { ...prev.editor, scenario: mutate(prev.editor.scenario), dirty: true } };
  });
}

/** Adds a stop id to the active line (creating a new scenario stop when `create` is given). */
export function appendEditorStop(stopId: string, create?: { name: string; lon: number; lat: number }) {
  updateScenario((scenario) => {
    const lineIndex = state.editor.activeLine;
    const stops = create ? [...scenario.stops, { stop_id: stopId, ...create }] : scenario.stops;
    if (lineIndex === null || !scenario.lines[lineIndex]) return { ...scenario, stops };
    const lines = scenario.lines.slice();
    const line = lines[lineIndex];
    if (line.stops.length && line.stops[line.stops.length - 1].stop_id === stopId) return { ...scenario, stops };
    lines[lineIndex] = { ...line, stops: [...line.stops, { stop_id: stopId, travel_s: null, dwell_s: null }] };
    return { ...scenario, stops, lines };
  });
}

export function moveEditorStop(stopId: string, lon: number, lat: number) {
  updateScenario((scenario) => ({ ...scenario, stops: scenario.stops.map((s) => (s.stop_id === stopId ? { ...s, lon, lat } : s)) }));
}

/**
 * Removes a stop from the active line (last occurrence); a scenario stop no
 * line uses any more is deleted from the document.
 */
export function removeEditorStop(stopId: string) {
  updateScenario((scenario) => {
    const lineIndex = state.editor.activeLine;
    const lines = scenario.lines.slice();
    if (lineIndex !== null && lines[lineIndex]) {
      const stops = lines[lineIndex].stops.slice();
      const last = stops.map((s) => s.stop_id).lastIndexOf(stopId);
      if (last >= 0) stops.splice(last, 1);
      lines[lineIndex] = { ...lines[lineIndex], stops };
    }
    const used = lines.some((line) => line.stops.some((s) => s.stop_id === stopId));
    const stops = used ? scenario.stops : scenario.stops.filter((s) => s.stop_id !== stopId);
    return { ...scenario, lines, stops };
  });
}

let editorStopCounter = 0;
/** A scenario stop id that no scenario or base stop uses yet. */
export function nextEditorStopId(): string {
  const scenario = state.editor.scenario;
  const taken = new Set<string>([...(scenario?.stops.map((s) => s.stop_id) ?? []), ...state.editor.baseStops.map((s) => s.stop_id)]);
  for (;;) {
    editorStopCounter += 1;
    const id = `new_${editorStopCounter}`;
    if (!taken.has(id)) return id;
  }
}
