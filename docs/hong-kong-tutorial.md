# Hong Kong roads, 3D walking and transit

This example imports the full Hong Kong OSM road extract, Lands Department outdoor and indoor pedestrian networks, MTR platforms and exits, and a combined transit feed. The console can draw source elevations, underground station paths and platform-bound transit journeys.

The walking and road networks are separate selectable datasets. `hk_pedestrian_3d` joins the outdoor and indoor sources by XYZ and supports walking plus transit. `hk_roads` supports driving on OSM roads with turn restrictions. The importer does not merge OSM and GeoPackage graphs or support a car-to-indoor interchange across these two datasets.

## Start the prepared example

A fresh clone does not contain the downloaded datasets, terrain tiles or compiled executable. Follow **Prepare from source files** below first. `--serve-only` is for a workspace that has already completed preparation.

From the repository root:

```bash
scripts/setup_hong_kong.sh --serve-only
```

Open <http://127.0.0.1:8080/>. The server loads `hk_pedestrian_3d`, the ordinary and step-free pedestrian profiles, and `hong_kong_multimodal`. The prepared September 2026 feed covers September 10 through 16. Choose a departure inside the imported window; the frontend starts on the first imported service day in Hong Kong time.

For the road network, stop that process and run:

```bash
scripts/setup_hong_kong.sh --serve-only --roads
```

You can also switch datasets through **Setup** without restarting. Choose `hk_roads` with `car_research_v3`, or `hk_pedestrian_3d` with the pedestrian profiles and combined feed. Transit platform bindings belong to the pedestrian dataset.

## Choose the walking preference

The default `pedestrian_fastest_multilayer` minimises walking time. It retains slope speeds, stairs, escalators, lift waiting and actual 3D geometry. `pedestrian_step_free_fastest_multilayer` applies the corresponding step-free restrictions. Neither adds a preference for covered paths or a separate ascent burden.

The optional `pedestrian_multilayer` and `pedestrian_step_free_multilayer` research profiles favour shelter and reduced climbing. An uncovered second costs an extra 0.75 seconds without a shade overlay. This can deliberately choose longer passages around buildings, underground links or lifts. A Causeway Bay–Central comparison gave 3,734 m and 35.6 minutes with the shelter profile versus 3,599 m and 32.2 minutes with fastest walking. These are different objectives, not equivalent route choices.

For an existing console session, select `pedestrian_fastest_multilayer` in **Profile**; a saved explicit profile selection is preserved on refresh. Use the matching transit transfer profile. The street geometry follows the Lands Department source, including real bends and passages. Switching preferences does not straighten source links through buildings or remove valid alleys. The checked corridor includes 1–4 m footway jogs and source links classified as service lanes. Matching the source verifies the import, not the real-world accuracy or current public access of every passage. Inspect those attributes in **Network explorer** when reviewing an individual route.

## Prepare from source files

Requirements are Rust 1.96.1 or later, Node.js 22.12 or later with pnpm, Python 3.11 or later, and GDAL's `ogr2ogr` and `ogrinfo`. Building the platform join requires GDAL Python bindings, and optional DEM preparation also requires NumPy. Use a Python environment whose GDAL bindings match its installed GDAL library. The feed downloader, fusion and network audit use standard-library Python.

Keep the downloaded source files in a separate analysis directory. This does not have to be a Git repository. The expected layout before full setup is:

```text
hong-kong-analysis/
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

The source links and station download/join commands below explain how to create this layout. The setup script installs frontend dependencies with the committed pnpm lockfile, builds the frontend and CLI, and performs the imports. Do not use `--skip-build` for the first run.

If the separate analysis repository already contains the full raw files and platform join:

```bash
scripts/setup_hong_kong.sh \
  --analysis-repo /path/to/hong-kong-analysis \
  --service-start 2026-09-10 --service-days 7
```

The script also discovers `../analyses/hong-kong-analysis` and `../hong-kong-analysis`. To reuse already transformed files:

```bash
scripts/setup_hong_kong.sh \
  --analysis-repo /path/to/hong-kong-analysis \
  --prepared-dir /path/to/hong-kong-analysis/prepared \
  --service-start 2026-09-10 --service-days 7 --no-serve
