import { useEffect, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ApiError, apiRequest, joinUrl } from "../api/client";
import type { JobRecord, ProfileFileInfo, ProfileTemplate, UploadInfo, WorkspaceInfo } from "../api/types";
import { getState, pushTiming, setState, useStore } from "../state/store";
import { fmtNumber } from "./format";

type Step = "where" | "network" | "profiles" | "transit" | "load";

const STEPS: { id: Step; label: string; blurb: string }[] = [
  { id: "where", label: "1. Where things live", blurb: "Folders NetWeevil writes to" },
  { id: "network", label: "2. Street network", blurb: "OSM or Overture extract" },
  { id: "profiles", label: "3. Routing profiles", blurb: "Car, bicycle, walking" },
  { id: "transit", label: "4. Transit (optional)", blurb: "GTFS feeds" },
  { id: "load", label: "5. Load and go", blurb: "Pick what the API serves" },
];

async function call<T>(path: string, options: { method?: "GET" | "POST" | "DELETE"; body?: unknown } = {}): Promise<T> {
  const base = getState().apiBase;
  try {
    const response = await apiRequest<T>(base, path, { tool: "setup", ...options });
    pushTiming(response.timing);
    return response.data;
  } catch (error) {
    if (error instanceof ApiError) pushTiming(error.timing);
    throw error;
  }
}

/** Streams a file to the API upload endpoint with progress (XHR exposes upload progress; fetch does not). */
function uploadFile(file: File, name: string, onProgress: (fraction: number) => void): Promise<UploadInfo> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    const url = joinUrl(getState().apiBase, `/v1/workspace/uploads?name=${encodeURIComponent(name)}&overwrite=true`);
    xhr.open("POST", url);
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable) onProgress(e.loaded / e.total);
    };
    xhr.onload = () => {
      try {
        const body = JSON.parse(xhr.responseText || "null") as UploadInfo | { error?: string } | null;
        if (xhr.status >= 200 && xhr.status < 300 && body && "name" in body) resolve(body);
        else reject(new Error((body as { error?: string } | null)?.error ?? `Upload failed (HTTP ${xhr.status})`));
      } catch {
        reject(new Error(`Upload failed (HTTP ${xhr.status})`));
      }
    };
    xhr.onerror = () => reject(new Error("Upload failed: network error"));
    xhr.send(file);
  });
}

export function useWorkspace() {
  const apiBase = useStore((s) => s.apiBase);
  return useQuery({
    queryKey: ["workspace", apiBase],
    queryFn: () => call<WorkspaceInfo>("/v1/workspace"),
    // Poll while the panel is open: imports, builds and activations started
    // elsewhere (CLI, another tab) show up without a reload.
    refetchInterval: (query) => (query.state.data?.jobs.some((j) => j.state === "running") ? 2000 : 6000),
    retry: 1,
  });
}

/** Polls one job until it finishes; the callback fires once on completion. */
export function useJob(jobId: string | null, onDone?: (job: JobRecord) => void) {
  const apiBase = useStore((s) => s.apiBase);
  const query = useQuery({
    queryKey: ["job", apiBase, jobId],
    queryFn: () => call<JobRecord>(`/v1/workspace/jobs/${encodeURIComponent(jobId!)}`),
    enabled: !!jobId,
    refetchInterval: (q) => (q.state.data && q.state.data.state !== "running" ? false : 1000),
  });
  const [notified, setNotified] = useState<string | null>(null);
  useEffect(() => {
    const job = query.data;
    if (job && job.state !== "running" && notified !== job.job_id) {
      setNotified(job.job_id);
      onDone?.(job);
    }
  }, [query.data, notified, onDone]);
  return query;
}

