# NetWeevil console

A map-first web console for exercising every endpoint of the `netweevil` HTTP
API: street routes, directions, locate, waypoints, OD batches, matrices,
accessibility, service areas and sequences, betweenness, scenario batches,
GTFS transit routing and reach, and live traffic simulations. Every request is
timed and logged so query speed can be compared and exported.

Stack: Vite, React 19, TanStack Query, MapLibre GL. No UI framework, no map
token; the basemaps are key-free (OpenFreeMap Positron, CARTO dark, OSM).

## Run

New workspace, nothing imported yet: build the console once and start the
API in setup mode, then use Setup in the console:

```bash
pnpm install && pnpm build          # writes frontend/dist
cargo build --release -p netweevil-cli   # embeds frontend/dist into the executable
target/release/netweevil bootstrap  # http://127.0.0.1:8080/ opens in the browser
```

Or simply `./scripts/start.sh` from the repository root, which does all of
that and keeps the data in `~/NetWeevil`.

Or start the API with an existing dataset, for example:

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

- **Setup** (bottom left of the rail) is the beginner path and the place to
  see where files are saved: every folder under `.netweevil/` with its
  purpose, then upload or reference an OSM/Overture extract and import it,
  create car/bicycle/pedestrian profiles from the built-in templates (edit
  the YAML before saving), import GTFS zips, and load a dataset/profile/feed
  selection. Imports and loading run as background jobs with progress. The
  selection is saved to `.netweevil/workspace.json` so the next start needs
  no arguments. When the API has no dataset loaded, Setup opens by itself.
- **GTFS editor** (transit group) creates scenarios: new lines on top of a
  loaded feed (for example a Lelylijn scenario on the national feed) or a
  stand-alone feed. Add a line, then click stops on the map in order: base
  feed stops (small circles) are reused, a click elsewhere creates a new stop
  (drag to move, right-click to remove). Set speed, dwell, both directions,
  days and headway windows; "Existing routes" copies a route's stop pattern
  (or an express variant to thin out) and can drop base routes from the
  scenario. "Build feed" writes a separate feed and loads it, the feed picker
  switches to it, and every transit tool can compare against the original by
  switching back. "Revert" removes the built feed; the original feed and its
  zip are never touched. "Export GTFS" downloads the scenario as a zip. The
  status block shows where the scenario, the built bundle and the export
  live.
- **Network explorer** (street group) colours the edges in the view by a
  source attribute (road class, surface, posted limit, gradient, access,
  one-way) or by what the selected profile makes of them (speed, travel
  time, cost per km, allowed). Pick a second profile to colour by the
  difference (speed delta, time ratio, allowed by one or both). Hover an
  edge for all values; it re-runs live as the map moves once zoomed in.
- Pick an analysis in the left rail. Route tools (route, directions,
  waypoints, OD pairs, transit route, scenario batch) share one list of
  start/end pairs: click the map to place A, then B; drag markers to move
  them; right-click to remove. The pairs stay in place when you switch
  between tools. "+ Add" places another route; every route runs at once
  (one request per route, or one batched request for OD pairs) and each
  route gets its own colour. "Scatter routes in view" drops N random routes
  for load tests and "Paste" accepts JSON pairs or `lon,lat,lon,lat` lines.
  In the waypoints tool, clicks after A and B add via stops.
- The header search box takes a place name (OpenStreetMap Nominatim, limited
  to the dataset extent) or a `lon, lat` pair; a result flies the map there
  or sets A or B of the active route.
- "Compare with" under the profile picker runs the same routes with extra
  profiles at once (for example car versus bicycle); each profile gets its
  own colour and a row in the results table.
- "Live" (on by default for route tools) re-runs the request whenever the
  inputs change, so dragging a marker updates the route as you move it.
  Older in-flight requests are cancelled. The Run button (or Enter) still
  works and also zooms to the result.
- Other tools list their own point slots; click the map to add points to
  the highlighted slot, drag to move, right-click to remove. Slots are shared
  too (matrix and accessibility origins are the same points).
- Common options are form fields grouped by concern (snapping, connectivity,
  fallback, returns, alternatives, time, request-defined profiles). The
  "Request" button shows the exact JSON; editing it overrides the form until
  reset. "Copy curl" reproduces the call in a shell. The "–" button next to
  the tool title minimises the settings; the "‹" tab on the map edge hides
  the whole panel.
- JSON or GeoJSON response format can be chosen per run for endpoints that
  support `?format=geojson`.
- Results render on the map with hover details. Map-corner buttons: "Style"
  (reach polygon palette or single colour, fill opacity, outline, network and
  route line widths; a legend lists the bands), "Clear map" (remove drawn
  results, keep points) and "Clear all" (points, routes and results; also
  Ctrl+Shift+Backspace). The results panel has tabs: Directions (turn-by-turn
  steps for the directions tool; click a step to fly there), Summary (key
  figures; with several routes, a comparison table with one row per route,
  click a row to highlight it on the map), Table (any row set in the
  response, sortable, CSV export; "All routes" merges the same rows from
  every route with a route column), and JSON. Exports: response JSON, map
  GeoJSON (client-side conversion), server GeoJSON (re-runs with
  `format=geojson`), and the request body.
- Keyboard: Enter runs, Escape cancels an in-flight run, Delete removes the
  active route's last placed point.
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

### Hong Kong in 3D

Run the [Hong Kong tutorial](../docs/hong-kong-tutorial.md) to build the road, pedestrian and transit dataset, then connect the console to that API workspace. The network explorer returns source XYZ coordinates. Pick a foot profile and zoom in before running the explorer to stay within its edge limit.

