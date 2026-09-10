import { useState } from "react";
import type { ServiceInfo } from "../api/types";
import { BASEMAP_LABELS } from "../map/basemaps";
import { setState, useStore, type Basemap } from "../state/store";
import { fmtNumber } from "./format";
import { PlaceSearch } from "./PlaceSearch";

interface Props {
  service: ServiceInfo | null;
  error: string | null;
  loading: boolean;
  /** API is up but no dataset is loaded yet. */
  setupNeeded?: boolean;
}

export function Header({ service, error, loading, setupNeeded }: Props) {
  const apiBase = useStore((s) => s.apiBase);
  const basemap = useStore((s) => s.basemap);
  const [editingBase, setEditingBase] = useState(false);
  const [draftBase, setDraftBase] = useState(apiBase);
  const [showInfo, setShowInfo] = useState(false);

  const status = loading ? "connecting" : setupNeeded ? "setup" : error ? "offline" : "online";

  return (
    <header className="header">
      <div className="brand">
        <span className="brand-mark" aria-hidden="true" />
        <span className="brand-name">NetWeevil console</span>
      </div>
      <button type="button" className={`status status-${status}`} onClick={() => (status === "setup" ? setState({ setupOpen: true }) : setShowInfo((v) => !v))} title={status === "setup" ? "Open Setup" : "Show dataset details"}>
        <span className="status-dot" />
        {status === "online" && service ? service.dataset.dataset_id : status === "offline" ? "API unreachable" : status === "setup" ? "No dataset loaded – open Setup" : "Connecting"}
      </button>
      <PlaceSearch service={service} />
      <div className="header-spacer" />
      <div className="segmented" role="group" aria-label="Basemap">
        {(Object.keys(BASEMAP_LABELS) as Basemap[]).map((id) => (
          <button key={id} type="button" className={basemap === id ? "is-active" : ""} onClick={() => setState({ basemap: id })}>
            {BASEMAP_LABELS[id]}
          </button>
        ))}
      </div>
      {editingBase ? (
        <form
          className="api-base-form"
          onSubmit={(e) => {
            e.preventDefault();
            setState({ apiBase: draftBase.trim() });
            setEditingBase(false);
          }}
        >
          <input value={draftBase} onChange={(e) => setDraftBase(e.target.value)} placeholder="same origin (dev proxy)" aria-label="API base URL" />
          <button type="submit" className="btn btn-small">Use</button>
        </form>
      ) : (
        <button type="button" className="btn btn-ghost btn-small" onClick={() => setEditingBase(true)} title="Change the API base URL">
          {apiBase || "API: same origin"}
        </button>
      )}
      {showInfo && (
        <div className="popover service-popover">
          {error && <p className="error-text">{error}</p>}
          {service && (
            <dl className="kv">
              <dt>Dataset</dt>
              <dd>{service.dataset.dataset_id}</dd>
              <dt>Source</dt>
              <dd>{service.dataset.source_path}</dd>
              <dt>Nodes / edges / turns</dt>
              <dd>
                {fmtNumber(service.dataset.node_count ?? 0)} / {fmtNumber(service.dataset.edge_count ?? 0)} / {fmtNumber(service.dataset.turn_count ?? 0)}
              </dd>
              <dt>Components</dt>
              <dd>
                {fmtNumber(service.dataset.connected_components?.component_count ?? 0)} (largest {fmtNumber(service.dataset.connected_components?.largest_component_node_count ?? 0)} nodes)
              </dd>
              <dt>Engine</dt>
              <dd>
                {service.engine.route_engine} / {service.engine.batch_engine}
                <br />
                {service.engine.acceleration}
              </dd>
              <dt>Profiles</dt>
              <dd>
                {service.loaded_profiles.map((p) => (
                  <div key={p.profile_id}>
                    {p.profile_id} <span className="muted">({p.mode}, {p.profile_hash.slice(0, 8)})</span>
                  </div>
                ))}
              </dd>
              <dt>Transit feeds</dt>
              <dd>
                {service.loaded_transit_feeds.length === 0 && <span className="muted">none loaded</span>}
                {service.loaded_transit_feeds.map((f) => (
                  <div key={f.feed_id}>
                    {f.feed_id} <span className="muted">({fmtNumber(f.stop_count)} stops, {fmtNumber(f.trip_count)} trips, {f.agency_timezone})</span>
                  </div>
                ))}
              </dd>
              <dt>Analyses</dt>
              <dd className="muted">{service.capabilities.analyses.join(", ")}</dd>
            </dl>
          )}
        </div>
      )}
    </header>
  );
}
