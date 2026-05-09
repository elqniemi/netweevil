#!/usr/bin/env python3
"""Summarize sampled transit service-bundle routes into strict/relaxed scores."""

from __future__ import annotations

import argparse
import csv
import json
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path


STUDY_DIR = Path(__file__).resolve().parents[1]
RAW = STUDY_DIR / "outputs/raw/transit_neighbourhood_service_bundle_sample.json"
OUT_CSV = STUDY_DIR / "outputs/processed/transit_service_bundle_scores.csv"
OUT_JSON = STUDY_DIR / "metadata/generated/transit_service_bundle_summary.json"

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
    parser.add_argument("--raw", type=Path, default=RAW)
    parser.add_argument("--out-csv", type=Path, default=OUT_CSV)
    parser.add_argument("--out-json", type=Path, default=OUT_JSON)
    args = parser.parse_args()

    with args.raw.open() as handle:
        batch = json.load(handle)
    rows = build_score_rows(batch)
    write_rows(args.out_csv, rows)
    summary = summarize(rows, batch)
    args.out_json.parent.mkdir(parents=True, exist_ok=True)
    args.out_json.write_text(json.dumps(summary, indent=2) + "\n")


def build_score_rows(batch: dict) -> list[dict[str, str]]:
    by_origin_window: dict[tuple[str, str], dict[str, float | None]] = defaultdict(dict)
    outcomes: dict[tuple[str, str], Counter[str]] = defaultdict(Counter)
    for item in batch.get("items", []):
        route_id = item.get("route_id", "")
        parsed = parse_route_id(route_id)
        if parsed is None:
            continue
        window, origin_id, category = parsed
        result = item.get("result") or {}
        outcome = result.get("outcome") or "failed"
        outcomes[(origin_id, window)][outcome] += 1
        time_s = None
        if outcome == "scheduled":
            time_s = result.get("summary", {}).get("total_travel_time_s")
        by_origin_window[(origin_id, window)][category] = float(time_s) if time_s is not None else None

    rows = []
    for (origin_id, window), category_times in sorted(by_origin_window.items()):
        for threshold_s in [900, 1800, 2700]:
            missing = [
                category
                for category in BASELINE_CATEGORIES
                if category_times.get(category) is None or float(category_times[category]) > threshold_s
            ]
            reached = len(BASELINE_CATEGORIES) - len(missing)
            rows.append(
                {
                    "origin_id": origin_id,
                    "window": window,
                    "threshold_s": str(threshold_s),
                    "reached_categories": str(reached),
                    "required_categories": str(len(BASELINE_CATEGORIES)),
                    "strict_score": f"{reached / len(BASELINE_CATEGORIES):.6f}",
                    "complete": str(not missing).lower(),
                    "relaxed_complete": str(len(missing) <= 1).lower(),
                    "missing_categories": ";".join(missing),
                    "scheduled_route_count": str(outcomes[(origin_id, window)]["scheduled"]),
                    "unreachable_route_count": str(outcomes[(origin_id, window)]["unreachable"]),
                    "failed_route_count": str(outcomes[(origin_id, window)]["failed"]),
                }
            )
    return rows


def parse_route_id(route_id: str) -> tuple[str, str, str] | None:
    parts = route_id.split("__", 3)
    if len(parts) != 4:
        return None
    window, origin_id, category, _destination_id = parts
    return window, origin_id, category


def summarize(rows: list[dict[str, str]], batch: dict) -> dict[str, object]:
    counters: dict[tuple[str, str], Counter[str]] = defaultdict(Counter)
    missing: dict[tuple[str, str], Counter[str]] = defaultdict(Counter)
    for row in rows:
        key = (row["window"], row["threshold_s"])
        counters[key]["origin_window_count"] += 1
        if row["complete"] == "true":
            counters[key]["strict_complete_count"] += 1
        if row["relaxed_complete"] == "true":
            counters[key]["relaxed_complete_count"] += 1
        for category in row["missing_categories"].split(";"):
            if category:
                missing[key][category] += 1
    window_thresholds = []
    for key, counter in sorted(counters.items()):
        window, threshold_s = key
        origin_count = counter["origin_window_count"]
        window_thresholds.append(
            {
                "window": window,
                "threshold_s": float(threshold_s),
                "origin_window_count": origin_count,
                "strict_complete_count": counter["strict_complete_count"],
                "strict_complete_share": ratio(counter["strict_complete_count"], origin_count),
                "relaxed_complete_count": counter["relaxed_complete_count"],
                "relaxed_complete_share": ratio(counter["relaxed_complete_count"], origin_count),
                "top_missing_category": missing[key].most_common(1)[0][0] if missing[key] else "",
            }
        )
    return {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "source": str(RAW),
        "route_count": batch.get("route_count", 0),
        "scheduled_count": batch.get("scheduled_count", 0),
        "unreachable_count": batch.get("unreachable_count", 0),
        "failed_count": batch.get("failed_count", 0),
        "window_thresholds": window_thresholds,
    }


def ratio(numerator: int, denominator: int) -> float:
    return round(numerator / denominator, 6) if denominator else 0.0


def write_rows(path: Path, rows: list[dict[str, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0].keys()) if rows else [])
        if rows:
            writer.writeheader()
            writer.writerows(rows)


if __name__ == "__main__":
    main()
