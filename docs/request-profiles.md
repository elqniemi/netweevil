# Request-defined street profiles

`POST /v1/route` accepts a profile in the request. NetWeevil compiles it in
memory, customizes the dataset's CCH weights, and caches the prepared engine.
Subsequent requests with the same effective profile reuse that engine. This is
network-wide compilation on first use; costs are not evaluated lazily during
search.

Requests without `profile` or `profile_overrides` use the existing prepared
profile lookup. They do not serialize or hash profiles, acquire cache locks,
or wait for a profile compilation slot. The query engine and its search loops
are unchanged.

## Override a loaded profile

```json
{
  "profile_id": "car_north_nl_2026_05",
  "profile_overrides": {
    "preferences": {"use_highways": 0.2, "use_tolls": 0.0},
    "turns": {"left_penalty_s": 12.0}
  },
  "request": {
    "route_id": "example",
    "origin": {"id": "o", "lon": 6.5665, "lat": 53.2194},
    "destination": {"id": "d", "lon": 6.5716, "lat": 53.2148},
    "snap": {"max_distance_m": 500},
    "returns": {"geometry": "full"}
  }
}
```

Omit `profile_id` to override the server's default profile. Objects merge
recursively, while arrays and scalar values replace their previous values.
For example, `speed_rules` replaces the entire ordered rule list. An empty
object preserves existing entries in a map; it does not clear them. Null is a
literal value and can clear optional fields such as `slope_model`; it is not a
map-key deletion operation. Unknown fields and invalid profiles return HTTP
400. An unknown base profile returns HTTP 404.

The profile schema and parameter meanings are the same as file profiles.
Preference values express weighting; `use_tolls: 0.0` is not a hard toll ban.
Use exclusion rules for hard exclusions. A request cannot restore roads,
directions, or attributes that were not retained during dataset import.

The file [route_profile_overrides.json](../examples/api/route_profile_overrides.json)
can be sent directly:

```bash
curl -i http://127.0.0.1:8080/v1/route \
  -H 'Content-Type: application/json' \
  --data-binary @examples/api/route_profile_overrides.json
```

## Supply a complete profile

Instead of `profile_id` and `profile_overrides`, supply `profile` containing a
complete profile document as JSON, including its `profile` header with `id`,
`label`, `mode`, and `defaults_pack`. Unspecified optional fields use the same
defaults as file profiles. `profile` cannot be combined with either of the
other profile-selection fields.

This release supports request-defined profiles on `/v1/route` only. Other
analyses, transit bindings, simulation and the QGIS profile selector continue
to use loaded profiles. Existing route options, including GeoJSON output,
retain their normal behavior after profile resolution.

## Cache and resource use

The cache retains two engines by default. Set
`NETWEEVIL_DYNAMIC_PROFILE_CACHE_ENTRIES` to an integer from 1 through 16 before
starting the server to change the limit. Eviction removes the least recently
used entry; routes already using it retain their engine until they finish.
The limit counts retained engines, not bytes or engines held by active routes.
Cold compilation temporarily needs additional memory. Large datasets can need
gigabytes per prepared profile, so start with a small entry limit.

The cache belongs to one immutable dataset runtime and is keyed by the
effective profile fingerprint. Dataset topology and acceleration cannot be
mixed across server instances. Equivalent overrides and complete documents
share entries after defaults are resolved. A profile matching an already
loaded profile reuses that prepared engine. Profile header and return-default
fields participate in the existing fingerprint, so changing them also changes
the key.

Cold compilations run one at a time on a dedicated single-thread Rayon pool.
They do not take a routing-worker permit or use the global Rayon pool.
Identical concurrent requests share one compilation and its resulting engine,
including when other profiles cause cache eviction. Cancelling an HTTP waiter
does not release the slot while its blocking
compilation is still running or discard the completed engine. Failed
compilations do not enter the cache.

CPU, memory bandwidth and allocator capacity are still shared with routing.
This isolation avoids adding compilation work to the prepared request path;
it cannot guarantee zero hardware contention under mixed load. For strict
latency isolation, run request-profile traffic in a separate API process with
appropriate CPU and memory limits.

