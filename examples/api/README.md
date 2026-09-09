# Netweevil API Examples

`north_nl_match.json` exercises `POST /v1/match` with a timestamped GPS trace.
See [trace matching](../../docs/trace-matching.md) for scoring, gaps and limits.

`route_profile_overrides.json` demonstrates request-defined street routing on
the North Netherlands dataset. Send it to `/v1/route`; the first request compiles
the effective profile and subsequent identical profiles reuse the cached engine.
See [request-defined profiles](../../docs/request-profiles.md) for merge semantics
and inline profiles.

## Groningen Batch Examples

These Groningen examples target:

- dataset: `groningen_2026_03`
- profile: `examples/profiles/car_research_v3.yml`
- profile id: `car_research_v3`

Files added for this dataset:

- `examples/api/groningen_province_admin_bounds.geojson`
- `examples/api/groningen_examples_meta.json`
- `examples/api/groningen_od_1000.json`
- `examples/api/groningen_matrix_50x50.json`
- `examples/api/groningen_routes_every_10th_from_od.json`
- `examples/api/run_groningen_routes_every_10th_to_gpkg.py`

The exact province boundary comes from the Groningen administrative relation in OSM:

- relation id: `47826`
- ISO 3166-2: `NL-GR`
- admin level: `4`

Extract it from the local Groningen PBF with `osmium`:

```bash
osmium tags-filter datasets/groningen-260317.osm.pbf r/ISO3166-2=NL-GR -f opl
osmium getid -r datasets/groningen-260317.osm.pbf r47826 \
  -o examples/api/groningen_province_admin_bounds.osm.pbf
osmium export examples/api/groningen_province_admin_bounds.osm.pbf \
  -o examples/api/groningen_province_admin_bounds.geojson \
  -f geojson
```

The generated request points in the Groningen examples are sampled deterministically from drivable highway geometry whose coordinates fall inside that exact province polygon, not from a loose bbox.

## Rebuild Into The Current Dataset Format

The current API path expects datasets imported with the new uncompressed bundle layout:

- topology: `.netweevil/bundles/topology/*.bin`
- edge names: `.netweevil/bundles/names/*.bin`
- acceleration: `.netweevil/bundles/acceleration/*.bin`
- compiled profile metrics: `.netweevil/bundles/metrics/*.bin`

If you have older cached datasets from the previous format, remove them first and re-import.

Remove the old cache state for a clean rebuild:

```bash
rm -rf .netweevil/datasets .netweevil/bundles/topology .netweevil/bundles/names .netweevil/bundles/acceleration .netweevil/bundles/metrics .netweevil/compiled_profiles
mkdir -p .netweevil/bundles/topology .netweevil/bundles/names .netweevil/bundles/acceleration .netweevil/bundles/metrics .netweevil/datasets .netweevil/compiled_profiles
```

Import the dataset again in the new format:

```bash
cargo run -p netweevil-cli -- dataset import datasets/ile-de-france-latest.osm.pbf --name ile_de_france_2026_03
```

Compile every profile you want hot before starting the API:

```bash
cargo run -p netweevil-cli -- profile compile --dataset ile_de_france_2026_03 --profile examples/profiles/car_research_v1.yml
cargo run -p netweevil-cli -- profile compile --dataset ile_de_france_2026_03 --profile examples/profiles/pedestrian_research_v1.yml
```

For the Groningen batch examples, compile the v3 car profile against the Groningen dataset:

```bash
cargo run -p netweevil-cli -- profile compile --dataset groningen_2026_03 --profile examples/profiles/car_research_v3.yml
```

## Start The API

Start the API with one default profile and preload any additional selectable profiles:

```bash
cargo run -p netweevil-cli -- api serve \
  --dataset ile_de_france_2026_03 \
  --default-profile examples/profiles/car_research_v1.yml \
  --profile examples/profiles/pedestrian_research_v1.yml \
  --bind 127.0.0.1:8080
```

Start the API for the Groningen examples:

```bash
cargo run -p netweevil-cli -- api serve \
  --dataset groningen_2026_03 \
  --default-profile examples/profiles/car_research_v3.yml \
  --bind 127.0.0.1:8080
```

Inspect the loaded dataset, available profiles, and supported options:

```bash
curl http://127.0.0.1:8080/v1/service
curl http://127.0.0.1:8080/v1/profiles
```

## Fast Path

For the warmest and cheapest route path:

- preload profiles at startup with repeated `--profile` flags
- send normal JSON, not `format=geojson`
- keep `returns.geometry: none`
- keep `returns.segment_rows: false`
- leave breakdown arrays empty unless you need them

That path keeps geometry and edge names cold, reuses the prepared in-memory engine, reuses scratch space, and avoids extra response shaping.
For these summary-only requests, the JSON response also omits `node_path` and `edge_path` so the hot API path does not pay to serialize path ID arrays by default.

Minimal summary-only route request:

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

Run a car route from Bastille to La Defense:

```bash
curl -X POST http://127.0.0.1:8080/v1/route \
  -H 'content-type: application/json' \
  --data @examples/api/ile_de_france_route_car.json
```

