import { useEffect } from "react";
import { MapView } from "./map/MapView";
import { abortRun, runTool, startLiveRunner } from "./state/run";
import { useService } from "./state/service";
import { clearAll, getState, removeVia, setRouteEnd, setState, useStore } from "./state/store";
import { TOOL_BY_ID } from "./tools/registry";
import { Header } from "./ui/Header";
import { Legend } from "./ui/Legend";
import { PerfHud } from "./ui/PerfHud";
import { MapTools } from "./ui/MapTools";
import { Rail } from "./ui/Rail";
import { ResultsPanel } from "./ui/ResultsPanel";
import { SetupPanel } from "./ui/SetupPanel";
import { ToolPanel } from "./ui/ToolPanel";
import { fmtValue } from "./ui/format";
import { transitTimeContext } from "./api/time";
import { ApiError } from "./api/client";

let autoOpenedSetup = false;

export function App() {
  const service = useService();
  const hover = useStore((s) => s.hoverInfo);
  const sidebarOpen = useStore((s) => s.sidebarOpen);
  const info = service.data ?? null;
  // The API answers 503 until a dataset is loaded: open Setup once, automatically.
  const setupNeeded = service.error instanceof ApiError && service.error.status === 503;
  useEffect(() => {
    if (setupNeeded && !autoOpenedSetup) {
      autoOpenedSetup = true;
      setState({ setupOpen: true });
    }
  }, [setupNeeded]);

  // Keep the profile/feed selection valid for the connected API and share the
  // service info with non-React code (request building, live runner).
  useEffect(() => {
    const current = getState();
    if (!info) {
      if (current.service) setState({ service: null });
      return;
    }
    const patch: Partial<typeof current> = { service: info };
    const profiles = info.loaded_profiles.map((p) => p.profile_id);
    if (!current.profileId || !profiles.includes(current.profileId)) patch.profileId = info.default_profile_id;
    const feeds = info.loaded_transit_feeds.map((f) => f.feed_id);
    if (!current.feedId || !feeds.includes(current.feedId)) patch.feedId = feeds[0] ?? null;
    setState(patch);
  }, [info]);

  // Live updating: re-run route tools as their inputs change.
  useEffect(() => startLiveRunner(), []);

  // Keyboard: Enter runs, Escape cancels, Delete removes the active route's last point, Ctrl+Shift+Backspace clears everything.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      const typing = !!target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT" || target.isContentEditable);
      const s = getState();
      if (e.key === "Escape") {
        abortRun();
        setState({ hoverInfo: null, setupOpen: false });
        return;
      }
      if (typing || s.setupOpen) return;
      if (TOOL_BY_ID[s.tool].input === "editor") return;
      if (e.key === "Enter" && s.tool !== "simulation") {
        e.preventDefault();
        void runTool(s.tool, { fit: true });
      } else if ((e.key === "Delete" || e.key === "Backspace") && e.ctrlKey && e.shiftKey) {
        e.preventDefault();
        clearAll();
      } else if (e.key === "Delete" || e.key === "Backspace") {
        if (TOOL_BY_ID[s.tool].input !== "routes") return;
        e.preventDefault();
        const index = Math.min(s.activeRoute, s.routes.length - 1);
        const route = s.routes[index];
        if (route.vias.length) removeVia(index, route.vias.length - 1);
        else if (route.destination) setRouteEnd(index, "destination", null);
        else if (route.origin) setRouteEnd(index, "origin", null);
        setState({ activeEnd: route.destination || route.vias.length ? "destination" : "origin" });
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="app">
      <Header service={info} error={service.error ? (service.error as Error).message : null} loading={service.isLoading} setupNeeded={setupNeeded} />
      <div className="body">
        <Rail />
        <SetupPanel />
        {sidebarOpen && (
          <div className="sidebar">
            <ToolPanel service={info} />
            <ResultsPanel />
          </div>
        )}
        <main className="map-area">
          <MapView service={info} />
          <button
            type="button"
            className={`sidebar-toggle${sidebarOpen ? " is-open" : ""}`}
            onClick={() => setState({ sidebarOpen: !sidebarOpen })}
            title={sidebarOpen ? "Hide panel" : "Show panel"}
            aria-label={sidebarOpen ? "Hide panel" : "Show panel"}
          >
            {sidebarOpen ? "‹" : "›"}
          </button>
          {hover && <HoverCard x={hover.x} y={hover.y} props={hover.props} />}
          <MapTools />
          <Legend />
          <PerfHud />
        </main>
      </div>
    </div>
  );
}

const HIDDEN_PROPS = new Set(["dataset_id", "profile_hash", "feed_id", "agency_timezone", "time_origin_unix_s"]);

function HoverCard({ x, y, props }: { x: number; y: number; props: Record<string, unknown> }) {
  const entries = Object.entries(props)
    .filter(([k, v]) => !k.startsWith("_") && !HIDDEN_PROPS.has(k) && v !== null && v !== undefined && v !== "")
    .slice(0, 12);
  if (!entries.length) return null;
  const kind = typeof props._kind === "string" && props._kind !== "root" ? props._kind : null;
  const run = typeof props._run_label === "string" ? props._run_label : null;
  return (
    <div className="hover-card" style={{ left: x + 14, top: y + 14 }}>
      {(kind || run) && (
        <div className="hover-kind">
          {run && <strong>{run}</strong>}
          {run && kind && " · "}
          {kind && kind.replace(/_/g, " ")}
        </div>
      )}
      <dl>
        {entries.map(([k, v]) => (
          <div key={k}>
            <dt>{k.replace(/_/g, " ")}</dt>
            <dd>{fmtValue(k, v, transitTimeContext(props))}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