Request profiles do not create files under `.netweevil/`, modify loaded
profiles or appear in `/v1/profiles`. The ordinary startup loading/compilation
of named profiles still uses its existing persistent cache. Save both the
base profile content and overrides, or the complete effective document, to
reproduce an experiment.

## Timing and verification

Dynamic responses include two HTTP headers, including for GeoJSON output:

- `x-netweevil-profile-cache`: `miss`, `hit`, or `preloaded`.
- `x-netweevil-profile-prepare-ms`: time spent resolving the profile, including
  validation, hashing, queueing, compilation and preparation as applicable.

The response's existing `service.profile_hash` identifies the effective
profile. `service.acceleration` describes the engine actually used. Ordinary
prepared responses retain their existing body and do not add these headers.

Use the benchmark script with a release binary and a local dataset. It starts
its own API, checks health/readiness, warms the route corpus, measures HTTP
latency, and samples process RSS on Linux. Run binaries sequentially on an
otherwise idle machine.

```bash
python3 scripts/benchmark_dynamic_profiles.py \
  --binary /tmp/netweevil-before-dynamic \
  --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --json .netweevil/reports/request-profiles/baseline.json

python3 scripts/benchmark_dynamic_profiles.py \
  --binary target/release/netweevil \
  --dataset north_nl_2026_05_10 \
  --profile examples/profiles/car_north_nl_2026_05.yml \
  --overrides examples/perf/profile_overrides.json \
  --reference .netweevil/reports/request-profiles/baseline.json \
  --json .netweevil/reports/request-profiles/candidate.json
```

The script asserts identical prepared-route results across binaries and before
and after dynamic compilation. It also records cold preparation, warm dynamic
latency, profile hash, acceleration selection and process memory. RSS includes
shared topology, worker scratch space and allocator retention; it is not the
cache's exact allocation size.

## Local measurements, 2026-09-07

The North Netherlands dataset used here has 2,107,900 nodes and 4,395,161
directed edges. These measurements use the first 300 routes of
`examples/perf/north_nl_routes.csv`, five timed passes after warmup, sequential
HTTP requests with a persistent connection, and release binaries. The baseline
is the release binary captured before this change. No builds ran during the
latency measurements.

| Request path | Median | p95 | Mean |
| --- | ---: | ---: | ---: |
| Original binary, prepared base profile | 1.191 ms | 2.612 ms | 1.331 ms |
| Updated binary, prepared base profile | 1.071 ms | 2.031 ms | 1.151 ms |
| Updated binary, warm request profile | 1.239 ms | 2.449 ms | 9.274 ms |
| Updated binary, prepared base after dynamic use | 1.084 ms | 2.030 ms | 1.159 ms |
| Original binary, file profile with the same changed settings | 1.048 ms | 2.165 ms | 8.846 ms |

All 300 prepared-route results matched before/after the change and after use of
the dynamic cache. All 300 dynamic results and the effective profile hash
matched the independently compiled file profile. No prepared-path regression
was observed; the lower timings should not be interpreted as a routing speedup.
For the changed profile, a separate accelerated-versus-exact search check also
matched distance, travel time and generalized cost on all 300 routes within
an absolute tolerance of `1e-6`.
The changed profile has slow outliers in both the dynamic and file-profile
runs, which explains its larger mean relative to the median.

Cold preparation took **4.630 seconds**, with a first-response time of
4.631 seconds. Process RSS grew from **2.54 GiB to 4.10 GiB** immediately after
compilation, then reached **4.25 GiB** after warm routing. This is one additional
cached profile. Its weights still used CCH with the existing restriction
sequence validation and exact fallback.

Raw local reports are in `.netweevil/reports/request-profiles/`, including
`baseline.json`, `candidate.json` and `file-profile.json`. They record binary
and corpus checksums, per-route result hashes, service metadata and memory.
Generated reports and the dataset are not committed.