Run a pedestrian route from Jardin du Luxembourg to Notre-Dame:

```bash
curl -X POST http://127.0.0.1:8080/v1/route \
  -H 'content-type: application/json' \
  --data @examples/api/ile_de_france_route_pedestrian.json
```

Run an OD batch:

```bash
curl -X POST http://127.0.0.1:8080/v1/od \
  -H 'content-type: application/json' \
  --data @examples/api/ile_de_france_od.json
```

Run a small matrix:

```bash
curl -X POST http://127.0.0.1:8080/v1/matrix \
  -H 'content-type: application/json' \
  --data @examples/api/ile_de_france_matrix.json
```

If you want route GeoJSON or per-segment names, request them explicitly. Those options intentionally pull in colder payloads and do more work per request.

## Groningen API Runs

Run the 1000-pair Groningen OD list:

```bash
curl -X POST http://127.0.0.1:8080/v1/od \
  -H 'content-type: application/json' \
  --data @examples/api/groningen_od_1000.json
```

Run the 50x50 Groningen matrix:

```bash
curl -X POST http://127.0.0.1:8080/v1/matrix \
  -H 'content-type: application/json' \
  --data @examples/api/groningen_matrix_50x50.json
```

`examples/api/groningen_routes_every_10th_from_od.json` is a manifest of 100 single-route request bodies, one for every 10th OD pair from the 1000-pair list. These requests now ask for:

- `geometry: full`
- `segment_rows: true`
- road-type breakdowns for `time_s` and `distance_m`
- surface breakdowns for `time_s` and `distance_m`
- `penalty_breakdown: true`
- `explain_cost_derivation: true`

Parallel API execution example:

```bash
python3 - <<'PY'
import concurrent.futures
import json
import pathlib
import urllib.request

payload = json.loads(
    pathlib.Path("examples/api/groningen_routes_every_10th_from_od.json").read_text()
)
requests = payload["requests"]
url = "http://127.0.0.1:8080/v1/route"

def post(body):
    req = urllib.request.Request(
        url,
        data=json.dumps(body).encode("utf-8"),
        headers={"content-type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req) as resp:
        return resp.status

with concurrent.futures.ThreadPoolExecutor(max_workers=16) as pool:
    statuses = list(pool.map(post, requests))

print(f"posted {len(statuses)} routes")
print(f"status range: {min(statuses)}..{max(statuses)}")
PY
```

If you want one QGIS-ready GeoPackage with assigned route geometry plus per-route detail tables, use the dedicated route-batch CLI path instead of posting 100 single route requests:

```bash
cargo run --release -p netweevil-cli -- analyze route-batch \
  --dataset groningen_2026_03 \
  --profile examples/profiles/car_research_v3.yml \
  --requests examples/api/groningen_routes_every_10th_from_od.json \
  --out .netweevil/runs/groningen_routes_every_10th.gpkg
```

That writes one GeoPackage containing:

- `routes`: route line geometry layer
- `route_segments`: non-spatial segment rows
- `route_breakdown_road_class`: non-spatial road-class breakdown table
- `route_breakdown_surface`: non-spatial surface breakdown table
- `route_violations`: non-spatial violation table
- `route_failures`: non-spatial failures table

The older helper script is still available if you explicitly want one GeoPackage per route:

```bash
python3 examples/api/run_groningen_routes_every_10th_to_gpkg.py
```

## Inspect routing locations

`POST /v1/locate` returns profile-accessible snap candidates for each input point.
Use `direction: "origin"` for outgoing routes or `"destination"` for incoming
routes. The response includes snapped coordinates, directed edge IDs, position
along the edge, component IDs, and snap distance. Candidates use the same
spatial index, elevation window, and attribute filters as route requests.
A point with no candidate fails the request with the route snapping diagnostic.

```bash
curl -X POST http://127.0.0.1:8080/v1/locate \
  -H 'Content-Type: application/json' \
  --data @examples/api/locate.json
```

## Routes with intermediate locations

`POST /v1/waypoints` visits ordered locations and returns route legs and their
combined distance, travel time and generalized cost. A `break` location splits
legs and resets turn history. A `through` location stays inside its leg,
preserves turn history and prohibits an immediate U-turn at the location.

```bash
curl -X POST 'http://127.0.0.1:8080/v1/waypoints?format=geojson' \
  -H 'Content-Type: application/json' \
  --data @examples/api/north_nl_waypoints.json
```

Set `optimize_order: true` to minimize directed static costs while keeping the
first and last locations fixed. Optimization supports at most 16 intermediate
`break` locations; the returned `waypoint_order` contains their original input
indexes. Ordered routing supports up to 128 locations. Temporal, scenario and
component-constrained requests are rejected. Requests use a loaded profile and
the same elevation and attribute snapping options as street routes.

## Directed snapping

`locate_constraints.json` sends a bearing and curb-side requirement to
`POST /v1/locate`. The same `snap.point_constraints` map works in routes,
directions, point sets for matrices, waypoint requests and GPS trace matching.
Keys refer to input point IDs. See [bearing and road-side constraints](../../docs/snap-constraints.md)
for angle validation, driving-side choices and directed intersection behavior.
