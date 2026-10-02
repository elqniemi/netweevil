import { useMemo, useState } from "react";
import { ORDINAL, PALETTE } from "../geo/features";
import { setState, type RunRecord } from "../state/store";
import { formatClock, stopLetter } from "../tools/clock";
import { fmtDistance, fmtDuration, fmtMs, fmtNumber } from "./format";

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function resultOf(run: RunRecord): Record<string, unknown> | null {
  const root = isObject(run.response) ? run.response : null;
  if (!root) return null;
  return isObject(root.result) ? root.result : null;
}

// --- Elevation profile chart ---

export interface ProfilePoint {
  distance: number;
  height: number | null;
}

/** Clean axis ticks (1, 2, 2.5 or 5 times a power of ten) covering [min, max]. */
export function niceTicks(min: number, max: number, count = 4): number[] {
  if (!Number.isFinite(min) || !Number.isFinite(max)) return [];
  if (max - min < 1e-9) return [min];
  const raw = (max - min) / count;
  const power = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * power).find((s) => s >= raw) ?? raw;
  const ticks: number[] = [];
  for (let t = Math.ceil(min / step) * step; t <= max + step * 1e-6; t += step) ticks.push(Math.round(t * 1e6) / 1e6);
  return ticks;
}

const W = 340;
const H = 132;
const PAD = { left: 38, right: 8, top: 8, bottom: 22 };

/**
 * Height against distance: a 2px line over a light wash, hairline grid, and a
 * crosshair tooltip. Hovering also marks the position on the map when the
 * chart knows the coordinates of its points.
 */
export function ElevationChart({ points, coordinates, title }: { points: ProfilePoint[]; coordinates?: number[][] | null; title: string }) {
  const [hover, setHover] = useState<number | null>(null);
  const known = points.filter((p): p is { distance: number; height: number } => p.height !== null && Number.isFinite(p.height));
  const geometry = useMemo(() => {
    if (known.length < 2) return null;
    const maxD = points[points.length - 1].distance || 1;
    let minH = Math.min(...known.map((p) => p.height));
    let maxH = Math.max(...known.map((p) => p.height));
    if (maxH - minH < 10) {
      const mid = (maxH + minH) / 2;
      minH = mid - 5;
      maxH = mid + 5;
    }
    const yTicks = niceTicks(minH, maxH, 3);
    const lo = Math.min(minH, yTicks[0] ?? minH);
    const hi = Math.max(maxH, yTicks[yTicks.length - 1] ?? maxH);
    const x = (d: number) => PAD.left + (d / maxD) * (W - PAD.left - PAD.right);
    const y = (h: number) => PAD.top + (1 - (h - lo) / (hi - lo || 1)) * (H - PAD.top - PAD.bottom);
    // Unknown heights break the line rather than dropping to zero.
    const runs: { distance: number; height: number }[][] = [];
    let current: { distance: number; height: number }[] = [];
    for (const p of points) {
      if (p.height === null || !Number.isFinite(p.height)) {
        if (current.length) runs.push(current);
        current = [];
      } else current.push(p as { distance: number; height: number });
    }
    if (current.length) runs.push(current);
    const line = runs.map((run) => run.map((p, i) => `${i ? "L" : "M"}${x(p.distance).toFixed(1)},${y(p.height).toFixed(1)}`).join("")).join("");
    const base = H - PAD.bottom;
    const area = runs
      .filter((run) => run.length > 1)
      .map((run) => `M${x(run[0].distance).toFixed(1)},${base}${run.map((p) => `L${x(p.distance).toFixed(1)},${y(p.height).toFixed(1)}`).join("")}L${x(run[run.length - 1].distance).toFixed(1)},${base}Z`)
      .join("");
    const xTicks = niceTicks(0, maxD / 1000, 4).map((km) => km * 1000);
    return { x, y, line, area, yTicks, xTicks, maxD };
  }, [points, known]);

  if (!geometry) return <p className="muted">{title}: no known heights along this line. The dataset may have been imported without a DEM.</p>;
  const { x, y, line, area, yTicks, xTicks, maxD } = geometry;

  const onMove = (event: React.PointerEvent<SVGSVGElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    const d = ((event.clientX - rect.left) / rect.width * W - PAD.left) / (W - PAD.left - PAD.right) * maxD;
    let best = 0;
    for (let i = 1; i < points.length; i++) if (Math.abs(points[i].distance - d) < Math.abs(points[best].distance - d)) best = i;
    setHover(best);
    const c = coordinates?.[best];
    setState({ profileHover: c && coordinates!.length === points.length ? [c[0], c[1]] : null });
  };
  const onLeave = () => {
    setHover(null);
    setState({ profileHover: null });
  };
  const hovered = hover !== null ? points[hover] : null;
  return (
    <figure className="elevation-chart">
      <figcaption>{title}</figcaption>
      <svg viewBox={`0 0 ${W} ${H}`} role="img" aria-label={`${title}, heights from ${fmtNumber(Math.min(...known.map((p) => p.height)))} to ${fmtNumber(Math.max(...known.map((p) => p.height)))} m`} onPointerMove={onMove} onPointerLeave={onLeave}>
        {yTicks.map((t) => (
          <g key={`y${t}`}>
            <line x1={PAD.left} x2={W - PAD.right} y1={y(t)} y2={y(t)} className="chart-grid" />
            <text x={PAD.left - 5} y={y(t)} className="chart-tick" textAnchor="end" dominantBaseline="middle">
              {fmtNumber(t)}
            </text>
          </g>
        ))}
        {xTicks.map((t) => (
          <text key={`x${t}`} x={x(t)} y={H - 6} className="chart-tick" textAnchor={t === 0 ? "start" : "middle"}>
            {t === 0 ? "0 km" : fmtNumber(t / 1000)}
          </text>
        ))}
        <path d={area} fill={PALETTE.ochre} fillOpacity={0.12} stroke="none" />
        <path d={line} fill="none" stroke={PALETTE.ochre} strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
        {hovered && (
          <g pointerEvents="none">
            <line x1={x(hovered.distance)} x2={x(hovered.distance)} y1={PAD.top} y2={H - PAD.bottom} className="chart-crosshair" />
            {hovered.height !== null && <circle cx={x(hovered.distance)} cy={y(hovered.height)} r={4} fill={PALETTE.ochre} stroke="#ffffff" strokeWidth={2} />}
          </g>
        )}
        <rect x={PAD.left} y={PAD.top} width={W - PAD.left - PAD.right} height={H - PAD.top - PAD.bottom} fill="transparent" />
      </svg>
      <div className="chart-readout" aria-live="polite">
        {hovered ? `${fmtDistance(hovered.distance)} · ${hovered.height === null ? "height unknown" : `${hovered.height.toFixed(1)} m`}` : "Hover the profile to read heights and see the position on the map."}
      </div>
    </figure>
  );
}