```

The enriched `3D_Indoor_Network_MTR_Platforms.gpkg` takes precedence over the baseline indoor file. It is staged as `3D_Indoor_Network.gpkg` so the mapping finds it. Horizontal coordinates become WGS84 longitude/latitude; Z remains Hong Kong Principal Datum metres. Do not reproject those heights into ellipsoidal or terrain-relative elevation.

The setup script builds the console and CLI, downloads missing transit/platform files, creates audited precision connectors, imports both datasets, audits retained geometry and attributes, compiles profiles, imports the feed, and builds ordinary and step-free walking transfer tables. This is a substantial import, not a small demo. Allow several minutes and disk space for multi-gigabyte routing bundles and transfer paths. `--skip-build` reuses an existing executable and frontend build. `--serve-only` skips all preparation.

To apply profile or cost-model changes to existing imports without importing the spatial data again:

```bash
scripts/setup_hong_kong.sh --profiles-only --no-serve
scripts/setup_hong_kong.sh --serve-only
```

This rebuilds all four walking profiles, their transfer tables and the road profile. Cost-model revisions change profile fingerprints so old metrics cannot silently retain rounded zero-cost links. The current compiler recovers the physical XYZ distance of sub-metre edges that had rounded to zero whole metres; truly coincident connectors still have zero distance.

## Download sources when they are missing

```bash
python3 scripts/download_hong_kong_sources.py --list
python3 scripts/download_hong_kong_sources.py --output-dir .netweevil/hong-kong/raw
```

Existing downloads are reused and validated. Pass `--refresh` to replace them after a successful download. The downloader records URLs, retrieval metadata and SHA-256 checksums.

| Data | Source |
| --- | --- |
| Full road extract | [Geofabrik Hong Kong](https://download.geofabrik.de/asia/china/hong-kong.html) |
| Outdoor 3D pedestrian network | [Lands Department outdoor network](https://data.gov.hk/en-data/dataset/hk-landsd-openmap-3d-pedestrian-network) |
| Indoor 3D pedestrian network | [Lands Department indoor network](https://data.gov.hk/en-data/dataset/hk-landsd-openmap-3d-indoor-network) |
| Station maps and platform polygons | [Lands Department 3D indoor map](https://data.gov.hk/en-data/dataset/hk-landsd-openmap-3d-indoor-map) |
| Bus, ferry, tram and funicular schedules | [Transport Department aggregate-headway GTFS](https://static.data.gov.hk/td/pt-headway-en/gtfs.zip) |
| MTR and light rail GTFS | [Wheels Hong Kong Community GTFS](https://github.com/wheelstransit/hongkong-community-gtfs) |
| Numbered platforms, directions and exits | [Wheels CSDI platform and exit catalogue](https://github.com/wheelstransit/mtr-platform-exits-crawler) |
| MTR line/station ordering | [MTR open-data CSV](https://opendata.mtr.com.hk/data/mtr_lines_and_stations.csv) |

The large Lands Department spatial exports use CSDI download pages. Keep `3D_Pedestrian_Network.gpkg`, `3D_Indoor_Network.gpkg`, station indoor map archives, and the platform join under the analysis repository's `datasets/`. The source exports are not committed. The local sources used for this example were already present, including all 98 station archives.

For a fresh station-map collection, download the Lands Department indoor-map index from the source page above. If it arrives as a FileGDB, convert its extracted `.gdb` directory with `ogr2ogr -f GPKG /path/to/indoor-map-index.gpkg /path/to/Index.gdb`. Keep the index's original attributes. With the outdoor and indoor GeoPackages downloaded, build the platform join before running full setup:

```bash
HK_ANALYSIS=/path/to/hong-kong-analysis

python3 scripts/download_hong_kong_sources.py \
  --output-dir .netweevil/hong-kong/raw --only mtr-lines

python3 scripts/download_hong_kong_mtr_indoor_maps.py \
  /path/to/indoor-map-index.gpkg \
  --output-dir "$HK_ANALYSIS/datasets/3d_indoor_map_mtr" --extract

python3 scripts/build_hong_kong_mtr_platform_join.py \
  --downloads "$HK_ANALYSIS/datasets/3d_indoor_map_mtr" \
  --indoor-network "$HK_ANALYSIS/datasets/3D_Indoor_Network.gpkg" \
  --mtr-lines .netweevil/hong-kong/raw/mtr_lines_and_stations.csv \
  --output-dir "$HK_ANALYSIS/datasets/mtr_platform_join"

