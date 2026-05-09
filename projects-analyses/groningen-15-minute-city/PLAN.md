# Organic 15-Minute Cities: Groningen and Hong Kong Analysis Plan

## Purpose

Use netweevil and supporting spatial analysis to test a sharper claim about the
15-minute city:

> The 15-minute city is weak when treated as a planning checklist or
> blank-canvas design target. It is strong when used diagnostically to reveal
> whether an existing city already supports rich, mixed, optional daily life.
> Groningen shows that the condition already exists organically, especially by
> bicycle, but also that new planned neighbourhoods risk becoming technically
> complete yet experientially thin. Hong Kong shows a stronger polycentric
> version: daily needs are nearby, but the city still pulls people beyond their
> local bubble through transit, density, and layered centres.

The analysis should therefore not stop at binary compliance. The core question
is:

> Where do Groningen and Hong Kong provide not just 15-minute access, but mixed,
> varied, optional, open-ended daily life?

The Groningen baseline is already expected to pass in most residential areas.
The interesting result is whether that access is organic and rich, or whether it
collapses into constrained commute-shop-entertain corridors. Hong Kong is the
comparative case for a polycentric, transit-oriented, high-density model that
can satisfy local needs while still enticing people beyond their immediate
activity space.

## Study Layout

Keep the Groningen package here:

```text
projects-analyses/groningen-15-minute-city/
  PLAN.md
  README.md
  profiles/
  requests/
    origins/
    destinations/
    service_areas/
    route_batches/
    transit/
  experiments/
  outputs/
    raw/
    processed/
    maps/
      gis/
      png/
    plots/
      png/
    reports/
  scripts/
  metadata/
```

Add the Hong Kong comparative package using the same structure:

```text
projects-analyses/hong-kong-polycentric-15-minute-city/
  PLAN.md
  README.md
  profiles/
  requests/
  outputs/
  scripts/
  metadata/
```

Shared comparisons should live in:

```text
projects-analyses/organic-15-minute-city-comparison/
  outputs/
    maps/
    plots/
    reports/
  scripts/
  metadata/
```

## Local Data Sources

### Groningen

Use the current netweevil/OSM assets plus the new local GeoPackage:

- Full OSM extract: `datasets/groningen-260508.osm.pbf`
- Routing/admin extract: `datasets/groningen-260508-routing-admin.osm.pbf`
- Admin-only extract: `datasets/groningen-260508-admin.osm.pbf`
- Existing routing dataset: `groningen_2026_05_08`
- Existing OpenOV feed: `openov_nl_2026_05_09`
- CBS/BAG/supporting layers: `datasets/downloads/groningen_data.gpkg`
  - `bag_adres`: BAG address points with `pandbouwjaar`, `oppervlakteverblijfsobject`, `verblijfsobjectgebruiksdoel`
  - `buurten`: CBS neighbourhoods with population, business, housing, income, service-distance, and post-2000 housing fields
  - `wijken`: CBS district aggregation

Do not add more Groningen data unless a concrete metric cannot be computed from
these files. Planned/new neighbourhood boundaries may be a small hand-curated
GeoJSON if official polygons are not already present.

### Hong Kong

Use the data already present locally:

- OSM extract: `datasets/hong-kong-260508.osm.pbf`
- GTFS feeds:
  - `datasets/gtfs-hong-kong-en-260430.zip`
  - `datasets/gtfs-hong-kong-en-260430-netweevil.zip`
  - `datasets/gtfs-hong-kong-community-wheels.zip`
  - `datasets/gtfs-hong-kong-community-wheels-netweevil.zip`
- TPU/Subunit boundaries:
  - `datasets/downloads/hong_kong/hk_2021_tpu_subunit_boundaries.gpkg`
  - layer `hk_2021_tpu_subunit_boundaries`
- Population table/archive:
  - `datasets/downloads/LTPUG_21C.zip`
- OZP zoning:
  - `datasets/downloads/hong_kong/hk_ozp_land_use_zonings.gpkg`
  - layer `hk_ozp_land_use_zonings`
- Land utilization raster:
  - `datasets/downloads/hong_kong/hk_land_utilization_2024_rendered.tif`
  - `datasets/downloads/hong_kong/hk_land_utilization_2024_rendered.vrt`

Do not expand the HK data stack yet. The first comparative pass should be OSM +
GTFS + TPU population + OZP + land utilization. Add 3D pedestrian network, stairs,
slopes, estate boundaries, or workplace data only after the basic comparison is
working.

## Projection and Cartography Rules

Analysis and final cartography must use local projected CRSs:

- Groningen: `EPSG:28992`
- Hong Kong: `EPSG:2326`

H3 cells may be generated in WGS84 because H3 is geodesic, but every area,
density, length, cartographic join, and final map export should be reprojected to
the local CRS.

