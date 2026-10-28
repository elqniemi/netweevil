# Benchmarking

`netweevil bench` measures routing throughput and latency in the process or
over HTTP. It also compares two runs. It reads compiled bundles and does not
change them.

## Prerequisites

- An imported dataset and a compiled profile.
- For HTTP benchmarks: a running server. Use a release build for
  measurements.

## Make a route corpus

A corpus is a CSV file of origin-destination pairs. `bench corpus` makes a
deterministic corpus:

1. It samples coordinates inside the topology bounds, or inside `--bbox`.
2. It snaps both ends with the engine's own snapping.
3. It keeps a pair only when both ends snap and share a connected component.
4. It sorts pairs into straight-line distance buckets: `short` (< 5 km),
   `medium` (5–30 km) and `long` (> 30 km).

The buckets keep one route length from dominating the aggregate. A seeded
SplitMix64 generator drives the sampling. The same `--seed`, dataset and
profile give the same file.

```bash
netweevil bench corpus \
  --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --count 1000 --seed 42 \
  --out examples/perf/north_nl_routes.csv
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--count` | 1000 | Number of routes |
| `--seed` | 42 | Random seed |
| `--bbox` | topology bounds | `min_lon,min_lat,max_lon,max_lat` |

### Corpus format

```text
id,source_lon,source_lat,target_lon,target_lat,bucket
```

`bucket` is optional. When it is absent, the reader derives it from the
straight-line distance. Ids must be unique. Coordinates must be finite WGS84
longitude and latitude.

!!! note
    A corpus belongs to the dataset that it was sampled from. Coordinates
    outside the coverage of a dataset do not snap. They count as failures.

### Checked-in corpora

| File | Area | Notes |
| --- | --- | --- |
| `examples/perf/north_nl_routes.csv` | Northern Netherlands | 1000 pairs, seed 42, short, medium and long |
| `examples/perf/ile_de_france_routes.csv` | Île-de-France | Medium and long car routes |
| `examples/perf/groningen_routes.csv` | Groningen | Short urban routes |
| `examples/perf/paris_routes.csv` | Paris | Short urban routes |

## Measure in the process

`bench run` measures a workload against one shared engine. `--concurrency`
uses scoped threads over that engine. Thus the numbers show the engine under
load, not N independent processes.

```bash
netweevil bench run \
  --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --corpus examples/perf/north_nl_routes.csv \
  --engine accelerated \
  --workload route \
  --concurrency 16 --warmup 50 \
  --json /tmp/route-c16.json
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--engine` | `accelerated` | `accelerated` uses the CCH bundle. `exact` searches the exact graph |
| `--workload` | `route` | See the workload table |
| `--concurrency` | 1 | Requests at the same time |
| `--warmup` | 0 | Untimed requests before the measurement |
| `--iterations` | 1 | Passes over the corpus |
| `--limit` | none | Uses only the first N rows |
| `--matrix-size` | 100 | Side length of the `matrix` workload |
| `--threshold-s` | 900 | Time limit of the `service-area` workload |
| `--service-area-output` | `network` | `network` (summary without geometry) or `polygon` (isochrone polygons with geometry) |
| `--json` | none | Writes the report |

| Workload | Meaning |
| --- | --- |
| `route` | Point-to-point routes, summaries only |
| `route-geometry` | Point-to-point routes with full geometry |
| `matrix` | One N×N matrix over the first `--matrix-size` corpus origins |
| `service-area` | One service area per corpus row at `--threshold-s` |

Route workloads request summaries only. Thus the timings measure the search,
not geometry serialization. Use `route-geometry` to measure the geometry
path.

`--engine exact` ignores the CCH acceleration bundle. Use it to see what the
acceleration gives. Use `--limit` when the exact engine is slow.

Each run reports the request count, failures, wall time, throughput,
p50/p90/p95/p99/max latency, cold engine-load time and peak RSS (`VmHWM`).
It breaks these down per distance bucket.

## Measure a running server