scripts/setup_hong_kong.sh --analysis-repo "$HK_ANALYSIS" \
  --service-start 2026-09-10 --service-days 7
```

Use `--help` on those scripts for download retries, concurrency and validation-only options. Preparation checks the resulting MTR columns rather than silently accepting an unenriched indoor graph. Raw pedestrian/indoor FileGDB exports can also be converted with GDAL to the GeoPackage filenames above; preserve their layer names, fields and 3D HK1980/HKPD coordinate reference. The preparation script checks those before reprojection.

The separate [terrain instructions](terrain-viewer.md) link the DEM ZIP and show how to prepare it. Put the extracted `Whole_HK_DTM_5m.asc` under `datasets/dtm/` to include terrain in full setup, or use `--terrain-only --dem /path/to/Whole_HK_DTM_5m.asc` afterwards.

## Explore the third dimension

1. Search for Central, Admiralty, Tsuen Wan or a `lon, lat` coordinate. Open **Network explorer** and zoom to a station so the viewport request can show individual pedestrian links.
2. In **Style**, enable **3D**. Tilt the map and set vertical exaggeration. Start near 3× for stations, then compare with 1×. Exaggeration changes rendering only.
3. Colour the network by elevation, pedestrian facility, indoor location, level or structure. Hover a link to inspect retained source attributes. Negative HKPD height does not by itself mean underground, and a hillside subway may have positive height.
4. Use the underground/x-ray controls to lower basemap opacity. Elevation clipping selects a height slab and clips lines at its boundaries. Source heights remain unchanged in exported GeoJSON.
5. **Station platforms** is enabled by default in 3D. The viewer fetches source platform surfaces and GTFS-bound boarding points for the whole Hong Kong feed automatically. Zoom to a station and follow the access, transfer and egress paths across its floors. Hover a platform to inspect its verified numbers, lines, source level and provenance. Whole-floor footprints have a distinct style and label.
6. Additional reference GeoJSON files can still be imported through **Style**. Multiple files remain visible when switching tools:

```text
.netweevil/hong-kong/transit/mtr_platform_surfaces.geojson
.netweevil/hong-kong/transit/mtr_catalog_platforms.geojson
.netweevil/hong-kong/transit/mtr_platforms.geojson
.netweevil/hong-kong/transit/mtr_exits.geojson
```

The surface file contains the original XYZ polygons. The catalogue file contains all numbered platform points; the boarding file contains the actual GTFS-bound stops and binding provenance. The exit file contains exits. Imported reference overlays are for inspection. The imported pedestrian graph and stop bindings determine routing.

For **Transit route**, select the combined feed and an imported service date. Expand **Transit** and use **Allowed transit modes** to check permitted modes and uncheck excluded modes. For MTR-only journeys, click **Allow none**, then check **MTR / Subway / Metro**. Tram and light rail share **Tram / Light rail**. Use network street access, network walking geometry and the matching transfer profile. The Hong Kong frontend defaults select these when available. This makes access, station transfers and egress follow the pedestrian graph.

**Max marker snap distance** controls attachment to the pedestrian graph and defaults to 50 m. **Max walk access** controls which transit stops can be reached by walking. Increasing the latter does not permit a distant graph attachment. The map shows accepted endpoint gaps as dashed amber **Off-network connection (not routed)** lines; they are separate from the surveyed walking geometry. The estimated gap distance and time appear in the network-path components. The request field is `modes.max_endpoint_snap_distance_m`.

A fixed A/B altitude constrains snapping to that height, even in 2D. Map moves clear the old fixed altitude; enter a new value only when targeting a known floor. Clear the altitude for automatic snapping. For the reported Sai Ying Pun street point at 114.1441, 22.2879, the source street height is +3.47 m. Retaining a different station exit's +0.058 m height produced zero egress candidates; automatic snapping found a scheduled MTR journey. Both Sai Ying Pun platforms at −30.93 m connect to that street point in both directions. `mtr-sai-ying-pun.json` reproduces the working request.

Vehicle routes use GTFS shapes where present. Their Z is interpolated between bound platform stops for display and is marked `geometry_elevation_source`. With stop segments requested, the main geometry passes through every returned intermediate platform height. Without them, interpolation uses the leg endpoints. These heights are not surveyed rail tunnel geometry. Walking paths use the actual 3D network. Both walking and transit requests accept an optional endpoint `z`; network transit access and egress use a one-metre height window around an explicit endpoint height. Unknown source elevations remain marked as unknown; a display height of zero is not a measurement.

Enable **Style → 3D → DEM terrain** to show the local 5 m Hong Kong terrain. It shares the network exaggeration and preserves x-ray viewing of underground paths. [Terrain setup and height rules](terrain-viewer.md) explain preparation, source accuracy and the HKPD datum. Existing imports can add terrain with `scripts/setup_hong_kong.sh --terrain-only --skip-build --no-serve`.

Station surfaces are served by `POST /v1/transit-feeds/{feed_id}/station-geometry` with a WGS84 `bbox`. The GeoJSON response includes coverage/truncation metadata. The server derives the optional surface sidecar from the imported GTFS filename: `hong_kong_multimodal.gtfs.zip` uses `hong_kong_multimodal.gtfs.stations.geojson` beside it. Boarding points come directly from the loaded feed's graph bindings. No client-supplied filesystem path is accepted.

The local sidecar preserves 175 polygons across 98 stations: 148 platform units and 27 whole-floor footprints. It assigns 233 of 234 numbered CSDI points by unique containment and matching height; Nam Cheong platform 4 lacks a matching source polygon. Shared surfaces can carry multiple verified numbers. Unassigned surfaces, including some AWE, POA and Racecourse geometry, remain visible without guessed numbers. Polygon elevations and routing binding elevations are separate source measurements and are exposed separately.

## Reproduce the example requests

The walking and MTR fixtures under `examples/hong-kong/` use actual catalogued station exits. Separate bus, tram and ferry fixtures use source GTFS stops; the driving fixture uses road coordinates. MTR uses the API mode `subway`, and light rail uses `tram`. Transit time is explicit Hong Kong time, `+08:00`. Change it when importing a different service window.

```bash
curl --fail-with-body http://127.0.0.1:8080/v1/network/edges \
  -H 'Content-Type: application/json' --data-binary @examples/hong-kong/network.json

