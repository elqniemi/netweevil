# Service areas

A service area is the part of the street network that origins reach within a
time or distance limit. This guide shows how to calculate service areas,
polygons, inbound service areas and service-area sequences over time. For
transit catchments, see [Transit](transit.md#transit-service-areas).

## Prerequisites

- An imported dataset and a compiled profile. See [Datasets](datasets.md).
- For HTTP requests: a running API that loads the profile.

## Calculate a service area

=== "CLI"

    ```bash
    cargo run -p netweevil-cli -- analyze service-area \
      --dataset groningen_2026_05 \
      --profile examples/profiles/pedestrian_research_v1.yml \
      --request examples/requests/service_area.json \
      --out .netweevil/runs/example-service-area.geojson
    ```

=== "HTTP"

    1. Put the service-area request in the `request` field.
    2. Send it to `POST /v1/service-area`. Add `?format=geojson` for GeoJSON.

    The envelope is `{profile_id?, request}`.

## Request fields

| Field | Default | Meaning |
| --- | --- | --- |
| `analysis_id` | required | Identifier of the result. Must not be empty |
| `origins[]` | required | One or more `{id, lon, lat, z?}` points |
| `thresholds[]` | required | `{id?, limit, metric}`. `metric` is `travel_time_s` or `distance_m`. `limit` must be above zero |
| `snap`, `connectivity` | defaults | As for a route. See [Routing](routing.md#route-request-fields) |
| `fallback` | strict | Must stay at the default value |
| `output_mode` | `both` | `network`, `polygon` or `both` |
| `band_mode` | `cumulative` | `cumulative`, `ring` or `none` |
| `boundary_mode` | `overlap` | `overlap` or `cut_at_boundary` |
| `multi_origin_mode` | `merge` | `merge`, `overlap` or `cut` |
| `direction` | `outbound` | `outbound` or `inbound` |
| `polygon` | defaults | Polygon options. See [Polygon options](#polygon-options) |
| `returns` | defaults | Output options. See [Return options](#return-options) |
| `avoid`, `vehicle` | none | As for a route |
| temporal fields | none | `departure_time`, `arrive_by`, `scenario`, `holidays`, `overlays` |

### Polygon options

| Field | Default | Meaning |
| --- | --- | --- |
| `hull_aggressiveness` | 1.0 | Scale of the boundary tracing |
| `cell_size_m` | 25 m × `hull_aggressiveness`, at least 10 m | Grid cell size that traces the boundary. Smaller cells follow the network more closely and cost more |
| `simplification_tolerance_m` | none | Douglas-Peucker tolerance for each ring, in metres |
| `denoise` | none | Drops parts smaller than this fraction (0 to 1) of the largest part of the band. `1` keeps only the largest part |
| `min_part_area_m2` | none | Drops parts smaller than this area |
| `min_hole_area_m2` | none | Fills holes smaller than this area |

### Return options

| Field | Default | Meaning |
| --- | --- | --- |
| `geometry` | `true` | Includes geometry |
| `attributes` | `true` | Includes feature attributes |
| `per_threshold_summary` | `true` | Includes a summary for each threshold |
| `diagnostics` | `true` | Includes diagnostics |
| `segments` | `false` | Includes network segments |
| `max_features` | 250,000 | Upper limit of features |
| `max_segments` | 250,000 | Upper limit of segments |
| `max_geometry_points` | 1,000,000 | Upper limit of geometry points |

## Inbound service areas

By default, a service area expands outbound from its origins. Set
`"direction": "inbound"` to get each position that reaches an origin within
the thresholds. An example question is "who can get here in ten minutes".
The backward search applies the same turn costs and turn restrictions as
forward routes.

See `examples/requests/service_area_inbound.json`.

!!! note
    Inbound service areas use static costs. They do not accept
    `departure_time`. They do not return component vectors.

## Multiple origins

With more than one origin, `multi_origin_mode` controls how the engine
combines the reachable spans:

- `merge` makes a union of the reachable spans. A merged span has no single
  owning origin or winning path.
- `overlap` keeps the overlapping paths of each origin.
- `cut` assigns disjoint spans and keeps labels that belong to one path.

See `examples/requests/service_area_multi_origin.json`.

## Component vectors

When a compiled profile declares named components, each service-area segment
and network feature reports `start_components` and `end_components`. These
are cumulative totals on the winning path from the origin, at the start and
end fractions of the segment. They are not the contribution of the segment
alone.

Static component-aware service areas use an exact, threshold-bounded Dijkstra
search instead of PHAST. PHAST keeps scalar labels and cannot rebuild the
component vector. A result warning records this choice.

!!! note
    With `multi_origin_mode: merge`, merged spans have empty component maps,
    and the result carries a warning. Use `overlap` or `cut` when path-level
    component totals are necessary. Polygons and other aggregate summaries
    also do not have a single cumulative vector. Use the network segments and
    features for path-level component accounting.

For component profiles, see
[Multilayer and temporal routing](multilayer-temporal-routing.md#compile-pedestrian-profiles).

## Service-area sequences

A service-area sequence runs one spatial request for a series of departure
times. Use it to make a temporal animation layer.

1. Write a sequence request. Put the service-area request in `request`. Do
   not set `departure_time` or `arrive_by` in that nested request.
2. Supply the departure times in one of two forms:
    - an explicit `departure_times` list; or
    - an inclusive `start_time` and `end_time` range with `step_s`.
3. Run the sequence.

| Field | Meaning |
| --- | --- |
| `sequence_id` | Identifier of the sequence |
| `request` | The service-area request template |
| `departure_times` | List of RFC 3339 times |
| `start_time`, `end_time`, `step_s` | Inclusive range and step in seconds |

!!! warning
    Do not supply both timing forms. A sequence can have at most 10,000
    frames.

=== "CLI"

    ```bash
    cargo run -p netweevil-cli -- analyze service-area-sequence \
      --dataset hk_pedestrian_3d \
      --profile examples/profiles/pedestrian_multilayer.yml \
      --request examples/requests/hong_kong_example_service_area_sequence.json \
      --out .netweevil/runs/hk-example-ten-am-frames.geojson
    ```

    `examples/requests/hong_kong_example_service_area_sequence.json` runs nine
    five-minute frames from 09:40 to 10:20. The frames span the example 10:00
    escalator direction change.

    `--departure-time` replaces the range or list with one explicit frame.
    The other temporal flags change the nested service-area template.

=== "HTTP"

    Send the request to `POST /v1/service-area-sequence`. Use
    `?format=geojson` for one flat FeatureCollection.

    ```bash
    curl -X POST \
      'http://127.0.0.1:8080/v1/service-area-sequence?format=geojson' \
      -H 'content-type: application/json' \
      --data-binary @- <<'JSON'
    {
      "profile_id": "pedestrian_multilayer",
      "request": {
        "sequence_id": "boundary_frames",
        "request": {
          "analysis_id": "boundary_area",
          "origins": [{"id":"origin","lon":114.1517,"lat":22.2839,"z":34.0}],
          "thresholds": [{"id":"ten_minutes","limit":600,"metric":"travel_time_s"}],
          "scenario": {
            "id": "central_escalator_direction",
            "features": [{"source_feature_id": 250009423, "force_closed": true}]
          }
        },
        "start_time": "2026-07-10T09:40:00+08:00",
        "end_time": "2026-07-10T10:20:00+08:00",
        "step_s": 300
      }
    }
    JSON
    ```

### Sequence result

The JSON result is `{sequence_id, frame_count, frames}`. Each frame has
`frame_index`, `departure_time` and the service-area `result`.

GeoJSON output is one FeatureCollection. Each feature carries `sequence_id`,
`frame_index`, `departure_time`, `scenario_id` and `analysis_id`. The
collection metadata lists the ordered departure times.

## Time zones

A departure time without an offset is read in the time zone of the first
origin. Service areas, accessibility and service-area sequences all use their
first origin. See [Regions and elevation](regions-and-elevation.md#time-zones).

## Related pages

- [Routing](routing.md).
- [Batch analysis](batch-analysis.md).
- [Transit](transit.md#transit-service-areas) for transit catchments.
- [Traffic](traffic.md) for service areas with traffic speeds.
- [HTTP API reference](../api/reference.md) for the full schema.
