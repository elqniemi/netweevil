# Examples

This folder contains profiles, requests and other input files for NetWeevil.
Most examples use the Groningen dataset `groningen_2026_05`. Follow
[`datasets/README.md`](../datasets/README.md) to download the source files.
Follow the [Quickstart](../README.md#quickstart) to import the dataset.

Run all commands from the repository root. In a source checkout, use
`cargo run --release -p netweevil-cli --` in place of `netweevil`.

## Folders

| Folder | Contents | Documentation |
|---|---|---|
| [`profiles/`](profiles/) | Routing profiles for car, bicycle, pedestrian, truck, bus, taxi, motorcycle, moped and emergency vehicles | [Profiles](../docs/concepts/profiles.md) |
| [`requests/`](requests/) | CLI requests: routes, OD pairs, matrices, service areas, transit, trips and a simulation scenario | [Routing](../docs/guides/routing.md) |
| [`api/`](api/) | HTTP API request bodies | [`api/README.md`](api/README.md) |
| [`hong-kong/`](hong-kong/) | API requests for the Hong Kong 3D walking and transit tutorial | [`hong-kong/README.md`](hong-kong/README.md) |
| [`ingest/`](ingest/) | GeoPackage mapping files for `dataset import --mapping` | [Datasets](../docs/guides/datasets.md) |
| [`scenarios/`](scenarios/) | Feature-state scenarios for `--scenario` (closures and changed facilities) | [Multilayer temporal routing](../docs/guides/multilayer-temporal-routing.md) |
| [`temporal/`](temporal/) | A holiday calendar and a time-dependent overlay CSV | [Multilayer temporal routing](../docs/guides/multilayer-temporal-routing.md) |
| [`traffic/`](traffic/) | A synthetic historical speed set for Groningen | [Traffic](../docs/guides/traffic.md) |
| [`transit/`](transit/) | Stop binding tables for `transit bindings apply` and `--stop-bindings` | [Transit](../docs/guides/transit.md) |
| [`experiments/`](experiments/) | A study file for `experiment run` | [Reports and experiments](../docs/guides/reports-and-experiments.md) |
| [`perf/`](perf/) | Route corpora for `netweevil bench` | [`perf/README.md`](perf/README.md) |

## Run the examples

### Route

```bash
netweevil profile compile --dataset groningen_2026_05 \
  --profile examples/profiles/car_research_v1.yml
netweevil analyze route --dataset groningen_2026_05 \
  --profile examples/profiles/car_research_v1.yml \
  --request examples/requests/route.json \
  --out .netweevil/runs/route.geojson
```

Use the same command with `route_alternatives.json`,
`route_degraded_fallback.json` or `route_disconnected_strict.json` to see
alternatives and the connectivity options.

### OD pairs and matrices

```bash
netweevil analyze od --dataset groningen_2026_05 \
  --profile examples/profiles/car_research_v1.yml \
  --pairs examples/requests/od_pairs.csv
netweevil analyze matrix --dataset groningen_2026_05 \
  --profile examples/profiles/car_research_v1.yml \
  --origins examples/requests/matrix_origins.csv \
  --destinations examples/requests/matrix_destinations.csv
```

The CSV and JSON versions of each file contain the same points.

### Service areas

```bash
netweevil analyze service-area --dataset groningen_2026_05 \
  --profile examples/profiles/car_research_v1.yml \
  --request examples/requests/service_area.json
```

`service_area_inbound.json` and `service_area_multi_origin.json` show inbound
areas and areas from more than one origin.

### Trip

```bash
netweevil analyze trip --dataset groningen_2026_05 \
  --profile examples/profiles/car_research_v1.yml \
  --request examples/requests/trip_groningen.json \
  --out .netweevil/runs/trip.geojson
```

### Transit

The transit requests use the OpenOV feed. Import the feed with
`netweevil transit import`, then run `analyze transit-route` with
`transit_openov_groningen.json`. The [transit guide](../docs/guides/transit.md)
gives the full procedure.

### Simulation

```bash
netweevil simulate run examples/requests/simulation_scenario.yml \
  --dataset groningen_2026_05 \
  --profile examples/profiles/car_research_v1.yml \
  --profile examples/profiles/cycling_north_nl_2026_05.yml \
  --temporal-geojson
```

### Traffic

```bash
netweevil traffic import --dataset groningen_2026_05 \
  examples/traffic/groningen_synthetic.yml
```

### Experiment

```bash
netweevil experiment run examples/experiments/baseline_sweep.yml
```

Paths in the study file are relative to the study file.

## Hong Kong example files

The files with `hong_kong_example` in their name use the shape of the Hong
Kong data. Their coordinates and feature IDs are for illustration. They do not
run without changes. The [Hong Kong tutorial](../docs/guides/hong-kong-tutorial.md)
and [`hong-kong/`](hong-kong/) contain requests that run on the real data.
