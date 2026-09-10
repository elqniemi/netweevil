// Styling of network explorer edges: one attribute drives the colour, and a
// legend describes the encoding. Categorical attributes use the ordinal
// palette; numeric ones use a sequential or diverging ramp over the range the
// API reported for the view.

import type { NetworkEdgesMeta } from "../api/types";
import { ORDINAL, PALETTE, heatRamp, ramp, type FeatureCollection } from "./features";

export interface LegendEntry {
  label: string;
  color: string;
}

export interface NetworkLegend {
  title: string;
  entries: LegendEntry[];
  /** Continuous ramp: the entries are sampled stops with min/max labels. */
  continuous: boolean;
}

const ROAD_CLASS_ORDER = ["motorway", "trunk", "primary", "secondary", "tertiary", "residential", "service", "track", "path", "ferry", "unknown"];
const ROAD_CLASS_COLORS: Record<string, string> = {
  motorway: "#8B1A2B",
  trunk: "#C8102E",
  primary: "#E8632B",
  secondary: "#B8860B",
  tertiary: "#7A9A2E",
  residential: "#1F5FBF",
  service: "#6B7A87",
  track: "#8C6D3F",
  path: "#0E8A6A",
  ferry: "#7B3FB8",
  unknown: "#A3ACB4",
};

const NUMERIC_KEYS = new Set(["speed_kph", "travel_time_s", "cost_per_km", "max_speed_kph", "grade_pct", "length_m", "delta_speed_kph", "delta_travel_time_s", "delta_cost", "time_ratio", "elevation_m"]);
const DIVERGING_KEYS = new Set(["delta_speed_kph", "delta_travel_time_s", "delta_cost", "grade_pct"]);

function divergingRamp(t: number): string {
  // Blue (negative) through pale grey (zero) to crimson (positive).
  const u = Math.max(0, Math.min(1, t));
  const mix = (a: string, b: string, k: number) => {
    const pa = parseInt(a.slice(1), 16);
    const pb = parseInt(b.slice(1), 16);
    const c = [16, 8, 0].map((shift) => Math.round(((pa >> shift) & 255) + (((pb >> shift) & 255) - ((pa >> shift) & 255)) * k));
    return `#${c.map((v) => v.toString(16).padStart(2, "0")).join("")}`;
  };
  return u < 0.5 ? mix("#1F5FBF", "#e6e9ec", u / 0.5) : mix("#e6e9ec", "#C8102E", (u - 0.5) / 0.5);
}

function fmt(v: number): string {
  if (Math.abs(v) >= 100) return v.toFixed(0);
  if (Math.abs(v) >= 10) return v.toFixed(1);
  return v.toFixed(2);
}

/**
 * Assigns `_color`, `_width`, `_opacity`, `_offset` to network edges and
 * returns the legend. `key` is the property to encode; `meta.ranges` gives the
 * numeric extent for the view.
 */
