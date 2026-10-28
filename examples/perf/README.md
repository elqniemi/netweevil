# Benchmark corpora

This folder contains route corpora for `netweevil bench`. The
[benchmarking guide](../../docs/guides/benchmarking.md) describes all bench
commands.

## Corpora

| File | Area | Notes |
| --- | --- | --- |
| `north_nl_routes.csv` | Northern Netherlands | 1000 pairs, seed 42, stratified short/medium/long |
| `ile_de_france_routes.csv` | Île-de-France | medium and long-range car routes |
| `groningen_routes.csv` | Groningen | short urban routes |
| `paris_routes.csv` | Paris | short urban routes |

## Format

Generated corpora use:

```text
id,source_lon,source_lat,target_lon,target_lat,bucket
```

`bucket` is the straight-line distance class: `short` (< 5 km), `medium`
(5–30 km) or `long` (> 30 km). Reports give the latency for each bucket. Thus
one route length does not control the total value.

The `bucket` column is optional. When it is missing, NetWeevil calculates it
from the straight-line distance. IDs must be unique. Coordinates must be finite
WGS84 longitude and latitude values.

## Make a corpus

`bench corpus` samples coordinates in the topology bounds or in `--bbox`. It
snaps both endpoints with the snapping of the engine. It keeps a pair only when
both ends snap to the same connected component. A seeded SplitMix64 generator
controls the sampling. The same `--seed`, dataset and profile give the same
file.

```bash
netweevil bench corpus \
  --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --count 1000 \
  --seed 42 \
  --out examples/perf/north_nl_routes.csv
```

Use a corpus with the dataset that it comes from. Coordinates outside the
dataset do not snap and count as failures.

## Run a corpus

```bash
mkdir -p .netweevil/reports/bench

# In-process, 16 threads over one shared engine.
netweevil bench run \
  --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --corpus examples/perf/north_nl_routes.csv \
  --concurrency 16 \
  --json .netweevil/reports/bench/netweevil-route-c16.json

# Against a running server, same corpus.
netweevil bench http \
  --url http://127.0.0.1:8080 \
  --corpus examples/perf/north_nl_routes.csv \
  --backend netweevil \
  --concurrency 16 \
  --json .netweevil/reports/bench/netweevil-http-c16.json

# Against OSRM, then diff latency and route agreement.
netweevil bench http \
  --url http://127.0.0.1:5000 \
  --corpus examples/perf/north_nl_routes.csv \
  --backend osrm \
  --osrm-profile driving \
  --concurrency 16 \
  --json .netweevil/reports/bench/osrm-http-c16.json

netweevil bench compare \
  --baseline .netweevil/reports/bench/netweevil-http-c16.json \
  --candidate .netweevil/reports/bench/osrm-http-c16.json
```

Route workloads request summaries only. Thus the times measure the search and
not the geometry output. Use `--workload route-geometry` to measure the
geometry output.

Keep JSON reports in `.netweevil/reports/` or outside the repository.

## Compare with a local OSRM

`scripts/fetch_osrm_benchmark.py` downloads the pinned official OSRM Linux
amd64 image. It verifies the manifest and layer digests of the image. It
extracts only the binaries and the Lua profiles. It needs Python 3.11 or later
and no container daemon. The default output folder is
`/tmp/netweevil-osrm-image`. Use `--out` to select a different folder.

```bash
python3 scripts/fetch_osrm_benchmark.py --out .netweevil/benchmarks/osrm-image
OSRM=.netweevil/benchmarks/osrm-image
DATA=.netweevil/benchmarks/osrm-data
mkdir -p "$DATA"

"$OSRM/usr/local/bin/osrm-extract" -t 2 -p "$OSRM/opt/car.lua" \
  -o "$DATA/north_nl.osrm" datasets/north_nl_2026_05_10_routing.osm.pbf
"$OSRM/usr/local/bin/osrm-partition" -t 2 "$DATA/north_nl.osrm"
"$OSRM/usr/local/bin/osrm-customize" -t 2 "$DATA/north_nl.osrm"

"$OSRM/usr/local/bin/osrm-routed" --algorithm mld \
  --ip 127.0.0.1 --port 15001 --threads 1 "$DATA/north_nl.osrm"
```

Serve the same extract with NetWeevil in a different terminal. Then run the
two HTTP benchmarks one after the other on a machine that does no other work:

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

The OSRM `car.lua` profile and the NetWeevil profile use different speeds,
turn penalties and access rules. Thus the same PBF does not give the same graph
or the same route costs. `bench compare` includes failed requests in the
latency comparison and gives the route agreement separately. To compare the
latency of successful routes, filter the two reports to their common
successful IDs.

## Verify results

Two scripts make sure that accelerated HTTP results agree with exact CLI
routing.

`scripts/verify_route_corpus.py` checks the full routes of a running server:
segment chain, edge path, geometry, and no fallback or violations. It compares
them with an exact `bench run` report of the same corpus:

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

`scripts/verify_matrix.py` requests a square matrix of the first `--size`
corpus points from `/v1/matrix`. It compares each cell with an exact
`bench run` of the local CLI (`--binary`, default `target/release/netweevil`):

```bash
python3 scripts/verify_matrix.py --url http://127.0.0.1:8080 \
  --corpus examples/perf/north_nl_routes.csv --size 20 \
  --dataset north_nl_2026_05_10 --profile examples/profiles/car_north_nl_2026_05.yml \
  --json .netweevil/reports/bench/matrix-validation.json
```
