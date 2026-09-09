import { useMemo, useState } from "react";
import { downloadJson, downloadText, timestampSlug, toCsv, type TableData } from "../geo/export";
import { fetchServerGeojson, mapGeojson } from "../state/run";
import { requestFit, setState, useStore, type ResultRecord } from "../state/store";
import { TOOL_BY_ID } from "../tools/registry";
import { fmtBytes, fmtMs, fmtValue } from "./format";

const SUMMARY_KEYS = [
  "outcome",
  "status",
  "total_distance_m",
  "total_travel_time_s",
  "total_generalized_cost",
  "network_distance_m",
  "network_travel_time_s",
  "waiting_time_s",
  "departure_time",
  "arrival_time",
  "segment_count",
  "violation_count",
  "fallback_used",
  "pair_count",
  "cell_count",
  "row_count",
  "origin_count",
  "destination_count",
  "processed_origin_count",
  "skipped_origin_count",
  "succeeded_count",
  "failed_count",
  "ignored_count",
  "threshold_count",
  "frame_count",
  "routed_pair_count",
  "unreachable_pair_count",
  "requested_pair_count",
  "routed_demand",
  "optimization_method",
  "transit_time_s",
  "access_egress_time_s",
  "transfer_time_s",
  "wait_time_s",
  "boarding_count",
  "max_travel_time_s",
  "language",
];

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Collects key figures from the top of the result and its summary. */
function summaryEntries(response: unknown): [string, unknown][] {
  const root = isPlainObject(response) ? response : {};
  const result = isPlainObject(root.result) ? root.result : isPlainObject(root.metadata) ? root.metadata : root;
  const candidates: Record<string, unknown> = {};
  const merge = (obj: unknown, prefix = "") => {
    if (!isPlainObject(obj)) return;
    for (const [k, v] of Object.entries(obj)) {
      if (typeof v !== "object" || v === null) candidates[prefix + k] = v;
    }
  };
  merge(result);
  merge(result.summary);
  if (isPlainObject(result.route)) {
    merge(result.route);
    merge(result.route.summary);
  }
  merge(result.time_context);
  const entries: [string, unknown][] = [];
  for (const key of SUMMARY_KEYS) if (key in candidates) entries.push([key, candidates[key]]);
  if (isPlainObject(result.summary) && isPlainObject(result.summary.components)) {
    for (const [k, v] of Object.entries(result.summary.components)) entries.push([`component ${k}`, v]);
  }
  if (Array.isArray(result.alternatives)) entries.push(["alternatives", result.alternatives.length]);
  if (Array.isArray(result.legs)) entries.push(["legs", result.legs.length]);
  if (Array.isArray(result.maneuvers)) entries.push(["maneuvers", result.maneuvers.length]);
  if (Array.isArray(result.features)) entries.push(["features", result.features.length]);
  if (Array.isArray(result.edges)) entries.push(["edges", result.edges.length]);
  if (Array.isArray(result.stops)) entries.push(["stops", result.stops.length]);
  if (Array.isArray(result.warnings) && result.warnings.length) entries.push(["warnings", result.warnings.join("; ")]);
  if (Array.isArray(result.diagnostics) && result.diagnostics.length) entries.push(["diagnostics", result.diagnostics.map((d) => (typeof d === "string" ? d : JSON.stringify(d))).join("; ")]);
  return entries;
}

interface TableCandidate {
  path: string;
  rows: Record<string, unknown>[];
}

/** Finds arrays of objects anywhere in the response (depth-limited). */
function findTables(response: unknown): TableCandidate[] {
  const out: TableCandidate[] = [];
  const walk = (value: unknown, path: string, depth: number) => {
    if (depth > 5 || value === null || typeof value !== "object") return;
    if (Array.isArray(value)) {
      if (value.length && value.every(isPlainObject)) {
        out.push({ path, rows: value as Record<string, unknown>[] });
      }
      // Also descend into nested arrays for things like locate candidates.
      value.slice(0, 50).forEach((item, i) => walk(item, `${path}[${i}]`, depth + 1));
      return;
    }
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) walk(v, path ? `${path}.${k}` : k, depth + 1);
  };
  walk(response, "", 0);
  // Prefer larger tables and shallower paths.
  return out.sort((a, b) => b.rows.length - a.rows.length || a.path.length - b.path.length).slice(0, 25);
}

