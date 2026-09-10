import { ApiError, apiRequest, type Timing } from "../api/client";
import type { ServiceInfo } from "../api/types";
import { PALETTE, cleanFeatures, extractFeatures, ramp, styleFeatures, type Feature, type FeatureCollection } from "../geo/features";
import { TOOL_BY_ID, requestPath, fieldDefaults, type BuildContext, type ToolDef } from "../tools/registry";
import { styleNetworkFeatures, type NetworkLegend } from "../geo/network";
import type { NetworkEdgesMeta } from "../api/types";
import {
  completeRoutes,
  getForm,
  getPoints,
  getState,
  pushTiming,
  requestFit,
  setState,
  subscribe,
  type ResponseFormat,
  type ResultRecord,
  type RunRecord,
  type ToolId,
} from "./store";

export function buildContext(tool: ToolDef, service: ServiceInfo | null = getState().service): BuildContext {
  const state = getState();
  const form = { ...fieldDefaults(tool), ...getForm(tool.id) };
  const engineMode = String(form.engine_mode ?? "auto");
  return {
    form,
    points: (slot) => getPoints(slot),
    routes: completeRoutes(state.routes),
    profileId: tool.usesProfile ? (state.profileId ?? service?.default_profile_id ?? null) : null,
    feedId: state.feedId ?? service?.loaded_transit_feeds[0]?.feed_id ?? null,
    engineMode,
    service,
    viewportBbox: state.viewport.bbox,
    viewportZoom: state.viewport.zoom,
  };
}

export interface PlannedRequest {
  label: string;
  routeIndex: number | null;
  /** Profile the request runs with, when route tools compare profiles. */
  profile: string | null;
  /** Hue index for the map (route index, or profile index when comparing). */
  colorIndex: number;
  body: unknown;
}

export interface Plan {
  requests: PlannedRequest[];
  error: string | null;
  /** True when the JSON editor override is in effect. */
  raw: boolean;
}

/** The request bodies the console would send: raw override or generated from the form. */
export function currentPlan(toolId: ToolId, service: ServiceInfo | null = getState().service): Plan {
  const tool = TOOL_BY_ID[toolId];
  const raw = getState().raw[toolId];
  if (raw !== null && raw !== undefined) {
    try {
      return { requests: [{ label: tool.label, routeIndex: null, profile: null, colorIndex: 0, body: JSON.parse(raw) }], error: null, raw: true };
    } catch (error) {
      return { requests: [], error: `Request JSON is invalid: ${(error as Error).message}`, raw: true };
    }
  }
  const ctx = buildContext(tool, service);
  const ready = tool.ready(ctx);
  if (ready) return { requests: [], error: ready, raw: false };
  try {
    if (tool.buildEach) {
      const each = tool.buildEach;
      const profiles = tool.usesProfile ? [ctx.profileId, ...getState().compareProfiles.filter((id) => id !== ctx.profileId)] : [null];
      const requests: PlannedRequest[] = [];
      profiles.forEach((profile, p) => {
        const profileCtx = profiles.length > 1 ? { ...ctx, profileId: profile } : ctx;
        for (const { route, index } of ctx.routes) {
          const routeLabel = ctx.routes.length > 1 ? `Route ${index + 1}` : tool.label;
          requests.push({
            label: profiles.length > 1 ? `${routeLabel} · ${profile}` : routeLabel,
            routeIndex: index,
            profile: profiles.length > 1 ? profile : null,
            colorIndex: profiles.length > 1 ? p : index,
            body: each(profileCtx, route, index),
          });
        }
      });
      return { requests, error: null, raw: false };
    }
    return { requests: [{ label: tool.label, routeIndex: null, profile: null, colorIndex: 0, body: tool.build!(ctx) }], error: null, raw: false };
  } catch (error) {
    return { requests: [], error: (error as Error).message, raw: false };
  }
}

