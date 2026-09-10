// Generic conversion of any API result into styled GeoJSON for the map.
//
// The API's JSON results carry geometry in a few recurring shapes: coordinate
// arrays under `geometry`, GeoJSON geometry objects, `lon`/`lat` pairs, and
// snapped/requested coordinate pairs. Instead of writing one converter per
// endpoint, this module walks the result once, emits a feature for every
// shape it recognises, and tags it with `_kind` (the enclosing key) so the
// style step can colour it.

import { DEFAULT_MAP_STYLE, type MapStyle } from "../state/store";
import { transitTimeContext, type TransitTimeContext } from "../api/time";

export type Position = number[];

export interface Geometry {
  type: "Point" | "LineString" | "Polygon" | "MultiPoint" | "MultiLineString" | "MultiPolygon";
  coordinates: unknown;
}

export interface Feature {
  type: "Feature";
  id?: number;
  geometry: Geometry;
  properties: Record<string, unknown>;
}

export interface FeatureCollection {
  type: "FeatureCollection";
  features: Feature[];
  metadata?: unknown;
}

export const EMPTY_COLLECTION: FeatureCollection = { type: "FeatureCollection", features: [] };

// Categorical palette (validated for CVD separation on a light surface).
export const PALETTE = {
  crimson: "#C8102E",
  blue: "#1F5FBF",
  green: "#0E8A6A",
  ochre: "#B8860B",
  violet: "#7B3FB8",
  pink: "#E87BA4",
  ink: "#16232E",
  grey: "#6B7A87",
};

export const SLOT_COLORS: Record<string, string> = {
  origin: PALETTE.crimson,
  destination: PALETTE.blue,
  origins: PALETTE.crimson,
  destinations: PALETTE.blue,
  points: PALETTE.violet,
  waypoints: PALETTE.ochre,
  od: PALETTE.crimson,
  zones: PALETTE.green,
};

const GEOMETRY_TYPES = new Set(["Point", "LineString", "Polygon", "MultiPoint", "MultiLineString", "MultiPolygon"]);

function isNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isPosition(value: unknown): value is Position {
  return Array.isArray(value) && value.length >= 2 && value.length <= 3 && isNumber(value[0]) && isNumber(value[1]);
}

function isPositionArray(value: unknown): value is Position[] {
  return Array.isArray(value) && value.length > 0 && value.every(isPosition);
}

function isGeometryObject(value: unknown): value is Geometry {
  return (
    typeof value === "object" &&
    value !== null &&
    GEOMETRY_TYPES.has((value as Geometry).type) &&
    "coordinates" in (value as Geometry)
  );
}

function isScalar(value: unknown): boolean {
  return value === null || ["string", "number", "boolean"].includes(typeof value);
}

function scalarProps(obj: Record<string, unknown>): Record<string, unknown> {
  const props: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(obj)) {
    if (isScalar(value)) props[key] = value;
    else if (Array.isArray(value) && value.length > 0 && value.length <= 8 && value.every((v) => typeof v === "string")) {
      props[key] = value.join(", ");
    } else if (key === "components" && typeof value === "object" && value !== null) {
      for (const [k, v] of Object.entries(value as Record<string, unknown>)) if (isScalar(v)) props[`component.${k}`] = v;
    }
  }
  return props;
}

const SKIP_KEYS = new Set(["node_path", "edge_path", "bbox", "topology_bounds"]);

/**
 * Extracts every recognisable geometry from an API response. Results are
 * tagged with `_kind` (nearest enclosing array/object key), `_path`, and
 * `_index` for styling and for linking back to the results table.
 */
