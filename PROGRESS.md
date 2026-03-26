# NETAN Progress

## Product Goal

Build a Rust-first network analysis tool with one native core powering:

- a reproducible CLI for research and batch execution
- a preloadable HTTP API for interactive and remote execution
- a QGIS plugin for spatial workflows on top of the API

The system targets local OSM routing and network analysis with explicit, versioned configuration, persistent caches, and outputs that are easy to cite in academic work.

## Current Status

### Completed

- [x] Initialize a Rust workspace with the planned crate split:
  - `core`
  - `ingest`
  - `profile`
  - `query`
  - `persist`
  - `report`
  - `cli`
  - `api`
- [x] Define shared schema for:
  - profile configuration
  - route request input
  - run manifests
  - dataset manifests
  - compiled profile manifests
- [x] Implement the first CLI surface for:
  - `dataset import`
  - `dataset list`
  - `profile validate`
  - `profile compile`
  - `analyze route`
  - `analyze od`
  - `analyze matrix`
  - `experiment run`
  - `report render`
  - `cache list`
  - `api serve`
- [x] Add example research profile and route request files
- [x] Add a cache/workspace layout under `.netan/`
- [x] Smoke-test the initial scaffold with:
  - `netan profile validate examples/profiles/car_research_v1.yml`
  - `netan dataset import datasets/groningen-260317.osm.pbf --name groningen_2026_03`
  - `netan cache list`

### In Progress

- [ ] Advanced analysis expansion:
  - Phase 1 service areas are complete for the shared backend surfaces: request/result schema, execution engine, network and polygon outputs, CLI/API execution, writers, reports, and example requests.
  - Phase 2 disconnected-network handling is complete for the shared backend surfaces, including service-area skip semantics, explicit connectivity metadata in outputs, and QGIS/plugin diagnostic surfacing.
  - Phase 3 degraded routing / failure modes is complete for route, OD, and matrix execution, including reverse-oneway, illegal-turn, ignored-restriction, and forbidden-UTurn fallbacks with explicit violation provenance.
  - Phase 4 QGIS support for service areas and the new diagnostics is still pending.
  - detailed task breakdown in `IMPLEMENT_ADVANCED_ANALYSIS.md`

### Not Started

- [ ] Acceleration layer:
  - edge-based CCH as the main target
  - CRP as a fallback candidate
- [ ] Benchmark suite
- [ ] Regression fixtures and golden outputs
- [ ] Packaging for standalone binaries

## Planned Execution Order

1. Scaffold workspace and shared schema
2. Implement PBF ingest and dataset import
3. Implement exact routing and path reconstruction
4. Implement explicit defaults packs and profile compilation
5. Implement exports and research manifests
6. Harden the API and QGIS workflow
7. Add persistent mmap bundle loading
8. Add acceleration structures
9. Add many-to-many and scenario sweeps
10. Add benchmarks, fixtures, and packaging

## Notes

