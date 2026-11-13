import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { h3Api } from "../api/client";
import type { CellAggregation, CellLayerSummary, CellPaintOp } from "../api/types";
import { PAINT_RESOLUTION, clampResolution, importBody, isScratchLayer, isValidLayerId, styleSurgeCells, styleValueCells, surgeLegend, valueLegend } from "../geo/cells";
import { EMPTY_COLLECTION, type FeatureCollection } from "../geo/features";
import { CELLS_TOOL, cellsCall } from "../state/cells";
import { getState, setCells, useStore } from "../state/store";
import { fmtNumber } from "./format";

const OPS: { value: CellPaintOp; label: string }[] = [
  { value: "set", label: "set" },
  { value: "add", label: "add" },
  { value: "multiply", label: "multiply" },
  { value: "clear", label: "clear" },
];

const AGGREGATIONS: CellAggregation[] = ["sum", "mean", "min", "max", "count", "last"];

function asCollection(data: unknown): FeatureCollection {
  const value = data as FeatureCollection | null;
  return value && Array.isArray(value.features) ? value : EMPTY_COLLECTION;
}

function layersKey(apiBase: string) {
  return ["h3-layers", apiBase];
}

/** Layer list, the selected layer's hexagons and the surge overlay; shared by the map and the panel. */
function useCellQueries() {
  const active = useStore((s) => s.tool === "cells");
  const apiBase = useStore((s) => s.apiBase);
  const layerId = useStore((s) => s.cells.layerId);
  const surgeOn = useStore((s) => s.cells.surge && !!s.cells.demandColumn && !!s.cells.supplyColumn);
  const demand = useStore((s) => s.cells.demandColumn);
  const supply = useStore((s) => s.cells.supplyColumn);
  const layers = useQuery({
    queryKey: layersKey(apiBase),
    queryFn: async () => {
      const data = await cellsCall((base) => h3Api.layers(base, CELLS_TOOL));
      return (data?.layers ?? []).filter((l) => !isScratchLayer(l.layer_id));
    },
    enabled: active,
    retry: false,
    staleTime: 10_000,
  });
  const summary = layers.data?.find((l) => l.layer_id === layerId) ?? null;
  const geo = useQuery({
    queryKey: ["h3-layer-geojson", apiBase, layerId],
    queryFn: () => cellsCall((base) => h3Api.geojson(base, CELLS_TOOL, layerId!)),
    enabled: active && !!summary,
    retry: false,
  });
  const surge = useQuery({
    queryKey: ["h3-surge", apiBase, layerId, demand, supply],
    queryFn: () => cellsCall((base) => h3Api.surgeGeojson(base, CELLS_TOOL, { demand: { layer_id: layerId!, column: demand }, supply: { layer_id: layerId!, column: supply } })),
    enabled: active && !!summary && surgeOn,
    retry: false,
  });
  return { active, apiBase, layers, summary, geo, surge, surgeOn: surgeOn && !!summary };
}

function refetchCells(queryClient: QueryClient) {
  for (const key of ["h3-layers", "h3-layer-geojson", "h3-surge"]) void queryClient.invalidateQueries({ queryKey: [key] });
}

/** Adds or replaces a layer summary in the cached list, so a new layer can be selected at once. */
function cacheSummary(queryClient: QueryClient, apiBase: string, summary: CellLayerSummary) {
  queryClient.setQueryData<CellLayerSummary[]>(layersKey(apiBase), (old) => [...(old ?? []).filter((l) => l.layer_id !== summary.layer_id), summary]);
}

/**
 * Keeps the Cells selection valid and turns the layer (or surge) GeoJSON into
 * styled hexagons for the map. Mounted once, in the app shell.
 */