export function extractFeatures(root: unknown): FeatureCollection {
  const features: Feature[] = [];
  const seen = new WeakSet<object>();
  let counter = 0;

  function push(geometry: Geometry, props: Record<string, unknown>) {
    features.push({ type: "Feature", id: counter++, geometry, properties: props });
  }

  function walk(value: unknown, key: string, path: string, kind: string, index: number | null, depth: number, inherited?: TransitTimeContext) {
    if (depth > 14 || value === null || typeof value !== "object") return;
    if (seen.has(value as object)) return;
    seen.add(value as object);
    const context = transitTimeContext(value) ?? inherited;

    if (Array.isArray(value)) {
      // Coordinate arrays under known keys are handled by their parent object.
      for (let i = 0; i < value.length; i++) walk(value[i], key, `${path}[${i}]`, key, i, depth + 1, context);
      return;
    }

    const obj = value as Record<string, unknown>;
    if (isGeometryObject(obj)) return; // consumed by the parent
    if (obj.type === "Feature" && isGeometryObject(obj.geometry)) {
      const props = { ...(typeof obj.properties === "object" && obj.properties ? (obj.properties as Record<string, unknown>) : {}) };
      push(obj.geometry, { ...context, ...scalarProps(props), _kind: kind, _path: path, _index: index });
      return;
    }
    if (obj.type === "FeatureCollection" && Array.isArray(obj.features)) {
      walk(obj.features, "features", `${path}.features`, kind === "root" ? "features" : kind, null, depth + 1, context);
      return;
    }

    const base = { ...context, ...scalarProps(obj), _kind: kind, _path: path, _index: index };
    let emitted = false;

    // 1. GeoJSON geometry object or coordinate array under `geometry`.
    const geometry = obj.geometry;
    if (isGeometryObject(geometry)) {
      push(geometry, base);
      emitted = true;
    } else if (isPositionArray(geometry)) {
      const coords = geometry.map((p) => [p[0], p[1]]);
      push(coords.length === 1 ? { type: "Point", coordinates: coords[0] } : { type: "LineString", coordinates: coords }, base);
      emitted = true;
    }

    // 2. Polygon rings (simulation zones, endpoint distributions).
    if (isPositionArray(obj.polygon)) {
      const ring = obj.polygon.map((p) => [p[0], p[1]]);
      const first = ring[0];
      const last = ring[ring.length - 1];
      if (first[0] !== last[0] || first[1] !== last[1]) ring.push(first);
      push({ type: "Polygon", coordinates: [ring] }, { ...base, _kind: "zones" });
      emitted = true;
    }
    if (Array.isArray(obj.bbox) && obj.bbox.length === 4 && obj.bbox.every(isNumber) && key === "bbox") {
      // handled by the parent (endpoint distribution)
    }

    // 3. Point-like objects.
    if (isNumber(obj.lon) && isNumber(obj.lat) && !emitted) {
      push({ type: "Point", coordinates: [obj.lon, obj.lat] }, base);
      emitted = true;
    }
    if (isNumber(obj.snapped_lon) && isNumber(obj.snapped_lat)) {
      push({ type: "Point", coordinates: [obj.snapped_lon, obj.snapped_lat] }, { ...base, _kind: "snapped" });
      if (isNumber(obj.requested_lon) && isNumber(obj.requested_lat)) {
        push(
          {
            type: "LineString",
            coordinates: [
              [obj.requested_lon, obj.requested_lat],
              [obj.snapped_lon, obj.snapped_lat],
            ],
          },
          { ...base, _kind: "snap_link" },
        );
      }
      emitted = true;
    }
    if (isPosition(obj.location) && key !== "root") {
      push({ type: "Point", coordinates: [obj.location[0], obj.location[1]] }, base);
      emitted = true;
    }
    if (isPosition(obj.origin) && isPosition(obj.destination)) {
      push(
        {
          type: "LineString",
          coordinates: [
            [obj.origin[0], obj.origin[1]],
            [obj.destination[0], obj.destination[1]],
          ],
        },
        { ...base, _kind: "od_pairs" },
      );
      emitted = true;
    }

    // Recurse into children (skipping arrays we consumed above).
    for (const [childKey, child] of Object.entries(obj)) {
      if (SKIP_KEYS.has(childKey)) continue;
      if (childKey === "geometry" || childKey === "polygon" || childKey === "location") continue;
      if (child === null || typeof child !== "object") continue;
      walk(child, childKey, `${path}.${childKey}`, Array.isArray(child) ? childKey : kind === "root" ? childKey : kind, null, depth + 1, context);
    }
  }

  walk(root, "root", "$", "root", null, 0);
  return { type: "FeatureCollection", features };
}

