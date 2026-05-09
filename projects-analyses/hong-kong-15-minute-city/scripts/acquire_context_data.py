#!/usr/bin/env python3
"""Acquire minimal Hong Kong context datasets for the comparative study."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import time
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlencode
from urllib.request import urlopen


STUDY_DIR = Path(__file__).resolve().parents[1]
REPO_DIR = STUDY_DIR.parents[1]
DOWNLOAD_DIR = REPO_DIR / "datasets/downloads/hong_kong"
META_DIR = STUDY_DIR / "metadata/generated"

TPUSU_SERVICE = "https://portal.csdi.gov.hk/server/rest/services/common/pland_rcd_1634022783366_65050/FeatureServer/0/query"
TPUSU_METADATA = "https://data.gov.hk/en-data/dataset/hk-pland-pland1-boundaries-of-tpu-sb-vc/resource/93433a07-238e-4ea2-b7a3-9cd67edbf7b6"
TPUSU_GEOJSON = DOWNLOAD_DIR / "hk_2021_tpu_subunit_boundaries.geojson"
TPUSU_GPKG = DOWNLOAD_DIR / "hk_2021_tpu_subunit_boundaries.gpkg"

OZP_SERVICE = "https://services3.arcgis.com/6j1KwZfY2fZrfNMR/arcgis/rest/services/ZONE/FeatureServer/0/query"
OZP_METADATA = "https://www.arcgis.com/sharing/rest/content/items/203ef828de0741d59ecb105dee6c9cbe?f=json"
OZP_GEOJSON = DOWNLOAD_DIR / "hk_ozp_land_use_zonings.geojson"
OZP_GPKG = DOWNLOAD_DIR / "hk_ozp_land_use_zonings.gpkg"

LAND_UTIL_EXPORT = "https://portal.csdi.gov.hk/server/rest/services/common/pland_rcd_1725865972233_20687/MapServer/export"
LAND_UTIL_METADATA = "https://data.gov.hk/en-data/dataset/hk-pland-pland1-land-utilization-in-hong-kong-raster-grid/resource/c55df285-7425-4606-b5f9-4813a69c3a60"
LAND_UTIL_TILE_DIR = DOWNLOAD_DIR / "land_utilization_2024_tiles"
LAND_UTIL_VRT = DOWNLOAD_DIR / "hk_land_utilization_2024_rendered.vrt"
LAND_UTIL_TIF = DOWNLOAD_DIR / "hk_land_utilization_2024_rendered.tif"

HK_BOUNDS_2326 = (800000.0, 800000.0, 863750.0, 848000.0)


def main() -> None:
    global DOWNLOAD_DIR, TPUSU_GEOJSON, TPUSU_GPKG, OZP_GEOJSON, OZP_GPKG
    global LAND_UTIL_TILE_DIR, LAND_UTIL_VRT, LAND_UTIL_TIF
    parser = argparse.ArgumentParser()
    parser.add_argument("--out-dir", type=Path, default=DOWNLOAD_DIR)
    parser.add_argument("--skip-raster", action="store_true")
    parser.add_argument("--skip-gpkg", action="store_true")
    args = parser.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)
    META_DIR.mkdir(parents=True, exist_ok=True)
    DOWNLOAD_DIR = args.out_dir
    TPUSU_GEOJSON = DOWNLOAD_DIR / "hk_2021_tpu_subunit_boundaries.geojson"
    TPUSU_GPKG = DOWNLOAD_DIR / "hk_2021_tpu_subunit_boundaries.gpkg"
    OZP_GEOJSON = DOWNLOAD_DIR / "hk_ozp_land_use_zonings.geojson"
    OZP_GPKG = DOWNLOAD_DIR / "hk_ozp_land_use_zonings.gpkg"
    LAND_UTIL_TILE_DIR = DOWNLOAD_DIR / "land_utilization_2024_tiles"
    LAND_UTIL_VRT = DOWNLOAD_DIR / "hk_land_utilization_2024_rendered.vrt"
    LAND_UTIL_TIF = DOWNLOAD_DIR / "hk_land_utilization_2024_rendered.tif"

    manifest = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "output_dir": str(DOWNLOAD_DIR),
        "datasets": [],
    }

    manifest["datasets"].append(
        acquire_feature_service(
            label="2021 TPU and Subunit boundaries",
            service_url=TPUSU_SERVICE,
            metadata_url=TPUSU_METADATA,
            out_geojson=TPUSU_GEOJSON,
            out_gpkg=TPUSU_GPKG,
            page_size=3000,
            skip_gpkg=args.skip_gpkg,
        )
    )
    manifest["datasets"].append(
        acquire_feature_service(
            label="Outline Zoning Plans land-use zonings",
            service_url=OZP_SERVICE,
            metadata_url=OZP_METADATA,
            out_geojson=OZP_GEOJSON,
            out_gpkg=OZP_GPKG,
            page_size=2000,
            skip_gpkg=args.skip_gpkg,
        )
    )
    if not args.skip_raster:
        manifest["datasets"].append(acquire_land_utilization_raster())

    out_manifest = META_DIR / "hk_context_data_sources.json"
    out_manifest.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"context data manifest written to {out_manifest}")


def acquire_feature_service(
    label: str,
    service_url: str,
    metadata_url: str,
    out_geojson: Path,
    out_gpkg: Path,
    page_size: int,
    skip_gpkg: bool,
) -> dict[str, object]:
    count = feature_count(service_url)
    features = []
    for offset in range(0, count, page_size):
        params = {
            "f": "geojson",
            "where": "1=1",
            "outFields": "*",
            "returnGeometry": "true",
            "outSR": "4326",
            "resultOffset": str(offset),
            "resultRecordCount": str(page_size),
        }
        page = get_json(service_url, params)
        features.extend(page.get("features", []))
        time.sleep(0.1)
    collection = {"type": "FeatureCollection", "features": features}
    out_geojson.parent.mkdir(parents=True, exist_ok=True)
    out_geojson.write_text(json.dumps(collection) + "\n")
    gpkg_status = "skipped"
    if not skip_gpkg:
        gpkg_status = convert_to_gpkg(out_geojson, out_gpkg)
    return {
        "label": label,
        "source_api": service_url,
        "metadata_url": metadata_url,
        "format": "ArcGIS REST query API",
        "feature_count": len(features),
        "geojson": str(out_geojson),
        "geojson_sha256": sha256(out_geojson),
        "gpkg": str(out_gpkg) if out_gpkg.exists() else "",
        "gpkg_status": gpkg_status,
        "crs": "EPSG:4326",
    }


def feature_count(service_url: str) -> int:
    data = get_json(
        service_url,
        {
            "f": "json",
            "where": "1=1",
            "returnCountOnly": "true",
        },
    )
    return int(data["count"])


def acquire_land_utilization_raster() -> dict[str, object]:
    LAND_UTIL_TILE_DIR.mkdir(parents=True, exist_ok=True)
    min_x, min_y, max_x, max_y = HK_BOUNDS_2326
    mid_x = (min_x + max_x) / 2
    mid_y = (min_y + max_y) / 2
    boxes = [
        ("sw", (min_x, min_y, mid_x, mid_y)),
        ("se", (mid_x, min_y, max_x, mid_y)),
        ("nw", (min_x, mid_y, mid_x, max_y)),
        ("ne", (mid_x, mid_y, max_x, max_y)),
    ]
    geotiffs = []
    tile_records = []
    for tile_id, bbox in boxes:
        png_path, tif_path, record = export_land_util_tile(tile_id, bbox)
        geotiffs.append(tif_path)
        tile_records.append(record | {"png": str(png_path), "geotiff": str(tif_path)})
    subprocess.run(["gdalbuildvrt", str(LAND_UTIL_VRT), *[str(path) for path in geotiffs]], check=True)
    subprocess.run(
        [
            "gdal_translate",
            "-co",
            "COMPRESS=DEFLATE",
            "-co",
            "PREDICTOR=2",
            str(LAND_UTIL_VRT),
            str(LAND_UTIL_TIF),
        ],
        check=True,
    )
    return {
        "label": "2024 raster grids on land utilization",
        "source_api": LAND_UTIL_EXPORT,
        "metadata_url": LAND_UTIL_METADATA,
        "format": "ArcGIS MapServer export tiles, georeferenced as rendered GeoTIFF",
        "crs": "EPSG:2326",
        "resolution_note": "Requested at approximately 10 m pixels using four MapServer export tiles; source service returns rendered PNG tiles rather than class-code raster cells.",
        "tile_count": len(tile_records),
        "tiles": tile_records,
        "vrt": str(LAND_UTIL_VRT),
        "geotiff": str(LAND_UTIL_TIF),
        "geotiff_sha256": sha256(LAND_UTIL_TIF),
    }


def export_land_util_tile(tile_id: str, bbox: tuple[float, float, float, float]) -> tuple[Path, Path, dict[str, object]]:
    min_x, min_y, max_x, max_y = bbox
    width = round((max_x - min_x) / 10)
    height = round((max_y - min_y) / 10)
    params = {
        "bbox": f"{min_x},{min_y},{max_x},{max_y}",
        "bboxSR": "2326",
        "imageSR": "2326",
        "size": f"{width},{height}",
        "format": "png32",
        "transparent": "false",
        "f": "json",
    }
    exported = get_json(LAND_UTIL_EXPORT, params)
    href = exported["href"]
    png_path = LAND_UTIL_TILE_DIR / f"land_utilization_2024_{tile_id}.png"
    tif_path = LAND_UTIL_TILE_DIR / f"land_utilization_2024_{tile_id}.tif"
    download_binary(href, png_path)
    extent = exported["extent"]
    subprocess.run(
        [
            "gdal_translate",
            "-of",
            "GTiff",
            "-a_srs",
            "EPSG:2326",
            "-a_ullr",
            str(extent["xmin"]),
            str(extent["ymax"]),
            str(extent["xmax"]),
            str(extent["ymin"]),
            str(png_path),
            str(tif_path),
        ],
        check=True,
    )
    return (
        png_path,
        tif_path,
        {
            "tile_id": tile_id,
            "bbox_2326": [min_x, min_y, max_x, max_y],
            "requested_size": [width, height],
            "export_href": href,
            "export_extent": extent,
        },
    )


def get_json(url: str, params: dict[str, str]) -> dict:
    request_url = f"{url}?{urlencode(params)}"
    with urlopen(request_url, timeout=120) as response:
        return json.loads(response.read().decode("utf-8"))


def download_binary(url: str, out: Path) -> None:
    with urlopen(url, timeout=120) as response:
        out.write_bytes(response.read())


def convert_to_gpkg(source: Path, out: Path) -> str:
    try:
        subprocess.run(["ogr2ogr", "-f", "GPKG", str(out), str(source)], check=True)
    except (FileNotFoundError, subprocess.CalledProcessError) as error:
        return f"failed: {error}"
    return "written"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


if __name__ == "__main__":
    main()