export function useCellsOverlay() {
  const queryClient = useQueryClient();
  const { active, layers, summary, geo, surge, surgeOn } = useCellQueries();
  const layerId = useStore((s) => s.cells.layerId);
  const column = useStore((s) => s.cells.column);
  const revision = useStore((s) => s.cells.revision);
  const shownLayer = useRef<string | null>(null);

  useEffect(() => {
    if (!active || !layers.isSuccess) return;
    const ids = layers.data.map((l) => l.layer_id);
    if (layerId && ids.includes(layerId)) return;
    if (layerId === (ids[0] ?? null)) return;
    setCells({ layerId: ids[0] ?? null, surge: false });
  }, [active, layers.isSuccess, layers.data, layerId]);

  // A newly shown layer starts on its first column, unless it has the current one.
  useEffect(() => {
    if (!summary || shownLayer.current === summary.layer_id) return;
    shownLayer.current = summary.layer_id;
    const names = summary.columns.map((c) => c.name);
    const current = getState().cells.column;
    if (!names.length || names.includes(current)) return;
    setCells({ column: names[0] });
  }, [summary]);

  useEffect(() => {
    if (revision > 0) refetchCells(queryClient);
  }, [revision, queryClient]);

  const stats = summary?.columns.find((c) => c.name === column) ?? null;
  useEffect(() => {
    if (!active) {
      const s = getState().cells;
      if (s.features || s.legend) setCells({ features: null, legend: null });
      return;
    }
    if (surgeOn) {
      setCells({ features: surge.data ? styleSurgeCells(asCollection(surge.data)) : null, legend: surgeLegend() });
      return;
    }
    setCells({ features: summary && geo.data ? styleValueCells(asCollection(geo.data), column, stats) : null, legend: valueLegend(column, stats) });
  }, [active, surgeOn, surge.data, geo.data, summary, column, stats]);
}

