# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
- Workspace toolchain aligned on Rust 1.96.1.
