# Changelog

All notable changes to NetWeevil are in this file.

## 0.4.0-alpha - 2026-10-28

### Breaking changes

- The `netweevil-manifest` crate is now the `manifest` module of
  `netweevil-persist`. Use `netweevil_persist::manifest` in place of
  `netweevil_manifest`.
- `GET /readyz` is a readiness probe. It returns `{"status": "ready"}` with the
  dataset id and the default profile id when a dataset is loaded. In setup mode
  it returns `{"status": "setup"}` with HTTP 503. Before, it returned the
  `/v1/service` body. Use `GET /v1/service` for service information.
- Travel modes no longer accept the serde aliases `transit` (use `bus`),
  `truck` (use `hgv`), and `motor_scooter` and `scooter` (use `moped`).
- Traffic updates and traffic CSV files no longer accept `way_id`. Use
  `feature_id`.
- Transit access and egress legs no longer use the coordinate-only street-time
  estimate as a fallback. In network street-access mode, a stop that the
  street network cannot reach is unreachable from the point.
- The Hong Kong scripts take the source folder from `--source-dir` or
  `NETWEEVIL_HK_SOURCE_DIR`. The `--analysis-repo` option and the search in
  sibling folders are removed.
- NetWeevil no longer computes SHA-256 hashes of profiles or
  sources. A run is reproducible from its dataset and profile document; the
  profile id and dataset id identify them in outputs. Existing `.netweevil/`
  state must be re-imported: dataset, transit feed and traffic imports,
  compiled profiles and transfer tables all use new schemas.
  - API: `profile_hash` is gone from `service` in route, OD, matrix,
    service-area, trip, waypoint, locate and trace-matching responses, from
    GeoJSON feature properties, and from `/v1/service` and `/v1/profiles`
    profile entries. `dataset.source_sha256` is gone from `/v1/service`, the
    reload summary and the dataset update response.
  - Manifests: `DatasetManifest.source_sha256`,
    `last_update.previous_source_sha256`,
    `methods_summary.locked_profile_hash`, the transit feed's
    `source_sha256` and `stop_binding_sha256`, the traffic manifest's
    `source_sha256`, and the DEM and admin-boundary `sha256` provenance in
    topology bundles are removed. Run reports list the profile id instead of
    a locked profile hash.
  - Compiled profile manifests store the profile document they were compiled
    from (`profile`) and are reused only for an identical document. Compile
    ids are `<dataset>-<profile_id>`; recompiling a changed profile replaces
    its bundle.
  - Dataset imports no longer read the source an extra time to hash it.
    Every import and update writes bundles with fresh ids. `dataset update`
    no longer rejects an unchanged replacement extract, and it checks that
    the recorded boundaries file and DEM still exist instead of comparing
    their contents.
  - Transit transfer tables record the profile document they were routed
    with (`profile` in the feed's `transfer_tables`); the API rejects a
    table whose profile document differs from the loaded one. Transit
    bundles are written to `transit-<feed_id>.bin` and transfer tables to
    `transfers-<feed>-<dataset>-<profile_id>.bin`.
  - Benchmark reports (schema 3) carry the selected corpus rows
    (`settings.selected_corpus`) in place of `corpus_sha256`; comparison
    requires identical rows.
  - `netweevil profile validate` and `netweevil dataset import` no longer
    print hashes.
### Added

- The API serves its OpenAPI 3.1 document at `GET /openapi.yaml` and a Swagger
  UI page at `GET /docs`. The document is also in `docs/api/openapi.yaml`. A
  test makes sure that the document and the router agree.
- Each CLI command and common option has help text (`netweevil <command>
  --help`).
- The `docs/` folder is a MkDocs (Material) site, published at
  <https://elqniemi.github.io/netweevil/>. The README is a short landing page
  and the reference content is in the guides.

### Other changes

- `netweevil analyze directions` returns turn-by-turn directions from the
  CLI. It reads a route request or a `/v1/directions` body, takes
  `--language`, `--units`, `--driving-side` and `--facility-attribute`, and
  writes the API `result` JSON with a `directions` run manifest.
- Route failure modes pick the cheapest orientation of the snapped
  attachment on cost plus illegal-movement penalties. Before, the first
  routable orientation won, so a penalized forbidden U-turn could replace a
  cheaper legal route.
- `fallback.max_illegal_distance_m` and `max_illegal_turns` no longer fail a
  request whose cheapest penalized route breaks them. The cheapest
  orientation and route within the limits is returned, and the request fails
  only when none exists. Alternatives over a limit are dropped.
- Profiles: `preferences.max_grade` excludes edges whose steepest DEM grade
  in the travel direction is above a limit, either one rise/run value or
  separate `uphill`/`downhill` limits.
- `returns.admin_areas` and `returns.elevation` also annotate waypoint legs
  (`/v1/waypoints`) and alternative routes, each from its own path.
- Directions: two same-side turns joined by a short connector (a cycle path
  hairpin) are one sharp turn, not a U-turn. Walking and cycling jogs that
  barely change the heading get no step.
- Inbound service areas (`"direction": "inbound"`): positions that reach the
  origins within the thresholds, from a backward search with turn costs and
  restrictions (backward PHAST for large areas).
- Isochrone polygons: `denoise`, `min_part_area_m2` and `min_hole_area_m2`
  drop small parts and holes; `simplification_tolerance_m` is now
  Douglas-Peucker generalization.
- `POST /v1/locate` takes `count` and returns the nearest road segments with
  edge attributes; points without a match return empty lists.
- `POST /v1/expansion` returns the settled edges of a route or service-area
  search as GeoJSON.
- `GET /v1/tiles/{z}/{x}/{y}.mvt` serves the profile's routable network as
  vector tiles; the console map can overlay them.
- `netweevil dataset update` applies OSM change files (`.osc`, `.osc.gz`) or
  a newer extract to an imported OSM dataset without nested-dissection
  ordering: the topology equals a full import of the changed extract and the
  CCH is contracted with the previous elimination order. Updates report fill
  growth against the last full ordering and reorder from scratch past
  `--max-fill-growth` or with `--reorder always`. OSM PBF imports keep a
  source snapshot under `.netweevil/bundles/source` for this and read the
  extract in one pass; datasets imported earlier need a re-import to update.
  The API applies updates with `POST /v1/workspace/datasets/{id}/update`
  and reloads changed datasets with `POST /v1/workspace/reload`, swapping
  the runtime once the new build is loaded. Cached compiled profiles are
  only reused for the topology they were compiled against, and bundle and
  manifest files are replaced atomically.
- Directions: `POST /v1/directions` takes `directions_options` (`language`,
  `units`, `driving_side`, `facility_attribute`) and returns maneuvers in
  English, Dutch, German, French, Finnish or Traditional Chinese (Hong Kong)
  with forks, ramps, exits, merges, U-turns, ferries, stairs, escalators and
  lifts, arrival side, lane guidance from `turn:lanes`, signposts from
  `destination*`, `junction:ref` and `exit_to`, and verbal pre-alert, alert and
  post-transition text. Time-dependent routes are supported. The edge-name
  bundle now uses the sectioned format and carries these tags, so datasets
  must be re-imported.
- `netweevil bootstrap` and argument-less `api serve` start in setup mode and
  serve the web console, which is embedded in the executable when
  `frontend/dist` exists at build time. `bootstrap` opens the browser
  (`--no-open` to skip). `scripts/start.sh`, `scripts/start.cmd` and
  `scripts/package_release.sh` give one-step starts; `docker compose up`
  starts in setup mode. The console's Setup flow uploads or references
  OSM/Overture extracts and GTFS zips, imports them as background jobs,
  creates profiles from built-in car/bicycle/pedestrian templates, and loads a
  dataset/profile/feed selection without restarting; the selection is saved to
  `.netweevil/workspace.json`. Every folder under `.netweevil/` is listed with
  its purpose in the console and the bootstrap banner.
- GTFS editor: scenario documents (`.netweevil/gtfs_scenarios/`) describe new
  lines drawn on the map (or copied from existing route patterns as express
  variants) with headway-based timetables. A scenario builds into a separate
  feed on top of an imported feed or from scratch, loads on the fly, can be
  reverted, and exports as a GTFS zip. Base feeds are never modified.
- Network explorer (`POST /v1/network/edges`): edges in the map view with
  source attributes and per-profile speed, time, cost and access, with a
  second profile for side-by-side deltas.
- Transit feeds can be added to and removed from a running API.
- Road snapping uses a prepared edge index, including the interiors of long
  edges. Snap caches preserve point IDs and full coordinate precision.
- CCH and restriction-aware Dijkstra stopping account for partial-edge
  destination costs. Matrix searches validate restrictions and reuse an exact
  frontier across destinations from the same origin.
- `POST /v1/locate` exposes profile-aware, directional snap candidates.
- Transit distinguishes each dated/frequency vehicle run, enforces scheduled
  pickup/drop-off rules, and preserves repeated-stop connection sequences.
  Geometric transfers cover the complete requested radius and honor transfer
  slack in both directions. Transit bundles require re-importing.
- Transit departure indexes use connection references and continuation links.
- Benchmarking supports Valhalla alongside NetWeevil and OSRM. Reports
  record the selected corpus rows and require the current schema.
- OD and point-set files use canonical longitude/latitude columns and object
  documents. Report exports and the QGIS plugin use the same names.
- Unused helpers, compatibility readers, settings migrations and manifest
  reexports have been removed.
