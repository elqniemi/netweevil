# NETAN Performance Implementation Plan

## Goal

Reach region-wide and countrywide routing in the `100-500 ms` range for warm single-route queries, with sustained `100+ rps`, while preserving reproducibility and correctness.

This plan treats performance as an architectural change, not a tuning pass. The current exact engine is useful as the reference oracle, but it is not the long-term path to Netherlands-scale latency targets.

## Current Baseline

- The API already keeps one dataset and one or more compiled profiles warm in memory through `PreparedRoutingEngine`.
- The current hot path still runs exact raw-graph search per request.
- The current query graph relies on allocation-heavy structures such as nested vectors and per-query hash maps.
- OD and matrix execution currently repeat the single-pair solver.
- New topology imports now use uncompressed mmap-friendly bundles so the hot graph can load without gzip decompression overhead.

## Recommended Architecture

Use:

- `edge-based CCH` as the main route accelerator
- `many-to-many` on top of the accelerated graph for OD and matrix
- `tiles` only for cold data such as geometry, names, and snap payloads
- the current exact engine as a correctness oracle and fallback

Why this direction:

- `edge-based` is required for correctness once turn restrictions and turn penalties both matter.
- `CCH` fits the existing topology-versus-profile split: preprocess topology once, customize per compiled profile.
- `tiles` help storage, locality, and rebuild scope, but should not be the primary query accelerator.

## Design Principles

- Finish the intended routing semantics before accelerating them.
- Keep the hot routing path numeric, flat, contiguous, and mmap-friendly.
- Split hot bundles from cold bundles.
- Preserve deterministic manifests, hashes, and provenance.
- Keep an exact engine available for differential testing.

## Target Runtime Layout

### Hot Topology Bundle

- edge-based routing graph
- flat CSR-style adjacency arrays
- turn-transition tables
- minimal coordinate payload for heuristics and snapping
- uncompressed mmap-friendly binary format

### Cold Tile Bundles

- path geometry
- names and annotations
- optional snap-side spatial payloads

### Acceleration Bundle

- nested-dissection order
- CCH topology
- separator metadata
- unpacking metadata

### Profile Metric Bundle

- edge weights
- turn weights
- customized CCH weights

## Phase Plan

### Phase 0: Benchmarks First

Build deterministic benchmarks before deeper architectural work.

Status: Completed for the current topology/name split.

Deliverables:

- [x] fixed route corpora for urban, regional, and long-distance queries
- [x] a reproducible API benchmark runner
- warm `p50/p95/p99`, throughput, startup-time, and RSS measurements

Exit criteria:

- every major performance change is measured against the same corpus
- exact and accelerated engines can be compared on the same requests

### Phase 1: Freeze Correct Routing Semantics

Complete the intended route model before acceleration:

- turn restrictions
- [x] turn penalties
- [x] u-turn handling
- [x] ferry duration and boarding-cost handling
- [x] stable access semantics by mode

Status: Completed.

Completed in this phase:

- [x] compiled profile bundles now persist turn-cost configuration
- [x] the exact solver now applies geometric left, right, and u-turn penalties
- [x] the exact solver now applies configured traffic-signal and roundabout-entry penalties
- [x] ferry durations and boarding costs now flow into compiled edge travel times
- [x] ingest now applies explicit mode-specific OSM access overrides on top of base road-class access
- [x] golden tests now cover route-choice changes caused by turn penalties

Exit criteria:

- the exact solver is the oracle for the intended cost model
- golden fixtures cover restrictions and penalties together

### Phase 2: Replace the Hot Graph Layout

Replace allocation-heavy structures in the hot loop with flat arrays and reusable scratch space.

Status: Implementation complete; corpus revalidation pending.

Completed in this phase:

- [x] prepared routing graphs now build flat CSR-style adjacency arrays for traversable edges
- [x] unrestricted exact A* queries now reuse thread-local dense scratch buffers on the hot path
- [x] pairwise turn restrictions now compile into flat turn-table offsets and entries
- [x] multi-edge restriction sequences remain on the automaton fallback instead of the normal hot path
- [x] snap candidate search now keeps only the nearest bounded set instead of collecting full candidate vectors
- [x] matrix execution now pre-snaps origin and destination point sets once per batch
- [x] OD execution now reuses snapped endpoint candidates for repeated points within a batch
- [x] multi-edge restriction fallback now reuses automaton-search hash maps and heap scratch across requests
- [x] single-route candidate evaluation now reuses exact-solver results when multiple snap candidates collapse onto the same origin/destination node pair

Target structures:

- `first_out`
- `head`
- `edge_weight`
- `edge_length`
- turn-table offsets and entries

Exit criteria:

- exact bidirectional search is materially faster before any accelerator work
- warm queries allocate near zero on the hot path

### Phase 3: Split Hot and Cold Bundles

Separate routing-critical payloads from geometry and names.

Status: In progress.

Completed in this phase:

- [x] new topology imports now write uncompressed mmap-friendly `.bin` bundles
- [x] edge names now persist in a separate cold bundle and load only when route segment rows are requested

