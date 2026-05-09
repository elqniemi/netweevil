# NETWEEVIL Advanced Analysis Implementation Plan

## Goal

Add a second analysis layer on top of the current exact routing engine:

- service areas with network and polygon outputs
- explicit disconnected-network handling
- opt-in route failure modes for "illegal but useful" fallback analysis
- QGIS plugin support for all of the above

This plan treats these as shared core capabilities, not QGIS-only features. CLI, API, reports, and QGIS should all use the same serialized request model and produce explicit provenance.

## Design Principles

- Keep default behavior strict and legally valid.
- Make every fallback mode explicit in request payloads, manifests, and warnings.
- Prefer one shared execution model across CLI, API, and QGIS.
- Preserve reproducibility: no hidden plugin-only flags or implied UI behavior.
- Distinguish "reachable in the legal graph" from "reachable with fallback".
- Return machine-readable diagnostics for unreachable and degraded solves.

## Phase 0: Shared Design And Data Contracts

Status: Completed.

Deliverables:

- [x] Define a new `service_area` analysis surface in `crates/query` with JSON/YAML schemas.
- [x] Extend the API capability document and CLI command tree to advertise the new analysis type.
- [x] Add a shared `ConnectivityPolicy` / `FallbackPolicy` model for route, OD, matrix, and service area requests.
- [x] Define a shared diagnostics payload for:
  - disconnected component detection
  - snap succeeded but legal route failed
  - fallback mode used
  - suggested next flags or remedies
- [x] Decide which fields belong in the request versus in profile defaults versus in run manifests.
- [x] Add examples for every new request family under `examples/requests/`.

Exit criteria:

- every feature below has a serialized schema first
- CLI, API, and QGIS can point at the same request vocabulary

Field placement decision:

- Request payloads own per-run advanced analysis intent: service-area parameters, connectivity policy, and fallback policy.
- Profiles continue to own reusable mode/cost/default-return behavior, not hidden disconnected/failure-mode execution state.
- Run manifests copy connectivity/fallback policy as provenance so a realized run remains auditable even if the source request moves or changes later.

## Phase 1: Service Areas

Status: Completed for the shared backend surfaces. CLI, API, reports, and file writers now execute service-area analyses with network and polygon outputs; QGIS-specific surfacing remains Phase 4 work.

### 1.1 Request And Result Schema

- [x] Add `ServiceAreaRequest` with:
  - one or more origin points
  - one or more thresholds, with units explicit as distance or travel time
  - snap options
  - output mode: `network`, `polygon`, or `both`
  - band mode: `cumulative` or `ring`
  - boundary mode: `overlap`, `cut_at_boundary`
  - multi-origin mode: `merge`, `overlap`, `cut`
  - polygon generation options, including hull aggressiveness / simplification controls
  - return options for geometry, attributes, per-threshold summaries, and diagnostics
- [x] Add `ServiceAreaResult` with:
  - per-origin and/or merged areas
  - per-threshold metadata
  - geometry type metadata
  - reachable network length and edge counts
  - warnings and diagnostics

### 1.2 Execution Engine

- [x] Implement one-to-many expansion from each snapped origin over the exact graph until each threshold limit.
- [x] Reuse a single expansion to emit multiple thresholds in one request instead of rerunning per band.
- [x] Support both distance-limited and time-limited expansion.
- [x] Build ring outputs by differencing consecutive bands.
- [x] Preserve partial-edge handling at the threshold frontier so service areas can stop inside an edge.
- [x] Support batched multi-origin execution without duplicating identical snap or tree work.

### 1.3 Geometry Products

- [x] Implement network output as the reachable edge subset with threshold, origin, and fallback attributes.
- [x] Implement polygon output from reachable network geometry with configurable hull aggressiveness.
- [x] Support:
  - overlapping polygons
  - clipped non-overlapping polygons
  - ring polygons by threshold band
- [x] Define deterministic polygon clipping / dissolve rules so repeated runs are stable.
- [x] Ensure polygon output stays valid when reachable areas are fragmented.

### 1.4 Writers, CLI, API, Reports

- [x] Add `analyze service-area` to the CLI.
- [x] Add `/v1/service-area` to the API, with JSON and GeoJSON response modes.
- [x] Extend result writers for GeoJSON, GeoPackage, Parquet, and JSON service-area outputs.
- [x] Extend run manifests and report rendering to summarize thresholds, output mode, merge mode, and fallback usage.

