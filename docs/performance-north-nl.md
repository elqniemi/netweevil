# North Netherlands performance measurements

The final hierarchy has 22.14 million shortcuts and 453.10 MiB profile bundles. Single-worker routing reaches 1,156.85 requests/s on the measured corpus with exact-route agreement. Matrix workloads remain much slower because of restriction-aware exact fallback; this does not establish general performance parity with OSRM or MOTIS.

Measurements use `examples/perf/north_nl_routes.csv`, 1,000 origin-destination pairs with seed 42. The local dataset is `north_nl_2026_05_10`; the profile is `examples/profiles/car_north_nl_2026_05.yml`. Raw reports are under `.netweevil/reports/performance-2026-09-07/` and are excluded from Git.

The machine reports an Intel Core i7-13800H, 20 logical CPUs and a Microsoft WSL2 Linux 6.18.33.2 kernel. RSS is the process high-water mark from `/proc`, including engine loading and warmup. It is not incremental memory per request. HTTP client RSS and API server RSS are recorded separately.

## Dataset and profile comparison

The local routing extract is 26,234,487 bytes with SHA-256 `b11902298fbe520bd691bc800955bb7ed65f644bfbba17e5138aa11e55322178`. It contains 2,107,900 topology nodes and 4,395,161 directed edges. The extract covers northern Netherlands and is named for May 10, 2026.

