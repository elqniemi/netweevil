import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ApiError, apiRequest } from "../api/client";
import type { ServiceInfo } from "../api/types";
import { getPoints, getState, pushTiming, setPoints, useStore, type InputPoint } from "../state/store";
import { fmtMs } from "./format";

/** `GET /v1/traffic`: the loaded historical set, cached engines and live state. */
export interface TrafficStatus {
  enabled: boolean;
  traffic_id?: string;
  timezone?: string;
  bucket_minutes?: number;
  pattern_count?: number;
  feature_count?: number;
  matched_edge_count?: number;
  distinct_bucket_metrics?: number;
  current_bucket?: { index: number; label: string; metric_bucket: number };
  live_version?: number;
  live_entry_count?: number;
  live_active_entry_count?: number;
  last_swap?: { version: number; update_count: number; entry_count: number; swap_ms: number; published_at: string; unmatched_feature_ids?: number[] };
  profiles?: { profile_id: string; policy: unknown; cached_engines: number }[];
}

const WAY_SLOT = "traffic_ways";
/** Furthest a click may be from the road it selects. */
const WAY_SNAP_M = 250;

async function call<T>(path: string, options: { method?: "GET" | "POST" | "DELETE"; body?: unknown } = {}): Promise<T> {
  try {
    const response = await apiRequest<T>(getState().apiBase, path, { tool: "traffic", ...options });
    pushTiming(response.timing);
    return response.data;
  } catch (error) {
    if (error instanceof ApiError) pushTiming(error.timing);
    throw error;
  }
}

export function useTrafficStatus(enabled = true) {
  const apiBase = useStore((s) => s.apiBase);
  return useQuery({
    queryKey: ["traffic", apiBase],
    queryFn: () => call<TrafficStatus>("/v1/traffic"),
    enabled,
    staleTime: 15_000,
    refetchInterval: 30_000,
    retry: false,
  });
}

/** One line under the profile picker of traffic-aware tools. */
export function TrafficBadge() {
  const status = useTrafficStatus();
  const data = status.data;
  if (!data?.enabled) return null;
  const live = data.live_active_entry_count ?? 0;
  const parts = [
    data.traffic_id ? `${data.traffic_id}` : "no historical speeds",
    data.current_bucket ? `now ${data.current_bucket.label}` : null,
    `${live} live ${live === 1 ? "update" : "updates"}`,
  ].filter(Boolean);
  return (
    <p className="traffic-badge" title="Traffic applies to requests with a departure time. Set Traffic in the Time options.">
      <span className="traffic-dot" aria-hidden="true" />
      Traffic: {parts.join(" · ")}
    </p>
  );
}

/**
 * Live traffic tool: resolves clicked points to OSM ways, shows the traffic
 * status, and clears every live entry. Posting goes through the Run button.
 */
