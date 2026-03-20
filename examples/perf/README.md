# Performance Examples

This directory holds deterministic benchmark inputs for repeatable performance runs.

Recommended corpora:

- `ile_de_france_routes.csv`: medium and long-range car routes for regional benchmarking
- `../requests/paris_routes.csv`: short urban routes for city-scale benchmarking

Run the reproducible API benchmark harness with a fixed corpus:

```bash
CORPUS_PATH=examples/perf/ile_de_france_routes.csv \
DATASET_ID=ile_2026_03 \
PROFILE_PATH=examples/profiles/car_research_v1.yml \
scripts/perf_api_route_corpus.sh
```

The corpus format is:

```text
id,source_x,source_y,target_x,target_y
```

The benchmark runner defaults to summary-only route responses so the timing focuses on the routing path rather than geometry serialization. On the current API path that also omits `node_path` and `edge_path` from JSON, keeping the benchmark aligned with the intended low-payload warm-query mode.
