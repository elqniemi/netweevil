#!/usr/bin/env python3
"""Join LandsD MTR platform polygons to the 3D Indoor Network and MTR codes."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import re
import shutil
import sys
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

try:
    from osgeo import ogr, osr
except ImportError as error:  # pragma: no cover - environment diagnostic
    raise SystemExit("GDAL Python bindings (osgeo) are required") from error

ogr.UseExceptions()


LINE_NAMES = {
    "AEL": ("Airport Express",),
    "DRL": ("Disneyland Resort Line", "Disneyland Line"),
    "EAL": ("East Rail Line",),
    "ISL": ("Island Line",),
    "KTL": ("Kwun Tong Line",),
    "SIL": ("South Island Line",),
    "TCL": ("Tung Chung Line",),
    "TKL": ("Tseung Kwan O Line",),
    "TML": ("Tuen Ma Line",),
    "TWL": ("Tsuen Wan Line",),
}


@dataclass(frozen=True)
class StationLine:
    line_code: str
    station_code: str
    station_id: str
    name_en: str
    name_zh: str
    directions: tuple[str, ...]


@dataclass
class NetworkEdge:
    object_id: int
    route_id: int
    terminal_id: int | None
    terminal_name: str
    floor_id: int | None
    floor_polygon_id: str
    points: list[list[tuple[float, float, float]]]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--downloads", type=Path, required=True)
    parser.add_argument("--indoor-network", type=Path, required=True)
    parser.add_argument("--mtr-lines", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--feed-id", default="hk_mtr_platforms")
    parser.add_argument("--z-window-m", type=float, default=2.0)
    parser.add_argument("--check-only", action="store_true")
    return parser.parse_args()


def normalize(value: str | None) -> str:
    value = (value or "").lower().replace("&", "and")
    value = re.sub(r"\btseun\b", "tsuen", value)
    value = re.sub(r"\bstation\b", "", value)
    return re.sub(r"[^a-z0-9\u3400-\u9fff]+", "", value)


def nested_name(properties: dict[str, Any], language: str) -> str:
    value = properties.get("name")
    if isinstance(value, dict):
        return str(value.get(language) or "").strip()
    return str(value or "").strip()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_station_lines(path: Path) -> tuple[dict[str, list[StationLine]], list[dict[str, str]]]:
    with path.open("r", encoding="utf-8-sig", newline="") as handle:
        source_rows = list(csv.DictReader(handle))

    grouped: dict[tuple[str, str], list[dict[str, str]]] = defaultdict(list)
    for row in source_rows:
        if not row.get("Line Code", "").strip() or not row.get("English Name", "").strip():
            continue
        grouped[(normalize(row["English Name"]), row["Line Code"].strip())].append(row)

    by_station: dict[str, list[StationLine]] = defaultdict(list)
    for (station_key, line_code), rows in grouped.items():
        station_codes = {row["Station Code"].strip() for row in rows}
        station_ids = {row["Station ID"].strip() for row in rows}
        if len(station_codes) != 1 or len(station_ids) != 1:
            raise RuntimeError(f"inconsistent MTR identifiers for {station_key}/{line_code}")
        first = rows[0]
        by_station[station_key].append(
            StationLine(
                line_code=line_code,
                station_code=next(iter(station_codes)),
                station_id=next(iter(station_ids)),
                name_en=first["English Name"].strip(),
                name_zh=first["Chinese Name"].strip(),
                directions=tuple(sorted({row["Direction"].strip() for row in rows})),
            )
        )
    for lines in by_station.values():
        lines.sort(key=lambda line: line.line_code)
    # Racecourse is an operational East Rail branch station and appears in the
    # MTR Next Train code list as RAC, but is omitted from this topology CSV.
    by_station[normalize("Racecourse")].append(
        StationLine(
            line_code="EAL",
            station_code="RAC",
            station_id="RAC",
            name_en="Racecourse",
            name_zh="馬場",
            directions=("DT", "UT"),
        )
    )
    return dict(by_station), source_rows


def locate_dataset(extracted_to: str) -> Path:
    root = Path(extracted_to)
    direct = root / "unit.geojson"
    if direct.is_file():
        return root
    matches = sorted(root.glob("*/unit.geojson"))
    if len(matches) != 1:
        raise RuntimeError(f"expected one unit.geojson below {root}, found {len(matches)}")
    return matches[0].parent


def load_downloaded_stations(downloads: Path) -> list[dict[str, Any]]:
    manifest_path = downloads / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if (manifest.get("failure_count") != 0
            or not manifest.get("station_count")
            or manifest.get("downloaded_count") != manifest.get("station_count")):
        raise RuntimeError(f"download manifest is incomplete: {manifest_path}")
    resources = manifest["resources"]
    for resource in resources:
        # Manifests move with datasets between machines. Resolve the archived
        # relative layout before falling back to the historical absolute path.
        archived = Path(resource.get("archive", ""))
        local_root = downloads / "extracted" / archived.stem
        extracted = local_root if local_root.is_dir() else Path(resource["extracted_to"])
        resource["dataset_dir"] = str(locate_dataset(str(extracted)))
    return resources


def geometry_lines(geometry: ogr.Geometry) -> list[list[tuple[float, float, float]]]:
    flat_type = ogr.GT_Flatten(geometry.GetGeometryType())
    if flat_type == ogr.wkbLineString:
        return [
            [
                (
                    float(geometry.GetX(index)),
                    float(geometry.GetY(index)),
                    float(geometry.GetZ(index)),
                )
                for index in range(geometry.GetPointCount())
            ]
        ]
    lines: list[list[tuple[float, float, float]]] = []
    for index in range(geometry.GetGeometryCount()):
        lines.extend(geometry_lines(geometry.GetGeometryRef(index)))
    return lines


def load_network(
    path: Path,
) -> tuple[
    dict[str, list[NetworkEdge]],
    dict[tuple[str, str], list[NetworkEdge]],
    osr.SpatialReference,
]:
    dataset = ogr.Open(str(path), 0)
    if dataset is None:
        raise RuntimeError(f"cannot open indoor network: {path}")
    layer = dataset.GetLayerByName("indoor_pedestrian_route")
    if layer is None:
        raise RuntimeError("indoor network has no indoor_pedestrian_route layer")
    source_srs = layer.GetSpatialRef().Clone()
    source_srs.SetAxisMappingStrategy(osr.OAMS_TRADITIONAL_GIS_ORDER)

    by_station: dict[str, list[NetworkEdge]] = defaultdict(list)
    by_floor: dict[tuple[str, str], list[NetworkEdge]] = defaultdict(list)
    for feature in layer:
        geometry = feature.GetGeometryRef()
        if geometry is None:
            continue
        terminal_name = str(feature.GetField("terminal_name_en") or "").strip()
        station_key = normalize(terminal_name)
        edge = NetworkEdge(
            object_id=int(feature.GetFID()),
            route_id=int(feature.GetField("PedestrianRouteID")),
            terminal_id=(
                int(feature.GetField("TerminalID"))
                if feature.GetField("TerminalID") is not None
                else None
            ),
            terminal_name=terminal_name,
            floor_id=(
                int(feature.GetField("FloorID"))
                if feature.GetField("FloorID") is not None
                else None
            ),
            floor_polygon_id=str(feature.GetField("floor_polygon_id") or "").strip(),
            points=geometry_lines(geometry),
        )
        by_station[station_key].append(edge)
        by_floor[(station_key, edge.floor_polygon_id)].append(edge)
    dataset = None
    return dict(by_station), dict(by_floor), source_srs


def edit_distance(left: str, right: str) -> int:
    previous = list(range(len(right) + 1))
    for left_index, left_char in enumerate(left, start=1):
        current = [left_index]
        for right_index, right_char in enumerate(right, start=1):
            current.append(
                min(
                    current[-1] + 1,
                    previous[right_index] + 1,
                    previous[right_index - 1] + (left_char != right_char),
                )
            )
        previous = current
    return previous[-1]


def match_floor_id(map_floor_id: str, network_floor_ids: Iterable[str]) -> tuple[str | None, str]:
    network_floor_ids = sorted({value for value in network_floor_ids if value})
    if map_floor_id in network_floor_ids:
        return map_floor_id, "exact"
    if map_floor_id[:-1] in network_floor_ids:
        return map_floor_id[:-1], "map_trailing_digit_removed"
    distances = [(edit_distance(map_floor_id, value), value) for value in network_floor_ids]
    if not distances:
        return None, "unmatched"
    distances.sort()
    best_distance, best_value = distances[0]
    if best_distance <= 1 and (len(distances) == 1 or distances[1][0] > best_distance):
        return best_value, "unique_edit_distance_1"
    return None, "unmatched"


def nearest_point_on_network(
    x: float,
    y: float,
    edges: Iterable[NetworkEdge],
) -> tuple[NetworkEdge, float, float, float, float]:
    best: tuple[float, NetworkEdge, float, float, float] | None = None
    for edge in edges:
        for line in edge.points:
            if len(line) == 1:
                px, py, pz = line[0]
                distance = math.hypot(x - px, y - py)
                candidate = (distance, edge, px, py, pz)
                if best is None or candidate[0] < best[0]:
                    best = candidate
                continue
            for start, end in zip(line, line[1:]):
                ax, ay, az = start
                bx, by, bz = end
                dx, dy = bx - ax, by - ay
                length_sq = dx * dx + dy * dy
                fraction = 0.0 if length_sq == 0.0 else ((x - ax) * dx + (y - ay) * dy) / length_sq
                fraction = min(1.0, max(0.0, fraction))
                px = ax + fraction * dx
                py = ay + fraction * dy
                pz = az + fraction * (bz - az)
                distance = math.hypot(x - px, y - py)
                candidate = (distance, edge, px, py, pz)
                if best is None or candidate[0] < best[0]:
                    best = candidate
    if best is None:
        raise RuntimeError("matched network floor contains no usable line geometry")
    distance, edge, px, py, pz = best
    return edge, px, py, pz, distance


def infer_lines(level_name: str, station_lines: list[StationLine]) -> tuple[list[str], str]:
    normalized_level = normalize(level_name)
    matched = [
        line.line_code
        for line in station_lines
        if any(normalize(alias) in normalized_level for alias in LINE_NAMES.get(line.line_code, ()))
    ]
    if matched:
        return sorted(set(matched)), "level_name"
    if len(station_lines) == 1:
        return [station_lines[0].line_code], "single_line_station"
    return (
        sorted({line.line_code for line in station_lines}),
        "interchange_shared_or_unlabelled",
    )


def centroid_lon_lat(feature: dict[str, Any]) -> tuple[float, float]:
    geometry = ogr.CreateGeometryFromJson(json.dumps(feature["geometry"]))
    if geometry is None or geometry.IsEmpty():
        raise RuntimeError("platform feature has empty geometry")
    centroid = geometry.Centroid()
    return centroid.GetX(), centroid.GetY()


def load_platform_candidates(dataset_dir: Path) -> list[dict[str, Any]]:
    levels_doc = json.loads((dataset_dir / "level.geojson").read_text(encoding="utf-8"))
    units_doc = json.loads((dataset_dir / "unit.geojson").read_text(encoding="utf-8"))
    levels = {str(feature["id"]): feature for feature in levels_doc["features"]}
    candidates: list[dict[str, Any]] = []
    unit_level_ids: set[str] = set()
    for unit in units_doc["features"]:
        if str(unit.get("properties", {}).get("category") or "").lower() != "platform":
            continue
        level_id = str(unit["properties"].get("level_id") or "")
        level = levels.get(level_id)
        if level is None:
            raise RuntimeError(f"platform unit references unknown level {level_id}")
        unit_level_ids.add(level_id)
        candidates.append(
            {
                "source_kind": "unit",
                "source_id": str(unit.get("id") or unit["properties"].get("UnitPolyID") or ""),
                "feature": unit,
                "level": level,
            }
        )
    for level_id, level in levels.items():
        level_name = nested_name(level["properties"], "en")
        if "platform" in level_name.lower() and level_id not in unit_level_ids:
            candidates.append(
                {
                    "source_kind": "platform_level_fallback",
                    "source_id": level_id,
                    "feature": level,
                    "level": level,
                }
            )
    return candidates


def build_join(
    resources: list[dict[str, Any]],
    station_lines_by_name: dict[str, list[StationLine]],
    network_by_station: dict[str, list[NetworkEdge]],
    network_by_floor: dict[tuple[str, str], list[NetworkEdge]],
    network_srs: osr.SpatialReference,
    z_window_m: float,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], dict[str, Any]]:
    wgs84 = osr.SpatialReference()
    wgs84.ImportFromEPSG(4326)
    wgs84.SetAxisMappingStrategy(osr.OAMS_TRADITIONAL_GIS_ORDER)
    to_network = osr.CoordinateTransformation(wgs84, network_srs)
    to_wgs84 = osr.CoordinateTransformation(network_srs, wgs84)

    rows: list[dict[str, Any]] = []
    joined_features: list[dict[str, Any]] = []
    qa: dict[str, Any] = {
        "station_count": len(resources),
        "stations_without_mtr_code": [],
        "stations_without_network": [],
        "stations_without_platform_candidates": [],
        "unmatched_platform_floors": [],
        "shared_interchange_platforms": [],
    }

    for resource in sorted(resources, key=lambda item: item["venue_name_en"]):
        station_name = resource["venue_name_en"]
        station_key = normalize(station_name)
        station_lines = station_lines_by_name.get(station_key, [])
        if not station_lines:
            qa["stations_without_mtr_code"].append(station_name)
            continue
        station_edges = network_by_station.get(station_key, [])
        if not station_edges:
            qa["stations_without_network"].append(station_name)
            continue
        candidates = load_platform_candidates(Path(resource["dataset_dir"]))
        if not candidates:
            qa["stations_without_platform_candidates"].append(station_name)
            continue

        network_floor_ids = {edge.floor_polygon_id for edge in station_edges if edge.floor_polygon_id}
        for candidate_index, candidate in enumerate(candidates, start=1):
            feature = candidate["feature"]
            level = candidate["level"]
            level_properties = level["properties"]
            center_lon, center_lat = centroid_lon_lat(feature)
            center_x, center_y, _ = to_network.TransformPoint(center_lon, center_lat)
            map_floor_id = str(level_properties.get("FloorPolyID") or "")
            network_floor_id, floor_match_method = match_floor_id(map_floor_id, network_floor_ids)
            if network_floor_id is None:
                fallback_edge, _, _, _, fallback_distance = nearest_point_on_network(
                    center_x, center_y, station_edges
                )
                if fallback_distance <= 50.0 and fallback_edge.floor_polygon_id:
                    network_floor_id = fallback_edge.floor_polygon_id
                    floor_match_method = "same_station_spatial_fallback"
                else:
                    qa["unmatched_platform_floors"].append(
                        {
                            "station": station_name,
                            "source_id": candidate["source_id"],
                            "map_floor_polygon_id": map_floor_id,
                            "nearest_same_station_network_m": fallback_distance,
                        }
                    )
                    continue

            level_name_en = nested_name(level_properties, "en")
            level_name_zh = nested_name(level_properties, "zh")
            line_codes, line_match_method = infer_lines(level_name_en, station_lines)
            if line_match_method == "interchange_shared_or_unlabelled":
                qa["shared_interchange_platforms"].append(
                    {
                        "station": station_name,
                        "source_id": candidate["source_id"],
                        "level_name_en": level_name_en,
                        "station_lines": [line.line_code for line in station_lines],
                    }
                )
            floor_edges = network_by_floor[(station_key, network_floor_id)]
            edge, point_x, point_y, point_z, snap_distance = nearest_point_on_network(
                center_x, center_y, floor_edges
            )
            point_lon, point_lat, _ = to_wgs84.TransformPoint(point_x, point_y)
            short_name_value = level_properties.get("short_name")
            short_name = (
                str(short_name_value.get("en") or "")
                if isinstance(short_name_value, dict)
                else str(short_name_value or "")
            )

            station_line_map = {line.line_code: line for line in station_lines}
            for line_code in line_codes:
                station_line = station_line_map[line_code]
                stop_id = (
                    f"MTR_{line_code}_{station_line.station_code}_"
                    f"{normalize(short_name or str(level_properties.get('ordinal', 'level'))).upper() or 'LEVEL'}_"
                    f"{candidate_index:02d}"
                )
                row = {
                    "stop_id": stop_id,
                    "venue_id": resource["venue_id"],
                    "station_code": station_line.station_code,
                    "station_id": station_line.station_id,
                    "station_name_en": station_line.name_en,
                    "station_name_zh": station_line.name_zh,
                    "line_code": line_code,
                    "directions": "|".join(station_line.directions),
                    "platform_direction_status": (
                        "shared_or_unlabelled_platform_geometry"
                        if line_match_method == "interchange_shared_or_unlabelled"
                        else "shared_platform_geometry"
                    ),
                    "platform_source_kind": candidate["source_kind"],
                    "platform_source_id": candidate["source_id"],
                    "level_id": str(level.get("id") or ""),
                    "level_name_en": level_name_en,
                    "level_name_zh": level_name_zh,
                    "level_short_name": short_name,
                    "level_ordinal": level_properties.get("ordinal"),
                    "map_floor_polygon_id": map_floor_id,
                    "network_floor_polygon_id": network_floor_id,
                    "network_floor_id": edge.floor_id,
                    "terminal_id": edge.terminal_id,
                    "pedestrian_route_id": edge.route_id,
                    "binding_lon": point_lon,
                    "binding_lat": point_lat,
                    "binding_z": point_z,
                    "platform_centroid_lon": center_lon,
                    "platform_centroid_lat": center_lat,
                    "centroid_to_network_m": snap_distance,
                    "z_window_m": z_window_m,
                    "floor_match_method": floor_match_method,
                    "line_match_method": line_match_method,
                }
                rows.append(row)
                joined_feature = json.loads(json.dumps(feature))
                joined_feature["properties"] = {**feature.get("properties", {}), **row}
                joined_features.append(joined_feature)

    qa["joined_platform_count"] = len(rows)
    qa["joined_station_count"] = len({row["station_code"] for row in rows})
    qa["joined_station_line_count"] = len(
        {(row["station_code"], row["line_code"]) for row in rows}
    )
    expected_station_lines = {
        (line.station_code, line.line_code)
        for station_lines in station_lines_by_name.values()
        for line in station_lines
    }
    joined_station_lines = {(row["station_code"], row["line_code"]) for row in rows}
    qa["missing_station_lines"] = [
        {"station_code": station_code, "line_code": line_code}
        for station_code, line_code in sorted(expected_station_lines - joined_station_lines)
    ]
    qa["max_centroid_to_network_m"] = max(
        (row["centroid_to_network_m"] for row in rows), default=None
    )
    qa["qa_passed"] = not any(
        qa[key]
        for key in (
            "stations_without_mtr_code",
            "stations_without_network",
            "stations_without_platform_candidates",
            "unmatched_platform_floors",
            "missing_station_lines",
        )
    )
    return rows, joined_features, qa


def write_csv(path: Path, rows: list[dict[str, Any]]) -> None:
    with path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def write_bindings(path: Path, feed_id: str, rows: list[dict[str, Any]]) -> None:
    document = {
        "schema_version": 1,
        "feed_id": feed_id,
        "bindings": [
            {
                "stop_id": row["stop_id"],
                "coordinate": {
                    "lon": row["binding_lon"],
                    "lat": row["binding_lat"],
                    "z": row["binding_z"],
                    # Nearest-Z selection distinguishes stacked platforms. A
                    # fixed window is intentionally omitted because the source
                    # and routable line Z values can use different facility
                    # reference surfaces within the same platform level.
                    "attribute_filter": {"MTRPlatformLevel": "1"},
                },
            }
            for row in rows
        ],
    }
    path.write_text(json.dumps(document, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def write_gtfs_stops(path: Path, rows: list[dict[str, Any]]) -> None:
    fieldnames = [
        "stop_id",
        "stop_code",
        "stop_name",
        "stop_desc",
        "stop_lat",
        "stop_lon",
        "location_type",
        "parent_station",
        "wheelchair_boarding",
    ]
    platform_rows: list[dict[str, Any]] = []
    grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        parent = f"MTR_STATION_{row['station_code']}"
        grouped[parent].append(row)
        platform_rows.append(
            {
                "stop_id": row["stop_id"],
                "stop_code": row["station_code"],
                "stop_name": f"{row['station_name_en']} — {row['line_code']} {row['level_short_name']}",
                "stop_desc": row["level_name_en"],
                "stop_lat": row["binding_lat"],
                "stop_lon": row["binding_lon"],
                "location_type": 0,
                "parent_station": parent,
                "wheelchair_boarding": "",
            }
        )
    parents = []
    for parent, children in sorted(grouped.items()):
        first = children[0]
        parents.append(
            {
                "stop_id": parent,
                "stop_code": first["station_code"],
                "stop_name": first["station_name_en"],
                "stop_desc": "MTR station",
                "stop_lat": sum(row["binding_lat"] for row in children) / len(children),
                "stop_lon": sum(row["binding_lon"] for row in children) / len(children),
                "location_type": 1,
                "parent_station": "",
                "wheelchair_boarding": "",
            }
        )
    with path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(parents + platform_rows)


def enrich_network(source: Path, destination: Path, rows: list[dict[str, Any]]) -> None:
    for candidate in (
        destination,
        Path(f"{destination}-wal"),
        Path(f"{destination}-shm"),
    ):
        candidate.unlink(missing_ok=True)
    shutil.copy2(source, destination)
    station_by_terminal: dict[int, list[dict[str, Any]]] = defaultdict(list)
    platform_by_floor: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        if row["terminal_id"] is not None:
            station_by_terminal[int(row["terminal_id"])].append(row)
        platform_by_floor[row["network_floor_polygon_id"]].append(row)

    dataset = ogr.Open(str(destination), 1)
    if dataset is None:
        raise RuntimeError(f"cannot open derived network for update: {destination}")
    layer = dataset.GetLayerByName("indoor_pedestrian_route")
    if layer is None:
        raise RuntimeError("derived network has no indoor_pedestrian_route layer")
    existing = {
        layer.GetLayerDefn().GetFieldDefn(index).GetName()
        for index in range(layer.GetLayerDefn().GetFieldCount())
    }
    columns = {
        "MTRVenueID": ogr.OFTString,
        "MTRStationCode": ogr.OFTString,
        "MTRStationID": ogr.OFTString,
        "MTRLineCodes": ogr.OFTString,
        "MTRPlatformLevel": ogr.OFTInteger,
        "MTRPlatformLineCodes": ogr.OFTString,
        "MTRPlatformSourceIDs": ogr.OFTString,
        "MTRPlatformStopIDs": ogr.OFTString,
    }
    for name, field_type in columns.items():
        if name not in existing:
            layer.CreateField(ogr.FieldDefn(name, field_type))

    layer.ResetReading()
    layer.StartTransaction()
    try:
        for feature in layer:
            terminal_id = feature.GetField("TerminalID")
            station_rows = (
                station_by_terminal.get(int(terminal_id), [])
                if terminal_id is not None
                else []
            )
            if station_rows:
                first = station_rows[0]
                feature.SetField("MTRVenueID", first["venue_id"])
                feature.SetField("MTRStationCode", first["station_code"])
                feature.SetField("MTRStationID", first["station_id"])
                feature.SetField(
                    "MTRLineCodes",
                    json.dumps(sorted({row["line_code"] for row in station_rows})),
                )
            floor_polygon_id = str(feature.GetField("floor_polygon_id") or "")
            platform_rows = platform_by_floor.get(floor_polygon_id, [])
            feature.SetField("MTRPlatformLevel", 1 if platform_rows else 0)
            if platform_rows:
                feature.SetField(
                    "MTRPlatformLineCodes",
                    json.dumps(sorted({row["line_code"] for row in platform_rows})),
                )
                feature.SetField(
                    "MTRPlatformSourceIDs",
                    json.dumps(sorted({row["platform_source_id"] for row in platform_rows})),
                )
                feature.SetField(
                    "MTRPlatformStopIDs",
                    json.dumps(sorted({row["stop_id"] for row in platform_rows})),
                )
            layer.SetFeature(feature)
        layer.CommitTransaction()
    except Exception:
        layer.RollbackTransaction()
        raise
    dataset = None


def main() -> int:
    args = parse_args()
    resources = load_downloaded_stations(args.downloads)
    station_lines, _ = load_station_lines(args.mtr_lines)
    network_by_station, network_by_floor, network_srs = load_network(args.indoor_network)
    rows, features, qa = build_join(
        resources,
        station_lines,
        network_by_station,
        network_by_floor,
        network_srs,
        args.z_window_m,
    )
    print(json.dumps(qa, ensure_ascii=False, indent=2))
    if args.check_only:
        return 0 if qa["qa_passed"] else 1
    if not qa["qa_passed"]:
        print("platform join is incomplete; refusing to write production outputs", file=sys.stderr)
        return 1
    if not rows:
        raise RuntimeError("platform join produced no rows")

    args.output_dir.mkdir(parents=True, exist_ok=True)
    write_csv(args.output_dir / "mtr_platform_join.csv", rows)
    (args.output_dir / "mtr_platforms_joined.geojson").write_text(
        json.dumps(
            {"type": "FeatureCollection", "features": features},
            ensure_ascii=False,
            separators=(",", ":"),
        )
        + "\n",
        encoding="utf-8",
    )
    write_bindings(args.output_dir / "netweevil_stop_bindings.json", args.feed_id, rows)
    write_gtfs_stops(args.output_dir / "gtfs_stops.txt", rows)
    enrich_network(
        args.indoor_network,
        args.output_dir / "3D_Indoor_Network_MTR_Platforms.gpkg",
        rows,
    )
    qa["inputs"] = {
        "downloads_manifest": str((args.downloads / "manifest.json").resolve()),
        "downloads_manifest_sha256": sha256_file(args.downloads / "manifest.json"),
        "indoor_network": str(args.indoor_network.resolve()),
        "indoor_network_sha256": sha256_file(args.indoor_network),
        "mtr_lines": str(args.mtr_lines.resolve()),
        "mtr_lines_sha256": sha256_file(args.mtr_lines),
    }
    (args.output_dir / "qa_report.json").write_text(
        json.dumps(qa, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(f"Wrote complete platform join to {args.output_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