- The current code intentionally treats CLI, API, and QGIS state as serialized configuration first.
- Hidden UI-only state is out of scope.
- Deterministic outputs and explicit provenance are product requirements, not polish items.
- Verified locally on 2026-03-26: `cargo fmt --all` and `cargo test` pass after landing shared Phase 1 service-area execution across query, CLI, API, report, and output-writer surfaces.
- Verified locally on 2026-03-26: `cargo test` passes after completing the remaining Phase 2 disconnected-network handling tasks and Phase 3 degraded-routing failure modes, including route/OD/matrix violation reporting and QGIS diagnostic logging.
- Verified locally on 2026-03-18: the workspace builds, the example profile validates, `dataset import` scans `datasets/groningen-260317.osm.pbf` into a topology bundle with 493,348 nodes and 1,033,012 directed edges, `profile compile` writes a separate metric bundle with 1,033,012 edge metrics, `analyze route` solves `examples/requests/route.json` with 27.0 m / 36.5 m snaps, 1,665 m total distance, and 152.496 s total travel time, `analyze od` completes 2 example pairs, and `analyze matrix` completes a 2x2 example matrix.
- Verified locally on 2026-03-18: `cargo test` passes after adding ferry-duration ingest and profile compilation handling.
- Verified locally on 2026-03-18: the rebuilt CLI accepts `examples/requests/od_pairs.csv`, `examples/requests/matrix_origins.csv`, and `examples/requests/matrix_destinations.csv`, writes route outputs to CSV and GeoJSON, and writes matrix outputs to GeoPackage.
- Verified locally on 2026-03-18: re-importing `datasets/groningen-260317.osm.pbf` as `groningen_2026_03_turns`, recompiling `car_research_v1`, and rerunning `analyze route` succeeds on the new topology schema; that extract reported `turn_count: 0` under the current first-pass node-based restriction support.
- Verified locally on 2026-03-18: `experiment run examples/experiments/baseline_sweep.yml` now executes route, OD, and matrix scenarios sequentially, writes per-scenario outputs plus run manifests, and writes an experiment summary JSON under `.netan/runs/`.
- Verified locally on 2026-03-18: `report render` now emits Markdown, HTML, or a report bundle directory with `index.md`, `index.html`, `run-manifest.json`, and a copied result artifact when present.
- Verified locally on 2026-03-18: `cargo test` passes after adding experiment schema coverage, report rendering tests, and report bundle export tests.
- Verified locally on 2026-03-18: `cargo test` passes after adding via-way turn-restriction sequence ingest, bidirectional exact routing with A* fallback for multi-edge restrictions, Parquet/GeoParquet writers, and mmap-backed binary bundle loading.

### Newly Completed

- [x] Remove the deprecated native GUI crate and make the QGIS plugin plus HTTP API the supported interactive workflow
- [x] Add an in-repo QGIS plugin package that talks directly to the HTTP API and loads spatial outputs into QGIS
- [x] Add a preloadable JSON HTTP API for route, OD, and matrix execution with selectable compiled profiles

- [x] Convert `dataset import` from dataset registration into real PBF ingest
- [x] Build immutable topology bundles from `.osm.pbf`
- [x] PBF parsing for Geofabrik extracts
- [x] Directed edge-based graph construction (first pass, without turn restrictions or geometry payloads)
- [x] Separate topology compilation from profile metric compilation
- [x] Replace placeholder `analyze route` execution with a correctness-first routing kernel
- [x] Snapping engine for single-route origin and destination points
- [x] Path reconstruction and optional per-segment route outputs
- [x] Extend the correctness-first routing kernel from single-route queries to OD and matrix execution
- [x] OD-list execution pipeline
- [x] OD matrix execution pipeline
- [x] Capture ferry duration metadata from OSM tags during topology ingest
- [x] Compile ferry edge travel times from tagged durations with inference fallback when configured
- [x] CSV input loading for OD-pair and matrix point-set requests
- [x] CSV, GeoJSON, and GeoPackage result writers for route, OD, and matrix outputs
- [x] First-pass node-based turn restriction relation handling during topology ingest
- [x] Exact route, OD, and matrix execution honoring persisted prohibited turn transitions
- [x] Service-area execution with network and polygon outputs across CLI, API, reports, and file writers
- [x] Shared disconnected-network handling with explicit connectivity diagnostics across route, OD, matrix, service-area outputs, and QGIS plugin logging
- [x] Explicit degraded-routing failure modes with penalties, violation provenance, reports, and exported outputs for route, OD, and matrix execution
- [x] Scenario sweep execution with per-scenario outputs and experiment summary artifacts
- [x] HTML/Markdown research bundle export with methods summary and report bundle output
- [x] Via-way turn restriction relation handling and broader mode-specific restriction coverage
- [x] Exact bidirectional Dijkstra/A* query engine
- [x] Parquet and GeoParquet writers
- [x] Memory-mapped bundle loading

