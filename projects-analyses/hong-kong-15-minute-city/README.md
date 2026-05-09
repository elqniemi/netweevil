# Hong Kong 15-Minute City Study

This directory contains the reproducible setup for a Hong Kong 15-minute city
analysis using `datasets/hong-kong-260508.osm.pbf` and a Hong Kong Community
GTFS feed that includes MTR rail.

## Workflow

1. Import the routing dataset and compile the road profiles:

   ```bash
   cargo run --release -p netweevil-cli -- dataset import datasets/hong-kong-260508.osm.pbf --name hong_kong_2026_05_08 --acceleration-profile balanced
   cargo run --release -p netweevil-cli -- profile compile --dataset hong_kong_2026_05_08 --profile projects-analyses/hong-kong-15-minute-city/profiles/pedestrian_15min_hong_kong_v1.yml
   cargo run --release -p netweevil-cli -- profile compile --dataset hong_kong_2026_05_08 --profile projects-analyses/hong-kong-15-minute-city/profiles/car_15min_hong_kong_v1.yml
   ```

2. Import the Hong Kong Community GTFS feed. The hosted feed is stored
   unchanged, and `datasets/gtfs-hong-kong-community-wheels-netweevil.zip` is the
   Netweevil-compatible analysis copy with blank non-timepoint `stop_times` rows
   interpolated:

   ```bash
   python3 projects-analyses/hong-kong-15-minute-city/scripts/normalize_gtfs_for_netweevil.py \
     --source datasets/gtfs-hong-kong-community-wheels.zip \
     --out datasets/gtfs-hong-kong-community-wheels-netweevil.zip \
     --metadata projects-analyses/hong-kong-15-minute-city/metadata/generated/gtfs_hong_kong_community_wheels_netweevil.json
   cargo run --release -p netweevil-cli -- transit import datasets/gtfs-hong-kong-community-wheels-netweevil.zip \
     --name hong_kong_community_wheels_2026_05_06 \
     --service-start 2026-05-09 \
     --service-days 7
   ```

3. Extract residential origins and destination point sets from the full OSM extract:

   ```bash
   uv run --project projects-analyses/hong-kong-15-minute-city \
     python projects-analyses/hong-kong-15-minute-city/scripts/extract_osm_features.py
   ```

   The extractor requires `osmium` on `PATH`. It writes CSV point sets under
   `requests/`, GeoJSON/GPKG source layers under `outputs/processed/`, raw
   filtered OSM PBFs under `outputs/raw/`, and per-layer metadata under
   `metadata/generated/`.

4. Acquire the small set of non-OSM context layers needed for the Groningen/HK
   comparison:

   ```bash
   uv run --project projects-analyses/hong-kong-15-minute-city \
     python projects-analyses/hong-kong-15-minute-city/scripts/acquire_context_data.py
   ```

   This writes:

   - `datasets/downloads/hong_kong/hk_2021_tpu_subunit_boundaries.gpkg`
   - `datasets/downloads/hong_kong/hk_ozp_land_use_zonings.gpkg`
   - `datasets/downloads/hong_kong/hk_land_utilization_2024_rendered.tif`
   - `metadata/generated/hk_context_data_sources.json`

   The TPU/Subunit and OZP layers are acquired through ArcGIS REST query APIs
   with pagination. The land-utilization layer is acquired through the CSDI
   MapServer export API as rendered PNG tiles, then georeferenced and mosaicked
   as an EPSG:2326 GeoTIFF. Treat that raster as a contextual/cartographic land
   use overlay unless a raw class-code raster becomes available.

5. Run reduced accessibility campaigns for pedestrian and car:

   ```bash
   cargo run --release -p netweevil-cli -- analyze accessibility \
     --dataset hong_kong_2026_05_08 \
     --profile projects-analyses/hong-kong-15-minute-city/profiles/pedestrian_15min_hong_kong_v1.yml \
     --origins projects-analyses/hong-kong-15-minute-city/requests/origins/residential_h3_r9_points.csv \
     --destination food_retail=projects-analyses/hong-kong-15-minute-city/requests/destinations/food_retail.csv \
     --destination education=projects-analyses/hong-kong-15-minute-city/requests/destinations/education.csv \
     --destination healthcare=projects-analyses/hong-kong-15-minute-city/requests/destinations/healthcare.csv \
     --destination public_services=projects-analyses/hong-kong-15-minute-city/requests/destinations/public_services.csv \
     --destination parks_recreation=projects-analyses/hong-kong-15-minute-city/requests/destinations/parks_recreation.csv \
     --destination transit_stops=projects-analyses/hong-kong-15-minute-city/requests/destinations/transit_stops.csv \
     --out projects-analyses/hong-kong-15-minute-city/outputs/raw/walking_accessibility_reduced.csv
   ```

   Repeat the same command with `profiles/car_15min_hong_kong_v1.yml` and
   `outputs/raw/car_accessibility_reduced.csv`.

## Data Provenance

- OSM extract: `datasets/hong-kong-260508.osm.pbf`
- Context acquisition manifest: `projects-analyses/hong-kong-15-minute-city/metadata/generated/hk_context_data_sources.json`
- 2021 TPU/Subunit boundaries: `datasets/downloads/hong_kong/hk_2021_tpu_subunit_boundaries.gpkg`
- 2024 land-utilization rendered raster: `datasets/downloads/hong_kong/hk_land_utilization_2024_rendered.tif`
- OZP land-use zonings: `datasets/downloads/hong_kong/hk_ozp_land_use_zonings.gpkg`
- Primary GTFS ZIP: `datasets/gtfs-hong-kong-community-wheels.zip`
- Primary GTFS source URL: `https://feed.justusewheels.com/hk.gtfs.zip`
- Primary GTFS repository: `https://github.com/wheelstransit/hongkong-community-gtfs`
- Primary GTFS provider: Wheels / Hong Kong Community GTFS
- Primary GTFS coverage includes MTR rail, Light Rail, MTR Bus, bus, ferry, and tram routes
- Primary GTFS SHA-256: `15c31c34429d90c40ba98bcb65146efd947fca5b8f36d1969ad44c5213fe4799`
- Netweevil-compatible primary GTFS SHA-256: `c87de431d51e241203757de3f5f25842c5cf0f295b8815c2c9aedf315f36a03c`
- Secondary official TD headway GTFS ZIP: `datasets/gtfs-hong-kong-en-260430.zip`
- Secondary official TD headway GTFS source URL: `https://static.data.gov.hk/td/pt-headway-en/gtfs.zip`

## Cartography

- Analysis/cartography CRS: `EPSG:2326`
- Cross-city web-style basemap: CARTO Positron with CARTO and OpenStreetMap
  attribution
- Final maps should keep data layers in local projected CRS for measurement and
  export high-resolution PNGs for the report.
