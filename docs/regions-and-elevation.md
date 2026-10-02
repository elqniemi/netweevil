# Admin areas, timezones and elevation

Every import assigns each network node an IANA timezone. Two inputs are optional: country and state boundaries, and a digital elevation model (DEM). Edges take the timezone and administrative area of their tail node. An edge whose two ends are in different countries is a border crossing.

```bash
netweevil dataset import datasets/groningen-latest.osm.pbf --name groningen \
  --admin-boundaries datasets/boundaries.geojson

netweevil dataset import datasets/hong-kong-260508.osm.pbf --name hong_kong \
  --dem /path/to/Whole_HK_DTM_5m.asc --dem-crs EPSG:2326 --dem-vertical-datum HKPD
```

The console import endpoint (`POST /v1/workspace/datasets/import`) takes the same inputs: `admin_boundaries` (an upload name or a path) and `dem: {source, crs, vertical_datum}`. Each boundaries file and DEM is hashed into the dataset's bundle ids. If either input changes, compiled profiles are rebuilt rather than reused.

## Timezones

Zones come from the timezone boundary data embedded in [`tzf-rs`](https://github.com/ringsaturn/tzf-rs). That is a simplified build of [timezone-boundary-builder](https://github.com/evansiroky/timezone-boundary-builder), release 2026d. No network access or system tz database is needed. The binary grows by about 4.5 MB, and a lookup takes about 1 µs. The simplified polygons can be wrong within a few hundred metres of a zone border, which matters only where neighbouring zones have different clocks. Points at sea get the nautical `Etc/GMT±N` zones.

Time-dependent routing evaluates edge rules (opening hours, direction schedules and time-dependent speeds) on the edge's local wall clock. A route that crosses a zone border changes clocks with the network. An instant is never shifted; only its local reading changes. Datasets imported before this change have no zones and keep evaluating rules on the request's offset.

Requests may give `departure_time`/`arrive_by` without an offset (`2026-07-10T08:30`, `2026-07-10T08:30:00`, or a date meaning midnight). These are read in the zone at the origin; `arrive_by` uses the destination. Service areas, accessibility and service-area sequences use their first origin. The daylight-saving policy is the same as for GTFS requests ([transit-time.md](transit-time.md)):

- an autumn time that occurs twice (`2026-10-25T02:30` in Amsterdam) is ambiguous and rejected;
- a spring time skipped by the change (`2026-03-29T02:30`) does not exist and is rejected;
- either one works with an explicit offset (`2026-10-25T02:30:00+01:00` is the second occurrence).

Returned times carry the zone's offset at that instant. Rust callers that evaluate their own local-time rules (for example `*:conditional` tags) can use `netweevil_query::NetworkTimezones` (`edge_zone`, `edge_local_time`, `zone_at`), `in_zone` and `parse_local_datetime`.

## Administrative areas

NetWeevil keeps two levels. Countries come from `admin_level=2` and are coded by `ISO3166-1:alpha2`/`ISO3166-1`. States come from `admin_level=4` and are coded by `ISO3166-2`. Without `--admin-boundaries`, an OSM import reads the `boundary=administrative` relations in the extract itself. That costs one more pass over the ways. A relation is used only when every member way and node is present and every ring closes. A partial ring could put nodes on the wrong side of a border. Regional extracts usually cut country relations, and the import warns about the relations it skipped. A state without its country still yields the country code, from its ISO 3166-2 prefix. In the Groningen province extract, `NL-GR` assembles, so its nodes get `NL`/`NL-GR`. The cut Dutch and German country relations, and neighbouring provinces, are skipped.

`--admin-boundaries` replaces the extract's relations. Use it for cut extracts, Overture and GeoPackage sources. It accepts:

- GeoJSON (`.geojson`/`.json`): Polygon or MultiPolygon features. `admin_level` is read when present. Otherwise a feature with an ISO 3166-2 code (`ISO3166-2`, `iso_3166_2`) is a state, and any other feature is a country (`ISO3166-1:alpha2`, `ISO3166-1`, `ISO_A2_EH`, `ISO_A2`, `iso_a2`, `country_code`). Natural Earth admin-0/admin-1 files work as they are.
- OSM PBF: a boundaries-only extract, for example `osmium tags-filter planet.osm.pbf r/boundary=administrative`.

Rings use the even-odd rule, so holes and enclaves need no role bookkeeping. Where boundaries overlap, the smaller one wins.

Admin areas are used in four places:

- **Driving side.** `curb` and `opposite` snap approaches use the country's traffic side when `point_constraints.<id>.driving_side` is omitted. Without a known country they fall back to right-hand traffic. `netweevil_core::country_driving_side` and `TopologyBundle::edge_driving_side` expose the same rule to guidance.
- **Profile rules.** `country` and `state` are match keys in `speed_rules`, `factors` and `exclude_rules`:

  ```yaml
  speed_rules:
    - {match: {country: DE, highway: motorway}, speed_kph: 130}
  exclude_rules:
    - match: {country: NL, highway: trunk}    # e.g. no bicycles on Dutch trunk roads
  ```
- **Border crossings.** `preferences.avoid_border_crossings: true` excludes crossing edges. `border_crossing_penalty_s` adds generalized cost per crossing, and `border_crossing_time_s` adds travel time, for example for a border check. These are edge metrics, so CCH routes stay exact. Requests set them through `profile_overrides`, for example `{"preferences": {"avoid_border_crossings": true}}`.
- **Responses.** `returns.admin_areas: true` adds `regions` to a route: country/state spans along the route with their driving side and distances, the border-crossing count, and the timezones entered.

## Elevation

`--dem` samples a DEM for OSM and Overture imports. GeoPackage imports keep their surveyed heights and reject `--dem`.

- **Formats.** Single-band GeoTIFF is supported. Its CRS comes from the GeoKey EPSG code, and its PixelIsArea/PixelIsPoint georeferencing and GDAL nodata tag are honoured. ESRI ASCII grid (`.asc`) is also supported. Its rows are parsed in parallel and only the window covering the network is read.
- **CRS.** `--dem-crs` accepts `EPSG:4326`/`4258`, `3857`, `2326` (HK1980 Grid), `28992` (RD New), `27700`, UTM (`326xx`, `327xx`, `258xx`), or any `+proj=` definition. Transforms use the pure-Rust [`proj4rs`](https://github.com/3liz/proj4rs) with published seven-parameter datum shifts. These are accurate to well under a metre, inside DEM accuracy. An ASCII grid without `--dem-crs` must be in longitude/latitude.
- **Vertical datum.** Heights are kept as the raster stores them. `--dem-vertical-datum` only records the label (`HKPD`, `EGM2008`, ...) and does not convert between height references.

Sampling is bilinear between cell centres. Nodata neighbours are dropped, but a position whose own cell is nodata has no height, so coverage never extends past valid cells. Node heights are stored in whole decimetres; nodes outside the raster keep unknown (`NaN`) heights.

Bridges and tunnels (OSM `bridge=*` or `tunnel=*` other than `building_passage`, Overture `is_bridge`/`is_tunnel`) are not draped on the terrain. A node that only lies on such ways is interpolated between the ground nodes the structure connects, weighted by the inverse path distance to each. Along a simple bridge this is linear. Ground edges longer than the sample spacing are densified for their ascent, descent and steepest uphill/downhill grade. The spacing is one DEM cell, and at least 10 m. Structure edges use their end heights only. Edge lengths remain the 3D node-to-node length.

The existing `slope_model` (Tobler or piecewise) uses the stored ascent and descent, so bicycle and walking profiles on OSM data respond to terrain. Example: [`examples/profiles/pedestrian_hilly_osm.yml`](../examples/profiles/pedestrian_hilly_osm.yml).

`returns.elevation: true` adds `elevation` to a route:

- `points`: `[distance_m, height]` per vertex, with `null` heights where unknown;
- `ascent_m` and `descent_m` from the densified edges;
- the minimum and maximum height;
- `max_uphill_grade` and `max_downhill_grade` (rise/run);
- the vertical datum.

OSM and Overture datasets imported without a DEM report `null` heights rather than their placeholder zeros.

### `POST /v1/height`

The import keeps the DEM, cropped to the network, as a tiled raster under `.netweevil/bundles/elevation/`. Tiles are 256 cells, decimetre-rounded and deflated. The API opens it on the first height query and decodes tiles on demand, keeping 64 decoded tiles in memory. Request:

```json
{"points": [[114.1455, 22.2759], [114.1699, 22.2793]]}
{"encoded_polyline": "...", "polyline_precision": 6, "range": true, "resample_distance_m": 25}
```

Use either `points` (`[lon, lat]`) or `encoded_polyline` (latitude first, precision 5 or 6, default 6). `range: true` returns `range_heights` as `[distance_m, height]` instead of `heights`. `resample_distance_m` (at least 1 m) samples the shape at even intervals and returns it as `shape`, or as `encoded_polyline` when the input was encoded. The limit is 20,000 positions. The `service` block reports the dataset, vertical datum and CRS. Heights are `null` outside coverage. A dataset without a raster answers 404.

## Storage

Topology schema 14 stores each layer as a compact column:

| Layer | Storage |
| --- | --- |
| Admin areas, zone names | Interned tables in the bundle header |
| Area and zone of each node | `u32` index columns; a single-zone dataset collapses to a constant column |
| Steepest up/down grade of each edge | Permille `u16` columns |

Edge ascent and descent and node heights use decimetre codecs. Measured on the Groningen routing extract (494k nodes, 1.04M edges), the timezone layer adds 1.6 KB to a 6.21 MB topology bundle. The full Groningen extract with its in-extract `NL-GR` boundary grows from 6,136,757 to 6,140,872 bytes (+0.07 %).
