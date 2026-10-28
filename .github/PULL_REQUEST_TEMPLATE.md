## What

<!-- What does this PR change? -->

## Why

<!-- What problem does it solve? Link issues with "Fixes #123" where applicable. -->

## How was it verified?

<!-- Tests added/updated, manual verification, benchmark numbers (dataset,
hardware, before/after) for performance claims. -->

## Checklist

- [ ] `cargo fmt --all` is clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `cargo test --workspace` passes
- [ ] README / `examples/` updated for user-facing CLI or API changes
- [ ] Routing changes preserve exactness (equivalence-tested against the exact engine where applicable)
