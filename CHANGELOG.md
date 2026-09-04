# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Arrive-by transit search: `time.arrive_by` on transit routes and transit
  service areas runs a latest-departure scan against the requested arrival
  deadline, with `search_window_s` bounding how far before that deadline
  trips may arrive. Boarding and transfer slacks, transfer limits, mode
  filters, directed transfer tables, and network access/egress pricing apply
  exactly as they do depart-after. The QGIS transit and service-area tabs
  expose it as an Arrive by checkbox.
- Overture Maps support: `dataset import` reads transportation-theme
  GeoParquet (a file or a directory of files) alongside OSM PBF, detected
  from the path or forced with `--format`. Segments are split into
  connector-to-connector chunks; classes, access restrictions (including
  heading-scoped oneways), speed limits, surfaces, prohibited transitions,
  and — on releases that carry the column — lanes are mapped into the
  shared topology model. Dataset manifests record the source format.
- Per-direction `max_speed_kph` and `lanes` edge attributes. OSM
  imports fill them from `maxspeed`/`lanes` tags, Overture imports from
  `speed_limits`/`lanes`. Real lane counts drive simulation capacity.
- Profile setting `speeds.posted_limits` (`prefer` | `cap` | `ignore`,
  default `cap`) controlling how posted limits combine with profile
  speeds for motorized modes: take precedence over them, cap them, or be
  ignored.
- Docker build-time data provisioning: `NETWEEVIL_FETCH_OSM_URL` or
  `NETWEEVIL_FETCH_OVERTURE_BBOX` compose build args fetch and import the
  data during the image build, baking only the imported `.netweevil`
  bundles into the image (they seed the state volume on first mount). The
  default flow — runtime import from the `./datasets` mount — is unchanged.

- Exact CCH many-to-many engine for OD and matrix batches: each unique
  endpoint pays for one complete upward search space and every combination
  is answered by a merge-join, preserving pairwise exactness.
- `modes.street_access = "network"` for transit routes and service areas:
  access/egress legs are priced with real street-network times when
  matching street profiles are loaded (API), falling back to straight-line
  estimates otherwise.
- `alternatives.max_search_attempts` (default 24) bounds banned-edge
  alternative searches, sampled evenly along the best path.
- `netweevil-manifest` crate: manifest types moved out of
  `netweevil-report` (which re-exports them) so state and import builds no
  longer compile parquet/arrow/sqlite.
- Tracked `datasets/README.md` with download instructions for the
  quickstart datasets, plus `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`,
  `SECURITY.md`, and issue/PR templates.

### Changed

- Bundle persistence has one format. Topology, acceleration, and
  compiled-profile bundles are written and read as sectioned `NWSECB05`
  files; anything else is rejected with an error telling you to re-import
  the dataset and recompile profiles. Upgrading requires re-importing every
  dataset and recompiling every profile.
- `TopologyBundle` stores edges only as `edge_layers`
  (`TopologyEdgeLayers`); build them with
  `TopologyEdgeLayers::from_directed_edges` or `TopologyBundle::push_edge`.
- Each bundle type carries a single schema version constant
  (`TOPOLOGY_BUNDLE_SCHEMA_VERSION`, `COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION`,
  `ACCELERATION_BUNDLE_SCHEMA_VERSION`) that readers check on load.
- API transit payloads: `transfer_profile_id` is accepted only under
  `request.modes.transfer_profile_id`; the top-level shorthand beside
  `feed_id` was removed.
- `netweevil-api` exposes only `serve` and `ApiServeOptions`; request and
  response DTOs are crate-private and the crate depends on
  `netweevil-manifest` instead of `netweevil-report`.
- `netweevil_ingest::import_dataset` is the single import entry point
  (multiple sources plus a progress callback); the single-source and
  no-progress variants were removed.
- `new_run_manifest` requires the completed algorithm info and methods
  summary instead of filling placeholder text.

### Removed

- The bincode bundle readers and every mirror struct that parsed them.
- The `AnalysisOutcome::NotImplemented` and
  `AnalysisDiagnosticCode::AnalysisNotImplemented` variants, which no
  analysis produced.
- The `TopologyBundle::edges` array-of-structs edge list.
- Presentation decks and their build assets are no longer tracked
  (`/pitch_assets/`, `/*.pptx`), along with the obsolete extension plan
  document and an unreferenced QGIS symbology database.
- Unused dependencies (`directories`, `flate2`, `arrow-array` in query,
  `uuid`/`time` in report, `time` in core, `tracing` in the CLI) and dead
  code in query, ingest, transit, and the QGIS plugin, including the
  disabled arrive-by transit checkbox.

- Service-area polygons are now concave: reachable segments are traced on
  a metric grid into boundary rings that hug the network, and enclosed
  unreachable pockets (water, restricted areas) become GeoJSON holes
  instead of being covered by a convex hull. New `polygon.cell_size_m`
  option controls the resolution.
- Profile compilation customizes per-metric CCH weight sets (travel time
  and distance) alongside the generalized-cost weights; service areas and
  accessibility adaptively switch from the bounded Dijkstra to an exact
  PHAST hierarchy sweep once the reachable ball exceeds an eighth of the
  graph. Profiles compiled without those weight sets keep the Dijkstra
  path.
- Failure-mode requests on pairwise-only-restriction datasets route over
  penalty-customized CCH weights instead of graph-wide A*; multi-edge
  restriction datasets and reverse-oneway variants keep the exact A* path.
- Query-time topology nodes do not carry the import-only OSM node id.

- Bundle persistence: topology, acceleration, and compiled-profile bundles
  use a sectioned fast-load format whose large primitive arrays are read
  with one memcpy per array from the memory-mapped file.
- CCH preprocessing: the contraction now stores arcs in per-state sorted
  pending lists (no global hash set, no arc table), cutting peak memory
  roughly 4x, and the recursive-bisection ordering and CSR sorts run in
  parallel.
- Transit service areas: static per-stop transfer adjacency (built once per
  prepared router), parallel per-origin searches, and memoized shape-point
  lookups replace per-state spatial scans.
- Simulation startup: demand is planned up front but routed lazily in
  departure-time windows, so runs start immediately instead of routing
  every agent before tick 0; simulating without an acceleration bundle now
  warns.
- Simulation loop: reroute searches run in parallel with reused FxHash A*
  scratch; per-edge stat accumulators are dense arrays instead of a hot
  HashMap.
- Batch OD/matrix strategy: CCH search-space reuse from 4 average
  destinations per origin; single-source Dijkstra trees remain for wide
  fan-outs without acceleration.
- Service areas and accessibility: expansions record the reached edge set
  (interval construction no longer scans every edge in the dataset), are
  built in parallel across unique origins, and are shared instead of
  cloned.
- Failure-mode requests (`allow_reverse_oneway`, etc.) reuse cached
  degraded graphs instead of cloning the topology and rebuilding the CSR
  graph per request.
- Missing CCH halves (dataset bundle or compiled profile weights) now log a
  warning instead of silently falling back to the exact engine.
- One shared geodesic implementation in `netweevil_core::geo` replaces the
  per-crate haversine copies; ingest node tables use FxHash and the
  component union-find is iterative (no stack overflow on long chains).
- Snapping refuses full linear-scan fallbacks on large graphs, and matrix
  requests refuse to materialize more than 4M cells.
- CI denies clippy warnings and adds MSRV, macOS, and rustdoc jobs; all
  crates carry crates.io metadata and crate-level documentation.
- Workspace toolchain aligned on Rust 1.96.1.
