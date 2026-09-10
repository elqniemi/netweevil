import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ApiError, apiRequest, joinUrl } from "../api/client";
import type { FeedRoute, GtfsScenario, JobRecord, RoutePattern, ScenarioInfo, ScenarioLine, ScenarioResponse, ScenarioService, ServiceInfo, TransitMode } from "../api/types";
import { flyTo, getState, pushTiming, setEditor, setState, updateScenario, useStore } from "../state/store";
import { useJob } from "./SetupPanel";
import { DAY_LABELS, TRANSIT_MODES, lineColor } from "./editorShared";
import { fmtDuration, fmtNumber } from "./format";
import { JobProgress } from "./SetupPanel";

interface Props {
  service: ServiceInfo | null;
}

async function call<T>(path: string, options: { method?: "GET" | "POST" | "DELETE" | "PUT"; body?: unknown } = {}): Promise<T> {
  const base = getState().apiBase;
  try {
    const response = await apiRequest<T>(base, path, { tool: "transit_editor", ...(options as { method?: "GET" | "POST" | "DELETE"; body?: unknown }) });
    pushTiming(response.timing);
    return response.data;
  } catch (error) {
    if (error instanceof ApiError) pushTiming(error.timing);
    throw error;
  }
}

/** PUT is not in the timed client; send it directly. */
async function put<T>(path: string, body: unknown): Promise<T> {
  const response = await fetch(joinUrl(getState().apiBase, path), { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const text = await response.text();
  const data = text ? (JSON.parse(text) as T | { error?: string }) : null;
  if (!response.ok) throw new Error((data as { error?: string } | null)?.error ?? `HTTP ${response.status}`);
  return data as T;
}

function newService(): ScenarioService {
  return { days: [true, true, true, true, true, false, false], windows: [{ start: "06:00", end: "22:00", headway_min: 15 }], departures: [] };
}

function newLine(index: number, mode: TransitMode = "bus"): ScenarioLine {
  return {
    line_id: `L${index + 1}`,
    short_name: `${index + 1}`,
    long_name: "",
    mode,
    color: null,
    headsign: null,
    stops: [],
    average_speed_kph: mode === "rail" ? 120 : mode === "subway" ? 40 : mode === "tram" ? 25 : 30,
    default_dwell_s: mode === "rail" ? 60 : 20,
    bidirectional: true,
    services: [newService()],
  };
}

export function GtfsEditorPanel({ service }: Props) {
  const queryClient = useQueryClient();
  const editor = useStore((s) => s.editor);
  const apiBase = useStore((s) => s.apiBase);
  const [error, setError] = useState<string | null>(null);
  const [buildJob, setBuildJob] = useState<string | null>(null);
  const list = useQuery({ queryKey: ["scenarios", apiBase], queryFn: () => call<ScenarioInfo[]>("/v1/gtfs-editor/scenarios"), retry: 1 });

  // Open the persisted scenario (or none) whenever the id changes.
  useEffect(() => {
    if (!editor.scenarioId) {
      if (editor.scenario) setEditor({ scenario: null, info: null, dirty: false, activeLine: null });
      return;
    }
    if (editor.scenario?.scenario_id === editor.scenarioId) return;
    let cancelled = false;
    call<ScenarioResponse>(`/v1/gtfs-editor/scenarios/${encodeURIComponent(editor.scenarioId)}`)
      .then((response) => {
        if (cancelled) return;
        setEditor({ scenario: response.scenario, info: response.info, dirty: false, activeLine: response.scenario.lines.length ? 0 : null, selectedStop: null });
      })
      .catch((e: Error) => {
        if (cancelled) return;
        setError(e.message);
        setEditor({ scenarioId: null, scenario: null, info: null });
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editor.scenarioId]);

  const refreshInfo = async () => {
    if (!editor.scenarioId) return;
    const response = await call<ScenarioResponse>(`/v1/gtfs-editor/scenarios/${encodeURIComponent(editor.scenarioId)}`);
    setEditor({ info: response.info });
    void queryClient.invalidateQueries({ queryKey: ["scenarios"] });
  };

  const save = async (): Promise<boolean> => {
    if (!editor.scenario) return false;
    setError(null);
    try {
      const response = await put<ScenarioResponse>(`/v1/gtfs-editor/scenarios/${encodeURIComponent(editor.scenario.scenario_id)}`, editor.scenario);
      setEditor({ scenario: response.scenario, info: response.info, dirty: false });
      void queryClient.invalidateQueries({ queryKey: ["scenarios"] });
      return true;
    } catch (e) {
      setError((e as Error).message);
      return false;
    }
  };

  const job = useJob(buildJob, (finished: JobRecord) => {
    void refreshInfo();
    void queryClient.invalidateQueries({ queryKey: ["service"] });
    if (finished.state === "done" && finished.result && typeof finished.result.feed_id === "string") {
      // Make the built feed the active one for transit tools.
      setTimeout(() => getState().service && setEditorFeed(String(finished.result!.feed_id)), 400);
    }
  });

  const build = async () => {
    if (!editor.scenario) return;
    if (editor.dirty && !(await save())) return;
    setError(null);
    try {
      const started = await call<{ job_id: string }>(`/v1/gtfs-editor/scenarios/${encodeURIComponent(editor.scenario.scenario_id)}/build`, { body: { load: true } });
      setBuildJob(started.job_id);
    } catch (e) {
      setError((e as Error).message);
    }
  };

  const revert = async () => {
    if (!editor.scenario || !editor.info?.built) return;
    if (!window.confirm(`Remove the built feed '${editor.info.built.feed_id}'? The scenario stays; the original feed is unchanged.`)) return;
    setError(null);
    try {
      await call(`/v1/gtfs-editor/scenarios/${encodeURIComponent(editor.scenario.scenario_id)}/revert`, { method: "POST" });
      await refreshInfo();
      void queryClient.invalidateQueries({ queryKey: ["service"] });
      const base = editor.scenario.base_feed_id;
      if (base) setEditorFeed(base);
    } catch (e) {
      setError((e as Error).message);
    }
  };

  const remove = async () => {
    if (!editor.scenario) return;
    if (!window.confirm(`Delete scenario '${editor.scenario.label}' and its built feed (if any)?`)) return;
    try {
      await call(`/v1/gtfs-editor/scenarios/${encodeURIComponent(editor.scenario.scenario_id)}`, { method: "DELETE" });
      setEditor({ scenarioId: null, scenario: null, info: null, dirty: false, activeLine: null });
      void queryClient.invalidateQueries({ queryKey: ["scenarios"] });
      void queryClient.invalidateQueries({ queryKey: ["service"] });
    } catch (e) {
      setError((e as Error).message);
    }
  };

  return (
    <div className="editor-panel">
      <ScenarioPicker list={list.data ?? []} service={service} onError={setError} />
      {error && <div className="error-text">{error}</div>}
      {editor.scenario && editor.info && (
        <>
          <ScenarioStatus info={editor.info} scenario={editor.scenario} dirty={editor.dirty} />
          <div className="run-bar">
            <div className="run-row">
              <button type="button" className="btn btn-primary" onClick={() => void build()} disabled={job.data?.state === "running" || !!editor.info.validation_error}>
                {job.data?.state === "running" ? "Building…" : editor.info.built ? "Rebuild feed" : "Build feed"}
              </button>
              <button type="button" className="btn btn-small" onClick={() => void save()} disabled={!editor.dirty}>
                {editor.dirty ? "Save" : "Saved"}
              </button>
              {editor.info.built && (
                <button type="button" className="btn btn-ghost btn-small" onClick={() => void revert()} title="Unload and delete the built feed; keeps the scenario and the original feed">
                  Revert
                </button>
              )}
              <a className="btn btn-ghost btn-small" href={joinUrl(apiBase, `/v1/gtfs-editor/scenarios/${encodeURIComponent(editor.scenario.scenario_id)}/export`)} download title="Download the scenario as a GTFS zip">
                Export GTFS
              </a>
              <span className="spacer" />
              <button type="button" className="btn btn-ghost btn-small" onClick={() => void remove()}>
                Delete
              </button>
            </div>
            <JobProgress job={job.data} />
          </div>
          <LinesEditor scenario={editor.scenario} activeLine={editor.activeLine} mode={editor.mode} />
          {editor.scenario.base_feed_id && <BaseRoutesTools scenario={editor.scenario} feedId={editor.scenario.base_feed_id} />}
          {!editor.scenario.base_feed_id && <AgencyEditor scenario={editor.scenario} />}
        </>
      )}
    </div>
  );
}

/** Makes a feed the one the transit tools query, once the service lists it. */
function setEditorFeed(feedId: string) {
  const feeds = getState().service?.loaded_transit_feeds.map((f) => f.feed_id) ?? [];
  if (feeds.includes(feedId)) setState({ feedId });
}

// --- Scenario list / creation ---

function ScenarioPicker({ list, service, onError }: { list: ScenarioInfo[]; service: ServiceInfo | null; onError: (m: string | null) => void }) {
  const queryClient = useQueryClient();
  const scenarioId = useStore((s) => s.editor.scenarioId);
  const [creating, setCreating] = useState(false);
  const [label, setLabel] = useState("");
  const [id, setId] = useState("");
  const [base, setBase] = useState<string>("");
  const [start, setStart] = useState(() => new Date().toISOString().slice(0, 10));
  const [timezone, setTimezone] = useState("Europe/Amsterdam");
  const feeds = service?.loaded_transit_feeds ?? [];
  useEffect(() => {
    if (!base && feeds.length) setBase(feeds[0].feed_id);
  }, [feeds, base]);
  const create = async () => {
    onError(null);
    const scenario_id = (id || label).toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "");
    if (!scenario_id) {
      onError("Give the scenario a name.");
      return;
    }
    const document: GtfsScenario = {
      schema_version: 1,
      scenario_id,
      label: label || scenario_id,
      base_feed_id: base || null,
      output_feed_id: base ? `${base}_${scenario_id}` : scenario_id,
      agency: { name: label || scenario_id, url: "", timezone },
      service_start_date: start,
      service_days: 7,
      stops: [],
      lines: [],
      removed_route_ids: [],
      created_at: "",
      updated_at: "",
    };
    try {
      const response = await call<ScenarioResponse>("/v1/gtfs-editor/scenarios", { body: document });
      setEditor({ scenarioId: response.scenario.scenario_id, scenario: response.scenario, info: response.info, dirty: false, activeLine: null, selectedStop: null });
      void queryClient.invalidateQueries({ queryKey: ["scenarios"] });
      setCreating(false);
      setLabel("");
      setId("");
    } catch (e) {
      onError((e as Error).message);
    }
  };
  return (
    <div className="points">
      <div className="points-head">
        <span className="section-title">Scenario</span>
        <button type="button" className="btn btn-ghost btn-small" onClick={() => setCreating((v) => !v)}>
          {creating ? "Cancel" : "+ New"}
        </button>
      </div>
      <div className="slot-body">
        {!creating && (
          <select
            value={scenarioId ?? ""}
            onChange={(e) => {
              const next = e.target.value || null;
              if (next === scenarioId) return;
              setEditor({ scenarioId: next, scenario: null, info: null, dirty: false, activeLine: null, selectedStop: null });
            }}
            aria-label="Scenario"
          >
            <option value="">{list.length ? "choose a scenario…" : "no scenarios yet"}</option>
            {list.map((s) => (
              <option key={s.scenario_id} value={s.scenario_id}>
                {s.label} {s.base_feed_id ? `(on ${s.base_feed_id})` : "(stand-alone)"}
                {s.built ? " · built" : ""}
              </option>
            ))}
          </select>
        )}
        {creating && (
          <div className="form-grid">
            <label className="field field-wide">
              <span>Name</span>
              <input type="text" value={label} onChange={(e) => setLabel(e.target.value)} placeholder="Lelylijn 2035" autoFocus />
            </label>
            <label className="field">
              <span>Id</span>
              <input type="text" value={id} onChange={(e) => setId(e.target.value)} placeholder={label.toLowerCase().replace(/[^a-z0-9]+/g, "_") || "lelylijn"} />
            </label>
            <label className="field">
              <span>Builds on</span>
              <select value={base} onChange={(e) => setBase(e.target.value)}>
                <option value="">nothing (new feed from scratch)</option>
                {feeds.map((f) => (
                  <option key={f.feed_id} value={f.feed_id}>
                    {f.feed_id}
                  </option>
                ))}
              </select>
            </label>
            {!base && (
              <>
                <label className="field">
                  <span>First service date</span>
                  <input type="date" value={start} onChange={(e) => setStart(e.target.value)} />
                </label>
                <label className="field">
                  <span>Timezone</span>
                  <input type="text" value={timezone} onChange={(e) => setTimezone(e.target.value)} placeholder="Europe/Amsterdam" />
                </label>
              </>
            )}
            <div className="field-wide run-row">
              <button type="button" className="btn btn-primary btn-small" onClick={() => void create()}>
                Create scenario
              </button>
              <span className="muted">Saved as `.netweevil/gtfs_scenarios/&lt;id&gt;.json`</span>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function ScenarioStatus({ info, scenario, dirty }: { info: ScenarioInfo; scenario: GtfsScenario; dirty: boolean }) {
  return (
    <div className="editor-status">
      <div className="editor-status-row">
        <strong>{scenario.label}</strong>
        {dirty ? <span className="pill pill-warn">unsaved</span> : <span className="pill pill-ok">saved</span>}
        {info.built ? <span className={`pill ${info.built.loaded ? "pill-ok" : "pill-warn"}`}>feed {info.built.feed_id}{info.built.loaded ? " loaded" : " built, not loaded"}</span> : <span className="pill">not built</span>}
      </div>
      <div className="muted">
        {scenario.base_feed_id ? `On feed ${scenario.base_feed_id}` : "Stand-alone feed"} · service {scenario.service_start_date} + {scenario.service_days} d · {scenario.agency.timezone}
      </div>
      {info.summary && (
        <div className="muted">
          {info.summary.line_count} lines, {info.summary.stop_count} stops, {fmtNumber(info.summary.trip_count)} trips per week
          {scenario.removed_route_ids.length ? `, ${scenario.removed_route_ids.length} base routes removed` : ""}
        </div>
      )}
      {info.validation_error && <div className="hint-text">{info.validation_error}</div>}
      {!info.base_loaded && <div className="hint-text">Base feed {scenario.base_feed_id} is not loaded; load it in Setup to see its stops.</div>}
      <details className="editor-paths">
        <summary className="muted">Where this is saved</summary>
        <dl className="kv kv-compact">
          <dt>Scenario</dt>
          <dd>
            <code>{info.path}</code>
          </dd>
          {info.built && (
            <>
              <dt>Built feed</dt>
              <dd>
                <code>{info.built.bundle_path}</code>
              </dd>
            </>
          )}
          <dt>Export</dt>
          <dd>
            <code>{info.export_path}</code>
          </dd>
          {scenario.base_feed_id && (
            <>
              <dt>Original</dt>
              <dd className="muted">Feed {scenario.base_feed_id} and its GTFS file are never modified; Revert removes only the built feed.</dd>
            </>
          )}
        </dl>
      </details>
    </div>
  );
}

// --- Lines ---

function LinesEditor({ scenario, activeLine, mode }: { scenario: GtfsScenario; activeLine: number | null; mode: "select" | "add" }) {
  const line = activeLine !== null ? scenario.lines[activeLine] : null;
  const addLine = () => {
    updateScenario((s) => ({ ...s, lines: [...s.lines, newLine(s.lines.length)] }));
    setEditor({ activeLine: scenario.lines.length, mode: "add" });
  };
  const removeLine = (index: number) => {
    if (!window.confirm(`Remove line ${scenario.lines[index].short_name || scenario.lines[index].line_id}?`)) return;
    updateScenario((s) => {
      const lines = s.lines.filter((_, i) => i !== index);
      const used = new Set(lines.flatMap((l) => l.stops.map((st) => st.stop_id)));
      return { ...s, lines, stops: s.stops.filter((st) => used.has(st.stop_id)) };
    });
    setEditor({ activeLine: null });
  };
  return (
    <div className="points">
      <div className="points-head">
        <span className="section-title">Lines</span>
        <span className="muted">{scenario.lines.length}</span>
        <button type="button" className="btn btn-ghost btn-small" onClick={addLine}>
          + Add line
        </button>
      </div>
      {scenario.lines.length === 0 && <p className="muted slot-body">Add a line, then click stops on the map in order. Existing stops of the base feed can be clicked; clicking elsewhere creates a new stop.</p>}
      {scenario.lines.map((l, index) => (
        <div key={l.line_id} className={`slot${index === activeLine ? " is-active" : ""}`}>
          <button type="button" className="slot-head" onClick={() => setEditor({ activeLine: index, mode: "add" })}>
            <span className="slot-swatch" style={{ background: lineColor(l, index) }} />
            <span className="slot-label">
              {l.short_name || l.line_id} {l.long_name && <span className="muted">{l.long_name}</span>}
            </span>
            <span className="slot-count">
              {l.stops.length} stops · {l.mode}
            </span>
            <button type="button" className="icon-btn" onClick={(e) => { e.stopPropagation(); removeLine(index); }} title="Remove line" aria-label="Remove line">
              ×
            </button>
          </button>
          {index === activeLine && line && <LineDetail line={line} index={index} scenario={scenario} mode={mode} />}
        </div>
      ))}
    </div>
  );
}

function LineDetail({ line, index, scenario, mode }: { line: ScenarioLine; index: number; scenario: GtfsScenario; mode: "select" | "add" }) {
  const baseStops = useStore((s) => s.editor.baseStops);
  const patch = (p: Partial<ScenarioLine>) => updateScenario((s) => ({ ...s, lines: s.lines.map((l, i) => (i === index ? { ...l, ...p } : l)) }));
  const stopName = (id: string) => scenario.stops.find((s) => s.stop_id === id)?.name ?? baseStops.find((s) => s.stop_id === id)?.name ?? id;
  const stopCoord = (id: string) => scenario.stops.find((s) => s.stop_id === id) ?? baseStops.find((s) => s.stop_id === id) ?? null;
  const setStop = (position: number, p: { travel_s?: number | null; dwell_s?: number | null }) => patch({ stops: line.stops.map((s, i) => (i === position ? { ...s, ...p } : s)) });
  const move = (position: number, delta: number) => {
    const stops = line.stops.slice();
    const target = position + delta;
    if (target < 0 || target >= stops.length) return;
    [stops[position], stops[target]] = [stops[target], stops[position]];
    patch({ stops });
  };
  const removeAt = (position: number) => {
    const removed = line.stops[position].stop_id;
    updateScenario((s) => {
      const lines = s.lines.map((l, i) => (i === index ? { ...l, stops: l.stops.filter((_, k) => k !== position) } : l));
      const used = lines.some((l) => l.stops.some((st) => st.stop_id === removed));
      return { ...s, lines, stops: used ? s.stops : s.stops.filter((st) => st.stop_id !== removed) };
    });
  };
  const renameStop = (id: string, name: string) => updateScenario((s) => ({ ...s, stops: s.stops.map((st) => (st.stop_id === id ? { ...st, name } : st)) }));
  const isScenarioStop = (id: string) => scenario.stops.some((s) => s.stop_id === id);
  const totalTime = estimateRunTime(line, stopCoord);
  return (
    <div className="slot-body line-detail">
      <div className="form-grid">
        <label className="field">
          <span>Short name</span>
          <input type="text" value={line.short_name} onChange={(e) => patch({ short_name: e.target.value })} />
        </label>
        <label className="field">
          <span>Long name</span>
          <input type="text" value={line.long_name} onChange={(e) => patch({ long_name: e.target.value })} placeholder="Groningen – Lelystad" />
        </label>
        <label className="field">
          <span>Mode</span>
          <select value={line.mode} onChange={(e) => patch({ mode: e.target.value as TransitMode })}>
            {TRANSIT_MODES.map((m) => (
              <option key={m.value} value={m.value}>
                {m.label}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span>Colour</span>
          <input type="color" value={lineColor(line, index)} onChange={(e) => patch({ color: e.target.value.replace("#", "") })} aria-label="Line colour" />
        </label>
        <label className="field">
          <span>Average speed (km/h)</span>
          <input type="number" min={1} step={5} value={line.average_speed_kph} onChange={(e) => patch({ average_speed_kph: Number(e.target.value) })} />
          <small className="muted">Used where a stop has no explicit running time.</small>
        </label>
        <label className="field">
          <span>Dwell per stop (s)</span>
          <input type="number" min={0} step={10} value={line.default_dwell_s} onChange={(e) => patch({ default_dwell_s: Number(e.target.value) })} />
        </label>
        <label className="field field-check">
          <input type="checkbox" checked={line.bidirectional} onChange={(e) => patch({ bidirectional: e.target.checked })} />
          <span>Run both directions</span>
        </label>
      </div>
      <div className="points-head editor-stops-head">
        <span className="section-title">Stops</span>
        <span className="muted">{line.stops.length}{totalTime !== null ? ` · ${fmtDuration(totalTime)} end to end` : ""}</span>
        <button type="button" className={`btn btn-small${mode === "add" ? " btn-primary" : ""}`} onClick={() => setEditor({ mode: mode === "add" ? "select" : "add" })} title="When on, map clicks append stops to this line">
          {mode === "add" ? "Adding stops: click the map" : "Add stops"}
        </button>
      </div>
      {line.stops.length === 0 && <p className="muted">Click stops on the map in order. Right-click a stop to remove it; drag a new stop to move it.</p>}
      <ol className="point-list editor-stop-list">
        {line.stops.map((s, position) => {
          const coord = stopCoord(s.stop_id);
          return (
            <li key={`${s.stop_id}-${position}`}>
              <span className="point-index">{position + 1}</span>
              {isScenarioStop(s.stop_id) ? (
                <input value={stopName(s.stop_id)} onChange={(e) => renameStop(s.stop_id, e.target.value)} aria-label="Stop name" title={`New stop ${s.stop_id}`} />
              ) : (
                <button type="button" className="stop-name" onClick={() => coord && flyTo(coord.lon, coord.lat, 14)} title={`Base feed stop ${s.stop_id}`}>
                  {stopName(s.stop_id)}
                </button>
              )}
              {position > 0 && (
                <input type="number" className="point-weight" min={0} step={30} value={s.travel_s ?? ""} placeholder="auto" onChange={(e) => setStop(position, { travel_s: e.target.value === "" ? null : Number(e.target.value) })} title="Running time from previous stop (s); blank = from speed" aria-label="Running time" />
              )}
              <button type="button" className="icon-btn" onClick={() => move(position, -1)} title="Move up" aria-label="Move up" disabled={position === 0}>
                ↑
              </button>
              <button type="button" className="icon-btn" onClick={() => move(position, 1)} title="Move down" aria-label="Move down" disabled={position === line.stops.length - 1}>
                ↓
              </button>
              <button type="button" className="icon-btn" onClick={() => removeAt(position)} title="Remove from line" aria-label="Remove">
                ×
              </button>
            </li>
          );
        })}
      </ol>
      <div className="points-head editor-stops-head">
        <span className="section-title">Timetable</span>
        <button type="button" className="btn btn-ghost btn-small" onClick={() => patch({ services: [...line.services, newService()] })}>
          + Service
        </button>
      </div>
      {line.services.map((svc, si) => (
        <ServiceEditor key={si} service={svc} onChange={(next) => patch({ services: line.services.map((x, i) => (i === si ? next : x)) })} onRemove={line.services.length > 1 ? () => patch({ services: line.services.filter((_, i) => i !== si) }) : undefined} />
      ))}
    </div>
  );
}

function estimateRunTime(line: ScenarioLine, coord: (id: string) => { lon: number; lat: number } | null): number | null {
  if (line.stops.length < 2) return null;
  let total = 0;
  for (let i = 1; i < line.stops.length; i++) {
    const s = line.stops[i];
    if (s.travel_s != null) total += s.travel_s;
    else {
      const a = coord(line.stops[i - 1].stop_id);
      const b = coord(s.stop_id);
      if (!a || !b) return null;
      total += (haversine(a.lon, a.lat, b.lon, b.lat) / (line.average_speed_kph * 1000)) * 3600;
    }
    if (i < line.stops.length - 1) total += s.dwell_s ?? line.default_dwell_s;
  }
  return Math.round(total);
}

function haversine(lon1: number, lat1: number, lon2: number, lat2: number): number {
  const r = Math.PI / 180;
  const dLat = (lat2 - lat1) * r;
  const dLon = (lon2 - lon1) * r;
  const h = Math.sin(dLat / 2) ** 2 + Math.cos(lat1 * r) * Math.cos(lat2 * r) * Math.sin(dLon / 2) ** 2;
  return 2 * 6371008.8 * Math.asin(Math.sqrt(h));
}

function ServiceEditor({ service, onChange, onRemove }: { service: ScenarioService; onChange: (s: ScenarioService) => void; onRemove?: () => void }) {
  const toggleDay = (i: number) => {
    const days = [...service.days] as ScenarioService["days"];
    days[i] = !days[i];
    onChange({ ...service, days });
  };
  const setWindow = (wi: number, p: Partial<ScenarioService["windows"][number]>) => onChange({ ...service, windows: service.windows.map((w, i) => (i === wi ? { ...w, ...p } : w)) });
  return (
    <div className="service-editor">
      <div className="day-row">
        {DAY_LABELS.map((d, i) => (
          <button key={d} type="button" className={`chip${service.days[i] ? " is-active" : ""}`} onClick={() => toggleDay(i)}>
            {d}
          </button>
        ))}
        <span className="spacer" />
        {onRemove && (
          <button type="button" className="icon-btn" onClick={onRemove} title="Remove service" aria-label="Remove service">
            ×
          </button>
        )}
      </div>
      {service.windows.map((w, wi) => (
        <div key={wi} className="window-row">
          <input type="time" value={w.start} onChange={(e) => setWindow(wi, { start: e.target.value })} aria-label="First departure" />
          <span className="muted">to</span>
          <input type="time" value={w.end} onChange={(e) => setWindow(wi, { end: e.target.value })} aria-label="Last departure" />
          <span className="muted">every</span>
          <input type="number" min={1} value={w.headway_min} onChange={(e) => setWindow(wi, { headway_min: Number(e.target.value) })} aria-label="Headway (minutes)" />
          <span className="muted">min</span>
          <button type="button" className="icon-btn" onClick={() => onChange({ ...service, windows: service.windows.filter((_, i) => i !== wi) })} title="Remove window" aria-label="Remove window">
            ×
          </button>
        </div>
      ))}
      <div className="run-row">
        <button type="button" className="btn btn-ghost btn-small" onClick={() => onChange({ ...service, windows: [...service.windows, { start: "07:00", end: "09:00", headway_min: 10 }] })}>
          + Headway window
        </button>
        <label className="field-inline">
          <span className="muted">Extra departures</span>
          <input type="text" value={service.departures.join(", ")} onChange={(e) => onChange({ ...service, departures: e.target.value.split(/[,\s]+/).map((v) => v.trim()).filter(Boolean) })} placeholder="23:30, 00:15" aria-label="Extra departures" />
        </label>
      </div>
    </div>
  );
}

function AgencyEditor({ scenario }: { scenario: GtfsScenario }) {
  return (
    <details className="points">
      <summary className="points-head section-title">Agency and service window</summary>
      <div className="slot-body form-grid">
        <label className="field">
          <span>Agency name</span>
          <input type="text" value={scenario.agency.name} onChange={(e) => updateScenario((s) => ({ ...s, agency: { ...s.agency, name: e.target.value } }))} />
        </label>
        <label className="field">
          <span>Timezone</span>
          <input type="text" value={scenario.agency.timezone} onChange={(e) => updateScenario((s) => ({ ...s, agency: { ...s.agency, timezone: e.target.value } }))} />
        </label>
        <label className="field">
          <span>First service date</span>
          <input type="date" value={scenario.service_start_date ?? ""} onChange={(e) => updateScenario((s) => ({ ...s, service_start_date: e.target.value }))} />
        </label>
        <label className="field">
          <span>Days</span>
          <input type="number" min={1} max={31} value={scenario.service_days ?? 7} onChange={(e) => updateScenario((s) => ({ ...s, service_days: Number(e.target.value) }))} />
        </label>
      </div>
    </details>
  );
}

// --- Base feed tools: clone a route as an express variant, remove routes ---

function BaseRoutesTools({ scenario, feedId }: { scenario: GtfsScenario; feedId: string }) {
  const apiBase = useStore((s) => s.apiBase);
  const [q, setQ] = useState("");
  const [routeId, setRouteId] = useState<string>("");
  const routes = useQuery({ queryKey: ["feed-routes", apiBase, feedId, q], queryFn: () => call<FeedRoute[]>(`/v1/transit-feeds/${encodeURIComponent(feedId)}/routes?limit=200&q=${encodeURIComponent(q)}`), staleTime: 60_000 });
  const patterns = useQuery({
    queryKey: ["feed-patterns", apiBase, feedId, routeId],
    queryFn: () => call<RoutePattern[]>(`/v1/transit-feeds/${encodeURIComponent(feedId)}/patterns?route_id=${encodeURIComponent(routeId)}&limit=8`),
    enabled: !!routeId,
    staleTime: 60_000,
  });
  const selected = routes.data?.find((r) => r.route_id === routeId);
  const clone = (pattern: RoutePattern, express: boolean) => {
    if (!selected) return;
    const stops = pattern.stops.map((s, i) => ({
      stop_id: s.stop_id,
      // Keep the observed running times between consecutive kept stops.
      travel_s: i === 0 ? null : Math.max(30, s.arrival_offset_s - pattern.stops[i - 1].departure_offset_s),
      dwell_s: null,
    }));
    const line: ScenarioLine = {
      ...newLine(scenario.lines.length, selected.mode),
      line_id: `${selected.short_name || selected.route_id}${express ? "X" : ""}_${scenario.lines.length + 1}`.replace(/[^A-Za-z0-9_-]/g, "_"),
      short_name: `${selected.short_name || selected.route_id}${express ? "X" : ""}`,
      long_name: `${express ? "Express " : ""}${selected.long_name || pattern.headsign}`,
      headsign: pattern.headsign || null,
      stops,
      bidirectional: true,
    };
    updateScenario((s) => ({ ...s, lines: [...s.lines, line] }));
    setEditor({ activeLine: scenario.lines.length, mode: "select" });
    const first = pattern.stops[0];
    if (first) flyTo(first.lon, first.lat, 11);
  };
  const toggleRemoved = (id: string) => updateScenario((s) => ({ ...s, removed_route_ids: s.removed_route_ids.includes(id) ? s.removed_route_ids.filter((r) => r !== id) : [...s.removed_route_ids, id] }));
  return (
    <details className="points">
      <summary className="points-head section-title">Existing routes of {feedId}</summary>
      <div className="slot-body">
        <p className="muted">Copy a route to edit it (remove stops for an express variant, change its timetable), or drop it from the scenario feed so your line replaces it.</p>
        <div className="run-row">
          <input type="text" value={q} onChange={(e) => setQ(e.target.value)} placeholder="search routes" aria-label="Search routes" />
          <select value={routeId} onChange={(e) => setRouteId(e.target.value)} aria-label="Route">
            <option value="">choose a route…</option>
            {(routes.data ?? []).map((r) => (
              <option key={r.route_id} value={r.route_id}>
                {r.short_name || r.route_id} {r.long_name} ({r.mode}, {r.trip_count} trips)
              </option>
            ))}
          </select>
        </div>
        {selected && (
          <div className="run-row">
            <label className="field-check">
              <input type="checkbox" checked={scenario.removed_route_ids.includes(selected.route_id)} onChange={() => toggleRemoved(selected.route_id)} />
              <span>Remove this route from the scenario feed</span>
            </label>
          </div>
        )}
        {patterns.data?.map((p, i) => (
          <div key={i} className="pattern-row">
            <span>
              → {p.headsign || "(no headsign)"} <span className="muted">{p.stops.length} stops, {p.run_count} runs/week, {fmtDuration(p.stops[p.stops.length - 1]?.arrival_offset_s ?? 0)}</span>
            </span>
            <button type="button" className="btn btn-ghost btn-small" onClick={() => clone(p, false)}>
              Copy
            </button>
            <button type="button" className="btn btn-ghost btn-small" onClick={() => clone(p, true)} title="Copy as an express variant: then remove intermediate stops from the list">
              Copy as express
            </button>
          </div>
        ))}
        {scenario.removed_route_ids.length > 0 && (
          <div className="muted">
            Removed: {scenario.removed_route_ids.map((id) => (
              <button key={id} type="button" className="chip is-active" onClick={() => toggleRemoved(id)} title="Click to keep the route again">
                {id} ×
              </button>
            ))}
          </div>
        )}
      </div>
    </details>
  );
}