### 1.5 Validation And Tests

- [x] Unit tests for:
  - threshold frontier behavior
  - ring vs cumulative bands
  - merge / overlap / cut semantics
  - polygon validity on fragmented reachable subnetworks
- [x] Golden fixtures for one origin, many origins, many thresholds, and mixed distance/time requests.
- [x] Performance check that multi-threshold service areas reuse one expansion per origin.

Exit criteria:

- one request can generate multiple thresholds and multiple origins in one run
- both network and polygon outputs are supported
- band and multi-origin semantics are explicit and tested

## Phase 2: Disconnected-Network Handling

Status: Completed.

### 2.1 Connectivity Metadata

- [x] Persist connected-component labels during dataset import.
- [x] Decide whether to store weak components only or both weak and strongly connected components.
- [x] Add component metadata to topology manifests and API service info where useful.
- [x] Add helper lookups so snapped points can be annotated with component membership cheaply.

### 2.2 Request Policies

- [x] Add explicit disconnected-area options such as:
  - `strict`
  - `ignore_unreachable`
  - `hop_origin_to_nearest_reachable_component`
  - `hop_destination_to_nearest_reachable_component`
  - `hop_either_end`
- [x] Define hop semantics precisely:
  - whether the hop is straight-line only
  - whether hop distance affects reported network cost
  - whether hop distance is reported separately
  - maximum allowed hop distance / time
- [x] Extend route, OD, and matrix analyses to either fail, hop, or mark disconnected cases explicitly.
- [x] Apply the same skip/fail/mark disconnected-case semantics to service-area execution once that engine exists.

### 2.3 Solver And Diagnostics

- [x] Check component mismatch before full route search when possible.
- [x] Short-circuit obvious unreachable pairs with a diagnostic instead of generic "no route found".
- [x] If hop fallback is enabled, stitch a route result that separates:
  - legal network travel
  - non-network hop segment(s)
  - any remaining unreachable state
- [x] Add warnings and suggestions, for example:
  - points snapped to different components
  - nearest reachable component is beyond current snap / hop tolerance
  - consider enabling a specific disconnected-area flag

### 2.4 Writers And UX

- [x] Extend route, OD, and matrix result schemas so outputs expose:
  - component ids when relevant
  - hop distances
  - whether a fallback was required
  - whether the result is legal, degraded, or partial
- [x] Extend the same connectivity fields to service-area outputs once service-area execution exists.
- [x] Surface these diagnostics in CLI stderr / logs, API JSON, and reports.
- [x] Surface the same diagnostics in QGIS plugin messages.

### 2.5 Validation And Tests

- [x] Add fixtures with island subnetworks, ferry-only subnetworks, and one-way-isolated subnetworks.
- [x] Test strict failure, ignored unreachable cases, and each hop policy.
- [x] Test that legal network cost remains unchanged when a hop segment is configured to be informational only.

Exit criteria:

- unreachable cases are classified instead of opaque
- disconnected-area fallback is explicit, bounded, and auditable

Decision note:

- Store weak connected-component labels only for now. They are enough to classify disconnected snap pairs cheaply across route, OD, matrix, and service-area flows, and they keep dataset metadata stable until a strong-component use case exists.

## Phase 3: Failure Modes For Otherwise-Unsolvable Routes

Status: Completed for route, OD, and matrix execution. Service-area requests explicitly reject failure-mode fallbacks until a defensible illegal-movement service-area model exists.

These are deliberately unsafe analysis modes and must stay opt-in.

### 3.1 Policy Model

- [x] Add explicit failure-mode flags such as:
  - `allow_reverse_oneway`
  - `allow_illegal_turn`
  - `ignore_turn_restrictions`
  - `allow_uturn_where_normally_forbidden`
- [x] Keep these separate from disconnected-area hop policies so provenance stays readable.
- [x] Add optional penalty controls so illegal movements can be discouraged instead of treated as free.

### 3.2 Solver Behavior

- [x] Extend routing-graph construction or query-time expansion to expose illegal transitions only when requested.
- [x] Tag every illegal traversal used in the final route with its violation type.
- [x] Return route summaries that separate:
  - legal network cost
  - penalty cost from illegal movements
  - count and type of violations used
