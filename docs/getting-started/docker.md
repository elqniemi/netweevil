# Docker

The repository contains a `Dockerfile` and a `docker-compose.yml` file. The
image contains the `netweevil` binary with the web console embedded.

## Image structure

The `Dockerfile` has these build stages:

| Stage | Base image | Purpose |
| ----- | ---------- | ------- |
| `console` | `node:22.23.4-bookworm-slim` | Builds the web console with pnpm. |
| `builder` | `rust:1.96.1-slim-bookworm` | Builds `netweevil` with the console in `frontend/dist`. |
| `runtime` | `debian:bookworm-slim` | Contains the binary, `curl` and the user `netweevil`. |
| `data-fetch` | `debian:bookworm-slim` | Optional. Downloads source data at build time. |
| `importer` | `runtime` | Optional. Imports the downloaded data. |
| `final` | `runtime` | Contains the runtime and the imported state directory. |

In the container:

- The working directory is `/data`.
- The state directory is `/data/.netweevil`.
- The entry point is `netweevil`. The default command is `--help`.
- The API port is `8080`.

## Compose service

The Compose file defines the service `api`.

| Item | Value |
| ---- | ----- |
| Command | `bootstrap --no-open --bind 0.0.0.0:8080` |
| Port | `${NETWEEVIL_PORT:-8080}:8080` |
| Volume `netweevil-state` | Mounted on `/data/.netweevil` |
| Bind mount `./datasets` | Mounted read-only on `/data/datasets` |
| Bind mount `./examples` | Mounted read-only on `/data/examples` |
| Health check | `curl -sf http://localhost:8080/healthz` |

The service starts in setup mode. Open `http://localhost:8080/` and use
**Setup** in the web console to import data. Files in `./datasets` are in
the container at `/data/datasets/<file>`. You can give that path to the
import step. You do not have to upload the file.

## Start in setup mode

1.  Build the image:

    ```bash
    docker compose build
    ```

2.  Start the service:

    ```bash
    docker compose up -d api
    ```

3.  Make sure that the service runs:

    ```bash
    curl http://127.0.0.1:8080/healthz
    ```

4.  Open `http://localhost:8080/` in a browser.

## Import a dataset at runtime

This procedure keeps the image small. The data stays in the
`netweevil-state` volume.

1.  Put the source extract in `./datasets`. See
    [Datasets](../guides/datasets.md).
2.  Import the extract into the volume:

    ```bash
    docker compose run --rm api dataset import \
      datasets/groningen-260508-routing.osm.pbf \
      --name osm
    ```

3.  Start the service:

    ```bash
    docker compose up -d api
    ```

4.  In the web console, select the dataset `osm` and the profiles.

The same command imports an Overture `.parquet` file or a directory of
parquet files.

## Serve a dataset at start

To load a dataset when the container starts, replace `command` in
`docker-compose.yml`:

```yaml
command: ["api", "serve", "--dataset", "osm", "--default-profile",
          "examples/profiles/car_research_v1.yml", "--bind", "0.0.0.0:8080"]
```

The profile path is relative to `/data`. The `./examples` bind mount makes
`examples/profiles/` available.

## Bake data into the image

You can also download and import data when you build the image. The image
then contains the imported state directory. On the first start, Docker
copies this state into the empty `netweevil-state` volume.

!!! note
    The raw downloads stay in the intermediate `data-fetch` stage. Only the
    imported bundles go into the final image.

The Compose file sends these environment variables to the build:

| Environment variable | Build argument | Effect |
| -------------------- | -------------- | ------ |
| `NETWEEVIL_FETCH_OSM_URL` | `DATA_SOURCE_URL` | Downloads an OSM PBF extract from the URL. |
| `NETWEEVIL_FETCH_OVERTURE_BBOX` | `OVERTURE_BBOX` | Downloads Overture transportation segments for the box `west,south,east,north`. |
| `NETWEEVIL_DATASET` | `DATASET_NAME` | Sets the id of the imported dataset. The default is `osm`. |

Bake an OSM extract:

```bash
NETWEEVIL_FETCH_OSM_URL=https://download.geofabrik.de/europe/netherlands/groningen-latest.osm.pbf \
docker compose build
```

Bake Overture segments for a bounding box:

```bash
NETWEEVIL_FETCH_OVERTURE_BBOX=6.4,53.1,6.7,53.3 \
docker compose build
```

The Overture download uses the `overturemaps` Python CLI. The build installs
it only when you set the bounding box.

Without Compose, give the build arguments to `docker build`:

```bash
docker build --build-arg DATA_SOURCE_URL=https://download.geofabrik.de/europe/netherlands/groningen-latest.osm.pbf \
  --build-arg DATASET_NAME=osm -t netweevil:latest .
```

!!! warning
    The volume copies the baked state only when the volume is empty. To use
    a new baked image with an existing volume, remove the volume first with
    `docker compose down -v`. This command deletes all state in the volume.

## Compose variables

| Variable | Default | Use |
| -------- | ------- | --- |
| `NETWEEVIL_PORT` | `8080` | Host port. |
| `NETWEEVIL_DATASET` | `osm` | Dataset id for the baked import. |
| `NETWEEVIL_FETCH_OSM_URL` | empty | OSM extract URL for the baked import. |
| `NETWEEVIL_FETCH_OVERTURE_BBOX` | empty | Overture bounding box for the baked import. |

Example with a different host port:

```bash
NETWEEVIL_PORT=18080 docker compose up -d api
```

!!! warning
    The API has no authentication. Do not publish the port to a network
    that you do not trust.
