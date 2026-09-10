import { transitTimeContext } from "../api/time";
import { flyTo, type RunRecord } from "../state/store";
import { fmtDistance, fmtDuration, fmtTransitTime, fmtValue } from "./format";

interface WalkingManeuver {
  instruction: string;
  kind?: string;
  location?: number[];
  distance_m?: number;
  edge_time_s?: number;
  from_elevation_m?: number;
  to_elevation_m?: number;
}

interface TransitDirection {
  sequence: number;
  kind: string;
  instruction: string;
  departure_s?: number;
  arrival_s?: number;
  duration_s?: number;
  departure_datetime?: string;
  arrival_datetime?: string;
  platform?: unknown;
  route_short_name?: string;
  headsign?: string;
  location?: number[];
  walking_maneuvers?: WalkingManeuver[];
}

export function extractTransitDirections(response: unknown): TransitDirection[] {
  if (!response || typeof response !== "object") return [];
  const directions = (response as { directions?: unknown }).directions;
  if (!Array.isArray(directions)) return [];
  return directions.filter((step): step is TransitDirection => step && typeof step === "object" && typeof step.instruction === "string");
}

function focus(location: number[] | undefined) {
  if (location && Number.isFinite(location[0]) && Number.isFinite(location[1])) flyTo(location[0], location[1], 17);
}

/** The selected run's instructions share the map's route scope and XYZ result. */
export function TransitDirectionsList({ run }: { run: RunRecord }) {
  const steps = extractTransitDirections(run.response);
  const context = transitTimeContext(run.response);
  const diagnostics = (run.response as { directions_diagnostics?: string[] } | null)?.directions_diagnostics ?? [];
  if (run.error) return <p className="error-text">{run.error}</p>;
  if (!steps.length) return <p className="muted">No journey directions were returned for these endpoints and settings.</p>;
  return <section className="directions transit-directions" aria-label="Journey directions">
    <div className="directions-head"><strong>Journey directions</strong><span className="muted">{steps.length} steps</span></div>
    {diagnostics.map((message, index) => <p className="style-note" key={index}>{message}</p>)}
    <ol className="steps">
      {steps.map((step, index) => <li className="step" key={`${step.sequence}-${index}`}>
        <button type="button" className="step-btn" onClick={() => focus(step.location)} title={step.location ? "Show on map" : undefined}>
          <span className="step-marker"><span>{step.sequence ?? index + 1}</span>{index < steps.length - 1 && <span className="step-line" />}</span>
          <span className="step-body">
            <span className="step-text">{step.instruction}</span>
            <span className="step-meta">
              {String(step.kind).replace(/_/g, " ")}
              {step.route_short_name && ` · ${step.route_short_name}`}
              {step.headsign && ` → ${step.headsign}`}
              {step.platform !== null && step.platform !== undefined && ` · Platform ${fmtValue("platform", step.platform)}`}
            </span>
            {(typeof step.departure_s === "number" || typeof step.arrival_s === "number") && <span className="step-meta">
              {typeof step.departure_s === "number" && <time dateTime={step.departure_datetime}>{fmtTransitTime(step.departure_s, context)}</time>}
              {typeof step.arrival_s === "number" && step.arrival_s !== step.departure_s && <> → <time dateTime={step.arrival_datetime}>{fmtTransitTime(step.arrival_s, context)}</time></>}
            </span>}
          </span>
          <span className="step-at muted">{typeof step.duration_s === "number" && step.duration_s > 0 ? fmtDuration(step.duration_s) : ""}</span>
        </button>
        {!!step.walking_maneuvers?.length && <details className="transit-walking-steps">
          <summary>Walking instructions ({step.walking_maneuvers.length})</summary>
          <ol>{step.walking_maneuvers.map((maneuver, i) => <li key={i}>
            <button type="button" className="btn btn-ghost btn-small" onClick={() => focus(maneuver.location)} title={maneuver.location ? "Show on map" : undefined}>{maneuver.instruction}</button>
            <span className="step-meta">{typeof maneuver.distance_m === "number" && fmtDistance(maneuver.distance_m)}{typeof maneuver.edge_time_s === "number" && ` · ${fmtDuration(maneuver.edge_time_s)}`}</span>
            {typeof maneuver.from_elevation_m === "number" && typeof maneuver.to_elevation_m === "number" && <span className="step-meta"> · Source elevation {maneuver.from_elevation_m.toFixed(2)} → {maneuver.to_elevation_m.toFixed(2)} m</span>}
          </li>)}</ol>
        </details>}
      </li>)}
    </ol>
  </section>;
}
