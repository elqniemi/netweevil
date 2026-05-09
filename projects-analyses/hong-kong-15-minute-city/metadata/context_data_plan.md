# Hong Kong Context Data Plan

This study intentionally keeps the non-OSM data stack small so the Hong Kong
extension remains comparable to the Groningen workflow.

## Acquired

| Layer | Local artifact | Use |
| --- | --- | --- |
| 2021 TPU and Subunit boundaries | `datasets/downloads/hong_kong/hk_2021_tpu_subunit_boundaries.gpkg` | Aggregation, polycentric summaries, population joins |
| 2024 land utilization raster grid | `datasets/downloads/hong_kong/hk_land_utilization_2024_rendered.tif` | Contextual land-use map overlay; coarse mixed-use interpretation |
| OZP land-use zonings | `datasets/downloads/hong_kong/hk_ozp_land_use_zonings.gpkg` | Planned zoning vs organic mixed-use argument |

Detailed source URLs, SHA-256 hashes, counts, and API notes are recorded in
`metadata/generated/hk_context_data_sources.json`.

## Already Available Or In Progress

| Layer | Local artifact | Use |
| --- | --- | --- |
| OSM extract | `datasets/hong-kong-260508.osm.pbf` | Streets, POIs, buildings, H3 origins, transit stops |
| GTFS | `datasets/gtfs-hong-kong-community-wheels-netweevil.zip` | Transit accessibility and polycentric travel windows |

## Groningen Context

The Groningen comparison context is already consolidated in
`datasets/downloads/groningen_data.gpkg` with these layers:

- `bag_adres`
- `buurten`
- `wijken`

Use this instead of redownloading CBS/BAG/PDOK context data unless a specific
missing attribute is needed later.

## Basemap

Use CARTO Positron for cross-city consistency. Keep attribution in report maps:
CARTO and OpenStreetMap contributors. For city-specific high-polish Groningen
maps, PDOK/BRT can still be used as a local authoritative alternative.