export function JobProgress({ job }: { job: JobRecord | undefined }) {
  if (!job) return null;
  const pct = job.percent ?? (job.state === "running" ? null : 100);
  return (
    <div className={`job job-${job.state}`}>
      <div className="job-head">
        <span className={`pill ${job.state === "done" ? "pill-ok" : job.state === "failed" ? "pill-error" : "pill-warn"}`}>{job.state}</span>
        <span className="job-label">{job.label}</span>
        <span className="muted">{job.stage}</span>
      </div>
      {job.state === "running" && (
        <div className="sim-progress">
          <div className="sim-progress-bar" style={{ width: `${pct ?? 30}%`, opacity: pct === null ? 0.5 : 1 }} />
        </div>
      )}
      {job.message && job.state !== "failed" && <div className="muted job-message">{job.message}</div>}
      {job.error && <div className="error-text">{job.error}</div>}
    </div>
  );
}

export function SetupPanel() {
  const open = useStore((s) => s.setupOpen);
  const workspace = useWorkspace();
  const [step, setStep] = useState<Step>("where");
  const info = workspace.data ?? null;
  useEffect(() => {
    // Land on the first step that still needs something.
    if (!open || !info) return;
    if (!info.datasets.length) setStep((s) => (s === "where" ? "where" : s));
  }, [open, info]);
  if (!open) return null;
  const close = () => setState({ setupOpen: false });
  return (
    <div className="setup-overlay" role="dialog" aria-label="Setup">
      <div className="setup-panel">
        <aside className="setup-nav">
          <div className="setup-nav-head">
            <strong>Setup</strong>
            <button type="button" className="icon-btn" onClick={close} title="Close setup" aria-label="Close setup">
              ×
            </button>
          </div>
          {STEPS.map((s) => (
            <button key={s.id} type="button" className={`setup-step${step === s.id ? " is-active" : ""}`} onClick={() => setStep(s.id)}>
              <span>{s.label}</span>
              <small className="muted">{s.blurb}</small>
              <StepBadge step={s.id} info={info} />
            </button>
          ))}
          <div className="setup-nav-foot muted">
            {info ? (
              info.loaded ? (
                <span>Loaded: dataset {info.active?.dataset_id}</span>
              ) : (
                <span>Nothing loaded yet. Finish step 5 to start routing.</span>
              )
            ) : workspace.error ? (
              <span className="error-text">API unreachable: {(workspace.error as Error).message}</span>
            ) : (
              <span>Connecting…</span>
            )}
          </div>
        </aside>
        <section className="setup-body">
          {step === "where" && <WhereStep info={info} />}
          {step === "network" && <NetworkStep info={info} />}
          {step === "profiles" && <ProfilesStep info={info} />}
          {step === "transit" && <TransitStep info={info} />}
          {step === "load" && <LoadStep info={info} onDone={close} />}
        </section>
      </div>
    </div>
  );
}

function StepBadge({ step, info }: { step: Step; info: WorkspaceInfo | null }) {
  if (!info) return null;
  const ok =
    step === "where" ? true : step === "network" ? info.datasets.length > 0 : step === "profiles" ? info.profiles.some((p) => !p.error) : step === "transit" ? info.transit_feeds.length > 0 : info.loaded;
  const optional = step === "transit";
  return <span className={`setup-badge${ok ? " is-ok" : optional ? " is-optional" : ""}`}>{ok ? "✓" : optional ? "opt" : "·"}</span>;
}

// --- Step 1: where things live ---

