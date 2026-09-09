import { useMemo, useState } from "react";
import type { ServiceInfo } from "../api/types";
import { toCurl } from "../api/client";
import { copyText } from "../geo/export";
import { SLOT_COLORS } from "../geo/features";
import { currentBody, runTool } from "../state/run";
import {
  clearToolPoints,
  getForm,
  nextPointId,
  removePoint,
  resetForm,
  setFormValue,
  setPoints,
  setState,
  updatePoint,
  useStore,
  type InputPoint,
  type ToolId,
} from "../state/store";
import { TOOL_BY_ID, fieldDefaults, type Field, type SlotDef, type ToolDef } from "../tools/registry";
import { SimulationPanel } from "./SimulationPanel";

interface Props {
  service: ServiceInfo | null;
}

export function ToolPanel({ service }: Props) {
  const toolId = useStore((s) => s.tool);
  const tool = TOOL_BY_ID[toolId];
  return (
    <section className="tool-panel" aria-label={tool.label}>
      <div className="tool-head">
        <h1>{tool.label}</h1>
        <code className="endpoint">{tool.method} {tool.path}</code>
        <p className="tool-desc">{tool.description}</p>
      </div>
      <ProfilePicker tool={tool} service={service} />
      <PointsEditor tool={tool} />
      <FormFields tool={tool} service={service} />
      {tool.id === "simulation" ? <SimulationPanel service={service} /> : <RunBar tool={tool} service={service} />}
    </section>
  );
}

function ProfilePicker({ tool, service }: { tool: ToolDef; service: ServiceInfo | null }) {
  const profileId = useStore((s) => s.profileId);
  const feedId = useStore((s) => s.feedId);
  const form = useStore((s) => s.form[tool.id]);
  const engineMode = String(form?.engine_mode ?? "auto");
  const showEngine = ["route", "od", "matrix", "accessibility"].includes(tool.id);
  const showFeed = tool.group === "transit";
  if (!tool.usesProfile && !showFeed) return null;
  return (
    <div className="picker-row">
      {tool.usesProfile && (
        <label className="field">
          <span>Profile</span>
          <select value={profileId ?? service?.default_profile_id ?? ""} onChange={(e) => setState({ profileId: e.target.value })}>
            {(service?.loaded_profiles ?? []).map((p) => (
              <option key={p.profile_id} value={p.profile_id}>
                {p.profile_id} ({p.mode})
              </option>
            ))}
            {!service && <option value="">loading…</option>}
          </select>
        </label>
      )}
      {showFeed && (
        <label className="field">
          <span>Transit feed</span>
          <select value={feedId ?? service?.loaded_transit_feeds[0]?.feed_id ?? ""} onChange={(e) => setState({ feedId: e.target.value })}>
            {(service?.loaded_transit_feeds ?? []).map((f) => (
              <option key={f.feed_id} value={f.feed_id}>
                {f.feed_id}
              </option>
            ))}
            {service && service.loaded_transit_feeds.length === 0 && <option value="">no feed loaded</option>}
          </select>
        </label>
      )}
      {showEngine && (
        <label className="field">
          <span>Engine</span>
          <select value={engineMode} onChange={(e) => setFormValue(tool.id, "engine_mode", e.target.value)}>
            <option value="auto">auto (all restrictions)</option>
            <option value="ignore_multi_edge_restrictions">ignore multi-edge restrictions</option>
          </select>
        </label>
      )}
    </div>
  );
}

// --- Points ---

function PointsEditor({ tool }: { tool: ToolDef }) {
  const points = useStore((s) => s.points);
  const activeSlot = useStore((s) => s.activeSlot[tool.id]) ?? tool.slots[0]?.id;
  const viewport = useStore((s) => s.viewport.bbox);
  if (!tool.slots.length) return null;
  const total = tool.slots.reduce((n, slot) => n + (points[`${tool.id}:${slot.id}`]?.length ?? 0), 0);
  return (
    <div className="points">
      <div className="points-head">
        <span className="section-title">Map points</span>
        <span className="muted">click to add, drag to move, right-click to remove</span>
        {total > 0 && (
          <button type="button" className="btn btn-ghost btn-small" onClick={() => clearToolPoints(tool.id)}>
            Clear
          </button>
        )}
      </div>
      {tool.slots.map((slot) => (
        <SlotRow
          key={slot.id}
          tool={tool}
          slot={slot}
          points={points[`${tool.id}:${slot.id}`] ?? []}
          active={activeSlot === slot.id}
          viewport={viewport}
        />
      ))}
    </div>
  );
}

