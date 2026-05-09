# Groningen 15-Minute City Study

This directory contains the reproducible study package for the Groningen
15-minute city analysis described in `PLAN.md`.

## Workflow

1. Import the routing dataset and compile profiles:

   ```bash
   cargo run --release -p netweevil-cli -- dataset import datasets/groningen-260508-routing-admin.osm.pbf --name groningen_2026_05_08 --acceleration-profile balanced
   cargo run --release -p netweevil-cli -- profile compile --dataset groningen_2026_05_08 --profile projects-analyses/groningen-15-minute-city/profiles/cycling_15min_groningen_v1.yml
   cargo run --release -p netweevil-cli -- profile compile --dataset groningen_2026_05_08 --profile examples/profiles/pedestrian_research_v1.yml
   cargo run --release -p netweevil-cli -- profile compile --dataset groningen_2026_05_08 --profile examples/profiles/car_research_v3.yml
   ```

2. Extract origins and destination point sets from the full OSM extract:

   ```bash
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/extract_osm_features.py
   ```

   The extractor requires `osmium` on `PATH`. It writes CSV point sets under
   `requests/`, GeoJSON/GPKG-ready source layers under `outputs/processed/`, and
   per-layer metadata under `metadata/generated/`. The baseline experiment uses
   H3 resolution 9 origins in `requests/origins/residential_h3_r9_points.csv`;
   the full
   `requests/origins/residential_points.csv` table is retained for audits and
   later full-resolution runs.

3. Run the reduced baseline accessibility campaign:

   ```bash
   cargo run --release -p netweevil-cli -- analyze accessibility \
     --dataset groningen_2026_05_08 \
     --profile projects-analyses/groningen-15-minute-city/profiles/cycling_15min_groningen_v1.yml \
     --origins projects-analyses/groningen-15-minute-city/requests/origins/residential_h3_r9_points.csv \
     --destination food_retail=projects-analyses/groningen-15-minute-city/requests/destinations/food_retail.csv \
     --destination education=projects-analyses/groningen-15-minute-city/requests/destinations/education.csv \
     --destination healthcare=projects-analyses/groningen-15-minute-city/requests/destinations/healthcare.csv \
     --destination public_services=projects-analyses/groningen-15-minute-city/requests/destinations/public_services.csv \
     --destination parks_recreation=projects-analyses/groningen-15-minute-city/requests/destinations/parks_recreation.csv \
     --destination transit_stops=projects-analyses/groningen-15-minute-city/requests/destinations/transit_stops.csv \
     --destination social_life=projects-analyses/groningen-15-minute-city/requests/destinations/social_life.csv \
     --destination bicycle_support=projects-analyses/groningen-15-minute-city/requests/destinations/bicycle_support.csv \
     --out projects-analyses/groningen-15-minute-city/outputs/raw/cycling_accessibility_reduced.csv
   cargo run --release -p netweevil-cli -- analyze service-area \
     --dataset groningen_2026_05_08 \
     --profile projects-analyses/groningen-15-minute-city/profiles/cycling_15min_groningen_v1.yml \
     --request projects-analyses/groningen-15-minute-city/requests/service_areas/cycling_neighbourhood_centres_5_10_15_20.json \
     --out projects-analyses/groningen-15-minute-city/outputs/raw/cycling_neighbourhood_service_areas.gpkg
   ```

   Repeat `analyze accessibility` for walking and car with the six baseline
   categories. This command is the preferred operational path: it performs one
   bounded network expansion per H3 origin and mode, then reduces all destination
   categories to nearest-service times and threshold counts without
   materializing millions of OD cells. The old monolithic experiment file and
   `scripts/run_matrix_chunks.py` remain available for small validation samples,
   but the full OD campaign is too slow for the main study.

   For a fast verification run, build sampled point sets and run the smoke
   experiment first:

   ```bash
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/build_sample_point_sets.py
   cargo run --release -p netweevil-cli -- experiment run projects-analyses/groningen-15-minute-city/experiments/smoke_accessibility.yml
   ```

4. Summarize reduced accessibility outputs into nearest-service and 15-minute completeness
   tables:

   ```bash
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/summarize_accessibility.py
   ```

   Join score tables back onto H3 geometry for map production:

   ```bash
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/export_accessibility_geojson.py
   ```

   Export GIS-ready map layers and compute sensitivity outputs:

   ```bash
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/export_gis_map_layers.py
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/build_threshold_sensitivity.py --thresholds-s 600,720,900,1200
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/build_sensitivity_scenarios.py
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/build_neighbourhood_aggregates.py
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/render_png_maps_plots.py
   ```

5. Build and run a route-quality batch from the nearest required services:

   ```bash
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/build_route_quality_batch.py --max-origins 50
   cargo run --release -p netweevil-cli -- analyze route-batch \
     --dataset groningen_2026_05_08 \
     --profile projects-analyses/groningen-15-minute-city/profiles/cycling_15min_groningen_v1.yml \
     --requests projects-analyses/groningen-15-minute-city/requests/route_batches/cycling_nearest_required_services.json \
     --out projects-analyses/groningen-15-minute-city/outputs/raw/cycling_nearest_required_services.gpkg
   ogr2ogr -f GeoJSON \
     projects-analyses/groningen-15-minute-city/outputs/maps/gis/cycling_route_quality_routes.geojson \
     projects-analyses/groningen-15-minute-city/outputs/raw/cycling_nearest_required_services.gpkg \
     routes
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/summarize_route_quality.py
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/render_accessibility_report.py
   ```

6. Run transit time-window spot checks after importing the OpenOV feed:

   ```bash
   cargo run --release -p netweevil-cli -- analyze transit-route \
     --feed openov_nl_2026_05_09 \
     --request projects-analyses/groningen-15-minute-city/requests/transit/weekday_am_peak_centrum_to_zernike.json \
     --out projects-analyses/groningen-15-minute-city/outputs/raw/transit_weekday_am_peak_centrum_to_zernike.json
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/build_transit_neighbourhood_batch.py
   cargo run --release -p netweevil-cli -- analyze transit-batch \
     --feed openov_nl_2026_05_09 \
     --requests projects-analyses/groningen-15-minute-city/requests/transit/neighbourhood_centres_to_zernike_batch.json \
     --out projects-analyses/groningen-15-minute-city/outputs/raw/transit_neighbourhood_centres_to_zernike_batch.json
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/summarize_transit_batch.py
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/build_transit_category_bundle_batch.py
   cargo run --release -p netweevil-cli -- analyze transit-batch \
     --feed openov_nl_2026_05_09 \
     --requests projects-analyses/groningen-15-minute-city/requests/transit/neighbourhood_service_bundle_sample.json \
     --out projects-analyses/groningen-15-minute-city/outputs/raw/transit_neighbourhood_service_bundle_sample.json
   uv run --project projects-analyses/groningen-15-minute-city \
     python projects-analyses/groningen-15-minute-city/scripts/summarize_transit_category_bundle.py
   ```

## Output Conventions

Reduced accessibility outputs are expected to use this filename shape:

```text
outputs/raw/{mode}_accessibility_reduced.csv
```

The summarizer also still recognizes `outputs/raw/{mode}_{category}_matrix.csv`
for smoke and validation matrices. It recognizes `walking`, `cycling`, `car`,
and `transit` modes and the service categories defined in `metadata/study.yml`.

## Data Provenance

Every generated analysis layer should have a matching JSON metadata file in
`metadata/generated/` with source path, source SHA-256, extraction command,
timestamp, CRS, geometry type, record count, and tag filters.
