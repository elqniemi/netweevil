import { ApiError, apiRequest, type Timing } from "../api/client";
import type { ServiceInfo } from "../api/types";
import { PALETTE, cleanFeatures, extractFeatures, ramp, styleFeatures, type Feature, type FeatureCollection } from "../geo/features";
import { TOOL_BY_ID, requestPath, fieldDefaults, type BuildContext, type ToolDef } from "../tools/registry";
import { getForm, getPoints, getState, pushTiming, requestFit, setState, type ResponseFormat, type ToolId } from "./store";

export function buildContext(tool: ToolDef, service: ServiceInfo | null): BuildContext {
  const state = getState();
  const form = { ...fieldDefaults(tool), ...getForm(tool.id) };
  const engineMode = String(form.engine_mode ?? "auto");
  return {
    form,
    points: (slot) => getPoints(tool.id, slot),
    profileId: tool.usesProfile ? (state.profileId ?? service?.default_profile_id ?? null) : null,
    feedId: state.feedId ?? service?.loaded_transit_feeds[0]?.feed_id ?? null,
    engineMode,
    service,
    viewportBbox: state.viewport.bbox,
  };
}

/** The request body the console would send: raw override or generated from the form. */
export function currentBody(toolId: ToolId, service: ServiceInfo | null): { body: unknown; error: string | null; raw: boolean } {
  const tool = TOOL_BY_ID[toolId];
  const raw = getState().raw[toolId];
  if (raw !== null && raw !== undefined) {
    try {
      return { body: JSON.parse(raw), error: null, raw: true };
    } catch (error) {
      return { body: null, error: `Request JSON is invalid: ${(error as Error).message}`, raw: true };
    }
  }
  const ctx = buildContext(tool, service);
  const ready = tool.ready(ctx);
  if (ready) return { body: null, error: ready, raw: false };
  try {
    return { body: tool.build(ctx), error: null, raw: false };
  } catch (error) {
    return { body: null, error: (error as Error).message, raw: false };
  }
}

export interface RunOutcome {
  timing: Timing;
  data: unknown;
  rawText: string;
  features: FeatureCollection;
  error?: string;
}

async function execute(tool: ToolDef, body: unknown, format: ResponseFormat, tag?: string): Promise<RunOutcome> {
  const state = getState();
  const path = requestPath(tool, format);
  try {
    const response = await apiRequest(state.apiBase, path, { tool: tool.id, body, tag });
    let features = styleFeatures(extractFeatures(response.data), { tool: tool.id });
    if (features.features.length === 0) features = synthesizeFeatures(tool.id, response.data);
    response.timing.featureCount = features.features.length;
    return { timing: response.timing, data: response.data, rawText: response.rawText, features };
  } catch (error) {
    if (error instanceof ApiError) {
      return { timing: error.timing, data: error.body, rawText: "", features: { type: "FeatureCollection", features: [] }, error: error.message };
    }
    throw error;
  }
}

/** Runs the active tool once and stores the result for the map and panels. */
export async function runTool(toolId: ToolId, service: ServiceInfo | null): Promise<void> {
  const tool = TOOL_BY_ID[toolId];
  const { body, error } = currentBody(toolId, service);
  const format = getState().format;
  if (error) {
    setState({ result: { tool: toolId, path: tool.path, requestBody: body, format, response: null, rawText: "", timing: emptyTiming(toolId, tool.path), features: { type: "FeatureCollection", features: [] }, error } });
    return;
  }
  setState({ running: true });
  try {
    const outcome = await execute(tool, body, format);
    pushTiming(outcome.timing);
    setState({
      result: {
        tool: toolId,
        path: requestPath(tool, format),
        requestBody: body,
        format,
        response: outcome.data,
        rawText: outcome.rawText,
        timing: outcome.timing,
        features: outcome.features,
        error: outcome.error,
      },
      resultsTab: outcome.error ? "json" : getState().resultsTab,
    });
    if (!outcome.error && outcome.features.features.length) requestFit();
  } finally {
    setState({ running: false });
  }
}

/**
 * Repeats the last request `runs` times with the given concurrency and logs
 * every timing with a `bench` tag. Results are not stored (only timings).
 */
export async function benchmark(runs: number, concurrency: number, onProgress?: (done: number) => void): Promise<Timing[]> {
  const result = getState().result;
  if (!result || result.requestBody === null) return [];
  const tool = TOOL_BY_ID[result.tool];
  const timings: Timing[] = [];
  let next = 0;
  let done = 0;
  setState({ running: true });
  const worker = async () => {
    while (next < runs) {
      next += 1;
      const outcome = await execute(tool, result.requestBody, result.format, "bench");
      timings.push(outcome.timing);
      pushTiming(outcome.timing);
      done += 1;
      onProgress?.(done);
    }
  };
  try {
    await Promise.all(Array.from({ length: Math.max(1, Math.min(concurrency, runs)) }, worker));
  } finally {
    setState({ running: false });
  }
  return timings;
}

/** Re-runs the last request asking the server for GeoJSON, for export. */
export async function fetchServerGeojson(): Promise<{ text: string; timing: Timing } | null> {
  const result = getState().result;
  if (!result) return null;
  const tool = TOOL_BY_ID[result.tool];
  if (!tool.supportsGeojson) return null;
  const outcome = await execute(tool, result.requestBody, "geojson", "export");
  pushTiming(outcome.timing);
  if (outcome.error) throw new Error(outcome.error);
  return { text: outcome.rawText, timing: outcome.timing };
}

export function mapGeojson(): FeatureCollection | null {
  const result = getState().result;
  return result ? cleanFeatures(result.features) : null;
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
  for (const slot of ["origins", "destinations", "od"]) for (const p of getPoints(toolId, slot)) lookup.set(p.id, p);
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