export function TrafficPanel({ service, open }: { service: ServiceInfo | null; open: boolean }) {
  const queryClient = useQueryClient();
  const status = useTrafficStatus();
  const result = useStore((s) => (s.result?.tool === "traffic" ? s.result : null));
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  useWayResolver(service, setMessage);

  // A posted batch changes the live state: refresh the status.
  useEffect(() => {
    if (result) void queryClient.invalidateQueries({ queryKey: ["traffic"] });
  }, [result, queryClient]);

  const clearAll = async () => {
    setBusy(true);
    setMessage(null);
    try {
      const report = await call<{ version: number; swap_ms: number }>("/v1/traffic/live?wait=true", { method: "DELETE" });
      setMessage(`Cleared every live entry (version ${report.version}, ${fmtMs(report.swap_ms)}).`);
      void queryClient.invalidateQueries({ queryKey: ["traffic"] });
    } catch (error) {
      setMessage((error as Error).message);
    } finally {
      setBusy(false);
    }
  };

  if (!open) return message ? <p className="hint-text">{message}</p> : null;
  const data = status.data;
  return (
    <div className="traffic-panel">
      <div className="points-head">
        <span className="section-title">Traffic status</span>
        <button type="button" className="btn btn-ghost btn-small" onClick={() => void status.refetch()} disabled={status.isFetching}>
          {status.isFetching ? "Refreshing…" : "Refresh"}
        </button>
        <button type="button" className="btn btn-small" onClick={clearAll} disabled={busy || !data?.enabled}>
          {busy ? "Clearing…" : "Clear all live traffic"}
        </button>
      </div>
      {status.error && <p className="error-text">{(status.error as Error).message}</p>}
      {data && !data.enabled && <p className="muted">Traffic is off: the API has no car, HGV or transit-vehicle profile loaded.</p>}
      {data?.enabled && (
        <dl className="kv kv-compact">
          <dt>Historical set</dt>
          <dd>{data.traffic_id ? `${data.traffic_id} (${data.timezone}, ${data.bucket_minutes} min buckets)` : "none; live updates apply to profile speeds"}</dd>
          {data.current_bucket && (
            <>
              <dt>Current bucket</dt>
              <dd>{data.current_bucket.label}</dd>
            </>
          )}
          {data.traffic_id && (
            <>
              <dt>Coverage</dt>
              <dd>
                {data.feature_count?.toLocaleString("en-US")} features, {data.matched_edge_count?.toLocaleString("en-US")} edges, {data.distinct_bucket_metrics} distinct metrics
              </dd>
            </>
          )}
          <dt>Live entries</dt>
          <dd>
            {data.live_active_entry_count ?? 0} active of {data.live_entry_count ?? 0} (version {data.live_version ?? 0})
          </dd>
          {data.last_swap && (
            <>
              <dt>Last update</dt>
              <dd>
                {data.last_swap.update_count} updates in {fmtMs(data.last_swap.swap_ms)} at {new Date(data.last_swap.published_at).toLocaleTimeString()}
                {data.last_swap.unmatched_feature_ids?.length ? ` · unmatched ways ${data.last_swap.unmatched_feature_ids.join(", ")}` : ""}
              </dd>
            </>
          )}
          {!!data.profiles?.length && (
            <>
              <dt>Profiles</dt>
              <dd>{data.profiles.map((p) => `${p.profile_id} (${p.cached_engines} cached)`).join(", ")}</dd>
            </>
          )}
        </dl>
      )}
      {message && <p className="hint-text">{message}</p>}
    </div>
  );
}

interface LocatedPoint {
  point_id: string;
  nearest: { distance_m: number; lon: number; lat: number; edges: { way_id: number; name: string | null }[] }[];
}

/**
 * Looks up the OSM way nearest to every clicked road point that has none yet
 * (or was dragged since), and moves the marker onto that road.
 */
function useWayResolver(service: ServiceInfo | null, report: (message: string | null) => void) {
  const points = useStore((s) => s.points[WAY_SLOT]);
  const profileId = useStore((s) => s.profileId);
  const resolvedAt = useRef(new Map<string, string>());
  const inFlight = useRef(new Set<string>());
  useEffect(() => {
    if (!service || !points?.length) return;
    const key = (p: InputPoint) => `${p.lon.toFixed(7)},${p.lat.toFixed(7)}`;
    const pending = points.filter((p) => {
      if (inFlight.current.has(p.id)) return false;
      if (p.way_id === undefined) return true;
      const at = resolvedAt.current.get(p.id);
      return at !== undefined && at !== key(p);
    });
    if (!pending.length) return;
    for (const p of pending) inFlight.current.add(p.id);
    const body = {
      profile_id: profileId ?? service.default_profile_id,
      request: { points: pending.map((p) => ({ id: p.id, lon: p.lon, lat: p.lat })), snap: { max_distance_m: WAY_SNAP_M }, count: 1 },
    };
    void call<{ result: LocatedPoint[] }>("/v1/locate", { body })
      .then((response) => {
        const byId = new Map(response.result.map((located) => [located.point_id, located]));
        const missing: string[] = [];
        const next = getPoints(WAY_SLOT).flatMap((p) => {
          if (!pending.some((q) => q.id === p.id)) return [p];
          const nearest = byId.get(p.id)?.nearest[0];
          const edge = nearest?.edges[0];
          if (!nearest || !edge) {
            missing.push(p.id);
            resolvedAt.current.delete(p.id);
            return [];
          }
          const moved: InputPoint = { ...p, lon: nearest.lon, lat: nearest.lat, way_id: edge.way_id, way_name: edge.name };
          delete moved.z;
          resolvedAt.current.set(p.id, key(moved));
          return [moved];
        });
        setPoints(WAY_SLOT, next);
        report(missing.length ? `No road within ${WAY_SNAP_M} m of ${missing.length === 1 ? "that click" : `${missing.length} clicks`}.` : null);
      })
      .catch((error) => report(`Road lookup failed: ${(error as Error).message}`))
      .finally(() => {
        for (const p of pending) inFlight.current.delete(p.id);
      });
  }, [points, service, profileId, report]);
}
