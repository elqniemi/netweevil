# API request examples

This folder contains request bodies for the NetWeevil HTTP API. The
[API guide](../../docs/api/index.md) and
[`openapi.yaml`](../../docs/api/openapi.yaml) describe all endpoints.

## Files

| File | Endpoint | Dataset | Profile |
|---|---|---|---|
| `route_avoid.json` | `POST /v1/route` | Groningen | `car_research_v1` |
| `route_truck_vehicle.json` | `POST /v1/route` | Groningen | `truck_hgv_v1` |
| `route_profile_overrides.json` | `POST /v1/route` | North Netherlands | `car_north_nl_2026_05` |
| `route_traffic.json` | `POST /v1/route` | North Netherlands | `car_north_nl_2026_05` |
| `north_nl_directions.json` | `POST /v1/directions` | North Netherlands | default profile |
| `north_nl_waypoints.json` | `POST /v1/waypoints` | North Netherlands | `car_north_nl_2026_05` |
| `north_nl_match.json` | `POST /v1/match` | North Netherlands | `car_north_nl_2026_05` |
| `locate.json` | `POST /v1/locate` | North Netherlands | `car_north_nl_2026_05` |
| `locate_constraints.json` | `POST /v1/locate` | North Netherlands | `car_north_nl_2026_05` |
| `expansion_route.json` | `POST /v1/expansion` | Groningen | `car_research_v1` |
| `traffic_live_updates.json` | `POST /v1/traffic/live` | Groningen | any |
| `height.json` | `POST /v1/height` | Hong Kong, imported with `--dem` | any |
| `ile_de_france_route_car.json` | `POST /v1/route` | Île-de-France | default profile |
| `ile_de_france_route_pedestrian.json` | `POST /v1/route` | Île-de-France | `pedestrian_research_v1` |
| `ile_de_france_od.json` | `POST /v1/od` | Île-de-France | default profile |
| `ile_de_france_matrix.json` | `POST /v1/matrix` | Île-de-France | default profile |
| `groningen_province_admin_bounds.geojson` | `dataset import --admin-boundaries` | Groningen | none |

The Groningen and North Netherlands examples use points in the city of
Groningen.

## Prepare the API

1. Import a dataset. This example imports the Île-de-France extract:

    ```bash
    cargo run --release -p netweevil-cli -- dataset import \
      datasets/ile-de-france-latest.osm.pbf --name ile_de_france_2026_03
    ```

2. Compile each profile that you want to load:

    ```bash
    cargo run --release -p netweevil-cli -- profile compile \
      --dataset ile_de_france_2026_03 \
      --profile examples/profiles/car_research_v1.yml
    cargo run --release -p netweevil-cli -- profile compile \
      --dataset ile_de_france_2026_03 \
      --profile examples/profiles/pedestrian_research_v1.yml
    ```

3. Start the API with a default profile. Add each other profile with
   `--profile`:

    ```bash
    cargo run --release -p netweevil-cli -- api serve \
      --dataset ile_de_france_2026_03 \
      --default-profile examples/profiles/car_research_v1.yml \
      --profile examples/profiles/pedestrian_research_v1.yml
    ```

4. Read the loaded dataset and profiles:

    ```bash
    curl http://127.0.0.1:8080/v1/service
    ```

## Send a request

Send a file with `curl`:

```bash
curl -X POST http://127.0.0.1:8080/v1/route \
  -H 'content-type: application/json' \
  --data @examples/api/ile_de_france_route_car.json
```

Add `?format=geojson` to get GeoJSON from endpoints that support it.

## Make routes fast

These settings give the fastest route requests:

- Load all profiles at startup with `--profile`.
- Use JSON, not `format=geojson`.
- Set `returns.geometry` to `none`.
- Set `returns.segment_rows` to `false`.
- Keep the breakdown arrays empty.

With these settings, the API does not read geometry or edge names. The result
also does not contain `node_path` and `edge_path`.

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

## Notes on the examples

### Avoid areas and vehicle limits

`route_avoid.json` routes through the center of Groningen. It excludes the
edges near a point and adds a penalty to a block.

`route_truck_vehicle.json` routes a 3.5 t van with the truck profile
`examples/profiles/truck_hgv_v1.yml`. The request `vehicle` block gives the
height, weight and length of the van. These values replace the 40 t truck
limits of the profile, so the route is shorter.