Click **3D** above the map to draw network edges, route legs and stops at their source elevations. **Style** contains tilt, vertical exaggeration, an x-ray basemap opacity control, elevation colours and an altitude slice. Slice limits use original source metres; exports keep original XYZ even when the display uses exaggeration. Vertical lifts and stairs remain three dimensional. The x-ray view keeps underground geometry visible above the basemap canvas. It does not estimate ground depth.

**DEM terrain** optionally adds the local Hong Kong LandsD 5 m terrain and subtle hillshade. It starts off, runs only in 3D, and shares the network's vertical exaggeration. Both datasets use HKPD heights; the viewer never adds DEM height to surveyed network coordinates. The separate network overlay stays visible through the terrain. Terrain preferences persist across reloads and basemap changes. **Refresh terrain sources** discovers newly prepared local tiles; unavailable sources and tile errors appear in the style panel. Missing-coverage tiles do not disable the covered terrain. Source resolution and datum are shown beside the control, with Lands Department attribution on the map. See [terrain preparation and limitations](../docs/terrain-viewer.md).

With a transit feed loaded, **3D** automatically loads platform surfaces and bound GTFS boarding points. Hong Kong's complete station layer stays loaded while panning and zooming, so raised and underground geometry is not removed by ground-coordinate viewport bounds. Feeds exceeding 5,000 features use a padded viewport query; the previous geometry remains visible until its replacement arrives. **Station platforms and boarding points** toggles this overlay. Purple surfaces are surveyed platform units, amber surfaces are whole platform-floor footprints where individual outlines are unavailable, and teal points are GTFS boarding locations bound to the pedestrian network. Labels identify stations and platforms; hover shows source provenance and floor-footprint fallbacks. Source surface elevations are preserved even when they differ from boarding elevations. The altitude slice retains whole surfaces intersecting the selected range.

Under **Local reference layers**, you can also load `.netweevil/hong-kong/transit/mtr_platforms.geojson` and `mtr_exits.geojson` together. These overlays remain visible while running routes or changing tools. Imported platform markers are purple and exit markers green in normal colours. Each layer has a remove button; **Fit reference layers** frames their combined extent. Local files stay in the tab and must be loaded again after a page reload. The import limit is 50 MB per file; use the API network explorer for the full pedestrian network.

Network colours include source elevation, elevation coverage, structure, pedestrian connection type, source level and indoor/outdoor status. Heights are absolute source elevations, not floor numbers or depth below terrain. Missing Z coordinates draw at zero, appear grey in elevation colours and are identified in the style panel. Hover prioritizes elevation provenance, including transit geometry interpolated between platform bindings. GTFS vehicle paths with inferred heights are not surveyed tunnel alignments.

A Hong Kong feed with a loaded foot profile defaults to network access and network walking geometry, includes intermediate stop segments for platform-height anchors, chooses its registered transfer profile when available, and starts at 08:30 Hong Kong time on the feed's first service day. Existing explicit form choices take precedence. Verify the selected date lies within the imported feed's service window.

**Max marker snap distance (m)** limits the gap from A/B to the street network, independently of the walking distance allowed to reach a transit stop. Its default is 50 m; zero requires an exact attachment. Dashed amber lines show off-network gaps from the saved request endpoints to the surveyed walking geometry. They are unsurveyed estimates, not routed or verified traversable paths. Their estimated distance and time are included in journey totals and exposed in the walking path components. Moving markers later does not change an earlier result's connection geometry.

MTR and light rail use community estimated times. Surface services use Transport Department headways. The transit panel displays this distinction. In 3D mode, A/B altitude fields let you constrain snapping to a source elevation. JSON route pastes retain optional `z` values.

Moving a route endpoint, via or analysis marker to a different horizontal position clears its fixed elevation. Enter a new source height after moving it if you need to constrain the floor. Metadata edits and moves that leave the coordinates unchanged retain the height.
Endpoint altitude fields also remain visible in 2D while either endpoint has a fixed elevation, so a saved height constraint can be inspected or cleared.

The 3D renderer loads on demand. The regular flat map remains available if the browser cannot start the GPU overlay. Reference points and route geometry use the same source coordinate datum; no conversion between differing vertical datums happens in the viewer.

Transit route and Transit reach use **Allowed transit modes** checkboxes. Checked modes are permitted; **Allow all** and **Allow none** change the whole selection. An empty selection excludes every transit vehicle mode. MTR uses **MTR / Subway / Metro**, while Hong Kong light rail uses **Tram / Light rail**. Legacy complete mode strings are accepted; partial values are rejected before a request is sent.

**Transit directions** sends the same route inputs and transit settings to `POST /v1/transit-directions`. Its Summary shows ordered walking, boarding, riding, alighting and transfer instructions, with platform details and times in the feed's timezone. Expand **Walking instructions** for available street and facility steps; click an instruction to focus its location on the map. The underlying 3D itinerary remains visible. Table and JSON retain the complete response, and the map export retains its XYZ geometry. This endpoint returns JSON; the other transit tools also support server GeoJSON.

New forms and blank legacy values retain the API's nine default transit modes. Air and Other start unchecked; **Allow all** includes them. When custom request JSON is active, a persistent notice explains that it overrides the form. **Use form** removes that override and keeps your current form choices. **Reset** restores the Hong Kong network-access settings and imported service date when a Hong Kong feed is loaded.

Enable **Style → 3D → DEM terrain** to display a prepared local elevation raster. Hong Kong terrain uses the surveyed network's HKPD height datum and shares its exaggeration. X-ray basemap opacity fades the terrain while station paths remain visible. Prepare or refresh tiles with `scripts/setup_hong_kong.sh --terrain-only --skip-build --no-serve` from the repository root. See [terrain setup](../docs/terrain-viewer.md) for source and accuracy details.
