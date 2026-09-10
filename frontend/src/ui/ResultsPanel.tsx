import { useEffect, useMemo, useState } from "react";
import { downloadJson, downloadText, timestampSlug, toCsv, type TableData } from "../geo/export";
import { PALETTE, routeColor } from "../geo/features";
import { exportRequest, exportResponse, fetchServerGeojson, mapGeojson } from "../state/run";
import { requestFit, setState, useStore, type ResultRecord, type RunRecord } from "../state/store";
import { TOOL_BY_ID } from "../tools/registry";
import { DirectionsList } from "./DirectionsList";
import { TransitDirectionsList } from "./TransitDirectionsList";
import { fmtBytes, fmtMs, fmtValue } from "./format";
import { transitTimeContext, type TransitTimeContext } from "../api/time";

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
  "departure_s",
  "arrival_s",
  "catchment_mode",
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

/** Columns of the per-route comparison table, in display order (only those present are shown). */
const RUN_COLUMNS = [
  "outcome",
  "status",
  "total_distance_m",
  "total_travel_time_s",
  "total_generalized_cost",
  "departure_time",
  "arrival_time",
  "departure_s",
  "arrival_s",
  "transit_time_s",
  "boarding_count",
  "segment_count",
  "violation_count",
  "fallback_used",
  "alternatives",
  "legs",
  "maneuvers",
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

/** One comparison row per run: headline figures plus request timing. */
function runRows(runs: RunRecord[]): TableData {
  const rows = runs.map((run, index) => {
    const figures = Object.fromEntries(summaryEntries(run.response));
    const row: Record<string, unknown> = { "#": index + 1, route: run.profile ? `Route ${(run.routeIndex ?? 0) + 1}` : run.label };
    if (run.profile) row.profile = run.profile;
    if (run.error) row.error = run.error;
    for (const key of RUN_COLUMNS) if (key in figures) row[key] = figures[key];
    row.wall_ms = Math.round(run.timing.totalMs * 10) / 10;
    row.bytes = run.timing.responseBytes;
    row.http = run.timing.status;
    return row;
  });
  const columns: string[] = [];
  const seen = new Set<string>();
  for (const row of rows) for (const key of Object.keys(row)) if (!seen.has(key)) (seen.add(key), columns.push(key));
  // Keep a stable order: id columns, then figures, then timing.
  const order = ["#", "route", "profile", "error", ...RUN_COLUMNS, "wall_ms", "bytes", "http"];
  columns.sort((a, b) => order.indexOf(a) - order.indexOf(b));
  return { columns, rows };
}

interface TableCandidate {
  path: string;
  rows: Record<string, unknown>[];
  timeContexts?: (TransitTimeContext | undefined)[];
}

/** Finds arrays of objects anywhere in the response (depth-limited). */
function findTables(response: unknown): TableCandidate[] {
  const out: TableCandidate[] = [];
  const walk = (value: unknown, path: string, depth: number, inherited?: TransitTimeContext) => {
    if (depth > 5 || value === null || typeof value !== "object") return;
    const context = transitTimeContext(value) ?? inherited;
    if (Array.isArray(value)) {
      if (value.length && value.every(isPlainObject)) {
        out.push({ path, rows: value as Record<string, unknown>[], timeContexts: value.map((row) => transitTimeContext(row) ?? context) });
      }
      // Also descend into nested arrays for things like locate candidates.
      value.slice(0, 50).forEach((item, i) => walk(item, `${path}[${i}]`, depth + 1, context));
      return;
    }
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) walk(v, path ? `${path}.${k}` : k, depth + 1, context);
  };
  walk(response, "", 0);
  // Prefer larger tables and shallower paths.
  return out.sort((a, b) => b.rows.length - a.rows.length || a.path.length - b.path.length).slice(0, 25);
}

