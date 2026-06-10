# Repository Guidelines

## Project Structure & Module Organization

This repository is a Rust workspace for local OSM network analysis with CLI, HTTP API, simulation, transit, reporting, and QGIS surfaces.

- `crates/core`: graph primitives, IDs, topology bundles, metrics, and acceleration data types
- `crates/ingest`: OSM PBF import, routing topology build, turn restrictions, classification, and acceleration preprocessing
- `crates/profile`: YAML/TOML profile schema, validation, compilation, progress reporting, turn costs, and edge costs
- `crates/query`: routing engines, snapping, route batches, OD, matrix, service areas, accessibility, alternatives, and request/result models
- `crates/persist`: `.netweevil/` workspace state, JSON manifests, binary bundles, mmap reads, and cache layout
- `crates/report`: run/dataset/profile manifests, summaries, report rendering, and exports to JSON/CSV/GeoJSON/GPKG/Parquet/GeoParquet where supported
- `crates/transit`: GTFS import, persisted transit bundles, pedestrian+transit routing, and transit service areas
- `crates/simulate`: agent-based traffic simulation scenarios, congestion outputs, temporal frames, and QGIS playback data
- `crates/api`: Axum HTTP API with preloadable datasets, profiles, transit feeds, and simulation endpoints
- `crates/cli`: `netweevil` binary wiring all commands
- `qgis_plugin/netweevil_qgis`: QGIS 3/4 plugin that talks to the API and loads outputs
- `examples/profiles`: maintained profile examples
- `examples/requests`: route, OD, matrix, service-area, transit, and simulation request fixtures
- `examples/api`: API-ready request payloads and generated-example helpers
- `examples/perf`: route corpora and performance helper inputs
- `scripts`: packaging and performance scripts
- `datasets`: local development extracts and GTFS archives; avoid adding large data unless explicitly needed

Ignore planning analyses and other markdown status documents when determining current production behavior. Prefer the Rust sources, manifests, scripts, Docker files, examples, and plugin metadata.

## Build, Test, and Development Commands

- `cargo fmt --all`: format the workspace
- `cargo check`: fast compile check across all crates
- `cargo test`: run unit and integration tests
- `cargo run -p netweevil-cli -- --help`: inspect the current CLI surface
- `cargo run -p netweevil-cli -- dataset import datasets/groningen-260508-routing.osm.pbf --name groningen_2026_05`: import the checked-in Groningen routing extract
- `cargo run -p netweevil-cli -- profile validate examples/profiles/car_research_v1.yml`: validate a profile
- `cargo run -p netweevil-cli -- profile compile --dataset groningen_2026_05 --profile examples/profiles/car_research_v1.yml`: compile a profile for an imported dataset
- `cargo run -p netweevil-cli -- analyze route --dataset groningen_2026_05 --profile examples/profiles/car_research_v1.yml --request examples/requests/route.json --out .netweevil/runs/example-route.geojson`: run a route analysis
- `cargo run -p netweevil-cli -- api serve --dataset groningen_2026_05 --default-profile examples/profiles/car_research_v1.yml --bind 127.0.0.1:8080`: run the local API
- `docker compose build`: build the production-style container image
- `docker compose run --rm api dataset import datasets/groningen-260508-routing.osm.pbf --name osm`: import data into the Docker named volume
- `docker compose up -d api`: serve the API from Docker on `${NETWEEVIL_PORT:-8080}`
- `scripts/package_qgis_plugin.sh`: create `dist/netweevil_qgis-<version>.zip` from plugin metadata

## Runtime State And Setup

All local state is rooted at `.netweevil/` under the current working directory. Important subdirectories are:

- `.netweevil/datasets`: dataset manifests
- `.netweevil/transit_feeds`: GTFS feed manifests
- `.netweevil/compiled_profiles`: compiled profile manifests
- `.netweevil/bundles/topology`: topology bundles
- `.netweevil/bundles/names`: cold edge-name bundles
- `.netweevil/bundles/acceleration`: dataset acceleration bundles
- `.netweevil/bundles/metrics`: compiled profile metric bundles
- `.netweevil/bundles/transit`: transit bundles
- `.netweevil/runs`: analysis and simulation outputs
- `.netweevil/reports`: report artifacts

The current fast path expects `.bin` bundles. Legacy gzip topology bundles are intentionally rejected; re-import the source `.osm.pbf` if cached state is old.

## Coding Style & Naming Conventions

Use default Rust style with `cargo fmt`. Prefer small modules, explicit types, deterministic outputs, and repository-local helpers over ad hoc parsing. Use `snake_case` for functions, files, and modules; `PascalCase` for structs and enums; `SCREAMING_SNAKE_CASE` for constants.

Serialized config, request, response, manifest, and bundle fields are user-facing contracts shared by CLI, API, reports, and QGIS. Keep them stable, human-readable where practical, and provenance-friendly.

## Testing Guidelines

Use Rust's built-in test framework. Put focused unit tests next to the code they cover and cross-crate workflow tests in each crate's `tests/` directory when needed. Name tests by behavior, for example `validates_empty_profile_id` or `imports_dataset_manifest`.

Prefer deterministic fixtures and small local extracts over large data. For changes touching API, QGIS-visible outputs, exports, profiles, persistence, or routing behavior, verify with the narrowest relevant command plus `cargo test` when feasible.

## Commit & Pull Request Guidelines

Use short, imperative commit subjects such as `Add profile manifest writer` or `Fix API matrix export`. Keep commits scoped to one logical change. PRs should include:

- behavior changed
- affected crates, plugin code, scripts, or examples
- commands used for verification
- linked issue or research task when applicable
- screenshots only for QGIS-visible changes

## Reproducibility & Configuration

Do not introduce hidden UI-only state or opaque defaults. Any user-facing option should serialize to file-backed config, request payloads, response metadata, or manifest output. Preserve source paths, hashes, timestamps, bundle IDs, profile hashes, software versions, and run metadata so results remain defensible for academic and operational use.
