# Historical and live traffic

NetWeevil can route cars, HGVs and transit vehicles with time-of-week
historical speeds and live speed or closure updates. Requests that carry a
`departure_time` use them; requests without a time keep the profile's
free-flow speeds.

## Speed sets

A traffic source is a YAML or JSON document. Patterns are weekly speed
profiles; assignments bind a pattern to a directed source feature (an OSM way
id for OSM imports, the mapped feature id otherwise).

```yaml
traffic_id: groningen_synthetic
timezone: Europe/Amsterdam   # IANA zone of the time-of-week buckets
bucket_minutes: 15           # must divide 60, so clock changes fall on boundaries
patterns:
  - id: arterial_peak
    # One day of buckets (repeated Monday to Sunday) or a full week from
    # Monday 00:00. 0 means "no observation": the profile speed applies.
    speeds_kph: [50, 50, ...]
    weekend_speeds_kph: [50, 50, ...]   # optional, with a one-day profile
assignments:
  - {feature_id: 4463602, direction: both, pattern: arterial_peak}
assignments_csv: assignments.csv        # feature_id,direction,pattern_id
patterns_csv: patterns.csv              # pattern_id,speeds_kph (space separated)
```

`direction` is `forward`, `backward` or `both`, relative to the source
geometry. An assignment CSV may carry a `speeds_kph` column instead of
`pattern_id` for per-segment data. Identical speed vectors are stored once,
whichever way they arrive.

```bash
netweevil traffic import examples/traffic/groningen_synthetic.yml --dataset groningen
netweevil traffic list
```

Import reports how many directed features and edges of the dataset match.
The bundle is written to `.netweevil/bundles/traffic/traffic-<id>.bin`
in the sectioned format (patterns as delta varints, feature ids as gaps) with
a manifest under `.netweevil/traffic/`. The bundle is keyed by source feature
id, so it binds to a re-imported or updated dataset at load time.

`examples/traffic/groningen_synthetic.yml` was produced by
`netweevil traffic synthesize --dataset groningen --name groningen_synthetic
--out examples/traffic/groningen_synthetic.yml`: road-class free-flow speeds
with weekday morning and evening dips, a milder weekend and four speed
variants per class. It is synthetic test data, not observations.

## How speeds change costs

An observed speed `v` turns an edge's profile time `t` into
`t' = length / v` (cars) or `max(t, length / v)` (HGVs and transit vehicles,
which keep their own limits). Bicycle and foot profiles ignore traffic, as do
ferries. The generalized cost changes by `time_weight * (t' - t)`, the rule
the exact temporal engine already uses for `SpeedFactor` schedules, floored
at zero.

## Request semantics

The `traffic` request field (next to `departure_time`) selects the behaviour:

| `traffic` | Behaviour |
| --- | --- |
| `auto` (default) | Departure-bucket CCH with live updates valid at departure |
| `historical` | Departure-bucket CCH, live updates ignored |
| `time_dependent` | Exact time-dependent label-setting with traffic (routes only) |
| `off` | Ignore traffic; the request keeps its previous semantics |

**Departure-bucket routing.** The route is chosen on the CCH customized for
the time-of-week bucket that contains the departure, plus the live entries
valid at that instant. It is exact for that metric: equivalence tests compare
it with the exact engine running on the same per-edge metric. The arrival
time, per-segment entry and exit times and the cost are then integrated along
the chosen path with the speed in force at every instant, crossing bucket
boundaries and live expiries. The integration is FIFO: entering an edge later
never exits it earlier. Imported time rules (opening hours, closures) apply
during the integration; if the path turns out to be closed at a later entry
time, the request falls back to the exact time-dependent engine.

This is not full time-dependent optimality: a trip that crosses into a slower
bucket may have a better alternative the departure metric could not see. The
response reports `bucket_boundaries_crossed`; zero means the departure
metric held for the whole trip. With 15-minute buckets on Groningen, peak
ETAs differ from the departure-bucket metric's own travel time by about 8%
on average.

**Exact time-dependent routing.** `traffic: time_dependent`, `arrive_by`,
scenarios and overlays use the exact label-setting engine with traffic speeds
integrated along every edge, including live closures (which may be waited
out when the profile allows waiting; live expiries round up to whole minutes
for that reason). It is optimal for the full time-dependent cost but slow:
seconds to tens of seconds per route on a province.

Full time-dependent CCH variants (time-dependent CCH, CATCHUp) would close
that gap but store piecewise-linear functions per arc. Per-bucket scalar
customizations reuse the existing hierarchy and exactness tests unchanged.

