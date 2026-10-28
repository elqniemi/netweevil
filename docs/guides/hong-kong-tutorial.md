# Hong Kong tutorial: roads, 3D walking and transit

This tutorial imports a full Hong Kong example:

- the OSM road extract;
- the Lands Department outdoor and indoor 3D pedestrian networks;
- MTR platforms and exits;
- a combined transit feed.

The web console then shows source elevations, underground station paths and
platform-bound transit journeys.

The walking and road networks are separate datasets:

- `hk_pedestrian_3d` joins the outdoor and indoor sources by XYZ. It supports
  walking and transit.
- `hk_roads` supports driving on OSM roads with turn restrictions.

!!! note
    The importer does not merge the OSM and GeoPackage graphs. A car-to-indoor
    interchange across the two datasets is not supported.

## Prerequisites

- Rust 1.96.1 or later.
- Node.js 22.12 or later, with pnpm.
- Python 3.11 or later.
- GDAL `ogr2ogr` and `ogrinfo`.
- GDAL Python bindings for the platform join. Use a Python environment whose
  GDAL bindings match its installed GDAL library.
- NumPy for the optional DEM preparation.

The feed downloader, the fusion and the network audit use standard-library
Python only. Run the unit tests of the helper scripts from the repository
root:

```bash
python3 -m unittest discover -s scripts -p 'test_*.py'
```

The terrain tests need GDAL and NumPy.

## Start a prepared workspace

