# Contributing to NetWeevil

Thanks for your interest in improving NetWeevil. This document covers the
practical parts: getting a working setup, the checks we run, and how changes
land.

## Getting set up

1. Install a Rust toolchain (rustup recommended). The workspace declares
   `rust-version = 1.96.1` and edition 2024.
2. Clone the repository and make sure the workspace builds:

   ```bash
   cargo check
   cargo test
   ```

   No datasets are required for the test suite — it runs on checked-in
   fixtures.
3. For end-to-end work (imports, routing against real data, the API, the
   QGIS plugin), download the sample datasets: see
   [`datasets/README.md`](datasets/README.md), then follow the Local
   Quickstart in the [README](README.md).

## Before you open a pull request

Run the standard checks:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Guidelines that reviewers will hold changes to:

- **Exactness is non-negotiable.** Routing results must stay exact: no
  budgets, no heuristics that can return a non-optimal path, and accelerated
  paths (CCH) must produce the same costs as the exact engine. New fast
  paths need an equivalence test against the exact engine.
- Prefer existing crate boundaries and request/result models over
  cross-crate shortcuts. See "Repository Layout" in
  [AGENTS.md](AGENTS.md) for what each crate owns.
- Keep persisted state under `.netweevil/`; never commit generated local
  state, imported datasets, or large derived bundles.
- For CLI/API behavior changes, update the README snippets and `examples/`
  payloads that document the affected commands.
- The QGIS plugin supports QGIS 3.28 through 4.99. Preserve Qt 5/Qt 6
  compatibility and route version differences through
  `qgis_plugin/netweevil_qgis/compat.py`.
- Add focused tests near the crate that owns the behavior; use `examples/`
  fixtures for routing/profile/query coverage when practical.

## Pull request flow

- Branch from `main`, keep PRs focused on one topic.
- Describe **what** changed and **why**; include benchmark numbers for
  performance claims (dataset, hardware, before/after).
- CI must pass: formatting, clippy (deny warnings), the full test suite, and
  the Docker build.
- Maintainers squash-merge unless the branch history is intentionally
  structured.

## Reporting issues

Use the issue templates. For bugs, a minimal reproduction is gold: the
dataset (or a public extract), the profile, the request payload, and the
observed vs. expected result.

## Licensing of contributions

By contributing you agree that your contributions are dual-licensed under
MIT and Apache-2.0, matching the repository license (see
[LICENSE](LICENSE)).