/** The single body shown in the JSON editor (first planned request). */
export function currentBody(toolId: ToolId, service: ServiceInfo | null = getState().service): { body: unknown; error: string | null; raw: boolean; count: number } {
  const plan = currentPlan(toolId, service);
  return { body: plan.requests[0]?.body ?? null, error: plan.error, raw: plan.raw, count: plan.requests.length };
}

export interface RunOutcome {
  timing: Timing;
  data: unknown;
  rawText: string;
  features: FeatureCollection;
  error?: string;
  aborted?: boolean;
}

interface ExecuteOptions {
  tag?: string;
  signal?: AbortSignal;
  runIndex?: number;
  runCount?: number;
}

async function execute(tool: ToolDef, body: unknown, format: ResponseFormat, options: ExecuteOptions = {}): Promise<RunOutcome> {
  const state = getState();
  const path = requestPath(tool, format);
  try {
    const response = await apiRequest(state.apiBase, path, { tool: tool.id, body, tag: options.tag, signal: options.signal });
    const styleCtx = { tool: tool.id, runIndex: options.runIndex ?? 0, runCount: options.runCount ?? 1, mapStyle: getState().mapStyle };
    let features: FeatureCollection;
    if (tool.id === "network") {
      features = extractFeatures(response.data);
      networkLegend = applyNetworkStyle(features, response.data);
    } else {
      features = styleFeatures(extractFeatures(response.data), styleCtx);
    }
    if (features.features.length === 0) features = synthesizeFeatures(tool.id, response.data);
    response.timing.featureCount = features.features.length;
    return { timing: response.timing, data: response.data, rawText: response.rawText, features };
  } catch (error) {
    if (options.signal?.aborted) {
      return { timing: emptyTiming(tool.id, path), data: null, rawText: "", features: { type: "FeatureCollection", features: [] }, aborted: true };
    }
    if (error instanceof ApiError) {
      return { timing: error.timing, data: error.body, rawText: "", features: { type: "FeatureCollection", features: [] }, error: error.message };
    }
    throw error;
  }
}

/** Runs `tasks` with at most `limit` in flight, preserving result order. */
async function pooled<T>(tasks: (() => Promise<T>)[], limit: number): Promise<T[]> {
  const results: T[] = new Array(tasks.length);
  let next = 0;
  const worker = async () => {
    while (next < tasks.length) {
      const index = next++;
      results[index] = await tasks[index]();
    }
  };
  await Promise.all(Array.from({ length: Math.max(1, Math.min(limit, tasks.length)) }, worker));
  return results;
}

const MAX_IN_FLIGHT = 8;

let activeController: AbortController | null = null;
let runSequence = 0;

export interface RunOptions {
  /** Fit the map to the result when it has geometry (Run button behaviour). */
  fit?: boolean;
  /** Triggered by live updating: never fit, never switch tabs, cancel older runs. */
  live?: boolean;
}

