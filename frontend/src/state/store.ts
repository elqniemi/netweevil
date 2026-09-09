import { useSyncExternalStore } from "react";
import type { Timing } from "../api/client";
import type { LabeledPoint } from "../api/types";
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
  | "simulation";

export type Basemap = "positron" | "dark" | "osm";
export type ResponseFormat = "json" | "geojson";

/** A map-placed input point. `kind` is used by waypoints (break/through). */
export interface InputPoint extends LabeledPoint {
  kind?: string;
  weight?: number;
}

export interface ResultRecord {
  tool: ToolId;
  path: string;
  requestBody: unknown;
  format: ResponseFormat;
  response: unknown;
  rawText: string;
  timing: Timing;
  features: FeatureCollection;
  error?: string;
}

export interface AppState {
  apiBase: string;
  tool: ToolId;
  profileId: string | null;
  feedId: string | null;
  format: ResponseFormat;
  basemap: Basemap;
  /** Points keyed by `${tool}:${slot}`. */
  points: Record<string, InputPoint[]>;
  /** Active point slot per tool (which slot a map click feeds). */
  activeSlot: Partial<Record<ToolId, string>>;
  /** Form values per tool. */
  form: Partial<Record<ToolId, Record<string, unknown>>>;
  /** Raw JSON overrides per tool (null = generated from the form). */
  raw: Partial<Record<ToolId, string | null>>;
  result: ResultRecord | null;
  running: boolean;
  timings: Timing[];
  hudOpen: boolean;
  resultsTab: "summary" | "table" | "json";
  hoverInfo: { x: number; y: number; props: Record<string, unknown> } | null;
  viewport: { bbox: [number, number, number, number] | null };
  /** Bumps whenever the map should fit to the current result. */
  fitRequest: number;
  simulation: {
    selectedId: string | null;
    frameIndex: number;
    playing: boolean;
    showEdges: boolean;
    agentFeatures: FeatureCollection | null;
    edgeFeatures: FeatureCollection | null;
  };
}

const initialState: AppState = {
  apiBase: "",
  tool: "route",
  profileId: null,
  feedId: null,
  format: "json",
  basemap: "positron",
  points: {},
  activeSlot: {},
  form: {},
  raw: {},
  result: null,
  running: false,
  timings: [],
  hudOpen: false,
  resultsTab: "summary",
  hoverInfo: null,
  viewport: { bbox: null },
  fitRequest: 0,
  simulation: {
    selectedId: null,
    frameIndex: 0,
    playing: false,
    showEdges: true,
    agentFeatures: null,
    edgeFeatures: null,
  },
};

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

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useStore<T>(selector: (s: AppState) => T): T {
  return useSyncExternalStore(subscribe, () => selector(state), () => selector(state));
}

// --- Persistence of the lightweight parts (points, forms, settings). ---

const PERSIST_KEY = "netweevil-console-v1";
let persistTimer: number | null = null;

function schedulePersist() {
  if (persistTimer !== null) return;
  persistTimer = window.setTimeout(() => {
    persistTimer = null;
    try {
      const { apiBase, tool, profileId, feedId, format, basemap, points, activeSlot, form, raw, hudOpen } = state;
      localStorage.setItem(
        PERSIST_KEY,
        JSON.stringify({ apiBase, tool, profileId, feedId, format, basemap, points, activeSlot, form, raw, hudOpen }),
      );
    } catch {
      // Storage may be unavailable; the console still works without it.
    }
  }, 250);
}

function loadPersisted(base: AppState): AppState {
  try {
    const stored = localStorage.getItem(PERSIST_KEY);
    if (!stored) return base;
    const parsed = JSON.parse(stored) as Partial<AppState>;
    return { ...base, ...parsed, result: null, running: false, timings: [] };
  } catch {
    return base;
  }
}

// --- Point helpers ---

export function slotKey(tool: ToolId, slot: string) {
  return `${tool}:${slot}`;
}

export function getPoints(tool: ToolId, slot: string): InputPoint[] {
  return state.points[slotKey(tool, slot)] ?? [];
}

export function setPoints(tool: ToolId, slot: string, points: InputPoint[]) {
  setState((prev) => ({ points: { ...prev.points, [slotKey(tool, slot)]: points } }));
}

export function addPoint(tool: ToolId, slot: string, point: InputPoint, max?: number) {
  const current = getPoints(tool, slot);
  let next: InputPoint[];
  if (max === 1) next = [point];
  else if (max !== undefined && current.length >= max) next = [...current.slice(1), point];
  else next = [...current, point];
  setPoints(tool, slot, next);
}

export function updatePoint(tool: ToolId, slot: string, index: number, patch: Partial<InputPoint>) {
  const current = getPoints(tool, slot);
  if (!current[index]) return;
  const next = current.slice();
  next[index] = { ...current[index], ...patch };
  setPoints(tool, slot, next);
}

export function removePoint(tool: ToolId, slot: string, index: number) {
  const current = getPoints(tool, slot);
  setPoints(
    tool,
    slot,
    current.filter((_, i) => i !== index),
  );
}

export function clearToolPoints(tool: ToolId) {
  setState((prev) => {
    const points: Record<string, InputPoint[]> = {};
    for (const [key, value] of Object.entries(prev.points)) {
      if (!key.startsWith(`${tool}:`)) points[key] = value;
    }
    return { points };
  });
}

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

export function requestFit() {
  setState((prev) => ({ fitRequest: prev.fitRequest + 1 }));
}

let pointCounter = 0;
export function nextPointId(prefix: string): string {
  pointCounter += 1;
  return `${prefix}_${pointCounter}`;
}
