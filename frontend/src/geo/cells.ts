// Pure helpers of the Cells tool: colour scales for H3 cell layers and the
// surge overlay, paint requests from map strokes, and CSV header parsing for
// imports. The API does all cell math; the console only knows the hexagons
// the API has returned.

import type { CellAggregation, CellColumnSpec, CellColumnSummary, CellLayerWriteRequest, CellPaintOp, CellPaintRequest } from "../api/types";
import { heatRamp, ramp, type Feature, type FeatureCollection } from "./features";

export type LonLat = [number, number];

export const PAINT_RESOLUTION = { min: 5, max: 11, default: 8 } as const;
export const SURGE_MULTIPLIER = { min: 1, max: 3 } as const;

/** Prefix of the short-lived layers that resolve brush strokes to cell ids. */
export const SCRATCH_PREFIX = "console-brush-";

/** Average H3 hexagon edge length in metres, by resolution 0..15. */
const H3_EDGE_M = [
  1281256, 483057, 182513, 68979, 26072, 9854, 3725, 1406, 531.4, 200.8, 75.86, 28.66, 10.83, 4.092, 1.546, 0.5842,
];

/** Most points one brush stroke sends to the API. */
const MAX_STROKE_SAMPLES = 5000;

export function h3EdgeMetres(resolution: number): number {
  const r = Math.max(0, Math.min(15, Math.round(resolution)));
  return H3_EDGE_M[r];
}

export function clampResolution(value: unknown, fallback: number = PAINT_RESOLUTION.default): number {
  const n = Number(value);
  if (!Number.isFinite(n)) return fallback;
  return Math.max(PAINT_RESOLUTION.min, Math.min(PAINT_RESOLUTION.max, Math.round(n)));
}

export function isValidLayerId(id: string): boolean {
  return /^[A-Za-z0-9_-]+$/.test(id);
}

export function isScratchLayer(id: string): boolean {
  return id.startsWith(SCRATCH_PREFIX);
}

// --- Colours ---

/** Sequential colour of a value between the column's min and max; null without a value. */
export function cellValueColor(value: unknown, min: number | null, max: number | null): string | null {
  if (typeof value !== "number" || !Number.isFinite(value)) return null;
  const lo = min ?? value;
  const hi = max ?? value;
  const t = hi > lo ? (value - lo) / (hi - lo) : 1;
  return ramp(0.12 + 0.88 * Math.max(0, Math.min(1, t)));
}

/** Surge colour: neutral at the minimum multiplier, darkest at the maximum. */
export function surgeColor(multiplier: unknown, min: number = SURGE_MULTIPLIER.min, max: number = SURGE_MULTIPLIER.max): string | null {
  if (typeof multiplier !== "number" || !Number.isFinite(multiplier)) return null;
  if (multiplier <= min) return NEUTRAL;
  const t = max > min ? (multiplier - min) / (max - min) : 1;
  return heatRamp(Math.max(0, Math.min(1, t)));
}

const NEUTRAL = "#c3ccd4";
const OUTLINE = "#6b7a87";

/** Adds `_color`, `_line` and `_fill` to cell hexagons, coloured by one column. */
export function styleValueCells(collection: FeatureCollection, column: string, stats: CellColumnSummary | null): FeatureCollection {
  const features = collection.features.map((f): Feature => {
    const color = column ? cellValueColor(f.properties[column], stats?.min ?? null, stats?.max ?? null) : null;
    return {
      ...f,
      properties: { ...f.properties, _kind: "cell", _color: color ?? OUTLINE, _line: color ? "#0b2f6f" : OUTLINE, _fill: color ? 0.62 : 0, _width: color ? 0.6 : 1 },
    };
  });
  return { type: "FeatureCollection", features };
}

/** Adds `_color`, `_line` and `_fill` to surge hexagons, coloured by `multiplier`. */
export function styleSurgeCells(collection: FeatureCollection, min: number = SURGE_MULTIPLIER.min, max: number = SURGE_MULTIPLIER.max): FeatureCollection {
  const features = collection.features.map((f): Feature => {
    const color = surgeColor(f.properties.multiplier, min, max);
    const neutral = color === NEUTRAL;
    return {
      ...f,
      properties: { ...f.properties, _kind: "surge", _color: color ?? OUTLINE, _line: color && !neutral ? "#6b0f1a" : OUTLINE, _fill: color ? (neutral ? 0.25 : 0.68) : 0, _width: 0.6 },
    };
  });
  return { type: "FeatureCollection", features };
}

export interface CellLegend {
  title: string;
  colors: string[];
  labels: [string, string, string];
}