function SlotRow({ tool, slot, points, active, viewport }: { tool: ToolDef; slot: SlotDef; points: InputPoint[]; active: boolean; viewport: [number, number, number, number] | null }) {
  const [randomCount, setRandomCount] = useState(slot.max === 1 ? 1 : 20);
  const [pasteOpen, setPasteOpen] = useState(false);
  const [pasteText, setPasteText] = useState("");
  const color = SLOT_COLORS[slot.id] ?? "#7B3FB8";
  const multi = slot.max !== 1;

  const scatter = () => {
    if (!viewport) return;
    const [w, s, e, n] = viewport;
    // Keep away from the edges so snapping has network nearby.
    const padX = (e - w) * 0.1;
    const padY = (n - s) * 0.1;
    const generated: InputPoint[] = Array.from({ length: Math.max(1, randomCount) }, () => ({
      id: nextPointId(slot.id),
      lon: w + padX + Math.random() * (e - w - 2 * padX),
      lat: s + padY + Math.random() * (n - s - 2 * padY),
      kind: slot.kinds ? "break" : undefined,
    }));
    setPoints(tool.id, slot.id, multi ? [...points, ...generated] : generated.slice(0, 1));
  };

  const applyPaste = () => {
    const parsed: InputPoint[] = [];
    try {
      const value = JSON.parse(pasteText);
      const list = Array.isArray(value) ? value : Array.isArray((value as { points?: unknown[] }).points) ? (value as { points: unknown[] }).points : [];
      for (const item of list as Record<string, unknown>[]) {
        if (typeof item.lon === "number" && typeof item.lat === "number") {
          parsed.push({ id: String(item.id ?? nextPointId(slot.id)), lon: item.lon, lat: item.lat, z: typeof item.z === "number" ? item.z : undefined, weight: typeof item.weight === "number" ? item.weight : undefined });
        }
      }
    } catch {
      // Fall back to "lon,lat" or "lon lat" per line.
      for (const line of pasteText.split(/\n/)) {
        const m = line.trim().match(/^(-?\d+(?:\.\d+)?)[ ,;\t]+(-?\d+(?:\.\d+)?)/);
        if (m) parsed.push({ id: nextPointId(slot.id), lon: Number(m[1]), lat: Number(m[2]) });
      }
    }
    if (parsed.length) {
      setPoints(tool.id, slot.id, multi ? [...points, ...parsed] : parsed.slice(0, 1));
      setPasteText("");
      setPasteOpen(false);
    }
  };

  return (
    <div className={`slot${active ? " is-active" : ""}`}>
      <button type="button" className="slot-head" onClick={() => setState((prev) => ({ activeSlot: { ...prev.activeSlot, [tool.id]: slot.id } }))} title="Map clicks add to this slot">
        <span className="slot-swatch" style={{ background: color }} />
        <span className="slot-label">{slot.label}</span>
        <span className="slot-count">{points.length}</span>
      </button>
      {active && (
        <div className="slot-body">
          {slot.hint && <div className="muted">{slot.hint}</div>}
          <div className="slot-actions">
            {multi && <input type="number" min={1} max={5000} value={randomCount} onChange={(e) => setRandomCount(Number(e.target.value))} aria-label="Random point count" />}
            <button type="button" className="btn btn-small" onClick={scatter} disabled={!viewport}>
              {multi ? "Scatter in view" : "Random in view"}
            </button>
            <button type="button" className="btn btn-ghost btn-small" onClick={() => setPasteOpen((v) => !v)}>
              Paste
            </button>
          </div>
          {pasteOpen && (
            <div className="paste">
              <textarea rows={4} value={pasteText} onChange={(e) => setPasteText(e.target.value)} placeholder={'[{"id":"a","lon":6.56,"lat":53.22}] or one "lon,lat" per line'} />
              <button type="button" className="btn btn-small" onClick={applyPaste}>
                Add points
              </button>
            </div>
          )}
          {points.length > 0 && (
            <ul className="point-list">
              {points.slice(0, 60).map((p, index) => (
                <li key={`${p.id}-${index}`}>
                  <span className="point-index">{index + 1}</span>
                  <input value={p.id} onChange={(e) => updatePoint(tool.id, slot.id, index, { id: e.target.value })} aria-label="Point id" />
                  <span className="point-coord">
                    {p.lon.toFixed(5)}, {p.lat.toFixed(5)}
                  </span>
                  {slot.kinds && index > 0 && index < points.length - 1 && (
                    <select value={p.kind ?? "break"} onChange={(e) => updatePoint(tool.id, slot.id, index, { kind: e.target.value })} aria-label="Stop kind">
                      {slot.kinds.map((k) => (
                        <option key={k} value={k}>
                          {k}
                        </option>
                      ))}
                    </select>
                  )}
                  {slot.weighted && (
                    <input type="number" className="point-weight" step={0.5} min={0} value={p.weight ?? 1} onChange={(e) => updatePoint(tool.id, slot.id, index, { weight: Number(e.target.value) })} aria-label="Weight" title="Demand weight" />
                  )}
                  <button type="button" className="icon-btn" onClick={() => removePoint(tool.id, slot.id, index)} aria-label="Remove point">
                    ×
                  </button>
                </li>
              ))}
              {points.length > 60 && <li className="muted">and {points.length - 60} more</li>}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}

// --- Form ---

function FormFields({ tool, service }: { tool: ToolDef; service: ServiceInfo | null }) {
  const form = useStore((s) => s.form[tool.id]) ?? {};
  const merged = useMemo(() => ({ ...fieldDefaults(tool), ...form }), [tool, form]);
  const [openGroups, setOpenGroups] = useState<Record<string, boolean>>({});
  const groups: { name: string | undefined; fields: Field[] }[] = [];
  for (const field of tool.fields) {
    if (field.when && !field.when(merged)) continue;
    const last = groups[groups.length - 1];
    if (last && last.name === field.group) last.fields.push(field);
    else groups.push({ name: field.group, fields: [field] });
  }
  return (
    <div className="form">
      {groups.map((group, i) =>
        group.name ? (
          <details key={`${group.name}-${i}`} open={openGroups[group.name] ?? false} onToggle={(e) => setOpenGroups((g) => ({ ...g, [group.name!]: (e.target as HTMLDetailsElement).open }))}>
            <summary>{group.name}</summary>
            <div className="form-grid">
              {group.fields.map((f) => (
                <FieldControl key={f.key} tool={tool.id} field={f} value={merged[f.key]} service={service} />
              ))}
            </div>
          </details>
        ) : (
          <div className="form-grid" key={`ungrouped-${i}`}>
            {group.fields.map((f) => (
              <FieldControl key={f.key} tool={tool.id} field={f} value={merged[f.key]} service={service} />
            ))}
          </div>
        ),
      )}
    </div>
  );
}

function FieldControl({ tool, field, value, service }: { tool: ToolId; field: Field; value: unknown; service: ServiceInfo | null }) {
  const set = (v: unknown) => setFormValue(tool, field.key, v);
  if (field.type === "checkbox") {
    return (
      <label className="field field-check">
        <input type="checkbox" checked={value === true} onChange={(e) => set(e.target.checked)} />
        <span>{field.label}</span>
      </label>
    );
  }
  if (field.type === "select") {
    const options = typeof field.options === "function" ? field.options(service) : (field.options ?? []);
    return (
      <label className="field">
        <span>{field.label}</span>
        <select value={String(value ?? "")} onChange={(e) => set(e.target.value)}>
          {options.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
      </label>
    );
  }
  if (field.type === "json") {
    return (
      <label className="field field-wide">
        <span>{field.label}</span>
        <textarea rows={2} value={String(value ?? "")} onChange={(e) => set(e.target.value)} placeholder={field.placeholder} spellCheck={false} />
        {field.hint && <small className="muted">{field.hint}</small>}
      </label>
    );
  }
  return (
    <label className="field">
      <span>{field.label}</span>
      <input
        type={field.type === "number" ? "number" : "text"}
        value={value === undefined || value === null ? "" : String(value)}
        step={field.step}
        min={field.min}
        placeholder={field.placeholder}
        onChange={(e) => set(field.type === "number" ? (e.target.value === "" ? undefined : Number(e.target.value)) : e.target.value)}
      />
      {field.hint && <small className="muted">{field.hint}</small>}
    </label>
  );
}

// --- Request preview and run ---

function RunBar({ tool, service }: { tool: ToolDef; service: ServiceInfo | null }) {
  const running = useStore((s) => s.running);
  const format = useStore((s) => s.format);
  const apiBase = useStore((s) => s.apiBase);
  const raw = useStore((s) => s.raw[tool.id] ?? null);
  // Re-derive the preview when points or form values change.
  useStore((s) => s.points);
  useStore((s) => s.form[tool.id]);
  useStore((s) => s.profileId);
  useStore((s) => s.feedId);
  const [showJson, setShowJson] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const preview = currentBody(tool.id, service);
  const previewText = raw ?? (preview.body ? JSON.stringify(preview.body, null, 2) : "");

  const run = () => {
    void runTool(tool.id, service);
  };

  const copy = async (what: "json" | "curl") => {
    const text = what === "json" ? previewText : toCurl(apiBase, tool.supportsGeojson && format === "geojson" ? `${tool.path}?format=geojson` : tool.path, preview.body);
    if (await copyText(text)) {
      setCopied(what);
      setTimeout(() => setCopied(null), 1200);
    }
  };

  return (
    <div className="run-bar">
      <div className="run-row">
        <button type="button" className="btn btn-primary" onClick={run} disabled={running || !!preview.error}>
          {running ? "Running…" : `Run ${tool.label.toLowerCase()}`}
        </button>
        {tool.supportsGeojson && (
          <div className="segmented" role="group" aria-label="Response format">
            <button type="button" className={format === "json" ? "is-active" : ""} onClick={() => setState({ format: "json" })}>
              JSON
            </button>
            <button type="button" className={format === "geojson" ? "is-active" : ""} onClick={() => setState({ format: "geojson" })}>
              GeoJSON
            </button>
          </div>
        )}
        <button type="button" className={`btn btn-ghost btn-small${showJson ? " is-active" : ""}`} onClick={() => setShowJson((v) => !v)}>
          Request
        </button>
      </div>
      {preview.error && <div className="hint-text">{preview.error}</div>}
      {showJson && (
        <div className="json-editor">
          <textarea
            value={previewText}
            spellCheck={false}
            onChange={(e) => setState((prev) => ({ raw: { ...prev.raw, [tool.id]: e.target.value } }))}
            rows={12}
            aria-label="Request JSON"
          />
          <div className="json-editor-actions">
            {raw !== null ? (
              <span className="pill pill-warn">edited by hand</span>
            ) : (
              <span className="muted">Editing here overrides the form until reset.</span>
            )}
            <span className="spacer" />
            <button type="button" className="btn btn-ghost btn-small" onClick={() => copy("json")}>
              {copied === "json" ? "Copied" : "Copy JSON"}
            </button>
            <button type="button" className="btn btn-ghost btn-small" onClick={() => copy("curl")}>
              {copied === "curl" ? "Copied" : "Copy curl"}
            </button>
            <button
              type="button"
              className="btn btn-ghost btn-small"
              onClick={() => {
                resetForm(tool.id);
              }}
            >
              Reset
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

export function useFormValues(tool: ToolId) {
  const form = useStore((s) => s.form[tool]);
  return form ?? getForm(tool);
}
