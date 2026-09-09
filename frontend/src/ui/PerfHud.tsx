import { useMemo, useState } from "react";
import type { Timing } from "../api/client";
import { downloadJson, downloadText, timestampSlug, toCsv } from "../geo/export";
import { benchmark } from "../state/run";
import { setState, useStore } from "../state/store";
import { fmtBytes, fmtMs, percentile } from "./format";

/**
 * Bottom-right performance readout: the last request at a glance, a sparkline
 * of recent wall times, and an expandable log with per-endpoint percentiles,
 * a repeat benchmark, and CSV/JSON export.
 */
export function PerfHud() {
  const timings = useStore((s) => s.timings);
  const open = useStore((s) => s.hudOpen);
  const running = useStore((s) => s.running);
  const hasResult = useStore((s) => s.result !== null && !s.result.error);
  const last = timings[timings.length - 1];
  const [runs, setRuns] = useState(20);
  const [concurrency, setConcurrency] = useState(1);
  const [progress, setProgress] = useState<number | null>(null);
  const [filter, setFilter] = useState<string>("all");

  const stats = useMemo(() => {
    const groups = new Map<string, Timing[]>();
    for (const t of timings) {
      const key = t.path.split("?")[0];
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key)!.push(t);
    }
    return Array.from(groups.entries()).map(([path, list]) => {
      const ok = list.filter((t) => t.ok).map((t) => t.totalMs);
      return {
        path,
        count: list.length,
        errors: list.length - ok.length,
        p50: percentile(ok, 50),
        p95: percentile(ok, 95),
        min: ok.length ? Math.min(...ok) : NaN,
        max: ok.length ? Math.max(...ok) : NaN,
        mean: ok.length ? ok.reduce((a, b) => a + b, 0) / ok.length : NaN,
        bytes: list.reduce((a, t) => a + t.responseBytes, 0),
      };
    });
  }, [timings]);

  const visible = useMemo(() => {
    const list = filter === "all" ? timings : timings.filter((t) => t.path.split("?")[0] === filter);
    return list.slice(-200).reverse();
  }, [timings, filter]);

  const runBenchmark = async () => {
    setProgress(0);
    try {
      await benchmark(runs, concurrency, setProgress);
    } finally {
      setProgress(null);
    }
  };

  const exportCsv = () => {
    const rows = timings.map((t) => ({
      id: t.id,
      at: t.at,
      tool: t.tool,
      tag: t.tag ?? "",
      method: t.method,
      path: t.path,
      status: t.status,
      ok: t.ok,
      ttfb_ms: round(t.ttfbMs),
      download_ms: round(t.downloadMs),
      parse_ms: round(t.parseMs),
      total_ms: round(t.totalMs),
      request_bytes: t.requestBytes,
      response_bytes: t.responseBytes,
      features: t.featureCount ?? "",
      profile_cache: t.serverHints["profile-cache"] ?? "",
      profile_prepare_ms: t.serverHints["profile-prepare-ms"] ?? "",
      error: t.error ?? "",
    }));
    downloadText(`netweevil_timings_${timestampSlug()}.csv`, toCsv({ columns: Object.keys(rows[0] ?? { id: 0 }), rows }), "text/csv");
  };

  return (
    <aside className={`hud${open ? " is-open" : ""}`} aria-label="Performance">
      <button type="button" className="hud-strip" onClick={() => setState({ hudOpen: !open })} aria-expanded={open}>
        <Sparkline values={timings.slice(-40).map((t) => (t.ok ? t.totalMs : -1))} />
        {last ? (
          <span className="hud-last">
            <span className={`hud-status ${last.ok ? "ok" : "err"}`}>{last.status || "–"}</span>
            <span className="hud-path">{last.tool}</span>
            <strong>{fmtMs(last.totalMs)}</strong>
            <span className="muted">{fmtBytes(last.responseBytes)}</span>
            {last.featureCount !== null && <span className="muted">{last.featureCount} feat.</span>}
          </span>
        ) : (
          <span className="hud-last muted">No requests yet</span>
        )}
        <span className="hud-count muted">{timings.length}</span>
      </button>
      {open && (
        <div className="hud-body">
          {last && (
            <div className="hud-breakdown">
              <span>
                first byte <strong>{fmtMs(last.ttfbMs)}</strong>
              </span>
              <span>
                download <strong>{fmtMs(last.downloadMs)}</strong>
              </span>
              <span>
                parse <strong>{fmtMs(last.parseMs)}</strong>
              </span>
              <span>
                sent <strong>{fmtBytes(last.requestBytes)}</strong>
              </span>
              {Object.entries(last.serverHints).map(([k, v]) => (
                <span key={k}>
                  {k.replace(/-/g, " ")} <strong>{v}</strong>
                </span>
              ))}
            </div>
          )}
          <div className="hud-bench">
            <span>Repeat last request</span>
            <input type="number" min={1} max={2000} value={runs} onChange={(e) => setRuns(Number(e.target.value))} aria-label="Runs" />
            <span>runs at</span>
            <input type="number" min={1} max={64} value={concurrency} onChange={(e) => setConcurrency(Number(e.target.value))} aria-label="Concurrency" />
            <span>in flight</span>
            <button type="button" className="btn btn-small" onClick={runBenchmark} disabled={running || !hasResult}>
              {progress !== null ? `${progress}/${runs}` : "Benchmark"}
            </button>
          </div>
          {stats.length > 0 && (
            <table className="hud-table">
              <thead>
                <tr>
                  <th>Endpoint</th>
                  <th>n</th>
                  <th>err</th>
                  <th>min</th>
                  <th>p50</th>
                  <th>mean</th>
                  <th>p95</th>
                  <th>max</th>
                  <th>bytes</th>
                </tr>
              </thead>
              <tbody>
                {stats.map((s) => (
                  <tr key={s.path} className={filter === s.path ? "is-active" : ""} onClick={() => setFilter(filter === s.path ? "all" : s.path)}>
                    <td>{s.path.replace("/v1/", "")}</td>
                    <td>{s.count}</td>
                    <td>{s.errors || ""}</td>
                    <td>{fmtMs(s.min)}</td>
                    <td>{fmtMs(s.p50)}</td>
                    <td>{fmtMs(s.mean)}</td>
                    <td>{fmtMs(s.p95)}</td>
                    <td>{fmtMs(s.max)}</td>
                    <td>{fmtBytes(s.bytes)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          <div className="hud-log">
            {visible.map((t) => (
              <div key={t.id} className={`hud-log-row${t.ok ? "" : " is-error"}`} title={t.error ?? t.path}>
                <span className="muted">{t.at.slice(11, 19)}</span>
                <span className={`hud-status ${t.ok ? "ok" : "err"}`}>{t.status || "–"}</span>
                <span className="hud-path">
                  {t.tool}
                  {t.tag ? ` · ${t.tag}` : ""}
                </span>
                <span>{fmtMs(t.totalMs)}</span>
                <span className="muted">{fmtBytes(t.responseBytes)}</span>
              </div>
            ))}
          </div>
          <div className="hud-actions">
            <button type="button" className="btn btn-small" onClick={exportCsv} disabled={!timings.length}>
              Export CSV
            </button>
            <button type="button" className="btn btn-small" onClick={() => downloadJson(`netweevil_timings_${timestampSlug()}.json`, { exported_at: new Date().toISOString(), stats, timings })} disabled={!timings.length}>
              Export JSON
            </button>
            <span className="spacer" />
            <button type="button" className="btn btn-ghost btn-small" onClick={() => setState({ timings: [] })} disabled={!timings.length}>
              Clear
            </button>
          </div>
        </div>
      )}
    </aside>
  );
}

function round(v: number) {
  return Math.round(v * 100) / 100;
}

function Sparkline({ values }: { values: number[] }) {
  const width = 96;
  const height = 22;
  if (values.length < 2) return <svg className="spark" width={width} height={height} aria-hidden="true" />;
  const valid = values.filter((v) => v >= 0);
  const max = Math.max(1, ...valid);
  const step = width / (values.length - 1);
  const points = values.map((v, i) => `${(i * step).toFixed(1)},${v < 0 ? height : (height - 2 - (v / max) * (height - 4)).toFixed(1)}`);
  const lastIndex = values.length - 1;
  const lastValue = values[lastIndex];
  return (
    <svg className="spark" width={width} height={height} aria-hidden="true">
      <polyline points={points.join(" ")} fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round" />
      {lastValue >= 0 && <circle cx={lastIndex * step} cy={height - 2 - (lastValue / max) * (height - 4)} r="2.5" fill="currentColor" />}
    </svg>
  );
}