// --- Route extras: elevation and regions ---

interface RegionSpan {
  country_code: string | null;
  country_name: string | null;
  state_code: string | null;
  state_name: string | null;
  driving_side: string | null;
  start_distance_m: number;
  distance_m: number;
}

function spanLabel(span: RegionSpan): string {
  const country = span.country_name ?? span.country_code;
  const state = span.state_name ?? span.state_code;
  if (!country && !state) return "outside known areas";
  return [state, country].filter(Boolean).join(", ");
}

/** Country/state spans along the route as one proportional bar, with a keyed list. */
export function RegionSpans({ regions }: { regions: Record<string, unknown> }) {
  const spans = (Array.isArray(regions.spans) ? regions.spans : []) as RegionSpan[];
  const total = spans.reduce((sum, s) => sum + (s.distance_m || 0), 0) || 1;
  // Colours follow the area, in order of first appearance.
  const keys: string[] = [];
  for (const s of spans) {
    const key = spanLabel(s);
    if (!keys.includes(key)) keys.push(key);
  }
  const color = (s: RegionSpan) => (spanLabel(s) === "outside known areas" ? "#c9d0d6" : ORDINAL[keys.indexOf(spanLabel(s)) % ORDINAL.length]);
  const timezones = Array.isArray(regions.timezones) ? (regions.timezones as string[]) : [];
  return (
    <div className="region-spans">
      <div className="section-title">Regions</div>
      {spans.length === 0 && <p className="muted">No country or state boundaries are known along this route; the dataset was imported without them.</p>}
      {spans.length > 0 && (
        <div className="region-bar" role="img" aria-label={spans.map((s) => `${spanLabel(s)} ${fmtDistance(s.distance_m)}`).join(", ")}>
          {spans.map((s, i) => (
            <span key={i} style={{ flexGrow: Math.max(s.distance_m, 0.001) / total, background: color(s) }} title={`${spanLabel(s)} · ${fmtDistance(s.distance_m)} from ${fmtDistance(s.start_distance_m)}`} />
          ))}
        </div>
      )}
      <ul className="region-list">
        {spans.map((s, i) => (
          <li key={i}>
            <span className="row-swatch" style={{ background: color(s) }} />
            <span>{spanLabel(s)}</span>
            <span className="muted">
              {fmtDistance(s.start_distance_m)} – {fmtDistance(s.start_distance_m + s.distance_m)}
              {s.driving_side ? ` · drives on the ${s.driving_side}` : ""}
            </span>
          </li>
        ))}
      </ul>
      <dl className="kv kv-compact">
        <dt>Border crossings</dt>
        <dd>{fmtNumber(Number(regions.border_crossing_count ?? 0))}</dd>
        <dt>Timezones</dt>
        <dd>{timezones.length ? timezones.join(" → ") : "–"}</dd>
      </dl>
    </div>
  );
}