```bash
netweevil bench http --url http://127.0.0.1:8080 \
  --corpus examples/perf/north_nl_routes.csv \
  --backend netweevil --concurrency 16 --json /tmp/http-c16.json

netweevil bench http --url http://127.0.0.1:5000 \
  --corpus examples/perf/north_nl_routes.csv \
  --backend osrm --osrm-profile driving --concurrency 16 --json /tmp/osrm-c16.json

netweevil bench http --url http://127.0.0.1:8002 \
  --corpus examples/perf/north_nl_routes.csv \
  --backend valhalla --valhalla-costing auto --concurrency 16 --json /tmp/valhalla-c16.json
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--url` | `http://127.0.0.1:8080` | Base URL of the server |
| `--backend` | `netweevil` | `netweevil` (`POST /v1/route`), `osrm` or `valhalla` |
| `--profile-id` | server default | `profile_id` for the NetWeevil API |
| `--osrm-profile` | `driving` | OSRM profile path segment |
| `--valhalla-costing` | `auto` | Valhalla costing model |
| `--concurrency`, `--warmup`, `--iterations`, `--limit` | as `bench run` | As for `bench run` |
| `--request-interval-ms` | 0 | Minimum interval between request starts. Requires `--concurrency 1` |
| `--timeout-s` | 300 | Timeout per request |
| `--json` | none | Writes the report |

- The `osrm` backend calls
  `GET /route/v1/{profile}/{lon},{lat};{lon},{lat}?overview=false&radiuses=500;500`.
  It reads `routes[0].duration` and `routes[0].distance`.
- The `valhalla` backend posts to `/route`. It requests OSRM output with no
  shape or directions, and reads the same fields. Its location search cutoff
  is 500 metres.
- HTTP status failures count separately from routing failures. Thus a broken
  server does not look like a fast one.
- The client has no TLS. Use plain `http://` endpoints.
- HTTP requests identify the client as NetWeevil.

!!! warning
    For a remote endpoint, obey its usage policy. For example,
    `--request-interval-ms 1100` with `--concurrency 1` spaces request starts
    by at least 1.1 seconds. The reported latency excludes this delay.
    Throughput includes it and thus measures the imposed rate.

## Compare two reports

Compare two reports from the same corpus. The output shows latency per bucket
next to route-quality agreement. Thus you can see a speed gain that changes
the routes.

```bash
netweevil bench compare --baseline /tmp/http-c16.json --candidate /tmp/osrm-c16.json
```

The comparison rejects reports whose selected rows or workload settings
differ, including the matrix size and the service-area threshold.

## Report contents

JSON reports carry:

- the selected corpus coordinates, ids and distance buckets;
- the git commit, dataset, profile, engine and all settings;
- one record per request: `id`, `ok`, `latency_ms`, `duration_s`,
  `distance_m` and the NetWeevil `generalized_cost`.

Matrix records also include succeeded, failed and ignored cell counts. A
successful matrix request can contain unreachable cells.

Reports require the current schema. Make them again after a schema change.
Store reports under `.netweevil/reports/` or outside the checkout.

## Benchmark trip optimization

`bench trip` measures trip optimization on real stop matrices. For each size,
the first N corpus origins that can reach each other form a roundtrip. It
reports the matrix time and the default solve. It also reports the gap to
three references:

- Held–Karp, for small sizes;
- the best of several longer seeded runs;
- the assignment lower bound.

```bash
netweevil bench trip --dataset groningen \
  --profile examples/profiles/car_research_v1.yml \
  --corpus /tmp/groningen-2000.csv --sizes 10,50,200,1000 --json /tmp/trip.json
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--sizes` | `10,50,200,1000` | Stop counts |
| `--reference-runs` | 8 | Seeded reference runs for large sizes |
| `--reference-factor` | 5 | Iteration multiplier of each reference run |
| `--bound-max-size` | 1000 | Skips the O(n³) assignment bound above this size |