Exit criteria:

- hot startup only loads the routing bundle
- geometry and names are only touched when explicitly requested

### Phase 4: Edge-Based Expansion

Persist explicit edge-to-edge routing state with legal turn transitions and turn costs.

Status: In progress.

Completed in this phase:

- [x] prepared routing graphs now precompute explicit legal edge-to-edge transitions in memory
- [x] normal exact search now expands precomputed turn-adjusted edge transitions instead of recomputing turns in the hot loop
- [x] topology imports now persist raw edge-based adjacency and successor topology for startup reuse

Exit criteria:

- exact edge-based solver matches expected paths on turn-cost and restriction fixtures
- the automaton fallback is no longer the normal hot-path model

### Phase 5: Dataset-Level CCH Preprocessing

At dataset import time:

- compute nested-dissection ordering
- build the CCH topology once per dataset
- persist the acceleration bundle

Exit criteria:

- dataset import writes raw topology, cold tiles, and the CCH topology bundle

### Phase 6: Profile-Level Customization

At profile compile time:

- compile edge weights
- compile turn weights
- customize the CCH
- persist customized weights

Exit criteria:

- recompiled profiles do not rebuild dataset topology
- customization is fast enough for iterative profile work

### Phase 7: Accelerated Query Engine

Implement:

- edge snapping with phantom-node handling
- CCH route queries
- path unpacking
- on-demand geometry fetch from cold bundles

Exit criteria:

- warm single-route country-scale queries hit target latency
- unpacked paths remain correct under penalties and restrictions

### Phase 8: Service Throughput

Make the runtime scale under real load:

- dedicated CPU worker pool for routing
- per-thread scratch reuse
- shared read-only prepared engines
- summary-only response mode as the performance default

Exit criteria:

- sustained `100+ rps` on warm summary-only route queries

### Phase 9: Many-to-Many

Replace repeated single-pair solves for OD and matrix with many-to-many acceleration.

Exit criteria:

- OD and matrix are materially faster than repeated route solves

### Phase 10: Differential Verification

Protect correctness with:

- golden fixtures
- randomized differential tests on small and medium extracts
- sampled differential tests on larger datasets
- path validity checks
- manifest-level provenance for accelerator bundles

Exit criteria:

- accelerated results are defensibly consistent with the exact engine

## Where Tiles Fit

Use tiles for:

- geometry
- names
- snap/search locality
- rebuild scope

Do not use naive geographic tiles as the routing accelerator itself. Routing partitions should be graph-aware.

## Recommended Execution Order

1. Add deterministic benchmarks and route corpora.
2. Finish the exact route model with turn penalties.
3. Refactor the hot graph to CSR-style arrays and scratch reuse.
4. Split hot and cold bundles.
5. Build explicit edge-based routing state.
6. Add dataset-level CCH preprocessing.
7. Add profile-level customization.
8. Replace the route query path.
9. Add many-to-many acceleration.
10. Tune service concurrency and keep differential verification in place.

## Immediate Tasks

These are the first concrete tasks to execute now:

1. [x] Check in deterministic route corpora and a benchmark runner.
2. [x] Persist new topology bundles as uncompressed mmap-friendly binaries.
3. [x] Keep gzip topology reads for backward compatibility with existing imported datasets.
4. [x] Use the exact engine as the benchmark oracle while the accelerated stack is still under construction.

## Completed So Far

- [x] Added deterministic benchmark inputs under `examples/perf/`.
- [x] Added a reproducible route-corpus API benchmark runner at `scripts/perf_api_route_corpus.sh`.
- [x] Switched new topology imports to uncompressed mmap-friendly bundles.
- [x] Dropped legacy gzip topology bundle support from the execution path; datasets must now be re-imported into the current format.
- [x] Split edge names into a separate cold bundle and require the current dataset format for segment-name loading.
- [x] Extended compiled profile bundles to persist turn-cost configuration.
- [x] Persisted dataset-level edge-based topology arrays so runtime graph preparation can reuse bundled adjacency topology instead of rebuilding it from edges.
- [x] Preserved backward-compatible reads for legacy compiled metric bundles.
- [x] Implemented exact-solver turn penalties for geometric left, right, and u-turn transitions.
- [x] Replaced prepared-engine adjacency lists with flat CSR-style arrays.
- [x] Added thread-local dense scratch reuse for unrestricted exact A* queries.
- [x] Added flat turn-table fast-path handling for pairwise prohibited turns.
- [x] Reduced snap candidate allocation by keeping only the nearest bounded candidate set during search.
- [x] Removed repeated matrix-cell snapping by precomputing batch snap candidates.
- [x] Added per-batch snap candidate reuse for repeated OD endpoints.
- [x] Reused scratch allocations in the multi-edge automaton fallback path.
- [x] Avoided duplicate exact solves for repeated snapped node pairs during single-route candidate evaluation.
- [x] Summary-only route responses now omit `node_path` and `edge_path` payloads unless richer route detail is requested.
