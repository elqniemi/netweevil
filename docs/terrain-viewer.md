# Optional terrain in the 3D viewer

The viewer can render local Hong Kong DEM terrain beneath the surveyed network. Open **Style**, enable **3D**, then enable **DEM terrain**. Tilt the camera to see relief. The terrain uses the same exaggeration as the network; the x-ray basemap opacity also fades terrain, leaving station paths visible.

The setting is off by default and is remembered with the other map settings. Terrain is disabled while the viewer is in 2D. It does not change routing, source XYZ, altitude filters or exports. The current deck overlay draws through terrain so underground station paths remain inspectable.

## Prepare the Hong Kong source

The checked Hong Kong preparation produced: 3,342 PNG tiles, about 139 MiB, covering 113.8242–114.4441° E and 22.1380–22.5719° N. After building the CLI/frontend and obtaining the source raster, prepare terrain without importing graphs or rebuilding transfer tables:

```bash
scripts/setup_hong_kong.sh --terrain-only --skip-build --no-serve
```

For a different location of the same HK1980/HKPD source:

```bash
python3 scripts/prepare_hong_kong_terrain.py \
  --input /path/to/Whole_HK_DTM_5m.asc \
  --output-dir .netweevil/terrain/hong_kong_5m
```

The preparation requires GDAL's Python bindings and NumPy. It writes Terrarium PNG tiles at zooms 8 through 15, a provenance manifest, and content-versioned tile paths. The viewer reuses zoom-15 tiles at closer zooms. Repeating preparation reuses a complete matching version. Full Hong Kong setup also prepares terrain when the local DTM is present. Start or restart the API with the current executable, then refresh the console to discover it.

`GET /v1/terrain-sources` lists prepared sources. Tiles are served from `/v1/terrain/{source}/{version}/{z}/{x}/{y}.png` under `.netweevil/terrain/`. No external terrain account or token is needed. The control reports unavailable terrain when no prepared source exists.

## Data already available

Place the source at `/path/to/hong-kong-analysis/datasets/dtm/Whole_HK_DTM_5m.asc`. Generated rasters and tiles are not included in a clone. The ASCII header has 12,751 columns, 9,601 rows, 5 m cells, lower-left corner 799997.5/799997.5 and nodata -9999. Prefer the ASCII source because it retains its grid definition. Its filename and grid agree with LandsD's published product; a reproducible import should record its checksum and provenance rather than assume the existing local file matches a newly downloaded revision.

LandsD specifies HK1980 Grid coordinates and Hong Kong Principal Datum heights. The published accuracy is ±5 m at 90% confidence. The product specification describes imagery from 2014 and 2015, publication in 2017 and revision in March 2019. These dates matter when comparing it with newer stations or reclaimed land. The portal's resource-update date is not evidence that the terrain was surveyed again. [LandsD DTM specification](https://www.landsd.gov.hk/en/spatial-data/open-data/kf_dtm.html).