curl --fail-with-body http://127.0.0.1:8080/v1/route \
  -H 'Content-Type: application/json' --data-binary @examples/hong-kong/walk.json

curl --fail-with-body http://127.0.0.1:8080/v1/transit-route \
  -H 'Content-Type: application/json' --data-binary @examples/hong-kong/transit.json
```

Request an ordered itinerary with the same routing payload at `/v1/transit-directions`:

```bash
curl --fail-with-body http://127.0.0.1:8080/v1/transit-directions \
  -H 'Content-Type: application/json' --data-binary @examples/hong-kong/transit-directions.json
```

The JSON response contains `service`, the complete route `result`, and ordered `directions`. Instructions cover access, waiting, boarding, riding, alighting, station transfers and egress. Each instruction has a leg index, duration and timestamps in the feed timezone. Available station names, platform codes, headsigns and XYZ locations accompany the text. Expand `walking_maneuvers` for street turns and source stair, escalator or lift instructions. Turns are included only when the instruction engine reproduces the saved walking path; `directions_diagnostics` explains any omission. Unreachable journeys have an empty directions array. This endpoint always includes geometry, stops and intermediate stop segments and returns JSON.

`transit-directions-step-free.json` demonstrates a lift-based MTR journey using Central Exit A and the step-free fastest profile. The ordinary MTR fixture uses Exit J3, whose modeled escalator access is excluded by that profile.

In the console, choose **Transit directions** to use the same mode selectors and route settings, then inspect the ordered itinerary in Summary. Station geometry is loaded once for the whole Hong Kong feed and retained as the camera moves.

Run the checked walking, MTR, station-interchange, directions, bus, tram and ferry examples together:

```bash
python3 scripts/verify_hong_kong.py --date 2026-09-10
# Include driving if a second hk_roads server is running:
python3 scripts/verify_hong_kong.py --date 2026-09-10 --roads-url http://127.0.0.1:8081
```

The script asserts successful outcomes, platform boarding, retained negative Z and a real network interchange, ordered platform instructions, and saves responses plus `smoke-summary.json`. Run a second road server with `scripts/setup_hong_kong.sh --serve-only --roads --bind 127.0.0.1:8081`.

Inspect every bound stop against the currently loaded graph:

```bash
curl --fail-with-body \
  'http://127.0.0.1:8080/v1/transit-feeds/hong_kong_multimodal/bindings?pedestrian_profile_id=pedestrian_fastest_multilayer'