/** Row sets for a whole multi-run result: the comparison table, then same-path arrays merged with a `route` column. */
function mergedTables(runs: RunRecord[]): TableCandidate[] {
  const byPath = new Map<string, Record<string, unknown>[]>();
  const contexts = new Map<string, (TransitTimeContext | undefined)[]>();
  const order: string[] = [];
  runs.forEach((run) => {
    for (const table of findTables(run.response)) {
      if (!byPath.has(table.path)) {
        byPath.set(table.path, []);
        contexts.set(table.path, []);
        order.push(table.path);
      }
      byPath.get(table.path)!.push(...table.rows.map((row) => ({ route: run.label, ...row })));
      contexts.get(table.path)!.push(...(table.timeContexts ?? table.rows.map(() => undefined)));
    }
  });
  const out: TableCandidate[] = [{ path: "routes", rows: runRows(runs).rows, timeContexts: runs.map((run) => transitTimeContext(run.response)) }];
  for (const path of order) out.push({ path, rows: byPath.get(path)!, timeContexts: contexts.get(path) });
  return out;
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

/** Index of the candidate matching the tool's preferred row set, else the first. */
function preferredIndex(tables: TableCandidate[], key: string | undefined): number {
  if (!key) return 0;
  const index = tables.findIndex((t) => t.path === key || t.path.endsWith(`.${key}`) || t.path === `result.${key}`);
  return index >= 0 ? index : 0;
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
  const hasSteps = result.tool === "directions";
  const tabs = hasSteps ? (["steps", "summary", "table", "json"] as const) : (["summary", "table", "json"] as const);
  const labels = { steps: "Directions", summary: "Summary", table: "Table", json: "JSON" };
  return (
    <section className="results">
      <ResultHeader result={result} />
      <div className="tabs" role="tablist">
        {tabs.map((t) => (
          <button key={t} type="button" role="tab" aria-selected={tab === t} className={tab === t ? "is-active" : ""} onClick={() => setState({ resultsTab: t })}>
            {labels[t]}
          </button>
        ))}
      </div>
      {tab === "steps" && hasSteps && <DirectionsList result={result} />}
      {tab === "steps" && !hasSteps && <SummaryTab result={result} />}
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
  const runs = result.runs;
  const failed = runs.filter((r) => r.error).length;
  const ok = runs.length - failed;
  const sumMs = runs.reduce((a, r) => a + r.timing.totalMs, 0);
  const bytes = runs.reduce((a, r) => a + r.timing.responseBytes, 0);
  const single = runs.length === 1 ? runs[0] : null;

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
          <span className="pill pill-error">not sent</span>
        ) : single ? (
          single.error ? (
            <span className="pill pill-error">{single.timing.status ? `HTTP ${single.timing.status}` : "failed"}</span>
          ) : (
            <span className="pill pill-ok">{single.timing.status}</span>
          )
        ) : (
          <>
            <span className="pill pill-ok">{ok} ok</span>
            {failed > 0 && <span className="pill pill-error">{failed} failed</span>}
          </>
        )}
        {result.live && <span className="pill pill-live">live</span>}
        {!result.error && (
          <span className="muted">
            {single ? fmtMs(single.timing.totalMs) : `${runs.length} routes · wall ${fmtMs(result.wallMs)} · Σ ${fmtMs(sumMs)}`} · {fmtBytes(bytes)} · {result.features.features.length} map features
          </span>
        )}
      </div>
      {result.error && <p className="error-text">{result.error}</p>}
      {single?.error && <p className="error-text">{single.error}</p>}
      {!result.error && (
        <div className="export-row">
          <button type="button" className="btn btn-small" onClick={() => downloadJson(`${slug}.json`, exportResponse(result))}>
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
          <button type="button" className="btn btn-ghost btn-small" onClick={() => downloadJson(`${slug}.request.json`, exportRequest(result))}>
            Request
          </button>
          <button type="button" className="btn btn-ghost btn-small" onClick={() => requestFit()} disabled={!result.features.features.length}>
            Zoom to
          </button>
          {message && <span className="muted">{message}</span>}
        </div>
      )}
    </div>
  );
}

// --- Sortable table ---

type SortState = { column: string; direction: 1 | -1 } | null;

