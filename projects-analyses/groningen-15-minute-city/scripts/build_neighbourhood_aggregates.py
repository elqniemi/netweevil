#!/usr/bin/env python3
"""Extract OSM neighbourhood polygons and aggregate H3 accessibility scores."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import shutil
import subprocess
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from statistics import median
from typing import Any


STUDY_DIR = Path(__file__).resolve().parents[1]
REPO_DIR = STUDY_DIR.parents[1]
ADMIN_OSM = REPO_DIR / "datasets/groningen-260508-admin.osm.pbf"
RAW_DIR = STUDY_DIR / "outputs/raw"
PROCESSED_DIR = STUDY_DIR / "outputs/processed"
MAP_DIR = STUDY_DIR / "outputs/maps/gis"
META_DIR = STUDY_DIR / "metadata/generated"
H3_GEOJSON = PROCESSED_DIR / "residential_h3_r9.geojson"
SCORES = PROCESSED_DIR / "accessibility_scores.csv"
NEAREST = PROCESSED_DIR / "accessibility_nearest_services.csv"
DEFAULT_BBOX = (6.45, 53.16, 6.68, 53.27)

PLACE_PRIORITY = {"neighbourhood": 0, "quarter": 1, "suburb": 2}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--admin-osm", type=Path, default=ADMIN_OSM)
    parser.add_argument("--h3", type=Path, default=H3_GEOJSON)
    parser.add_argument("--scores", type=Path, default=SCORES)
    parser.add_argument("--nearest", type=Path, default=NEAREST)
    parser.add_argument("--bbox", default=",".join(str(value) for value in DEFAULT_BBOX))
    args = parser.parse_args()

    require_tool("osmium")
    bbox = parse_bbox(args.bbox)
    source = args.admin_osm.resolve()
    if not source.exists():
        raise SystemExit(f"admin OSM source not found: {source}")

    RAW_DIR.mkdir(parents=True, exist_ok=True)
    PROCESSED_DIR.mkdir(parents=True, exist_ok=True)
    MAP_DIR.mkdir(parents=True, exist_ok=True)
    META_DIR.mkdir(parents=True, exist_ok=True)

    raw_pbf = RAW_DIR / "residential_neighbourhoods.osm.pbf"
    raw_geojson = RAW_DIR / "residential_neighbourhoods_raw.geojson"
    neighbourhoods_geojson = PROCESSED_DIR / "residential_neighbourhoods.geojson"

    tags_command = [
        "osmium",
        "tags-filter",
        str(source),
        "a/place=neighbourhood,quarter,suburb",
        "-o",
        str(raw_pbf),
        "-O",
    ]
    export_command = ["osmium", "export", str(raw_pbf), "-o", str(raw_geojson), "-O"]
    subprocess.run(tags_command, check=True)
    subprocess.run(export_command, check=True)

    neighbourhoods = load_neighbourhood_features(raw_geojson, bbox)
    write_geojson(neighbourhoods_geojson, neighbourhoods)
    maybe_write_gpkg(neighbourhoods_geojson, PROCESSED_DIR / "residential_neighbourhoods.gpkg", "residential_neighbourhoods")
    write_metadata(
        "residential_neighbourhoods",
        source,
        sha256_file(source),
        tags_command + ["&&"] + export_command,
        "EPSG:4326",
        "MultiPolygon",
        len(neighbourhoods),
        ["a/place=neighbourhood,quarter,suburb"],
        bbox,
    )

    h3_features = load_feature_collection(args.h3)
    assignments = assign_h3_to_neighbourhoods(h3_features, neighbourhoods)
    write_rows(PROCESSED_DIR / "h3_neighbourhood_assignments.csv", assignments)

    score_rows = aggregate_scores(assignments, read_rows(args.scores))
    category_rows = aggregate_category_times(assignments, read_rows(args.nearest))
    write_rows(PROCESSED_DIR / "neighbourhood_accessibility_scores.csv", score_rows)
    write_rows(PROCESSED_DIR / "neighbourhood_category_times.csv", category_rows)
    write_geojson(MAP_DIR / "neighbourhood_accessibility_scores.geojson", neighbourhood_score_features(neighbourhoods, score_rows))

    summary = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "neighbourhood_count": len(neighbourhoods),
        "assigned_h3_count": sum(1 for row in assignments if row["neighbourhood_id"]),
        "unassigned_h3_count": sum(1 for row in assignments if not row["neighbourhood_id"]),
        "score_row_count": len(score_rows),
        "category_row_count": len(category_rows),
        "mode_inequality": mode_inequality(score_rows),
    }
    (META_DIR / "neighbourhood_accessibility_summary.json").write_text(json.dumps(summary, indent=2) + "\n")


def load_neighbourhood_features(path: Path, bbox: tuple[float, float, float, float]) -> list[dict[str, Any]]:
    features = []
    for feature in load_feature_collection(path):
        geometry = feature.get("geometry") or {}
        if geometry.get("type") not in {"Polygon", "MultiPolygon"}:
            continue
        props = clean_properties(feature.get("properties") or {})
        place = props.get("place", "")
        name = props.get("name", "")
        if place not in PLACE_PRIORITY or not name:
            continue
        centroid = representative_point(geometry)
        if centroid is None or not in_bbox(centroid[0], centroid[1], bbox):
            continue
        area = approximate_area_m2(geometry)
        neighbourhood_id = stable_token(f"{place}:{name}")
        features.append(
            {
                "type": "Feature",
                "geometry": geometry,
                "properties": {
                    "id": neighbourhood_id,
                    "name": name,
                    "place": place,
                    "source_osm_id": osm_id(feature, props),
                    "area_m2": round(area, 3),
                    "centroid_lon": centroid[0],
                    "centroid_lat": centroid[1],
                },
            }
        )
    features.sort(key=lambda feature: (feature["properties"]["place"], feature["properties"]["name"]))
    return features


def assign_h3_to_neighbourhoods(h3_features: list[dict[str, Any]], neighbourhoods: list[dict[str, Any]]) -> list[dict[str, str]]:
    output = []
    for feature in h3_features:
        props = feature.get("properties") or {}
        lon = float(props.get("centroid_lon"))
        lat = float(props.get("centroid_lat"))
        candidates = []
        for neighbourhood in neighbourhoods:
            if point_in_geometry(lon, lat, neighbourhood.get("geometry") or {}):
                nprops = neighbourhood["properties"]
                candidates.append(
                    (
                        PLACE_PRIORITY.get(nprops.get("place", ""), 99),
                        float(nprops.get("area_m2", 0.0)),
                        nprops,
                    )
                )
        candidates.sort(key=lambda item: (item[0], item[1]))
        if candidates:
            selected = candidates[0][2]
            output.append(
                {
                    "origin_id": props["id"],
                    "neighbourhood_id": selected["id"],
                    "neighbourhood_name": selected["name"],
                    "place": selected["place"],
                    "weight": str(props.get("weight", 1.0)),
                    "residential_point_count": str(props.get("residential_point_count", 0)),
                }
            )
        else:
            output.append(
                {
                    "origin_id": props["id"],
                    "neighbourhood_id": "",
                    "neighbourhood_name": "",
                    "place": "",
                    "weight": str(props.get("weight", 1.0)),
                    "residential_point_count": str(props.get("residential_point_count", 0)),
                }
            )
    return output


def aggregate_scores(assignments: list[dict[str, str]], scores: list[dict[str, str]]) -> list[dict[str, str]]:
    assignment_by_origin = {row["origin_id"]: row for row in assignments if row["neighbourhood_id"]}
    grouped: dict[tuple[str, str], list[tuple[dict[str, str], dict[str, str]]]] = defaultdict(list)
    for score in scores:
        assignment = assignment_by_origin.get(score["origin_id"])
        if assignment:
            grouped[(assignment["neighbourhood_id"], score["mode"])].append((assignment, score))

    output = []
    for (neighbourhood_id, mode), rows in sorted(grouped.items(), key=lambda item: (item[0][0], item[0][1])):
        assignment = rows[0][0]
        weights = [float(row[0]["weight"] or 1.0) for row in rows]
        strict_scores = [float(row[1]["strict_score"]) for row in rows]
        complete = [row[1]["complete_15m"] == "true" for row in rows]
        relaxed = [row[1]["relaxed_complete_15m"] == "true" for row in rows]
        output.append(
            {
                "neighbourhood_id": neighbourhood_id,
                "neighbourhood_name": assignment["neighbourhood_name"],
                "place": assignment["place"],
                "mode": mode,
                "h3_count": str(len(rows)),
                "residential_weight": format_number(sum(weights)),
                "mean_strict_score": format_number(mean(strict_scores)),
                "weighted_mean_strict_score": format_number(weighted_mean(strict_scores, weights)),
                "strict_complete_count": str(sum(complete)),
                "strict_complete_share": format_number(sum(complete) / len(complete)),
                "relaxed_complete_count": str(sum(relaxed)),
                "relaxed_complete_share": format_number(sum(relaxed) / len(relaxed)),
            }
        )
    return output


def aggregate_category_times(assignments: list[dict[str, str]], nearest_rows: list[dict[str, str]]) -> list[dict[str, str]]:
    assignment_by_origin = {row["origin_id"]: row for row in assignments if row["neighbourhood_id"]}
    grouped: dict[tuple[str, str, str], list[float]] = defaultdict(list)
    meta: dict[tuple[str, str, str], dict[str, str]] = {}
    for row in nearest_rows:
        assignment = assignment_by_origin.get(row["origin_id"])
        time_s = parse_float(row.get("nearest_travel_time_s"))
        if assignment is None or time_s is None:
            continue
        key = (assignment["neighbourhood_id"], row["mode"], row["category"])
        grouped[key].append(time_s)
        meta[key] = assignment

    output = []
    for key, values in sorted(grouped.items()):
        neighbourhood_id, mode, category = key
        assignment = meta[key]
        output.append(
            {
                "neighbourhood_id": neighbourhood_id,
                "neighbourhood_name": assignment["neighbourhood_name"],
                "place": assignment["place"],
                "mode": mode,
                "category": category,
                "origin_count": str(len(values)),
                "median_nearest_travel_time_s": format_number(median(values)),
                "p90_nearest_travel_time_s": format_number(percentile(values, 0.9)),
            }
        )
    return output


def neighbourhood_score_features(neighbourhoods: list[dict[str, Any]], score_rows: list[dict[str, str]]) -> list[dict[str, Any]]:
    by_id = {feature["properties"]["id"]: feature for feature in neighbourhoods}
    features = []
    for row in score_rows:
        base = by_id[row["neighbourhood_id"]]
        features.append(
            {
                "type": "Feature",
                "geometry": base["geometry"],
                "properties": {
                    **base["properties"],
                    **row,
                    "mean_strict_score": parse_float(row["mean_strict_score"]),
                    "weighted_mean_strict_score": parse_float(row["weighted_mean_strict_score"]),
                    "strict_complete_share": parse_float(row["strict_complete_share"]),
                    "relaxed_complete_share": parse_float(row["relaxed_complete_share"]),
                },
            }
        )
    return features


def mode_inequality(score_rows: list[dict[str, str]]) -> list[dict[str, Any]]:
    grouped: dict[str, list[float]] = defaultdict(list)
    for row in score_rows:
        grouped[row["mode"]].append(float(row["weighted_mean_strict_score"]))
    output = []
    for mode, values in sorted(grouped.items()):
        p10 = percentile(values, 0.1)
        p25 = percentile(values, 0.25)
        p75 = percentile(values, 0.75)
        p90 = percentile(values, 0.9)
        output.append(
            {
                "mode": mode,
                "neighbourhood_count": len(values),
                "p10_weighted_strict_score": round(p10, 6),
                "p25_weighted_strict_score": round(p25, 6),
                "p75_weighted_strict_score": round(p75, 6),
                "p90_weighted_strict_score": round(p90, 6),
                "iqr_weighted_strict_score": round(p75 - p25, 6),
                "p90_p10_ratio": round(p90 / p10, 6) if p10 > 0 else None,
            }
        )
    return output


def load_feature_collection(path: Path) -> list[dict[str, Any]]:
    with path.open() as handle:
        data = json.load(handle)
    if data.get("type") == "FeatureCollection":
        return data.get("features", [])
    return []


def read_rows(path: Path) -> list[dict[str, str]]:
    with path.open(newline="") as handle:
        return list(csv.DictReader(handle))


def write_rows(path: Path, rows: list[dict[str, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if not rows:
        path.write_text("")
        return
    with path.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0].keys()))
        writer.writeheader()
        writer.writerows(rows)


def write_geojson(path: Path, features: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({"type": "FeatureCollection", "features": features}, indent=2) + "\n")


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
    (META_DIR / f"{layer_id}.json").write_text(json.dumps(payload, indent=2) + "\n")


def point_in_geometry(lon: float, lat: float, geometry: dict[str, Any]) -> bool:
    geom_type = geometry.get("type")
    coords = geometry.get("coordinates", [])
    if geom_type == "Polygon":
        return point_in_polygon(lon, lat, coords)
    if geom_type == "MultiPolygon":
        return any(point_in_polygon(lon, lat, polygon) for polygon in coords)
    return False


def point_in_polygon(lon: float, lat: float, polygon: list[Any]) -> bool:
    if not polygon:
        return False
    if not point_in_ring(lon, lat, polygon[0]):
        return False
    return not any(point_in_ring(lon, lat, hole) for hole in polygon[1:])


def point_in_ring(lon: float, lat: float, ring: list[Any]) -> bool:
    inside = False
    points = [(float(x), float(y)) for x, y, *_ in ring]
    if len(points) < 3:
        return False
    x1, y1 = points[-1]
    for x2, y2 in points:
        intersects = (y1 > lat) != (y2 > lat) and lon < (x2 - x1) * (lat - y1) / ((y2 - y1) or 1e-12) + x1
        if intersects:
            inside = not inside
        x1, y1 = x2, y2
    return inside


def representative_point(geometry: dict[str, Any] | None) -> tuple[float, float] | None:
    if not geometry:
        return None
    points = list(iter_lon_lat(geometry.get("coordinates", [])))
    if not points:
        return None
    return sum(point[0] for point in points) / len(points), sum(point[1] for point in points) / len(points)


def iter_lon_lat(value: Any):
    if isinstance(value, list) and len(value) >= 2 and all(isinstance(item, (int, float)) for item in value[:2]):
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


def clean_properties(props: dict[str, Any]) -> dict[str, str]:
    return {str(key): str(value) for key, value in props.items() if value is not None}


def osm_id(feature: dict[str, Any], props: dict[str, str]) -> str:
    for key in ["@id", "id", "osm_id"]:
        if key in props:
            return props[key]
    feature_id = feature.get("id")
    if feature_id is not None:
        return str(feature_id)
    return hashlib.sha1(json.dumps(feature, sort_keys=True).encode("utf-8")).hexdigest()[:16]


def stable_token(value: str) -> str:
    token = "".join(ch.lower() if ch.isalnum() else "_" for ch in value)
    token = "_".join(part for part in token.split("_") if part)
    return token[:80] or hashlib.sha1(value.encode("utf-8")).hexdigest()[:16]


def parse_float(value: str | None) -> float | None:
    if value in {None, ""}:
        return None
    return float(value)


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    if not ordered:
        return 0.0
    index = min(len(ordered) - 1, max(0, round((len(ordered) - 1) * fraction)))
    return ordered[index]


def mean(values: list[float]) -> float:
    return sum(values) / len(values) if values else 0.0


def weighted_mean(values: list[float], weights: list[float]) -> float:
    total = sum(weights)
    return sum(value * weight for value, weight in zip(values, weights)) / total if total else 0.0


def format_number(value: float) -> str:
    if abs(value - round(value)) < 1.0e-9:
        return str(int(round(value)))
    return f"{value:.6f}".rstrip("0").rstrip(".")


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


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def require_tool(name: str) -> None:
    if shutil.which(name) is None:
        raise SystemExit(f"required tool not found on PATH: {name}")


if __name__ == "__main__":
    main()
