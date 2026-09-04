<p align="center">
  <img src="qgis_plugin/netweevil_qgis/logo.svg" alt="netweevil logo" width="128" height="128">
</p>

# netweevil

`netweevil` is a Rust-first local network analysis system for OSM routing research, reproducible analysis runs, GTFS transit experiments, agent-based traffic simulation, and QGIS workflows.

The production surface is the `netweevil` CLI plus a preloadable HTTP API. The QGIS plugin talks to that API and loads spatial outputs back into a project.

## What Is In This Repo

- `crates/core`: graph primitives, topology bundles, metrics, and acceleration data types
- `crates/ingest`: OSM PBF, Overture, and mapped GeoPackage import; topology build, turn restrictions, road classification, and acceleration preprocessing
- `crates/profile`: profile schema, validation, compilation, turn costs, and edge costs
- `crates/query`: routing, route batches, OD, matrix, service areas, accessibility, alternatives, snapping, and request/result models
- `crates/persist`: `.netweevil/` state layout, manifests, binary bundle IO, and cache reads
- `crates/report`: manifests, report rendering, and local exports
- `crates/transit`: GTFS import and street-access (walk/bicycle/car) + transit routing
- `crates/simulate`: agent-based traffic simulation with congestion and temporal outputs
- `crates/api`: Axum HTTP API for loaded datasets, profiles, transit feeds, and simulations
- `crates/cli`: the `netweevil` binary
- `qgis_plugin/netweevil_qgis`: QGIS 3/4 plugin
- `examples/profiles`: profile examples
- `examples/ingest`: GeoPackage mapping examples
- `examples/requests`: CLI request fixtures
- `examples/scenarios`: runtime feature-state examples
- `examples/temporal`: holiday and continuous-overlay examples
- `examples/transit`: stop-to-network binding examples
- `examples/api`: API payload examples
- `examples/perf`: performance route corpora
- `scripts`: QGIS packaging and performance scripts
- `datasets`: local development OSM extracts and GTFS archives

## Requirements

- Rust toolchain with edition 2024 support. The crates declare `rust-version = 1.96.1`; the Docker build uses Rust `1.96.1`.
- Docker and Docker Compose for the containerized API path.
- QGIS `3.28` through `4.99` for the plugin.
- `curl` for API smoke tests.
- Optional: `osmium` if you want to prefilter your own OSM extracts.

## Local Quickstart

Download the Groningen sample extract first — datasets are not checked in;
[`datasets/README.md`](datasets/README.md) has the download and filter
commands. Then run:

```bash
cargo fmt --all
cargo check
cargo test

cargo run -p netweevil-cli -- dataset import \
  datasets/groningen-260508-routing.osm.pbf \
  --name groningen_2026_05

# Or import Overture Maps transportation segments instead of OSM
# (a .parquet/.geoparquet file or a directory of them; format is detected
# from the path, or force it with --format overture):
# cargo run -p netweevil-cli -- dataset import \
#   datasets/overture-groningen-segments.parquet \
#   --name groningen_overture

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

## 3D GeoPackage And Temporal Analysis

GeoPackage ingest expects inputs already transformed to WGS84
longitude/latitude with Z in metres. NetWeevil does not directly transform raw
Hong Kong EPSG:2326+5738 data or read FileGDB; perform that conversion in the
analysis preprocessing environment first. Then import outdoor and indoor
sources together and audit the retained fields and segment geometry:

```bash
scripts/prepare_hong_kong_pedestrian.sh \
  /path/to/hong-kong-analysis

cargo run -p netweevil-cli -- dataset import \
  /path/to/hong-kong-analysis/prepared/3D_Pedestrian_Network.gpkg \
  /path/to/hong-kong-analysis/prepared/3D_Indoor_Network.gpkg \
  --name hk_pedestrian_3d \
  --format gpkg \
  --mapping examples/ingest/hong_kong_pedestrian_mapping.yml

cargo run -p netweevil-cli -- dataset audit \
  --dataset hk_pedestrian_3d \
  --against \
    /path/to/hong-kong-analysis/prepared/3D_Pedestrian_Network.gpkg \
    /path/to/hong-kong-analysis/prepared/3D_Indoor_Network.gpkg \
  --mapping examples/ingest/hong_kong_pedestrian_mapping.yml