Basemaps:

- Groningen: prefer PDOK/BRT or a muted local basemap in `EPSG:28992`; CARTO
  Positron is acceptable for consistent cross-city styling.
- Hong Kong: use CARTO Positron or a muted OSM-vector-derived basemap unless a
  clean HK government basemap is already available locally.

Final deliverables:

- High-resolution PNG maps and plots.
- GeoPackage layers for GIS review.
- GeoJSON only as web/interchange output.
- SVG only as an internal render intermediate or final print vector if explicitly
  needed.

Maps must use muted basemaps, legible labels, consistent legends, clear
non-residential/water treatment, and restrained annotation.

## Existing Groningen Baseline To Keep

The current Groningen analysis already provides the accessibility foundation:

- H3 resolution 9 residential origin grid.
- Destination tables for food retail, education, healthcare, public services,
  parks/recreation, social life, transit stops, and bicycle support.
- `netweevil analyze accessibility` reduced accessibility for walking, cycling,
  and car.
- Strict/relaxed 15-minute scores.
- Threshold, scenario, route-quality, neighbourhood, transit-window, and transit
  service-bundle sample outputs.
- PNG map/plot renderer.

Do not throw this away. Extend it with quality-of-life, mixed-use, optionality,
centrality, corridor, and organic/planned metrics.

## Required New Measures

### 1. Mixed-Use Intensity

Measure how many different useful activities are present locally, not just
whether the nearest required category is reachable.

For each H3 cell and neighbourhood, compute:

- POI count by category.
- Total POI count.
- Category richness: number of active categories.
- Essential-service richness: baseline categories present within the local area.
- Extended daily-life richness: baseline + social life + bicycle support.
- Business/employment proxy from `buurten` where available:
  - `aantal_bedrijfsvestigingen`
  - sector fields such as `aantal_bedrijven_handel_en_horeca`,
    `aantal_bedrijven_overheid_onderwijs_en_zorg`,
    `aantal_bedrijven_cultuur_recreatie_overige`

Outputs:

- `outputs/processed/mixed_use_h3.csv`
- `outputs/processed/mixed_use_neighbourhoods.csv`
- `outputs/maps/gis/groningen_mixed_use_h3.gpkg`
- `outputs/maps/png/groningen_mixed_use_diversity_h3.png`

### 2. Diversity Score

Compute entropy/evenness across categories:

```text
entropy = -sum(p_i * ln(p_i))
evenness = entropy / ln(category_count)
```

Use both raw POI counts and reachable-within-15-min counts. Report the difference
between "many services nearby" and "many types of services reachable."

Outputs:

- `outputs/processed/diversity_scores_h3.csv`
- `outputs/maps/png/groningen_diversity_evenness_h3.png`

### 3. Optionality Score

The current analysis records destination counts within 5/10/15/20 minutes. Use
that to measure choice:

```text
optionality_score = mean(log1p(destinations_within_900s) by required category)
minimum_optionality = min(destinations_within_900s across required categories)
```

This distinguishes places with one barely reachable service from places with
many plausible alternatives.

Outputs:

- `outputs/processed/optionality_scores_h3.csv`
- `outputs/maps/png/groningen_optionality_h3.png`

### 4. Centrality Gradient

Compare mixed-use intensity, diversity, optionality, and 15-minute score against
distance from the centre and against CBS neighbourhood type.

Initial Groningen centre point:

- Grote Markt / city centre: approximately lon `6.5683`, lat `53.2194`

For each H3 cell:

- Distance to centre in metres in `EPSG:28992`.
- Ring band: 0-1 km, 1-2 km, 2-3 km, 3-5 km, 5+ km.
- Neighbourhood assignment from `buurten`.
- Newer-planned proxy:
  - share of post-2000 housing from `percentage_bouwjaarklasse_vanaf_2000`
  - address/building-year median from `bag_adres.pandbouwjaar`
  - optional manually curated planned-area polygon.

Outputs:

- `outputs/processed/centrality_gradient_h3.csv`
- `outputs/plots/png/groningen_centrality_gradient.png`
- `outputs/maps/png/groningen_centrality_mixed_use_h3.png`

### 5. Activity-Space Constraint

Use netweevil service areas with edge-distance output. For each representative
origin and selected H3 samples, compute:

- Reachable network edge length within 5/10/15/20 minutes.
- Reachable service categories within each band.
- Effective activity-space size:
  - network edge length
  - polygon area if generated
  - service count per reachable kilometre

Interpretation:

- High completeness + low reachable network variety = constrained local bubble.
- High completeness + high reachable network variety = open-ended 15-minute city.

Outputs:

- `outputs/processed/activity_space_service_areas.csv`
- `outputs/maps/gis/activity_space_edges.gpkg`
- `outputs/maps/png/groningen_activity_space_constraint.png`