**Batches.** OD batches, matrices and service areas with a `departure_time`
run on the departure-bucket engine. Their costs and times come from the
bucket metric and are not integrated over the trip. Time-dependent batches
with traffic are not supported; set `traffic: off` to use the exact temporal
engine without traffic.

Not combined with traffic: component constraints and Pareto routes (error),
failure-mode fallbacks (error), request-defined profiles and
`/v1/directions` (traffic ignored), engine modes other than `auto` (error).
Holidays from a holiday calendar affect imported rules, not the speed
buckets.

## Engines and caching

Each motorized profile keeps a least-recently-used cache of customized
engines (`NETWEEVIL_TRAFFIC_CACHE_ENTRIES`, default 4). Buckets whose speed
columns are identical across all patterns share one engine; the Groningen
example has 30 distinct metrics in its 672 buckets. Concurrent requests for an
uncached bucket wait for one customization. Engines that differ from the
profile only in speeds share the profile engine's adjacency, spatial index and
hierarchy arrays; a live closure changes the adjacency and builds a full
graph.

## Live updates

```bash
curl -X POST 'http://127.0.0.1:8080/v1/traffic/live?wait=true' \
  -H 'content-type: application/json' \
  -d '{"updates": [
        {"way_id": 4463602, "closed": true, "ttl_s": 1800},
        {"feature_id": 6348460, "direction": "forward", "speed_kph": 12, "expires_at": "2026-10-05T09:00:00+02:00"}
      ]}'
curl -X DELETE 'http://127.0.0.1:8080/v1/traffic/live?wait=true'
curl http://127.0.0.1:8080/v1/traffic
```

Each update sets exactly one of `closed: true` or `speed_kph`, or `clear: true`
to drop a feature's live state. Validity starts at receipt and ends at
`expires_at` or after `ttl_s` (default 900 s). `"replace": true` replaces all
live entries; otherwise updates merge, later ones winning per directed
feature. Invalid batches are rejected with HTTP 400 before they are queued.

A single background worker applies batches: it merges queued batches,
customizes every motorized profile's engine for the current bucket with the
new live state, and only then publishes the new snapshot. Requests running
during that time keep the previous snapshot and engines; none waits for the
customization. Without `?wait=true` the endpoint answers `202 Accepted`. The
same worker prepares engines about 60 s before each bucket boundary and live
expiry, so the first request after the boundary hits a ready engine. A
workspace reload (`/v1/workspace/reload`, dataset updates) carries live state
over to the new runtime.

Responses of traffic-aware analyses carry `service.traffic`: the engine used,
departure bucket, live version, live entries applied, cache hit or miss,
preparation time and bucket boundaries crossed.

## Serving and CLI

```bash
netweevil api serve --dataset groningen \
  --default-profile examples/profiles/car_north_nl_2026_05.yml \
  --traffic groningen_synthetic
```

The selection is saved in `.netweevil/workspace.json` (`traffic`), and the
console activation request accepts a `traffic` field. Without `--traffic`
live updates still work on top of the profile speeds.

`netweevil traffic route --dataset ... --profile ... --traffic ... --request
route.json [--departure-time ...] [--mode time-dependent] [--live updates.json]`
runs one traffic-aware route from the CLI.

## Measurements (Groningen, 2026-10-02)

Groningen province, 1,035,995 directed edges, 7.7 M hierarchy arcs, profile
`car_north_nl_2026_05`, `examples/perf/groningen_routes.csv` (25 routes),
release build on a shared Apple Silicon machine (timings are noisy):

```bash
netweevil bench traffic --dataset groningen \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --traffic groningen_synthetic --corpus examples/perf/groningen_routes.csv --verify-exact
```

| Measure | Result |
| --- | --- |
| Bundle | 43.5 KB for 16 patterns and 9,378 directed features (52,489 edges); import 0.24 s |
| Bucket engine preparation (customization plus re-costed graph) | 160–420 ms |
| Memory per cached bucket engine | about 220 MiB |
| Static route without traffic | p50 0.30 ms, p95 0.83 ms |
| Departure-bucket route with integrated ETA (warm) | p50 0.57–0.71 ms, p95 1.1–1.8 ms |
| Bucket CCH vs exact engine on the bucket metric | 0 mismatches over 25 routes × 4 buckets |
| Live swap, 1 to 1,000 updates (closures included) | 485–606 ms in process, 370 ms over HTTP |
| Concurrent reader during a swap | p50 0.67 ms, max 14–74 ms (CPU contention, no blocking) |
| Exact time-dependent route with traffic | 2–34 s depending on length |

Over HTTP the first request in a new bucket took 207 ms (customization), the
following ones 2–5 ms; a matrix and a service area on the cached bucket
engine took 2 ms and 122 ms.