cargo run -p netweevil-cli -- profile compile \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_multilayer.yml
```

The preparation script validates GDAL and both source files, preserves the
GeoPackage basenames, layers, fields, and HKPD Z values, transforms horizontal
coordinates to WGS84, and writes source hashes plus transformation details to
`prepared/netweevil-preprocessing-manifest.txt`. Use `--output-dir` to choose a
different destination. Platform-level GTFS synthesis and real stop bindings
remain project-specific input generation; the routing, transfer-table, and
analysis engine workflow is otherwise runnable from this repository.

Run time-dependent, constrained/Pareto, criticality, and failure-scenario
examples after adapting their clearly marked illustrative Hong Kong coordinates
and source feature ids:

```bash
cargo run -p netweevil-cli -- analyze route \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_multilayer.yml \
  --request examples/requests/hong_kong_example_temporal_route.json \
  --out .netweevil/runs/hk-example-temporal-route.geojson

cargo run -p netweevil-cli -- analyze betweenness \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_multilayer.yml \
  --request examples/requests/hong_kong_example_betweenness.json \
  --out .netweevil/runs/hk-example-betweenness.gpkg

cargo run -p netweevil-cli -- analyze scenario-batch \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_multilayer.yml \
  --request examples/requests/hong_kong_example_scenario_batch.yml \
  --out .netweevil/runs/hk-example-scenario-batch.json

cargo run -p netweevil-cli -- analyze service-area-sequence \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_multilayer.yml \
  --request examples/requests/hong_kong_example_service_area_sequence.json \
  --out .netweevil/runs/hk-example-ten-am-frames.geojson
```

See [`docs/multilayer-temporal-routing.md`](docs/multilayer-temporal-routing.md)
for the mapping roles, preprocessing contract, opening-hours format, profile
components, temporal rules, overlays, Pareto constraints, portal QA, transit
bindings, and network-transfer workflow.

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

Import the dataset into the persistent volume (works the same for an
Overture parquet file or directory under `datasets/`):

```bash
docker compose run --rm api dataset import \
  datasets/groningen-260508-routing.osm.pbf \
  --name osm
```

Alternatively, provision the data at build time. The default and
recommended flow is the runtime import above — it keeps images small and
data out of layers — but a self-contained image is useful for
reproducible deployments. Pass one of:

```bash
# Bake an OSM extract fetched at build time
NETWEEVIL_FETCH_OSM_URL=https://download.geofabrik.de/europe/netherlands/groningen-latest.osm.pbf \
docker compose build

# Bake Overture transportation segments for a bounding box
# (west,south,east,north; fetched with the official overturemaps CLI)
NETWEEVIL_FETCH_OVERTURE_BBOX=6.4,53.1,6.7,53.3 \
docker compose build
```

The build fetches the data in an intermediate stage, runs
`dataset import --name $NETWEEVIL_DATASET` during the build, and copies
only the imported `.netweevil` bundles into the final image — raw
downloads never land in image layers. On the first `up`, Docker seeds the
empty `netweevil-state` volume from the baked state, so the API serves
immediately without a separate import step.

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

- `dataset import|audit|list`
- `transit import|bindings apply|transfers build|list`
- `profile validate|compile`
- `analyze route|route-batch|od|matrix|accessibility|service-area|service-area-sequence|betweenness|scenario-batch|transit-route|transit-batch`
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

Street-analysis commands also accept `--departure-time`, `--scenario`,
`--holiday-calendar`, and repeatable `--overlay` overrides. Scalars replace
only the matching request-file field; CLI overlays append to any overlays
already in the document. See the multilayer runbook for matrix and
service-area-sequence semantics.

## Transit

Import an existing GTFS archive:

```bash
cargo run -p netweevil-cli -- transit import \
  datasets/gtfs-openov-nl.zip \
  --name openov_nl_2026_05 \
  --service-start 2026-05-09 \
  --service-days 7
```

An exact stop-to-network binding table can be applied during import with
`--stop-bindings bindings.json`, or replaced later:

```bash
cargo run -p netweevil-cli -- transit bindings apply \
  --feed hk_example_mtr \
  examples/transit/hong_kong_example_stop_bindings.json

cargo run -p netweevil-cli -- transit transfers build \
  --feed hk_example_mtr \
  --dataset hk_pedestrian_3d \
  --profile examples/profiles/pedestrian_step_free_multilayer.yml \
  --max-transfer-distance-m 500