See [Trip optimization](trip-optimization.md#quality).

## Benchmark traffic

`bench traffic` measures bucket customization, live swaps and route latency
with traffic.

```bash
netweevil bench traffic --dataset groningen \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --traffic groningen_synthetic --corpus examples/perf/groningen_routes.csv --verify-exact
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--departures` | four times on 4 and 5 October 2026 | Departures whose buckets are customized and routed |
| `--live-batches` | `1,100,1000` | Live batch sizes, alternating closures and slowdowns |
| `--verify-exact` | off | Compares bucket routes with the exact engine on the same metric |
| `--time-dependent-routes` | 0 | Corpus routes to time with the exact time-dependent engine |
| `--passes` | 5 | Passes over the corpus per measurement |
| `--json` | none | Writes the report |

## Compare with a local OSRM

`scripts/fetch_osrm_benchmark.py` downloads the pinned official OSRM Linux
amd64 image. It verifies the manifest and layer hashes. It extracts only the
binaries and Lua profiles. It needs Python 3.11 or later and no container
daemon. The default output directory is `/tmp/netweevil-osrm-image`. Use
`--out` to choose another.

1. Fetch OSRM and prepare the extract:

    ```bash
    python3 scripts/fetch_osrm_benchmark.py --out .netweevil/benchmarks/osrm-image
    OSRM=.netweevil/benchmarks/osrm-image
    DATA=.netweevil/benchmarks/osrm-data
    mkdir -p "$DATA"

    "$OSRM/usr/local/bin/osrm-extract" -t 2 -p "$OSRM/opt/car.lua" \
      -o "$DATA/north_nl.osrm" datasets/north_nl_2026_05_10_routing.osm.pbf
    "$OSRM/usr/local/bin/osrm-partition" -t 2 "$DATA/north_nl.osrm"
    "$OSRM/usr/local/bin/osrm-customize" -t 2 "$DATA/north_nl.osrm"
    ```

2. Start OSRM:

    ```bash
    "$OSRM/usr/local/bin/osrm-routed" --algorithm mld \
      --ip 127.0.0.1 --port 15001 --threads 1 "$DATA/north_nl.osrm"
    ```

3. Start NetWeevil on the same extract in another terminal.
4. Run both HTTP benchmarks one after the other on an idle machine:

    ```bash
    netweevil bench http --backend osrm --url http://127.0.0.1:15001 \
      --corpus examples/perf/north_nl_routes.csv --concurrency 1 --warmup 20 --iterations 3 \
      --json .netweevil/reports/bench/osrm-mld-http-c1.json
    netweevil bench http --backend netweevil --url http://127.0.0.1:8080 \
      --profile-id car_north_nl_2026_05 \
      --corpus examples/perf/north_nl_routes.csv --concurrency 1 --warmup 20 --iterations 3 \
      --json .netweevil/reports/bench/netweevil-http-c1.json
    netweevil bench compare \
      --baseline .netweevil/reports/bench/osrm-mld-http-c1.json \
      --candidate .netweevil/reports/bench/netweevil-http-c1.json
    ```

!!! note
    The stock OSRM `car.lua` and the NetWeevil profile use different speeds,
    turn penalties and access rules. The same PBF does not give the same
    graph or route costs. `bench compare` counts failed requests in latency
    comparisons and reports route agreement separately. Filter both reports
    to their common successful ids before you compare successful-route
    latency.

## Verify accelerated results

Two scripts check that accelerated HTTP results agree with exact CLI routing.

`scripts/verify_route_corpus.py` checks full route reconstruction from a
running server: segment chain, edge path, geometry, no fallback and no
violations. It compares against an exact `bench run` report over the same
corpus:

```bash
netweevil bench run --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --corpus examples/perf/north_nl_routes.csv --engine exact \
  --json .netweevil/reports/bench/exact.json

python3 scripts/verify_route_corpus.py --url http://127.0.0.1:8080 \
  --corpus examples/perf/north_nl_routes.csv \
  --exact .netweevil/reports/bench/exact.json \
  --json .netweevil/reports/bench/route-validation.json
```

`scripts/verify_matrix.py` requests a square matrix over the first `--size`
corpus points from `/v1/matrix`. It compares each cell with an exact
`bench run` of the local CLI (`--binary`, default
`target/release/netweevil`):

```bash
python3 scripts/verify_matrix.py --url http://127.0.0.1:8080 \
  --corpus examples/perf/north_nl_routes.csv --size 20 \
  --dataset north_nl_2026_05_10 --profile examples/profiles/car_north_nl_2026_05.yml \
  --json .netweevil/reports/bench/matrix-validation.json
```

For request-defined profiles, see
[Request-defined profiles](request-profiles.md#benchmark-request-profiles).

## Related pages

- [Routing](routing.md#response-speed).
- [Trip optimization](trip-optimization.md).
- [Traffic](traffic.md).
