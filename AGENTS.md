# Repository Guidelines

## Project Structure & Module Organization

This repository is a Rust workspace centered on one shared routing engine with two front ends.

- `crates/core`: graph primitives, IDs, and shared network types
- `crates/ingest`: dataset import and future OSM PBF topology build
- `crates/profile`: YAML/TOML profile schema and validation
- `crates/query`: route, OD, matrix, and experiment request models
- `crates/persist`: local cache/state layout under `.netan/`
- `crates/report`: dataset/profile/run manifests and report rendering
- `crates/cli`: `netan` CLI entry point
- `crates/gui`: minimal native `egui` shell
- `examples/`: sample profiles and requests
- `datasets/`: local OSM extracts for development only

Track roadmap and implementation status in `PROGRESS.md`.

## Build, Test, and Development Commands

- `cargo fmt --all`: format the entire workspace
- `cargo check`: fast compile check across all crates
- `cargo test`: run unit and integration tests when present
- `cargo run -p netan-cli -- profile validate examples/profiles/car_research_v1.yml`: validate a sample profile
- `cargo run -p netan-cli -- dataset import datasets/groningen-260317.osm.pbf --name groningen_2026_03`: register a local dataset
- `cargo run -p netan-cli -- cache list`: inspect cached manifests

## Coding Style & Naming Conventions

Use default Rust style with `cargo fmt`; do not hand-format around it. Prefer small modules, explicit types, and deterministic behavior. Use `snake_case` for functions, files, and modules; `PascalCase` for structs and enums; `SCREAMING_SNAKE_CASE` for constants. Keep serialized config fields stable and human-readable because CLI and GUI must share the exact same schema.

## Testing Guidelines

Use Rust’s built-in test framework. Put focused unit tests next to the code they cover and cross-crate workflow tests under each crate’s `tests/` directory when added. Name tests by behavior, for example `validates_empty_profile_id` or `imports_dataset_manifest`. Prefer deterministic fixtures and small local extracts over large datasets.

## Commit & Pull Request Guidelines

There is no existing commit history yet. Start with short, imperative commit subjects such as `Add profile manifest writer` or `Scaffold route request schema`. Keep commits scoped to one logical change. PRs should include:

- a clear summary of behavior changed
- affected crates and commands used for verification
- linked issue or research task when applicable
- screenshots only for GUI-visible changes

## Reproducibility & Configuration

Do not introduce hidden GUI-only state or opaque defaults. Any user-facing option should serialize to file-backed config or manifest output. Preserve provenance fields, hashes, and version metadata so runs remain defensible for academic use.
