# Transit fusion and station routing

NetWeevil transit feeds can optionally bind GTFS stops to exact locations in a
street or indoor graph and consume profile-specific, directed network transfer
tables. With neither configured, routing keeps the existing coordinate snap and
straight-line transfer behavior.

## Stop bindings

A JSON binding table is feed-scoped and replaces all bindings on the bundle when
applied with `apply_transit_stop_bindings`:

```json
{
  "schema_version": 1,
  "feed_id": "hk_example_mtr",
  "bindings": [
    {"stop_id": "ILLUSTRATIVE-PLATFORM-A", "node": {"node_id": 18432}},
    {"stop_id": "ILLUSTRATIVE-PLATFORM-B", "edge": {"edge_id": 9012, "fraction": 0.35}},
    {
      "stop_id": "ILLUSTRATIVE-PLATFORM-C",
      "coordinate": {
        "lon": 114.1577,
        "lat": 22.2820,
        "z": -8.2,
        "z_window_m": 1.5,
        "attribute_filter": {"indoor_location": "platform"}
      }
    }
  ]
}
```

These Hong Kong-shaped ids, graph ids, and coordinates are illustrative, not
built-in station data. Replace them with values derived from the imported feed
and prepared network. A complete file is available at
[`examples/transit/hong_kong_example_stop_bindings.json`](../examples/transit/hong_kong_example_stop_bindings.json).

`load_transit_stop_bindings` also accepts CSV. Its supported columns are
`stop_id,kind,node_id,edge_id,fraction,lon,lat,z,z_window_m,attribute_filter`; the last
column is a JSON object encoded inside the CSV cell. `kind` may be omitted when
the populated target columns are unambiguous. Bindings are validated and
applied atomically. The bundle records a binding fingerprint so transfer tables
built against stale platform locations are rejected.

Network hosts receive the complete `TransitStop`, including its binding,
through `StreetTimeEstimator::point_to_stop_path`, `stop_to_point_path`, and
`stop_to_stop_path`. A host that only implements the original coordinate/time
method remains compatible for unbound stops, which continue to use their GTFS
longitude and latitude. A bound stop whose binding-aware path is unreachable
does not fall back to that coordinate method, because doing so would bypass the
platform/node constraint.

## Network transfer tables

`build_transit_transfer_table` enumerates nearby stop pairs and asks the injected
street/indoor router for each directed path. Its `TransitTransferBuildOptions`
contains the walking profile identifier, distance radius, and candidate cap.
Build separate tables for profiles such as `walk` and `step_free`.
Unreachable pairs are omitted, so an unavailable lift or an inaccessible
platform does not acquire a geometric fallback inside that table.

Transfer tables can be written as readable JSON (a `.json` path) or compact
bincode and loaded with `read_transit_transfer_table`. Construct a router with
`PreparedTransitRouter::new_with_transfer_tables`, then select one in a transit
request:

```json
{
  "modes": {
    "transfer_profile_id": "pedestrian_step_free_multilayer",
    "max_transfer_distance_m": 500
  }
}
```

When a profile is selected, the connection-scan search uses only that directed
network table for stop-to-stop transfers. It does not fall back to straight-line
walking for missing entries. The resulting access, transfer, or egress leg may
include `network_path` with `travel_time_s`, network distance, a 3D geometry,
routed edge IDs, and named cost components. This exposes the actual station
corridor, escalator, stairs, or lift path in the door-to-door result.

`modes.street_access: "network"` controls the first and last mile separately
from `transfer_profile_id`. The CLI must load the matching street graph and
compiled foot profile explicitly:

```bash
netweevil analyze transit-route \
  --feed hk_example_mtr \
  --request path/to/network-access-request.json \
  --street-dataset hk_pedestrian_3d \
  --street-profile examples/profiles/pedestrian_multilayer.yml

netweevil analyze transit-batch \
  --feed hk_example_mtr \
  --requests path/to/network-access-batch.json \
  --street-dataset hk_pedestrian_3d \
  --street-profile examples/profiles/pedestrian_multilayer.yml
```