/** Runs the active tool (all placed routes) and stores the result for the map and panels. */
export async function runTool(toolId: ToolId, options: RunOptions = {}): Promise<void> {
  const tool = TOOL_BY_ID[toolId];
  const plan = currentPlan(toolId);
  const format = getState().format;
  const path = requestPath(tool, format);
  liveSignature = planSignature(toolId, format, plan);
  if (plan.error || !plan.requests.length) {
    if (!options.live) {
      setState({ result: { tool: toolId, path, format, runs: [], features: { type: "FeatureCollection", features: [] }, wallMs: 0, error: plan.error ?? "Nothing to send", live: false }, selectedRun: null });
    }
    return;
  }
  activeController?.abort();
  const controller = new AbortController();
  activeController = controller;
  const sequence = ++runSequence;
  setState({ running: true });
  const started = performance.now();
  try {
    const outcomes = await pooled(
      plan.requests.map((request) => () => execute(tool, request.body, format, { signal: controller.signal, runIndex: request.colorIndex, runCount: plan.requests.length })),
      MAX_IN_FLIGHT,
    );
    if (controller.signal.aborted || outcomes.some((o) => o.aborted)) return;
    const wallMs = performance.now() - started;
    const runs: RunRecord[] = outcomes.map((outcome, index) => {
      pushTiming(outcome.timing);
      return {
        label: plan.requests[index].label,
        routeIndex: plan.requests[index].routeIndex,
        profile: plan.requests[index].profile,
        colorIndex: plan.requests[index].colorIndex,
        requestBody: plan.requests[index].body,
        response: outcome.data,
        rawText: outcome.rawText,
        timing: outcome.timing,
        features: outcome.features,
        error: outcome.error,
      };
    });
    const features = mergeRunFeatures(runs);
    const allFailed = runs.every((r) => r.error);
    const previous = getState();
    let resultsTab = previous.resultsTab;
    if (allFailed && !options.live) resultsTab = "json";
    else if (toolId === "directions" && resultsTab === "summary") resultsTab = "steps";
    else if (toolId !== "directions" && resultsTab === "steps") resultsTab = "summary";
    setState({
      result: { tool: toolId, path, format, runs, features, wallMs, live: !!options.live },
      selectedRun: previous.selectedRun !== null && previous.selectedRun < runs.length ? previous.selectedRun : null,
      resultsTab,
    });
    if (options.fit && features.features.length) requestFit();
  } finally {
    if (sequence === runSequence) {
      activeController = null;
      setState({ running: false });
    }
  }
}

/** Legend of the last network explorer run (colour encoding of the view). */
export let networkLegend: NetworkLegend | null = null;

function applyNetworkStyle(features: FeatureCollection, data: unknown): NetworkLegend {
  const state = getState();
  const form = { ...fieldDefaults(TOOL_BY_ID.network), ...getForm("network") };
  const meta = ((data as { meta?: NetworkEdgesMeta } | null)?.meta ?? null) as NetworkEdgesMeta | null;
  const compare = !!form.compare_profile_id && !!meta?.compare_profile_id;
  const key = compare ? String(form.compare_by ?? "delta_speed_kph") : String(form.color_by ?? "speed_kph");
  return styleNetworkFeatures(features, key, meta, { onlyAllowed: form.only_allowed === true, lineScale: state.mapStyle.lineScale, lineOpacity: state.mapStyle.lineOpacity });
}

/** Cancels the in-flight run, if any. */
export function abortRun() {
  activeController?.abort();
}

/**
 * Re-applies the current map style to the stored result without new
 * requests. Synthesised features (fast-path matrix and accessibility) keep
 * their styling.
 */
export function restyleResult() {
  const result = getState().result;
  if (!result) return;
  const mapStyle = getState().mapStyle;
  const runs = result.runs.map((run) => {
    if (run.error || run.features.features.some((f) => f.properties._synthetic)) return run;
    if (result.tool === "network") {
      const features = extractFeatures(run.response);
      networkLegend = applyNetworkStyle(features, run.response);
      return { ...run, features };
    }
    const features = styleFeatures(extractFeatures(run.response), { tool: result.tool, runIndex: run.colorIndex, runCount: result.runs.length, mapStyle });
    return { ...run, features };
  });
  setState({ result: { ...result, runs, features: mergeRunFeatures(runs) } });
}

function mergeRunFeatures(runs: RunRecord[]): FeatureCollection {
  const features: Feature[] = [];
  runs.forEach((run, runIndex) => {
    for (const f of run.features.features) {
      features.push({ ...f, id: features.length, properties: { ...f.properties, _run: runIndex, _run_label: run.label } });
    }
  });
  return { type: "FeatureCollection", features };
}

// --- Live updating ---

let liveTimer: number | null = null;
let liveSignature = "";
let lastInputs: unknown[] = [];

function planSignature(toolId: ToolId, format: ResponseFormat, plan: Plan): string {
  return `${toolId}|${format}|${plan.error ?? ""}|${JSON.stringify(plan.requests.map((r) => r.body))}`;
}

