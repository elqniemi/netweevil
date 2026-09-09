# Local OSRM comparison

On 2026-09-09, a localhost comparison over the same North Netherlands PBF found
NetWeevil about 2.1 times slower than OSRM at median HTTP latency on the 909
routes both engines could serve. This establishes a local baseline, not routing
or feature parity.

## Inputs and versions

- OSRM 26.9.0, MLD, official stock `car.lua`, one server worker.
- Official image revision `d63a1df9c25c84f2edf304de8bc7b691aa9020f9`.
- Linux amd64 image manifest
  `sha256:c29a50d67b9be17d10773fa2b52bb045ee3fbb8f42e5f9d1c671ce0d9bb21f37`.
- NetWeevil release binary reports commit `fb20851`, using
  `examples/profiles/car_north_nl_2026_05.yml` and its default API worker pool.
- Input `datasets/north_nl_2026_05_10_routing.osm.pbf`, SHA256
  `b11902298fbe520bd691bc800955bb7ed65f644bfbba17e5138aa11e55322178`.
- Corpus `examples/perf/north_nl_routes.csv`, all 1,000 rows, concurrency one,
  20 warmup requests, three measured repetitions, 500 m snapping radius.
- Both services ran on the same machine. Requests omitted route geometry and
  instructions. HTTP runs were sequential, followed by an in-process run.

OSRM's car profile optimizes its `routability` weight. NetWeevil's profile sets
different speeds, turn penalties, access rules and preferences. Identical PBF
input therefore does not imply identical traversable graphs or route costs.
The [OSRM README](https://github.com/Project-OSRM/osrm-backend) describes the
MLD preprocessing pipeline and the
[26.9.0 release](https://github.com/Project-OSRM/osrm-backend/releases/tag/v26.9.0)
identifies the measured engine version.

## Results

The table filters existing samples to the 909 IDs that succeeded in both
engines. Each row includes 2,727 measurements; percentiles use linear
interpolation, as in `netweevil bench`.

| Execution | Median latency | p95 latency |
| --- | ---: | ---: |
| OSRM MLD, localhost HTTP | 0.750 ms | 1.485 ms |
| NetWeevil, localhost HTTP | 1.561 ms | 3.306 ms |
| NetWeevil, in process | 0.951 ms | 2.532 ms |

Across all requests, OSRM returned successful routes for 909 unique pairs and
NetWeevil for all 1,000. Of OSRM's 91 failures, 84 were `NoSegment` within the
500 m radius and seven were `NoRoute`. These failures are excluded from the
table above. Among successful pairs, median duration difference was 12.47%
and median distance difference was 2.66%; p95 differences were 98.26% and
38.20%. These are different routing models, so the numbers do not establish
cost agreement.

After the HTTP runs, server RSS was 163,352 KiB for OSRM and 2,659,496 KiB for
NetWeevil. HTTP benchmark reports measure the client process RSS, not these
server values. The in-process NetWeevil report recorded 2.63 GiB peak RSS.
No profiler attribution was collected in this run.

Raw reports, detailed failure bodies, executable hashes and the derived
successful-route summary are local artifacts under
`.netweevil/reports/local-osrm-parity/`. They are not committed datasets.

## Reproduce

Rootless Podman failed here because user-namespace creation was denied. The
standalone release archive required newer glibc than the host, but the official
Debian image's binaries ran directly. The helper below verifies manifest and
layer hashes, extracts only the official binaries and Lua profiles into `/tmp`,
and changes no system packages. It requires Python 3.11+ and Linux amd64.

```bash
python3 scripts/fetch_osrm_benchmark.py
/tmp/netweevil-osrm-image/usr/local/bin/osrm-routed --version
mkdir -p .netweevil/benchmarks/osrm-26.9.0

/tmp/netweevil-osrm-image/usr/local/bin/osrm-extract -t 2 \
  -p /tmp/netweevil-osrm-image/opt/car.lua \
  -o .netweevil/benchmarks/osrm-26.9.0/north_nl.osrm \
  datasets/north_nl_2026_05_10_routing.osm.pbf
/tmp/netweevil-osrm-image/usr/local/bin/osrm-partition -t 2 \
  .netweevil/benchmarks/osrm-26.9.0/north_nl.osrm
/tmp/netweevil-osrm-image/usr/local/bin/osrm-customize -t 2 \
  .netweevil/benchmarks/osrm-26.9.0/north_nl.osrm
```

Start each service in its own terminal:

```bash
/tmp/netweevil-osrm-image/usr/local/bin/osrm-routed \
  --algorithm mld --ip 127.0.0.1 --port 15001 --threads 1 --verbosity WARNING \
  .netweevil/benchmarks/osrm-26.9.0/north_nl.osrm
```

```bash
RUST_LOG=warn target/release/netweevil api serve \
  --dataset north_nl_2026_05_10 \
  --default-profile examples/profiles/car_north_nl_2026_05.yml \
  --bind 127.0.0.1:18081
```

Run the benchmarks sequentially while the machine is otherwise idle:

```bash
mkdir -p .netweevil/reports/local-osrm-parity
target/release/netweevil bench http --backend osrm \
  --url http://127.0.0.1:15001 --corpus examples/perf/north_nl_routes.csv \
  --limit 1000 --concurrency 1 --warmup 20 --iterations 3 \
  --json .netweevil/reports/local-osrm-parity/osrm-mld-http-c1.json
target/release/netweevil bench http --backend netweevil \
  --url http://127.0.0.1:18081 --profile-id car_north_nl_2026_05 \
  --corpus examples/perf/north_nl_routes.csv \
  --limit 1000 --concurrency 1 --warmup 20 --iterations 3 \
  --json .netweevil/reports/local-osrm-parity/netweevil-http-c1.json
target/release/netweevil bench run --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --corpus examples/perf/north_nl_routes.csv \
  --limit 1000 --concurrency 1 --warmup 20 --iterations 3 \
  --json .netweevil/reports/local-osrm-parity/netweevil-inprocess-c1.json
target/release/netweevil bench compare \
  --baseline .netweevil/reports/local-osrm-parity/osrm-mld-http-c1.json \
  --candidate .netweevil/reports/local-osrm-parity/netweevil-http-c1.json
```

`bench compare` includes failed requests in latency comparisons and reports
route agreement separately. Filter both reports to their common successful
IDs before drawing a conclusion about successful-route latency. HTTP failure
bodies distinguish snapping failures from disconnected routes.