The [routing guide](../../docs/guides/routing.md) describes these options.

### Request-defined profiles

`route_profile_overrides.json` changes the profile in the request. The first
request compiles the changed profile. Later requests with the same profile use
the cached engine. See [request profiles](../../docs/guides/request-profiles.md).

### Traffic

`route_traffic.json` has a `departure_time` in the evening peak. The API uses
the traffic bucket of that time. `traffic_live_updates.json` slows one road
and closes another. See [traffic](../../docs/guides/traffic.md).

### Heights

`height.json` asks for DEM heights along three Hong Kong points. It resamples
the line every 100 m and returns `[distance, height]` pairs. Import the dataset
with `--dem`. See [regions and elevation](../../docs/guides/regions-and-elevation.md).

### Trace matching and snap constraints

`north_nl_match.json` sends a GPS trace with timestamps. See
[trace matching](../../docs/guides/trace-matching.md).

`locate_constraints.json` gives a bearing and a curb side for a point. The
same `snap.point_constraints` map works in routes, directions, matrix points,
waypoints and trace matching. See
[snap constraints](../../docs/guides/snap-constraints.md).

### Locate

`POST /v1/locate` gives two lists for each point:

- `candidates`: the snap candidates of a route origin or destination, with
  snapped coordinates, directed edge IDs, the position along the edge,
  component IDs and the snap distance.
- `nearest`: the `count` nearest road segments that the profile can use
  (1 to 100, default 1). Each segment has its directed edges with their
  attributes and the travel time, speed and cost of the profile.

A point without a match in `snap.max_distance_m` gets empty lists.

### Search expansion

`POST /v1/expansion` returns the edges that a search settles, in settle
order, as GeoJSON. Use it to debug or animate a search.

- `action: "route"` runs the exact edge-based Dijkstra search from `origin` to
  `destination`. It gives the same optimal cost as the route endpoint.
- `action: "service_area"` searches to `limit` on `metric` (`travel_time_s` or
  `distance_m`). Set `direction: "inbound"` to search toward the origin.
- `action: "hierarchy"` records the contraction hierarchy query of the route
  endpoint: the forward and backward searches in the order that they run.

`max_edges` stops the search (default 50,000, maximum 500,000). `meta.truncated`
tells if the search stopped early.

### Vector tiles

`GET /v1/tiles/{z}/{x}/{y}.mvt?profile_id=...` serves the edges that the
profile can use as Mapbox Vector Tiles (layer `network`). Zoom 14 and more
contains all edges. Lower zoom levels contain fewer road classes.

```bash
curl -o tile.mvt 'http://127.0.0.1:8080/v1/tiles/15/16981/10640.mvt?profile_id=car_research_v1'
```

### Waypoints and trips

`north_nl_waypoints.json` visits ordered locations. A `break` location starts a
new leg. A `through` location stays in its leg and prevents a U-turn.

`POST /v1/trip` puts up to 2,000 stops in the best order:

```bash
curl -X POST 'http://127.0.0.1:8080/v1/trip?format=geojson' \
  -H 'Content-Type: application/json' \
  --data '{"request": '"$(cat examples/requests/trip_groningen.json)"'}'
```

See [trip optimization](../../docs/guides/trip-optimization.md).

## Make a route batch GeoPackage

Use `analyze route-batch` to write many routes to one GeoPackage. The
`--requests` file has the form
`{"requests": [{"profile_id": "...", "request": {...}}]}`. Each `request` is a
`/v1/route` request.

```bash
cargo run --release -p netweevil-cli -- analyze route-batch \
  --dataset groningen_2026_05 \
  --profile examples/profiles/car_research_v1.yml \
  --requests routes.json \
  --out .netweevil/runs/groningen_routes.gpkg
```

The GeoPackage contains the `routes` layer and the tables `route_segments`,
`route_breakdown_road_class`, `route_breakdown_surface`, `route_violations`
and `route_failures`.

## Make the province boundary file

`groningen_province_admin_bounds.geojson` contains the Groningen province
boundary (OSM relation `47826`, `NL-GR`, admin level 4). To make it again from
a Groningen extract:

```bash
osmium getid -r datasets/groningen-latest.osm.pbf r47826 \
  -o groningen_province_admin_bounds.osm.pbf
osmium export groningen_province_admin_bounds.osm.pbf \
  -o examples/api/groningen_province_admin_bounds.geojson -f geojson
```
