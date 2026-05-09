#!/usr/bin/env python3
"""Summarize representative cycling route-quality diagnostics."""

from __future__ import annotations

import argparse
import csv
import json
import sqlite3
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from statistics import median


STUDY_DIR = Path(__file__).resolve().parents[1]
GPKG = STUDY_DIR / "outputs/raw/cycling_nearest_required_services.gpkg"
OUT_DIR = STUDY_DIR / "outputs/processed"
META_DIR = STUDY_DIR / "metadata/generated"

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
    parser.add_argument("--gpkg", type=Path, default=GPKG)
    parser.add_argument("--out-dir", type=Path, default=OUT_DIR)
    parser.add_argument("--summary", type=Path, default=META_DIR / "cycling_route_quality_summary.json")
    args = parser.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)
    args.summary.parent.mkdir(parents=True, exist_ok=True)

    routes = load_routes(args.gpkg)
    road_rows = load_breakdown(args.gpkg, "route_breakdown_road_class", "road_class")
    surface_rows = load_breakdown(args.gpkg, "route_breakdown_surface", "surface")

    category_rows = summarize_routes(routes)
    road_summary = summarize_breakdown(road_rows, "road_class")
    surface_summary = summarize_breakdown(surface_rows, "surface")

    write_rows(args.out_dir / "cycling_route_quality_summary.csv", category_rows)
    write_rows(args.out_dir / "cycling_route_quality_road_class.csv", road_summary)
    write_rows(args.out_dir / "cycling_route_quality_surface.csv", surface_summary)

    payload = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "source": str(args.gpkg),
        "route_count": len(routes),
        "category_summary": category_rows,
        "road_class_summary": road_summary,
        "surface_summary": surface_summary,
    }
    args.summary.write_text(json.dumps(payload, indent=2) + "\n")


def load_routes(path: Path) -> list[dict[str, object]]:
    with sqlite3.connect(path) as connection:
        connection.row_factory = sqlite3.Row
        rows = connection.execute(
            """
            SELECT route_id, origin_id, destination_id, outcome, fallback_used,
                   origin_snap_distance_m, destination_snap_distance_m,
                   total_distance_m, total_travel_time_s, total_generalized_cost,
                   violation_count, segment_count
            FROM routes
            ORDER BY request_index
            """
        ).fetchall()
    return [dict(row) | {"category": infer_category(row["route_id"])} for row in rows]


def load_breakdown(path: Path, table: str, label_column: str) -> list[dict[str, object]]:
    with sqlite3.connect(path) as connection:
        connection.row_factory = sqlite3.Row
        rows = connection.execute(
            f"""
            SELECT route_id, {label_column} AS label, distance_m, time_s
            FROM {table}
            ORDER BY route_id, label
            """
        ).fetchall()
    return [dict(row) | {"category": infer_category(row["route_id"])} for row in rows]


def infer_category(route_id: str) -> str:
    marker = "_to_"
    suffix = route_id.split(marker, 1)[1] if marker in route_id else route_id
    for category in sorted(BASELINE_CATEGORIES, key=len, reverse=True):
        if suffix.startswith(f"{category}_"):
            return category
    return "unknown"


def summarize_routes(routes: list[dict[str, object]]) -> list[dict[str, str]]:
    grouped: dict[str, list[dict[str, object]]] = defaultdict(list)
    for route in routes:
        grouped[str(route["category"])].append(route)

    output = []
    for category, rows in sorted(grouped.items()):
        times = [float(row["total_travel_time_s"]) for row in rows]
        distances = [float(row["total_distance_m"]) for row in rows]
        costs = [float(row["total_generalized_cost"]) for row in rows]
        segments = [float(row["segment_count"]) for row in rows]
        origin_snaps = [float(row["origin_snap_distance_m"]) for row in rows]
        destination_snaps = [float(row["destination_snap_distance_m"]) for row in rows]
        output.append(
            {
                "category": category,
                "route_count": str(len(rows)),
                "legal_count": str(sum(1 for row in rows if row["outcome"] == "legal")),
                "fallback_count": str(sum(1 for row in rows if int(row["fallback_used"]) != 0)),
                "violation_count": str(sum(int(row["violation_count"]) for row in rows)),
                "median_distance_m": format_number(median(distances)),
                "p90_distance_m": format_number(percentile(distances, 0.9)),
                "median_travel_time_s": format_number(median(times)),
                "p90_travel_time_s": format_number(percentile(times, 0.9)),
                "median_generalized_cost": format_number(median(costs)),
                "median_segment_count": format_number(median(segments)),
                "median_origin_snap_distance_m": format_number(median(origin_snaps)),
                "median_destination_snap_distance_m": format_number(median(destination_snaps)),
            }
        )
    return output


def summarize_breakdown(rows: list[dict[str, object]], label_column: str) -> list[dict[str, str]]:
    totals: dict[tuple[str, str], dict[str, float]] = defaultdict(lambda: {"distance_m": 0.0, "time_s": 0.0})
    category_totals: dict[str, dict[str, float]] = defaultdict(lambda: {"distance_m": 0.0, "time_s": 0.0})
    for row in rows:
        category = str(row["category"])
        label = str(row["label"])
        distance = float(row["distance_m"] or 0.0)
        time_s = float(row["time_s"] or 0.0)
        totals[(category, label)]["distance_m"] += distance
        totals[(category, label)]["time_s"] += time_s
        category_totals[category]["distance_m"] += distance
        category_totals[category]["time_s"] += time_s

    output = []
    for (category, label), values in sorted(totals.items()):
        category_distance = category_totals[category]["distance_m"]
        category_time = category_totals[category]["time_s"]
        output.append(
            {
                "category": category,
                label_column: label,
                "distance_m": format_number(values["distance_m"]),
                "distance_share": format_ratio(values["distance_m"], category_distance),
                "time_s": format_number(values["time_s"]),
                "time_share": format_ratio(values["time_s"], category_time),
            }
        )
    return output


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    if not ordered:
        return 0.0
    index = min(len(ordered) - 1, max(0, round((len(ordered) - 1) * fraction)))
    return ordered[index]


def format_number(value: float) -> str:
    if abs(value - round(value)) < 1.0e-9:
        return str(int(round(value)))
    return f"{value:.3f}"


def format_ratio(numerator: float, denominator: float) -> str:
    if denominator == 0:
        return "0.000000"
    return f"{numerator / denominator:.6f}"


def write_rows(path: Path, rows: list[dict[str, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0].keys()))
        writer.writeheader()
        writer.writerows(rows)


if __name__ == "__main__":
    main()