function shortNumber(value: number): string {
  if (Math.abs(value) >= 1000) return value.toLocaleString("en-US", { maximumFractionDigits: 0 });
  return Number(value.toPrecision(3)).toString();
}

export function valueLegend(column: string, stats: CellColumnSummary | null): CellLegend | null {
  if (!column || !stats || stats.min === null || stats.max === null) return null;
  const { min, max } = stats;
  const colors = [0, 0.25, 0.5, 0.75, 1].map((t) => cellValueColor(min + (max - min) * t, min, max) ?? OUTLINE);
  return { title: column, colors, labels: [shortNumber(min), shortNumber((min + max) / 2), shortNumber(max)] };
}

export function surgeLegend(min: number = SURGE_MULTIPLIER.min, max: number = SURGE_MULTIPLIER.max): CellLegend {
  const colors = [NEUTRAL, ...[0.25, 0.5, 0.75, 1].map((t) => surgeColor(min + (max - min) * t, min, max) ?? OUTLINE)];
  return { title: "Surge multiplier", colors, labels: [`${shortNumber(min)}×`, `${shortNumber((min + max) / 2)}×`, `${shortNumber(max)}×`] };
}

// --- Strokes ---

/** Ground distance in metres (equirectangular; fine at stroke scale). */
export function distanceMetres(a: LonLat, b: LonLat): number {
  const rad = Math.PI / 180;
  const x = (b[0] - a[0]) * rad * Math.cos(((a[1] + b[1]) / 2) * rad);
  const y = (b[1] - a[1]) * rad;
  return Math.hypot(x, y) * 6_371_008.8;
}

/** Points along a stroke no more than `spacing` metres apart, including the vertices. */
export function sampleStroke(points: LonLat[], spacing: number, limit = MAX_STROKE_SAMPLES): LonLat[] {
  if (!points.length) return [];
  const out: LonLat[] = [points[0]];
  for (let i = 1; i < points.length; i++) {
    const a = points[i - 1];
    const b = points[i];
    const steps = Math.max(1, Math.ceil(distanceMetres(a, b) / Math.max(spacing, 1e-6)));
    for (let k = 1; k <= steps; k++) {
      const t = k / steps;
      const p: LonLat = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
      const last = out[out.length - 1];
      if (p[0] !== last[0] || p[1] !== last[1]) out.push(p);
    }
  }
  if (out.length <= limit) return out;
  const stride = out.length / limit;
  return Array.from({ length: limit }, (_, i) => out[Math.floor(i * stride)]);
}

export function strokeBbox(points: LonLat[]): [number, number, number, number] {
  const first = points[0];
  const last = points[points.length - 1];
  return [Math.min(first[0], last[0]), Math.min(first[1], last[1]), Math.max(first[0], last[0]), Math.max(first[1], last[1])];
}

function pointInRing(p: LonLat, ring: number[][]): boolean {
  let inside = false;
  for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
    const [xi, yi] = ring[i];
    const [xj, yj] = ring[j];
    if (yi > p[1] !== yj > p[1] && p[0] < ((xj - xi) * (p[1] - yi)) / (yj - yi) + xi) inside = !inside;
  }
  return inside;
}

/** Id of a drawn hexagon of `resolution` that contains the point, if one is drawn. */
export function drawnCellAt(point: LonLat, features: Feature[], resolution: number): string | null {
  for (const f of features) {
    if (f.geometry.type !== "Polygon" || Number(f.properties.resolution) !== resolution || typeof f.properties.cell !== "string") continue;
    const ring = (f.geometry.coordinates as number[][][])[0];
    if (ring && pointInRing(point, ring)) return f.properties.cell;
  }
  return null;
}

export interface PaintSettings {
  column: string;
  op: CellPaintOp;
  value: number;
  resolution: number;
  mode: "brush" | "area";
}

/**
 * The requests of one map stroke. A click paints the cell under the pointer
 * with a point bbox; an area drag paints the rectangle in one request; a
 * brush drag paints every cell along the stroke once. Brush points inside
 * drawn hexagons resolve here; the rest (`unresolved`) need the API.
 */
export type StrokePlan =
  | { kind: "request"; body: CellPaintRequest }
  | { kind: "brush"; cells: string[]; unresolved: LonLat[]; resolution: number; base: CellPaintRequest };

export function paintBase(settings: PaintSettings): CellPaintRequest {
  const body: CellPaintRequest = { column: settings.column, op: settings.op };
  if (settings.op !== "clear") body.value = settings.value;
  return body;
}

