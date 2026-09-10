import { useMemo, useState } from "react";
import { PALETTE, routeColor } from "../geo/features";
import { flyTo, useStore, type ResultRecord, type RunRecord } from "../state/store";
import { fmtDistance, fmtDuration } from "./format";

interface Maneuver {
  kind: string;
  instruction: string;
  street_name: string | null;
  location: [number, number] | null;
  distance_m: number;
  edge_time_s: number;
  roundabout_exit_count: number | null;
}

interface Summary {
  total_distance_m?: number;
  total_travel_time_s?: number;
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/** Pulls the maneuver list and route summary out of a directions response. */
export function extractDirections(response: unknown): { maneuvers: Maneuver[]; summary: Summary } | null {
  if (!isObject(response) || !isObject(response.result)) return null;
  const result = response.result;
  if (!Array.isArray(result.maneuvers)) return null;
  const maneuvers = (result.maneuvers as Record<string, unknown>[]).map((m) => ({
    kind: String(m.kind ?? "continue"),
    instruction: String(m.instruction ?? ""),
    street_name: typeof m.street_name === "string" ? m.street_name : null,
    location: Array.isArray(m.location) && m.location.length >= 2 ? ([Number(m.location[0]), Number(m.location[1])] as [number, number]) : null,
    distance_m: typeof m.distance_m === "number" ? m.distance_m : 0,
    edge_time_s: typeof m.edge_time_s === "number" ? m.edge_time_s : 0,
    roundabout_exit_count: typeof m.roundabout_exit_count === "number" ? m.roundabout_exit_count : null,
  }));
  const route = isObject(result.route) ? result.route : {};
  const summary = isObject(route.summary) ? (route.summary as Summary) : {};
  return { maneuvers, summary };
}

/** Rotation (degrees, clockwise) of the arrow glyph for a maneuver kind. */
const TURN_ANGLE: Record<string, number> = {
  continue: 0,
  straight: 0,
  slight_left: -40,
  slight_right: 40,
  left: -90,
  right: 90,
  sharp_left: -135,
  sharp_right: 135,
  merge_left: -30,
  merge_right: 30,
  fork_left: -30,
  fork_right: 30,
  ramp_left: -45,
  ramp_right: 45,
  keep_left: -20,
  keep_right: 20,
};

function ManeuverIcon({ kind, exit }: { kind: string; exit: number | null }) {
  if (kind === "depart") {
    return (
      <svg viewBox="0 0 24 24" className="step-icon" aria-hidden="true">
        <circle cx="12" cy="12" r="5" fill="currentColor" />
      </svg>
    );
  }
  if (kind === "arrive") {
    return (
      <svg viewBox="0 0 24 24" className="step-icon" aria-hidden="true">
        <path d="M12 2a6 6 0 0 0-6 6c0 4.5 6 12 6 12s6-7.5 6-12a6 6 0 0 0-6-6zm0 8.5a2.5 2.5 0 1 1 0-5 2.5 2.5 0 0 1 0 5z" fill="currentColor" />
      </svg>
    );
  }
  if (kind.startsWith("uturn")) {
    const flip = kind.endsWith("right") ? -1 : 1;
    return (
      <svg viewBox="0 0 24 24" className="step-icon" aria-hidden="true" style={{ transform: `scaleX(${flip})` }}>
        <path d="M15 20V8a4 4 0 0 0-8 0v3" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" />
        <path d="M4 9l3 3 3-3" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    );
  }
  if (kind.startsWith("roundabout") || kind.startsWith("rotary")) {
    return (
      <svg viewBox="0 0 24 24" className="step-icon" aria-hidden="true">
        <circle cx="12" cy="13" r="5.5" fill="none" stroke="currentColor" strokeWidth="2.2" />
        <path d="M12 7.5V2.5" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" />
        <path d="M9 5.5l3-3 3 3" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" />
        {exit !== null && (
          <text x="12" y="16" textAnchor="middle" fontSize="8" fontWeight="700" fill="currentColor">
            {exit}
          </text>
        )}
      </svg>
    );
  }
  const angle = TURN_ANGLE[kind] ?? 0;
  return (
    <svg viewBox="0 0 24 24" className="step-icon" aria-hidden="true" style={{ transform: `rotate(${angle}deg)` }}>
      <path d="M12 21V5" stroke="currentColor" strokeWidth="2.6" strokeLinecap="round" />
      <path d="M6 11l6-6 6 6" fill="none" stroke="currentColor" strokeWidth="2.6" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

/** Turn-by-turn list in the style of a navigation app: one row per maneuver, with the leg distance and time after it. */
export function DirectionsList({ result }: { result: ResultRecord }) {
  const selectedRun = useStore((s) => s.selectedRun);
  const [scope, setScope] = useState<number | null>(null);
  const [active, setActive] = useState<number | null>(null);
  const multi = result.runs.length > 1;
  const index = Math.min(scope ?? selectedRun ?? 0, result.runs.length - 1);
  const run: RunRecord | undefined = result.runs[index];
  const directions = useMemo(() => (run ? extractDirections(run.response) : null), [run]);

  if (!run) return null;
  if (run.error) return <p className="error-text">{run.error}</p>;
  if (!directions || !directions.maneuvers.length) return <p className="muted">No maneuvers in this response.</p>;

  const total = directions.summary;
  const totalDistance = total.total_distance_m ?? directions.maneuvers.reduce((a, m) => a + m.distance_m, 0);
  const totalTime = total.total_travel_time_s ?? directions.maneuvers.reduce((a, m) => a + m.edge_time_s, 0);
  const color = multi ? routeColor(run.colorIndex) : PALETTE.crimson;
  let cumulative = 0;

  return (
    <div className="directions">
      <div className="directions-head">
        {multi && (
          <select value={index} onChange={(e) => setScope(Number(e.target.value))} aria-label="Route">
            {result.runs.map((r, i) => (
              <option key={i} value={i}>
                {r.label}
              </option>
            ))}
          </select>
        )}
        <span className="directions-swatch" style={{ background: color }} />
        <strong>{fmtDuration(totalTime)}</strong>
        <span className="muted">{fmtDistance(totalDistance)}</span>
        <span className="spacer" />
        <span className="muted">{directions.maneuvers.length} steps</span>
      </div>
      <ol className="steps">
        {directions.maneuvers.map((m, i) => {
          const at = cumulative;
          cumulative += m.distance_m;
          const last = i === directions.maneuvers.length - 1;
          return (
            <li key={i} className={`step${active === i ? " is-active" : ""}`}>
              <button
                type="button"
                className="step-btn"
                onClick={() => {
                  setActive(i);
                  if (m.location) flyTo(m.location[0], m.location[1], 16);
                }}
                title={m.location ? "Show on map" : undefined}
              >
                <span className="step-marker" style={{ color }}>
                  <ManeuverIcon kind={m.kind} exit={m.roundabout_exit_count} />
                  {!last && <span className="step-line" />}
                </span>
                <span className="step-body">
                  <span className="step-text">{m.instruction || m.kind.replace(/_/g, " ")}</span>
                  {!last && (
                    <span className="step-meta">
                      {fmtDistance(m.distance_m)} · {fmtDuration(m.edge_time_s)}
                    </span>
                  )}
                </span>
                <span className="step-at muted">{i === 0 ? "" : fmtDistance(at)}</span>
              </button>
            </li>
          );
        })}
      </ol>
    </div>
  );
}
