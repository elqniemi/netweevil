import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ApiError, apiRequest, joinUrl } from "../api/client";
import type { ServiceInfo, SimulationFramesResponse, SimulationInfo } from "../api/types";
import { PALETTE, heatRamp, type Feature, type FeatureCollection } from "../geo/features";
import { currentBody } from "../state/run";
import { getState, pushTiming, requestFit, setState, useStore } from "../state/store";
import { fmtDuration, fmtMs, fmtNumber } from "./format";

const FLEET_COLORS = [PALETTE.crimson, PALETTE.blue, PALETTE.green, PALETTE.ochre, PALETTE.violet, PALETTE.pink];

interface Props {
  service: ServiceInfo | null;
}

async function call<T>(path: string, options: { method?: "GET" | "POST" | "DELETE"; body?: unknown; tag?: string } = {}): Promise<T> {
  const base = getState().apiBase;
  try {
    const response = await apiRequest<T>(base, path, { tool: "simulation", ...options });
    pushTiming(response.timing);
    return response.data;
  } catch (error) {
    if (error instanceof ApiError) pushTiming(error.timing);
    throw error;
  }
}

function isActive(state: string | undefined) {
  return state === "pending" || state === "dispatching" || state === "running" || state === "paused";
}

export function SimulationPanel({ service }: Props) {
  const queryClient = useQueryClient();
  const selectedId = useStore((s) => s.simulation.selectedId);
  const running = useStore((s) => s.running);
  const [createError, setCreateError] = useState<string | null>(null);
  const [showJson, setShowJson] = useState(false);
  const raw = useStore((s) => s.raw.simulation ?? null);
  useStore((s) => s.points);
  useStore((s) => s.form.simulation);
  useStore((s) => s.viewport.bbox);
  const preview = currentBody("simulation", service);
  const previewText = raw ?? (preview.body ? JSON.stringify(preview.body, null, 2) : "");

  const list = useQuery({
    queryKey: ["simulations", getState().apiBase],
    queryFn: () => call<SimulationInfo[]>("/v1/simulation"),
    refetchInterval: (query) => (query.state.data?.some((s) => isActive(s.status.state)) ? 2000 : 10000),
  });

  const create = async () => {
    if (preview.error || !preview.body) return;
    setCreateError(null);
    setState({ running: true });
    try {
      const created = await call<{ simulation_id: string }>("/v1/simulation", { body: preview.body });
      setState((prev) => ({ simulation: { ...prev.simulation, selectedId: created.simulation_id, frameIndex: 0, playing: true, agentFeatures: null, edgeFeatures: null } }));
      void queryClient.invalidateQueries({ queryKey: ["simulations"] });
    } catch (error) {
      setCreateError((error as Error).message);
    } finally {
      setState({ running: false });
    }
  };

  return (
    <div className="sim-panel">
      <div className="run-bar">
        <div className="run-row">
          <button type="button" className="btn btn-primary" onClick={create} disabled={running || !!preview.error}>
            Start simulation
          </button>
          <button type="button" className={`btn btn-ghost btn-small${showJson ? " is-active" : ""}`} onClick={() => setShowJson((v) => !v)}>
            Scenario JSON
          </button>
        </div>
        {preview.error && <div className="hint-text">{preview.error}</div>}
        {createError && <div className="error-text">{createError}</div>}
        {showJson && (
          <div className="json-editor">
            <textarea value={previewText} spellCheck={false} rows={14} onChange={(e) => setState((prev) => ({ raw: { ...prev.raw, simulation: e.target.value } }))} aria-label="Scenario JSON" />
            <div className="json-editor-actions">
              {raw !== null ? <span className="pill pill-warn">edited by hand</span> : <span className="muted">Full scenario schema: fleets, zones, traffic model, output.</span>}
              <span className="spacer" />
              <button type="button" className="btn btn-ghost btn-small" onClick={() => setState((prev) => ({ raw: { ...prev.raw, simulation: null } }))}>
                Reset
              </button>
            </div>
          </div>
        )}
      </div>
      <div className="sim-list">
        <div className="points-head">
          <span className="section-title">Simulations</span>
          <span className="muted">{list.data?.length ?? 0} in this API process</span>
        </div>
        {list.data?.length === 0 && <p className="muted">None yet. Start one above; it runs in the API process and streams frames here.</p>}
        {list.data?.map((sim) => (
          <button
            key={sim.simulation_id}
            type="button"
            className={`sim-item${sim.simulation_id === selectedId ? " is-active" : ""}`}
            onClick={() => setState((prev) => ({ simulation: { ...prev.simulation, selectedId: sim.simulation_id, frameIndex: 0, playing: isActive(sim.status.state), agentFeatures: null, edgeFeatures: null } }))}
          >
            <span className={`sim-state sim-state-${sim.status.state}`}>{sim.status.state}</span>
            <span className="sim-id">{sim.simulation_id}</span>
            <span className="muted">
              {fmtDuration(sim.status.sim_time_s)} / {fmtDuration(sim.status.duration_s)}
            </span>
          </button>
        ))}
      </div>
      {selectedId && <SimulationDetail simulationId={selectedId} />}
    </div>
  );
}

