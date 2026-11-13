# Indoor routing

This guide shows how to import an indoor site, route inside it, and show it
on a map. An indoor site is a building, a station or a campus that the
[indoor engine](../concepts/indoor-routing.md) routes on. A workspace can
hold many sites. Indoor routes need no street dataset.

```mermaid
flowchart LR
    A[Engine input JSON<br/>local metres, WGS84 or a projected CRS] -->|indoor import| C[Site<br/>manifest + input + graph]
    B[OSM .osm.pbf / .osm<br/>Simple Indoor Tagging] -->|indoor import| C
    C -->|analyze indoor-route| D[Route in WGS84]
    C -->|POST /v1/indoor/route| D
    C -->|GET /v1/indoor/sites/id/geojson| E[Site features for a map]
```

## Sites and anchors

The indoor engine works in metres. A site has an **anchor**: a longitude and
a latitude for the origin of its coordinates, and an optional rotation. The
anchor defines a local tangent plane. NetWeevil converts between the site
metres and WGS84 with it. The conversion is exact in both directions: a
point converted to WGS84 and back moves by less than 1 mm over 5 km.

| Anchor field | Meaning |
| ------------ | ------- |
| `lon`, `lat` | WGS84 position of the site point `(0, 0)`. |
| `rotation_deg` | Angle from east to the site x axis, counter-clockwise, in degrees. Default `0`: x points east and y points north. |
| `elevation_m` | Height of the site `z = 0`. Output `z` is this value plus the engine `z`. Default `0`. |

## Entrances

Entrances join a site to the street network. An entrance is a portal, a
node or a POI that has the tag `entrance`, or a portal of kind `entrance`.
The import records each entrance with:

- its position and level,
- its **outdoor side**: the point where it meets the streets,
- `step_free`: the `step_free` value of the portal.

The outdoor side of a portal is the side that is not on an indoor surface.
An indoor surface has walls or has the tag `indoor`. When both sides or no
side is on an indoor surface, the side farther from the site centre is the
outdoor side. A node or a POI is its own outdoor side. To set the outdoor
side yourself, give the element `props.outdoor` as `[x, y]` in site metres.

A site without entrances works for indoor routes, but street routes cannot
enter it.

## Import a site

```bash
netweevil indoor import <SOURCE> --name <SITE_ID> [--label TEXT]
    [--anchor LON,LAT] [--rotation DEG] [--elevation M]
    [--crs EPSG:CODE] [--bbox MIN_LON,MIN_LAT,MAX_LON,MAX_LAT]
```

The import compiles the site, finds its entrances and prints the counts,
the entrances and the compile warnings. A second import with the same name
replaces the site.

### Engine input in local metres

