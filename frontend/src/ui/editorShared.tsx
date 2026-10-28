import type { ScenarioLine, TransitMode } from "../api/types";
import { ORDINAL } from "../geo/features";

export const TRANSIT_MODES: { value: TransitMode; label: string }[] = [
  { value: "bus", label: "bus" },
  { value: "tram", label: "tram" },
  { value: "subway", label: "metro" },
  { value: "rail", label: "rail" },
  { value: "coach", label: "coach" },
  { value: "ferry", label: "ferry" },
  { value: "cable_car", label: "cable car" },
  { value: "gondola", label: "gondola" },
  { value: "funicular", label: "funicular" },
  { value: "other", label: "other" },
];

/** Colour of a scenario line on the map: its GTFS colour or an ordinal hue. */
export function lineColor(line: Pick<ScenarioLine, "color">, index: number): string {
  const raw = (line.color ?? "").trim().replace(/^#/, "");
  if (/^[0-9a-fA-F]{6}$/.test(raw)) return `#${raw}`;
  return ORDINAL[index % ORDINAL.length];
}

export const DAY_LABELS = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];
