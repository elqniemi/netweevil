import { useEffect, useMemo, useRef, useState } from "react";
import { bandColor } from "../geo/features";
import { restyleResult } from "../state/run";
import { DEFAULT_MAP_STYLE, clearAll, clearMap, setMapStyle, setState, useStore, type MapStyle, type PolygonPalette } from "../state/store";
import { TOOL_BY_ID, type StyleLayer } from "../tools/registry";
import { elevatedData } from "../geo/elevation";
import { selectedTerrain } from "../map/terrain";
import { extractFeatures, styleFeatures } from "../geo/features";

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
  const references = useStore((s) => s.referenceLayers);
  const stationGeometry = useStore((s) => s.stationGeometry);
  const stationStatus = useStore((s) => s.stationGeometryStatus);
  const terrainSources = useStore((s) => s.terrainSources);
  const terrainStatus = useStore((s) => s.terrainStatus);
  const terrainError = useStore((s) => s.terrainError);
  const terrain = selectedTerrain(terrainSources, style.terrainSourceId);
  const [importError, setImportError] = useState<string | null>(null);
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
  }, [style.lineScale, style.lineOpacity, style.lineColor, style.pointScale, style.pointColor, style.markerScale, style.polygonPalette, style.polygonColor, style.polygonOpacity, style.polygonOutline, style.networkWidth]);

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
  const elevations = useMemo(() => open && style.view3d
    ? elevatedData({ type: "FeatureCollection", features: [...(result?.features.features ?? []), ...references.flatMap((layer) => layer.features.features), ...(style.stationPlatforms ? stationGeometry?.features ?? [] : [])] }, { verticalExaggeration: 1, altitudeSlice: false, minAltitude: 0, maxAltitude: 0 })
    : null, [open, style.view3d, style.stationPlatforms, result, references, stationGeometry]);

  const importReferences = async (files: FileList | null) => {
    if (!files) return;
    setImportError(null);
    try {
      const layers = await Promise.all(Array.from(files, async (file) => {
        if (file.size > 50 * 1024 * 1024) throw new Error(`${file.name} exceeds the 50 MB overlay limit. Use the network explorer for full networks.`);
        const data: unknown = JSON.parse(await file.text());
        if (!data || typeof data !== "object" || !["FeatureCollection", "Feature"].includes(String((data as { type?: string }).type))) throw new Error(`${file.name} must contain GeoJSON features.`);
        const features = styleFeatures(extractFeatures(data), { tool: "network" });
        if (!features.features.length) throw new Error(`${file.name} has no supported geometries.`);
        for (const f of features.features) {
          f.properties._reference = true;
          f.properties.overlay = file.name;
          f.properties._color = file.name.toLowerCase().includes("exit") ? "#0e8a6a" : "#7b3fb8";
          f.properties._radius = 5;
        }
        return { name: file.name, features };
      }));
      setState((state) => ({ referenceLayers: [...state.referenceLayers.filter((old) => !layers.some((layer) => layer.name === old.name)), ...layers] }));
    } catch (error) { setImportError(error instanceof Error ? error.message : String(error)); }
  };

  return (
    <div className="map-tools">
      <div className="map-tool-row">
        <button type="button" className={`map-tool-btn${style.view3d ? " is-active" : ""}`} aria-pressed={style.view3d} onClick={() => { set({ view3d: !style.view3d }); setOpen(true); }} title="Render paths and points at their source elevations">3D</button>
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
          <Section title="Local reference layers">
            <label className="style-row">Load platform / exit GeoJSON<input type="file" accept=".geojson,.json,application/geo+json" multiple onChange={(e) => { void importReferences(e.target.files); e.target.value = ""; }} /></label>
            <p className="style-note">Local files stay in this browser tab. Platforms are purple; files named "exit" are green. Use 3D to see their source heights.</p>
            {references.map((layer) => <div className="reference-layer" key={layer.name}><span>{layer.name} · {layer.features.features.length.toLocaleString()}</span><button type="button" className="btn btn-ghost btn-small" aria-label={`Remove ${layer.name}`} onClick={() => setState((s) => ({ referenceLayers: s.referenceLayers.filter((f) => f.name !== layer.name) }))}>×</button></div>)}
            {!!references.length && <button type="button" className="btn btn-ghost btn-small" onClick={() => setState((s) => ({ referenceFitRequest: s.referenceFitRequest + 1 }))}>Fit reference layers</button>}
            {importError && <p className="error-text" role="alert">{importError}</p>}
          </Section>
          <Section title="3D network and routes">
            <label className="style-check"><input type="checkbox" checked={style.view3d} onChange={(e) => set({ view3d: e.target.checked })} /> Show source elevations</label>
            {style.view3d && <>
              <label className="style-check"><input type="checkbox" checked={style.terrainEnabled} disabled={!style.terrainEnabled && !terrain} onChange={(e) => set({ terrainEnabled: e.target.checked })} /> DEM terrain</label>
              {terrainSources.length > 1 && <label className="style-note">Terrain source<select value={terrain?.id ?? ""} onChange={(e) => set({ terrainSourceId: e.target.value })}>{terrainSources.map(source => <option key={source.id} value={source.id}>{source.name}</option>)}</select></label>}
              {terrain && <p className="style-note">{terrain.name} · {terrain.resolution_m} m grid · {terrain.vertical_datum}. Terrain uses the same exaggeration as the network. Surveyed route heights are unchanged; the overlay remains visible through terrain.</p>}
              {terrainStatus && <p className="style-note">{terrainStatus}</p>}
              {style.terrainEnabled && terrainError && <p className="style-note" role="status">{terrainError}</p>}
              <button type="button" className="btn btn-ghost btn-small" onClick={() => setState(state => ({ terrainRefreshRequest: state.terrainRefreshRequest + 1 }))}>Refresh terrain sources</button>
              <label className="style-check"><input type="checkbox" checked={style.stationPlatforms} onChange={(e) => set({ stationPlatforms: e.target.checked })} /> Station platforms and boarding points</label>
              {style.stationPlatforms && <p className="style-note">{stationStatus ?? "Load a transit feed to see station geometry."} Purple: surveyed platform units. Amber: whole platform-floor footprints. Teal: bound GTFS boarding points. Hover for source and platform details.</p>}
              <Slider label="Exaggeration" value={style.verticalExaggeration} min={1} max={20} step={0.5} format={(v) => `${v}×`} onChange={(v) => set({ verticalExaggeration: v })} />
              <Slider label="Tilt" value={style.pitch} min={0} max={80} step={1} format={(v) => `${v}°`} onChange={(v) => set({ pitch: v })} />
              <label className="style-check"><input type="checkbox" checked={style.xray} onChange={(e) => set({ xray: e.target.checked })} /> X-ray basemap for underground paths</label>
              {style.xray && <Slider label="Basemap" value={style.basemapOpacity} min={0} max={1} step={0.05} format={(v) => `${Math.round(v * 100)}%`} onChange={(v) => set({ basemapOpacity: v })} />}
              <label className="style-check"><input type="checkbox" checked={style.elevationColor} onChange={(e) => set({ elevationColor: e.target.checked })} /> Colour by elevation</label>
              {style.elevationColor && elevations?.min !== null && elevations?.max !== null && <div className="elevation-ramp"><span>{elevations?.min?.toFixed(1)} m</span><i /><span>{elevations?.max?.toFixed(1)} m</span></div>}
              <label className="style-check"><input type="checkbox" checked={style.altitudeSlice} onChange={(e) => set({ altitudeSlice: e.target.checked })} /> Slice by source altitude</label>
              {style.altitudeSlice && <div className="altitude-inputs">
                <label>Min m<input type="number" value={style.minAltitude} step={1} onChange={(e) => { if (e.target.value !== "") set({ minAltitude: Number(e.target.value) }); }} /></label>
                <label>Max m<input type="number" value={style.maxAltitude} step={1} onChange={(e) => { if (e.target.value !== "") set({ maxAltitude: Number(e.target.value) }); }} /></label>
              </div>}
              {style.altitudeSlice && style.minAltitude > style.maxAltitude && <p className="style-note" role="alert">Minimum must be below maximum. No paths match this slice.</p>}
              {style.altitudeSlice && <p className="style-note">Lines are clipped to the slice. Surveyed surfaces that intersect it are shown whole.</p>}
              <p className="style-note">Heights are source metres, not floor numbers or depth below terrain. Below-datum paths stay visible. Exaggeration affects display only.</p>
              {!!elevations?.unknown && <p className="style-note">{elevations.unknown.toLocaleString()} geometries have missing or incomplete elevation. Missing coordinates draw at 0 m and use grey in elevation colours.</p>}
            </>}
          </Section>
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