```

For a transit request with `modes.street_access: "network"`, pass both
`--street-dataset` and `--street-profile` to `analyze transit-route` or
`analyze transit-batch`. The CLI network estimator currently supports a foot
profile with walk-only access and egress. See
[`docs/transit-fusion.md`](docs/transit-fusion.md).

The transfer build routes directed stop pairs through the loaded street/indoor
graph, registers the resulting table on the feed, and records the dataset,
profile hash, and binding fingerprint used to create it. Select it in a transit
request with `modes.transfer_profile_id: "pedestrian_step_free_multilayer"`. See
[`docs/transit-fusion.md`](docs/transit-fusion.md) for binding formats and the
full result contract.

Run a pedestrian+transit route:

```bash
cargo run -p netweevil-cli -- analyze transit-route \
  --feed openov_nl_2026_05 \
  --request examples/requests/transit_openov_groningen.json \
  --out .netweevil/runs/example-transit-route.json
```

First and last miles are not limited to walking: `modes.access` and
`modes.egress` accept `walk`, `bicycle`, or `car`, each with its own speed
(`walk_speed_kph`, `bicycle_speed_kph`, `car_access_speed_kph`) and distance
limits (`max_access_distance_m`/`max_egress_distance_m` for walking,
`max_bicycle_*` and `max_car_*` variants for the other modes). Transit service
areas accept the same `modes` block, so cycling+transit or car+transit
catchments work out of the box (see
`examples/requests/transit_service_area_car_access.json`).

By default the access and egress mode lists must match. To plan an asymmetric
first/last mile, for example cycle to the station but walk from the final
stop, set `modes.mixed_access_egress: true` (see
`examples/requests/transit_bike_access_groningen.json`).

Access and egress legs are priced by straight-line distance over the mode
speed by default. Set `modes.street_access: "network"` to price them with
real street-network travel times instead; the API resolves a loaded street
profile per access mode (walk requires a foot profile). Network mode is strict:
a stop candidate is omitted when no matching engine/path exists, so a
disconnected component, paid-area barrier, or missing profile cannot become a
straight-line teleport. Use `street_access: "straight_line"` explicitly when
geometric access is desired. Transit service areas accept the same option.

For API requests, `transfer_profile_id` may be supplied either beside
`feed_id` or as `request.modes.transfer_profile_id`. `/v1/service` reports each
feed's bound-stop count and available transfer profile IDs.

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
- `POST /v1/accessibility`
- `POST /v1/service-area`
- `POST /v1/service-area-sequence`
- `POST /v1/betweenness`
- `POST /v1/scenario-batch`
- `POST /v1/transit-route`
- `POST /v1/transit-service-area`

For `route`, `od`, `matrix`, `service-area`, `service-area-sequence`,
`betweenness`, and `scenario-batch`, add `?format=geojson` to request GeoJSON
instead of JSON:

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

## Importing Overture Maps Data

`dataset import` also accepts Overture Maps transportation-theme
GeoParquet — a single `.parquet`/`.geoparquet` file or a directory of them
(only segment files are read; connector files in the same directory are
skipped automatically). The format is detected from the path; use
`--format overture` or `--format osm-pbf` to force it.

The importer consumes road-subtype segments and maps:

- `class`/`subclass` to the highway classification (including `link` ramps)
- `connectors` to graph nodes — segments are split into
  connector-to-connector chunks, so linearly scoped rules apply per chunk
- `access_restrictions` to per-mode, per-direction access (heading-scoped
  denies become oneways); time- or vehicle-conditional rules are skipped
- `speed_limits` to per-direction posted limits (mph converted to km/h)
- `road_surface` and `road_flags` (under-construction and abandoned
  segments are dropped)
- `prohibited_transitions` to turn restrictions
- `lanes` to per-direction lane counts on releases that still carry the
  column (it was removed from the GA schema)

Posted speed limits combine with profile speeds per the profile's
`speeds.posted_limits` setting — `prefer` (the posted limit takes
precedence over the profile speed wherever the data carries one), `cap`
(the default: travel is never assumed faster than the posted limit, but
lower profile speeds win), or `ignore` (profile speeds only). Lane counts
feed the traffic simulation. Both also work for OSM sources via the
`maxspeed` and `lanes` tags. `datasets/README.md` has download commands.

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