function ElevationSummary({ elevation, geometry }: { elevation: Record<string, unknown>; geometry: number[][] | null }) {
  const raw = Array.isArray(elevation.points) ? (elevation.points as [number, number | null][]) : [];
  const points = raw.map(([distance, height]) => ({ distance, height }));
  const grade = (v: unknown) => (typeof v === "number" ? `${(v * 100).toFixed(1)} %` : "–");
  const metre = (v: unknown) => (typeof v === "number" ? `${fmtNumber(v)} m` : "–");
  return (
    <div className="route-elevation">
      <ElevationChart points={points} coordinates={geometry} title={`Elevation (m${elevation.vertical_datum ? `, ${elevation.vertical_datum}` : ""})`} />
      <dl className="kv kv-figures">
        <div><dt>ascent</dt><dd>{metre(elevation.ascent_m)}</dd></div>
        <div><dt>descent</dt><dd>{metre(elevation.descent_m)}</dd></div>
        <div><dt>lowest</dt><dd>{metre(elevation.min_elevation_m)}</dd></div>
        <div><dt>highest</dt><dd>{metre(elevation.max_elevation_m)}</dd></div>
        <div><dt>steepest uphill</dt><dd>{grade(elevation.max_uphill_grade)}</dd></div>
        <div><dt>steepest downhill</dt><dd>{grade(elevation.max_downhill_grade)}</dd></div>
      </dl>
    </div>
  );
}

/** Elevation profile and region spans of a route response, when requested. */
export function RouteExtras({ run }: { run: RunRecord }) {
  const result = resultOf(run);
  if (!result) return null;
  const elevation = isObject(result.elevation) ? result.elevation : null;
  const regions = isObject(result.regions) ? result.regions : null;
  if (!elevation && !regions) return null;
  const geometry = Array.isArray(result.geometry) ? (result.geometry as number[][]) : null;
  return (
    <div className="route-extras">
      {elevation && <ElevationSummary elevation={elevation} geometry={geometry} />}
      {regions && <RegionSpans regions={regions} />}
    </div>
  );
}

// --- Trip ---