The remote service is `http://router.project-osrm.org`, using the `driving` route endpoint. Its published [Europe/Asia data timestamp](https://map.project-osrm.org/timestamps/careuasi.data_timestamp), checked September 7, was `2026-09-04T07:00:00Z`. The operator's [server description](https://routing.openstreetmap.de/about.html) lists OSRM 5.27.1, worldwide car coverage, and graphs refreshed roughly every two days. The serving binary and deployed profile revision were not independently verified.

The [service policy](https://routing.openstreetmap.de/about.html) allows at most one request per second and forbids heavy use. The corpus comparison uses one worker, no warmup, one pass, and at least 1.1 seconds between request starts. This measures observed HTTP latency and route agreement. Its reported throughput includes the deliberate pacing and cannot establish OSRM's capacity. Local HTTP latency and remote HTTP latency also include different network paths and server hardware.

The profiles have different objectives and speeds. NetWeevil uses its configured fastest objective with highway, toll, track, service-road and ferry preferences. Its class speed rules include motorway 100, primary 70, residential 30 and service 20 km/h. The operator's published [car profile](https://github.com/fossgis-routing-server/cbf-routing-profiles/blob/master/car.lua) uses a routability objective and nominal values of 90, 65, 25 and 15 km/h for those classes. Posted speed limits and other rules can change the effective speeds in both engines. NetWeevil configures a 25-second u-turn penalty and six seconds for traffic signals; the published OSRM profile uses 20 and two seconds. Its turn penalty also depends on turn angle. These costs, access rules, snapping choices, extract boundaries and the different OSM dates can change both the chosen route and its reported time.

Routes and source data are from [OSRM](https://project-osrm.org/) and [OpenStreetMap contributors](https://www.openstreetmap.org/copyright), under ODbL. Report map-data problems using [OpenStreetMap's map editor](https://www.openstreetmap.org/fixthemap).

The baseline release binary used Rust 1.96.1; the final release binary used Rust 1.98.1. Compiler differences are a confounder in the before/after performance comparison. Bundle sizes and shortcut counts do not depend on the benchmark timer.

## Baseline measurements

The rebuilt baseline has 38,985,986 shortcuts and 49,116,493 total directed acceleration arcs. Its topology bundle is 543,478,318 bytes, dataset acceleration bundle 266,788,750 bytes and car profile bundle 1,463,165,481 bytes. Together these three files occupy 2,273,432,549 bytes.

The accelerated route corpus completed all 1,000 requests in 3.241 seconds with one worker and ten warmup requests: 308.58 requests/s, p50 1.945 ms, p95 10.543 ms and peak RSS 4,172,627,968 bytes. Engine loading took 5.299 seconds. Import took 81.83 seconds with peak RSS 1,936,260 KiB; profile compilation took 7.59 seconds with peak RSS 2,858,832 KiB. The exact baseline ran alongside ordering experiments and is a correctness reference; its latency is not an uncontended performance measurement.

## Ordering and storage

Ordering operates directly on the edge-based graph. Nested dissection with minimum-degree ordering inside 32-state leaves produces 22,136,654 shortcuts and 32,267,161 total directed arcs. This meets the 25-million shortcut target and reduces shortcut count by 43.2% against the rebuilt baseline. An independent run spent 341.034 seconds on ordering and 348.868 seconds on ordering plus contraction. These ordering experiments overlapped other work, so their times are not isolated performance measurements.

The leaf-size sweep reused the same nested-dissection partitions above 256 states. Building those partitions took 297.916 seconds and produced 52,402 cells. The complete sweep process peaked at 1,944,820 KiB RSS. Each variant retains all 10,130,507 base arcs.

| Maximum leaf size | Shortcuts | Total directed arcs |
| --- | ---: | ---: |
| 16 | 22,293,133 | 32,423,640 |
| 32 | 22,136,654 | 32,267,161 |
| 64 | 22,235,427 | 32,365,934 |
| 128 | 22,603,802 | 32,734,309 |
| 256 | 23,162,809 | 33,293,316 |

The 32-state leaf setting is the smallest result among these measured variants. The independent run and shared-partition sweep produce the same count.

The input contains 4,352,146 u-turn transitions among 10,130,507 legal directed transitions, or 42.96%. Production ordering retains those transitions. Removing u-turns would change the paths available to the routing engine, including access to branches and permitted reversals. Shortcut comparisons must therefore retain them or explicitly identify a diagnostic graph that cannot be used for routing.

A diagnostic contraction removed the u-turn transitions in memory while keeping the production ranks fixed. It produced 5,778,361 base arcs and 15,838,840 shortcuts, 6,297,814 fewer shortcuts, or 28.45%. This isolates a substantial contribution to fill from u-turn connectivity, but is not a valid replacement for the production graph. Contraction took 2.249 seconds; the diagnostic process peaked at 1,425,964 KiB RSS.

Profile acceleration stores three `u32` metrics per directed arc, 12 bytes total, with no middle-state arrays. Each unit is 1/1024 of a cost, time or distance unit. Reconstruction finds lower triangles whose integer weights sum to the parent weight. Finite overflow is distinct from unreachable infinity and causes exact routing. The [CCH design](cch-design.md) states the rounding and route-choice error bounds, including the effect on service-area thresholds. These bounds still apply even though this corpus has no route disagreements.

## Final bundle sizes and routing checks

The routing cache uses `NWSECB05`. Car, cycling and pedestrian profiles are
recompiled against the final hierarchy.

| Bundle | Bytes | MiB |
| --- | ---: | ---: |
| Topology | 543,478,318 | 518.30 |
| Shared acceleration | 199,391,422 | 190.15 |
| Car profile | 475,109,593 | 453.10 |
| Cycling profile | 475,109,597 | 453.10 |
| Pedestrian profile | 475,109,600 | 453.10 |
| Edge names | 514,122 | 0.49 |

Final import took 289.96 seconds and peaked at 1,800,296 KiB RSS. Final car compilation took 1.68 seconds and peaked at 1,719,328 KiB RSS, as reported by `/usr/bin/time`. These preprocessing runs overlapped other work. One intermediate run showed inconsistent external elapsed time versus internal monotonic timing; query tables use the benchmark's monotonic timer throughout.

The car profile is 67.53% smaller than the rebuilt baseline's 1,463,165,481 bytes. All three profile bundles now fit within a few hundred MiB each. Topology, shared acceleration, edge names and one car profile still total 1,218,493,455 bytes, about 1.13 GiB. The whole network is therefore larger than a few hundred MiB.

The final accelerated and exact engines succeeded on all 1,000 corpus routes. Their recomputed generalized cost, travel time and distance agree within an absolute tolerance of `1e-6`; `routing-agreement.json` contains no disagreements. This check compares the original cost model after path reconstruction, not just the quantized search weights. The full HTTP reconstruction check also passed all 1,000 routes: segment adjacency, edge-path mapping, geometry endpoints, no violations or fallback, and recomputed distance, time and cost within `1e-6`. Its report is `reconstruction-validation.json`.

## Local workload measurements

Reports use production routing code from revision `e22b454`, with matrix cell-count reporting in the workload binary. Each process loads the same rebuilt car profile. Filesystem caches may already be warm. The local measurements ran sequentially after imports, compilation and correctness work finished.

| Workload | Workers | Measured requests | Requests/s | p50 ms | p95 ms | Peak process RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| In-process route | 1 | 1,000 | 1,156.85 | 0.723 | 1.577 | 2,632.89 |
| In-process route | 4 | 1,000 | 3,448.18 | 0.780 | 1.728 | 2,935.44 |
| Service area, 300 s | 1 | 100 | 59.03 | 16.556 | 20.213 | 2,572.09 |
| Service area, 900 s | 1 | 100 | 29.11 | 32.244 | 54.871 | 2,596.30 |
| Service area, 1,800 s | 1 | 100 | 5.46 | 164.700 | 310.118 | 2,722.67 |
| Local HTTP route | 1 | 1,000 | 681.95 | 1.325 | 2.838 | 9.62 client |
| Local HTTP route | 4 | 1,000 | 2,751.35 | 1.179 | 2.349 | 12.77 client |

All listed requests succeeded. Routes and HTTP runs use ten warmup requests. Service areas use one warmup and the first 100 corpus origins, which interleave short, medium and long route buckets. Service-area output is the reachable network, without polygon generation. HTTP `/healthz` and `/readyz` passed before measurement; the API process peaked at 2,906.29 MiB after both HTTP runs. The HTTP rows report client memory separately.

Compared with the baseline's single-worker corpus, throughput increased from 308.58 to 1,156.85 requests/s and peak RSS fell from 3,979.33 to 2,632.89 MiB, a 33.84% reduction. The compiler difference and a single measured corpus pass limit attribution and confidence intervals.

| Matrix | Cells succeeded | Failed / ignored | Measured requests | Request latency s | Peak process RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| 10×10 | 100 | 0 / 0 | 1 | 7.766 | 2,970.91 |
| 50×50 | 2,500 | 0 / 0 | 1 | 44.533 | 2,972.42 |
| 100×100 | 10,000 | 0 / 0 | 1 | 153.863 | 2,969.48 |

Matrix requests use the first N corpus origins on both axes, one measured pass and no warmup. One sample is a completed measurement, not a latency distribution. Each report records succeeded, failed and ignored cells so request-level success cannot hide partial output.

## Matrix restriction fallback

The first 10 corpus origins expose expensive paths that the point-to-point corpus does not. Expanding their 10×10 matrix into 100 separate route requests took 7.083 seconds. Seven pairs from origin 9 account for 6.96 seconds, while the median request takes 0.89 ms. The engine falls back to exact search when a CCH candidate violates a multi-edge turn restriction.

A diagnostic for origin 9 to origin 0 confirms that the restriction affects the result. Enforcing restrictions produces cost 3,748.6325 with 1,065 edges; ignoring them produces cost 3,741.5965 with 1,064 edges. The latter is not a valid performance replacement. Matrix measurements retain the legal routing behavior.

## Remote OSRM results

The remote corpus completed all 1,000 routes with no HTTP or routing failures. Observed latency was p50 34.182 ms, p95 47.338 ms, p99 205.019 ms and maximum 5,048.029 ms. Elapsed measured time including pacing was 1,105.829 seconds. The reported 0.904 requests/s reflects the deliberate rate limit.

The matching local HTTP single-worker corpus has p50 1.325 ms and p95 2.838 ms. The lower observed local latency includes the advantage of a loopback connection and does not measure the engines on equal hardware or network paths. The full comparison is `netweevil-osrm-comparison.txt`.

Against NetWeevil's final HTTP route summaries, the median absolute relative difference is 11.04% for travel time and 2.66% for distance; the p95 differences are 52.15% and 34.07%. These distributions include 999 routes with nonzero NetWeevil values. The remaining route, `short_0065`, has zero time and distance in both engines.

The differences include snapping behavior. For `short_0290`, OSRM snaps both input coordinates to `[6.136558, 53.005718]` on 4e Wijk, at distances of 130.56 and 277.42 metres from the inputs. It returns a zero-length route. NetWeevil returns 1,579 metres for that request. `short_0243` also has zero distance in OSRM and 56 metres in NetWeevil. Neither engine's route-time estimate is a measurement of actual driving time; profile and data differences above prevent treating these discrepancies as an accuracy ranking.

## Repository and cache checks

`cargo fmt --all`, `cargo check` and `cargo test` pass on Rust 1.98.1, with
263 tests passed. The ignored ordering experiment was run separately for the
leaf sweep and u-turn diagnostic. The CLI help probes and Python validator
syntax check also pass. Cycling and pedestrian cache smoke routes succeed.

Cache cleanup removed 17 obsolete metric bundles and 17 stale manifests,
reclaiming 11,422,792,094 bytes while retaining the three rebuilt profiles.

## Reproducing the measurements

Use the release binary and the rebuilt local bundles. For matrix workloads, each measured request is an entire square matrix; report cells as well as requests. Service areas use a 900-second threshold. Run local workload measurements sequentially to avoid contention from another benchmark or dataset import.

```bash
netweevil bench run --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --corpus examples/perf/north_nl_routes.csv --engine exact \
  --json .netweevil/reports/performance-2026-09-07/final-exact.json

netweevil bench run --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --corpus examples/perf/north_nl_routes.csv --engine accelerated \
  --workload matrix --matrix-size 100 --warmup 0 --iterations 1 \
  --json .netweevil/reports/performance-2026-09-07/matrix-100.json

netweevil bench run --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --corpus examples/perf/north_nl_routes.csv --engine accelerated \
  --workload service-area --threshold-s 900 --limit 100 --warmup 1 \
  --json .netweevil/reports/performance-2026-09-07/service-area-900.json

netweevil api serve --dataset north_nl_2026_05_10 \
  --default-profile examples/profiles/car_north_nl_2026_05.yml \
  --bind 127.0.0.1:8080

netweevil bench http --url http://127.0.0.1:8080 \
  --corpus examples/perf/north_nl_routes.csv --backend netweevil \
  --concurrency 1 --warmup 10 \
  --json .netweevil/reports/performance-2026-09-07/netweevil-http-c1.json

netweevil bench http --url http://router.project-osrm.org \
  --corpus examples/perf/north_nl_routes.csv --backend osrm \
  --concurrency 1 --request-interval-ms 1100 --timeout-s 30 \
  --json .netweevil/reports/performance-2026-09-07/osrm-http-c1.json

python3 scripts/verify_route_corpus.py --url http://127.0.0.1:8080 \
  --corpus examples/perf/north_nl_routes.csv \
  --exact .netweevil/reports/performance-2026-09-07/final-exact.json \
  --json .netweevil/reports/performance-2026-09-07/reconstruction-validation.json

netweevil bench compare \
  --baseline .netweevil/reports/performance-2026-09-07/netweevil-http-c1.json \
  --candidate .netweevil/reports/performance-2026-09-07/osrm-http-c1.json
```