A fresh clone does not contain the downloaded datasets, terrain tiles or a
compiled executable. Do the steps in
[Prepare from source files](#prepare-from-source-files) first. Use
`--serve-only` only for a workspace that completed the preparation.

1. From the repository root, start the pedestrian and transit view:

    ```bash
    scripts/setup_hong_kong.sh --serve-only
    ```

2. Open <http://127.0.0.1:8080/>.
3. Choose a departure inside the imported service window. The frontend
   starts on the first imported service day in Hong Kong time.

The server loads `hk_pedestrian_3d`, the ordinary and step-free pedestrian
profiles, and the `hong_kong_multimodal` feed. The September 2026 preparation
covers 10 to 16 September.

For the road network, stop that process. Then run:

```bash
scripts/setup_hong_kong.sh --serve-only --roads
```

You can also change datasets in **Setup** without a restart. Choose
`hk_roads` with `car_research_v3`, or `hk_pedestrian_3d` with the pedestrian
profiles and the combined feed. Transit platform bindings belong to the
pedestrian dataset.

## Choose the walking profile

| Profile | Behavior |
| --- | --- |
| `pedestrian_fastest_multilayer` (default) | Minimizes walking time. Keeps slope speeds, stairs, escalators, lift waiting and the actual 3D geometry |
| `pedestrian_step_free_fastest_multilayer` | Same, with step-free restrictions |
| `pedestrian_multilayer` | Research profile. Prefers shelter and less climbing |
| `pedestrian_step_free_multilayer` | Step-free research profile. Prefers shelter and less climbing |

The fastest profiles add no preference for covered paths and no separate
ascent burden. In the research profiles, an uncovered second costs an extra
0.75 seconds without a shade overlay. These profiles can choose longer
passages around buildings, underground links or lifts. For example, compare
one Causeway Bay to Central route:

- shelter profile: 3,734 m and 35.6 minutes;
- fastest walking: 3,599 m and 32.2 minutes.

These are different objectives, not equivalent route choices.

To change the profile in an open console session:

1. Select `pedestrian_fastest_multilayer` in **Profile**. The console keeps an
   explicit profile selection after a refresh.
2. Use the matching transit transfer profile.

The street geometry follows the Lands Department source, including real
bends and passages. A profile change does not straighten source links through
buildings or remove valid alleys. The checked corridor includes footway jogs
of 1 to 4 m and source links classified as service lanes.

!!! note
    A match with the source verifies the import. It does not verify the
    real-world accuracy or current public access of each passage. Inspect
    those attributes in **Network explorer** when you review a route.

## Prepare from source files

Keep the downloaded source files in a source directory outside the checkout.
The layout before full setup is:

```text
hk-sources/
  datasets/
    3D_Pedestrian_Network.gpkg
    3D_Indoor_Network.gpkg
    3d_indoor_map_mtr/                 # downloaded station archives + manifest
    mtr_platform_join/
      3D_Indoor_Network_MTR_Platforms.gpkg
      mtr_platforms_joined.geojson
      mtr_platform_join.csv
      netweevil_stop_bindings.json
    dtm/Whole_HK_DTM_5m.asc            # optional terrain source
```

The sections [Download the sources](#download-the-sources) and
[Build the platform join](#build-the-platform-join) show how to make this
layout.

### Run the full setup

1. Supply the source directory with `--source-dir`, or set
   `NETWEEVIL_HK_SOURCE_DIR`.
2. Run the setup:

    ```bash
    scripts/setup_hong_kong.sh \
      --source-dir /path/to/hk-sources \
      --service-start 2026-09-10 --service-days 7
    ```

Do not use `--skip-build` on the first run. The setup script does these
steps:

1. It installs the frontend dependencies from the committed pnpm lockfile.
2. It builds the console and the CLI.
3. It downloads missing transit and platform files.
4. It makes audited precision connectors.
5. It imports both datasets. When the DTM is present, the road import samples
   it with `--dem`.
6. It audits the retained geometry and attributes.
7. It compiles the profiles.
8. It imports the feed.
9. It builds ordinary and step-free walking transfer tables.
10. It prepares terrain tiles when the DTM is present.

This is a large import, not a small demo. Allow several minutes and a few
gigabytes of disk space for the routing bundles. Each packed walking transfer
table is about 40 MB.

To use files that you already transformed:

```bash
scripts/setup_hong_kong.sh \
  --source-dir /path/to/hk-sources \
  --prepared-dir /path/to/hk-sources/prepared \
  --service-start 2026-09-10 --service-days 7 --no-serve
```

The enriched `3D_Indoor_Network_MTR_Platforms.gpkg` takes precedence over the
baseline indoor file. The setup stages it as `3D_Indoor_Network.gpkg` so that
the mapping finds it. Horizontal coordinates become WGS84 longitude and
latitude. Z stays in Hong Kong Principal Datum metres.

!!! warning
    Do not reproject the HKPD heights into ellipsoidal or terrain-relative
    elevation.

### Setup options

| Option | Effect |
| --- | --- |
| `--source-dir DIR` | Directory with `datasets/3D_*` and `datasets/mtr_platform_join`. Default: `NETWEEVIL_HK_SOURCE_DIR` |
| `--prepared-dir DIR` | Uses outdoor and enriched indoor GeoPackages that are already reprojected |
| `--service-start DATE` | Feed service start, `YYYY-MM-DD`. Default: today in local time |
| `--service-days N` | Service horizon. Default: 7 |
| `--no-serve` | Prepares, imports and audits without starting the API |
| `--serve-only` | Starts the prepared example without a rebuild |
| `--profiles-only` | Rebuilds profiles and transfer tables from the existing imports |
| `--terrain-only` | Prepares DEM tiles without a new import of routing data |
| `--dem FILE` | DEM ASCII raster. Default: `SOURCE_DIR/datasets/dtm/Whole_HK_DTM_5m.asc` |
| `--roads` | Starts the OSM road view instead of the pedestrian and transit view |
| `--skip-build` | Uses the existing CLI and frontend build |
| `--bind HOST:PORT` | Listen address. Default: `127.0.0.1:8080` |

### Apply profile changes

To apply profile or cost-model changes without a new import of the spatial
data:

```bash
scripts/setup_hong_kong.sh --profiles-only --no-serve
scripts/setup_hong_kong.sh --serve-only
```

This rebuilds all four walking profiles, their transfer tables and the road
profile. A compiled-profile schema change makes cached metrics recompile. The
compiler recovers the physical XYZ distance of sub-metre edges. Connectors
whose ends are truly at the same place still have zero distance.

## Download the sources

```bash
python3 scripts/download_hong_kong_sources.py --list
python3 scripts/download_hong_kong_sources.py --output-dir .netweevil/hong-kong/raw
```

The downloader uses existing downloads again after validation. Pass
`--refresh` to replace them after a successful download. The downloader
records URLs, retrieval metadata and SHA-256 checksums.

| Data | Source |
| --- | --- |
| Full road extract | [Geofabrik Hong Kong](https://download.geofabrik.de/asia/china/hong-kong.html) |
| Outdoor 3D pedestrian network | [Lands Department outdoor network](https://data.gov.hk/en-data/dataset/hk-landsd-openmap-3d-pedestrian-network) |
| Indoor 3D pedestrian network | [Lands Department indoor network](https://data.gov.hk/en-data/dataset/hk-landsd-openmap-3d-indoor-network) |
| Station maps and platform polygons | [Lands Department 3D indoor map](https://data.gov.hk/en-data/dataset/hk-landsd-openmap-3d-indoor-map) |
| Bus, ferry, tram and funicular schedules | [Transport Department aggregate-headway GTFS](https://static.data.gov.hk/td/pt-headway-en/gtfs.zip) |
| MTR and light rail GTFS | [Wheels Hong Kong Community GTFS](https://github.com/wheelstransit/hongkong-community-gtfs) |
| Numbered platforms, directions and exits | [Wheels CSDI platform and exit catalogue](https://github.com/wheelstransit/mtr-platform-exits-crawler) |
| MTR line and station order | [MTR open-data CSV](https://opendata.mtr.com.hk/data/mtr_lines_and_stations.csv) |

The large Lands Department spatial exports use CSDI download pages. Keep
`3D_Pedestrian_Network.gpkg`, `3D_Indoor_Network.gpkg`, the station
indoor-map archives and the platform join in the `datasets/` folder of the
source directory. The repository does not contain the source exports.

Raw pedestrian and indoor FileGDB exports can also be converted with GDAL to
the GeoPackage file names above. Keep their layer names, fields and 3D
HK1980/HKPD coordinate reference. The preparation script checks them before
reprojection. See
[Multilayer and temporal routing](multilayer-temporal-routing.md#prepare-the-source-data).

## Build the platform join

1. Download the Lands Department indoor-map index from the station-map source
   page.
2. If the index is a FileGDB, convert the extracted `.gdb` directory. Keep the
   original attributes of the index:

    ```bash
    ogr2ogr -f GPKG /path/to/indoor-map-index.gpkg /path/to/Index.gdb
    ```

3. Download the outdoor and indoor GeoPackages.
4. Build the platform join:

    ```bash
    HK_SOURCES=/path/to/hk-sources

    python3 scripts/download_hong_kong_sources.py \
      --output-dir .netweevil/hong-kong/raw --only mtr-lines

    python3 scripts/download_hong_kong_mtr_indoor_maps.py \
      /path/to/indoor-map-index.gpkg \
      --output-dir "$HK_SOURCES/datasets/3d_indoor_map_mtr" --extract

    python3 scripts/build_hong_kong_mtr_platform_join.py \
      --downloads "$HK_SOURCES/datasets/3d_indoor_map_mtr" \
      --indoor-network "$HK_SOURCES/datasets/3D_Indoor_Network.gpkg" \
      --mtr-lines .netweevil/hong-kong/raw/mtr_lines_and_stations.csv \
      --output-dir "$HK_SOURCES/datasets/mtr_platform_join"
    ```

5. Run the full setup:

    ```bash
    scripts/setup_hong_kong.sh --source-dir "$HK_SOURCES" \
      --service-start 2026-09-10 --service-days 7
    ```

Use `--help` on those scripts for download retries, concurrency and
validation-only options. The preparation checks the MTR columns of the
result. It does not accept an indoor graph without the enrichment.

## Add terrain

[Terrain](terrain.md) links the DEM ZIP and shows how to prepare it. Put the
extracted `Whole_HK_DTM_5m.asc` in `datasets/dtm/` to include terrain in the
full setup. Alternatively, add terrain to existing imports:

```bash
scripts/setup_hong_kong.sh --terrain-only --skip-build --no-serve \
  --dem /path/to/Whole_HK_DTM_5m.asc
```

## Explore the third dimension

1. Search for Central, Admiralty, Tsuen Wan or a `lon, lat` coordinate.
2. Open **Network explorer**. Zoom to a station so that the viewport request
   shows individual pedestrian links.
3. In **Style**, enable **3D**. Tilt the map and set the vertical
   exaggeration. Start near 3× for stations, then compare with 1×. The
   exaggeration changes the rendering only.
4. Color the network by elevation, pedestrian facility, indoor location,
   level or structure. Hover over a link to see its source attributes.
5. Use the underground and x-ray controls to lower the basemap opacity.
   Elevation clipping selects a height slab and clips lines at its
   boundaries. Source heights in exported GeoJSON do not change.
6. **Station platforms** is on by default in 3D. The viewer gets source
   platform surfaces and GTFS-bound boarding points for the full feed. Zoom
   to a station and follow the access, transfer and egress paths across its
   floors. Hover over a platform to see its verified numbers, lines, source
   level and provenance. Whole-floor footprints have a different style and
   label.
7. Optional: import more reference GeoJSON files through **Style**. Several
   files stay visible when you change tools:

    ```text
    .netweevil/hong-kong/transit/mtr_platform_surfaces.geojson
    .netweevil/hong-kong/transit/mtr_catalog_platforms.geojson
    .netweevil/hong-kong/transit/mtr_platforms.geojson
    .netweevil/hong-kong/transit/mtr_exits.geojson
    ```

!!! note
    A negative HKPD height does not by itself mean underground. A hillside
    subway can have a positive height.

| File | Contents |
| --- | --- |
| `mtr_platform_surfaces.geojson` | Original XYZ platform polygons |
| `mtr_catalog_platforms.geojson` | All numbered platform points |
| `mtr_platforms.geojson` | GTFS-bound boarding stops and binding provenance |
| `mtr_exits.geojson` | Exits |

Imported reference overlays are for inspection only. The pedestrian graph and
the stop bindings control routing.

Enable **Style → 3D → DEM terrain** to show the 5 m Hong Kong terrain. It
uses the network exaggeration and keeps x-ray viewing of underground paths.

### Plan a transit route in the console

1. Select **Transit route**.
2. Select the combined feed and an imported service date.
3. Expand **Transit**. In **Allowed transit modes**, check the permitted modes
   and clear the excluded modes.
    - For MTR-only journeys, click **Allow none**. Then check
      **MTR / Subway / Metro**.
    - Tram and light rail share **Tram / Light rail**.
4. Use network street access, network walking geometry and the matching
   transfer profile. The Hong Kong frontend defaults select these when
   available.

Then access, station transfers and egress follow the pedestrian graph.

**Max marker snap distance** controls the attachment to the pedestrian graph.
The default is 50 m. The request field is
`modes.max_endpoint_snap_distance_m`. **Max walk access** controls which
transit stops a walk can reach. A larger walk access does not permit a
distant graph attachment.

The map shows accepted endpoint gaps as dashed amber
**Off-network connection (not routed)** lines. They are separate from the
surveyed walking geometry. The estimated gap distance and time appear in the
network-path components.

### Fixed altitude

A fixed A/B altitude limits snapping to that height, also in 2D. A map move
clears the old fixed altitude. Enter a new value only for a known floor.
Clear the altitude for automatic snapping.

For example, the Sai Ying Pun street point at 114.1441, 22.2879 has a source
street height of +3.47 m. With the +0.058 m height of a different station
exit, the engine finds no egress candidates there. Automatic snapping finds a
scheduled MTR journey. Both Sai Ying Pun platforms at −30.93 m connect to
that street point in both directions.
`examples/hong-kong/mtr-sai-ying-pun.json` reproduces the working request.

Walking and transit requests accept an optional endpoint `z`. Network transit
access and egress use a one-metre height window around an explicit endpoint
height. See [Transit](transit.md#vehicle-geometry-heights) for the heights of
vehicle legs.

### Station surfaces

The console gets station surfaces from
`POST /v1/transit-feeds/{feed_id}/station-geometry`. See
[Transit](transit.md#station-geometry).

The local sidecar keeps 175 polygons across 98 stations: 148 platform units
and 27 whole-floor footprints. It assigns 233 of 234 numbered CSDI points by
unique containment and a matching height. Nam Cheong platform 4 has no
matching source polygon. Shared surfaces can carry several verified numbers.
Unassigned surfaces, including some AWE, POA and Racecourse geometry, stay
visible without guessed numbers. Polygon elevations and binding elevations
are separate source measurements. The viewer shows them separately.

## Reproduce the example requests

The walking and MTR fixtures in `examples/hong-kong/` use catalogued station
exits. The bus, tram and ferry fixtures use source GTFS stops. The driving
fixture uses road coordinates. MTR uses the API mode `subway`. Light rail
uses `tram`. Transit times are explicit Hong Kong time, `+08:00`.

!!! note
    The fixtures use 10 September 2026. Change the date when you import a
    different service window.

| File | Request |
| --- | --- |
| `drive.json` | Central waterfront to Wan Chai on `hk_roads` |
| `network.json` | Source 3D pedestrian links around Central and Admiralty |
| `walk-corridor.json` | Causeway Bay to Central. Compare fastest walking with the shelter profile |
| `walk.json` | Central Exit J3 to Admiralty Exit B |
| `mtr.json` | Tsuen Wan Exit C to Central Exit J3, MTR only |
| `mtr-sai-ying-pun.json` | Sha Tin to a Sai Ying Pun street point, with automatic endpoint elevation |
| `mtr-interchange.json` | Tsuen Wan to Tai Koo with a routed station interchange |
| `bus.json`, `tram.json`, `ferry.json` | Single-mode checks with source GTFS stops |
| `transit.json` | Tsuen Wan Exit C to Central Exit J3, with network access, egress and transfers |
| `transit-directions.json` | MTR interchange with walking, boarding and ride instructions |
| `transit-directions-step-free.json` | Lift-based MTR journey to Central Exit A with the step-free fastest profile |

```bash
curl --fail-with-body http://127.0.0.1:8080/v1/network/edges \
  -H 'Content-Type: application/json' --data-binary @examples/hong-kong/network.json

curl --fail-with-body http://127.0.0.1:8080/v1/route \
  -H 'Content-Type: application/json' --data-binary @examples/hong-kong/walk.json

curl --fail-with-body http://127.0.0.1:8080/v1/transit-route \
  -H 'Content-Type: application/json' --data-binary @examples/hong-kong/transit.json
```

Request an ordered itinerary with the same payload at
`/v1/transit-directions`:

```bash
curl --fail-with-body http://127.0.0.1:8080/v1/transit-directions \
  -H 'Content-Type: application/json' --data-binary @examples/hong-kong/transit-directions.json
```

See [Transit](transit.md#transit-directions) for the response. The ordinary
MTR fixture ends at Exit J3. Its modeled escalator access does not satisfy
the step-free profile, so `transit-directions-step-free.json` uses Central
Exit A.

In the console, choose **Transit directions** to use the same mode selectors
and route settings. Inspect the ordered itinerary in **Summary**. The console
loads station geometry once for the full feed and keeps it as the camera
moves.

### Run the checks

1. Run the checked walking, MTR, station-interchange, directions, bus, tram
   and ferry examples:

    ```bash
    python3 scripts/verify_hong_kong.py --date 2026-09-10
    ```

2. Optional: to include driving, start a second road server:

    ```bash
    scripts/setup_hong_kong.sh --serve-only --roads --bind 127.0.0.1:8081
    ```

3. Run the checks with the road server:

    ```bash
    python3 scripts/verify_hong_kong.py --date 2026-09-10 --roads-url http://127.0.0.1:8081
    ```

The script asserts successful outcomes, platform boarding, kept negative Z, a
real network interchange and ordered platform instructions. It saves the
responses and `smoke-summary.json`.

Audit each bound stop against the loaded graph:

```bash
curl --fail-with-body \
  'http://127.0.0.1:8080/v1/transit-feeds/hong_kong_multimodal/bindings?pedestrian_profile_id=pedestrian_fastest_multilayer'
```

See [Transit](transit.md#audit-bindings).

## Coverage and source limits

The figures below describe the September 2026 local preparation.

### Networks

- The preparation keeps 465,475 outdoor and 55,542 indoor source features.
- The road extract is the OSM snapshot of 8 May 2026.
- The station-map archive covers 98 MTR stations. The numbered catalogue has
  234 platform points and 586 exits.
- Thirteen precision connectors repair same-station breaks of at most 1 cm
  horizontally and 1 mm vertically. They keep the original source
  geometries. `prepared/Station_Precision_Connectors.json` lists each
  connector.
- There are 829 exact shared indoor and outdoor vertices.

### Feed

- The merged feed has 2,466 routes: 2,379 bus, 59 ferry, 17 tram and light
  rail, 10 MTR and one funicular.
- It keeps the official surface feed. It takes only MTR and light rail from
  the community feed, to prevent duplicate bus and ferry routes. All ids get
  a namespace before the combination.
- All 97 regular MTR stations have GTFS platform bindings. Racecourse is in
  the indoor map but has no scheduled service in these sources.
- Of 216 GTFS boarding platforms, 214 use numbered CSDI points. AsiaWorld-Expo
  platform 1 and Po Lam platform 1 use the earlier audited polygon join,
  because the catalogue does not have them. Their platform number and
  direction assignments are not verified.
- All selected coordinates lie on a matching station platform segment,
  within 0.00049 m horizontally in the checked snapshot.

!!! warning
    MTR and light rail times are community estimates, not an official
    working timetable. The Transport Department feed is an aggregate-headway
    feed. NetWeevil keeps those calendars, frequency windows and times. It
    does not embed a Hong Kong timetable. The schedules and any feed repair
    are preparation artifacts.

### Exits and connectivity

- The exit audit finds 584 of 586 catalogued exits within 10 m of a station
  segment with a weak path outdoors.
- Tai Koo Exit E1 is 12.58 m from the nearest station segment. That segment
  connects outdoors.
- Lo Wu Exit A is on the indoor network but has no outdoor path in the
  source. Lok Ma Chau and Racecourse also have no shared outdoor
  connections.
- Some station source geometries have small disconnected fragments.
- The live audit resolved all 216 boarding bindings in both directions, with
  no failures. Only the Lo Wu and Lok Ma Chau border-station platforms
  resolve into isolated components. The main boarding bindings in the other
  stations avoid the fragments.

!!! warning
    Do not draw a straight link across a border facility or between floors
    to close these gaps. Weak connectivity does not prove step-free or
    time-dependent access.

### Transfer tables

- Transfer tables route all candidate stop pairs within 500 m, with a cap of
  256 candidates per stop. This is more than the largest number of
  neighbors (117) among the 9,746 routing stops at that radius.
- The other 683 GTFS station and entrance records stay in the source feed and
  the reference overlays.
- Longer transfers need a rebuild with a larger radius.
- The ordinary and step-free profiles have separate tables, with 407,560 and
  389,328 directed network transfers.
- A step-free failure stays a failure. Stairs do not become accessible
  because a station has a lift elsewhere.

### Illustrative fixtures

The `ILLUSTRATIVE-*` stop bindings in `examples/transit/` only show the
accepted schema. The full setup makes real bindings under
`.netweevil/hong-kong/transit/`.

## Local output files

Git ignores all generated data. It stays under `.netweevil/`:

| Location | Contents |
| --- | --- |
| `hong-kong/raw/` | Downloaded feeds, numbered platform and exit catalogue, checksums |
| `hong-kong/prepared/` | WGS84 + HKPD GeoPackages and audited precision connectors |
| `hong-kong/transit/` | Combined GTFS, stop bindings, platform and exit GeoJSON, source manifest |
| `hong-kong/verification/` | Source-retention, connectivity and live API checks |
| `terrain/hong_kong_5m/` | Optional DEM manifest and versioned Terrarium tiles |
| `datasets/`, `bundles/`, `compiled_profiles/`, `transit_feeds/` | Datasets, routing indexes, profiles, transit and transfer bundles |

!!! note
    New inputs require a rebuild of the graph, profiles, bindings and
    transfer tables. A new application of bindings removes the old transfer
    registrations. The request dates of the tutorial do not change when you
    refresh a feed.

## Related pages

- [Multilayer and temporal routing](multilayer-temporal-routing.md).
- [Transit](transit.md).
- [Terrain](terrain.md).
- [HTTP API reference](../api/reference.md) for the full schema.