function flattenRow(row: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(row)) {
    if (v === null || typeof v !== "object") out[k] = v;
    else if (Array.isArray(v)) {
      if (v.length && v.every((x) => typeof x !== "object")) out[k] = v.join(", ");
      else out[k] = `${v.length} items`;
    } else {
      const obj = v as Record<string, unknown>;
      let scalarCount = 0;
      for (const [ik, iv] of Object.entries(obj)) {
        if (iv === null || typeof iv !== "object") {
          out[`${k}.${ik}`] = iv;
          scalarCount += 1;
          if (scalarCount > 12) break;
        }
      }
    }
  }
  return out;
}

function toTable(candidate: TableCandidate): TableData {
  const rows = candidate.rows.map(flattenRow);
  const columns: string[] = [];
  const seen = new Set<string>();
  for (const row of rows.slice(0, 200)) for (const key of Object.keys(row)) if (!seen.has(key)) (seen.add(key), columns.push(key));
  return { columns, rows };
}

export function ResultsPanel() {
  const result = useStore((s) => s.result);
  const tab = useStore((s) => s.resultsTab);
  if (!result) {
    return (
      <section className="results results-empty">
        <p>Run an analysis to see its summary, rows and raw response here. Results also draw on the map.</p>
      </section>
    );
  }
  return (
    <section className="results">
      <ResultHeader result={result} />
      <div className="tabs" role="tablist">
        {(["summary", "table", "json"] as const).map((t) => (
          <button key={t} type="button" role="tab" aria-selected={tab === t} className={tab === t ? "is-active" : ""} onClick={() => setState({ resultsTab: t })}>
            {t === "summary" ? "Summary" : t === "table" ? "Rows" : "JSON"}
          </button>
        ))}
      </div>
      {tab === "summary" && <SummaryTab result={result} />}
      {tab === "table" && <TableTab result={result} />}
      {tab === "json" && <JsonTab result={result} />}
    </section>
  );
}