- [x] Ensure normal routing remains unchanged when no failure modes are enabled.

### 3.3 Safeguards

- [x] Require warnings in every result when a failure mode was enabled.
- [x] Mark such outputs as degraded / noncompliant in manifests and reports.
- [x] Consider hard caps, for example max illegal distance or max illegal turns, to prevent pathological routes.
- [x] Decide whether service areas should ever support illegal-movement modes; default recommendation is no until route semantics are stable.

### 3.4 Validation And Tests

- [x] Add focused fixtures for:
  - one-way dead-end recovery
  - blocked legal turn recovered by illegal turn
  - restriction-only failure recovered by ignoring restrictions
- [x] Test that the strict engine still fails on the same fixtures by default.
- [x] Test that warnings and violation counts are persisted in all output formats.

Exit criteria:

- degraded routing is possible only when explicitly requested
- every violation is visible in results and manifests

## Phase 4: QGIS Plugin And UX

Status: Not started.

### 4.1 Service Area UX

- [ ] Add a dedicated `Service Area` tab instead of overloading the existing route or batch tabs.
- [ ] Support:
  - map-picked origins
  - origins from selected point layers
  - threshold lists entered once per request
  - distance versus time units
  - network versus polygon output
  - merge / overlap / cut choices
  - ring versus cumulative bands
  - hull aggressiveness controls with sensible presets
- [ ] Style outputs differently for:
  - thresholds
  - ring bands
  - multi-origin results
  - network versus polygon layers

### 4.2 Route / Batch Advanced Controls

- [ ] Add an advanced section for disconnected-area and failure-mode options.
- [ ] Keep unsafe options collapsed, clearly labeled, and off by default.
- [ ] Show preflight warnings before sending unsafe requests.
- [ ] Surface route-not-found diagnostics with suggested next actions instead of a generic message box.

### 4.3 Layer Loading And Inspection

- [ ] Load service-area results as separate layers or grouped sublayers by threshold and geometry mode.
- [ ] Preserve key attributes needed for filtering and symbolization in QGIS.
- [ ] Add quick actions for zooming to output and reusing prior origins / thresholds.
- [ ] Make plugin log output explicit when a result is strict, degraded, partial, or fallback-assisted.

### 4.4 Persistence And Compatibility

- [ ] Persist new UI settings through `QSettings` without inventing hidden execution state.
- [ ] Keep saved request JSON compatible with CLI and API payloads.
- [ ] Update plugin README with new flows and screenshots once behavior stabilizes.

### 4.5 Validation

- [ ] Manual QGIS acceptance pass for route, OD, matrix, and service area flows.
- [ ] Verify QGIS 3 and QGIS 4 compatibility for new widgets and layer styling.
- [ ] Verify large service-area responses do not freeze the dock unnecessarily.

Exit criteria:

- every new backend feature is reachable from the plugin
- unsafe modes are discoverable but hard to enable accidentally

## Cross-Cutting Tasks

- [x] Extend `crates/report` to summarize diagnostics, fallback usage, and degraded-result counts.
- [x] Extend `examples/` with representative service-area, disconnected, and degraded-routing inputs.
- [x] Add provenance fields to run manifests for connectivity policy and failure-mode policy.
- [ ] Decide whether batch experiment studies should support service-area scenarios in `examples/experiments/`.
- [ ] Add release notes / migration notes once request schemas settle.

## Recommended Execution Order

1. Phase 0 schema and diagnostics design
2. Phase 2 connectivity metadata and unreachable diagnostics
3. Phase 1 service-area network output
4. Phase 1 polygon generation and multi-origin semantics
5. Phase 3 unsafe failure modes
6. Phase 4 QGIS integration and UX polish

Rationale:

- disconnected diagnostics improve the existing route surface immediately
- service-area network output is easier to validate than polygons and should come first
- unsafe routing modes should build on the explicit diagnostics model instead of inventing a separate path

## Immediate Next Slice

The first implementation slice should be small and architecture-setting:

- [x] Add request/result schema types for service areas and shared diagnostics in `crates/query`.
- [x] Add connected-component labeling to dataset import and topology persistence.
- [x] Upgrade route-not-found errors into structured diagnostics before adding fallbacks.
- [x] Add API capability stubs and CLI command scaffolding for service areas, even if execution is still unimplemented.

This gives the project a stable contract before deeper engine and QGIS work starts.
