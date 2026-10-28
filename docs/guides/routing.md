# Street routing

This guide shows how to calculate a street route between two points. It also
covers alternatives, avoid areas, vehicle dimensions, failure modes,
waypoints, turn-by-turn directions, location inspection, search expansion,
network vector tiles and DEM heights.

## Prerequisites

- An imported dataset. See [Datasets](datasets.md).
- A profile that you compiled for that dataset with `netweevil profile compile`.
- For HTTP requests: a running API (`netweevil api serve`). The API must load
  the profile with `--default-profile` or `--profile`.

## Calculate a route

=== "CLI"

    1. Write a route request file, for example `examples/requests/route.json`.
    2. Run the route:

        ```bash
        cargo run -p netweevil-cli -- analyze route \
          --dataset groningen_2026_05 \
          --profile examples/profiles/car_research_v1.yml \
          --request examples/requests/route.json \
          --out .netweevil/runs/example-route.geojson
        ```

    3. Open the result file. The `--out` extension selects the format.
       Without `--out`, the CLI writes JSON to `.netweevil/runs/`.

=== "HTTP"

    1. Put the route request in the `request` field of the envelope.
    2. Send the envelope to `POST /v1/route`:

        ```bash
        curl -X POST http://127.0.0.1:8080/v1/route \
          -H 'content-type: application/json' \
          --data @examples/api/ile_de_france_route_car.json
        ```

    3. Add `?format=geojson` to get GeoJSON instead of JSON.

The HTTP envelope is `{profile_id?, engine_mode?, request}`. Without
`profile_id`, the API uses its default profile. `engine_mode` is `auto`
(default) or `ignore_multi_edge_restrictions`.

```json
{
  "profile_id": "car_research_v1",
  "request": {
    "route_id": "route_demo_001",
    "origin": { "id": "origin_a", "lon": 6.5665, "lat": 53.2194 },
    "destination": { "id": "destination_b", "lon": 6.5716, "lat": 53.2148 },
    "snap": { "max_distance_m": 500.0 },
    "returns": {
      "geometry": "full",
      "segment_rows": true,
      "road_type_breakdown": ["distance_m", "time_s"],
      "surface_breakdown": ["distance_m", "time_s"]
    }
  }
}
```

A route request can also carry `profile_overrides` or a complete `profile`.
See [Request-defined profiles](request-profiles.md).

### Route request fields