Both flags are required when any executed request selects network street
access. The current CLI estimator is backed by a foot routing profile and
therefore accepts walk-only `modes.access` and `modes.egress`; bicycle or car
network first/last-mile requests fail clearly instead of being silently priced
as walking. Requests that retain the default `straight_line` model do not need
the street flags.

Network first/last-mile selection is strict. Each candidate must receive a
routed network path or network travel time from the host estimator. A missing
engine, disconnected pair, inaccessible paid area, or other `None` estimate
omits that candidate; if none remain, the transit result is unreachable.
NetWeevil never substitutes straight-line time inside `street_access:
"network"`. Coordinate-only estimators remain supported through
`street_time_s`, while binding-aware estimators can additionally return the
full path. Geometric fallback occurs only when the request explicitly selects
`street_access: "straight_line"`.

## Depart-after and arrive-by searches

Each dated or frequency departure has a distinct `run_index`. Transit legs
return it alongside the public trip ID, so separate vehicles using the same
GTFS trip cannot be treated as staying aboard one vehicle. Pickup and drop-off
are allowed only for regular scheduled stops. GTFS types `1`, `2` and `3` are
excluded; arranging service by phone or with the driver is not modeled.

Transit bundles must be re-imported for the current schema. The reader rejects
unsupported schemas before decoding connections. Forward departure indexes
store connection indices and continuation links instead of duplicating
connection structs. Links distinguish repeated visits to the same stop.

Geometric transfers include every stop within `max_transfer_distance_m`, and
the prepared router caches that adjacency for the active radius. Precomputed
network transfer tables still use their explicit build-time candidate limit.

Transit routes and transit service areas plan in either direction. With
`time.arrive_by: false` the scan settles the earliest arrival reachable after
`time.datetime`, and `search_window_s` bounds how long after that time a trip
may depart. With `time.arrive_by: true` the scan settles the latest departure
that still reaches the destination by `time.datetime`, and `search_window_s`
bounds how far before that deadline a trip may arrive.

The arrive-by scan is the mirror image of the depart-after one and reads the
same connection arrays. Board slack, transfer slack, `max_transfers`, transit
mode filters, minimum transit leg constraints, straight-line or `network`
access/egress pricing, stop bindings, and precomputed transfer tables all
apply unchanged; a state is kept only while the traveller can still be at that
stop and reach the target in time. Because transfer tables are directed, the
reverse scan walks the transfers that *end* at a stop, so a passage priced only
in the `A -> B` direction is never traversed backwards.

Results keep their usual shape. An arrive-by route returns legs in forward
chronological order with scheduled timestamps: `summary.departure_s` is the
latest feasible departure, `summary.arrival_s` is the resulting arrival at or
before the deadline, the access leg is anchored to the first boarding, and each
transfer walk starts a transfer slack after the preceding alighting. An
arrive-by service area reports, per stop, the latest clock time at which a
traveller can be there in `arrival_s`, with `travel_time_s` measured back from
the deadline; ridden segments carry the time from their departure to the
deadline. Journeys that cannot meet the deadline are `unreachable`.

## CLI and API

Bindings can be attached while importing a feed or applied atomically later:

```bash
netweevil transit import gtfs.zip --name hk_example_mtr \
  --service-start 2026-07-03 --service-days 7 \
  --stop-bindings examples/transit/hong_kong_example_stop_bindings.json

netweevil transit bindings apply --feed hk_example_mtr \
  examples/transit/hong_kong_example_stop_bindings.json
```

Replacing bindings invalidates the feed's registered transfer tables. Build a
new directed table with a compiled foot profile:

```bash
netweevil transit transfers build \
  --feed hk_example_mtr \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_step_free_multilayer.yml \
  --max-transfer-distance-m 500 \
  --max-candidates-per-stop 32
```

The API loads tables registered for its active dataset. A transit route or
service-area payload selects one in `request.modes`:

```json
{
  "feed_id": "hk_example_mtr",
  "request": {
    "modes": {
      "street_access": "network",
      "transfer_profile_id": "pedestrian_step_free_multilayer"
    }
  }
}
```
