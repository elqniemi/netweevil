export const TRANSIT_MODE_OPTIONS = [
  { value: "subway", label: "MTR / Subway / Metro" },
  { value: "bus", label: "Bus" },
  { value: "tram", label: "Tram / Light rail" },
  { value: "rail", label: "Rail" },
  { value: "ferry", label: "Ferry" },
  { value: "funicular", label: "Funicular / Peak Tram" },
  { value: "cable_car", label: "Cable car" },
  { value: "gondola", label: "Gondola" },
  { value: "coach", label: "Coach" },
  { value: "air", label: "Air" },
  { value: "other", label: "Other" },
];

/** Match the API default. Air and Other require an explicit selection. */
export const DEFAULT_TRANSIT_MODES = TRANSIT_MODE_OPTIONS.map((option) => option.value).filter((mode) => mode !== "air" && mode !== "other");

/** Blank legacy text used API defaults. An explicit empty selection means none. */
export function selectedModes(value: unknown, allowed: readonly string[], defaults: readonly string[] = allowed): string[] {
  if (value == null || typeof value === "string" && !value.trim()) return [...defaults];
  const values = Array.isArray(value) ? value : String(value).split(",");
  const selected = [...new Set(values.map((item) => String(item).trim().toLowerCase()).filter(Boolean))];
  const invalid = selected.filter((mode) => !allowed.includes(mode));
  if (invalid.length) throw new Error(`Unknown transit mode: ${invalid.join(", ")}. Choose modes using the checkboxes.`);
  return selected;
}

export function transitModeSelection(value: unknown): string[] {
  return selectedModes(value, TRANSIT_MODE_OPTIONS.map((option) => option.value), DEFAULT_TRANSIT_MODES);
}
