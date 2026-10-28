#!/usr/bin/env python3
"""Prepare local HKPD terrain tiles. Requires GDAL's Python bindings and NumPy."""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import struct
import tempfile
import zlib

import numpy as np
from osgeo import gdal, osr

WORLD = 20037508.342789244
TILE_SIZE = 256
NODATA = -9999.0
PREPARATION_VERSION = 1


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def terrarium_encode(heights: np.ndarray) -> np.ndarray:
    """Encode scalar metres, retaining negative heights and 1/256 m precision."""
    if not np.isfinite(heights).all():
        raise ValueError("Terrain contains nonfinite heights")
    encoded = np.rint((heights.astype(np.float64) + 32768) * 256)
    if np.any(encoded < 0) or np.any(encoded > 16777215):
        raise ValueError("Terrain height is outside Terrarium range")
    encoded = encoded.astype(np.uint32)
    return np.stack(((encoded >> 16) & 255, (encoded >> 8) & 255, encoded & 255), axis=-1).astype(np.uint8)


def fill_display_nodata(heights: np.ndarray, valid: np.ndarray) -> np.ndarray:
    """Extend closest valid sample along each row, then closest populated row.

    This is a deterministic display boundary treatment, not surveyed elevation.
    The caller retains the original valid mask for coverage accounting.
    """
    if heights.shape != valid.shape or heights.ndim != 2:
        raise ValueError("Heights and validity must have matching 2D shapes")
    if not valid.any():
        raise ValueError("Raster contains no valid terrain samples")
    output = heights.copy()
    columns = np.arange(heights.shape[1])
    populated = np.flatnonzero(valid.any(axis=1))
    for y in populated:
        if valid[y].all():
            continue
        left = np.maximum.accumulate(np.where(valid[y], columns, -heights.shape[1]))
        right = np.minimum.accumulate(np.where(valid[y], columns, heights.shape[1] * 2)[::-1])[::-1]
        nearest = np.where(columns - left <= right - columns, left, right)
        output[y] = output[y, nearest]
    missing = np.flatnonzero(~valid.any(axis=1))
    for y in missing:
        nearest_row = populated[np.abs(populated - y).argmin()]
        output[y] = output[nearest_row]
    return output


def png_bytes(rgb: np.ndarray) -> bytes:
    if rgb.dtype != np.uint8 or rgb.ndim != 3 or rgb.shape[2] != 3:
        raise ValueError("PNG input must be uint8 RGB")
    height, width, _ = rgb.shape

    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    rows = b"".join(b"\0" + row.tobytes() for row in rgb)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows, 6)) + chunk(b"IEND", b"")


def srs(epsg: int) -> osr.SpatialReference:
    result = osr.SpatialReference()
    result.ImportFromEPSG(epsg)
    result.SetAxisMappingStrategy(osr.OAMS_TRADITIONAL_GIS_ORDER)
    return result


def boundary_points(ds: gdal.Dataset) -> list[tuple[float, float]]:
    gt = ds.GetGeoTransform()
    pixels = []
    for t in np.linspace(0, 1, 33):
        pixels.extend(((t * ds.RasterXSize, 0), (t * ds.RasterXSize, ds.RasterYSize),
                       (0, t * ds.RasterYSize), (ds.RasterXSize, t * ds.RasterYSize)))
    return [(gt[0] + x * gt[1] + y * gt[2], gt[3] + x * gt[4] + y * gt[5]) for x, y in pixels]


def transformed_bounds(points: list[tuple[float, float]], target: int) -> list[float]:
    transform = osr.CoordinateTransformation(srs(2326), srs(target))
    result = [transform.TransformPoint(x, y) for x, y in points]
    return [min(p[0] for p in result), min(p[1] for p in result), max(p[0] for p in result), max(p[1] for p in result)]


def tile_extent(bounds: list[float], zoom: int) -> tuple[int, int, int, int]:
    span = WORLD * 2 / (2 ** zoom)
    return (math.floor((bounds[0] + WORLD) / span), math.floor((WORLD - bounds[3]) / span),
            math.ceil((bounds[2] + WORLD) / span) - 1, math.ceil((WORLD - bounds[1]) / span) - 1)


