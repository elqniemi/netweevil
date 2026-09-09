# Routing parity assessment

NetWeevil does not yet have full OSRM performance, Valhalla feature coverage,
or MOTIS transit coverage. This assessment separates the implemented changes
from the work required to establish parity. It was checked on September 9, 2026.

## What the targets mean

OSRM offers route, nearest, table, match, trip and tile services. It supports
CH and MLD preprocessing. Its project recommends MLD for general use and CH
for some large matrix workloads. NetWeevil already has edge-based CCH, so a
wholesale replacement of its routing algorithm is not the first step.
Measure the query, snapping, reconstruction, matrix, and HTTP costs separately.
See the [OSRM project](https://github.com/Project-OSRM/osrm-backend) and
[service documentation](https://project-osrm.org/docs/v5.24.0/api/).

Valhalla covers dynamic mode costing, directions, optimized waypoint order,
matrices, isochrones, map matching, elevation, and network location inspection.
NetWeevil's existing dynamic profiles, temporal routing, alternatives, matrices,
and service areas cover part of that workload. They do not provide navigation
guidance or GPS trace matching.
See the [Valhalla service index](https://valhalla.github.io/valhalla/api/).

MOTIS combines street and public transport routing with multiple timetable
feeds, real-time updates and shared mobility. Its timetable uses traffic-day
bitsets instead of duplicating the whole schedule for every service day.
Supported inputs include GTFS, NeTEx, GTFS-RT, SIRI and GBFS. Its scope also
includes fares, flexible services, geocoding and map tiles.
See the [MOTIS project](https://github.com/motis-project/motis).

## Current implementation and remaining work

| Area | NetWeevil implementation | Work needed for parity |
| --- | --- | --- |
| Road search | Edge-based CCH with metric customization, exact reference search, restriction validation | Matched local OSRM measurements; reduce hierarchy memory and costly restriction fallback |
| Snapping | Directed edge-interior snapping with profile, elevation and attribute filters; prepared edge spatial index | Bearing and curb-side constraints, spherical date-line handling |
| Dynamic costing | Cached request-defined profiles and overrides | Compare cold customization cost, cache churn and concurrent preparation against Valhalla |
| Matrix | Shared CCH search spaces with legal-route validation and a reusable exact frontier per origin | Accelerate restriction-state searches; test large asymmetric matrices |
| Service areas | Network and polygon output, multiple thresholds/origins, temporal sequences | Match Valhalla contour semantics and measure polygon generation independently |
| Locate | `POST /v1/locate` returns the same profile-aware candidates as route snapping | Add richer edge metadata where consumers need it |
| Navigation | Route geometry, segments and costs | Maneuver generation, intersections, roundabout exits, lane guidance, localized text and voice instructions |
| Waypoints | Route batches and OD matrices | Ordered waypoint routing with break/through semantics and optimized waypoint order |
| Map matching | No trace-matching request or engine | Candidate emissions, transition-distance likelihoods, Viterbi search, timestamps, gaps and confidence |
| Elevation | Z-aware topology and multilayer pedestrian routing | DEM sampling and an elevation-along-path service |
| Transit search | Depart-at, arrive-by, transfer limits, alternatives, frequency trips, access/egress and network transfer tables | Brute-force timetable oracle and a shared MOTIS journey corpus |
| Transit storage | Imported connections expanded across a selected date window | Pattern/run storage, service-day bitsets, agency timezone-aware lookup across midnight and DST |
| Transit feeds | Separate imported GTFS feeds | Feed-qualified identifiers, cross-feed transfers, NeTEx and flexible services |
| Live transit | Static timetable | Immutable real-time snapshots, delays, cancellations, stop changes and alerts; GTFS-RT first |
| Passenger constraints | Transfer/access/egress options | GTFS transfers/pathways, wheelchair and bicycle carriage rules, fares, platform-level passenger information |
| Shared mobility | No GBFS routing integration | Availability, vehicle/station constraints, geofencing and rental legs |

## Changes in this pass

The CCH search now accounts for negative destination seed costs from
partial-edge snapping when deciding whether to stop. Exact bidirectional search
stops after discovering a sufficient upper bound. Regression tests compare
seeded hierarchy queries with complete unpruned search spaces.

Matrices now use shared hierarchy searches even when the dataset contains
multi-edge restrictions. Every reconstructed candidate is checked. An invalid
candidate uses a restriction-aware Dijkstra search that stops once its lower
bound proves the result. Later destinations reuse that origin's frontier.
Only the current origin's exact frontier is retained to bound memory.

Snapping caches use exact coordinate bits and preserve the requesting point's
ID. A prepared edge index includes long-edge interiors whose endpoints lie
outside the requested search radius. The same implementation serves routes,
matrices and the new locate endpoint.

Transit transfer search now uses every stop within the requested radius,
without a nearest-32 truncation or an expanding search out to 100 kilometres.
The prepared router caches transfer adjacency. Boarding searches use monotonic
departure-time bounds, then apply trip-specific transfer slack. Same-stop
transfers honor that slack in both search directions and service areas.

Every dated and frequency departure has a distinct run ID, preventing a
transfer between two vehicles from being treated as staying aboard one trip.
GTFS pickup/drop-off restrictions apply in both directions. Arranged services
requiring contact with an agency or driver are excluded until booking is
modeled. Transit legs expose `run_index`; existing transit bundles require
re-import. Forward indexes store a four-byte connection reference and a
four-byte continuation link per connection, replacing 24-byte copied records.
Connection-position links preserve repeated stops and their geometry.

The benchmark client now supports a local Valhalla server. It requests Valhalla's
OSRM output format with no geometry or instructions, so reported distance is
in metres and duration in seconds. OSRM receives the same full-precision
coordinates and 500-metre snap radius. Valhalla receives a 500-metre search
cutoff. These parameters still do not make the engines' snapping policies
identical. The response format and omission controls are documented in
[Valhalla's route reference](https://valhalla.github.io/valhalla/api/route/api-reference/).

Benchmark reports fingerprint the actual selected input coordinates, IDs and
distance buckets. Comparison rejects different fingerprints or workload
settings. This prevents an unchanged filename or reused route ID from hiding
a different workload. It does not establish that servers use matching data or
profiles; those must be recorded alongside the reports.

Cleanup removes unused helpers, input aliases, bare-array document readers,
QGIS settings migrations, and previous plugin changelog entries. CSV inputs and
outputs use longitude/latitude column names. Current endpoints remain under
`/v1`; there was no obsolete endpoint family to delete. Persisted data and
benchmark readers accept their current schemas only.

## Implementation order

The current transit solver uses a heap over connection states. Frequency
services with `exact_times=0` are expanded as explicit departures, so headway
uncertainty is not modeled. Minimum-leg constraints remain postfilters rather
than a full multicriteria label set. These need separate correctness work.

1. Preserve correctness while measuring road performance. Use the existing
   1,000-pair North Netherlands corpus and exact-route reconstruction checks.
   Add turn-restriction, zero-cost, disconnected, long-edge and partial-edge
   cases before changing query pruning. Keep CCH for static or cached metrics;
   keep the temporal engine for costs that depend on time during traversal.
2. Further reduce matrix restriction fallback. Profile the number and cost of rejected
   CCH candidates, then represent multi-edge restriction automaton states in
   the accelerated graph. Validate every new path against exact routing before
   judging speed. Removing restrictions or legal u-turns changes the problem.
3. Add ordered waypoints and optimized stops over the existing matrix engine.
   Specify which waypoint types permit reversals and whether restrictions may
   span a waypoint. Guidance should consume a reconstructed route with road
   names and intersection metadata, rather than influence shortest-path code.
4. Build map matching as a query module. Reuse the edge spatial index for
   candidates and the routing engine for transitions. Test parallel roads,
   tunnels, sparse GPS points, gaps, stationary samples and disconnected traces.
5. Replace expanded transit storage before importing year-long, multi-feed
   schedules. Keep public trip IDs separate from dated/frequency run IDs.
   Group trips by stopping pattern, store operating days as bitsets, and add
   timezone-aware service-day lookup. Compare a round-based pattern scanner
   with the current connection search using the same journey oracle.
6. Apply GTFS-RT to immutable timetable snapshots. Each request must see one
   complete update generation. Validate missed and newly possible transfers,
   run cancellations, changed platforms and delayed trips crossing midnight.
   Add cross-feed transfers, accessibility, fares and GBFS after that foundation.

## Acceptance and reproducibility

Run comparisons on locally hosted engines with the same OSM extract, hardware,
thread count, service date, profile rules and output detail. Record executable
revision, compiler, dataset hashes, preprocessing time, bundle size and server
RSS. Run engines sequentially to avoid resource contention. Separate cold
startup, warm queries, customization and geometry serialization.

Report p50, p95, p99, throughput and failures for short, medium and long routes;
20/100/1,000-location matrices; and several service-area thresholds. Agree a
tolerance before calling a result parity. A useful initial road target is
p50 and p95 within 10% of local OSRM at each agreed workload and concurrency,
with no unexplained reachability differences. This is a proposed acceptance
threshold, not a measured result.

For transit, compare feasible journeys and Pareto choices, not only request
latency. Include depart-at and arrive-by, frequency service, pickup/drop-off,
minimum transfers, consecutive runs of one trip, overnight service, DST,
wheelchair access, and delayed/cancelled trips. Record feed hashes and update
generations. Run a small exhaustive timetable oracle before large MOTIS tests.

The existing remote public OSRM results measure network latency and a paced
request rate. They cannot establish equal-hardware engine capacity. This
environment has no installed OSRM, Valhalla, MOTIS or Docker executable, so
cross-engine performance parity remains unverified.

## Local measurements from this pass

Both runs use `north_nl_2026_05_10`, the car profile and the 1,000-pair North
Netherlands corpus. The existing release executable reports revision `450f2d9`;
its routing code matches the starting revision `fb20851`. The candidate
contains this working tree's query changes, so its build-time HEAD label alone
does not identify the source. These timings preceded solver-name/comment
cleanup and the final transit-only repeated-stop fix.

| Workload | Before p50 | After p50 | Before p95 | After p95 | Requests |
| --- | ---: | ---: | ---: | ---: | ---: |
| Point route | 0.751 ms | 0.749 ms | 1.935 ms | 1.658 ms | 3,000 each |
| 20×20 matrix | 32.631 s | 1.545 s | 39.662 s | 1.702 s | 3 each |

Route throughput increased from 1,070 to 1,178 requests/s. Matrix median latency
fell by a factor of 21.1. Every route and every matrix cell succeeded. Route
peak process RSS increased from 2.57 to 2.63 GiB with the new spatial index;
matrix peak RSS fell from 3.20 to 3.06 GiB. All 3,000 accelerated route samples
agree with the 1,000-route exact reference in distance, time and generalized
cost within `1e-6`.

Live HTTP validation also passed all 400 matrix cells against independently
executed exact routes and all 1,000 full route reconstructions. Reconstruction
checks cover segment connectivity, edge-path correspondence, geometry
endpoints, restrictions, and original distance/time/cost totals. Health,
readiness, locate and transit endpoints returned successful responses. A
synthetic GTFS import exercised a through journey across an intermediate stop
where boarding and alighting are prohibited.

Workspace validation passed 291 tests, `cargo check`, formatting, and Clippy
with warnings denied. The QGIS plugin packaged successfully and its Python
modules parsed without a QGIS runtime. The sparse transit benchmark ran
separately; the unrelated hierarchy-ordering experiment remains ignored.

These are sequential local workloads, with 20 route warmups and one matrix
warmup. Three matrix samples are too few for a stable tail-latency estimate.
Some development compilation occurred during this session, so small timing
differences need controlled repetition. The large matrix reduction corresponds
to eliminating exhaustive repeated searches, but it remains short of an OSRM
comparison. Raw reports live under `.netweevil/reports/parity/` and are excluded
from Git. The selected corpus fingerprint is
`1d00d962d5dafcdd5ef2cf8d9e599fa3e234a5c6f77630f7b5910e978270b4c8`.

Reproduce route and matrix measurements with the current release executable:

```bash
netweevil bench run --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --corpus examples/perf/north_nl_routes.csv --warmup 20 --iterations 3 \
  --json .netweevil/reports/parity/route.json

netweevil bench run --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --corpus examples/perf/north_nl_routes.csv --workload matrix \
  --matrix-size 20 --warmup 1 --iterations 3 \
  --json .netweevil/reports/parity/matrix20.json

python3 scripts/verify_matrix.py --url http://127.0.0.1:8080 \
  --corpus examples/perf/north_nl_routes.csv --size 20 \
  --dataset north_nl_2026_05_10 --profile examples/profiles/car_north_nl_2026_05.yml \
  --json .netweevil/reports/parity/matrix-validation.json
```

A separate synthetic transit probe uses 1,003 stops and 100 repeated route
requests. Three alternating release runs compare the starting revision with
the same test on the candidate. Median construction plus 100 requests fell
from 288.0 ms to 7.55 ms. Construction alone fell from 287.4 ms to 0.291 ms,
while the 100-request phase increased from 0.672 ms to 7.26 ms because it now
includes lazy transfer-index construction. This demonstrates removal of the
sparse-feed expanding-grid initialization cost; it does not establish a
warm-query improvement or national-feed transit performance. Compilation
overlapped the experiment and the samples varied substantially.

```bash
cargo test --release -p netweevil-transit \
  benchmark_sparse_feed_preparation_and_repeated_routes -- --ignored --nocapture
```
