# Netan API Examples

## Rebuild Into The Current Dataset Format

The current API path expects datasets imported with the new uncompressed bundle layout:

- topology: `.netan/bundles/topology/*.bin`
- edge names: `.netan/bundles/names/*.bin`
- compiled profile metrics: `.netan/bundles/metrics/*.bin`

If you have older cached datasets from the previous format, remove them first and re-import.

Remove the old cache state for a clean rebuild:

```bash
rm -rf .netan/datasets .netan/bundles/topology .netan/bundles/names .netan/bundles/metrics .netan/compiled_profiles
mkdir -p .netan/bundles/topology .netan/bundles/names .netan/bundles/metrics .netan/datasets .netan/compiled_profiles
```

Import the dataset again in the new format:

```bash
cargo run -p netan-cli -- dataset import datasets/ile-de-france-latest.osm.pbf --name ile_de_france_2026_03
```

Compile every profile you want hot before starting the API:

```bash
cargo run -p netan-cli -- profile compile --dataset ile_de_france_2026_03 --profile examples/profiles/car_research_v1.yml
cargo run -p netan-cli -- profile compile --dataset ile_de_france_2026_03 --profile examples/profiles/pedestrian_research_v1.yml
```

## Start The API

Start the API with one default profile and preload any additional selectable profiles:

```bash
cargo run -p netan-cli -- api serve \
  --dataset ile_de_france_2026_03 \
  --default-profile examples/profiles/car_research_v1.yml \
  --profile examples/profiles/pedestrian_research_v1.yml \
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