function WhereStep({ info }: { info: WorkspaceInfo | null }) {
  return (
    <div className="setup-step-body">
      <h2>Where your data lives</h2>
      <p>
        NetWeevil keeps everything it creates in one folder, <code>.netweevil/</code>, inside the workspace the API was started in. Your source files
        (OSM extracts, GTFS zips) are never modified: uploads are copied into the folder, and files you point to by path are read in place.
      </p>
      {info && (
        <>
          <dl className="kv setup-kv">
            <dt>Workspace</dt>
            <dd>
              <code>{info.root}</code>
            </dd>
            <dt>State folder</dt>
            <dd>
              <code>{info.state_dir}</code>
            </dd>
            <dt>Saved selection</dt>
            <dd>
              <code>{info.config_path}</code>
              <div className="muted">Written when you load a selection in step 5; a plain `netweevil bootstrap` or `netweevil api serve` reloads it.</div>
            </dd>
          </dl>
          <table className="setup-table">
            <thead>
              <tr>
                <th>Folder</th>
                <th>What goes there</th>
              </tr>
            </thead>
            <tbody>
              {info.locations.slice(1).map((loc) => (
                <tr key={loc.key}>
                  <td>
                    <code>{loc.path.replace(info.state_dir, ".netweevil")}</code>
                  </td>
                  <td>{loc.purpose}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <p className="muted">
            To move a workspace, copy the whole <code>.netweevil/</code> folder. To start over, delete it. Nothing outside it changes.
          </p>
        </>
      )}
    </div>
  );
}

// --- Shared: source chooser (upload or path) ---

function SourceChooser({ info, accept, kinds, value, onChange }: { info: WorkspaceInfo | null; accept: string; kinds: UploadInfo["kind"][]; value: string; onChange: (v: string) => void }) {
  const queryClient = useQueryClient();
  const [progress, setProgress] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const uploads = (info?.uploads ?? []).filter((u) => kinds.includes(u.kind));
  const onFile = async (file: File | undefined) => {
    if (!file) return;
    setError(null);
    setProgress(0);
    try {
      const stored = await uploadFile(file, file.name, setProgress);
      onChange(stored.name);
      void queryClient.invalidateQueries({ queryKey: ["workspace"] });
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setProgress(null);
    }
  };
  return (
    <div className="source-chooser">
      <label className="field">
        <span>Upload a file</span>
        <input type="file" accept={accept} onChange={(e) => void onFile(e.target.files?.[0])} disabled={progress !== null} />
        {progress !== null && (
          <div className="sim-progress">
            <div className="sim-progress-bar" style={{ width: `${Math.round(progress * 100)}%` }} />
          </div>
        )}
        <small className="muted">Copied into `.netweevil/uploads/`. Large extracts take a while; the page stays usable.</small>
      </label>
      <label className="field">
        <span>Or a path on the machine running the API</span>
        <input type="text" value={value} onChange={(e) => onChange(e.target.value)} placeholder="/data/extract.osm.pbf or datasets/city.osm.pbf" />
        <small className="muted">Read in place, not copied. Relative paths resolve from the workspace folder.</small>
      </label>
      {uploads.length > 0 && (
        <label className="field">
          <span>Or a previous upload</span>
          <select value={uploads.some((u) => u.name === value) ? value : ""} onChange={(e) => e.target.value && onChange(e.target.value)}>
            <option value="">choose…</option>
            {uploads.map((u) => (
              <option key={u.name} value={u.name}>
                {u.name} ({fmtBytes(u.size_bytes)})
              </option>
            ))}
          </select>
        </label>
      )}
      {error && <div className="error-text">{error}</div>}
    </div>
  );
}

function fmtBytes(bytes: number): string {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(2)} GB`;
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(1)} MB`;
  if (bytes >= 1e3) return `${(bytes / 1e3).toFixed(0)} kB`;
  return `${bytes} B`;
}

function slug(text: string): string {
  return text
    .toLowerCase()
    .replace(/\.(osm\.pbf|pbf|parquet|geoparquet|gpkg|zip)$/i, "")
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .slice(0, 48);
}

// --- Step 2: street network ---

function NetworkStep({ info }: { info: WorkspaceInfo | null }) {
  const queryClient = useQueryClient();
  const [source, setSource] = useState("");
  const [name, setName] = useState("");
  const [format, setFormat] = useState("");
  const [jobId, setJobId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const job = useJob(jobId, () => void queryClient.invalidateQueries({ queryKey: ["workspace"] }));
  useEffect(() => {
    if (source && !name) setName(slug(source.split(/[\\/]/).pop() ?? source));
  }, [source, name]);
  const running = job.data?.state === "running" || (info?.jobs ?? []).some((j) => j.kind === "dataset-import" && j.state === "running");
  const start = async () => {
    setError(null);
    try {
      const started = await call<{ job_id: string }>("/v1/workspace/datasets/import", { body: { sources: [source], name, format: format || null } });
      setJobId(started.job_id);
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const remove = async (datasetId: string) => {
    if (!window.confirm(`Delete dataset '${datasetId}' and its derived bundles? The source file is kept.`)) return;
    try {
      await call(`/v1/workspace/datasets/${encodeURIComponent(datasetId)}`, { method: "DELETE" });
      void queryClient.invalidateQueries({ queryKey: ["workspace"] });
    } catch (e) {
      setError((e as Error).message);
    }
  };
  return (
    <div className="setup-step-body">
      <h2>Street network</h2>
      <p>
        Import an OpenStreetMap extract (<code>.osm.pbf</code>) or Overture Maps transportation segments (<code>.parquet</code>). Import reads the
        file once and writes a compact routing graph plus acceleration data under <code>.netweevil/bundles/</code>. A regional extract takes a few
        minutes; a whole country can take an hour.
      </p>
      <SourceChooser info={info} accept=".pbf,.osm.pbf,.parquet,.geoparquet,.gpkg" kinds={["osm", "overture", "gpkg"]} value={source} onChange={setSource} />
      <div className="form-grid">
        <label className="field">
          <span>Dataset name</span>
          <input type="text" value={name} onChange={(e) => setName(e.target.value.replace(/[^A-Za-z0-9_-]/g, "_"))} placeholder="groningen_2026" />
        </label>
        <label className="field">
          <span>Format</span>
          <select value={format} onChange={(e) => setFormat(e.target.value)}>
            <option value="">detect from file name</option>
            <option value="osm_pbf">OSM PBF</option>
            <option value="overture_parquet">Overture GeoParquet</option>
            <option value="geo_package">GeoPackage (needs a mapping; use the CLI)</option>
          </select>
        </label>
      </div>
      <div className="run-row">
        <button type="button" className="btn btn-primary" onClick={() => void start()} disabled={!source || !name || running}>
          {running ? "Importing…" : "Import network"}
        </button>
        {error && <span className="error-text">{error}</span>}
      </div>
      <JobProgress job={job.data ?? (info?.jobs ?? []).find((j) => j.kind === "dataset-import" && j.state === "running")} />
      <h3>Imported networks</h3>
      {info && info.datasets.length === 0 && <p className="muted">None yet.</p>}
      {info && info.datasets.length > 0 && (
        <table className="setup-table">
          <thead>
            <tr>
              <th>Dataset</th>
              <th>Source</th>
              <th>Size</th>
              <th>Profiles compiled</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {info.datasets.map((d) => (
              <tr key={d.dataset_id} className={d.active ? "is-active" : ""}>
                <td>
                  <strong>{d.dataset_id}</strong>
                  {d.active && <span className="pill pill-ok">loaded</span>}
                  <div className="muted">{d.source_format}, imported {d.imported_at.slice(0, 10)}</div>
                </td>
                <td>
                  <code className="path">{d.source_path}</code>
                </td>
                <td className="num">
                  {fmtNumber(d.node_count ?? 0)} nodes
                  <br />
                  {fmtNumber(d.edge_count ?? 0)} edges
                </td>
                <td>{d.compiled_profile_ids.length ? d.compiled_profile_ids.join(", ") : <span className="muted">none yet</span>}</td>
                <td>
                  <button type="button" className="btn btn-ghost btn-small" onClick={() => void remove(d.dataset_id)} disabled={d.active} title={d.active ? "Loaded datasets cannot be deleted" : "Delete dataset and derived bundles"}>
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

// --- Step 3: profiles ---

function ProfilesStep({ info }: { info: WorkspaceInfo | null }) {
  const queryClient = useQueryClient();
  const apiBase = useStore((s) => s.apiBase);
  const templates = useQuery({ queryKey: ["profile-templates", apiBase], queryFn: () => call<ProfileTemplate[]>("/v1/workspace/profile-templates"), staleTime: Infinity });
  const [templateId, setTemplateId] = useState<string>("car");
  const [yaml, setYaml] = useState<string>("");
  const [fileName, setFileName] = useState<string>("");
  const [editing, setEditing] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const template = templates.data?.find((t) => t.template_id === templateId) ?? templates.data?.[0];
  useEffect(() => {
    if (template && !editing) {
      setYaml(template.yaml);
      setFileName("");
    }
  }, [template, editing]);
  const idFromYaml = useMemo(() => /^\s*id:\s*([A-Za-z0-9_.-]+)/m.exec(yaml)?.[1] ?? "", [yaml]);
  const save = async (overwrite = false) => {
    setError(null);
    setMessage(null);
    try {
      const saved = await call<ProfileFileInfo>("/v1/workspace/profiles", { body: { yaml, file_name: fileName || null, overwrite } });
      setMessage(`Saved ${saved.path}`);
      setEditing(false);
      void queryClient.invalidateQueries({ queryKey: ["workspace"] });
    } catch (e) {
      const err = e as ApiError;
      if (err.status === 409 && window.confirm(`${err.message}\n\nReplace it?`)) return save(true);
      setError(err.message);
    }
  };
  const remove = async (p: ProfileFileInfo) => {
    if (!window.confirm(`Delete ${p.file_name}?`)) return;
    try {
      await call(`/v1/workspace/profiles/${encodeURIComponent(p.file_name)}`, { method: "DELETE" });
      void queryClient.invalidateQueries({ queryKey: ["workspace"] });
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const load = async (p: ProfileFileInfo) => {
    try {
      const name = p.source === "examples" ? `examples/${p.file_name}` : p.file_name;
      const content = await call<{ yaml: string }>(`/v1/workspace/profiles/${encodeURIComponent(name)}`);
      setYaml(content.yaml);
      setFileName(p.source === "examples" ? p.file_name : p.file_name);
      setEditing(true);
      setMessage(p.source === "examples" ? "Loaded an example; saving writes a copy to .netweevil/profiles/." : `Editing ${p.path}`);
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const workspaceProfiles = (info?.profiles ?? []).filter((p) => p.source === "workspace");
  const exampleProfiles = (info?.profiles ?? []).filter((p) => p.source === "examples");
  return (
    <div className="setup-step-body">
      <h2>Routing profiles</h2>
      <p>
        A profile is a small YAML file that says how a mode moves through the network: speeds per road class, what is excluded, turn penalties and
        preferences. Start from a default, adjust if you like, and save. Profiles are compiled against a dataset when you load them in step 5
        (a minute or so for a region), and recompiled automatically when the file changes. Saved profiles live in <code>.netweevil/profiles/</code>.
      </p>
      <div className="profile-editor">
        <div className="form-grid">
          <label className="field">
            <span>Start from</span>
            <select
              value={templateId}
              onChange={(e) => {
                setTemplateId(e.target.value);
                setEditing(false);
              }}
            >
              {(templates.data ?? []).map((t) => (
                <option key={t.template_id} value={t.template_id}>
                  {t.label}
                </option>
              ))}
            </select>
            {template && <small className="muted">{template.description}</small>}
          </label>
          <label className="field">
            <span>File name</span>
            <input type="text" value={fileName} onChange={(e) => setFileName(e.target.value)} placeholder={idFromYaml ? `${idFromYaml}.yml` : "profile.yml"} />
            <small className="muted">Saved as `.netweevil/profiles/{fileName || (idFromYaml ? `${idFromYaml}.yml` : "…")}`</small>
          </label>
        </div>
        <textarea
          className="profile-yaml"
          value={yaml}
          spellCheck={false}
          rows={16}
          onChange={(e) => {
            setYaml(e.target.value);
            setEditing(true);
          }}
          aria-label="Profile YAML"
        />
        <div className="run-row">
          <button type="button" className="btn btn-primary" onClick={() => void save()} disabled={!yaml.trim()}>
            Save profile
          </button>
          {editing && (
            <button type="button" className="btn btn-ghost btn-small" onClick={() => setEditing(false)}>
              Reset to template
            </button>
          )}
          {message && <span className="muted">{message}</span>}
          {error && <span className="error-text">{error}</span>}
        </div>
      </div>
      <h3>Your profiles</h3>
      <ProfileTable profiles={workspaceProfiles} onLoad={load} onRemove={remove} empty="None yet. Save one above; the three defaults (car, bicycle, pedestrian) cover most uses." />
      {exampleProfiles.length > 0 && (
        <>
          <h3>Repository examples</h3>
          <p className="muted">
            Read-only files from <code>{info?.example_profiles_dir}</code>. They can be loaded directly in step 5, or opened here and saved as your own copy.
          </p>
          <ProfileTable profiles={exampleProfiles} onLoad={load} empty="" />
        </>
      )}
    </div>
  );
}

function ProfileTable({ profiles, onLoad, onRemove, empty }: { profiles: ProfileFileInfo[]; onLoad: (p: ProfileFileInfo) => void; onRemove?: (p: ProfileFileInfo) => void; empty: string }) {
  if (!profiles.length) return empty ? <p className="muted">{empty}</p> : null;
  return (
    <table className="setup-table">
      <thead>
        <tr>
          <th>Profile</th>
          <th>Mode</th>
          <th>File</th>
          <th />
        </tr>
      </thead>
      <tbody>
        {profiles.map((p) => (
          <tr key={p.path} className={p.active ? "is-active" : ""}>
            <td>
              {p.profile_id ? <strong>{p.profile_id}</strong> : <span className="error-text">invalid</span>}
              {p.is_default && <span className="pill pill-ok">default</span>}
              {p.active && !p.is_default && <span className="pill pill-ok">loaded</span>}
              <div className="muted">{p.label ?? p.error}</div>
            </td>
            <td>{p.mode ?? "–"}</td>
            <td>
              <code className="path">{p.path}</code>
            </td>
            <td>
              <button type="button" className="btn btn-ghost btn-small" onClick={() => onLoad(p)}>
                Open
              </button>
              {onRemove && (
                <button type="button" className="btn btn-ghost btn-small" onClick={() => onRemove(p)} disabled={p.active}>
                  Delete
                </button>
              )}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

// --- Step 4: transit ---

function TransitStep({ info }: { info: WorkspaceInfo | null }) {
  const queryClient = useQueryClient();
  const [source, setSource] = useState("");
  const [name, setName] = useState("");
  const [start, setStart] = useState(() => new Date().toISOString().slice(0, 10));
  const [days, setDays] = useState(7);
  const [jobId, setJobId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const job = useJob(jobId, () => void queryClient.invalidateQueries({ queryKey: ["workspace"] }));
  useEffect(() => {
    if (source && !name) setName(slug(source.split(/[\\/]/).pop() ?? source));
  }, [source, name]);
  const running = job.data?.state === "running";
  const begin = async () => {
    setError(null);
    try {
      const started = await call<{ job_id: string }>("/v1/workspace/transit/import", { body: { source, name, service_start: start, service_days: days } });
      setJobId(started.job_id);
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const remove = async (feedId: string) => {
    if (!window.confirm(`Delete feed '${feedId}'? The GTFS source file is kept; the feed is unloaded if it is in use.`)) return;
    try {
      await call(`/v1/workspace/transit-feeds/${encodeURIComponent(feedId)}`, { method: "DELETE" });
      void queryClient.invalidateQueries({ queryKey: ["workspace"] });
      void queryClient.invalidateQueries({ queryKey: ["service"] });
    } catch (e) {
      setError((e as Error).message);
    }
  };
  return (
    <div className="setup-step-body">
      <h2>Transit feeds (optional)</h2>
      <p>
        Import a GTFS zip to route with public transport and to edit lines in the GTFS editor. The import expands the timetable for a window of days
        starting on the date you give (pick a date the feed covers). The zip itself is not changed.
      </p>
      <SourceChooser info={info} accept=".zip" kinds={["gtfs"]} value={source} onChange={setSource} />
      <div className="form-grid">
        <label className="field">
          <span>Feed name</span>
          <input type="text" value={name} onChange={(e) => setName(e.target.value.replace(/[^A-Za-z0-9_-]/g, "_"))} placeholder="gtfs_nl_2026" />
        </label>
        <label className="field">
          <span>First service date</span>
          <input type="date" value={start} onChange={(e) => setStart(e.target.value)} />
        </label>
        <label className="field">
          <span>Days of service to expand</span>
          <input type="number" min={1} max={31} value={days} onChange={(e) => setDays(Number(e.target.value))} />
          <small className="muted">Seven days covers a weekly pattern; more days cost memory.</small>
        </label>
      </div>
      <div className="run-row">
        <button type="button" className="btn btn-primary" onClick={() => void begin()} disabled={!source || !name || !start || running}>
          {running ? "Importing…" : "Import GTFS feed"}
        </button>
        {error && <span className="error-text">{error}</span>}
      </div>
      <JobProgress job={job.data} />
      <h3>Imported feeds</h3>
      {info && info.transit_feeds.length === 0 && <p className="muted">None yet.</p>}
      {info && info.transit_feeds.length > 0 && (
        <table className="setup-table">
          <thead>
            <tr>
              <th>Feed</th>
              <th>Window</th>
              <th>Size</th>
              <th>Source</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {info.transit_feeds.map((f) => (
              <tr key={f.feed_id} className={f.loaded ? "is-active" : ""}>
                <td>
                  <strong>{f.feed_id}</strong>
                  {f.loaded && <span className="pill pill-ok">loaded</span>}
                  {f.scenario_id && <span className="pill pill-warn">editor scenario {f.scenario_id}</span>}
                </td>
                <td>
                  {f.service_start_date} + {f.service_days} d<div className="muted">{f.agency_timezone}</div>
                </td>
                <td className="num">
                  {fmtNumber(f.stop_count)} stops
                  <br />
                  {fmtNumber(f.trip_count)} trips
                </td>
                <td>
                  <code className="path">{f.source_path}</code>
                </td>
                <td>
                  <button type="button" className="btn btn-ghost btn-small" onClick={() => void remove(f.feed_id)}>
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

// --- Step 5: load ---

function LoadStep({ info, onDone }: { info: WorkspaceInfo | null; onDone: () => void }) {
  const queryClient = useQueryClient();
  const current = info?.active ?? info?.saved ?? null;
  const [datasetId, setDatasetId] = useState<string>("");
  const [defaultProfile, setDefaultProfile] = useState<string>("");
  const [extra, setExtra] = useState<string[]>([]);
  const [feeds, setFeeds] = useState<string[]>([]);
  const [jobId, setJobId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [seeded, setSeeded] = useState(false);
  const valid = (info?.profiles ?? []).filter((p) => !p.error);
  useEffect(() => {
    if (!info || seeded) return;
    setSeeded(true);
    setDatasetId(current?.dataset_id ?? info.datasets[0]?.dataset_id ?? "");
    setDefaultProfile(current?.default_profile ?? valid.find((p) => p.source === "workspace")?.path ?? valid[0]?.path ?? "");
    setExtra(current?.profiles ?? []);
    setFeeds(current?.transit_feeds ?? []);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [info, seeded]);
  const job = useJob(jobId, (finished) => {
    void queryClient.invalidateQueries({ queryKey: ["workspace"] });
    void queryClient.invalidateQueries({ queryKey: ["service"] });
    if (finished.state === "done") setTimeout(onDone, 800);
  });
  const running = job.data?.state === "running" || (info?.jobs ?? []).some((j) => j.kind === "activate" && j.state === "running");
  const activate = async () => {
    setError(null);
    try {
      const started = await call<{ job_id: string }>("/v1/workspace/activate", {
        body: { dataset_id: datasetId, default_profile: defaultProfile, profiles: extra.filter((p) => p !== defaultProfile), transit_feeds: feeds },
      });
      setJobId(started.job_id);
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const toggle = (list: string[], set: (v: string[]) => void, value: string) => set(list.includes(value) ? list.filter((v) => v !== value) : [...list, value]);
  const compiledFor = (path: string) => {
    const profile = valid.find((p) => p.path === path);
    const dataset = info?.datasets.find((d) => d.dataset_id === datasetId);
    return !!profile?.profile_id && !!dataset?.compiled_profile_ids.includes(profile.profile_id);
  };
  return (
    <div className="setup-step-body">
      <h2>Load and go</h2>
      <p>
        Choose what the API serves. Profiles not yet compiled for the dataset are compiled now (about a minute per profile for a region, longer for a
        country) and cached under <code>.netweevil/bundles/metrics/</code>. The choice is saved to <code>workspace.json</code>, so the next start needs no
        arguments.
      </p>
      {info && info.datasets.length === 0 && <p className="hint-text">Import a street network in step 2 first.</p>}
      {info && valid.length === 0 && <p className="hint-text">Save a profile in step 3 first.</p>}
      <div className="form-grid">
        <label className="field">
          <span>Dataset</span>
          <select value={datasetId} onChange={(e) => setDatasetId(e.target.value)}>
            {(info?.datasets ?? []).map((d) => (
              <option key={d.dataset_id} value={d.dataset_id}>
                {d.dataset_id}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span>Default profile</span>
          <select value={defaultProfile} onChange={(e) => setDefaultProfile(e.target.value)}>
            {valid.map((p) => (
              <option key={p.path} value={p.path}>
                {p.profile_id} ({p.mode}, {p.source}){compiledFor(p.path) ? "" : " – compiles on load"}
              </option>
            ))}
          </select>
        </label>
      </div>
      <div className="field">
        <span>Also load these profiles</span>
        <div className="check-list">
          {valid
            .filter((p) => p.path !== defaultProfile)
            .map((p) => (
              <label key={p.path} className="field-check">
                <input type="checkbox" checked={extra.includes(p.path)} onChange={() => toggle(extra, setExtra, p.path)} />
                <span>
                  {p.profile_id} <span className="muted">({p.mode}{compiledFor(p.path) ? "" : ", compiles on load"})</span>
                </span>
              </label>
            ))}
          {valid.length <= 1 && <span className="muted">No other profiles yet.</span>}
        </div>
      </div>
      <div className="field">
        <span>Transit feeds</span>
        <div className="check-list">
          {(info?.transit_feeds ?? []).map((f) => (
            <label key={f.feed_id} className="field-check">
              <input type="checkbox" checked={feeds.includes(f.feed_id)} onChange={() => toggle(feeds, setFeeds, f.feed_id)} />
              <span>
                {f.feed_id} <span className="muted">({fmtNumber(f.stop_count)} stops)</span>
              </span>
            </label>
          ))}
          {(info?.transit_feeds ?? []).length === 0 && <span className="muted">No feeds imported (optional).</span>}
        </div>
      </div>
      <div className="run-row">
        <button type="button" className="btn btn-primary" onClick={() => void activate()} disabled={!datasetId || !defaultProfile || running}>
          {running ? "Loading…" : info?.loaded ? "Reload with this selection" : "Load selection"}
        </button>
        {error && <span className="error-text">{error}</span>}
      </div>
      <JobProgress job={job.data ?? (info?.jobs ?? []).find((j) => j.kind === "activate" && j.state === "running")} />
      {info?.saved && (
        <p className="muted">
          Saved selection in <code>{info.config_path}</code>: dataset {info.saved.dataset_id}, default profile <code>{info.saved.default_profile}</code>
          {info.saved.transit_feeds.length ? `, feeds ${info.saved.transit_feeds.join(", ")}` : ""}.
        </p>
      )}
    </div>
  );
}