// --- Styling ---

export interface StyleContext {
  tool: string;
  /** Colour index of this response (route or profile), for per-run hues. */
  runIndex?: number;
  /** How many responses ran together; above one, each gets a distinct hue. */
  runCount?: number;
  /** User styling of polygons, network and route lines. */
  mapStyle?: MapStyle;
}

/** Sequential green ramp (light to dark). */
export function greenRamp(t: number): string {
  const a = hexToRgb("#d9f0e3");
  const b = hexToRgb("#065a3f");
  const u = Math.max(0, Math.min(1, t));
  return rgbToHex([a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u, a[2] + (b[2] - a[2]) * u]);
}

/** Sequential grey ramp (light to dark). */
export function greyRamp(t: number): string {
  const a = hexToRgb("#e3e7eb");
  const b = hexToRgb("#2b3640");
  const u = Math.max(0, Math.min(1, t));
  return rgbToHex([a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u, a[2] + (b[2] - a[2]) * u]);
}

/** Colour of a reach band at position t (0 = nearest threshold, 1 = farthest) under a palette. */
export function bandColor(style: MapStyle, t: number): string {
  switch (style.polygonPalette) {
    case "heat":
      return heatRamp(0.15 + 0.85 * t);
    case "green":
      return greenRamp(0.3 + 0.7 * (1 - t));
    case "grey":
      return greyRamp(0.25 + 0.7 * (1 - t));
    case "single":
      return style.polygonColor;
    default:
      return ramp(0.25 + 0.7 * (1 - t));
  }
}

const TRANSIT_MODE_COLORS: Record<string, string> = {
  bus: PALETTE.green,
  rail: PALETTE.crimson,
  tram: PALETTE.ochre,
  subway: PALETTE.blue,
  ferry: PALETTE.violet,
  coach: PALETTE.green,
  cable_car: PALETTE.pink,
  gondola: PALETTE.pink,
  funicular: PALETTE.pink,
};

