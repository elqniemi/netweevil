#!/usr/bin/env python3
"""Build sampled transit requests from neighbourhood centres to nearest service categories."""

from __future__ import annotations

import argparse
import csv
import json
import math
from pathlib import Path


STUDY_DIR = Path(__file__).resolve().parents[1]
NEIGHBOURHOODS = STUDY_DIR / "outputs/processed/residential_neighbourhoods.geojson"
SCORES = STUDY_DIR / "outputs/processed/neighbourhood_accessibility_scores.csv"
DESTINATION_DIR = STUDY_DIR / "requests/destinations"
OUT = STUDY_DIR / "requests/transit/neighbourhood_service_bundle_sample.json"

BASELINE_CATEGORIES = [
    "food_retail",
    "education",
    "healthcare",
    "public_services",
    "parks_recreation",
    "transit_stops",
]
WINDOWS = {
    "weekday_am_peak": "2026-05-11T08:30:00+02:00",
    "weekday_midday": "2026-05-11T12:30:00+02:00",
    "weekday_evening": "2026-05-11T19:00:00+02:00",
    "weekend_midday": "2026-05-10T12:30:00+02:00",
}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--neighbourhoods", type=Path, default=NEIGHBOURHOODS)
    parser.add_argument("--scores", type=Path, default=SCORES)
    parser.add_argument("--destination-dir", type=Path, default=DESTINATION_DIR)
    parser.add_argument("--out", type=Path, default=OUT)
    parser.add_argument("--max-neighbourhoods", type=int, default=50)
    args = parser.parse_args()

    scored_ids = load_scored_neighbourhood_ids(args.scores)
    features = load_neighbourhood_features(args.neighbourhoods, scored_ids)
    if args.max_neighbourhoods:
        features = features[: args.max_neighbourhoods]
    destinations = {
        category: load_destinations(args.destination_dir / f"{category}.csv")
        for category in BASELINE_CATEGORIES
    }

    requests = []
    for feature in features:
        props = feature["properties"]
        origin_id = props["id"]
        origin = {
            "id": origin_id,
            "lon": props["centroid_lon"],
            "lat": props["centroid_lat"],
        }
        for category, destination_rows in destinations.items():
            nearest = nearest_destination(origin, destination_rows)
            if nearest is None:
                continue
            destination = {
                "id": nearest["id"],
                "lon": nearest["lon"],
                "lat": nearest["lat"],
            }
            for window_id, datetime in WINDOWS.items():
                requests.append(
                    {
                        "route_id": f"{window_id}__{origin_id}__{category}__{nearest['id']}",
                        "origin": origin,
                        "destination": destination,
                        "time": {
                            "datetime": datetime,
                            "arrive_by": False,
                            "search_window_s": 7200,
                        },
                        "modes": {
                            "access": ["walk"],
                            "egress": ["walk"],
                            "transit": ["bus", "tram", "rail"],
                            "walk_speed_kph": 4.8,
                            "max_access_distance_m": 1200.0,
                            "max_egress_distance_m": 1200.0,
                            "max_transfer_distance_m": 500.0,
                            "board_slack_s": 30,
                            "transfer_slack_s": 120,
                            "max_transfers": 3,
                        },
                        "returns": {
                            "include_geometry": False,
                            "walking_geometry": "straight_line",
                            "include_stops": False,
                            "include_stop_segments": False,
                        },
                    }
                )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps({"requests": requests}, indent=2) + "\n")


def load_scored_neighbourhood_ids(path: Path) -> set[str]:
    with path.open(newline="") as handle:
        return {row["neighbourhood_id"] for row in csv.DictReader(handle) if row.get("neighbourhood_id")}


def load_neighbourhood_features(path: Path, scored_ids: set[str]) -> list[dict]:
    with path.open() as handle:
        collection = json.load(handle)
    features = [
        feature
        for feature in collection.get("features", [])
        if feature.get("properties", {}).get("id") in scored_ids
    ]
    features.sort(key=lambda feature: feature["properties"].get("name", ""))
    return features


def load_destinations(path: Path) -> list[dict[str, object]]:
    rows = []
    with path.open(newline="") as handle:
        for row in csv.DictReader(handle):
            lon = parse_float(row.get("lon") or row.get("x"))
            lat = parse_float(row.get("lat") or row.get("y"))
            if row.get("id") and lon is not None and lat is not None:
                rows.append({"id": row["id"], "lon": lon, "lat": lat})
    return rows


def nearest_destination(origin: dict[str, object], destinations: list[dict[str, object]]) -> dict[str, object] | None:
    if not destinations:
        return None
    lon = float(origin["lon"])
    lat = float(origin["lat"])
    return min(destinations, key=lambda row: distance_m(lon, lat, float(row["lon"]), float(row["lat"])))


def distance_m(lon1: float, lat1: float, lon2: float, lat2: float) -> float:
    mean_lat = math.radians((lat1 + lat2) / 2)
    x = math.radians(lon2 - lon1) * math.cos(mean_lat)
    y = math.radians(lat2 - lat1)
    return math.hypot(x, y) * 6_371_000


def parse_float(value: str | None) -> float | None:
    if value in {None, ""}:
        return None
    try:
        return float(value)
    except ValueError:
        return None


if __name__ == "__main__":
    main()
