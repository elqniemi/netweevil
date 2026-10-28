# Terrain in the 3D viewer

The web console can show local DEM terrain under the surveyed network. This
guide shows how to prepare the Hong Kong terrain tiles and how to show them.

Terrain display does not change routing, source XYZ, altitude filters or
exports. To sample a DEM into routing data (node heights and edge grades),
import the dataset with `--dem`. See
[Regions and elevation](regions-and-elevation.md#elevation).

## Prerequisites

- A built CLI and frontend.
- GDAL Python bindings and NumPy.
- The Lands Department 5 m DTM:
  [ZIP](https://www.landsd.gov.hk/landsd_psi_data/SMO/data/Whole_HK_DTM_5m.zip),
  [dataset description](https://data.gov.hk/en-data/dataset/hk-landsd-openmap-5m-grid-dtm/resource/620c4f4f-eac4-472f-9074-dffa2ad596fd).

## Prepare the Hong Kong terrain

1. Download the ZIP and extract `Whole_HK_DTM_5m.asc`.
2. Prepare the tiles. This does not import graphs or rebuild transfer tables:

    ```bash
    scripts/setup_hong_kong.sh --terrain-only --skip-build --no-serve \
      --dem /path/to/Whole_HK_DTM_5m.asc
    ```

    Alternatively, run the preparation script directly:

    ```bash
    python3 scripts/prepare_hong_kong_terrain.py \
      --input /path/to/Whole_HK_DTM_5m.asc \
      --output-dir .netweevil/terrain/hong_kong_5m
    ```

3. Start or restart the API.
4. Refresh the console to discover the source.

The preparation writes Terrarium PNG tiles at zooms 8 to 15, a provenance
manifest and content-versioned tile paths. For all of Hong Kong this is about
3,300 tiles and 140 MiB. The viewer uses the zoom-15 tiles at closer zooms. A
repeated preparation uses a complete matching version again.

Full Hong Kong setup also prepares terrain when the DTM is in the
`datasets/dtm/` folder of the source directory. See the
[Hong Kong tutorial](hong-kong-tutorial.md).

## Show the terrain

1. Open **Style**.
2. Enable **3D**.
3. Enable **DEM terrain**.
4. Tilt the camera to see relief.

The setting is off by default. The console remembers it with the other map
settings. Terrain is off while the viewer is in 2D.

- The terrain uses the same vertical exaggeration as the network.
- The x-ray basemap opacity also fades the terrain. Station paths stay
  visible.
- The network overlay draws through the terrain. Thus you can inspect
  underground station paths.

## Terrain endpoints

| Endpoint | Purpose |
| --- | --- |
| `GET /v1/terrain-sources` | Lists the prepared sources and their tile URLs |
| `GET /v1/terrain/{source}/{version}/{z}/{x}/{y}.png` | Immutable local DEM tiles from `.netweevil/terrain/` |

No external terrain account or token is necessary. The control reports that
terrain is unavailable when no prepared source exists.

## Heights and accuracy

The DTM uses HK1980 Grid coordinates and Hong Kong Principal Datum (HKPD)
heights. This is the same vertical reference as the surveyed Hong Kong
network. The viewer shows surveyed points at their own height and terrain at
its own height. The shared exaggeration scales both. The viewer never adds
DEM height to surveyed coordinates.

LandsD publishes an accuracy of ±5 m at 90% confidence, from 2014–2015
imagery ([specification](https://www.landsd.gov.hk/en/spatial-data/open-data/kf_dtm.html)).

!!! warning
    The raster includes elevated roads, bridges and vegetation. Use it as
    terrain context only. It cannot tell if a route is underground or how
    much cover is above a station.

## Attribution

Derived tiles must carry Lands Department, HKSAR Government and DATA.GOV.HK
attribution. Reuse is subject to the
[DATA.GOV.HK terms](https://data.gov.hk/en/terms-and-conditions).

## Related pages

- [Regions and elevation](regions-and-elevation.md).
- [Hong Kong tutorial](hong-kong-tutorial.md).
