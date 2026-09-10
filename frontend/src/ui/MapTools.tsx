import { useEffect, useRef, useState } from "react";
import { bandColor } from "../geo/features";
import { restyleResult } from "../state/run";
import { DEFAULT_MAP_STYLE, clearAll, clearMap, setMapStyle, useStore, type MapStyle, type PolygonPalette } from "../state/store";
import { TOOL_BY_ID, type StyleLayer } from "../tools/registry";

const PALETTES: { id: PolygonPalette; label: string }[] = [
  { id: "blue", label: "Blue" },
  { id: "heat", label: "Heat" },
  { id: "green", label: "Green" },
  { id: "grey", label: "Grey" },
  { id: "single", label: "Single colour" },
];

/** Map-corner toolbar: clear buttons and a style popover for the layers the current tool draws. */
export function MapTools() {
  const style = useStore((s) => s.mapStyle);
  const tool = useStore((s) => s.tool);
  const result = useStore((s) => s.result);
  const hasResult = useStore((s) => s.result !== null || s.simulation.agentFeatures !== null || s.simulation.edgeFeatures !== null);
  const hasInputs = useStore((s) => s.routes.some((r) => r.origin || r.destination || r.vias.length) || Object.values(s.points).some((p) => p.length > 0));
  const [open, setOpen] = useState(false);
  const first = useRef(true);

  // Re-style the current result whenever a setting changes.
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    restyleResult();
  }, [style]);

  // Sections follow what is (or will be) drawn: the current result's layers, else the tool's.
  const layers = new Set<StyleLayer>(TOOL_BY_ID[result?.tool ?? tool].layers);
  if (result) {
    for (const f of result.features.features) {
      const t = f.geometry.type;
      if (t === "Polygon" || t === "MultiPolygon") layers.add("polygon");
      else if (t === "Point") layers.add("point");
      else if (f.properties.threshold_limit !== undefined) layers.add("network");
      else layers.add("line");
    }
  }

  const set = (patch: Partial<MapStyle>) => setMapStyle(patch);

  return (
    <div className="map-tools">
      <div className="map-tool-row">
        <button type="button" className={`map-tool-btn${open ? " is-active" : ""}`} onClick={() => setOpen((v) => !v)} title="Layer style">
          Style
        </button>
        <button type="button" className="map-tool-btn" onClick={clearMap} disabled={!hasResult} title="Remove drawn results, keep the points">
          Clear map
        </button>
        <button type="button" className="map-tool-btn" onClick={clearAll} disabled={!hasResult && !hasInputs} title="Remove results, points and routes (Ctrl+Shift+Backspace)">
          Clear all
        </button>
      </div>
      {open && (
        <div className="style-panel">
          {layers.has("line") && (
            <Section title="Lines">
              <Slider label="Width" value={style.lineScale} min={0.2} max={3} step={0.1} format={(v) => `${v.toFixed(1)}×`} onChange={(v) => set({ lineScale: v })} />
              <Slider label="Opacity" value={style.lineOpacity} min={0.1} max={1} step={0.05} format={(v) => `${Math.round(v * 100)}%`} onChange={(v) => set({ lineOpacity: v })} />
              <ColorChoice label="Colour" value={style.lineColor} fallback="#C8102E" onChange={(v) => set({ lineColor: v })} />
            </Section>
          )}
          {layers.has("point") && (
            <Section title="Points">
              <Slider label="Size" value={style.pointScale} min={0.2} max={3} step={0.1} format={(v) => `${v.toFixed(1)}×`} onChange={(v) => set({ pointScale: v })} />
              <ColorChoice label="Colour" value={style.pointColor} fallback="#16232E" onChange={(v) => set({ pointColor: v })} />
            </Section>
          )}
          {layers.has("polygon") && (
            <Section title="Reach polygons">
              <div className="style-row">
                <span>Palette</span>
                <div className="palette-row">
                  {PALETTES.map((p) => (
                    <button key={p.id} type="button" className={`palette-chip${style.polygonPalette === p.id ? " is-active" : ""}`} onClick={() => set({ polygonPalette: p.id })} title={p.label}>
                      {[0, 0.5, 1].map((t) => (
                        <span key={t} style={{ background: bandColor({ ...style, polygonPalette: p.id }, t) }} />
                      ))}
                    </button>
                  ))}
                  <input type="color" value={style.polygonColor} onChange={(e) => set({ polygonColor: e.target.value, polygonPalette: "single" })} aria-label="Single polygon colour" title="Single colour" />
                </div>
              </div>
              <Slider label="Fill opacity" value={style.polygonOpacity} min={0} max={0.9} step={0.02} format={(v) => `${Math.round(v * 100)}%`} onChange={(v) => set({ polygonOpacity: v })} />
              <Slider label="Outline" value={style.polygonOutline} min={0} max={6} step={0.5} format={(v) => `${v} px`} onChange={(v) => set({ polygonOutline: v })} />
            </Section>
          )}
          {layers.has("network") && (
            <Section title="Reach network">
              <Slider label="Width" value={style.networkWidth} min={0.5} max={8} step={0.5} format={(v) => `${v} px`} onChange={(v) => set({ networkWidth: v })} />
            </Section>
          )}
          <Section title="Markers">
            <Slider label="Size" value={style.markerScale} min={0.4} max={2.5} step={0.1} format={(v) => `${v.toFixed(1)}×`} onChange={(v) => set({ markerScale: v })} />
          </Section>
          <div className="style-actions">
            <button type="button" className="btn btn-ghost btn-small" onClick={() => setMapStyle(DEFAULT_MAP_STYLE)}>
              Reset
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="style-section">
      <div className="style-section-title">{title}</div>
      {children}
    </div>
  );
}

function Slider({ label, value, min, max, step, format, onChange }: { label: string; value: number; min: number; max: number; step: number; format: (v: number) => string; onChange: (v: number) => void }) {
  return (
    <label className="style-row style-slider">
      <span>{label}</span>
      <input type="range" min={min} max={max} step={step} value={value} onChange={(e) => onChange(Number(e.target.value))} />
      <output>{format(value)}</output>
    </label>
  );
}

/** Automatic colours, or one fixed colour from a picker. */
function ColorChoice({ label, value, fallback, onChange }: { label: string; value: string | null; fallback: string; onChange: (v: string | null) => void }) {
  return (
    <div className="style-row style-slider">
      <span>{label}</span>
      <div className="color-choice">
        <button type="button" className={`chip${value === null ? " is-active" : ""}`} onClick={() => onChange(null)}>
          auto
        </button>
        <input type="color" value={value ?? fallback} onChange={(e) => onChange(e.target.value)} aria-label={`${label} picker`} />
      </div>
      <span />
    </div>
  );
}