/**
 * Re-runs live-capable tools whenever their request would change (points
 * dragged, form edited, profile switched). Debounced so dragging a marker
 * streams at most a few requests per second, and older in-flight requests
 * are cancelled.
 */
export function startLiveRunner(delayMs = 120): () => void {
  return subscribe(() => {
    const s = getState();
    const inputs = [s.tool, s.live, s.format, s.routes, s.points, s.form[s.tool], s.raw[s.tool], s.profileId, s.feedId, s.service, s.compareProfiles, TOOL_BY_ID[s.tool].input === "viewport" ? s.viewport : null];
    if (inputs.length === lastInputs.length && inputs.every((v, i) => v === lastInputs[i])) return;
    lastInputs = inputs;
    const tool = TOOL_BY_ID[s.tool];
    if (!s.live || !tool.live || !s.service) return;
    const plan = currentPlan(s.tool);
    if (plan.error || !plan.requests.length) return;
    const signature = planSignature(s.tool, s.format, plan);
    if (signature === liveSignature) return;
    if (liveTimer !== null) clearTimeout(liveTimer);
    liveTimer = window.setTimeout(() => {
      liveTimer = null;
      void runTool(s.tool, { live: true });
    }, delayMs);
  });
}

/**
 * Repeats the last result's requests `runs` times with the given concurrency
 * and logs every timing with a `bench` tag. Multi-route results are cycled
 * round-robin. Results are not stored (only timings).
 */
export async function benchmark(runs: number, concurrency: number, onProgress?: (done: number) => void): Promise<Timing[]> {
  const result = getState().result;
  if (!result || !result.runs.length) return [];
  const tool = TOOL_BY_ID[result.tool];
  const bodies = result.runs.map((r) => r.requestBody);
  const timings: Timing[] = [];
  let done = 0;
  setState({ running: true });
  try {
    await pooled(
      Array.from({ length: runs }, (_, i) => async () => {
        const outcome = await execute(tool, bodies[i % bodies.length], result.format, { tag: "bench" });
        timings.push(outcome.timing);
        pushTiming(outcome.timing);
        done += 1;
        onProgress?.(done);
      }),
      concurrency,
    );
  } finally {
    setState({ running: false });
  }
  return timings;
}

/** Re-runs the last requests asking the server for GeoJSON, for export. Multi-route results are merged into one collection. */
export async function fetchServerGeojson(): Promise<{ text: string; timing: Timing } | null> {
  const result = getState().result;
  if (!result || !result.runs.length) return null;
  const tool = TOOL_BY_ID[result.tool];
  if (!tool.supportsGeojson) return null;
  const outcomes = await pooled(
    result.runs.map((run) => () => execute(tool, run.requestBody, "geojson", { tag: "export" })),
    MAX_IN_FLIGHT,
  );
  for (const o of outcomes) pushTiming(o.timing);
  const failed = outcomes.find((o) => o.error);
  if (failed) throw new Error(failed.error);
  if (outcomes.length === 1) return { text: outcomes[0].rawText, timing: outcomes[0].timing };
  const merged: FeatureCollection = { type: "FeatureCollection", features: [] };
  outcomes.forEach((o, i) => {
    const collection = o.data as { features?: Feature[] } | null;
    for (const f of collection?.features ?? []) merged.features.push({ ...f, properties: { ...(f.properties ?? {}), route: result.runs[i].label } });
  });
  const totalMs = outcomes.reduce((a, o) => a + o.timing.totalMs, 0);
  return { text: JSON.stringify(merged), timing: { ...outcomes[0].timing, totalMs } };
}

export function mapGeojson(): FeatureCollection | null {
  const result = getState().result;
  return result ? cleanFeatures(result.features) : null;
}

/** Response payload for export: the single response, or one entry per route. */
export function exportResponse(result: ResultRecord): unknown {
  if (result.runs.length === 1) return result.runs[0].response;
  return result.runs.map((r) => ({ route: r.label, request: r.requestBody, response: r.response, error: r.error ?? null }));
}

