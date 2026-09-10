import { useMemo } from "react";
import { networkLegend } from "../state/run";
import { useStore } from "../state/store";
import { fmtDistance, fmtDuration } from "./format";

interface Band {
  limit: number;
  color: string;
  metric: string;
}

/** Threshold bands of the current result (reach polygons and network), nearest first. */
export function Legend() {
  const result = useStore((s) => s.result);
  const elevationColors = useStore((s) => s.mapStyle.view3d && s.mapStyle.elevationColor);
  const bands = useMemo(() => {
    const byLimit = new Map<number, Band>();
    for (const f of result?.features.features ?? []) {
      const limit = f.properties.threshold_limit;
      if (typeof limit !== "number" || byLimit.has(limit)) continue;
      byLimit.set(limit, { limit, color: String(f.properties._color ?? "#888"), metric: String(f.properties.threshold_metric ?? "") });
    }
    return Array.from(byLimit.values()).sort((a, b) => a.limit - b.limit);
  }, [result]);
  // The 3D style panel shows its elevation range; a profile-speed legend
  // would describe colours that the elevation override has replaced.
  if (result?.tool === "network" && elevationColors) return null;
  if (result?.tool === "network" && networkLegend && result.features.features.length) {
    const legend = networkLegend;
    const meta = (result.runs[0]?.response as { meta?: { edge_count: number; truncated: boolean } } | null)?.meta;
    return (
      <div className="legend" aria-label="Network legend">
        <div className="legend-title">{legend.title}</div>
        {legend.continuous ? (
          <div className="legend-ramp">
            <div className="legend-ramp-bar" style={{ background: `linear-gradient(to right, ${legend.entries.map((e) => e.color).join(", ")})` }} />
            <div className="legend-ramp-labels">
              <span>{legend.entries[0].label}</span>
              <span>{legend.entries[Math.floor(legend.entries.length / 2)].label}</span>
              <span>{legend.entries[legend.entries.length - 1].label}</span>
            </div>
          </div>
        ) : (
          legend.entries.map((e) => (
            <div key={e.label} className="legend-row">
              <span className="legend-swatch" style={{ background: e.color }} />
              <span>{e.label}</span>
            </div>
          ))
        )}
        {meta && (
          <div className="legend-foot muted">
            {meta.edge_count} edges{meta.truncated ? " (limit reached, zoom in)" : ""}
          </div>
        )}
      </div>
    );
  }
  if (!bands.length) return null;
  const isDistance = bands[0].metric === "distance_m";
  return (
    <div className="legend" aria-label="Reach bands">
      <div className="legend-title">{isDistance ? "Reach by distance" : "Reach by travel time"}</div>
      {bands.map((b) => (
        <div key={b.limit} className="legend-row">
          <span className="legend-swatch" style={{ background: b.color }} />
          <span>{isDistance ? fmtDistance(b.limit) : fmtDuration(b.limit)}</span>
        </div>
      ))}
    </div>
  );
}