```

The audit exposes source coordinates, resolved graph coordinates, candidate components and explicit failures. It does not silently replace an unresolved platform with a street stop.

## Coverage and source limitations

The September 2026 local preparation retains 465,475 outdoor and 55,542 indoor source features. The road extract is the local May 8, 2026 OSM snapshot. The station-map archive covers 98 MTR stations. The separate numbered catalogue has 234 platform points and 586 exits.

The merged feed has 2,466 routes: 2,379 bus, 59 ferry, 17 tram/light rail, 10 MTR and one funicular. It retains the official surface feed and selects only MTR/light rail from the community feed, avoiding duplicate community bus and ferry routes. All IDs are namespaced before combination. All 97 regular MTR stations have GTFS platform bindings. Racecourse remains in the indoor map but has no scheduled service in these sources.

Of 216 GTFS boarding platforms, 214 use numbered CSDI-derived points. AsiaWorld-Expo platform 1 and Po Lam platform 1 use the earlier audited platform polygon join because the numbered catalogue lacks those entries. Their numbered-platform and direction assignments remain unverified. All selected coordinates lie on a matching station platform segment, within 0.00049 metres horizontally in the checked snapshot.

MTR and light rail times are community procedural estimates, not an official working timetable. The Transport Department feed is an aggregate-headway feed. NetWeevil preserves those calendars, frequency windows and times; it does not substitute the old synthetic five-minute MTR timetable.

Thirteen separate precision connectors repair same-station breaks of at most 1 cm horizontally and 1 mm vertically. They preserve the original source geometries and each connector appears in `prepared/Station_Precision_Connectors.json`. There are 829 exact shared indoor/outdoor vertices.

The exit audit finds 584 of 586 catalogued exits within 10 metres of a station segment with a weak path outdoors. Tai Koo Exit E1 is 12.58 metres from the nearest station segment, which does connect outdoors. Lo Wu Exit A is present on the indoor network but has no outdoor path in the supplied source. Lok Ma Chau and Racecourse also lack shared outdoor connections. Do not interpret those gaps as permission to draw a straight link across a border facility or between floors. Weak connectivity also does not prove step-free or time-dependent access. Small disconnected fragments remain in some station source geometries. The live audit resolved all 216 boarding bindings in both directions, with no failures. Only the selected Lo Wu and Lok Ma Chau border-station platforms resolve into isolated components. The main boarding bindings in the other stations avoid the stray fragments.

Transfer tables route all candidate stop pairs within 500 metres, using a cap of 256 candidates per stop. This exceeds the maximum 117 neighbors among the 9,746 imported routing stops in the checked feed at that radius. The other 683 GTFS station and entrance records remain in the source feed and reference overlays. Longer transfers require rebuilding with a larger radius. Ordinary and step-free profiles have separate tables, containing 407,560 and 389,328 directed network transfers in the checked snapshot. A step-free failure is retained as a failure; stairs do not become accessible because a station has a lift elsewhere.

## Local output files

All generated data is ignored by Git and stays under `.netweevil/`:

| Location | Contents |
| --- | --- |
| `hong-kong/raw/` | Downloaded feeds, numbered platform/exit catalogue and checksums |
| `hong-kong/prepared/` | WGS84 + HKPD GeoPackages and audited precision connectors |
| `hong-kong/transit/` | Combined GTFS, stop bindings, platform/exit GeoJSON and source manifest |
| `hong-kong/verification/` | Source-retention, connectivity and live API checks |
| `terrain/hong_kong_5m/` | Optional local DEM manifest and versioned Terrarium tiles |
| `datasets/`, `bundles/`, `compiled_profiles/`, `transit_feeds/` | Registered datasets, routing indexes, profiles, transit and transfer bundles |

Refreshing inputs requires rebuilding the graph, profiles, bindings and transfer tables. Reapplying bindings invalidates old transfer registrations. The tutorial's historical request date does not automatically update when you refresh a feed.