- `dataset import` now scans `.osm.pbf` input, extracts a first-pass directed topology bundle, and writes a binary bundle under `.netan/bundles/topology/` for mmap-backed loading.
- `dataset import` now writes edge names to a separate binary bundle under `.netan/bundles/names/`, so prepared routing can keep cold labels out of the hot topology load.
- `dataset import` now also writes a dataset-level acceleration bundle under `.netan/bundles/acceleration/`, containing a deterministic order over the edge-transition graph plus upward/downward oriented transition topology for future CCH preprocessing.
- The topology bundle remains correctness-first: directed edges, persisted raw edge-based adjacency/successor topology, expanded node-based and via-way prohibited turn sequences, optional ferry-duration metadata, per-edge roundabout and traffic-signal flags, road/surface classes, and name tables are persisted, while geometry payloads remain pending.
- Route execution now reads the cold edge-name bundle only when segment-row output is requested, and dataset-backed execution now expects the current format rather than older in-topology-name imports.
- Prepared routing engines now reuse persisted edge-based bundle topology directly; older dataset imports must be rebuilt into the current format instead of relying on startup rebuild compatibility.
- `profile compile` now reads the topology bundle and writes a separate binary metric bundle under `.netan/bundles/metrics/`, keeping profile-aware weights distinct from the immutable topology artifact while enabling mmap-backed loading at execution time.
- `profile compile` now also reads the dataset acceleration bundle when present and persists customized upward/downward acceleration-arc weights inside the compiled profile bundle.
- Ferry durations now prefer tagged OSM durations when available, are apportioned across emitted ferry segments during ingest, and fall back to inferred speed-based duration only when `ferry.infer_duration_when_missing` is enabled.
- Turn restriction ingest now parses `type=restriction` relations with `from` way, `via` node or ordered `via` way members, and `to` way members, expands `no_*` and `only_*` restrictions into prohibited edge sequences, and stores them in the topology bundle with broader mode-mask coverage.
- `analyze od` now accepts CSV files with `id,source_x,source_y,target_x,target_y`, and `analyze matrix` now accepts CSV origin/destination point sets with `id,x,y`.
- Result output format is now inferred from the `--out` extension: `.json`, `.csv`, `.geojson`, `.gpkg`, `.parquet`, and `.geoparquet` are supported for route, OD, and matrix runs.
- Route spatial exports write the solved path geometry, while OD and matrix spatial exports write requested desire lines with batch metrics attached as feature attributes.
- `analyze route` now loads the compiled topology and metric bundles through mmap-backed binary readers, snaps origin/destination to traversable nodes or interior edge phantoms, runs exact bidirectional edge-based search for the normal hot path, falls back to the automaton-augmented exact search only for multi-edge via-way restriction sequences, reconstructs the original edge/node path, and writes a result plus succeeded run manifest under `.netan/runs/`.
- Unrestricted route queries now keep first/last edge summaries plus geometry partial-edge aware for phantom snaps; persisted oriented acceleration topology remains stored for later work but is not currently used on the hot query path.
- `analyze od` and `analyze matrix` now reuse the same exact route kernel in repeated single-pair mode, writing compact per-pair and per-cell result tables plus succeeded run manifests under `.netan/runs/`.
- `experiment run` now resolves scenario-local paths relative to the study file, recompiles profiles as needed, executes route/OD/matrix scenarios sequentially, and writes a batch summary JSON with per-scenario status, output paths, run manifest paths, and compact metrics.
- `report render` now reads the run manifest plus result JSON when available, includes a concrete results section in the rendered report, writes Markdown for `.md`, HTML for `.html`, and writes a small archival report bundle when pointed at a directory-like output path.
- `api serve` now loads one chosen dataset into memory at startup, preloads and/or compiles the requested profiles into reusable prepared routing engines, exposes JSON endpoints under `/v1/` for route, OD, matrix, and profile metadata, and defaults requests to the configured startup profile when `profile_id` is omitted.
- Summary-only route execution now omits `node_path` and `edge_path` from route JSON unless geometry, segment rows, or other richer route detail is requested, reducing hot-path API payload size.
- The current exact kernel now models compiled edge costs, ferry durations and boarding costs, geometric turn penalties, traffic-signal and roundabout-entry penalties, persisted prohibited turn sequences, and mode-specific access semantics; many-to-many acceleration and contraction-based speedups remain pending.