/** Visit order, schedule and optimizer figures of a `/v1/trip` response. */
export function TripSummary({ run }: { run: RunRecord }) {
  const root = isObject(run.response) ? run.response : null;
  // GeoJSON responses carry the trip fields at the top level.
  const result = root && isObject(root.result) ? root.result : root;
  if (!result || !isObject(result.optimization)) return null;
  const opt = result.optimization;
  const visits = (Array.isArray(result.visits) ? result.visits : []).filter(isObject);
  const legs = (Array.isArray(result.legs) ? result.legs : []).filter(isObject);
  const late = Array.isArray(result.late_stops) ? result.late_stops.length : 0;
  const met = result.time_windows_met === true;
  const cost = (v: unknown) => (typeof v === "number" ? fmtNumber(v) : "–");
  const saving = typeof opt.input_order_cost === "number" && opt.input_order_cost > 0 && typeof opt.cost === "number" ? 1 - opt.cost / opt.input_order_cost : null;
  return (
    <div className="trip-summary">
      <p className="trip-status">
        <span className={`pill ${met ? "pill-ok" : "pill-warn"}`}>{met ? "✓ all windows met" : `! ${late} late ${late === 1 ? "stop" : "stops"}`}</span>
        <span className="muted">
          {formatClock(result.start_s as number)} – {formatClock(result.end_s as number)} · {result.mode as string}
        </span>
      </p>
      {visits.length > 0 && (
        <div className="table-scroll">
          <table className="trip-visits">
            <thead>
              <tr>
                <th>#</th>
                <th>stop</th>
                <th>arrive</th>
                <th>wait</th>
                <th>leave</th>
                <th>late</th>
                <th>leg to next</th>
              </tr>
            </thead>
            <tbody>
              {visits.map((v, i) => {
                const leg = legs[i];
                const summary = leg && isObject(leg.summary) ? leg.summary : null;
                const lateness = Number(v.lateness_s ?? 0);
                return (
                  <tr key={i} className={lateness > 1e-6 ? "is-late" : ""}>
                    <td>{i + 1}</td>
                    <td title={String(v.point_id ?? "")}>
                      {stopLetter(Number(v.stop_index))} <span className="muted">{String(v.point_id ?? "")}</span>
                    </td>
                    <td>{formatClock(v.arrival_s as number)}</td>
                    <td>{Number(v.wait_s) > 0 ? fmtDuration(Number(v.wait_s)) : ""}</td>
                    <td>{formatClock(v.departure_s as number)}</td>
                    <td>{lateness > 1e-6 ? fmtDuration(lateness) : ""}</td>
                    <td>{summary ? `${fmtDistance(Number(summary.total_distance_m))} · ${fmtDuration(Number(summary.total_travel_time_s))}` : ""}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      <details className="service-context" open>
        <summary>Optimization</summary>
        <dl className="kv">
          <dt>method</dt>
          <dd>
            {String(opt.method).replace(/_/g, " ")}
            {opt.proven_optimal === true ? " (proven optimal)" : ""}
            {opt.proven_infeasible === true ? " (windows proven infeasible)" : ""}
          </dd>
          <dt>cost</dt>
          <dd>
            {cost(opt.cost)}
            {saving !== null && Math.abs(saving) > 1e-4 ? ` (${(saving * 100).toFixed(1)} % below the input order)` : ""}
          </dd>
          <dt>input order cost</dt>
          <dd>{opt.input_order_cost === null ? "some leg has no route" : cost(opt.input_order_cost)}</dd>
          <dt>best construction</dt>
          <dd>{cost(opt.construction_cost)}</dd>
          <dt>iterations</dt>
          <dd>
            {fmtNumber(Number(opt.iterations ?? 0))} (seed {String(opt.seed)})
          </dd>
          <dt>time</dt>
          <dd>
            matrix {fmtMs(Number(opt.matrix_ms))} · solve {fmtMs(Number(opt.solve_ms))} · legs {fmtMs(Number(opt.legs_ms))}
          </dd>
        </dl>
      </details>
    </div>
  );
}

// --- Height probe ---

export function HeightSummary({ run }: { run: RunRecord }) {
  const result = resultOf(run);
  if (!result) return null;
  const body = isObject(run.requestBody) ? run.requestBody : {};
  const range = Array.isArray(result.range_heights) ? (result.range_heights as [number, number | null][]) : null;
  const shape = Array.isArray(result.shape) ? (result.shape as number[][]) : Array.isArray(body.points) ? (body.points as number[][]) : null;
  const heights = Array.isArray(result.heights) ? (result.heights as (number | null)[]) : null;
  const service = isObject((run.response as Record<string, unknown>)?.service) ? ((run.response as Record<string, unknown>).service as Record<string, unknown>) : {};
  const datum = service.vertical_datum ? ` (${service.vertical_datum})` : "";
  if (range) {
    return <ElevationChart points={range.map(([distance, height]) => ({ distance, height }))} coordinates={shape} title={`Terrain profile (m${service.vertical_datum ? `, ${service.vertical_datum}` : ""})`} />;
  }
  if (!heights) return null;
  return (
    <div className="table-scroll">
      <table>
        <thead>
          <tr>
            <th>#</th>
            <th>lon, lat</th>
            <th>height{datum}</th>
          </tr>
        </thead>
        <tbody>
          {heights.map((h, i) => (
            <tr key={i}>
              <td>{i + 1}</td>
              <td>{shape?.[i] ? `${shape[i][0].toFixed(5)}, ${shape[i][1].toFixed(5)}` : ""}</td>
              <td>{h === null ? "no data" : `${h.toFixed(1)} m`}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