export function styleNetworkFeatures(collection: FeatureCollection, key: string, meta: NetworkEdgesMeta | null, options: { onlyAllowed: boolean; lineScale: number; lineOpacity: number }): NetworkLegend {
  const features = options.onlyAllowed ? collection.features.filter((f) => f.properties.allowed === true) : collection.features;
  collection.features = features;
  const numeric = NUMERIC_KEYS.has(key);
  const range = meta?.ranges[key] ?? null;
  let min = range ? range[0] : 0;
  let max = range ? range[1] : 1;
  const diverging = DIVERGING_KEYS.has(key) || key === "time_ratio";
  if (diverging) {
    // Centre the scale on "no change" so colour sign is meaningful.
    const centre = key === "time_ratio" ? 1 : 0;
    const extent = Math.max(Math.abs(min - centre), Math.abs(max - centre), key === "time_ratio" ? 0.05 : 1e-6);
    min = centre - extent;
    max = centre + extent;
  }
  const span = max - min || 1;
  const categories = new Map<string, string>();
  const colorOf = (value: unknown): string => {
    if (numeric) {
      if (typeof value !== "number" || !Number.isFinite(value)) return "#c9d0d6";
      const t = (value - min) / span;
      if (diverging) return divergingRamp(t);
      if (key === "cost_per_km" || key === "travel_time_s") return heatRamp(t);
      return ramp(0.15 + 0.85 * t);
    }
    const label = categoryLabel(key, value);
    if (key === "road_class") return ROAD_CLASS_COLORS[label] ?? ROAD_CLASS_COLORS.unknown;
    if (key === "allowed") return label === "allowed" ? PALETTE.green : "#c9d0d6";
    if (key === "allowed_diff") return { both: PALETTE.grey, only_a: PALETTE.crimson, only_b: PALETTE.blue, neither: "#dfe3e7" }[label] ?? PALETTE.grey;
    if (key === "direction") return label === "one-way" ? PALETTE.ochre : PALETTE.blue;
    if (key === "temporal") return label === "yes" ? PALETTE.violet : "#c9d0d6";
    if (!categories.has(label)) categories.set(label, ORDINAL[categories.size % ORDINAL.length]);
    return categories.get(label)!;
  };
  for (const f of features) {
    const p = f.properties;
    const value = key === "access" ? accessLabel(p) : key === "direction" ? directionLabel(p) : p[key];
    const color = colorOf(value);
    const dimmed = p.allowed === false && key !== "allowed" && key !== "allowed_diff";
    p._color = color;
    p._width = (dimmed ? 1 : 2.2) * options.lineScale;
    p._opacity = (dimmed ? 0.35 : 0.92) * options.lineOpacity;
    p._radius = 0;
    p._dash = dimmed ? 1 : 0;
    p._fill = 0;
    p._sort = dimmed ? -1 : 0;
    // Two-way ways yield two edges; draw each on its own side so both stay visible.
    p._offset = p.oneway === true || typeof p.direction !== "number" ? 0 : p.direction * 1.4 * options.lineScale;
    p._legend = key === "access" || key === "direction" ? value : numeric ? undefined : categoryLabel(key, value);
  }
  if (numeric) {
    const stops = [0, 0.25, 0.5, 0.75, 1].map((t) => ({ label: fmt(min + t * span), color: colorOf(min + t * span) }));
    return { title: legendTitle(key, meta), entries: stops, continuous: true };
  }
  let entries: LegendEntry[];
  if (key === "road_class") entries = ROAD_CLASS_ORDER.filter((c) => features.some((f) => f.properties.road_class === c)).map((c) => ({ label: c, color: ROAD_CLASS_COLORS[c] }));
  else if (key === "allowed") entries = [{ label: "allowed", color: PALETTE.green }, { label: "excluded", color: "#c9d0d6" }];
  else if (key === "allowed_diff") entries = [{ label: `only ${meta?.profile_id ?? "A"}`, color: PALETTE.crimson }, { label: `only ${meta?.compare_profile_id ?? "B"}`, color: PALETTE.blue }, { label: "both", color: PALETTE.grey }, { label: "neither", color: "#dfe3e7" }];
  else if (key === "direction") entries = [{ label: "one-way", color: PALETTE.ochre }, { label: "two-way", color: PALETTE.blue }];
  else if (key === "temporal") entries = [{ label: "yes", color: PALETTE.violet }, { label: "no", color: "#c9d0d6" }];
  else entries = Array.from(categories.entries()).map(([label, color]) => ({ label, color }));
  return { title: legendTitle(key, meta), entries, continuous: false };
}

function categoryLabel(key: string, value: unknown): string {
  if (key === "allowed") return value === true ? "allowed" : "excluded";
  if (key === "temporal") return value === true ? "yes" : "no";
  if (value === null || value === undefined || value === "") return "unknown";
  return String(value);
}

function accessLabel(p: Record<string, unknown>): string {
  const parts = [p.access_car ? "car" : null, p.access_bicycle ? "bike" : null, p.access_foot ? "foot" : null].filter(Boolean);
  return parts.length ? parts.join("+") : "none";
}

function directionLabel(p: Record<string, unknown>): string {
  return p.oneway === true ? "one-way" : "two-way";
}

function legendTitle(key: string, meta: NetworkEdgesMeta | null): string {
  const a = meta?.profile_id ?? "profile";
  const b = meta?.compare_profile_id ?? "B";
  const titles: Record<string, string> = {
    speed_kph: `Speed under ${a} (km/h)`,
    travel_time_s: `Travel time under ${a} (s)`,
    cost_per_km: `Cost per km under ${a}`,
    allowed: `Allowed by ${a}`,
    max_speed_kph: "Posted speed limit (km/h)",
    grade_pct: "Gradient (%)",
    elevation_m: "Elevation (source metres)",
    elevation_known: "Source elevation available",
    structure: "Structure",
    pedestrian_kind: "Pedestrian connection type",
    level: "Source level",
    indoor_location: "Indoor / outdoor",
    delta_speed_kph: `Speed ${b} − ${a} (km/h)`,
    delta_travel_time_s: `Time ${b} − ${a} (s)`,
    delta_cost: `Cost ${b} − ${a}`,
    time_ratio: `Time ratio ${b} / ${a}`,
    allowed_diff: `Allowed by ${a} vs ${b}`,
    road_class: "Road class",
    highway: "Highway tag",
    surface: "Surface",
    smoothness: "Smoothness",
    access: "Source access",
    direction: "Direction",
    component: "Connected component",
    temporal: "Opening hours",
  };
  return titles[key] ?? key;
}