### 6. Daily-Life Corridor Concentration

Build representative route bundles from origins to nearest food, work/education,
social/culture, and recreation destinations. Use route geometries and segment
breakdowns to compute:

- Repeated edge share across a daily bundle.
- Corridor concentration index:

```text
corridor_concentration = repeated_route_length_m / total_bundle_route_length_m
```

- Whether a neighbourhood's daily life funnels through one or two corridors.

Use current route-batch logic, but add route bundles rather than one route per
category only.

Outputs:

- `requests/route_batches/groningen_daily_life_corridors.json`
- `outputs/raw/groningen_daily_life_corridors.gpkg`
- `outputs/processed/corridor_concentration.csv`
- `outputs/maps/png/groningen_daily_life_corridors.png`

### 7. Polycentricity Index

Identify local centres and measure how many centres are practically reachable.

For Groningen:

- Use clusters of high POI density + transit stops + business density.
- Expect a dominant centre with secondary centres such as Zernike, station area,
  Helpman, Paddepoel/Vinkhuizen, Lewenborg/Beijum, Hoogkerk.

For Hong Kong:

- Use MTR/major transit nodes, TPU population density, OZP mixed commercial
  zones, and POI clusters.
- Expect many strong centres.

Metrics:

- Number of centres reachable within 15/30 minutes.
- Share of daily needs reachable locally.
- Enticement score: reachable centres beyond the nearest centre within 30-45
  minutes.

Outputs:

- `outputs/processed/local_centres.csv`
- `outputs/processed/polycentricity_h3.csv`
- `outputs/maps/png/groningen_polycentricity_h3.png`
- `outputs/maps/png/hong_kong_polycentricity_h3.png`

### 8. Boring-New-Neighbourhood Test

This is the key thesis test for Groningen.

Compare neighbourhoods by:

- 15-minute strict score.
- Mixed-use intensity.
- Diversity/evenness.
- Optionality.
- Centrality distance.
- Building/address age:
  - median `pandbouwjaar`
  - share post-2000 housing from CBS.
- Business density and sector diversity from `buurten`.
- Street/network permeability:
  - node density
  - intersection density
  - block size proxy
  - reachable edge length within 15 minutes
- Daily-life corridor concentration.

Classification:

- `old_mixed`
- `central_mixed`
- `planned_or_new`
- `edge_suburban`
- `campus_or_specialized`

Start with rule-based classification from CBS/BAG fields, then optionally refine
with a small hand-curated `metadata/groningen_neighbourhood_typology.yml`.

Outputs:

- `outputs/processed/boring_new_neighbourhood_test.csv`
- `outputs/plots/png/groningen_old_vs_new_neighbourhoods.png`
- `outputs/maps/png/groningen_old_mixed_vs_new_planned.png`

## Hong Kong Comparative Measures

Use the same core model, but adapt the interpretation:

- Walking + transit should matter more than cycling.
- Completeness alone is expected to be high in many places.
- The important question is whether local completeness coexists with access to
  multiple centres.

HK outputs:

1. H3 or TPU/Subunit residential origin grid.
2. OSM POI destination categories equivalent to Groningen.
3. MTR/TOD access:
   - distance/time to nearest rail station or major stop
   - transit service frequency if available from GTFS
4. Local service completeness:
   - strict and relaxed 15-minute walking/local-transit scores
5. Polycentric access:
   - number of centres reachable within 15/30/45 minutes
6. Local-but-outward pull:
   - local completeness score
   - beyond-local centre reachability score
   - final "15 minutes locally, still drawn outward" index

HK output files:

- `outputs/processed/hk_accessibility_scores.csv`
- `outputs/processed/hk_mixed_use_h3.csv`
- `outputs/processed/hk_tod_access.csv`
- `outputs/processed/hk_polycentricity.csv`
- `outputs/maps/png/hk_polycentric_15m_access.png`
- `outputs/maps/png/hk_mtr_tod_local_completeness.png`
- `outputs/maps/png/hk_local_but_outward_pull.png`

## Updated Report Structure

The final report should not be led by "which cells pass." Use this structure:

1. **Argument**
   - 15-minute cities already exist in places like Groningen.
   - The planning question is quality, optionality, mixture, and openness.
   - Forced blank-canvas 15-minute districts risk becoming technically complete
     but boring.

2. **Groningen Baseline**
   - Show cycling/walking/car strict scores.
   - State that Groningen mostly passes, especially by bike.

3. **Beyond Compliance**
   - Mixed-use diversity.
   - Optionality.
   - Centrality gradient.
   - Activity-space constraint.
   - Corridor concentration.

4. **Old Mixed vs New Planned Groningen**
   - Use BAG/CBS/neighbourhood typology.
   - Compare old central/organic areas to post-2000/planned/new edge areas.

