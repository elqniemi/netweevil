# netweevil Extension Plan — 3D, Time-Dependent, Multilayer Pedestrian + Transit Routing

**Plan date:** 10 July 2026
**Target repo:** `../netan` (netweevil)
**Driving project:** `HONG_KONG_ROUTING_PROJECT_PLAN.md` (The Sheltered City / The Station Is Not a Point / The Thermal Lifeline Network)

This plan is grounded in a code audit of netweevil (structs, seams and file references below are verified against the current tree) and a field-level audit of the Hong Kong datasets (see the project plan §5). Netweevil stays a **generic** engine: nothing Hong-Kong-specific is hardcoded. HK specifics live in ingest-mapping configs, scenario files and preprocessing in the `hong-kong-analysis` repo.

---

## 1. Where netweevil is today (audit summary)

| Area | Current state | Consequence for this project |
|---|---|---|
| Graph | `TopologyNode { node_id, lon, lat }` — strictly 2D (`crates/core/src/graph.rs:329`). Fixed `DirectedEdge` struct + columnar `TopologyEdgeLayers` (routing / profile / presentation columns, `graph.rs:151-264`). No tag map, no extensible attributes. Per-edge geometry is not stored: ways are split at **every** shape vertex, so each edge is a straight segment between two nodes. | No place for Z, gradient, covered/indoor/level, opening hours. But the split-at-every-vertex model means we can preserve source geometry losslessly as segment chains — no geometry buffer needed. |
| Ingest | Two formats only (OSM PBF, Overture parquet), dispatched by a `match` on `SourceFormat` into a shared `ScanOutput { pending_ways, node_coords: FxHashMap<i64,(f64,f64)>, … }` (`crates/ingest/src/scan.rs:47`, `topology.rs:27-33`). Node identity is **by source node id only** — no coordinate snapping exists. | Clean seam for a new reader. GeoPackage sources have no shared node ids → coordinate-quantization node building must be added. `rusqlite` is already a workspace dep (report crate writes GPKG), so no new native dependency. |
| Profiles | YAML → **scalar** generalized cost precomputed per edge (`CompiledEdgeMetric { travel_time_s, generalized_cost }`). Tag matchers accept a **closed key set** (`highway, road_class, surface, smoothness, route, toll` — `crates/profile/src/edge_cost.rs:68-82`). No time-of-day anywhere. | Need open attribute matching, slope-aware speed, facility costs (stairs/escalator/lift), named cost components. |
| Query | Edge-based CCH + bidirectional Dijkstra + A* (restriction automaton). Route, batch/OD/matrix, service areas/isochrones, accessibility, alternatives. Costs frozen into `RoutingGraph.edge_costs: Vec<f64>` at build time (`crates/query/src/engine/graph.rs:14-33`); `RouteRequest` has no departure time. No centrality, no Pareto. | Time dependence must be threaded through the exact engines; CCH keeps serving static queries. Betweenness and constrained/bi-criteria search are new analysis code on existing machinery. |
| Transit | GTFS import (incl. `frequencies.txt`; **no** `pathways.txt`/`transfers.txt`), Connection Scan router — the only departure-time-aware code in the system. Transfers are straight-line geometric walks; street access legs can use the street network (`street_access: "network"`). | Good base. Needs explicit stop↔node binding, street-network (indoor-graph) transfer times, and tolerant stop_times parsing for the HK feed. |
| Persist | Custom sectioned binary, raw positional columns via `bytemuck`/mmap, per-bundle `schema_version`; changes ⇒ bump + re-import (the project's established migration norm). | Every struct change below lands as new sections + version bumps. Re-import is acceptable. |
| Missing entirely | Z/elevation, slope, opening hours, multilayer/indoor, departure time (street), betweenness, Pareto/multi-objective. | This plan. |

---

## 2. Design principles

1. **Generic engine, configured specificity.** New capabilities are expressed as: an ingest **mapping config** (which source fields mean what), **scenario overlay files** (runtime state changes), and **temporal overlay series** (externally computed per-edge time series such as shade). Hong Kong is just the first user of each.
2. **Lossless by construction.**
   - Split source polylines at every vertex (existing model) → geometry preserved exactly as node chains, now with Z.
   - Every source field is kept: mapped fields become typed semantic attributes; **all remaining fields** go into a per-source-feature attribute table (typed columns, string-interned), with each edge carrying a `u32` index to its source feature row. No duplication, nothing dropped.
   - Source feature id (e.g. `PedestrianRouteID`) is retained in the existing `source_way_id: i64`.
   - A round-trip audit command re-exports retained attributes and diffs them against the source.
3. **Preprocessing does conversion, netweevil does routing.** CRS reprojection (EPSG:2326+5738 → WGS84+Z metres), FileGDB→GPKG conversion, GTFS repair/synthesis, and solar/shadow computation happen in the analysis repo with GDAL/Python. Netweevil gains exactly one new reader (GeoPackage) and stays proj-free.
4. **Static stays fast, temporal stays exact.** CCH continues to serve all static queries untouched. Time-dependent queries run on the exact Dijkstra/A* engines — the HK pedestrian graph (~0.5M source features → a few million directed segment edges) is well within plain-Dijkstra one-to-many scale. Time-bucketed CCH customizations are a later optimization, for which the existing per-metric weight sets (`crates/core/src/metrics.rs:53-65`) are the precedent.
5. **Scalar optimization, vector accounting.** The optimizer keeps minimizing one generalized cost (existing model), but per-edge **named cost components** are compiled alongside it and accumulated into every result, so a route always reports its full vector (time, uncovered time, ascent, sun-minutes, …). True bi-criteria search is added only where a hard budget/Pareto answer is required (W6).

---

## 3. Workstreams

### W1 — 3D core and persistence (~1–1.5 wk)

- `TopologyNode` gains `z: f64` (HKPD metres); `ScanOutput.node_coords` widens to `(f64, f64, f64)` (OSM/Overture readers fill `z = 0` / NaN-means-unknown).
- Edge derived quantities computed at topology build: `length_m` becomes true 3D length; new per-directed-edge `ascent_m: f32`, `descent_m: f32` (from endpoint Z); gradient is derived on demand (`Δz / horizontal`), not stored.
- Persist: new raw sections (node z, ascent/descent columns), bump `TOPOLOGY_BUNDLE_SCHEMA_VERSION`.
- Exports (`crates/report/src/output/*`, `crates/api/src/geojson.rs`) emit Z coordinates when present; GPKG writer upgrades to 3D WKB.
- Snapping (`crates/query/src/snapping.rs`): `SnapOptions` gains optional `z_window_m` and attribute filters (e.g. only `location=outdoor`) so a street-level origin cannot snap to a tunnel edge two levels below.

### W2 — Generic GeoPackage ingest with mapping config (~2–3 wk)

- New `SourceFormat::GeoPackage`; reader on `rusqlite` + a Z-aware WKB parser (MultiLineString Z / LineString Z). New match arm produces the same `ScanOutput` → `build_topology_from_scan` stays format-agnostic.
- **Coordinate-quantization node building**: synthetic node ids from `(lon, lat, z)` quantized at fixed epsilons (~1e-9°, 1 cm Z). Identical source coordinates weld automatically — verified against HK data, where indoor and outdoor networks join only through ~900 exactly coincident portal vertices. Quantization runs on post-reprojection coordinates; deterministic reprojection preserves coincidence.
- **Multi-source datasets**: `dataset import` accepts several inputs into one dataset (outdoor + indoor GPKG) so portals weld into one graph. Source feature id namespaces must be disjoint (they are: outdoor ≤ 999,002,334 < indoor ≥ 1,000,482,918); the importer verifies this.
- **Ingest mapping YAML** (per layer):
  - field → semantic role: `feature_id`, `direction` (with value decode: −1/0/1), `access_schedule` (JSON opening-hours field + polarity field), `name` (multi-language), `class` (with a value→`HighwayClass`-analog decode table), plus boolean/enum roles used by costing (`covered`, `indoor_location`, `wheelchair_access`, `wheelchair_barrier`, `grouping ids` like terminal/building/floor);
  - enum decode tables (GPKG coded-value domains are read from `gpkg_data_column_constraints` automatically where present, overridable in the mapping);
  - `retain: all` — every unmapped field lands in the feature attribute table.
- **Feature attribute table**: one row per source feature; typed columns (interned string / f64 / i64 / bool / raw JSON); edges carry `feature_row: u32`. New persist sections.
- **Direction materialization**: `direction=0` → both directed edges; `±1` → one. Any feature named by a scenario direction-override (W4) gets both directed edges materialized so temporal state can gate them.
- Opening-hours parser for the mapped schedule role: JSON array of `{day_code, from_time, to_time}` with integer HHMM (`930` = 09:30), `from > to` wraps midnight, `ED` = every day, `OPEN`/`CLOSE` polarity. Parsed into the W4 temporal-rule representation at import.
- Lossless audit: `dataset audit --against <source.gpkg>` re-derives features from segments + attribute table and reports any field/vertex/Z drift.

### W3 — Pedestrian costing: attributes, slope, facilities, components (~2 wk)

- **Open attribute matching**: profile matchers may reference any semantic role or retained attribute by name; validated at compile against the dataset's attribute registry (replaces the closed list in `ensure_supported_keys`).
- **Slope model**: profile section `slope_model: {kind: tobler | piecewise, params…}` adjusting walking speed from gradient, asymmetric uphill/downhill (uses W1 ascent/descent).
- **Facility costs** per class value: stairs (vertical-metre speed + optional stair-equivalent burden component), escalator (fixed conveyor speed ± walking, boarding penalty; direction honored), lift (`wait_s` + vertical speed; produces a `lift_wait` component), travelator, ramp. Wheelchair/step-free profiles use exclude rules on `feature_type`/`wheelchair_barrier`.
- **Named cost components**: profile declares components as weighted expressions over per-edge quantities, e.g.
  `uncovered_time = travel_time × (covered == false)`, `ascent = ascent_m`, `sun_time = travel_time × (1 − overlay:shade_fraction)`.
  Compilation emits one `Vec<f32>` per component alongside `generalized_cost = travel_time + Σ weight_i × component_i`. Results (`RouteSummary`, service areas, matrices) report accumulated component vectors.
- Compiled-profile `schema_version` bump.

### W4 — Time dependence (~2–3 wk)

- **Temporal rules** (from W2 schedule parsing and scenario files): per-edge optional ref into a rule table; a rule set = `{day mask (Mon…Sun + public-holiday), minute intervals (midnight-wrapping), effect}` with effects: `Closed`, `OpenOnly` (closed outside intervals), `ForwardOnly`, `BackwardOnly`, `SpeedFactor(f32)`.
- **Scenario overlays** (runtime files, no re-import): keyed by source feature id — force-close (asset failure), direction schedule override (e.g. the Central–Mid-Levels escalator: downhill 06:00–10:00, uphill 10:00–24:00, which is *not* encoded in the source data), modified hours. Loaded per request/batch.
- **Calendar**: requests carry a departure datetime; the dataset can attach a holiday calendar file (dates list) resolving the PH bit.
- **Time-dependent routing**: `departure_time` (and arrive-by) on street requests, mirroring the transit request's time block. TD Dijkstra evaluates rules **at edge-entry time**; per-profile waiting policy `{allow_wait, max_wait_s}` — waiting for an opening keeps the model FIFO (label = max(arrival, next-open) + traversal). Runs on the exact engines; requests without a time keep using CCH.
- **TD one-to-many**: service areas / isochrones / accessibility / matrices with departure time (isochrone sequences around a boundary time, e.g. 09:40–10:20 in 5-min steps).
- **Temporal overlays** (continuous values): piecewise-constant per-feature time series loaded from Parquet/CSV (`feature_id, t_start, t_end, {name: value}`), e.g. `shade_fraction` computed externally by the solar pipeline. Components (W3) may reference `overlay:<name>` evaluated at edge-entry time. This is the entire solar integration surface — netweevil never computes shadows.

### W5 — Transit fusion and stations (~2–3 wk)

- **GTFS import hardening** for the HK feed: interpolate blank intermediate `stop_times` (only first stops carry times; frequencies-based trips), tolerate BOM, sentinel calendar spans (2020–2099) with real service in `calendar_dates`, >24:00 times (already supported).
- **Stop↔node binding table**: optional per-feed file `stop_id → node/edge (or explicit coordinate + z + attribute filter)`. MTR platform stops bind to indoor platform-level nodes; without a binding, today's 2D nearest-snap remains the fallback.
- **Network transfer times**: a netweevil command precomputes stop-to-stop transfer walk times **through the street/indoor graph** (profile-aware — standard vs step-free) and the CSA consumes this table instead of straight-line distance. MTR interchanges thus price the real corridor/escalator path, and a step-free profile prices the lift path.
- **Door-to-door**: existing `street_access: "network"` + bindings yields building-entrance → portal → concourse → platform → rail → … journeys; results expose the in-station legs (edge path + components) rather than a teleport.
- The MTR heavy-rail/light-rail **schedule itself is synthesized as a GTFS feed in preprocessing** (headways → `frequencies.txt`; platform stop coordinates from the indoor network) — no new schedule format in netweevil.

### W6 — Analysis layer (~2 wk)

- **Demand-weighted edge betweenness / usage**: one-to-many shortest-path trees from weighted origins to a destination set; accumulate demand over edges; static or TD; per-edge scores exported (GPKG/GeoJSON). This powers "vertical criticality".
- **Scenario batch & diff**: run any analysis under a list of scenario overlays (single-asset failures, top-k removals, mall closure = all features sharing a grouping id) and emit per-scenario diffs: accessibility loss, disconnected demand, rerouting burden.
- **Budget-constrained and bi-criteria routes**: constraint on one named component (e.g. `uncovered_time ≤ 60 s`, or `= 0` for fully-covered) via bi-criteria label-setting with dominance pruning; same machinery emits the two-component Pareto frontier. Pedestrian-scale graphs keep this tractable; guard with label limits.

### W7 — Surface: CLI, API, exports, QGIS (~1–2 wk)

- CLI: `dataset import --format gpkg --mapping m.yml <src…>`, `dataset audit`, `--departure-time` / `--scenario` / `--overlay` on analyze commands, `transit transfers build`, `analyze betweenness`, `analyze scenario-batch`.
- API: same additions on `/v1/*` requests; component vectors and scenario ids in responses.
- Exports: 3D GeoJSON/GPKG; isochrone **temporal frame** outputs (reuse the simulate crate's temporal GeoJSON pattern); component columns in all tabular outputs.
- QGIS plugin: load 3D outputs and temporal frames; scenario picker passthrough.

---

## 4. What each Hong Kong analysis needs (traceability)

| Analysis requirement | Workstreams |
|---|---|
| Routable unified outdoor+indoor 3D graph, lossless attributes | W1, W2 |
| Slope/stair/escalator/lift-honest walking times; step-free profiles | W3 |
| Covered vs fastest; exposure budgets; shelter premium | W3 (components), W6 (constraints) |
| 10 a.m. escalator flip; opening-hours closures; TD isochrones | W4 |
| Fare-gate (paid-area) feasibility | W2 (`indoor_location=paid` attribute) + W3 (profile exclude) |
| Platform-to-exit, transfer atlas, door-to-door via MTR | W5 |
| Moving-shadow / thermal-dose routing | W4 (overlays) + W3 (components); shadows computed in the analysis repo |
| Criticality, failure cascades, mall/asset closures | W6 (+W4 scenarios) |
| Refuge accessibility under exposure + step-free constraints | W4, W6 |
| Intervention testing (candidate links) | W2 (import candidate edges as a delta source) + W6 (diff) |
| 3D visual outputs, isochrone animations | W7 |

## 5. Sequencing, testing, risks

**Order:** W1 → W2 → W3 (milestone: correct static HK routes) → W4 (milestone: the 10 a.m. flip reproduces) → W5 → W6 → W7 (W7 items land incrementally with each). Roughly 3–4 months of focused single-developer work; W5 can proceed in parallel with W4 after W2.

**Testing** (follows the repo's fixture style — synthetic in-crate topologies, e.g. `crates/query/src/tests/fixtures.rs`):
- a synthetic two-level mini-station fixture (escalator with schedule, lift, paid gate, covered/uncovered edges) exercising W1–W5 end to end;
- golden tests for the opening-hours parser (HHMM wrap, `ED`, OPEN/CLOSE polarity) and direction materialization;
- integration checks against the real data: portal weld count (expect ~900 indoor↔outdoor junctions), station grouping counts, lossless round-trip audit;
- TD regression: boundary-time routes at 09:59/10:01 across the reversal rule.

**Risks & mitigations:**
- *Edge-count growth* (every vertex a node, both directions): u32 ids give 4.29B headroom; measured HK inputs are a few million edges — fine, but the importer reports counts.
- *CCH incompatibility with TD*: accepted by design (exact engines); if territory-wide TD batch matrices become slow, add time-bucketed CCH customizations using the existing multi-metric weight precedent.
- *2D snapping errors in multilayer areas*: mitigated by W1 snap filters; QA queries in the analysis repo verify snapped levels.
- *Bundle churn*: several schema bumps; batch them per release to limit re-imports.
