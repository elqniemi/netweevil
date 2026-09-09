import { useEffect } from "react";
import { MapView } from "./map/MapView";
import { useService } from "./state/service";
import { getState, setState, useStore } from "./state/store";
import { Header } from "./ui/Header";
import { PerfHud } from "./ui/PerfHud";
import { Rail } from "./ui/Rail";
import { ResultsPanel } from "./ui/ResultsPanel";
import { ToolPanel } from "./ui/ToolPanel";
import { fmtValue } from "./ui/format";

export function App() {
  const service = useService();
  const hover = useStore((s) => s.hoverInfo);
  const info = service.data ?? null;

  // Keep the profile/feed selection valid for the connected API.
  useEffect(() => {
    if (!info) return;
    const state = { profileId: undefined as string | undefined, feedId: undefined as string | undefined };
    const profiles = info.loaded_profiles.map((p) => p.profile_id);
    const current = getState();
    if (!current.profileId || !profiles.includes(current.profileId)) state.profileId = info.default_profile_id;
    const feeds = info.loaded_transit_feeds.map((f) => f.feed_id);
    if (!current.feedId || !feeds.includes(current.feedId)) state.feedId = feeds[0];
    if (state.profileId !== undefined || state.feedId !== undefined) {
      setState({ ...(state.profileId !== undefined ? { profileId: state.profileId } : {}), ...(state.feedId !== undefined ? { feedId: state.feedId } : {}) });
    }
  }, [info]);

  return (
    <div className="app">
      <Header service={info} error={service.error ? (service.error as Error).message : null} loading={service.isLoading} />
      <div className="body">
        <Rail />
        <div className="sidebar">
          <ToolPanel service={info} />
          <ResultsPanel />
        </div>
        <main className="map-area">
          <MapView service={info} />
          {hover && <HoverCard x={hover.x} y={hover.y} props={hover.props} />}
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
  return (
    <div className="hover-card" style={{ left: x + 14, top: y + 14 }}>
      {kind && <div className="hover-kind">{kind.replace(/_/g, " ")}</div>}
      <dl>
        {entries.map(([k, v]) => (
          <div key={k}>
            <dt>{k.replace(/_/g, " ")}</dt>
            <dd>{fmtValue(k, v)}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