def prepare(args: argparse.Namespace) -> dict:
    gdal.UseExceptions()
    source = Path(args.input).resolve()
    output = Path(args.output_dir).resolve()
    if not 0 <= args.minzoom <= args.maxzoom <= 15:
        raise ValueError("Require 0 <= minzoom <= maxzoom <= 15")
    source_hash = sha256(source)
    config = {"preparation_version": PREPARATION_VERSION, "source_sha256": source_hash,
              "minzoom": args.minzoom, "maxzoom": args.maxzoom, "tile_size": TILE_SIZE,
              "source_crs": "EPSG:2326", "vertical_datum": "HKPD", "encoding": "terrarium",
              "resampling": "bilinear_scalar_before_encoding", "nodata_fill": "nearest_in_row_then_nearest_populated_row",
              "gdal_version": gdal.VersionInfo("RELEASE_NAME")}
    version = hashlib.sha256(json.dumps(config, sort_keys=True).encode()).hexdigest()[:16]
    manifest_path = output / "manifest.json"
    final = output / version
    if manifest_path.exists():
        previous = json.loads(manifest_path.read_text())
        if previous.get("version") == version and len(list((final / "tiles").glob("*/*/*.png"))) == previous.get("tile_count"):
            print(f"Reusing {manifest_path} ({previous['tile_count']} tiles)", flush=True)
            return previous
    output.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=f".{version}-", dir=output))
    try:
        ds = gdal.Open(str(source))
        if ds is None or ds.RasterCount != 1:
            raise ValueError("Expected one-band Hong Kong terrain raster")
        points = boundary_points(ds)
        bounds = transformed_bounds(points, 4326)
        mercator = transformed_bounds(points, 3857)
        if not (113 < bounds[0] < bounds[2] < 116 and 21 < bounds[1] < bounds[3] < 24):
            raise ValueError("Source bounds do not describe a Hong Kong EPSG:2326 raster")
        source_metadata = {"path": str(source), "sha256": source_hash,
                           "width": ds.RasterXSize, "height": ds.RasterYSize,
                           "geotransform": list(ds.GetGeoTransform()), "nodata": ds.GetRasterBand(1).GetNoDataValue()}
        source_tif = staging / "source.tif"
        gdal.Translate(str(source_tif), ds, format="GTiff", outputSRS="EPSG:2326",
                       creationOptions=["TILED=YES", "COMPRESS=DEFLATE", "NUM_THREADS=ALL_CPUS"])
        ds = None
        tile_count = 0
        coverage = {}
        for zoom in range(args.minzoom, args.maxzoom + 1):
            x0, y0, x1, y1 = tile_extent(mercator, zoom)
            span = WORLD * 2 / (2 ** zoom)
            width, height = (x1 - x0 + 1) * TILE_SIZE, (y1 - y0 + 1) * TILE_SIZE
            target = staging / "scalar.tif"
            raster = gdal.Warp(str(target), str(source_tif), format="GTiff", dstSRS="EPSG:3857",
                               outputBounds=[x0 * span - WORLD, WORLD - (y1 + 1) * span,
                                             (x1 + 1) * span - WORLD, WORLD - y0 * span],
                               width=width, height=height, outputType=gdal.GDT_Float32,
                               resampleAlg="bilinear", dstNodata=NODATA, multithread=True,
                               warpOptions=["NUM_THREADS=ALL_CPUS"],
                               creationOptions=["TILED=YES", "COMPRESS=DEFLATE", "PREDICTOR=3"])
            heights = raster.ReadAsArray()
            valid = np.isfinite(heights) & (heights != NODATA)
            raster = None
            display = fill_display_nodata(heights, valid)
            count = 0
            skipped = 0
            for tx in range(x1 - x0 + 1):
                folder = staging / "tiles" / str(zoom) / str(x0 + tx)
                for ty in range(y1 - y0 + 1):
                    rows, cols = slice(ty * TILE_SIZE, (ty + 1) * TILE_SIZE), slice(tx * TILE_SIZE, (tx + 1) * TILE_SIZE)
                    if not valid[rows, cols].any():
                        skipped += 1
                        continue
                    folder.mkdir(parents=True, exist_ok=True)
                    (folder / f"{y0 + ty}.png").write_bytes(png_bytes(terrarium_encode(display[rows, cols])))
                    count += 1
            coverage[str(zoom)] = {"tile_bounds": [x0, y0, x1, y1], "tile_count": count,
                                   "omitted_empty_tiles": skipped, "valid_samples": int(valid.sum()),
                                   "display_filled_samples": int(valid.size - valid.sum()),
                                   "valid_height_min_m": float(heights[valid].min()),
                                   "valid_height_max_m": float(heights[valid].max())}
            tile_count += count
            del heights, valid, display
            target.unlink()
            print(f"Zoom {zoom}: {count} tiles; {tile_count} total", flush=True)
        source_tif.unlink()
        manifest = {"id": "hong_kong_5m", "name": "Hong Kong LandsD 5 m terrain", "version": version,
                    "bounds": bounds, "minzoom": args.minzoom, "maxzoom": args.maxzoom,
                    "encoding": "terrarium", "tile_size": TILE_SIZE, "vertical_datum": "HKPD",
                    "resolution_m": 5, "tile_count": tile_count,
                    "attribution": '<a href="https://www.landsd.gov.hk/en/spatial-data/open-data/kf_dtm.html">Terrain © Lands Department, HKSAR Government</a> · <a href="https://data.gov.hk">DATA.GOV.HK</a>',
                    "source": source_metadata, "preparation": config, "coverage_by_zoom": coverage,
                    "accuracy_m": 5, "accuracy_confidence": 0.9, "acquisition_years": [2014, 2015],
                    "source_url": "https://www.landsd.gov.hk/landsd_psi_data/SMO/data/Whole_HK_DTM_5m.zip",
                    "license_url": "https://data.gov.hk/en/terms-and-conditions",
                    "coverage_policy": "Wholly uncovered tiles omitted. Partial tiles extend nearest source-bearing row samples for display only; filled pixels are not measured terrain. Alpha is not a nodata mask.",
                    "limitations": "Source includes vegetation, elevated roads and bridges; not suitable for precise underground cover or clearance. Original HKPD heights preserved."}
        (staging / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        if final.exists():
            shutil.rmtree(final)
        os.replace(staging, final)
        temporary_manifest = output / f".manifest-{version}.json"
        temporary_manifest.write_text(json.dumps(manifest, indent=2) + "\n")
        os.replace(temporary_manifest, manifest_path)
        print(f"Prepared {tile_count} tiles: {manifest_path}", flush=True)
        return manifest
    except BaseException:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True, help="LandsD Whole_HK_DTM_5m.asc")
    parser.add_argument("--output-dir", default=".netweevil/terrain/hong_kong_5m")
    parser.add_argument("--minzoom", type=int, default=8)
    parser.add_argument("--maxzoom", type=int, default=15)
    prepare(parser.parse_args())


if __name__ == "__main__":
    main()
