# Performance Corpora

Deterministic benchmark inputs for `netweevil bench`. See the Benchmarking
section of the top-level `README.md` for the full command surface.

## Corpora

| File | Area | Notes |
| --- | --- | --- |
| `north_nl_routes.csv` | Northern Netherlands | 1000 pairs, seed 42, stratified short/medium/long |
| `ile_de_france_routes.csv` | Île-de-France | medium and long-range car routes |
| `groningen_routes.csv` | Groningen | short urban routes |
| `../requests/paris_routes.csv` | Paris | short urban routes |

## Format

Generated corpora use:

```text
id,source_lon,source_lat,target_lon,target_lat,bucket
```

`bucket` is the straight-line distance class: `short` (< 5 km), `medium`
(5–30 km), or `long` (> 30 km). Reports group latency by bucket, so mixing the
three keeps a single number from being dominated by one route length.

The optional `bucket` is derived from straight-line distance when omitted. IDs must be unique and coordinates must be finite WGS84 longitude/latitude.

## Regenerating

`bench corpus` samples coordinates inside the topology bounds (or `--bbox`),
snaps both endpoints with the engine's own snapping, and keeps a pair only when
both ends snap and share a connected component. Sampling is driven by a seeded
SplitMix64 generator, so the same `--seed`, dataset, and profile reproduce the
same file.

```bash
netweevil bench corpus \
  --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --count 1000 \
  --seed 42 \
  --out examples/perf/north_nl_routes.csv
```

Corpora are tied to the dataset they were sampled from: coordinates outside a
dataset's coverage will fail to snap at query time and be counted as failures.

## Using a corpus

```bash
# In-process, 16 threads over one shared engine.
netweevil bench run \
  --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --corpus examples/perf/north_nl_routes.csv \
  --concurrency 16 \
  --json /tmp/netweevil-route-c16.json

# Against a running server, same corpus.
netweevil bench http \
  --url http://127.0.0.1:8080 \
  --corpus examples/perf/north_nl_routes.csv \
  --backend netweevil \
  --concurrency 16 \
  --json /tmp/netweevil-http-c16.json

# Against OSRM, then diff latency and route agreement.
netweevil bench http \
  --url http://127.0.0.1:5000 \
  --corpus examples/perf/north_nl_routes.csv \
  --backend osrm \
  --osrm-profile driving \
  --concurrency 16 \
  --json /tmp/osrm-http-c16.json

netweevil bench compare \
  --baseline /tmp/netweevil-http-c16.json \
  --candidate /tmp/osrm-http-c16.json
```

Route workloads request summaries only, so timings measure the search rather
than geometry serialization. Use `--workload route-geometry` to measure the
geometry path on purpose.

Store JSON reports under `.netweevil/reports/` or outside the checkout.
See the [North Netherlands measurements](../../docs/performance-north-nl.md).
