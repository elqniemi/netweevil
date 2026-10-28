import type { FeatureCollection } from "./features";

export interface StationGeometryResponse extends FeatureCollection {
  metadata: { available: boolean; matched: number; returned: number; truncated: boolean; surface_source_available: boolean };
}

/** Keep surveyed coordinates separate from bound GTFS boarding elevations. */
export function stationGeometryFeatures(response: StationGeometryResponse): FeatureCollection {
  return { ...response, features: response.features.map((feature) => {
    const p = { ...feature.properties };
    p.name ??= p.station_name_en;
    const boarding = p.kind === "boarding_point";
    const fallback = p.source_kind === "platform_level_fallback";
    const numbers = Array.isArray(p.platform_numbers) ? p.platform_numbers.join(", ") : p.platform_code;
    const label = [p.name ?? p.station_code, numbers ? `Platform ${numbers}` : null].filter(Boolean).join("\n");
    if (Array.isArray(p.numbered_platforms)) {
      p.direction = p.numbered_platforms.map((platform: { ref?: string; directions?: { line_code?: string; direction?: string; destination_station_code?: string }[] }) =>
        (platform.directions ?? []).map(direction => `Platform ${platform.ref ?? "?"}: ${direction.line_code ?? ""} ${direction.direction ?? ""} → ${direction.destination_station_code ?? "?"}`).join("; ")
      ).filter(Boolean).join("; ");
    }
    if (p.binding_attributes && typeof p.binding_attributes === "object") {
      for (const [key, value] of Object.entries(p.binding_attributes)) p[`binding.${key}`] = value;
    }
    for (const [key, value] of Object.entries(p)) if (Array.isArray(value) && value.every(item => item === null || typeof item !== "object")) p[key] = value.join(", ");
    return { ...feature, properties: { ...p,
      _reference: true, _kind: boarding ? "boarding_point" : "platform_surface", _label: label,
      _color: boarding ? "#007D8A" : fallback ? "#B8860B" : "#7B3FB8",
      _fill: 0.35, _opacity: boarding ? 1 : 0.55, _width: 1, _radius: 5,
      geometry_elevation_source: boarding ? "bound_gtfs_boarding_point" : "surveyed_station_surface",
      surface_note: boarding ? "GTFS boarding position bound to the pedestrian network. Its elevation can differ from the surveyed platform surface."
        : fallback ? "Whole platform-floor footprint; an individual platform outline is unavailable. Original source XYZ is preserved."
        : "Surveyed platform unit. Original source XYZ is preserved; no adjustment to match boarding elevations.",
    } };
  }) };
}