function SimulationDetail({ simulationId }: { simulationId: string }) {
  const queryClient = useQueryClient();
  const apiBase = useStore((s) => s.apiBase);
  const frameIndex = useStore((s) => s.simulation.frameIndex);
  const playing = useStore((s) => s.simulation.playing);
  const showEdges = useStore((s) => s.simulation.showEdges);
  const [speedFactor, setSpeedFactor] = useState(1);
  const [maxAgents, setMaxAgents] = useState(3000);
  const [fps, setFps] = useState(4);
  const [controlError, setControlError] = useState<string | null>(null);
  const [zoneBusy, setZoneBusy] = useState(false);
  const framesRef = useRef<SimulationFramesResponse["frames"]>([]);
  const [frameCount, setFrameCount] = useState(0);
  const lastLoadedT = useRef(-1);

  const info = useQuery({
    queryKey: ["simulation", apiBase, simulationId],
    queryFn: () => call<SimulationInfo>(`/v1/simulation/${encodeURIComponent(simulationId)}`),
    refetchInterval: (query) => (isActive(query.state.data?.status.state) ? 1000 : false),
  });
  const status = info.data?.status;
  const active = isActive(status?.state);

  // Incrementally load frames while running, all at once when finished.
  useEffect(() => {
    framesRef.current = [];
    lastLoadedT.current = -1;
    setFrameCount(0);
  }, [simulationId]);

  const framesAvailable = status?.frames_available ?? 0;
  useEffect(() => {
    if (!status || framesAvailable === 0 || framesAvailable === framesRef.current.length) return;
    let cancelled = false;
    const start = lastLoadedT.current + 1e-6;
    void call<SimulationFramesResponse>(`/v1/simulation/${encodeURIComponent(simulationId)}/frames?start_s=${start}&max_agents=${maxAgents}`, { tag: "frames" })
      .then((data) => {
        if (cancelled) return;
        const fresh = data.frames.filter((f) => f.t > lastLoadedT.current);
        if (fresh.length) {
          framesRef.current = [...framesRef.current, ...fresh];
          lastLoadedT.current = fresh[fresh.length - 1].t;
          setFrameCount(framesRef.current.length);
        }
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [framesAvailable, simulationId]);

  // Congestion overlay: bins while running, summary when complete.
  const edgeBins = status?.edge_bins_available ?? 0;
  const finished = status?.state === "completed";
  useEffect(() => {
    if (!status || (edgeBins === 0 && !finished)) return;
    let cancelled = false;
    const path = finished
      ? `/v1/simulation/${encodeURIComponent(simulationId)}/edges?mode=summary&min_traversals=2`
      : `/v1/simulation/${encodeURIComponent(simulationId)}/edges?mode=bins&min_occupancy=0.05`;
    void call<FeatureCollection>(path, { tag: "edges" })
      .then((collection) => {
        if (cancelled) return;
        setState((prev) => ({ simulation: { ...prev.simulation, edgeFeatures: styleEdges(collection) } }));
        if (finished && !framesRef.current.length) requestFit();
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [edgeBins, finished, simulationId]);

  // Playback timer.
  useEffect(() => {
    if (!playing) return;
    const timer = window.setInterval(() => {
      const frames = framesRef.current;
      if (!frames.length) return;
      setState((prev) => {
        const next = prev.simulation.frameIndex + 1;
        if (next >= frames.length) {
          // Follow the live edge while running, loop when finished.
          return isActive(status?.state) ? { simulation: { ...prev.simulation, frameIndex: frames.length - 1 } } : { simulation: { ...prev.simulation, frameIndex: 0 } };
        }
        return { simulation: { ...prev.simulation, frameIndex: next } };
      });
    }, 1000 / fps);
    return () => window.clearInterval(timer);
  }, [playing, fps, status?.state]);

  // Render the current frame.
  const fleetIds = useMemo(() => status?.fleets.map((f) => f.fleet_id) ?? [], [status?.fleets]);
  useEffect(() => {
    const frame = framesRef.current[Math.min(frameIndex, framesRef.current.length - 1)];
    if (!frame) return;
    const features: Feature[] = new Array(frame.agents.id.length);
    for (let i = 0; i < frame.agents.id.length; i++) {
      const fleetIndex = Math.max(0, fleetIds.indexOf(frame.agents.fleet[i]));
      features[i] = {
        type: "Feature",
        id: frame.agents.id[i],
        geometry: { type: "Point", coordinates: [frame.agents.lon[i], frame.agents.lat[i]] },
        properties: { agent_id: frame.agents.id[i], fleet: frame.agents.fleet[i], speed_mps: frame.agents.speed_mps[i], edge: frame.agents.edge[i], _color: FLEET_COLORS[fleetIndex % FLEET_COLORS.length] },
      };
    }
    setState((prev) => ({ simulation: { ...prev.simulation, agentFeatures: { type: "FeatureCollection", features } } }));
  }, [frameIndex, frameCount, fleetIds]);

  const control = async (command: unknown) => {
    setControlError(null);
    try {
      await call(`/v1/simulation/${encodeURIComponent(simulationId)}/control`, { body: command, tag: "control" });
      void queryClient.invalidateQueries({ queryKey: ["simulation", apiBase, simulationId] });
    } catch (error) {
      setControlError((error as Error).message);
    }
  };

  const remove = async () => {
    try {
      await call(`/v1/simulation/${encodeURIComponent(simulationId)}`, { method: "DELETE" });
      setState((prev) => ({ simulation: { ...prev.simulation, selectedId: null, agentFeatures: null, edgeFeatures: null, playing: false } }));
      void queryClient.invalidateQueries({ queryKey: ["simulations"] });
    } catch (error) {
      setControlError((error as Error).message);
    }
  };

  const addZoneFromMap = async () => {
    const corners = getState().points["simulation:zone"] ?? [];
    if (corners.length < 3) {
      setControlError("Place at least 3 zone corners on the map first (Zone corners slot).");
      return;
    }
    setZoneBusy(true);
    await control({
      add_zone: {
        zone: {
          zone_id: `zone_${Date.now().toString(36)}`,
          label: "Console closure",
          polygon: corners.map((p) => [p.lon, p.lat]),
          effect: { kind: "no_access", modes: [] },
        },
      },
    });
    setZoneBusy(false);
  };

  const frame = framesRef.current[Math.min(frameIndex, framesRef.current.length - 1)];
  const temporalUrl = joinUrl(apiBase || "", `/v1/simulation/${encodeURIComponent(simulationId)}/temporal?max_agents=${maxAgents}`);

  return (
    <div className="sim-detail">
      <div className="points-head">
        <span className="section-title">{simulationId}</span>
        {status && <span className={`sim-state sim-state-${status.state}`}>{status.state}</span>}
      </div>
      {info.data?.error && <p className="error-text">{info.data.error}</p>}
      {status && (
        <>
          <div className="sim-progress">
            <div className="sim-progress-bar" style={{ width: `${Math.min(100, (status.sim_time_s / Math.max(1, status.duration_s)) * 100)}%` }} />
          </div>
          <dl className="kv kv-figures kv-compact">
            <div>
              <dt>sim time</dt>
              <dd>
                {fmtDuration(status.sim_time_s)} / {fmtDuration(status.duration_s)}
              </dd>
            </div>
            <div>
              <dt>agents</dt>
              <dd>
                {fmtNumber(status.agents_active)} active · {fmtNumber(status.agents_arrived)} arrived · {fmtNumber(status.agents_pending)} pending
                {status.agents_failed ? ` · ${fmtNumber(status.agents_failed)} failed` : ""}
              </dd>
            </div>
            <div>
              <dt>wall time</dt>
              <dd>{fmtMs(status.wall_time_ms)}</dd>
            </div>
            <div>
              <dt>frames</dt>
              <dd>
                {frameCount} loaded of {status.frames_available} (every {status.frame_interval_s} s)
              </dd>
            </div>
            {status.message && (
              <div>
                <dt>message</dt>
                <dd>{status.message}</dd>
              </div>
            )}
          </dl>
          {status.fleets.length > 0 && (
            <ul className="fleet-list">
              {status.fleets.map((fleet, i) => (
                <li key={fleet.fleet_id}>
                  <span className="slot-swatch" style={{ background: FLEET_COLORS[i % FLEET_COLORS.length] }} />
                  {fleet.fleet_id} <span className="muted">{fleet.profile_id}</span>
                  <span className="muted">
                    {fleet.arrived}/{fleet.requested} arrived · {fleet.reroutes} reroutes
                  </span>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
      <div className="sim-controls">
        {active && status?.state !== "paused" && (
          <button type="button" className="btn btn-small" onClick={() => control("pause")}>
            Pause
          </button>
        )}
        {status?.state === "paused" && (
          <button type="button" className="btn btn-small" onClick={() => control("resume")}>
            Resume
          </button>
        )}
        {active && (
          <button type="button" className="btn btn-ghost btn-small" onClick={() => control("cancel")}>
            Cancel
          </button>
        )}
        {active && (
          <>
            <input type="number" step={0.1} min={0.05} max={3} value={speedFactor} onChange={(e) => setSpeedFactor(Number(e.target.value))} aria-label="Global speed factor" title="Global speed factor (weather)" />
            <button type="button" className="btn btn-small" onClick={() => control({ set_global_speed_factor: { factor: speedFactor } })}>
              Set speed factor
            </button>
            <button type="button" className="btn btn-small" onClick={addZoneFromMap} disabled={zoneBusy}>
              Close zone from map
            </button>
          </>
        )}
        <button type="button" className="btn btn-ghost btn-small" onClick={remove}>
          Delete
        </button>
      </div>
      {controlError && <p className="error-text">{controlError}</p>}
      <div className="sim-playback">
        <button type="button" className="btn btn-small" onClick={() => setState((prev) => ({ simulation: { ...prev.simulation, playing: !prev.simulation.playing } }))} disabled={frameCount === 0}>
          {playing ? "Pause playback" : "Play"}
        </button>
        <input
          type="range"
          min={0}
          max={Math.max(0, frameCount - 1)}
          value={Math.min(frameIndex, Math.max(0, frameCount - 1))}
          onChange={(e) => setState((prev) => ({ simulation: { ...prev.simulation, frameIndex: Number(e.target.value), playing: false } }))}
          aria-label="Frame"
        />
        <span className="muted">{frame ? `${fmtDuration(frame.t)} · ${fmtNumber(frame.active_total)} active` : "no frames yet"}</span>
      </div>
      <div className="sim-options">
        <label className="field-inline">
          <span>fps</span>
          <input type="number" min={1} max={30} value={fps} onChange={(e) => setFps(Number(e.target.value))} />
        </label>
        <label className="field-inline">
          <span>agents/frame</span>
          <input type="number" min={100} max={50000} step={500} value={maxAgents} onChange={(e) => setMaxAgents(Number(e.target.value))} />
        </label>
        <label className="field-check">
          <input type="checkbox" checked={showEdges} onChange={(e) => setState((prev) => ({ simulation: { ...prev.simulation, showEdges: e.target.checked } }))} />
          <span>congestion overlay</span>
        </label>
        <button type="button" className="btn btn-ghost btn-small" onClick={requestFit}>
          Zoom to
        </button>
        <a className="btn btn-ghost btn-small" href={temporalUrl} download={`${simulationId}.temporal.geojson`} target="_blank" rel="noreferrer">
          Temporal GeoJSON
        </a>
      </div>
    </div>
  );
}

function styleEdges(collection: FeatureCollection): FeatureCollection {
  let max = 0;
  const value = (f: Feature) => {
    const p = f.properties;
    if (typeof p.congestion_level === "number") return p.congestion_level;
    if (typeof p.congestion_share === "number") return p.congestion_share;
    if (typeof p.traversals === "number") return p.traversals;
    return 0;
  };
  for (const f of collection.features) max = Math.max(max, value(f));
  for (const f of collection.features) {
    const t = max > 0 ? value(f) / max : 0;
    f.properties._color = heatRamp(Math.sqrt(t));
    f.properties._width = 1.5 + 4 * t;
    f.properties._kind = "edges";
  }
  return collection;
}
