# Regions, time zones and elevation

Each dataset import assigns an IANA time zone to each network node. Two more
inputs are optional: country and state boundaries, and a digital elevation
model (DEM). This guide shows how to add them and how routing uses them.

Each edge takes the time zone and the administrative area of its tail node.
An edge whose two ends are in different countries is a border crossing.

## Prerequisites

- An OSM, Overture or GeoPackage source. See [Datasets](datasets.md).
- Optional: a boundaries file (GeoJSON or OSM PBF).
- Optional: a DEM (GeoTIFF or ESRI ASCII grid) for OSM and Overture sources.

## Import with boundaries and a DEM

=== "CLI"

    ```bash
    netweevil dataset import datasets/groningen-latest.osm.pbf --name groningen \
      --admin-boundaries datasets/boundaries.geojson

    netweevil dataset import datasets/hong-kong-260508.osm.pbf --name hong_kong \
      --dem /path/to/Whole_HK_DTM_5m.asc --dem-crs EPSG:2326 --dem-vertical-datum HKPD
    ```

=== "HTTP"

    `POST /v1/workspace/datasets/import` accepts the same inputs:
    `admin_boundaries` and `dem: {source, crs?, vertical_datum?}`. Each value
    is the name of a file in `.netweevil/uploads/`. See
    [Datasets](datasets.md#import-through-the-api).

| Flag | Meaning |
| --- | --- |
| `--admin-boundaries` | Country (`admin_level` 2) and state (`admin_level` 4) boundaries as GeoJSON or OSM PBF |
| `--dem` | DEM to sample for node heights and edge grades (OSM and Overture only) |
| `--dem-crs` | DEM CRS as `EPSG:<code>` or a `+proj=` definition |
| `--dem-vertical-datum` | Vertical datum label recorded with the heights, for example `HKPD` |

!!! note
    Each import or update writes bundles with new ids. The engine rebuilds
    compiled profiles after each rebuild. It does not reuse them.

## Time zones

The time zones come from the boundary data embedded in
[`tzf-rs`](https://github.com/ringsaturn/tzf-rs). That is a simplified build
of [timezone-boundary-builder](https://github.com/evansiroky/timezone-boundary-builder),
release 2026d. No network access or system tz database is necessary. The data
adds about 4.5 MB to the binary. A lookup takes about 1 µs.

The simplified polygons can be wrong within a few hundred metres of a zone
border. This matters only where neighboring zones have different clocks.
Points at sea get the nautical `Etc/GMT±N` zones.

Time-dependent routing evaluates edge rules on the local clock of the edge.
These rules include opening hours, direction schedules and time-dependent
speeds. A route that crosses a zone border changes clocks with the network.
The engine never shifts an instant. Only its local reading changes.

### Times without an offset

Requests can give `departure_time` or `arrive_by` without an offset:
`2026-07-10T08:30`, `2026-07-10T08:30:00`, or a date that means midnight.

- `departure_time` is read in the zone at the origin.
- `arrive_by` is read in the zone at the destination.
- Service areas, accessibility and service-area sequences use their first
  origin.

The daylight-saving policy is the same as for GTFS requests. See
[Transit](transit.md#time-zones-and-service-days).

- An autumn time that occurs twice (`2026-10-25T02:30` in Amsterdam) is
  ambiguous. The engine rejects it.
- A spring time that the change skips (`2026-03-29T02:30`) does not exist.
  The engine rejects it.
- Both work with an explicit offset. `2026-10-25T02:30:00+01:00` is the
  second occurrence.

Returned times carry the offset of the zone at that instant.

Rust callers that evaluate their own local-time rules can use
`netweevil_query::NetworkTimezones` (`edge_zone`, `edge_local_time`,
`zone_at`), `in_zone` and `parse_local_datetime`. An example is
`*:conditional` tags.

## Administrative areas

NetWeevil keeps two levels:

- Countries come from `admin_level=2`. Their codes come from
  `ISO3166-1:alpha2` or `ISO3166-1`.
- States come from `admin_level=4`. Their codes come from `ISO3166-2`.

### Boundaries from the OSM extract

Without `--admin-boundaries`, an OSM import reads the
`boundary=administrative` relations in the extract. This costs one more pass
over the ways.

- The import uses a relation only when all member ways and nodes are present
  and each ring closes. A partial ring could put nodes on the wrong side of a
  border.
- Regional extracts usually cut country relations. The import warns about
  the relations that it skips.
- A state without its country still gives the country code, from its ISO
  3166-2 prefix.

For example, in the Groningen province extract, `NL-GR` assembles. Its nodes
get `NL` and `NL-GR`. The import skips the cut Dutch and German country
relations and the neighboring provinces.

### Boundaries from a file

`--admin-boundaries` replaces the relations of the extract. Use it for cut
extracts, Overture and GeoPackage sources. It accepts:

- **GeoJSON** (`.geojson` or `.json`) with Polygon or MultiPolygon features.
  The import reads `admin_level` when present. Otherwise a feature with an
  ISO 3166-2 code (`ISO3166-2`, `iso_3166_2`) is a state. Any other feature
  is a country (`ISO3166-1:alpha2`, `ISO3166-1`, `ISO_A2_EH`, `ISO_A2`,
  `iso_a2`, `country_code`). Natural Earth admin-0 and admin-1 files work
  without change.
- **OSM PBF**: a boundaries-only extract. For example:
  `osmium tags-filter planet.osm.pbf r/boundary=administrative`.

Rings use the even-odd rule, so holes and enclaves need no role data. Where
boundaries overlap, the smaller one wins.

`examples/api/groningen_province_admin_bounds.geojson` holds the Groningen
province boundary (OSM relation 47826, `NL-GR`, admin level 4). These
commands extract it from a local Groningen PBF:

```bash
osmium tags-filter datasets/groningen-260317.osm.pbf r/ISO3166-2=NL-GR -f opl
osmium getid -r datasets/groningen-260317.osm.pbf r47826 \
  -o examples/api/groningen_province_admin_bounds.osm.pbf
osmium export examples/api/groningen_province_admin_bounds.osm.pbf \
  -o examples/api/groningen_province_admin_bounds.geojson \
  -f geojson
```

### Uses of admin areas

- **Driving side.** `curb` and `opposite` snap approaches use the traffic
  side of the country when `point_constraints.<id>.driving_side` is absent.
  Without a known country, they use right-hand traffic. Directions use the
  same rule. `netweevil_core::country_driving_side` and
  `TopologyBundle::edge_driving_side` expose it to Rust callers.
- **Profile rules.** `country` and `state` are match keys in `speed_rules`,
  `factors` and `exclude_rules`:

    ```yaml
    speed_rules:
      - {match: {country: DE, highway: motorway}, speed_kph: 130}
    exclude_rules:
      - match: {country: NL, highway: trunk}    # e.g. no bicycles on Dutch trunk roads
    ```

- **Border crossings.** `preferences.avoid_border_crossings: true` excludes
  crossing edges. `border_crossing_penalty_s` adds generalized cost per
  crossing. `border_crossing_time_s` adds travel time, for example for a
  border check. These are edge metrics, so CCH routes stay exact. Requests
  set them through `profile_overrides`, for example
  `{"preferences": {"avoid_border_crossings": true}}`.
- **Responses.** `returns.admin_areas: true` adds `regions` to a route. It
  lists the country and state spans along the route with their driving side
  and distances, the border-crossing count and the time zones entered. Each
  waypoint leg and each alternative route gets its own `regions`.

## Elevation

`--dem` samples a DEM for OSM and Overture imports. GeoPackage imports keep
their surveyed heights and reject `--dem`.

### DEM formats

- **GeoTIFF**: single band. The CRS comes from the GeoKey EPSG code. The
  import honors PixelIsArea and PixelIsPoint georeferencing and the GDAL
  nodata tag.
- **ESRI ASCII grid** (`.asc`): the import parses rows in parallel and reads
  only the window that covers the network.

### CRS and vertical datum

`--dem-crs` accepts `EPSG:4326` and `4258`, `3857`, `2326` (HK1980 Grid),
`28992` (RD New), `27700`, UTM (`326xx`, `327xx`, `258xx`), or any `+proj=`
definition. Transforms use the pure-Rust
[`proj4rs`](https://github.com/3liz/proj4rs) with published seven-parameter
datum shifts. These are accurate to well under a metre, inside DEM accuracy.

!!! note
    An ASCII grid without `--dem-crs` must be in longitude and latitude.
    GeoTIFFs normally carry their own CRS.

The import keeps heights as the raster stores them. `--dem-vertical-datum`
only records the label (`HKPD`, `EGM2008`, ...). It does not convert between
height references.

### Sampling

- Sampling is bilinear between cell centers.
- The import drops nodata neighbors. A position whose own cell is nodata has
  no height. Thus coverage never extends past valid cells.
- Node heights are stored in whole decimetres. Nodes outside the raster keep
  unknown (`NaN`) heights.

Bridges and tunnels are not draped on the terrain. These are OSM `bridge=*`
or `tunnel=*` (other than `building_passage`), and Overture `is_bridge` or
`is_tunnel`. A node that lies only on such ways gets an interpolated height
between the ground nodes that the structure connects. The weights are the
inverse path distance to each ground node. Along a simple bridge, this is
linear.

Ground edges longer than the sample spacing are densified for their ascent,
descent and steepest uphill and downhill grade. The spacing is one DEM cell,
and at least 10 m. Structure edges use their end heights only. Edge lengths
stay the 3D node-to-node length.

### Slope in profiles

The `slope_model` of a profile (Tobler or piecewise) uses the stored ascent
and descent. Thus bicycle and walking profiles on OSM data respond to
terrain. See `examples/profiles/pedestrian_hilly_osm.yml`.

### Maximum grade

`preferences.max_grade` excludes edges that are too steep. The limit is rise
over run (`0.08` is 8%). The engine compares it with the steepest grade of
each edge in the travel direction.

- A single number limits uphill and downhill grades.
- `{uphill: 0.08, downhill: 0.12}` sets each direction separately. Omit one
  to keep that direction unlimited.

```yaml
preferences:
  max_grade: {uphill: 0.08, downhill: 0.12}
```

Grades are stored in whole permille. A grade equal to the limit is allowed.
A two-way street that climbs 10% is excluded uphill only when you limit only
`uphill`. A `downhill` limit excludes the other direction as well.

The exclusion is a per-edge metric like `avoid_border_crossings`, so CCH
routes stay exact. Requests can set it with `profile_overrides`, for example
`{"preferences": {"max_grade": 0.06}}`. See
`examples/profiles/bicycle_max_grade_osm.yml`.

!!! note
    The grades come from the DEM. Datasets imported without `--dem` exclude
    nothing. This includes GeoPackage imports with surveyed heights.

### Elevation in route results

`returns.elevation: true` adds `elevation` to a route, to each waypoint leg
and to each alternative:

- `points`: `[distance_m, height]` per vertex, with `null` heights where
  unknown;
- `ascent_m` and `descent_m` from the densified edges;
- the minimum and maximum height;
- `max_uphill_grade` and `max_downhill_grade` (rise over run);
- the vertical datum.

OSM and Overture datasets imported without a DEM report `null` heights, not
placeholder zeros.

## Query DEM heights

The import keeps the DEM, cropped to the network, as a tiled raster under
`.netweevil/bundles/elevation/`. Tiles are 256 cells, rounded to decimetres
and deflated. The API opens the raster on the first height query. It decodes
tiles on demand and keeps 64 decoded tiles in memory.

Send `POST /v1/height` with one of these bodies:

```json
{"points": [[114.1455, 22.2759], [114.1699, 22.2793]]}
```

```json
{"encoded_polyline": "...", "polyline_precision": 6, "range": true, "resample_distance_m": 25}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `points` | `[]` | `[lon, lat]` positions |
| `encoded_polyline` | none | Encoded polyline, latitude first, instead of `points` |
| `polyline_precision` | 6 | 5 or 6 |
| `range` | `false` | Returns `range_heights` as `[distance_m, height]` instead of `heights` |
| `resample_distance_m` | none | Samples the shape at even intervals (at least 1 m). Returns it as `shape`, or as `encoded_polyline` when the input was encoded |

The limit is 20,000 positions after resampling. The `service` block reports
the dataset, the vertical datum and the CRS. Heights are `null` outside
coverage. A dataset without a raster answers 404.

`examples/api/height.json` asks for heights along three Hong Kong points with
100 m resampling and range output.

## Storage

The topology bundle stores each layer as a compact column:

| Layer | Storage |
| --- | --- |
| Admin areas, zone names | Interned tables in the bundle header |
| Area and zone of each node | `u32` index columns. A single-zone dataset collapses to a constant column |
| Steepest up and down grade of each edge | Permille `u16` columns |

Edge ascent and descent and node heights use decimetre codecs. On a province
extract, the time zone and admin-area layers add less than 0.1% to the
topology bundle.

## Terrain display

Display of a DEM as terrain in the web console's 3D view is separate from
sampling it into the network. See [Terrain](terrain.md).

## Related pages

- [Datasets](datasets.md).
- [Snap constraints](snap-constraints.md).
- [Multilayer and temporal routing](multilayer-temporal-routing.md).
- [HTTP API reference](../api/reference.md) for the full schema.