The dataset includes elevated roads, bridges and vegetation heights. Treat it as terrain context, with some objects above ground represented in the raster. It cannot reliably determine the cover above a station, bridge clearance, a walking surface inside a building, or whether a route is underground. [Official dataset description and download](https://data.gov.hk/en-data/dataset/hk-landsd-openmap-5m-grid-dtm/resource/620c4f4f-eac4-472f-9074-dffa2ad596fd).

The portal lists [the downloadable ZIP](https://www.landsd.gov.hk/landsd_psi_data/SMO/data/Whole_HK_DTM_5m.zip). No large download was needed for this investigation. Derived tiles should carry Lands Department, HKSAR Government and DATA.GOV.HK attribution, the source link, and a description of reprojection and resampling. Free reuse and redistribution are subject to the published [DATA.GOV.HK terms](https://data.gov.hk/en/terms-and-conditions); preserve those terms in the data manifest.

## Height rules

The prepared Hong Kong network stores longitude/latitude horizontally and metres above HKPD vertically. Reprojecting XY from EPSG:2326 to longitude/latitude did not turn Z into WGS84 ellipsoidal height. The local DTM can therefore be compared with those heights without an additional vertical shift, provided its documented HKPD reference is preserved during tiling.

For a common display exaggeration `E`, render a surveyed point at `E * source_z` and the terrain at `E * dem_z`. Do not add DEM elevation to surveyed Z. For example, a walkway at 50 m HKPD above terrain at 40 m HKPD is 10 m above that terrain; adding the terrain would incorrectly move it to 90 m. Keep exported geometry, altitude filters and reported elevations in original metres.

Unknown OSM/GTFS elevations remain unknown. A later option could drape explicitly surface-only, unsurveyed features onto the DEM for display, with `elevation_source=dem_estimate`. It must not apply to tunnels, bridges, indoor paths or interpolated train geometry. DEM sampling is a separate operation from importing surveyed heights and should never alter routing topology by default.

For a global DEM, first establish the contributing source and its vertical datum. RGB encoding says how to decode a number; it does not establish that number's height reference. HKPD is distinct from WGS84 ellipsoidal height. A conversion requires an appropriate height model and documented transformation, not a guessed constant or a horizontal CRS change. [Hong Kong Geodetic Survey height transformation guidance](https://www.geodetic.gov.hk/en/gi/height_transf.htm).

## Renderer integration

The installed frontend uses MapLibre 5.24.0 and deck.gl 9.4.0. `frontend/src/map/ThreeDOverlay.ts` creates `MapboxOverlay` with `interleaved: false`. MapLibre renders the basemap and optional terrain in one canvas; deck renders the actual XYZ network in another canvas above it. This is supported by the [MapboxOverlay rendering modes](https://deck.gl/docs/api-reference/mapbox/mapbox-overlay).

The installed `@deck.gl/mapbox/src/deck-utils.ts` already checks `map.getTerrain()` and, for MapLibre, copies `map.transform.elevation` into `viewState.position[2]`. MapLibre's installed `src/render/terrain.ts` includes terrain exaggeration in that camera-target elevation. Retain this integration rather than introducing a second manual camera offset. Our custom `MapView` near/far settings merge with the supplied camera state and preserve its position.

A small Node projection probe checked target elevations of 0, 100, 1,000 and 20,000 m at zoom 18/pitch 45. The corrected overlay retained each target position and matched an ordinary deck `MapView` in XY within 0.000001 pixel. This establishes compatibility of the existing view override with elevated camera targets. It does not replace a browser test with loaded terrain tiles.

The frontend integration registers a `raster-dem` source, then calls `map.setTerrain({ source: terrainSourceId, exaggeration: E })`. Disabling the option calls `map.setTerrain(null)`. Re-register sources after a basemap style change, and enable terrain only while 3D mode is active. MapLibre documents the source and exaggeration properties in its [terrain specification](https://maplibre.org/maplibre-style-spec/terrain/) and provides a [terrain and hillshade example](https://maplibre.org/maplibre-gl-js/docs/examples/3d-terrain/).

Preserve the explicit overlay near/far settings and the full-feed station cache. A ground-plane viewport query is still insufficient for elevated or underground geometry at a pitched camera. Terrain must not reintroduce the disappearance or vertical-path dash artifacts fixed in the current viewer.

## Underground viewing and controls

The **DEM terrain** checkbox sits in the 3D controls, with a source selector when multiple sources are available. It defaults off for saved views and shows source resolution and datum. Terrain and network use the same vertical exaggeration. At large exaggerations the mountains will dominate the view; changing the shared scale retains the correct relative heights.

Keep the current x-ray opacity control. It fades the MapLibre canvas while the deck canvas remains visible, so terrain and the basemap can become translucent together. Paths will remain visible through terrain even when basemap opacity is 100%, because the two canvases do not share a depth buffer. Label this as an x-ray network view. It is useful for inspection but does not simulate physical occlusion.

Independent terrain and network exaggeration could be an advanced illustrative option. Use `Link terrain and network scale` enabled by default; unlinking would expose separate `Terrain exaggeration` and `Network exaggeration` controls. Then the displayed separation is `network_scale * source_z - terrain_scale * dem_z`, which can move a path visually through the terrain. The UI must state that relative heights are distorted while scales differ. Avoid that complexity in the first implementation. A terrain-relative readout such as `approximately 12 m below DEM` would also need the DEM's uncertainty and object-height caveat; the existing HKPD elevation readout should remain primary.

A later solid-ground mode could investigate deck/MapLibre interleaved rendering or a terrain mesh inside deck's own scene. Both require new depth, camera and transparency tests. Simply setting `interleaved: true` would change underground visibility and the custom clipping behavior. It is outside the first terrain addition.

## Preparing local tiles

The preparation script converts the ASCII raster on the server rather than loading it into the browser. It preserves HKPD heights while reprojecting the HK1980 horizontal coordinates. These are the tiling requirements:

1. Record source checksum, raster dimensions, geotransform, bounds, nodata, acquisition metadata and datum. Verify the ASCII corner convention and row orientation against known coordinates.
2. Reproject scalar heights horizontally from HK1980 Grid to a Web Mercator tile grid using GDAL. Use floating-point heights and nodata-aware interpolation. Preserve HKPD Z; do not request an implicit vertical transformation.
3. Generate every overview and tile in scalar metres before encoding RGB. Encode 256-pixel PNG tiles as Terrarium. At Hong Kong's latitude, zoom 15 has about 4.4 m ground pixels and is a sensible upper tile zoom for this 5 m source. Higher viewer zooms can reuse those tiles.
4. Check nodata and coastline handling before publishing. MapLibre 5.24's DEM decoder reads RGB without treating alpha as missing elevation. Transparent pixels therefore do not safely represent holes. Never encode -9999 as real terrain. Keep an explicit coverage mask, omit wholly uncovered tiles, and define a documented edge-fill policy for partially covered tiles. Any display fill outside valid coverage remains unavailable for elevation sampling. Do not equate missing coverage with zero metres HKPD or ocean level.
5. Write TileJSON and a manifest containing source/projection/datum, scalar resampling method, encoding, coverage, zoom range, checksum, attribution and preparation version. Verify decoded sample heights and adjacent tile boundaries. Never resize encoded RGB with ordinary bilinear image resampling, which can corrupt heights at channel carries.

Terrarium uses `(R * 256 + G + B / 256) - 32768` metres. MapLibre accepts this encoding through `raster-dem` and supports source bounds and maximum tile zoom. The tile format's precision is much finer than this DEM's real accuracy. [MapLibre raster DEM specification](https://maplibre.org/maplibre-style-spec/sources/#raster-dem), [Mapzen terrain encoding](https://www.mapzen.com/blog/terrain-tile-service/).

The same-origin API serves registered terrain tiles from local state with immutable caching keyed by the prepared-data version. Terrain discovery exposes display metadata to the frontend. No source filesystem path is supplied by the browser.

## Global fallback

For optional global context, AWS hosts public Mapzen terrain tiles without an AWS account at `https://elevation-tiles-prod.s3.amazonaws.com/terrarium/{z}/{x}/{y}.png`. The service combines several elevation sources and has source-specific attribution requirements. Its documentation provides an attribution list; do not label the combined dataset uniformly public domain or assume one resolution everywhere. [AWS dataset registry](https://registry.opendata.aws/terrain-tiles/), [provider attribution](https://github.com/tilezen/joerd/blob/master/docs/attribution.md), [contributing sources](https://github.com/tilezen/joerd/blob/master/docs/data-sources.md).

Use that as a selectable contextual background only until the local contributing data and vertical reference are established. For Hong Kong, the existing HKPD raster is the stronger starting point. Do not blend it into a global DEM at the boundary until their height references agree. An external tile source also needs browser CORS and availability checks, a bounded cache policy and visible attribution. This investigation did not perform those runtime checks.

## Validation

- Import a small known raster fixture with negative heights, nodata and cell boundaries. Decode generated tiles and compare with the scalar reprojection, including channel-carry values and seams.
- Verify that enabling terrain and changing exaggeration leaves surveyed exports and routing results byte-for-byte unchanged. Test camera synchronization at terrain elevations of 0, 100 and 1,000 m, including 20x exaggeration.
- In the existing Helium browser, inspect a hilly outdoor route, a bridge, and the Sai Ying Pun/Admiralty underground station geometry. Check zoom 10 through the viewer maximum and pitch 0/45/60, including x-ray, altitude slices, rapid zoom, terrain off/on and basemap changes.
- Confirm paths and all station geometry remain available across camera movement. Record that the overlay is intentionally visible through terrain. Missing tiles must leave the network usable and must not generate pits or spikes.
- Compare a few surveyed outdoor points with nearby raster heights and report the residuals. Use this to reveal datum or pixel-placement mistakes, not to force survey elevations onto the older, less accurate raster.

The first implementation uses the complete local Hong Kong raster. A global terrain source, DEM-based draping of unsurveyed roads and physically occluded underground geometry remain separate future options.