export function planStroke(points: LonLat[], moved: boolean, settings: PaintSettings, drawn: Feature[] = []): StrokePlan | null {
  if (!points.length) return null;
  const base = paintBase(settings);
  const resolution = settings.resolution;
  if (!moved || points.length === 1) {
    const [lon, lat] = points[points.length - 1];
    return { kind: "request", body: { ...base, bbox: [lon, lat, lon, lat], resolution } };
  }
  if (settings.mode === "area") return { kind: "request", body: { ...base, bbox: strokeBbox(points), resolution } };
  const cells = new Set<string>();
  const unresolved: LonLat[] = [];
  for (const p of sampleStroke(points, h3EdgeMetres(resolution) / 3)) {
    const cell = drawnCellAt(p, drawn, resolution);
    if (cell) cells.add(cell);
    else unresolved.push(p);
  }
  return { kind: "brush", cells: Array.from(cells), unresolved, resolution, base };
}

/** Points table that aggregates brush points into cells of one resolution. */
export function pointsCsv(points: LonLat[]): string {
  return ["lon,lat,n", ...points.map(([lon, lat]) => `${lon.toFixed(7)},${lat.toFixed(7)},1`)].join("\n");
}

/** Map preview of a stroke in progress: the brush path or the area rectangle. */
export function strokePreview(points: LonLat[], mode: "brush" | "area"): FeatureCollection {
  if (!points.length) return { type: "FeatureCollection", features: [] };
  if (mode === "area" && points.length > 1) {
    const [w, s, e, n] = strokeBbox(points);
    return { type: "FeatureCollection", features: [{ type: "Feature", geometry: { type: "Polygon", coordinates: [[[w, s], [e, s], [e, n], [w, n], [w, s]]] }, properties: {} }] };
  }
  if (points.length === 1) return { type: "FeatureCollection", features: [{ type: "Feature", geometry: { type: "Point", coordinates: points[0] }, properties: {} }] };
  return { type: "FeatureCollection", features: [{ type: "Feature", geometry: { type: "LineString", coordinates: points }, properties: {} }] };
}

// --- CSV import ---

/** Splits one CSV line, honouring double quotes. */
export function splitCsvLine(line: string): string[] {
  const out: string[] = [];
  let field = "";
  let quoted = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (quoted) {
      if (c === '"' && line[i + 1] === '"') {
        field += '"';
        i++;
      } else if (c === '"') quoted = false;
      else field += c;
    } else if (c === '"') quoted = true;
    else if (c === ",") {
      out.push(field.trim());
      field = "";
    } else field += c;
  }
  out.push(field.trim());
  return out;
}

function csvLines(text: string): string[] {
  return text.replace(/^﻿/, "").split(/\r?\n/).filter((line) => line.trim() !== "");
}

export function csvHeader(text: string): string[] {
  const first = csvLines(text)[0];
  return first ? splitCsvLine(first) : [];
}

/** Header columns with a numeric value in at least one of the first `sample` rows. */
export function numericCsvColumns(text: string, sample = 50): string[] {
  const lines = csvLines(text);
  if (!lines.length) return [];
  const header = splitCsvLine(lines[0]);
  const numeric = new Set<number>();
  for (const line of lines.slice(1, sample + 1)) {
    splitCsvLine(line).forEach((value, i) => {
      if (value !== "" && Number.isFinite(Number(value))) numeric.add(i);
    });
  }
  return header.filter((_, i) => numeric.has(i));
}

export interface ImportSettings {
  layerId: string;
  points: boolean;
  resolution: number;
  lonColumn: string;
  latColumn: string;
  aggregation: CellAggregation;
}

/**
 * Column specs for an import: none (the API sums every numeric column) for
 * `sum`, else one spec per numeric non-coordinate, non-`cell` column.
 */
export function importColumns(text: string, settings: ImportSettings): CellColumnSpec[] | undefined {
  if (settings.aggregation === "sum") return undefined;
  const skip = new Set(["cell", settings.lonColumn || "lon", settings.latColumn || "lat"].map((c) => c.toLowerCase()));
  const columns = numericCsvColumns(text).filter((c) => !skip.has(c.toLowerCase()));
  if (!columns.length) return undefined;
  return columns.map((source) => ({ source, aggregation: settings.aggregation }));
}

export function importBody(text: string, settings: ImportSettings): CellLayerWriteRequest {
  const body: CellLayerWriteRequest = { layer_id: settings.layerId, merge: true, csv: text };
  if (settings.points) {
    body.resolution = settings.resolution;
    if (settings.lonColumn && settings.lonColumn !== "lon") body.lon_column = settings.lonColumn;
    if (settings.latColumn && settings.latColumn !== "lat") body.lat_column = settings.latColumn;
  }
  const columns = importColumns(text, settings);
  if (columns) body.columns = columns;
  return body;
}
