# NetWeevil console

A map-first web console for exercising every endpoint of the `netweevil` HTTP
API: street routes, directions, locate, waypoints, OD batches, matrices,
accessibility, service areas and sequences, betweenness, scenario batches,
GTFS transit routing and reach, and live traffic simulations. Every request is
timed and logged so query speed can be compared and exported.

Stack: Vite, React 19, TanStack Query, MapLibre GL. No UI framework, no map
token; the basemaps are key-free (OpenFreeMap Positron, CARTO dark, OSM).

## Run

Start the API first, for example:

```bash
cargo run --release -p netweevil-cli -- api serve \
  --dataset north_nl_2026_05_10 \
  --default-profile examples/profiles/car_north_nl_2026_05.yml \
  --profile examples/profiles/cycling_north_nl_2026_05.yml \
  --profile examples/profiles/pedestrian_north_nl_2026_05.yml \
  --transit-feed openov_nl_2026_05_11
```

Then start the console:

```bash
cd frontend
pnpm install
pnpm dev            # http://localhost:5173, proxies /v1 to 127.0.0.1:8080
```

Point the dev proxy elsewhere with `NETWEEVIL_API=http://host:port pnpm dev`, or
set the API base URL in the header at runtime (the API sends permissive CORS
headers, so a direct origin works too).

`pnpm build` writes a static bundle to `frontend/dist/`; serve it from any
static host and set the API base in the header.

## Using it

- Pick an analysis in the left rail. Each panel lists the request's map point
  slots; click the map to add points to the highlighted slot, drag to move,
  right-click to remove. "Scatter in view" drops random points in the current
  viewport for load tests, and "Paste" accepts JSON point arrays or `lon,lat`
  lines.
- Common options are form fields grouped by concern (snapping, connectivity,
  fallback, returns, alternatives, time, request-defined profiles). The
  "Request" button shows the exact JSON; editing it overrides the form until
  reset. "Copy curl" reproduces the call in a shell.
- JSON or GeoJSON response format can be chosen per run for endpoints that
  support `?format=geojson`.
- Results render on the map with hover details. The results panel shows key
  figures, any tabular rows (with CSV export), and the raw JSON. Exports:
  response JSON, map GeoJSON (client-side conversion), server GeoJSON
  (re-runs with `format=geojson`), and the request body.
- The performance readout in the bottom-right corner shows the last request's
  status, wall time, payload size and feature count with a sparkline. Expand it
  for first-byte/download/parse breakdown, per-endpoint min/p50/mean/p95/max, a
  request log, a "repeat last request N times at concurrency C" benchmark, and
  CSV/JSON export of all timings.
- Simulation: builds a scenario from the form (agents, duration, departures,
  optional weighted destinations and a slow zone drawn on the map), starts it
  in the API, polls status, streams agent frames for playback, overlays edge
  congestion, and accepts mid-run controls (pause, resume, speed factor, close
  a zone drawn on the map).

In development, `window.__netweevil` exposes the store and run helpers for
scripted checks from the browser console.
