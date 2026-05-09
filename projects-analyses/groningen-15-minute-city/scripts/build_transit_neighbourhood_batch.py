#!/usr/bin/env python3
"""Build a transit batch from mapped neighbourhood centroids to Zernike."""

from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path


STUDY_DIR = Path(__file__).resolve().parents[1]
NEIGHBOURHOODS = STUDY_DIR / "outputs/processed/residential_neighbourhoods.geojson"
SCORES = STUDY_DIR / "outputs/processed/neighbourhood_accessibility_scores.csv"
OUT = STUDY_DIR / "requests/transit/neighbourhood_centres_to_zernike_batch.json"

WINDOWS = {
    "weekday_am_peak": "2026-05-11T08:30:00+02:00",
    "weekday_midday": "2026-05-11T12:30:00+02:00",
    "weekday_evening": "2026-05-11T19:00:00+02:00",
    "weekend_midday": "2026-05-10T12:30:00+02:00",
}

ZERNIKE = {"id": "zernike", "lon": 6.5337, "lat": 53.2406}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--neighbourhoods", type=Path, default=NEIGHBOURHOODS)
    parser.add_argument("--scores", type=Path, default=SCORES)
    parser.add_argument("--out", type=Path, default=OUT)
    parser.add_argument("--max-neighbourhoods", type=int)
    args = parser.parse_args()

    scored_ids = load_scored_neighbourhood_ids(args.scores)
    features = load_neighbourhood_features(args.neighbourhoods, scored_ids)
    if args.max_neighbourhoods:
        features = features[: args.max_neighbourhoods]

    requests = []
    for feature in features:
        props = feature["properties"]
        origin_id = props["id"]
        for window_id, datetime in WINDOWS.items():
            requests.append(
                {
                    "route_id": f"{window_id}_{origin_id}_to_zernike",
                    "origin": {
                        "id": origin_id,
                        "lon": props["centroid_lon"],
                        "lat": props["centroid_lat"],
                    },
                    "destination": ZERNIKE,
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


if __name__ == "__main__":
    main()
