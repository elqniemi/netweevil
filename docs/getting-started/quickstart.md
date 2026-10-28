# Quickstart

This procedure imports the Groningen street network, compiles two profiles
and calculates a route. It uses the CLI first and then the HTTP API.

Before you start, build NetWeevil. See [Installation](installation.md).
Run all commands from the repository root. NetWeevil writes its state to
`.netweevil/` in the current directory.

!!! note
    In a source checkout, replace `netweevil` with
    `cargo run --release -p netweevil-cli --`.

## 1. Download a source extract

1.  Download the Groningen province extract from Geofabrik:

    ```bash
    curl -L -o datasets/groningen-latest.osm.pbf \
      https://download.geofabrik.de/europe/netherlands/groningen-latest.osm.pbf
    ```

2.  Optional: reduce the extract to the routing objects with `osmium`. See
    [Datasets](../guides/datasets.md). The full extract also imports
    correctly.

!!! note
    The `datasets/` directory is not under version control. Only
    `datasets/README.md` is in the repository.

## 2. Import the dataset

1.  Import the extract as the dataset `groningen`:

    ```bash
    netweevil dataset import datasets/groningen-latest.osm.pbf --name groningen
    ```

2.  Make sure that the import is complete. The last line of the output
    shows the number of nodes and directed edges:

    ```text
    imported dataset 'groningen' with 495671 nodes and 1023014 directed edges, ...
    ```

    The numbers depend on the date of the extract.

The import builds the topology and the routing hierarchy. It writes the
dataset manifest to `.netweevil/datasets/groningen.json`.

## 3. Compile the profiles

1.  Validate the car profile:

    ```bash
    netweevil profile validate examples/profiles/car_research_v1.yml
    ```

2.  Compile the car profile for the dataset:

    ```bash
    netweevil profile compile \
      --dataset groningen \
      --profile examples/profiles/car_research_v1.yml
    ```

3.  Compile the pedestrian profile for the dataset:

    ```bash
    netweevil profile compile \
      --dataset groningen \
      --profile examples/profiles/pedestrian_research_v1.yml
    ```

Each compile writes a compiled profile manifest to
`.netweevil/compiled_profiles/` and a metric bundle to
`.netweevil/bundles/metrics/`. See [Profiles](../concepts/profiles.md).

## 4. Calculate a route with the CLI

1.  Calculate the route in `examples/requests/route.json`:

    ```bash
    netweevil analyze route \
      --dataset groningen \
      --profile examples/profiles/car_research_v1.yml \
      --request examples/requests/route.json \
      --out .netweevil/runs/example-route.geojson
    ```

2.  Make sure that the command writes two files:

    ```text
    route result written to .netweevil/runs/example-route.geojson
    run manifest written to .../.netweevil/runs/<run_id>.json
    ```

3.  Open `example-route.geojson` in QGIS or in another GIS viewer.

The extension of the `--out` file selects the output format. The run
manifest records the dataset, the profile and the effective request.

## 5. Start the API

1.  Start the API with the dataset and the two profiles:

    ```bash
    netweevil api serve \
      --dataset groningen \
      --default-profile examples/profiles/car_research_v1.yml \
      --profile examples/profiles/pedestrian_research_v1.yml
    ```

2.  In a second terminal, make sure that the service is ready:

    ```bash
    curl http://127.0.0.1:8080/healthz
    curl http://127.0.0.1:8080/readyz
    ```

    `/healthz` sends `{"status":"ok"}`. `/readyz` sends
    `{"status":"ready", ...}` when a dataset is loaded.

3.  List the loaded profiles:

    ```bash
    curl http://127.0.0.1:8080/v1/profiles
    ```

!!! note
    `api serve` with arguments writes the selection to
    `.netweevil/workspace.json`. The next `netweevil api serve` without
    arguments loads the same selection.

## 6. Send a route request

1.  Send a route request for the car profile:

    ```bash
    curl -X POST http://127.0.0.1:8080/v1/route \
      -H 'Content-Type: application/json' \
      -d '{
        "profile_id": "car_research_v1",
        "request": {
          "route_id": "quickstart",
          "origin": {"id": "a", "lon": 6.5665, "lat": 53.2194},
          "destination": {"id": "b", "lon": 6.5716, "lat": 53.2148}
        }
      }'
    ```

2.  Examine the response. The `service` object identifies the dataset, the
    profile and the engine. The `result.summary` object contains the
    distance and the travel time:

    ```json
    {
      "service": {
        "dataset_id": "groningen",
        "profile_id": "car_research_v1",
        "route_engine": "cch_with_restriction_sequence_validation",
        ...
      },
      "result": {
        "route_id": "quickstart",
        "summary": {"total_distance_m": 3079, "total_travel_time_s": 384.5, ...},
        ...
      }
    }
    ```

3.  Send the same request for the pedestrian profile. Add
    `?format=geojson` to get a GeoJSON `FeatureCollection`:

    ```bash
    curl -X POST 'http://127.0.0.1:8080/v1/route?format=geojson' \
      -H 'Content-Type: application/json' \
      -d '{
        "profile_id": "pedestrian_research_v1",
        "request": {
          "route_id": "quickstart-walk",
          "origin": {"id": "a", "lon": 6.5665, "lat": 53.2194},
          "destination": {"id": "b", "lon": 6.5716, "lat": 53.2148}
        }
      }'
    ```

See the [HTTP API](../api/index.md) for all operations.

## 7. Open the web console

1.  Make sure that the binary contains the web console. See
    [Build the web console](installation.md#build-the-web-console-optional).
2.  Open `http://127.0.0.1:8080/` in a browser.

The console shows the loaded dataset and profiles. See
[Web console](../tools/web-console.md).

!!! tip
    To start without a dataset, run `netweevil bootstrap`. The service
    starts in setup mode and opens the browser. Use **Setup** in the console
    to import data and to write profiles. In setup mode, `/readyz` sends
    `{"status":"setup"}` with status `503`.

## Next steps

- [Routing](../guides/routing.md)
- [Batch analysis](../guides/batch-analysis.md)
- [Service areas](../guides/service-areas.md)
- [CLI reference](../cli/reference.md)
