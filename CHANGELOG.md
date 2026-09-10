# Changelog

## Unreleased

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
  fingerprint selected corpus inputs and require the current schema.
- OD and point-set files use canonical longitude/latitude columns and object
  documents. Report exports and the QGIS plugin use the same names.
- Unused helpers, compatibility readers, settings migrations and manifest
  reexports have been removed.

See [routing parity](docs/routing-parity.md) for measurements, verification,
remaining feature gaps and implementation order.