| Field | Default | Meaning |
| --- | --- | --- |
| `route_id` | required | Identifier that the result repeats |
| `origin`, `destination` | required | `{id, lon, lat, z?}`. `z` is an elevation in metres in the dataset's vertical datum |
| `snap.max_distance_m` | 500 | Largest distance from a point to its network position |
| `snap.z_window_m` | none | With a point `z`, rejects network positions farther away in elevation |
| `snap.attribute_filters` | `{}` | Source or semantic attributes that candidate edges must match |
| `snap.point_constraints` | `{}` | Bearing and road-side constraints per point `id`. See [Snap constraints](snap-constraints.md) |
| `connectivity` | strict | Policy for disconnected network parts. See [Disconnected endpoints](#disconnected-endpoints) |
| `fallback` | strict | Failure modes. See [Failure modes](#failure-modes) |
| `returns` | profile defaults | Output options. See [Return options](#return-options) |
| `alternatives` | one route | See [Alternative routes](#alternative-routes) |
| `avoid` | none | See [Avoid locations, areas and features](#avoid-locations-areas-and-features) |
| `vehicle` | profile vehicle | See [Vehicle dimensions, weight and hazardous goods](#vehicle-dimensions-weight-and-hazardous-goods) |
| `departure_time`, `arrive_by` | none | RFC 3339 times. See [Multilayer and temporal routing](multilayer-temporal-routing.md) |
| `scenario`, `holidays`, `overlays` | none | Temporal content. See [Multilayer and temporal routing](multilayer-temporal-routing.md) |
| `max_labels_per_state` | 16 | Label guard for exact temporal searches |
| `arrive_by_lookback_s` | 86400 | Search window before an `arrive_by` time |
| `constraints`, `pareto` | none | Component budgets and Pareto routes. See [Multilayer and temporal routing](multilayer-temporal-routing.md#component-budgets-and-pareto-routes) |
| `traffic` | `auto` | Traffic behavior for timed requests. See [Traffic](traffic.md) |

### Return options

| Field | Default | Meaning |
| --- | --- | --- |
| `geometry` | `none` | `none`, `full` or `segments` |
| `segment_rows` | `false` | Adds one row per traversed segment, with edge names |
| `road_type_breakdown` | `[]` | Metrics per road class: `distance_m`, `time_s` |
| `surface_breakdown` | `[]` | Metrics per surface: `distance_m`, `time_s` |
| `penalty_breakdown` | `false` | Adds the penalties that make up the cost |
| `explain_cost_derivation` | `false` | Adds an explanation of the cost |
| `admin_areas` | `false` | Adds `regions`. See [Regions and elevation](regions-and-elevation.md) |
| `elevation` | `false` | Adds `elevation`. See [Regions and elevation](regions-and-elevation.md) |

### Response speed

Some options make a route request slower:

- JSON is the fastest response format.
- `?format=geojson` adds response shaping work.
- `segment_rows: true` loads the edge-name bundle.
- `geometry: full` and `geometry: segments` return coordinates and cost more.

For the fastest path, follow these steps:

1. Load the profiles at startup with repeated `--profile` flags.
2. Send JSON, not `?format=geojson`.
3. Set `returns.geometry` to `none`.
4. Set `returns.segment_rows` to `false`.
5. Keep the breakdown arrays empty.

A summary-only JSON response also omits `node_path` and `edge_path`.

```json
{
  "request": {
    "route_id": "fast_route_001",
    "origin": { "id": "a", "lon": 2.369, "lat": 48.853 },
    "destination": { "id": "b", "lon": 2.236, "lat": 48.892 },
    "returns": {
      "geometry": "none",
      "segment_rows": false,
      "road_type_breakdown": [],
      "surface_breakdown": [],
      "penalty_breakdown": false,
      "explain_cost_derivation": false
    }
  }
}
```

## Alternative routes

Set `alternatives.max_routes` to a value above 1 to get alternative routes.
Each alternative has its own `regions` and `elevation` when you request them.

| Field | Default | Meaning |
| --- | --- | --- |
| `max_routes` | 1 | Number of routes, including the best route |
| `max_cost_ratio` | 1.35 | Largest cost of an alternative relative to the best route |
| `max_extra_time_s` | none | Largest extra travel time |
| `max_extra_distance_m` | none | Largest extra distance |
| `min_jaccard_distance` | 0.2 | Smallest difference in edges from the routes already found |
| `max_search_attempts` | 24 | Upper limit of searches while the engine collects alternatives |

Each search attempt is a full route query without acceleration. Long routes
can need many attempts, so the limit keeps the cost bounded. See
`examples/requests/route_alternatives.json`.

## Avoid locations, areas and features

Route, directions, waypoints, OD, matrix, accessibility, service-area,
service-area-sequence, betweenness and trace-match requests accept an `avoid`
object next to `snap` and `returns`. For matrices and accessibility, put
`avoid` on either point set. The engine merges the sets.

```json
"avoid": {
  "points": [{"lon": 6.5690, "lat": 53.2178, "radius_m": 30}],
  "polygons": [{"rings": [[[6.565, 53.216], [6.570, 53.216], [6.570, 53.219], [6.565, 53.219]]], "penalty_factor": 4.0}],
  "source_feature_ids": [7064211]
}
```

| Field | Meaning |
| --- | --- |
| `points[]` | `{lon, lat, radius_m?, penalty_factor?}`. Matches edges within `radius_m` of the point. The default radius is 10 m. The maximum is 10 km. The engine measures the distance as for snapping |
| `polygons[]` | `{rings, penalty_factor?}`. Matches edges that touch the polygon, boundary included. `rings` are GeoJSON polygon coordinates: the outer ring, then holes. Rings can be open or closed |
| `source_feature_ids` | Matches every edge of these source features (OSM way ids). These edges are always excluded |

Without `penalty_factor`, the engine excludes the matching edges. Routes do
not use them, and endpoints do not snap onto them. A `penalty_factor` from 1
to 1000 multiplies their generalized cost instead. Travel times do not
change.

The results stay exact. The engine re-customizes the CCH with the excluded
edges at infinity and the penalized edges scaled. The prepared engine keeps
the four most recent avoid sets per profile. The cache key is the set of
edges that the avoid set resolves to.

A single route first runs on the unmodified hierarchy between the endpoints
that the avoid engine snaps to. If that best path touches no avoided edge, it
is also optimal with the avoid set. Then no re-customization occurs.

See `examples/api/route_avoid.json`.

## Vehicle dimensions, weight and hazardous goods

OSM imports keep these restrictions: `maxheight`, `maxwidth`, `maxlength`,
`maxweight`, `maxaxleload`, `hazmat`, `hazmat:water`,
`hazmat:adr_tunnel_cat`, `trailer` and `hgv_articulated`. They also keep
vehicle-property conditions such as `hgv:conditional=no @ (weight>7.5)`.

A profile's `vehicle` block describes the physical vehicle. Compilation
excludes each edge whose restriction the vehicle violates.

```yaml
vehicle:
  height_m: 4.0
  width_m: 2.55
  length_m: 16.5
  weight_t: 40.0        # actual gross weight, metric tonnes
  axle_load_t: 11.5
  hazmat: false         # dangerous goods; adr_tunnel_code (B-E) implies it
  hazmat_water: false   # water-polluting goods
  trailers: 1           # excluded by trailer=no and hgv_articulated=no
```

| Field | Meaning |
| --- | --- |
| `height_m`, `width_m`, `length_m` | Dimensions in metres |
| `weight_t` | Actual gross weight in metric tonnes |
| `axle_load_t` | Heaviest axle load in metric tonnes |
| `hazmat` | Carries dangerous goods |
| `hazmat_water` | Carries water-polluting goods |
| `adr_tunnel_code` | ADR tunnel category of the load, `B` to `E`. Implies `hazmat` |
| `trailers` | Number of trailers. `trailer=no` and `hgv_articulated=no` exclude a vehicle with trailers |

All fields are optional. An unset field never violates a restriction. The
truck, bus, taxi, motorcycle and moped templates in `examples/profiles/`
have a `vehicle` block. `GET /v1/workspace/profile-templates` also offers
them.

### Override the vehicle in a request

Route, directions, waypoints, trip, OD, matrix, accessibility, service-area,
service-area-sequence, betweenness and trace-match requests accept a
`vehicle` block next to `avoid`. Its fields replace the profile's fields one
by one. The engine does not compile a new profile.

```json
{
  "profile_id": "truck_hgv_v1",
  "request": {
    "route_id": "low_truck",
    "origin": {"id": "o", "lon": 6.5665, "lat": 53.2194},
    "destination": {"id": "d", "lon": 6.5716, "lat": 53.2148},
    "vehicle": {"height_m": 3.4, "weight_t": 12.0}
  }
}
```

The engine resolves the merged vehicle to the set of edges that it excludes:

- **Same set as the profile.** The request runs on the prepared CCH engine.
- **Superset.** A taller, heavier or hazmat vehicle adds exclusions. The extra
  edges join the avoid set of the request. The request uses the avoid engines
  and their single-route fast path.
- **Other set.** A vehicle can pass edges that the profile's vehicle cannot.
  Compiled profiles keep the metrics of the edges that their vehicle
  excludes. The engine restores those edges, removes the new ones and
  re-customizes the CCH. The prepared engine keeps engines for the four most
  recent exclusion sets. Vehicles that exclude the same edges share one
  engine.

Each case returns the same result as a profile compiled with the merged
vehicle. Under traffic, the departure-bucket engines apply the vehicle in the
same way, with traffic speeds on restored edges.

For matrices and accessibility, each point set that names a vehicle must name
the same vehicle.

As a guide, the first request for a new exclusion set prepares its engine in
a few hundred milliseconds on a province-size dataset. Later requests with
that set route as fast as the prepared profile.

See `examples/api/route_truck_vehicle.json`. That request routes a 3.5 t van
on the truck template. The van passes limits that exclude the 40 t truck, so
the route is shorter.

## Disconnected endpoints

`connectivity` controls what happens when the origin and destination are on
network parts that do not connect.

| Field | Default | Meaning |
| --- | --- | --- |
| `disconnected` | `strict` | `strict`, `ignore_unreachable`, `hop_origin_to_nearest_reachable_component`, `hop_destination_to_nearest_reachable_component` or `hop_either_end` |
| `max_hop_distance_m` | `snap.max_distance_m` | Largest snap distance for an endpoint that hops to another component |
| `report_hop_distance_separately` | `false` | Reports the hop distance separately from the route distance |

- `strict` fails the request when no route exists.
- `ignore_unreachable` returns an unreachable result instead of an error.
- The `hop_*` modes let the named endpoint snap farther than
  `snap.max_distance_m`, up to `max_hop_distance_m`. The endpoint snaps to a
  component that reaches the other endpoint.

See `examples/requests/route_disconnected_strict.json`.

## Failure modes

Route `fallback` flags make illegal movements available at a penalty:

| Field | Meaning |
| --- | --- |
| `allow_reverse_oneway` | Allows travel against a one-way direction |
| `allow_illegal_turn` | Allows illegal turns |
| `ignore_turn_restrictions` | Allows movements through turn restrictions |
| `allow_uturn_where_normally_forbidden` | Allows forbidden U-turns |
| `auto_relax_unreachable` | Routes strictly first. Relaxes only when no legal route exists |
| `penalties` | `reverse_oneway_penalty_s`, `illegal_turn_penalty_s`, `ignored_turn_restriction_penalty_s`, `forbidden_uturn_penalty_s` |
| `max_illegal_distance_m` | Largest reverse-oneway distance in metres |
| `max_illegal_turns` | Largest number of relaxed turn restrictions |

The route minimizes cost plus penalties. Thus the engine returns a legal
route when it costs no more than the cheapest penalized route. A returned
route is `degraded` only if it uses an illegal movement.

When the cheapest penalized route breaks a cap, the engine returns the
cheapest route and snapped orientation within the caps. The request fails
only when no route stays within the caps. The engine drops alternatives that
break a cap.

See `examples/requests/route_degraded_fallback.json`.

!!! note
    Service areas do not support failure modes. Keep `fallback` at its
    default value in a service-area request.

## Routes with intermediate locations

`POST /v1/waypoints` visits ordered locations. It returns route legs and
their combined distance, travel time and generalized cost.

```bash
curl -X POST 'http://127.0.0.1:8080/v1/waypoints?format=geojson' \
  -H 'Content-Type: application/json' \
  --data @examples/api/north_nl_waypoints.json
```

| Field | Default | Meaning |
| --- | --- | --- |
| `route_id` | required | Identifier of the result |
| `waypoints[]` | required | `{point, kind}`. `kind` is `break` (default) or `through` |
| `snap`, `returns` | defaults | As for a route |
| `optimize_order` | `false` | Optimizes the order of intermediate stops |
| `avoid`, `vehicle` | none | As for a route |

- A `break` location splits legs and resets the turn history.
- A `through` location stays inside its leg. It keeps the turn history and
  prohibits an immediate U-turn at the location.

Set `optimize_order: true` to minimize directed static costs. The first and
last locations stay fixed. The solver orders up to 16 intermediate `break`
locations exactly (Held–Karp). Larger sets use the local search of the trip
solver. See [Trip optimization](trip-optimization.md).

The result contains `waypoint_order` with the original input indexes.
`optimization_method` names the solver that ran.

!!! note
    A waypoint request supports 2 to 128 locations. Waypoint routing uses
    static costs only. The engine rejects temporal, scenario, holiday and
    component-constrained requests. With `optimize_order`, all intermediate
    locations must be `break` locations. Waypoint requests use a loaded
    profile.

## Turn-by-turn directions

`POST /v1/directions` accepts the same payload as `/v1/route`, including
request-defined profiles. It also accepts `directions_options`.

```json
"directions_options": {
  "language": "nl",
  "units": "metric",
  "driving_side": "right",
  "facility_attribute": "feature_type"
}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `language` | English | BCP 47 tag. Phrase tables exist for `en`, `nl`, `de`, `fr`, `fi` and `zh-Hant` (also `zh-HK`, `zh-TW`, `zh`) |
| `units` | `metric` | `metric` or `imperial` |
| `driving_side` | dataset | `left` or `right`. Replaces the side of the dataset |
| `facility_attribute` | profile | Attribute that marks stairs, escalators, lifts and moving walkways. The default is the profile's `facilities.attribute` |

The engine drops language subtags until a table matches: `nl-BE` uses `nl`.
An unknown language falls back to English and adds a warning.
`GET /v1/service` lists the languages under
`capabilities.directions_languages`.

Without `driving_side`, the engine uses the traffic side of the country at
the origin, when the dataset has admin areas. Otherwise it uses right-hand
traffic.

=== "CLI"

    `netweevil analyze directions` runs the same engine locally. It reads a
    bare route request or a `/v1/directions` body. `--language`, `--units`,
    `--driving-side` and `--facility-attribute` replace the matching
    `directions_options`. `--out` must be a `.json` path. The command records
    a `directions` run manifest.

    ```bash
    cargo run -p netweevil-cli -- analyze directions \
      --dataset north_nl_2026_05_10 \
      --profile examples/profiles/car_north_nl_2026_05.yml \
      --request examples/api/north_nl_directions.json \
      --language de --units imperial \
      --out .netweevil/runs/example-directions.json
    ```

=== "HTTP"

    ```bash
    curl -X POST http://127.0.0.1:8080/v1/directions \
      -H 'content-type: application/json' \
      --data @examples/api/north_nl_directions.json
    ```

### Directions result

The response is `{service, result: {language, units, driving_side,
driving_side_origin, warnings?, route, maneuvers}}`. The route has full
geometry and segments.

Each maneuver has a `kind`, a written `instruction` and `verbal` text:

- `verbal.pre_alert` is for announcement `pre_alert_distance_m` before the
  maneuver.
- `verbal.alert` is for announcement at the maneuver. It includes the next
  maneuver when that maneuver follows closely.
- `verbal.post_transition` gives the distance to continue.

The maneuver kinds are:

- `depart`, `continue`, turns from `slight_left` to `sharp_right`, and
  `uturn_left` and `uturn_right`. A dual-carriageway crossing becomes one
  U-turn.
- `ramp_*`, `exit_*` and `merge*` for link roads. `keep_*` at forks.
- `enter_roundabout` with `roundabout_exit_count`, and `exit_roundabout`.
- `ferry_enter` and `ferry_exit`.
- `stairs`, `escalator`, `elevator` and `travelator`, with `level_change_m`.
- `arrive` with `arrival_side`.

Other maneuver fields:

- `street_name` and `street_ref` name the road that the route enters. The
  engine announces a name change only when no name or route number carries
  over.
- `signs` holds `exit_number` (from `junction:ref` or the motorway junction
  node), `branch` (`destination:ref`), `toward` (`destination`,
  `destination:street`, `exit_to`) and the junction `name`.
- `lanes` lists the approach lanes from `turn:lanes`, left to right. Each
  lane has `indications` and `valid` and `active` flags.
- Each maneuver identifies a half-open range in `route.edge_path`. Distance
  and edge time are the sums over those segments. Turn penalties stay in the
  route summary.
- Time-dependent requests add `begin_time` and `waiting_time_s`.

Walking and cycling routes get plain turns: no ramps, exits, lanes or U-turn
collapse. The engine joins two turns with a connector of up to 10 m into one
turn. If they turn the route around, the result is a sharp turn. Only a motor
vehicle that crosses a dual carriageway gets a U-turn. A small jog that keeps
the heading, such as a step onto a parallel cycle path, gets no step. The
exception is a jog onto a street with a new name.

Roundabout exit counts include the legal exits passed and the chosen exit.
The engine infers no exit number when the route starts or ends inside a
roundabout.

!!! note
    Directions require a legal street route with all turn restrictions
    enforced. Time-dependent requests (`departure_time`, `arrive_by`,
    scenarios) are supported. Directions ignore traffic data.

`/v1/transit-directions` accepts the same `directions_options`. See
[Transit](transit.md#transit-directions).

## Inspect routing locations

`POST /v1/locate` reports, for each input point:

- `candidates`: the snap candidates that a route starts from (`direction:
  "origin"`) or ends at (`"destination"`). Each candidate has the snapped
  coordinates, directed edge ids, the position along the edge, component ids
  and the snap distance.
- `nearest`: the `count` nearest road segments that the profile can use,
  nearest first. Each segment has its distance, its projected point and its
  directed edges. Each edge has `fraction`, way id, name, road class, highway
  tag, surface, posted speed, lanes, length and profile-aware `oneway`. It
  also has the source access flags, and the profile's travel time, speed and
  generalized cost.

| Field | Default | Meaning |
| --- | --- | --- |
| `points` | required | Input points `{id, lon, lat, z?}` |
| `snap` | defaults | Same snapping options as a route, including `point_constraints` |
| `direction` | `origin` | `origin` or `destination` |
| `count` | 1 | Number of nearest segments, from 1 to 100 |

Both lists use the same spatial index, elevation window and attribute filters
as route requests. A point that matches nothing within
`snap.max_distance_m` gets empty lists. Invalid input fails the request.

```bash
curl -X POST http://127.0.0.1:8080/v1/locate \
  -H 'Content-Type: application/json' \
  --data @examples/api/locate.json
```

`examples/api/locate_constraints.json` sends a bearing and curb-side
constraint.

## Show the search expansion

`POST /v1/expansion` returns the edges that the exact edge-based Dijkstra
search settles, in settle order. The response is a GeoJSON
`FeatureCollection` for debugging or animation.

| Field | Default | Meaning |
| --- | --- | --- |
| `action` | required | `route`, `service_area` or `hierarchy` |
| `origin` | required | Start point |
| `destination` | none | Required for `route` and `hierarchy` |
| `limit` | none | Search limit for `service_area` |
| `metric` | `travel_time_s` | `travel_time_s` or `distance_m` |
| `direction` | `outbound` | `outbound` or `inbound`, for `service_area` |
| `snap` | defaults | Snapping options |
| `max_edges` | 50,000 | Stops the search. The maximum is 500,000 |

- `route` searches forward on generalized cost from `origin` until it
  settles `destination`. Production routes run on the contraction hierarchy,
  whose search space is shortcuts. This action shows the exact reference
  search over the same graph, turn costs and restrictions. It reaches the
  same optimal cost.
- `service_area` searches up to `limit` on `metric`, outbound from `origin`
  or inbound to it.
- `hierarchy` records the contraction hierarchy query that the route endpoint
  runs. It shows the upward forward search and the backward search,
  interleaved as they run. Each settled state is a street edge. A shortcut
  joins it to `previous_edge_index`. Profiles without a hierarchy reject
  this action.

Each feature has `order`, `edge_index`, `edge_id`, `cost`,
`previous_edge_index` and `side` (`forward` or `backward`). `meta` has
`settled_edge_count`, `truncated` and, for routes, `route_cost`.

!!! note
    The expansion does not apply failure modes or departure times.

```bash
curl -X POST http://127.0.0.1:8080/v1/expansion \
  -H 'Content-Type: application/json' \
  --data @examples/api/expansion_route.json
```

## Network vector tiles

`GET /v1/tiles/{z}/{x}/{y}.mvt?profile_id=...` serves the edges that the
profile can use as Mapbox Vector Tiles. The layer is `network`, the extent is
4096 and the buffer is 64 units.

- A two-way segment is one feature. A one-way segment points in its travel
  direction.
- Properties: `edge_id`, `way_id`, `road_class`, `highway`, `oneway`
  (profile-aware), `access_car`, `access_bicycle`, `access_foot`,
  `max_speed_kph`, `speed_kph` and `name`. Two-way segments also have
  `reverse_edge_id` and `reverse_speed_kph`.
- Zoom 14 and higher includes all edges. Zoom 12 and 13 drop service roads,
  tracks and paths. Zoom 10 and 11 keep motorways, trunks, primaries and
  ferries. Lower zooms keep motorways and trunks.
- A tile holds at most 100,000 features. The `X-NetWeevil-Truncated` header
  reports a cut tile.

Use the URL template as a MapLibre or QGIS vector tile source:

```bash
curl -o tile.mvt 'http://127.0.0.1:8080/v1/tiles/15/16981/10640.mvt?profile_id=car_research_v1'
```

The web console has a "Routing network" toggle that draws these tiles.

## Network edges in a bounding box

`POST /v1/network/edges` returns the directed edges inside a `bbox`. Each
edge has its source attributes and the travel time, speed and cost of the
selected profile. `compare_profile_id` adds a second profile and the
differences. The web console's network explorer uses this endpoint.

## DEM heights

`POST /v1/height` returns DEM heights for a dataset that you imported with
`--dem`. See [Regions and elevation](regions-and-elevation.md#query-dem-heights).

## Related pages

- [Batch analysis](batch-analysis.md) for many routes, OD pairs and matrices.
- [Request-defined profiles](request-profiles.md).
- [Snap constraints](snap-constraints.md).
- [Traffic](traffic.md) for departure-time routing with speeds.
- [HTTP API reference](../api/reference.md) for the full schema.