export function exportRequest(result: ResultRecord): unknown {
  if (result.runs.length === 1) return result.runs[0].requestBody;
  return result.runs.map((r) => r.requestBody);
}

function emptyTiming(tool: string, path: string): Timing {
  return {
    id: 0,
    at: new Date().toISOString(),
    tool,
    method: "POST",
    path,
    status: 0,
    ok: false,
    ttfbMs: 0,
    downloadMs: 0,
    parseMs: 0,
    totalMs: 0,
    requestBytes: 0,
    responseBytes: 0,
    featureCount: null,
    serverHints: {},
  };
}

/**
 * Draws results that carry no geometry (fast-path matrix cells, accessibility
 * rows) as straight lines or graded points between the input points, so the
 * map still shows what was computed.
 */
function synthesizeFeatures(toolId: ToolId, response: unknown): FeatureCollection {
  const empty: FeatureCollection = { type: "FeatureCollection", features: [] };
  const root = response as { result?: Record<string, unknown> } | null;
  const result = root?.result;
  if (!result) return empty;
  const lookup = new Map<string, { lon: number; lat: number }>();
  for (const slot of ["origins", "destinations"]) for (const p of getPoints(slot)) lookup.set(p.id, p);
  const features: Feature[] = [];
  if (toolId === "matrix" && Array.isArray(result.cells)) {
    const cells = result.cells as Record<string, unknown>[];
    const times = cells.map((c) => (typeof c.total_travel_time_s === "number" ? c.total_travel_time_s : NaN)).filter(Number.isFinite);
    const min = Math.min(...times);
    const span = Math.max(...times) - min || 1;
    cells.forEach((cell, index) => {
      const o = lookup.get(String(cell.origin_id));
      const d = lookup.get(String(cell.destination_id));
      if (!o || !d) return;
      const ok = typeof cell.total_travel_time_s === "number";
      features.push({
        type: "Feature",
        id: index,
        geometry: { type: "LineString", coordinates: [[o.lon, o.lat], [d.lon, d.lat]] },
        properties: { ...scalarOnly(cell), _kind: "cells", _synthetic: true, _color: ok ? ramp(((cell.total_travel_time_s as number) - min) / span) : PALETTE.grey, _width: 2, _opacity: 0.8, _radius: 4, _dash: ok ? 0 : 1, _fill: 0, _sort: 0 },
      });
    });
  } else if (toolId === "accessibility" && Array.isArray(result.rows)) {
    const rows = result.rows as Record<string, unknown>[];
    const counts = rows.map((r) => countOf(r));
    const max = Math.max(1, ...counts);
    rows.forEach((row, index) => {
      const o = lookup.get(String(row.origin_id));
      if (!o) return;
      features.push({
        type: "Feature",
        id: index,
        geometry: { type: "Point", coordinates: [o.lon, o.lat] },
        properties: { ...scalarOnly(row), ...thresholdCounts(row), _kind: "rows", _synthetic: true, _color: ramp(0.3 + 0.7 * (counts[index] / max)), _width: 1, _opacity: 0.9, _radius: 6 + 10 * (counts[index] / max), _dash: 0, _fill: 0, _sort: 0 },
      });
    });
  }
  return { type: "FeatureCollection", features };
}

function countOf(row: Record<string, unknown>): number {
  const counts = row.counts_within_threshold_s;
  if (typeof counts === "object" && counts !== null) {
    let max = 0;
    for (const v of Object.values(counts as Record<string, unknown>)) if (typeof v === "number" && v > max) max = v;
    return max;
  }
  return 0;
}

function scalarOnly(obj: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(obj)) if (v === null || typeof v !== "object") out[k] = v;
  return out;
}

function thresholdCounts(row: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  const counts = row.counts_within_threshold_s;
  if (typeof counts === "object" && counts !== null) {
    for (const [k, v] of Object.entries(counts as Record<string, unknown>)) out[`within_${k}_s`] = v;
  }
  return out;
}