function hexToRgb(hex: string): [number, number, number] {
  const n = parseInt(hex.slice(1), 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

function rgbToHex(rgb: [number, number, number]): string {
  return `#${rgb.map((c) => Math.max(0, Math.min(255, Math.round(c))).toString(16).padStart(2, "0")).join("")}`;
}

/** Single-hue sequential ramp (light to dark blue). */
export function ramp(t: number): string {
  const a = hexToRgb("#cfe0f5");
  const b = hexToRgb("#0b2f6f");
  const u = Math.max(0, Math.min(1, t));
  return rgbToHex([a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u, a[2] + (b[2] - a[2]) * u]);
}

/** Warm sequential ramp for load/score style magnitudes. */
export function heatRamp(t: number): string {
  const stops: [number, string][] = [
    [0, "#ffe8a3"],
    [0.5, "#e8632b"],
    [1, "#6b0f1a"],
  ];
  const u = Math.max(0, Math.min(1, t));
  for (let i = 1; i < stops.length; i++) {
    if (u <= stops[i][0]) {
      const [t0, c0] = stops[i - 1];
      const [t1, c1] = stops[i];
      const k = (u - t0) / (t1 - t0);
      const a = hexToRgb(c0);
      const b = hexToRgb(c1);
      return rgbToHex([a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k, a[2] + (b[2] - a[2]) * k]);
    }
  }
  return stops[stops.length - 1][1];
}

export const ORDINAL = [PALETTE.crimson, PALETTE.blue, PALETTE.green, PALETTE.ochre, PALETTE.violet, PALETTE.pink];

/** Colour of the n-th placed route (markers and result lines agree). */
export function routeColor(index: number): string {
  return ORDINAL[((index % ORDINAL.length) + ORDINAL.length) % ORDINAL.length];
}

/**
 * Adds `_color`, `_width`, `_opacity`, `_radius`, `_dash`, `_fill` and `_sort`
 * to every feature so the map layers can be fully data-driven.
 */
export function styleFeatures(collection: FeatureCollection, ctx: StyleContext): FeatureCollection {
  const features = collection.features;
  // Pre-compute ranges for sequential encodings.
  const range = (key: string, filter?: (f: Feature) => boolean) => {
    let min = Infinity;
    let max = -Infinity;
    for (const f of features) {
      if (filter && !filter(f)) continue;
      const v = f.properties[key];
      if (isNumber(v)) {
        if (v < min) min = v;
        if (v > max) max = v;
      }
    }
    return { min, max, span: max - min || 1 };
  };
  const timeRange = range("total_travel_time_s");
  const scoreRange = range("normalized_score");
  const arrivalRange = range("travel_time_s", (f) => f.geometry.type === "Point");
  const thresholdLimits = Array.from(
    new Set(features.map((f) => f.properties.threshold_limit).filter(isNumber) as number[]),
  ).sort((a, b) => a - b);
  const occupancyRange = range("congestion_level");
  const loadRange = range("congestion_share");
  const multi = (ctx.runCount ?? 1) > 1;
  const runHue = routeColor(ctx.runIndex ?? 0);
  const style = ctx.mapStyle ?? DEFAULT_MAP_STYLE;

  for (const f of features) {
    const p = f.properties;
    const kind = String(p._kind ?? "");
    const geomType = f.geometry.type;
    let color = PALETTE.violet;
    let width = 3;
    let opacity = 0.9;
    let radius = 5;
    let dash = 0;
    let fill = 0.18;
    let sort = 0;

    if (kind === "alternatives") {
      color = multi ? runHue : PALETTE.blue;
      width = 4;
      opacity = multi ? 0.45 : 0.75;
      sort = -1;
    } else if (kind === "snap_link") {
      color = PALETTE.grey;
      width = 1.5;
      dash = 1;
    } else if (kind === "snapped" || kind === "candidates") {
      color = PALETTE.green;
      radius = 6;
    } else if (kind === "maneuvers") {
      color = PALETTE.violet;
      radius = 4;
    } else if (kind === "pairs" || kind === "od_pairs") {
      const ok = p.status === undefined || p.status === "succeeded";
      color = ok ? PALETTE.crimson : PALETTE.grey;
      width = 2.5;
      dash = ok ? 0 : 1;
      if (isNumber(p.total_travel_time_s) && ctx.tool === "od" && features.length > 12) {
        color = ramp((p.total_travel_time_s - timeRange.min) / timeRange.span);
      }
    } else if (kind === "cells") {
      const ok = p.status === undefined || p.status === "succeeded";
      color = ok && isNumber(p.total_travel_time_s) ? ramp((p.total_travel_time_s - timeRange.min) / timeRange.span) : PALETTE.grey;
      width = 2;
      opacity = 0.8;
    } else if (kind === "edges" && isNumber(p.normalized_score)) {
      const t = (p.normalized_score - scoreRange.min) / scoreRange.span;
      color = heatRamp(t);
      width = 1 + 5 * t;
      sort = Math.round(t * 100);
    } else if (kind === "edges" && (isNumber(p.congestion_level) || isNumber(p.congestion_share))) {
      const value = isNumber(p.congestion_level)
        ? (p.congestion_level - occupancyRange.min) / occupancyRange.span
        : ((p.congestion_share as number) - loadRange.min) / loadRange.span;
      color = heatRamp(value);
      width = 1.5 + 4 * value;
    } else if (kind === "legs" && (ctx.tool === "transit_route" || p.leg_type !== undefined)) {
      const legType = String(p.leg_type ?? "transit");
      if (legType === "transit") {
        color = TRANSIT_MODE_COLORS[String(p.mode ?? "")] ?? PALETTE.crimson;
        width = 5;
      } else {
        color = PALETTE.ink;
        width = 2.5;
        dash = 1;
      }
    } else if (kind === "legs") {
      color = multi ? runHue : ORDINAL[(isNumber(p._index) ? p._index : 0) % ORDINAL.length];
      width = 5;
    } else if (kind === "stop_segments") {
      color = TRANSIT_MODE_COLORS[String(p.mode ?? "")] ?? PALETTE.blue;
      width = 2;
      opacity = 0.7;
    } else if (kind === "stops") {
      if (isNumber(p.travel_time_s) && arrivalRange.span > 0) {
        color = ramp(1 - (p.travel_time_s - arrivalRange.min) / arrivalRange.span);
        radius = 4;
      } else {
        color = TRANSIT_MODE_COLORS[String(p.mode ?? "")] ?? PALETTE.ink;
        radius = 4;
      }
    } else if (kind === "features" || p.threshold_limit !== undefined) {
      const idx = thresholdLimits.indexOf(p.threshold_limit as number);
      const t = thresholdLimits.length > 1 ? idx / (thresholdLimits.length - 1) : 0.5;
      const isPolygon = geomType === "Polygon" || geomType === "MultiPolygon";
      color = bandColor(style, t);
      width = isPolygon ? style.polygonOutline : style.networkWidth;
      // With a single colour, farther bands fade so nearer ones stay readable.
      fill = style.polygonPalette === "single" ? style.polygonOpacity * (1 - 0.5 * t) : style.polygonOpacity;
      opacity = isPolygon ? 0.9 : style.polygonPalette === "single" ? 0.9 - 0.5 * t : 0.9;
      sort = -idx;
    } else if (kind === "zones") {
      color = PALETTE.green;
      fill = 0.12;
      width = 1.5;
      dash = 1;
    } else if (kind === "origins") {
      color = PALETTE.crimson;
      radius = 5;
    } else if (kind === "destinations") {
      color = PALETTE.blue;
      radius = 5;
    } else if (geomType === "LineString" || geomType === "MultiLineString") {
      // Main route lines (route, directions, waypoint legs, scenario routes).
      color = multi ? runHue : PALETTE.crimson;
      width = 5;
      sort = 1;
      if (p.route_rank !== undefined && p.route_rank !== 0 && p.route_rank !== null) {
        color = multi ? runHue : PALETTE.blue;
        width = 4;
        opacity = multi ? 0.45 : 0.75;
        sort = -1;
      }
      if (p.scenario_id && ctx.tool === "scenario_batch") {
        color = PALETTE.ochre;
      }
    } else if (geomType === "Point") {
      color = PALETTE.ink;
      radius = 4;
    }

    // User overrides: bands keep their own controls; everything else scales.
    const isBand = kind === "features" || p.threshold_limit !== undefined;
    const isLine = geomType === "LineString" || geomType === "MultiLineString";
    if (!isBand && isLine) {
      width *= style.lineScale;
      opacity *= style.lineOpacity;
      if (style.lineColor && kind !== "snap_link") color = style.lineColor;
    } else if (geomType === "Point") {
      radius *= style.pointScale;
      if (style.pointColor) color = style.pointColor;
    } else if (!isBand) {
      opacity *= style.lineOpacity;
      if (style.lineColor) color = style.lineColor;
    }

    p._color = color;
    p._width = width;
    p._opacity = opacity;
    p._radius = radius;
    p._dash = dash;
    p._fill = fill;
    p._sort = sort;
  }
  return collection;
}

export function featureBounds(collection: FeatureCollection): [number, number, number, number] | null {
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  const visit = (coords: unknown) => {
    if (isPosition(coords)) {
      if (coords[0] < minX) minX = coords[0];
      if (coords[0] > maxX) maxX = coords[0];
      if (coords[1] < minY) minY = coords[1];
      if (coords[1] > maxY) maxY = coords[1];
    } else if (Array.isArray(coords)) {
      for (const c of coords) visit(c);
    }
  };
  for (const f of collection.features) visit(f.geometry.coordinates);
  if (!Number.isFinite(minX)) return null;
  return [minX, minY, maxX, maxY];
}

/** Public-facing copy of the collection without the styling keys. */
export function cleanFeatures(collection: FeatureCollection): FeatureCollection {
  return {
    type: "FeatureCollection",
    features: collection.features.map((f) => {
      const properties: Record<string, unknown> = {};
      for (const [k, v] of Object.entries(f.properties)) if (!k.startsWith("_")) properties[k] = v;
      return { type: "Feature", geometry: f.geometry, properties };
    }),
  };
}
