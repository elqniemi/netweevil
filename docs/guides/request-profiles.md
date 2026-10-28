# Request-defined profiles

`POST /v1/route` accepts a profile in the request. Use this to test profile
changes without a profile file and a restart. NetWeevil compiles the profile
in memory and customizes the CCH weights of the dataset. It caches the
prepared engine. Later requests with the same effective profile use that
engine again.

The compilation is network-wide on first use. The engine does not evaluate
costs lazily during the search.

Requests without `profile` or `profile_overrides` use the prepared profile
lookup. They do not serialize profiles, acquire cache locks or wait for a
compilation slot.

## Prerequisites

- A running API with a dataset and a default profile.
- For overrides: a base profile that the API loads, or the default profile.

## Override a loaded profile

1. Set `profile_id` to the base profile. Omit it to use the default profile
   of the server.
2. Put the changes in `profile_overrides`.
3. Send the request to `/v1/route`.

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

```bash
curl -i http://127.0.0.1:8080/v1/route \
  -H 'Content-Type: application/json' \
  --data-binary @examples/api/route_profile_overrides.json
```

### Merge rules

- Objects merge recursively.
- Arrays and scalar values replace the previous values. For example,
  `speed_rules` replaces the full ordered rule list.
- An empty object keeps the existing entries in a map. It does not clear
  them.
- `null` is a literal value. It can clear optional fields such as
  `slope_model`. It does not delete a map key.

The profile schema and parameter meanings are the same as for file profiles.
A preference value is a weight. `use_tolls: 0.0` is not a hard toll ban. Use
exclusion rules for hard exclusions.

!!! warning
    A request cannot restore roads, directions or attributes that the dataset
    import did not keep.

## Supply a complete profile

1. Put a complete profile document as JSON in `profile`. Include the
   `profile` header with `id`, `label`, `mode` and `defaults_pack`.
2. Do not send `profile_id` or `profile_overrides` with `profile`.
3. Send the request to `/v1/route`.

Optional fields that you do not supply use the same defaults as file
profiles.

## Errors

| Condition | HTTP status |
| --- | --- |
| Unknown fields or an invalid profile | 400 |
| `profile` together with `profile_id` or `profile_overrides` | 400 |
| Unknown base profile | 404 |

## Scope

Request-defined profiles work on `/v1/route` and `/v1/directions` only. Other
analyses, transit bindings, simulation and the QGIS profile selector use
loaded profiles. Route options, such as GeoJSON output, work as usual after
the engine resolves the profile.

To route a different vehicle in any street analysis, use the request
`vehicle` block instead. It does not compile a profile. See
[Routing](routing.md#override-the-vehicle-in-a-request).

!!! note
    Request-defined profiles ignore traffic data. See [Traffic](traffic.md).

## Cache and resource use

The cache keeps two engines by default.

1. Set `NETWEEVIL_DYNAMIC_PROFILE_CACHE_ENTRIES` to an integer from 1 to 16.
2. Start the server.

Eviction removes the least recently used entry. Routes that use an evicted
engine keep it until they finish. The limit counts the engines in the cache.
It does not count bytes or the engines that active routes hold.

!!! warning
    A cold compilation needs additional memory. A large dataset can need
    gigabytes for each prepared profile. Start with a small entry limit.

The cache belongs to one immutable dataset runtime. Its key is the effective
profile document:

- Equivalent overrides and complete documents share entries after the engine
  resolves the defaults.
- A profile that matches a loaded profile uses that prepared engine.
- The profile header and return defaults are part of the document. A change
  to them also changes the key.

Cold compilations run one at a time on a dedicated single-thread Rayon pool.
They do not take a routing-worker permit or use the global Rayon pool.
Identical concurrent requests share one compilation and its engine. This is
also true when other profiles cause cache eviction. A cancelled HTTP request
does not release the slot while its compilation runs. It does not discard the
completed engine. Failed compilations do not enter the cache.

CPU, memory bandwidth and allocator capacity are still shared with routing.
The isolation keeps compilation work off the prepared request path. It
cannot prevent all hardware contention under mixed load. For strict latency
isolation, run request-profile traffic in a separate API process with CPU and
memory limits.

Request profiles do not create files under `.netweevil/`. They do not change
loaded profiles, and they do not appear in `/v1/profiles`. The startup
compilation of named profiles still uses its persistent cache.

!!! note
    To reproduce an experiment, save the base profile and the overrides, or
    save the complete effective document.

## Timing headers

Dynamic responses include two HTTP headers, also for GeoJSON output:

| Header | Meaning |
| --- | --- |
| `x-netweevil-profile-cache` | `miss`, `hit` or `preloaded` |
| `x-netweevil-profile-prepare-ms` | Time to resolve the profile: validation, queueing, compilation and preparation as applicable |

`service.profile_id` names the base profile. The request's `profile` or
`profile_overrides` define the effective profile. `service.acceleration`
describes the engine that the request used. Prepared responses do not add
these headers.

## Benchmark request profiles

`scripts/benchmark_dynamic_profiles.py` measures request profiles with a
release binary and a local dataset. The script does these steps:

1. It starts its own API.
2. It checks health and readiness.
3. It warms the route corpus.
4. It measures HTTP latency.
5. It samples process RSS on Linux.

Run binaries one after the other on an idle machine.

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

The script asserts identical prepared-route results across binaries, and
before and after dynamic compilation. It records cold preparation, warm
dynamic latency, the acceleration selection and process memory. The reports
include binary and corpus checksums, result hashes per route and service
metadata.

RSS includes shared topology, worker scratch space and allocator retention.
It is not the exact allocation size of the cache.

As a guide, on a dataset of about 2 million nodes, cold preparation of one
request profile takes a few seconds. It adds about 1.5 GiB of RSS. Warm
request-profile routes have a latency similar to prepared routes.

## Related pages

- [Routing](routing.md).
- [Benchmarking](benchmarking.md).
- [HTTP API reference](../api/reference.md) for the full schema.
