<p align="center">
  <img src="qgis_plugin/netweevil_qgis/logo.svg" alt="netweevil logo" width="128" height="128">
</p>

# netweevil

`netweevil` is a Rust-first local network analysis system for OSM routing research, reproducible analysis runs, GTFS transit experiments, agent-based traffic simulation, and QGIS workflows.

The production surface is the `netweevil` CLI plus a preloadable HTTP API. The QGIS plugin talks to that API and loads spatial outputs back into a project.

## What Is In This Repo

- `crates/core`: graph primitives, topology bundles, metrics, and acceleration data types
- `crates/ingest`: OSM PBF import, topology build, turn restrictions, road classification, and acceleration preprocessing
- `crates/profile`: profile schema, validation, compilation, turn costs, and edge costs
- `crates/query`: routing, route batches, OD, matrix, service areas, accessibility, alternatives, snapping, and request/result models
- `crates/persist`: `.netweevil/` state layout, manifests, binary bundle IO, and cache reads
- `crates/report`: manifests, report rendering, and local exports
- `crates/transit`: GTFS import and pedestrian+transit routing
- `crates/simulate`: agent-based traffic simulation with congestion and temporal outputs
- `crates/api`: Axum HTTP API for loaded datasets, profiles, transit feeds, and simulations
- `crates/cli`: the `netweevil` binary
- `qgis_plugin/netweevil_qgis`: QGIS 3/4 plugin
- `examples/profiles`: profile examples
- `examples/requests`: CLI request fixtures
- `examples/api`: API payload examples
- `examples/perf`: performance route corpora
- `scripts`: QGIS packaging and performance scripts
- `datasets`: local development OSM extracts and GTFS archives

## Requirements

- Rust toolchain with edition 2024 support. The crates declare `rust-version = 1.85`; the Docker build uses Rust `1.88`.
- Docker and Docker Compose for the containerized API path.
- QGIS `3.28` through `4.99` for the plugin.
- `curl` for API smoke tests.
- Optional: `osmium` if you want to prefilter your own OSM extracts.

## Local Quickstart

Use the checked-in Groningen routing extract for a complete local setup:

```bash
cargo fmt --all
cargo check
cargo test

cargo run -p netweevil-cli -- dataset import \
  datasets/groningen-260508-routing.osm.pbf \
  --name groningen_2026_05

cargo run -p netweevil-cli -- profile validate \
  examples/profiles/car_research_v1.yml

cargo run -p netweevil-cli -- profile compile \
  --dataset groningen_2026_05 \
  --profile examples/profiles/car_research_v1.yml

cargo run -p netweevil-cli -- analyze route \
  --dataset groningen_2026_05 \
  --profile examples/profiles/car_research_v1.yml \
  --request examples/requests/route.json \
  --out .netweevil/runs/example-route.geojson
```

Start the API:

```bash
cargo run -p netweevil-cli -- api serve \
  --dataset groningen_2026_05 \
  --default-profile examples/profiles/car_research_v1.yml \
  --profile examples/profiles/pedestrian_research_v1.yml \
  --bind 127.0.0.1:8080
```

Smoke test it:

```bash
curl http://127.0.0.1:8080/healthz
curl http://127.0.0.1:8080/readyz
curl http://127.0.0.1:8080/v1/service
curl http://127.0.0.1:8080/v1/profiles
```

## Docker Production-Style Setup

The container runs the `netweevil` binary from `/data` and stores all state in `/data/.netweevil`, backed by the `netweevil-state` named volume.

Build the image:

```bash
docker compose build
```

Import the dataset into the persistent volume:

```bash
docker compose run --rm api dataset import \
  datasets/groningen-260508-routing.osm.pbf \
  --name osm
```

Serve the API:

```bash
docker compose up -d api
curl http://127.0.0.1:8080/healthz
```

The Compose file reads these environment variables:

- `NETWEEVIL_DATASET`: dataset id to serve, default `osm`
- `NETWEEVIL_PROFILE`: default profile path, default `examples/profiles/car_research_v1.yml`
- `NETWEEVIL_PORT`: host port, default `8080`

Example with an explicit profile and port:

```bash
NETWEEVIL_DATASET=osm \
NETWEEVIL_PROFILE=examples/profiles/car_research_v1.yml \
NETWEEVIL_PORT=18080 \
docker compose up -d api
```

## State Layout

All CLI and API state is under `.netweevil/` in the current workspace:

- `.netweevil/datasets`: imported dataset manifests
- `.netweevil/transit_feeds`: imported GTFS feed manifests
- `.netweevil/compiled_profiles`: compiled profile manifests
- `.netweevil/bundles/topology`: topology bundles
- `.netweevil/bundles/names`: cold edge-name bundles
- `.netweevil/bundles/acceleration`: dataset acceleration bundles
- `.netweevil/bundles/metrics`: compiled profile metric bundles
- `.netweevil/bundles/transit`: transit bundles
- `.netweevil/runs`: route, OD, matrix, service-area, experiment, and simulation outputs
- `.netweevil/reports`: rendered reports