5. **Hong Kong Comparative Case**
   - Polycentric local completeness.
   - TOD access.
   - Ability to meet needs nearby while still being pulled outward.

6. **Comparison**
   - Groningen: organic cycling 15-minute city, with centrality and planned-edge
     weaknesses.
   - Hong Kong: denser, more polycentric, stronger outward optionality if one has
     the means.

7. **Conclusion**
   - 15-minute city as a diagnostic tool is useful.
   - 15-minute city as a checklist design ideology is weak.

## Headline Map and Plot Deliverables

### Groningen

1. Strict 15-minute score by mode.
2. Mixed-use diversity by H3 cell.
3. Optionality score by H3 cell.
4. Old mixed neighbourhoods vs new planned neighbourhoods.
5. Centrality gradient showing where mixed use collapses toward the centre.
6. Activity-space constraint map from service-area edge-distance outputs.
7. Daily-life corridor concentration map.
8. Cycling advantage over walking and car.
9. Route-quality map for representative cycling route bundles.
10. Sensitivity bands for threshold, speed, snap, bundle definition, and
    weighting.

### Hong Kong

1. Polycentric 15-minute access map.
2. MTR/TOD access plus local service completeness.
3. "15 minutes locally, still drawn outward" map.
4. TPU/Subunit mixed-use and density context.
5. Transit time-window comparison.

### Comparative

1. Groningen vs Hong Kong organic/polycentric richness figure.
2. Compliance vs optionality scatter plot.
3. Local completeness vs outward pull plot.
4. Planned/checklist thinness vs organic richness summary chart.

## Implementation Modifications Needed

### A. Groningen data integration

Add scripts:

- `scripts/integrate_groningen_cbs_bag.py`
  - Read `datasets/downloads/groningen_data.gpkg`.
  - Reproject H3 cells to `EPSG:28992`.
  - Assign H3 cells to `buurten` and `wijken`.
  - Aggregate `bag_adres` by H3 and neighbourhood:
    - address count
    - median `pandbouwjaar`
    - post-2000 address share
    - residential floor-area proxy from `oppervlakteverblijfsobject`
  - Export `outputs/processed/groningen_h3_cbs_bag.csv`.

- `scripts/build_groningen_organic_metrics.py`
  - Consume current accessibility outputs, POI tables, CBS/BAG join, and
    service-area edge-distance outputs.
  - Produce mixed-use, diversity, optionality, centrality, and boring-new metrics.

- `scripts/build_daily_life_corridor_batch.py`
  - Generate route bundles for representative origins.
  - Include food, education/work proxy, social/culture, recreation, and transit.

### B. Netweevil service-area usage

Use the new service-area edge-distance output to avoid approximating activity
space from polygons alone.

Required service-area output fields:

- origin ID
- edge ID
- threshold band
- travel time to edge or along edge
- distance along edge
- mode/profile

Derived outputs:

- reachable edge length by threshold
- reachable service count by threshold
- category richness by service-area band
- activity-space constraint score

### C. Hong Kong pipeline

Create a mirrored HK extraction and analysis pipeline:

- Import `datasets/hong-kong-260508.osm.pbf` as a routing dataset.
- Import one HK GTFS feed first; prefer the netweevil-normalized feed if that is
  the intended local format.
- Extract OSM POIs into the same categories as Groningen.
- Use TPU/Subunit and population archive for aggregation and weights.
- Use OZP + land utilization for mixed-use and planned-use context.
- Run walking/local-transit accessibility first; add car only if useful.

### D. Map rendering

Replace diagnostic renderer assumptions with a cartographic pipeline:

- Generate projected GeoPackages first.
- Render final maps from projected layers.
- Use consistent style files per city.
- Use a light basemap:
  - PDOK/BRT or CARTO Positron for Groningen.
  - CARTO Positron or muted OSM vector basemap for HK.
- Export `3000px+` PNGs for report figures.
- Keep source layers beside each PNG.

## Success Criteria For The Revised Analysis

The revised analysis is ready when:

- Groningen report leads with the thesis framing, not only binary completeness.
- Groningen H3 cells are joined to CBS/BAG neighbourhood/address data.
- Mixed-use, diversity, optionality, centrality, activity-space, corridor, and
  old-vs-new metrics exist for Groningen.
- Service-area edge-distance outputs are used for activity-space metrics.
- HK has the same basic POI/accessibility/mixed-use/polycentric pipeline.
- HK uses TPU/Subunit and OZP/land-utilization context.
- Final maps are high-resolution PNGs with proper local projections and muted
  basemaps.
- The report includes a Groningen-HK comparison figure that directly addresses:
  organic richness, polycentricity, optionality, planned thinness, and outward
  pull.
