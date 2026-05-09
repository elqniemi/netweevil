#!/usr/bin/env python3
"""Extract Hong Kong study origins and POIs from the full OSM PBF.

The script intentionally depends on the `osmium` command-line tool instead of a
Python geospatial stack. Netweevil only needs stable point-set CSVs for routing;
the script also writes GeoJSON and metadata records for audit and map review.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import shutil
import subprocess
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


STUDY_DIR = Path(__file__).resolve().parents[1]
REPO_DIR = STUDY_DIR.parents[1]

FULL_OSM = REPO_DIR / "datasets/hong-kong-260508.osm.pbf"
RAW_DIR = STUDY_DIR / "outputs/raw"
PROCESSED_DIR = STUDY_DIR / "outputs/processed"
DEST_DIR = STUDY_DIR / "requests/destinations"
ORIGIN_DIR = STUDY_DIR / "requests/origins"
META_DIR = STUDY_DIR / "metadata/generated"
DEFAULT_BBOX = (113.813, 22.130413, 114.506, 22.56904)

SERVICE_CATEGORIES: dict[str, dict[str, list[str]]] = {
    "food_retail": {
        "shop": ["supermarket", "convenience", "bakery", "greengrocer", "butcher"],
        "amenity": ["marketplace"],
    },
    "education": {
        "amenity": ["kindergarten", "school", "college", "university", "childcare"],
    },
    "healthcare": {
        "amenity": ["doctors", "clinic", "hospital", "dentist", "pharmacy"],
        "healthcare": ["*"],
    },
    "public_services": {
        "amenity": [
            "library",
            "townhall",
            "post_office",
            "community_centre",
            "social_facility",
        ],
    },
    "parks_recreation": {
        "leisure": ["park", "playground", "sports_centre", "pitch"],
        "landuse": ["recreation_ground"],
    },
    "social_life": {
        "amenity": ["cafe", "restaurant", "pub", "fast_food", "cinema", "theatre"],
    },
    "transit_stops": {
        "highway": ["bus_stop"],
        "public_transport": ["platform", "stop_position"],
        "railway": ["station", "tram_stop"],
    },
}

RESIDENTIAL_BUILDINGS = [
    "apartments",
    "house",
    "residential",
    "terrace",
    "detached",
    "dormitory",
]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--osm", type=Path, default=FULL_OSM)
    parser.add_argument("--study-dir", type=Path, default=STUDY_DIR)
    parser.add_argument(
        "--bbox",
        default=",".join(str(value) for value in DEFAULT_BBOX),
        help="Study bbox as min_lon,min_lat,max_lon,max_lat.",
    )
    parser.add_argument("--h3-resolution", type=int, default=9)
    parser.add_argument("--skip-raw-export", action="store_true")
    args = parser.parse_args()

    require_tool("osmium")
    source = args.osm.resolve()
    if not source.exists():
        raise SystemExit(f"OSM source not found: {source}")
    bbox = parse_bbox(args.bbox)

    for path in [RAW_DIR, PROCESSED_DIR, DEST_DIR, ORIGIN_DIR, META_DIR]:
        path.mkdir(parents=True, exist_ok=True)

    source_sha256 = sha256_file(source)
    extract_residential(source, source_sha256, args.skip_raw_export, bbox, args.h3_resolution)
    for category, tag_filters in SERVICE_CATEGORIES.items():
        extract_category(source, source_sha256, category, tag_filters, args.skip_raw_export, bbox)


def extract_residential(
    source: Path,
    source_sha256: str,
    skip_raw_export: bool,
    bbox: tuple[float, float, float, float],
    h3_resolution: int,
) -> None:
    filters = [
        "n/addr:housenumber",
        f"w/building={','.join(RESIDENTIAL_BUILDINGS)}",
        f"a/building={','.join(RESIDENTIAL_BUILDINGS)}",
    ]
    geojson_path = PROCESSED_DIR / "residential_points.geojson"
    command = export_geojson(source, "residential_origins", filters, geojson_path, skip_raw_export)
    features = load_features(geojson_path)

    rows = []
    output_features = []
    seen_ids: set[str] = set()
    for feature in features:
        lon_lat = representative_point(feature.get("geometry"))
        if lon_lat is None:
            continue
        if not in_bbox(lon_lat[0], lon_lat[1], bbox):
            continue
        props = clean_properties(feature.get("properties", {}))
        if is_non_residential_address(props):
            continue
        source_id = osm_id(feature, props)
        if source_id in seen_ids:
            continue
        seen_ids.add(source_id)
        lon, lat = lon_lat
        weight = max(1.0, approximate_area_m2(feature.get("geometry")) / 120.0)
        row_id = f"res_{stable_token(source_id)}"
        rows.append(
            {
                "id": row_id,
                "x": f"{lon:.8f}",
                "y": f"{lat:.8f}",
                "source_osm_id": source_id,
                "weight": f"{weight:.3f}",
                "origin_type": residential_origin_type(props),
                "tags_json": compact_json(props),
            }
        )
        output_features.append(point_feature(row_id, lon, lat, props | {"weight": weight}))

    write_csv(ORIGIN_DIR / "residential_points.csv", rows)
    write_residential_h3(rows, source, source_sha256, bbox, h3_resolution)
    write_geojson(geojson_path, output_features)
    maybe_write_gpkg(geojson_path, PROCESSED_DIR / "residential_points.gpkg", "residential_points")
    write_metadata(
        "residential_points",
        source,
        source_sha256,
        command,
        "EPSG:4326",
        "Point",
        len(rows),
        filters,
        bbox,
    )


def extract_category(
    source: Path,
    source_sha256: str,
    category: str,
    tag_filters: dict[str, list[str]],
    skip_raw_export: bool,
    bbox: tuple[float, float, float, float],
) -> None:
    filters = osmium_filters(tag_filters)
    geojson_path = PROCESSED_DIR / f"{category}.geojson"
    command = export_geojson(source, category, filters, geojson_path, skip_raw_export)
    features = load_features(geojson_path)

    rows = []
    output_features = []
    deduped: list[tuple[str, float, float]] = []
    for feature in features:
        lon_lat = representative_point(feature.get("geometry"))
        if lon_lat is None:
            continue
        lon, lat = lon_lat
        if not in_bbox(lon, lat, bbox):
            continue
        props = clean_properties(feature.get("properties", {}))
        name = normalized_name(props)
        if duplicate_point(deduped, name, lon, lat, tolerance_m=20.0):
            continue
        deduped.append((name, lon, lat))
        source_id = osm_id(feature, props)
        row_id = f"{category}_{stable_token(source_id)}"
        rows.append(
            {
                "id": row_id,
                "x": f"{lon:.8f}",
                "y": f"{lat:.8f}",
                "category": category,
                "name": props.get("name", ""),
                "source_osm_id": source_id,
                "tags_json": compact_json(props),
            }
        )
        output_features.append(point_feature(row_id, lon, lat, props | {"category": category}))

    write_csv(DEST_DIR / f"{category}.csv", rows)
    write_geojson(geojson_path, output_features)
    maybe_write_gpkg(geojson_path, PROCESSED_DIR / f"{category}.gpkg", category)
    write_metadata(
        category,
        source,
        source_sha256,
        command,
        "EPSG:4326",
        "Point",
        len(rows),
        filters,
        bbox,
    )


def export_geojson(
    source: Path, label: str, filters: list[str], geojson_path: Path, skip_raw_export: bool
) -> list[str]:
    raw_pbf = RAW_DIR / f"{label}.osm.pbf"
    tags_command = ["osmium", "tags-filter", str(source), *filters, "-o", str(raw_pbf), "-O"]
    export_command = ["osmium", "export", str(raw_pbf), "-o", str(geojson_path), "-O"]
    if not skip_raw_export:
        subprocess.run(tags_command, check=True)
        subprocess.run(export_command, check=True)
    elif not geojson_path.exists():
        raise SystemExit(f"{geojson_path} does not exist; cannot use --skip-raw-export")
    return tags_command + ["&&"] + export_command


def osmium_filters(tag_filters: dict[str, list[str]]) -> list[str]:
    filters = []
    for key, values in tag_filters.items():
        if values == ["*"]:
            filters.extend([f"n/{key}", f"w/{key}", f"a/{key}"])
        else:
            value_list = ",".join(values)
            filters.extend([f"n/{key}={value_list}", f"w/{key}={value_list}", f"a/{key}={value_list}"])
    return filters


def parse_bbox(value: str) -> tuple[float, float, float, float]:
    parts = [float(part.strip()) for part in value.split(",")]
    if len(parts) != 4:
        raise SystemExit("--bbox must have four comma-separated values")
    min_lon, min_lat, max_lon, max_lat = parts
    if min_lon >= max_lon or min_lat >= max_lat:
        raise SystemExit("--bbox must be min_lon,min_lat,max_lon,max_lat")
    return min_lon, min_lat, max_lon, max_lat


def in_bbox(lon: float, lat: float, bbox: tuple[float, float, float, float]) -> bool:
    min_lon, min_lat, max_lon, max_lat = bbox
    return min_lon <= lon <= max_lon and min_lat <= lat <= max_lat


def load_features(path: Path) -> list[dict[str, Any]]:
    with path.open() as handle:
        data = json.load(handle)
    if data.get("type") == "FeatureCollection":
        return data.get("features", [])
    if data.get("type") == "Feature":
        return [data]
    return []


def representative_point(geometry: dict[str, Any] | None) -> tuple[float, float] | None:
    if not geometry:
        return None
    geom_type = geometry.get("type")
    coords = geometry.get("coordinates")
    if geom_type == "Point" and isinstance(coords, list) and len(coords) >= 2:
        return float(coords[0]), float(coords[1])
    points = list(iter_lon_lat(coords))
    if not points:
        return None
    lon = sum(point[0] for point in points) / len(points)
    lat = sum(point[1] for point in points) / len(points)
    return lon, lat


def iter_lon_lat(value: Any):
    if isinstance(value, list) and len(value) >= 2 and all(isinstance(v, (int, float)) for v in value[:2]):
        yield float(value[0]), float(value[1])
    elif isinstance(value, list):
        for item in value:
            yield from iter_lon_lat(item)


def approximate_area_m2(geometry: dict[str, Any] | None) -> float:
    if not geometry or geometry.get("type") not in {"Polygon", "MultiPolygon"}:
        return 0.0
    rings = []
    coords = geometry.get("coordinates", [])
    if geometry["type"] == "Polygon":
        rings = coords[:1]
    else:
        rings = [polygon[0] for polygon in coords if polygon]
    total = 0.0
    for ring in rings:
        points = [(float(lon), float(lat)) for lon, lat, *_ in ring]
        if len(points) < 3:
            continue
        lat0 = math.radians(sum(lat for _, lat in points) / len(points))
        projected = [
            (
                math.radians(lon) * 6371008.8 * math.cos(lat0),
                math.radians(lat) * 6371008.8,
            )
            for lon, lat in points
        ]
        total += abs(shoelace_area(projected))
    return total


def shoelace_area(points: list[tuple[float, float]]) -> float:
    area = 0.0
    for index, (x1, y1) in enumerate(points):
        x2, y2 = points[(index + 1) % len(points)]
        area += x1 * y2 - x2 * y1
    return area / 2.0


def duplicate_point(points: list[tuple[str, float, float]], name: str, lon: float, lat: float, tolerance_m: float) -> bool:
    for existing_name, existing_lon, existing_lat in points:
        if existing_name == name and haversine_m(lon, lat, existing_lon, existing_lat) <= tolerance_m:
            return True
    return False


def haversine_m(lon1: float, lat1: float, lon2: float, lat2: float) -> float:
    radius = 6371008.8
    phi1 = math.radians(lat1)
    phi2 = math.radians(lat2)
    delta_phi = math.radians(lat2 - lat1)
    delta_lambda = math.radians(lon2 - lon1)
    a = math.sin(delta_phi / 2) ** 2 + math.cos(phi1) * math.cos(phi2) * math.sin(delta_lambda / 2) ** 2
    return 2 * radius * math.atan2(math.sqrt(a), math.sqrt(1 - a))


def clean_properties(props: dict[str, Any]) -> dict[str, str]:
    return {str(key): str(value) for key, value in props.items() if value is not None}


def osm_id(feature: dict[str, Any], props: dict[str, str]) -> str:
    for key in ["@id", "id", "osm_id"]:
        if key in props:
            return props[key]
    feature_id = feature.get("id")
    if feature_id is not None:
        return str(feature_id)
    return hashlib.sha1(compact_json(feature).encode("utf-8")).hexdigest()[:16]


def stable_token(value: str) -> str:
    token = "".join(ch.lower() if ch.isalnum() else "_" for ch in value)
    token = "_".join(part for part in token.split("_") if part)
    if token:
        return token[:80]
    return hashlib.sha1(value.encode("utf-8")).hexdigest()[:16]


def normalized_name(props: dict[str, str]) -> str:
    name = props.get("name") or props.get("brand") or props.get("@id") or props.get("id") or ""
    return " ".join(name.casefold().split())


def residential_origin_type(props: dict[str, str]) -> str:
    if "addr:housenumber" in props:
        return "address"
    if "building" in props:
        return f"building:{props['building']}"
    return "residential"


def is_non_residential_address(props: dict[str, str]) -> bool:
    if "addr:housenumber" not in props:
        return False
    non_residential_keys = {
        "shop",
        "amenity",
        "tourism",
        "office",
        "leisure",
        "healthcare",
        "craft",
        "industrial",
        "public_transport",
        "railway",
    }
    return any(key in props for key in non_residential_keys)


def point_feature(row_id: str, lon: float, lat: float, props: dict[str, Any]) -> dict[str, Any]:
    return {
        "type": "Feature",
        "geometry": {"type": "Point", "coordinates": [lon, lat]},
        "properties": {"id": row_id, **props},
    }


def write_residential_h3(
    rows: list[dict[str, Any]],
    source: Path,
    source_sha256: str,
    bbox: tuple[float, float, float, float],
    resolution: int,
) -> None:
    h3 = import_h3()
    cells: dict[str, dict[str, Any]] = {}
    for row in rows:
        lon = float(row["x"])
        lat = float(row["y"])
        cell_id = h3.latlng_to_cell(lat, lon, resolution)
        cell = cells.setdefault(
            cell_id,
            {"lon_sum": 0.0, "lat_sum": 0.0, "point_count": 0.0, "weight": 0.0},
        )
        cell["lon_sum"] += lon
        cell["lat_sum"] += lat
        cell["point_count"] += 1.0
        cell["weight"] += float(row.get("weight") or 1.0)

    point_rows = []
    polygon_features = []
    for cell_id, cell in sorted(cells.items()):
        centroid_lon = cell["lon_sum"] / cell["point_count"]
        centroid_lat = cell["lat_sum"] / cell["point_count"]
        point_rows.append(
            {
                "id": cell_id,
                "x": f"{centroid_lon:.8f}",
                "y": f"{centroid_lat:.8f}",
                "h3_resolution": str(resolution),
                "residential_point_count": str(int(cell["point_count"])),
                "weight": f"{cell['weight']:.3f}",
            }
        )
        ring = [[float(lng), float(lat)] for lat, lng in h3.cell_to_boundary(cell_id)]
        ring.append(ring[0])
        polygon_features.append(
            {
                "type": "Feature",
                "geometry": {
                    "type": "Polygon",
                    "coordinates": [ring],
                },
                "properties": {
                    "id": cell_id,
                    "h3_resolution": resolution,
                    "residential_point_count": int(cell["point_count"]),
                    "weight": cell["weight"],
                    "centroid_lon": centroid_lon,
                    "centroid_lat": centroid_lat,
                },
            }
        )

    points_path = ORIGIN_DIR / f"residential_h3_r{resolution}_points.csv"
    write_csv(points_path, point_rows)
    h3_geojson = PROCESSED_DIR / f"residential_h3_r{resolution}.geojson"
    write_geojson(h3_geojson, polygon_features)
    maybe_write_gpkg(h3_geojson, PROCESSED_DIR / f"residential_h3_r{resolution}.gpkg", f"residential_h3_r{resolution}")
    write_metadata(
        f"residential_h3_r{resolution}",
        source,
        source_sha256,
        ["h3", "resolution", str(resolution), "derived", "from", str(ORIGIN_DIR / "residential_points.csv")],
        "EPSG:4326",
        "Polygon",
        len(point_rows),
        ["residential_points", f"h3_resolution={resolution}"],
        bbox,
    )


def import_h3():
    try:
        import h3
    except ImportError as error:
        raise SystemExit(
            "Python package 'h3' is required. Run with "
            "`uv run --project projects-analyses/hong-kong-15-minute-city python "
            "projects-analyses/hong-kong-15-minute-city/scripts/extract_osm_features.py`."
        ) from error
    return h3


def write_csv(path: Path, rows: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if not rows:
        path.write_text("id,x,y\n")
        return
    fieldnames = list(rows[0].keys())
    with path.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(rows)


def write_geojson(path: Path, features: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w") as handle:
        json.dump({"type": "FeatureCollection", "features": features}, handle, indent=2)
        handle.write("\n")


def maybe_write_gpkg(geojson_path: Path, gpkg_path: Path, layer: str) -> None:
    if shutil.which("ogr2ogr") is None:
        return
    subprocess.run(
        ["ogr2ogr", "-f", "GPKG", str(gpkg_path), str(geojson_path), "-nln", layer, "-overwrite"],
        check=True,
    )


def write_metadata(
    layer_id: str,
    source: Path,
    source_sha256: str,
    command: list[str],
    crs: str,
    geometry_type: str,
    record_count: int,
    filters: list[str],
    bbox: tuple[float, float, float, float],
) -> None:
    META_DIR.mkdir(parents=True, exist_ok=True)
    payload = {
        "layer_id": layer_id,
        "source_path": str(source),
        "source_sha256": source_sha256,
        "extraction_command": command,
        "extraction_timestamp": datetime.now(timezone.utc).isoformat(),
        "crs": crs,
        "geometry_type": geometry_type,
        "record_count": record_count,
        "tag_filters": filters,
        "bbox": {
            "min_lon": bbox[0],
            "min_lat": bbox[1],
            "max_lon": bbox[2],
            "max_lat": bbox[3],
        },
    }
    with (META_DIR / f"{layer_id}.json").open("w") as handle:
        json.dump(payload, handle, indent=2)
        handle.write("\n")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def compact_json(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def require_tool(name: str) -> None:
    if shutil.which(name) is None:
        raise SystemExit(f"required tool not found on PATH: {name}")


if __name__ == "__main__":
    main()