Current imports write `.bin` bundles. If you have old gzip topology bundles, re-import the source `.osm.pbf`; the current reader rejects legacy gzip topology files.

## CLI Commands

Inspect the live CLI surface:

```bash
cargo run -p netweevil-cli -- --help
cargo run -p netweevil-cli -- analyze --help
cargo run -p netweevil-cli -- api serve --help
```

Top-level commands:

- `dataset import|list`
- `transit import|list`
- `profile validate|compile`
- `analyze route|route-batch|od|matrix|accessibility|service-area|transit-route|transit-batch`
- `experiment run`
- `simulate validate|run`
- `report render`
- `cache list`
- `api serve`

Useful local examples:

```bash
cargo run -p netweevil-cli -- cache list

cargo run -p netweevil-cli -- analyze od \
  --dataset groningen_2026_05 \
  --profile examples/profiles/car_research_v1.yml \
  --pairs examples/requests/od_pairs.csv \
  --out .netweevil/runs/example-od.gpkg

cargo run -p netweevil-cli -- analyze matrix \
  --dataset groningen_2026_05 \
  --profile examples/profiles/car_research_v1.yml \
  --origins examples/requests/matrix_origins.csv \
  --destinations examples/requests/matrix_destinations.csv \
  --out .netweevil/runs/example-matrix.gpkg

cargo run -p netweevil-cli -- analyze service-area \
  --dataset groningen_2026_05 \
  --profile examples/profiles/pedestrian_research_v1.yml \
  --request examples/requests/service_area.json \
  --out .netweevil/runs/example-service-area.geojson
```

## Transit

Import an existing GTFS archive:

```bash
cargo run -p netweevil-cli -- transit import \
  datasets/gtfs-openov-nl.zip \
  --name openov_nl_2026_05 \
  --service-start 2026-05-09 \
  --service-days 7
```

Run a pedestrian+transit route:

```bash
cargo run -p netweevil-cli -- analyze transit-route \
  --feed openov_nl_2026_05 \
  --request examples/requests/transit_openov_groningen.json \
  --out .netweevil/runs/example-transit-route.json
```

Preload a transit feed into the API:

```bash
cargo run -p netweevil-cli -- api serve \
  --dataset groningen_2026_05 \
  --default-profile examples/profiles/car_research_v1.yml \
  --transit-feed openov_nl_2026_05 \
  --bind 127.0.0.1:8080
```

## Simulation

Validate a scenario:

```bash
cargo run -p netweevil-cli -- simulate validate \
  examples/requests/simulation_scenario.yml
```

Run it after compiling every profile referenced by the scenario fleets:

```bash
cargo run -p netweevil-cli -- simulate run \
  examples/requests/simulation_scenario.yml \
  --dataset groningen_2026_05 \
  --profile examples/profiles/car_research_v1.yml \
  --temporal-geojson \
  --out-dir .netweevil/runs
```

Simulation writes compact JSON results, busy-segment GeoJSON, congestion GeoJSON, optional temporal GeoJSON, and a run manifest.

The API also exposes simulation endpoints:

- `GET /v1/simulation`
- `POST /v1/simulation`
- `GET /v1/simulation/{simulation_id}`
- `DELETE /v1/simulation/{simulation_id}`
- `POST /v1/simulation/{simulation_id}/control`
- `GET /v1/simulation/{simulation_id}/frames`
- `GET /v1/simulation/{simulation_id}/edges`
- `GET /v1/simulation/{simulation_id}/temporal`
- `GET /v1/simulation/{simulation_id}/agents/{agent_id}`

## HTTP API

Start command:

```bash
cargo run -p netweevil-cli -- api serve \
  --dataset groningen_2026_05 \
  --default-profile examples/profiles/car_research_v1.yml \
  --profile examples/profiles/pedestrian_research_v1.yml \
  --transit-feed openov_nl_2026_05 \
  --bind 127.0.0.1:8080
```

Options:

- `--dataset <dataset_id>`: required imported dataset id
- `--default-profile <path>`: required default profile
- `--profile <path>`: optional, repeatable extra profiles
- `--transit-feed <feed_id>`: optional, repeatable imported GTFS feeds
- `--bind <host:port>`: default `127.0.0.1:8080`

Discovery endpoints:

- `GET /healthz`
- `GET /readyz`
- `GET /v1/service`
- `GET /v1/profiles`
- `GET /v1/profiles/{profile_id}`

Execution endpoints:

- `POST /v1/route`
- `POST /v1/od`
- `POST /v1/matrix`
- `POST /v1/service-area`
- `POST /v1/transit-route`
- `POST /v1/transit-service-area`

For `route`, `od`, and `matrix`, add `?format=geojson` to request GeoJSON instead of JSON:

```bash
curl -X POST 'http://127.0.0.1:8080/v1/route?format=geojson' \
  -H 'content-type: application/json' \
  --data @examples/api/ile_de_france_route_car.json
```

Route request shape:

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

Performance notes:

- JSON is the warmest response path.
- `?format=geojson` does extra response shaping.
- `segment_rows: true` loads the cold edge-name bundle.
- `geometry: none` minimizes geometry work.
- `geometry: full` and `geometry: segments` return route coordinates and cost more.

## QGIS Plugin

The plugin lives in `qgis_plugin/netweevil_qgis` and supports QGIS `3.28` through `4.99`.

Package it:

```bash
scripts/package_qgis_plugin.sh
```

The package is written to `dist/netweevil_qgis-<version>.zip`.

For development, symlink the plugin directory into your QGIS profile.

macOS:

```bash
mkdir -p "$HOME/Library/Application Support/QGIS/QGIS3/profiles/default/python/plugins"
ln -s "$(pwd)/qgis_plugin/netweevil_qgis" \
  "$HOME/Library/Application Support/QGIS/QGIS3/profiles/default/python/plugins/netweevil_qgis"
```

Linux:

```bash
mkdir -p "$HOME/.local/share/QGIS/QGIS3/profiles/default/python/plugins"
ln -s "$(pwd)/qgis_plugin/netweevil_qgis" \
  "$HOME/.local/share/QGIS/QGIS3/profiles/default/python/plugins/netweevil_qgis"
```

Windows PowerShell:

```powershell
New-Item -ItemType Directory -Force "$env:APPDATA\QGIS\QGIS3\profiles\default\python\plugins"
cmd /c mklink /D "$env:APPDATA\QGIS\QGIS3\profiles\default\python\plugins\netweevil_qgis" "C:\path\to\netweevil\qgis_plugin\netweevil_qgis"
```

Plugin runtime setup:

1. Start the API with the dataset and profiles you want to use.
2. Open QGIS and enable the `netweevil` plugin.
3. Set `Workspace root` to this repository.
4. Set `API base URL` to `http://127.0.0.1:8080`.
5. Press `Refresh Service`.
6. Run route, transit, OD, matrix, service-area, or simulation workflows.

The plugin loads JSON, GeoJSON, GPKG, and simulation outputs into grouped QGIS layers where supported.

## Reports And Experiments

Run an experiment study:

```bash
cargo run -p netweevil-cli -- experiment run \
  examples/experiments/baseline_sweep.yml
```

Render a run manifest:

```bash
cargo run -p netweevil-cli -- report render \
  .netweevil/runs/<run-manifest>.json \
  --out .netweevil/reports/example-report.html
```

If `--out` points to a directory instead of `.md` or `.html`, `report render` writes a small report bundle with `index.md`, `index.html`, the manifest copy, and the result file when available.

## Preparing Routing-Only OSM Extracts

The importer uses:

- routable `way` objects with `highway=*`
- ferry `way` objects with `route=ferry` or `ferry=*`
- turn-restriction `relation` objects with `type=restriction`
- traffic-signal `node` objects with `highway=traffic_signals`
- referenced nodes and relation members needed to keep topology valid

With `osmium`, keep those objects and their references:

```bash
cat > netweevil-routing-filters.txt <<'EOF'
w/highway
w/route=ferry
w/ferry
r/type=restriction
n/highway=traffic_signals
EOF

osmium tags-filter \
  --expressions=netweevil-routing-filters.txt \
  --remove-tags \
  source.osm.pbf \
  -o routing-only.osm.pbf \
  -O
```

Do not pass `-R` or `--omit-referenced`; netweevil needs referenced topology objects.

## Verification Checklist

For most changes:

```bash
cargo fmt --all
cargo check
cargo test
```

For API or QGIS-visible changes, also run:

```bash
cargo run -p netweevil-cli -- api serve \
  --dataset groningen_2026_05 \
  --default-profile examples/profiles/car_research_v1.yml \
  --bind 127.0.0.1:8080
```

Then smoke test:

```bash
curl http://127.0.0.1:8080/readyz
curl http://127.0.0.1:8080/v1/service
```

For Docker changes:

```bash
docker compose build
docker compose run --rm api --help
```

## License

The Rust workspace crates, CLI/API surfaces, scripts, examples, Docker
configuration, documentation, and repository tooling are licensed under either
MIT or Apache-2.0, at your option.

The QGIS plugin under `qgis_plugin/` is licensed under MIT and ships its license
text inside the installable plugin package.
