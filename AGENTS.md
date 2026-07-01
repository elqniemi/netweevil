# Repository Guidelines

## Project Overview

NetWeevil is a Rust-first local network analysis system for OSM routing research, reproducible analysis runs, GTFS transit experiments, agent-based traffic simulation, and QGIS workflows.

The main production surfaces are:

- `crates/cli`: the `netweevil` CLI.
- `crates/api`: the Axum HTTP API used by local tools and the QGIS plugin.
- `qgis_plugin/netweevil_qgis`: the QGIS 3/4 plugin.

## Repository Layout

- `crates/core`: graph primitives, topology bundles, metrics, and acceleration data types.
- `crates/ingest`: OSM import, topology build, restrictions, classification, and preprocessing.
- `crates/profile`: profile schema, validation, compilation, turn costs, and edge costs.
- `crates/query`: routing, batches, OD, matrix, service areas, accessibility, alternatives, snapping, and request/result models.
- `crates/persist`: `.netweevil/` state layout, manifests, binary bundle IO, and cache reads.
- `crates/report`: manifests, report rendering, and local exports.
- `crates/transit`: GTFS import and street-access (walk/bicycle/car) plus transit routing.
- `crates/simulate`: agent-based traffic simulation.
- `examples/`: profiles, request fixtures, API payloads, and performance route corpora.
- `scripts/`: packaging and performance helper scripts.
- `docs/`: design notes and longer-form documentation.

## Development Commands

Run the Rust checks before handing off non-trivial changes:

```bash
cargo fmt --all
cargo check
cargo test
```

Useful CLI probes:

```bash
cargo run -p netweevil-cli -- --help
cargo run -p netweevil-cli -- analyze --help
cargo run -p netweevil-cli -- api serve --help
```

Package the QGIS plugin with:

```bash
scripts/package_qgis_plugin.sh
```

Use Docker only when the container path is relevant:

```bash
docker compose build
docker compose up -d api
```

## Coding Notes

- This is a Rust 2024 workspace with `rust-version = 1.85`.
- Prefer existing crate boundaries and request/result models over introducing cross-crate shortcuts.
- Keep persisted state under `.netweevil/`; do not commit generated local state, imported datasets, or large derived bundles.
- For CLI/API behavior changes, update examples or README snippets when user-facing commands or payloads change.
- The QGIS plugin supports QGIS `3.28` through `4.99`; preserve Qt 5/Qt 6 compatibility and use `netweevil_qgis/compat.py` for version differences.
- Avoid broad UI rewrites in the QGIS plugin unless the task explicitly calls for them.

## Testing Guidance

- Prefer focused Rust tests near the crate that owns the behavior.
- For routing/profile/query changes, include fixture-based coverage using `examples/` data when practical.
- For API changes, check health/readiness and at least one affected endpoint against a local `api serve` run when feasible.
- For QGIS plugin changes, package the plugin and do static import/compatibility checks where a full QGIS runtime is unavailable.