export function CellsPanel() {
  const queryClient = useQueryClient();
  const { apiBase, layers, summary, geo, surge } = useCellQueries();
  const cells = useStore((s) => s.cells);
  const [newLayer, setNewLayer] = useState("");
  const [newColumn, setNewColumn] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [file, setFile] = useState<File | null>(null);
  const [importTarget, setImportTarget] = useState("");
  const [importPoints, setImportPoints] = useState(false);
  const [importResolution, setImportResolution] = useState<number>(PAINT_RESOLUTION.default);
  const [lonColumn, setLonColumn] = useState("lon");
  const [latColumn, setLatColumn] = useState("lat");
  const [aggregation, setAggregation] = useState<CellAggregation>("sum");
  const [valueText, setValueText] = useState(String(cells.value));
  const fileInput = useRef<HTMLInputElement | null>(null);

  const list = layers.data ?? [];
  const columns = summary?.columns ?? [];
  const columnNames = columns.map((c) => c.name);
  const layerId = summary ? cells.layerId : null;

  const act = async (work: () => Promise<string | null>) => {
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      setMessage(await work());
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  const createLayer = () =>
    act(async () => {
      const id = newLayer.trim();
      if (!isValidLayerId(id)) throw new Error("Layer ids use ASCII letters, digits, _ and -.");
      // `merge` keeps an existing layer of the same id intact.
      const response = await cellsCall((base) => h3Api.write(base, CELLS_TOOL, { layer_id: id, merge: true }));
      cacheSummary(queryClient, apiBase, response.layer);
      setCells({ layerId: id, surge: false });
      setNewLayer("");
      refetchCells(queryClient);
      return null;
    });

  const deleteLayer = () => {
    if (!layerId || !window.confirm(`Delete layer ${layerId}?`)) return;
    void act(async () => {
      await cellsCall((base) => h3Api.remove(base, CELLS_TOOL, layerId));
      queryClient.setQueryData<CellLayerSummary[]>(layersKey(apiBase), (old) => (old ?? []).filter((l) => l.layer_id !== layerId));
      setCells({ layerId: null, surge: false });
      refetchCells(queryClient);
      return null;
    });
  };

  const importCsv = () =>
    act(async () => {
      if (!file) throw new Error("Choose a CSV file.");
      const target = importTarget.trim() || layerId || "";
      if (!isValidLayerId(target)) throw new Error("Choose a layer or type a layer id.");
      const text = await file.text();
      const body = importBody(text, { layerId: target, points: importPoints, resolution: importResolution, lonColumn: lonColumn.trim(), latColumn: latColumn.trim(), aggregation });
      const response = await cellsCall((base) => h3Api.write(base, CELLS_TOOL, body));
      cacheSummary(queryClient, apiBase, response.layer);
      setCells({ layerId: target });
      setImportTarget("");
      setFile(null);
      if (fileInput.current) fileInput.current.value = "";
      refetchCells(queryClient);
      const rows = response.csv_rows;
      return rows ? `${fmtNumber(rows[0])} rows read, ${fmtNumber(rows[1])} skipped.` : null;
    });

  const applyNewColumn = () => {
    const name = newColumn.trim();
    if (!name) return;
    setCells({ column: name });
    setNewColumn("");
  };

  const columnOptions = cells.column && !columnNames.includes(cells.column) ? [...columnNames, cells.column] : columnNames;

  return (
    <div className="cells-panel">
      <div className="points">
        <div className="points-head">
          <span className="section-title">Layer</span>
          {cells.pending > 0 && <span className="muted">Painting…</span>}
          <button type="button" className="btn btn-ghost btn-small" onClick={() => void layers.refetch()} disabled={layers.isFetching}>
            Refresh
          </button>
        </div>
        <div className="slot-body">
          <select value={layerId ?? ""} onChange={(e) => setCells({ layerId: e.target.value || null, surge: false })} aria-label="Layer" disabled={!list.length}>
            {!list.length && <option value="">{layers.isLoading ? "loading…" : "no layers"}</option>}
            {list.map((l) => (
              <option key={l.layer_id} value={l.layer_id}>
                {l.label && l.label !== l.layer_id ? `${l.label} (${l.layer_id})` : l.layer_id} · {fmtNumber(l.cell_count)}
              </option>
            ))}
          </select>
          {summary && (
            <dl className="kv kv-compact">
              <dt>cells</dt>
              <dd>{fmtNumber(summary.cell_count)}</dd>
              {summary.resolutions.length > 0 && (
                <>
                  <dt>resolution</dt>
                  <dd>{summary.resolutions.join(", ")}</dd>
                </>
              )}
            </dl>
          )}
          <div className="cells-row">
            {layerId && (
              <>
                <button type="button" className="btn btn-ghost btn-small" onClick={() => setCells((prev) => ({ fitRequest: prev.fitRequest + 1 }))} disabled={!geo.data || !asCollection(geo.data).features.length}>
                  Zoom to
                </button>
                <a className="btn btn-ghost btn-small" href={h3Api.csvUrl(apiBase, layerId)} download={`${layerId}.csv`}>
                  CSV
                </a>
                <button type="button" className="btn btn-ghost btn-small" onClick={deleteLayer} disabled={busy}>
                  Delete
                </button>
              </>
            )}
          </div>
          <div className="cells-row">
            <input type="text" value={newLayer} onChange={(e) => setNewLayer(e.target.value)} onKeyDown={(e) => e.key === "Enter" && void createLayer()} placeholder="new layer id" aria-label="New layer id" />
            <button type="button" className="btn btn-small" onClick={() => void createLayer()} disabled={busy || !newLayer.trim()}>
              Create
            </button>
          </div>
          {layers.error && <p className="error-text">{(layers.error as Error).message}</p>}
          {geo.error && <p className="error-text">{(geo.error as Error).message}</p>}
        </div>
      </div>

      <div className="points">
        <div className="points-head">
          <span className="section-title">Column</span>
        </div>
        <div className="slot-body">
          <select value={cells.column} onChange={(e) => setCells({ column: e.target.value })} aria-label="Column" disabled={!columnOptions.length}>
            {!columnOptions.length && <option value="">no columns</option>}
            {columnOptions.map((name) => {
              const stats = columns.find((c) => c.name === name);
              return (
                <option key={name} value={name}>
                  {stats ? `${name} · ${fmtNumber(stats.count)}` : `${name} (new)`}
                </option>
              );
            })}
          </select>
          <div className="cells-row">
            <input type="text" value={newColumn} onChange={(e) => setNewColumn(e.target.value)} onKeyDown={(e) => e.key === "Enter" && applyNewColumn()} placeholder="new column" aria-label="New column" />
            <button type="button" className="btn btn-small" onClick={applyNewColumn} disabled={!newColumn.trim()}>
              Use
            </button>
          </div>
        </div>
      </div>

      <div className="points">
        <div className="points-head">
          <label className="field-check">
            <input type="checkbox" checked={cells.paint} onChange={(e) => setCells({ paint: e.target.checked, error: null })} disabled={!layerId} />
            <span className="section-title">Paint</span>
          </label>
        </div>
        {cells.paint && (
          <div className="slot-body">
            <div className="segmented" role="group" aria-label="Paint mode">
              <button type="button" className={cells.paintMode === "brush" ? "is-active" : ""} onClick={() => setCells({ paintMode: "brush" })} title="Click or drag to paint cells along the pointer">
                Brush
              </button>
              <button type="button" className={cells.paintMode === "area" ? "is-active" : ""} onClick={() => setCells({ paintMode: "area" })} title="Drag a rectangle to paint every cell inside it">
                Area
              </button>
            </div>
            <div className="form-grid">
              <label className="field">
                <span>Resolution</span>
                <input type="number" min={PAINT_RESOLUTION.min} max={PAINT_RESOLUTION.max} step={1} value={cells.resolution} onChange={(e) => setCells({ resolution: clampResolution(e.target.value, cells.resolution) })} />
              </label>
              <label className="field">
                <span>Operation</span>
                <select value={cells.op} onChange={(e) => setCells({ op: e.target.value as CellPaintOp })}>
                  {OPS.map((o) => (
                    <option key={o.value} value={o.value}>
                      {o.label}
                    </option>
                  ))}
                </select>
              </label>
              {cells.op !== "clear" && (
                <label className="field">
                  <span>Value</span>
                  <input
                    type="number"
                    step="any"
                    value={valueText}
                    onChange={(e) => {
                      setValueText(e.target.value);
                      const value = Number(e.target.value);
                      if (e.target.value.trim() !== "" && Number.isFinite(value)) setCells({ value });
                    }}
                    onBlur={() => setValueText(String(getState().cells.value))}
                  />
                </label>
              )}
            </div>
          </div>
        )}
        {cells.error && <p className="error-text cells-error">{cells.error}</p>}
      </div>

      <div className="points">
        <div className="points-head">
          <label className="field-check">
            <input type="checkbox" checked={cells.surge} onChange={(e) => setCells({ surge: e.target.checked })} disabled={!layerId} />
            <span className="section-title">Surge</span>
          </label>
        </div>
        <div className="slot-body">
          <div className="form-grid">
            <label className="field">
              <span>Demand</span>
              <select value={cells.demandColumn} onChange={(e) => setCells({ demandColumn: e.target.value })}>
                <option value="">–</option>
                {columnNames.map((name) => (
                  <option key={name} value={name}>
                    {name}
                  </option>
                ))}
              </select>
            </label>
            <label className="field">
              <span>Supply</span>
              <select value={cells.supplyColumn} onChange={(e) => setCells({ supplyColumn: e.target.value })}>
                <option value="">–</option>
                {columnNames.map((name) => (
                  <option key={name} value={name}>
                    {name}
                  </option>
                ))}
              </select>
            </label>
          </div>
          {surge.error && <p className="error-text">{(surge.error as Error).message}</p>}
        </div>
      </div>

      <div className="points">
        <div className="points-head">
          <span className="section-title">Import CSV</span>
        </div>
        <div className="slot-body">
          <input ref={fileInput} type="file" accept=".csv,text/csv" onChange={(e) => setFile(e.target.files?.[0] ?? null)} aria-label="CSV file" />
          <div className="form-grid">
            <label className="field">
              <span>Into layer</span>
              <input type="text" value={importTarget} onChange={(e) => setImportTarget(e.target.value)} placeholder={layerId ?? "layer id"} />
            </label>
            <label className="field">
              <span>Aggregation</span>
              <select value={aggregation} onChange={(e) => setAggregation(e.target.value as CellAggregation)}>
                {AGGREGATIONS.map((a) => (
                  <option key={a} value={a}>
                    {a}
                  </option>
                ))}
              </select>
            </label>
            <label className="field field-check">
              <input type="checkbox" checked={importPoints} onChange={(e) => setImportPoints(e.target.checked)} />
              <span>Points</span>
            </label>
            {importPoints && (
              <>
                <label className="field">
                  <span>Resolution</span>
                  <input type="number" min={0} max={15} step={1} value={importResolution} onChange={(e) => setImportResolution(Math.max(0, Math.min(15, Math.round(Number(e.target.value) || 0))))} />
                </label>
                <span />
                <label className="field">
                  <span>Longitude column</span>
                  <input type="text" value={lonColumn} onChange={(e) => setLonColumn(e.target.value)} placeholder="lon" />
                </label>
                <label className="field">
                  <span>Latitude column</span>
                  <input type="text" value={latColumn} onChange={(e) => setLatColumn(e.target.value)} placeholder="lat" />
                </label>
              </>
            )}
          </div>
          <div className="cells-row">
            <button type="button" className="btn btn-small" onClick={() => void importCsv()} disabled={busy || !file}>
              {busy ? "Working…" : "Import"}
            </button>
          </div>
        </div>
      </div>
      {message && <p className="muted">{message}</p>}
      {error && <p className="error-text">{error}</p>}
    </div>
  );
}
