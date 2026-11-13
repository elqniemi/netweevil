import { useEffect, useState } from "react";
import { ApiError, apiRequest } from "../api/client";
import { extractFeatures, styleFeatures } from "../geo/features";
import { flyTo, getForm, setFormValue, setState, useStore, type ToolId } from "../state/store";
import { INDOOR_LEVEL_KEY, INDOOR_SITE_KEY } from "../tools/registry";

interface SiteSummary {
  site_id: string;
  label: string;
  levels: string[];
  bbox: [number, number, number, number];
}

const INDOOR_TOOLS: ToolId[] = ["indoor_route", "door_to_door"];
const LAYER_PREFIX = "indoor:";

const FEATURE_COLORS: Record<string, string> = {
  surface: "#868e96",
  barrier: "#343a40",
  edge: "#495057",
  portal: "#e8590c",
  vertical: "#7048e8",
  node: "#495057",
  poi: "#1971c2",
  entrance: "#2b8a3e",
};

function setAll(key: string, value: unknown) {
  for (const tool of INDOOR_TOOLS) setFormValue(tool, key, value);
}

/** Site and level picker for the indoor tools; shows the level on the map. */
export function IndoorSitePanel({ tool }: { tool: ToolId }) {
  const apiBase = useStore((s) => s.apiBase);
  const form = useStore((s) => s.form[tool]);
  const [sites, setSites] = useState<SiteSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const siteId = typeof form?.[INDOOR_SITE_KEY] === "string" ? (form[INDOOR_SITE_KEY] as string) : "";
  const level = typeof form?.[INDOOR_LEVEL_KEY] === "string" ? (form[INDOOR_LEVEL_KEY] as string) : "";
  const site = sites?.find((s) => s.site_id === siteId) ?? null;

  useEffect(() => {
    let cancelled = false;
    apiRequest<SiteSummary[]>(apiBase, "/v1/indoor/sites", { tool })
      .then((response) => {
        if (cancelled) return;
        setSites(response.data);
        const current = getForm(tool)[INDOOR_SITE_KEY];
        if (!current && response.data.length) choose(response.data[0], false);
      })
      .catch((e) => !cancelled && setError(e instanceof ApiError ? e.message : String(e)));
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [apiBase]);

  useEffect(() => {
    if (!siteId || !level) return;
    let cancelled = false;
    const path = `/v1/indoor/sites/${encodeURIComponent(siteId)}/geojson?level=${encodeURIComponent(level)}`;
    apiRequest(apiBase, path, { tool })
      .then((response) => {
        if (cancelled) return;
        const features = styleFeatures(extractFeatures(response.data), { tool: "network" });
        for (const f of features.features) {
          const type = String(f.properties.feature_type ?? "");
          f.properties._reference = true;
          f.properties._color = FEATURE_COLORS[type] ?? "#868e96";
          f.properties._radius = type === "entrance" ? 6 : 4;
        }
        const name = `${LAYER_PREFIX}${siteId} level ${level}`;
        setState((s) => ({ referenceLayers: [...s.referenceLayers.filter((l) => !l.name.startsWith(LAYER_PREFIX)), { name, features }] }));
        setError(null);
      })
      .catch((e) => !cancelled && setError(e instanceof ApiError ? e.message : String(e)));
    return () => {
      cancelled = true;
    };
  }, [apiBase, siteId, level, tool]);

  function choose(next: SiteSummary, fly = true) {
    setAll(INDOOR_SITE_KEY, next.site_id);
    setAll(INDOOR_LEVEL_KEY, next.levels[0] ?? "");
    if (fly) {
      const [w, s, e, n] = next.bbox;
      flyTo((w + e) / 2, (s + n) / 2, 18);
    }
  }

  if (error && !sites) return <p className="style-note">{error}</p>;
  if (!sites) return null;
  if (!sites.length) return <p className="style-note">No indoor sites. Import one with netweevil indoor import.</p>;
  return (
    <div className="picker-row">
      <label className="field">
        <span>Site</span>
        <select value={siteId} onChange={(e) => { const next = sites.find((s) => s.site_id === e.target.value); if (next) choose(next); }}>
          {sites.map((s) => (
            <option key={s.site_id} value={s.site_id}>
              {s.label}
            </option>
          ))}
        </select>
      </label>
      {site && (
        <div className="field">
          <span>Level</span>
          <div className="chip-row">
            {site.levels.map((l) => (
              <button key={l} type="button" className={`chip${l === level ? " is-active" : ""}`} onClick={() => setAll(INDOOR_LEVEL_KEY, l)} title={`Show level ${l}`}>
                {l}
              </button>
            ))}
          </div>
        </div>
      )}
      {error && <small className="muted">{error}</small>}
    </div>
  );
}