The [input schema](../concepts/indoor-routing.md#input-schema) describes
the document. Give the anchor of the site:

```bash
netweevil indoor import examples/indoor/demo_site.json --name demo \
  --label "Demo building" --anchor 6.5665,53.2194
```

### Engine input in WGS84

Set `"crs": "EPSG:4326"` in the document and give coordinates as
`[lon, lat]`. The anchor is the centre of the bounding box of the
coordinates. `--anchor` replaces it.

### Engine input in a projected CRS

Give the CRS with `--crs` or with the `crs` field of the document. The CRS
is `EPSG:<code>` from the built-in table (UTM zones, RD New `EPSG:28992`,
British National Grid `EPSG:27700`, Hong Kong 1980 `EPSG:2326`, Web Mercator
`EPSG:3857`) or a `+proj=` definition. The anchor is the centre of the
coordinates.

```bash
netweevil indoor import campus_rd.json --name campus --crs EPSG:28992
```

### OpenStreetMap with Simple Indoor Tagging

The importer reads `.osm.pbf` and `.osm` (XML) files. It puts all indoor
features of the file into one site. Use `--bbox` to select one building
from a larger extract.

```bash
netweevil indoor import station.osm.pbf --name station \
  --bbox 6.5630,53.2095,6.5660,53.2115
```

| OSM | Site element |
| --- | ------------ |
| `indoor=room` (closed way or multipolygon) | Surface with walls, class `room`. |
| `indoor=corridor` | Surface with walls, class `corridor`. |
| `indoor=area` | Open surface without walls, class `area`. |
| `door=*`, `indoor=door`, `entrance=*` on the outline of a room, corridor or area | Portal across the outline. `width` sets the width (default 0.9 m, 1.2 m for an entrance). `wheelchair=no` or `door=revolving` makes the portal not step-free. `entrance=*` makes it a site entrance. |
| `highway=corridor\|footway\|path\|steps\|pedestrian` with `level` or `indoor=yes` | Edge. Steps are not step-free. A way on two or more levels is a vertical (stairs, ramp or escalator) from its first level to its last level. `incline=down` reverses the direction. |
| `highway=elevator` node with two or more levels | Lift with a stop on each level. |
| `highway=elevator` or `stairwell=*` area | A room on each level, and a lift or stairs at its centre. |
| `conveying=*` | On steps: an escalator (`forward` moves along the way, `backward` against it). On a flat way: a travelator edge at 2 m/s. |
| `indoor=wall`, `barrier=*` with `level` | Barrier. |
| Rooms with `name` or `ref`, `amenity=*`, `shop=*`, `office=*` with `level` | POI. Route to it by its name or ref. |

Levels:

- `level` and `repeat_on` give the levels. The importer accepts lists
  (`0;1`), ranges (`1-3`, `-2--1`), negative and fractional levels (`-1`,
  `0.5`). It repeats a feature on each of its levels.
- Level `n` has the height `z = n × storey height`. The storey height is
  `height / building:levels` of the building around the site centre when
  the building has both tags. Otherwise it is 3.5 m.
- The level id is the number (`0`, `1`, `-1`, `0.5`).

Element ids are the OSM ids (`w123`, `n45`, `r6`). A feature on several
levels has the id `w123@1` on level 1. Each element keeps its OSM id and
all its tags in `props`.

Doors that are not on the outline of a room, a corridor or an area on their
level are skipped. The import prints how many.

## Manage sites

```bash
netweevil indoor list
netweevil indoor show demo       # manifest: anchor, levels, entrances, counts
netweevil indoor remove demo
```

Sites are stored in `.netweevil/indoor_sites/<site_id>.json` and
`.netweevil/bundles/indoor/<site_id>/` (the input in site metres and the
compiled graph).

## Route inside a site

A request names the site, two points, a profile and an optional mode:

```json
{
  "route_id": "workshop-to-1.14",
  "site_id": "demo",
  "origin": {"poi": "Workshop"},
  "destination": {"poi": "1.14"},
  "profile": "wheelchair",
  "mode": "mixed"
}
```

| Field | Meaning |
| ----- | ------- |
| `origin`, `destination` | A point: `{lon, lat, level}`, `{poi}` or `{space}`. |
| `lon`, `lat` | A position in WGS84. |
| `level` | A level id (`"1"`) or a level number (`1`, `-1`, `0.5`). Without a level, the point snaps to the nearest level. |
| `poi` | A POI id, or the `name` or `ref` of a POI. The match ignores case. When several POIs match, the route goes to the best one. |
| `space` | A space id, or the name or ref of a POI. |
| `profile` | A built-in indoor profile (`walk`, `wheelchair`, `fastest`, `bike`, `evacuation`) or a profile object. Default `walk`. |
| `mode` | `mixed`, `edges_only` or `surfaces_only`. It replaces the mode of the profile. |

Run it with the CLI:

```bash
netweevil analyze indoor-route --request examples/indoor/indoor_route_request.json \
  --out route.json
```

`--out route.geojson` writes one line feature for each segment.

Or with the API:

```bash
curl -X POST 'http://127.0.0.1:8080/v1/indoor/route?format=geojson' \
  -H 'content-type: application/json' \
  --data @examples/indoor/indoor_route_request.json
```

The result has:

| Field | Meaning |
| ----- | ------- |
| `summary` | `duration_s`, `length_m`, `climb_m`, `descent_m` and `cost` (seconds weighted by the profile). |
| `geometry` | `[lon, lat, z]` points. `z` is metres above the base of the site. |
| `segments` | One entry for each walk, edge, portal, vertical and connector: `type`, `kind`, `id`, `level`, `space`, `class`, `length_m`, `duration_s`, `props`, and the index range in `geometry`. |
| `spaces` | The spaces that the route crosses, in order. |
| `origin_poi`, `destination_poi` | The POIs that the route starts or ends at. |
| `warnings` | Warnings of the engine. |

## Show a site on a map

`GET /v1/indoor/sites` lists the sites. `GET /v1/indoor/sites/{site_id}`
sends the manifest. `GET /v1/indoor/sites/{site_id}/geojson?level=1` sends
the site elements of one level as GeoJSON. Each feature has the property
`feature_type`: `surface`, `barrier`, `edge`, `portal`, `vertical`, `node`,
`poi` or `entrance`.

The API loads a site when a request first uses it. It reloads the site
after a new import.