function compareValues(a: unknown, b: unknown): number {
  if (a === b) return 0;
  if (a === undefined || a === null || a === "") return 1;
  if (b === undefined || b === null || b === "") return -1;
  if (typeof a === "number" && typeof b === "number") return a - b;
  return String(a).localeCompare(String(b), undefined, { numeric: true });
}

function useSorted(rows: Record<string, unknown>[], sort: SortState) {
  return useMemo(() => {
    if (!sort) return rows;
    const indexed = rows.map((row, index) => ({ row, index }));
    indexed.sort((x, y) => compareValues(x.row[sort.column], y.row[sort.column]) * sort.direction || x.index - y.index);
    return indexed.map((x) => x.row);
  }, [rows, sort]);
}

interface DataTableProps {
  timeContexts?: (TransitTimeContext | undefined)[];
  table: TableData;
  limit?: number;
  /** Index (in the unsorted rows) that is highlighted, and a click handler. */
  activeRow?: number | null;
  onRowClick?: (index: number) => void;
  /** Swatch colour for the row, by unsorted index. */
  rowColor?: (index: number) => string | null;
}

function DataTable({ table, limit = 300, activeRow = null, onRowClick, rowColor, timeContexts }: DataTableProps) {
  const [sort, setSort] = useState<SortState>(null);
  const indexed = useMemo(() => table.rows.map((row, index) => ({ ...row, __index: index })), [table.rows]);
  const sorted = useSorted(indexed, sort);
  const toggle = (column: string) =>
    setSort((s) => (s && s.column === column ? (s.direction === 1 ? { column, direction: -1 } : null) : { column, direction: 1 }));
  return (
    <div className="table-scroll">
      <table className={onRowClick ? "is-clickable" : ""}>
        <thead>
          <tr>
            {rowColor && <th className="th-swatch" />}
            {table.columns.map((c) => (
              <th key={c} onClick={() => toggle(c)} className={sort?.column === c ? "is-sorted" : ""} title="Sort">
                {c}
                {sort?.column === c && <span className="sort-mark">{sort.direction === 1 ? "▲" : "▼"}</span>}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {sorted.slice(0, limit).map((row) => {
            const index = row.__index as number;
            const color = rowColor?.(index) ?? null;
            return (
              <tr key={index} className={activeRow === index ? "is-active" : ""} onClick={onRowClick ? () => onRowClick(index) : undefined}>
                {rowColor && (
                  <td className="td-swatch">
                    <span className="row-swatch" style={{ background: color ?? "transparent" }} />
                  </td>
                )}
                {table.columns.map((c) => (
                  <td key={c} title={row[c] === undefined ? "" : String(row[c])}>
                    {row[c] === undefined || row[c] === null ? "" : typeof row[c] === "number" ? fmtValue(c, row[c], timeContexts?.[index]) : String(row[c])}
                  </td>
                ))}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

/** The per-route comparison table; clicking a row highlights that route on the map and zooms to it. */
function RunsTable({ result }: { result: ResultRecord }) {
  const selected = useStore((s) => s.selectedRun);
  const table = useMemo(() => runRows(result.runs), [result.runs]);
  const multi = result.runs.length > 1;
  const select = (index: number) => {
    const next = selected === index ? null : index;
    setState({ selectedRun: next });
    if (next !== null && result.runs[next].features.features.length) requestFit(next);
  };
  return (
    <DataTable
      table={table}
      timeContexts={result.runs.map((run) => transitTimeContext(run.response))}
      activeRow={selected}
      onRowClick={select}
      rowColor={(index) => {
        const run = result.runs[index];
        if (run.error) return PALETTE.grey;
        return multi ? routeColor(run.colorIndex) : PALETTE.crimson;
      }}
    />
  );
}

function SummaryTab({ result }: { result: ResultRecord }) {
  const selected = useStore((s) => s.selectedRun);
  const multi = result.runs.length > 1;
  const run = result.runs[selected ?? 0] ?? null;
  const entries = useMemo(() => (run ? summaryEntries(run.response) : []), [run]);
  const service = run && isPlainObject(run.response) && isPlainObject(run.response.service) ? run.response.service : null;
  const hints = run ? Object.entries(run.timing.serverHints) : [];
  return (
    <div className="summary">
      {multi && (
        <div className="runs-block">
          <RunsTable result={result} />
        </div>
      )}
      {run && multi && (
        <div className="table-toolbar summary-run-title">
          <span className="run-swatch" style={{ background: run.error ? PALETTE.grey : routeColor(run.colorIndex) }} />
          <strong>{run.label}</strong>
          {run.error && <span className="error-text">{run.error}</span>}
        </div>
      )}
      {run && entries.length === 0 && !run.error && <p className="muted">No headline figures for this response. See the table or JSON.</p>}
      {entries.length > 0 && (
        <dl className="kv kv-figures">
          {entries.map(([k, v]) => (
            <div key={k}>
              <dt>{k.replace(/_/g, " ")}</dt>
              <dd>{fmtValue(k, v, transitTimeContext(run?.response))}</dd>
            </div>
          ))}
        </dl>
      )}
      {run && result.tool === "transit_directions" && <TransitDirectionsList run={run} />}
      {(service || hints.length > 0) && (
        <details className="service-context">
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
  const tool = TOOL_BY_ID[result.tool];
  const multi = result.runs.length > 1;
  // -1 = all routes (merged), otherwise one run.
  const [scope, setScope] = useState<number>(multi ? -1 : 0);
  const effectiveScope = multi ? scope : 0;
  const tables = useMemo(
    () => (effectiveScope === -1 ? mergedTables(result.runs) : findTables(result.runs[effectiveScope]?.response ?? null)),
    [result.runs, effectiveScope],
  );
  const [selected, setSelected] = useState(() => preferredIndex(tables, tool.tableKey));
  useEffect(() => setSelected(preferredIndex(tables, tool.tableKey)), [tables, tool.tableKey]);
  const candidate = tables[Math.min(selected, tables.length - 1)];
  const table = useMemo(() => (candidate ? toTable(candidate) : null), [candidate]);
  const limit = 300;
  if (!table || !candidate) return <p className="muted">No tabular rows in this response.</p>;
  const isRoutes = candidate.path === "routes";
  return (
    <div className="table-tab">
      <div className="table-toolbar">
        {multi && (
          <select value={effectiveScope} onChange={(e) => setScope(Number(e.target.value))} aria-label="Route scope">
            <option value={-1}>All routes</option>
            {result.runs.map((run, i) => (
              <option key={i} value={i}>
                {run.label}
              </option>
            ))}
          </select>
        )}
        <select value={Math.min(selected, tables.length - 1)} onChange={(e) => setSelected(Number(e.target.value))} aria-label="Row set">
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
      {isRoutes ? <RunsTable result={result} /> : <DataTable table={table} limit={limit} timeContexts={candidate.timeContexts} />}
    </div>
  );
}

function JsonTab({ result }: { result: ResultRecord }) {
  const selectedRun = useStore((s) => s.selectedRun);
  const multi = result.runs.length > 1;
  const [scope, setScope] = useState<number | null>(null);
  const index = scope ?? selectedRun ?? 0;
  const run = result.runs[index] ?? null;
  const text = useMemo(() => {
    const value = run ? (run.response ?? { error: run.error }) : { error: result.error };
    const full = JSON.stringify(value, null, 2);
    return full.length > 200_000 ? `${full.slice(0, 200_000)}\n… truncated (${fmtBytes(full.length)} total; download the response for the full body)` : full;
  }, [run, result.error]);
  return (
    <div className="table-tab">
      {multi && (
        <div className="table-toolbar">
          <select value={index} onChange={(e) => setScope(Number(e.target.value))} aria-label="Route">
            {result.runs.map((r, i) => (
              <option key={i} value={i}>
                {r.label}
              </option>
            ))}
          </select>
          {run && <span className="muted">{fmtMs(run.timing.totalMs)} · {fmtBytes(run.timing.responseBytes)}</span>}
        </div>
      )}
      <pre className="json-view">{text}</pre>
    </div>
  );
}
