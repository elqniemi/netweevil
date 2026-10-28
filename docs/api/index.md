# HTTP API

The NetWeevil API is a local HTTP service. It runs routing and network
analyses on one imported street dataset. It also runs GTFS transit queries,
live traffic updates, agent simulations and the workspace setup flow. The web
console and the QGIS plugin use this API.

The full description is in [`openapi.yaml`](openapi.yaml) (OpenAPI 3.1). The
[API reference](reference.md) shows it as an interactive page.

## Start the service

Start the service with a dataset and a default profile:

```bash
netweevil api serve \
  --dataset groningen_2026_05 \
  --default-profile examples/profiles/car_research_v1.yml \
  --profile examples/profiles/pedestrian_research_v1.yml
```

From a source checkout, use `cargo run -p netweevil-cli -- api serve ...`.
Use `netweevil api serve --help` to see all options.

The default base URL is `http://127.0.0.1:8080`. Use `--bind` to change the
address and the port.

The service also sends its own description:

- `GET /openapi.yaml` sends the OpenAPI document.
- `GET /docs` sends a Swagger UI page. The page loads Swagger UI from a CDN.
  Thus the browser must have internet access.

## Content types

- Send request bodies as JSON, with `Content-Type: application/json`.
- Most responses are JSON (`application/json`).
- Network tiles are Mapbox Vector Tiles (`application/vnd.mapbox-vector-tile`).
- Terrain tiles are PNG images (`image/png`).
- `POST /v1/workspace/uploads` accepts the raw file as the request body.

The service allows requests from all origins (CORS). It has no
authentication. Do not expose it to a network that you do not trust.

## Request and response envelopes

Street analyses use one request envelope:

```json
{
  "profile_id": "car_research_v1",
  "engine_mode": "auto",
  "request": { "route_id": "demo", "origin": {"id": "a", "lon": 6.5665, "lat": 53.2194},
               "destination": {"id": "b", "lon": 6.5716, "lat": 53.2148} }
}
```

- `profile_id` is optional. When you do not send it, the service uses the
  default profile.
- `engine_mode` is optional. Not all operations accept it.
- `request` contains the analysis request.

Analysis responses use one response envelope:

```json
{ "service": { "dataset_id": "...", "profile_id": "...", "route_engine": "..." },
  "result": { } }
```

The `service` object identifies the dataset, the profile and the engine that
calculated the result.

## GeoJSON format switch

Some operations accept the query parameter `format=geojson`:

```bash
curl -X POST 'http://127.0.0.1:8080/v1/route?format=geojson' \
  -H 'Content-Type: application/json' \
  -d @examples/api/route_avoid.json
```

With this parameter, the response is a GeoJSON `FeatureCollection` with the
content type `application/geo+json`. The service then also calculates the full
geometry. These operations accept the switch:

- `/v1/route`, `/v1/directions`, `/v1/waypoints`, `/v1/trip`, `/v1/match`
- `/v1/od`, `/v1/matrix`
- `/v1/service-area`, `/v1/service-area-sequence`
- `/v1/betweenness`, `/v1/scenario-batch`
- `/v1/transit-service-area`
- `/v1/simulation/{simulation_id}/frames` (last frame only)

Some operations always send GeoJSON, for example `/v1/expansion`,
`/v1/network/edges` and the simulation edge statistics.

## Errors

The service sends errors with this body:

```json
{ "error": "unknown profile_id 'car_v9'" }
```

Analysis failures can also have a `diagnostics` array. Each diagnostic has a
`code`, a `severity`, a `message` and optional point ids, component ids and
suggested actions.

| Status | Meaning |
| ------ | ------- |
| `400` | The request is not valid, or the analysis failed. |
| `404` | The named item does not exist, for example a profile or a feed. |
| `409` | The operation conflicts with the current state, for example a name that exists. |
| `415` | The `Content-Type` header is not `application/json`. |
| `422` | The JSON body does not agree with the schema. |
| `500` | The service has an internal problem. |
| `503` | The service is in setup mode, or a background worker stopped. |

The HTTP framework rejects some requests before the service examines them.
These responses have a `text/plain` body. They are `415`, `422`, and `400` for
a JSON syntax error or a bad path or query parameter.

## Setup mode

The service can start without a dataset. This is setup mode. In setup mode:

- `GET /healthz` sends `{"status": "ok"}`.
- `GET /readyz` sends `{"status": "setup"}` with status `503`.
- The workspace operations under `/v1/workspace` work. Use them to upload
  files, import a dataset and write profiles.
- Operations that need a dataset send `503`.

To leave setup mode, send `POST /v1/workspace/activate`. The operation starts
a job. Follow the job with `GET /v1/workspace/jobs/{job_id}`. When the job is
done, `GET /readyz` sends `{"status": "ready"}`.

## Validate the description

The Rust test `api_docs` in `crates/api` makes sure that each route of the
router is in `openapi.yaml`, and that each path in `openapi.yaml` has a route.
To lint the document, run:

```bash
npx @redocly/cli lint docs/api/openapi.yaml --config docs/api/redocly.yaml
```
