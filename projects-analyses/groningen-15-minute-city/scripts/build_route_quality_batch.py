#!/usr/bin/env python3
"""Build a cycling route-batch request from nearest-service summaries."""

from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path


STUDY_DIR = Path(__file__).resolve().parents[1]
NEAREST_CSV = STUDY_DIR / "outputs/processed/accessibility_nearest_services.csv"
ORIGINS_CSV = STUDY_DIR / "requests/origins/residential_h3_r9_points.csv"
DEST_DIR = STUDY_DIR / "requests/destinations"
OUT_JSON = STUDY_DIR / "requests/route_batches/cycling_nearest_required_services.json"

BASELINE_CATEGORIES = [
    "food_retail",
    "education",
    "healthcare",
    "public_services",
    "parks_recreation",
    "transit_stops",
]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--nearest", type=Path, default=NEAREST_CSV)
    parser.add_argument("--origins", type=Path, default=ORIGINS_CSV)
    parser.add_argument("--destinations-dir", type=Path, default=DEST_DIR)
    parser.add_argument("--out", type=Path, default=OUT_JSON)
    parser.add_argument("--mode", default="cycling")
    parser.add_argument("--origin-id", action="append", default=[])
    parser.add_argument("--max-origins", type=int, default=200)
    args = parser.parse_args()

    origins = load_points(args.origins)
    destinations = load_destinations(args.destinations_dir)
    selected_origin_ids = set(args.origin_id)
    if not selected_origin_ids:
        selected_origin_ids = set(list(origins)[: args.max_origins])

    requests = []
    with args.nearest.open(newline="") as handle:
        for row in csv.DictReader(handle):
            if row.get("mode") != args.mode:
                continue
            origin_id = row.get("origin_id", "")
            category = row.get("category", "")
            destination_id = row.get("nearest_destination_id", "")
            if category not in BASELINE_CATEGORIES:
                continue
            if origin_id not in selected_origin_ids or not destination_id:
                continue
            origin = origins.get(origin_id)
            destination = destinations.get(destination_id)
            if origin is None or destination is None:
                continue
            requests.append(
                {
                    "request": {
                        "route_id": f"{args.mode}_{origin_id}_to_{category}_{destination_id}",
                        "origin": {
                            "id": origin_id,
                            "lon": origin["lon"],
                            "lat": origin["lat"],
                        },
                        "destination": {
                            "id": destination_id,
                            "lon": destination["lon"],
                            "lat": destination["lat"],
                        },
                        "snap": {"max_distance_m": 500.0},
                        "connectivity": {"disconnected": "strict"},
                        "fallback": {
                            "allow_reverse_oneway": False,
                            "allow_illegal_turn": False,
                            "ignore_turn_restrictions": False,
                            "allow_uturn_where_normally_forbidden": False,
                        },
                        "returns": {
                            "geometry": "full",
                            "segment_rows": True,
                            "road_type_breakdown": ["time_s", "distance_m"],
                            "surface_breakdown": ["time_s", "distance_m"],
                            "penalty_breakdown": True,
                            "explain_cost_derivation": True,
                        },
                    }
                }
            )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w") as handle:
        json.dump({"requests": requests}, handle, indent=2)
        handle.write("\n")


def load_points(path: Path) -> dict[str, dict[str, float]]:
    with path.open(newline="") as handle:
        return {
            row["id"]: {"lon": float(row.get("x") or row.get("lon")), "lat": float(row.get("y") or row.get("lat"))}
            for row in csv.DictReader(handle)
            if row.get("id")
        }


def load_destinations(dest_dir: Path) -> dict[str, dict[str, float]]:
    points = {}
    for path in sorted(dest_dir.glob("*.csv")):
        points.update(load_points(path))
    return points


if __name__ == "__main__":
    main()