function ResultHeader({ result }: { result: ResultRecord }) {
  const tool = TOOL_BY_ID[result.tool];
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const slug = `${result.tool}_${timestampSlug()}`;

  const exportServerGeojson = async () => {
    setBusy("server");
    setMessage(null);
    try {
      const out = await fetchServerGeojson();
      if (out) {
        downloadText(`${slug}.server.geojson`, out.text, "application/geo+json");
        setMessage(`Server GeoJSON in ${fmtMs(out.timing.totalMs)}`);
      }
    } catch (error) {
      setMessage((error as Error).message);
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="results-head">
      <div className="results-title">
        <strong>{tool.label}</strong>
        {result.error ? (
          <span className="pill pill-error">{result.timing.status ? `HTTP ${result.timing.status}` : "not sent"}</span>
        ) : (
          <span className="pill pill-ok">{result.timing.status}</span>
        )}
        {!result.error && (
          <span className="muted">
            {fmtMs(result.timing.totalMs)} · {fmtBytes(result.timing.responseBytes)} · {result.features.features.length} map features
          </span>
        )}
      </div>
      {result.error && <p className="error-text">{result.error}</p>}
      {!result.error && (
        <div className="export-row">
          <button type="button" className="btn btn-small" onClick={() => downloadJson(`${slug}.json`, result.response)}>
            Response JSON
          </button>
          <button type="button" className="btn btn-small" onClick={() => downloadJson(`${slug}.map.geojson`, mapGeojson())} disabled={!result.features.features.length}>
            Map GeoJSON
          </button>
          {tool.supportsGeojson && (
            <button type="button" className="btn btn-small" onClick={exportServerGeojson} disabled={busy !== null}>
              {busy === "server" ? "Fetching…" : "Server GeoJSON"}
            </button>
          )}
          <button type="button" className="btn btn-ghost btn-small" onClick={() => downloadJson(`${slug}.request.json`, result.requestBody)}>
            Request
          </button>
          <button type="button" className="btn btn-ghost btn-small" onClick={requestFit} disabled={!result.features.features.length}>
            Zoom to
          </button>
          {message && <span className="muted">{message}</span>}
        </div>
      )}
    </div>
  );
}

function SummaryTab({ result }: { result: ResultRecord }) {
  const entries = useMemo(() => summaryEntries(result.response), [result.response]);
  const service = isPlainObject(result.response) && isPlainObject(result.response.service) ? result.response.service : null;
  const hints = Object.entries(result.timing.serverHints);
  return (
    <div className="summary">
      {entries.length === 0 && !result.error && <p className="muted">No headline figures for this response. See rows or JSON.</p>}
      {entries.length > 0 && (
        <dl className="kv kv-figures">
          {entries.map(([k, v]) => (
            <div key={k}>
              <dt>{k.replace(/_/g, " ")}</dt>
              <dd>{fmtValue(k, v)}</dd>
            </div>
          ))}
        </dl>
      )}
      {(service || hints.length > 0) && (
        <details className="service-context" open>
          <summary>Execution context</summary>
          <dl className="kv">
            {service &&
              Object.entries(service).map(([k, v]) => (
                <div key={k}>
                  <dt>{k.replace(/_/g, " ")}</dt>
                  <dd>{String(v)}</dd>
                </div>
              ))}
            {hints.map(([k, v]) => (
              <div key={k}>
                <dt>{k.replace(/-/g, " ")}</dt>
                <dd>{v}</dd>
              </div>
            ))}
          </dl>
        </details>
      )}
    </div>
  );
}

function TableTab({ result }: { result: ResultRecord }) {
  const tables = useMemo(() => findTables(result.response), [result.response]);
  const [selected, setSelected] = useState(0);
  const candidate = tables[Math.min(selected, tables.length - 1)];
  const table = useMemo(() => (candidate ? toTable(candidate) : null), [candidate]);
  if (!table || !candidate) return <p className="muted">No tabular rows in this response.</p>;
  const limit = 300;
  return (
    <div className="table-tab">
      <div className="table-toolbar">
        <select value={selected} onChange={(e) => setSelected(Number(e.target.value))} aria-label="Row set">
          {tables.map((t, i) => (
            <option key={t.path} value={i}>
              {t.path || "root"} ({t.rows.length})
            </option>
          ))}
        </select>
        <button type="button" className="btn btn-small" onClick={() => downloadText(`${result.tool}_${candidate.path.replace(/[^a-z0-9]+/gi, "_")}_${timestampSlug()}.csv`, toCsv(table), "text/csv")}>
          CSV
        </button>
        <span className="muted">
          {table.rows.length > limit ? `showing ${limit} of ${table.rows.length}` : `${table.rows.length} rows`} · {table.columns.length} columns
        </span>
      </div>
      <div className="table-scroll">
        <table>
          <thead>
            <tr>
              {table.columns.map((c) => (
                <th key={c}>{c}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {table.rows.slice(0, limit).map((row, i) => (
              <tr key={i}>
                {table.columns.map((c) => (
                  <td key={c} title={row[c] === undefined ? "" : String(row[c])}>
                    {row[c] === undefined || row[c] === null ? "" : typeof row[c] === "number" ? fmtValue(c, row[c]) : String(row[c])}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function JsonTab({ result }: { result: ResultRecord }) {
  const text = useMemo(() => {
    const value = result.response ?? { error: result.error };
    const full = JSON.stringify(value, null, 2);
    return full.length > 200_000 ? `${full.slice(0, 200_000)}\n… truncated (${fmtBytes(full.length)} total; download the response for the full body)` : full;
  }, [result.response, result.error]);
  return <pre className="json-view">{text}</pre>;
}
