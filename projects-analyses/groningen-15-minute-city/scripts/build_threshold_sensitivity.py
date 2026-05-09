#!/usr/bin/env python3
"""Build threshold sensitivity tables from reduced nearest-service outputs."""

from __future__ import annotations

import argparse
import csv
import json
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path


STUDY_DIR = Path(__file__).resolve().parents[1]
NEAREST = STUDY_DIR / "outputs/processed/accessibility_nearest_services.csv"
OUT_CSV = STUDY_DIR / "outputs/processed/sensitivity_threshold_scores.csv"
OUT_JSON = STUDY_DIR / "metadata/generated/sensitivity_threshold_summary.json"
BASELINE_CATEGORIES = [
    "food_retail",
    "education",
    "healthcare",
    "public_services",
    "parks_recreation",
    "transit_stops",
]
EXTENDED_CATEGORIES = ["social_life", "bicycle_support"]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--nearest", type=Path, default=NEAREST)
    parser.add_argument("--thresholds-s", default="300,600,900,1200")
    parser.add_argument("--out-csv", type=Path, default=OUT_CSV)
    parser.add_argument("--out-json", type=Path, default=OUT_JSON)
    args = parser.parse_args()

    thresholds = [float(value) for value in args.thresholds_s.split(",") if value]
    rows = read_rows(args.nearest)
    scores = build_scores(rows, thresholds)
    write_rows(args.out_csv, scores)
    summary = summarize(scores)
    args.out_json.parent.mkdir(parents=True, exist_ok=True)
    args.out_json.write_text(json.dumps(summary, indent=2) + "\n")


def read_rows(path: Path) -> list[dict[str, str]]:
    with path.open(newline="") as handle:
        return list(csv.DictReader(handle))


def build_scores(rows: list[dict[str, str]], thresholds: list[float]) -> list[dict[str, str]]:
    times = {
        (row["origin_id"], row["mode"], row["category"]): parse_float(row.get("nearest_travel_time_s"))
        for row in rows
    }
    origins = sorted({row["origin_id"] for row in rows})
    modes = sorted({row["mode"] for row in rows})
    output = []
    for threshold in thresholds:
        for mode in modes:
            for origin_id in origins:
                missing = [
                    category
                    for category in BASELINE_CATEGORIES
                    if (times.get((origin_id, mode, category)) is None)
                    or (times[(origin_id, mode, category)] > threshold)
                ]
                extended_missing = [
                    category
                    for category in EXTENDED_CATEGORIES
                    if (times.get((origin_id, mode, category)) is None)
                    or (times[(origin_id, mode, category)] > threshold)
                ]
                reached = len(BASELINE_CATEGORIES) - len(missing)
                output.append(
                    {
                        "threshold_s": format_number(threshold),
                        "origin_id": origin_id,
                        "mode": mode,
                        "baseline_reached_categories": str(reached),
                        "baseline_required_categories": str(len(BASELINE_CATEGORIES)),
                        "strict_score": f"{reached / len(BASELINE_CATEGORIES):.6f}",
                        "complete": str(not missing).lower(),
                        "relaxed_complete": str(len(missing) <= 1).lower(),
                        "missing_baseline_categories": ";".join(missing),
                        "extended_reached_categories": str(len(EXTENDED_CATEGORIES) - len(extended_missing)),
                        "extended_required_categories": str(len(EXTENDED_CATEGORIES)),
                        "missing_extended_categories": ";".join(extended_missing),
                    }
                )
    return output


def summarize(rows: list[dict[str, str]]) -> dict[str, object]:
    counts: dict[tuple[str, str], Counter[str]] = defaultdict(Counter)
    for row in rows:
        key = (row["threshold_s"], row["mode"])
        counts[key]["origin_count"] += 1
        if row["complete"] == "true":
            counts[key]["strict_complete_count"] += 1
        if row["relaxed_complete"] == "true":
            counts[key]["relaxed_complete_count"] += 1
    threshold_modes = []
    for (threshold_s, mode), counter in sorted(counts.items(), key=lambda item: (float(item[0][0]), item[0][1])):
        origin_count = counter["origin_count"]
        threshold_modes.append(
            {
                "threshold_s": float(threshold_s),
                "mode": mode,
                "origin_count": origin_count,
                "strict_complete_count": counter["strict_complete_count"],
                "strict_complete_share": ratio(counter["strict_complete_count"], origin_count),
                "relaxed_complete_count": counter["relaxed_complete_count"],
                "relaxed_complete_share": ratio(counter["relaxed_complete_count"], origin_count),
            }
        )
    return {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "threshold_modes": threshold_modes,
    }


def parse_float(value: str | None) -> float | None:
    if value in {None, ""}:
        return None
    try:
        return float(value)
    except ValueError:
        return None


def format_number(value: float) -> str:
    return str(int(value)) if abs(value - round(value)) < 1.0e-9 else f"{value:.3f}"


def ratio(numerator: int, denominator: int) -> float:
    return round(numerator / denominator, 6) if denominator else 0.0


def write_rows(path: Path, rows: list[dict[str, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0].keys()))
        writer.writeheader()
        writer.writerows(rows)


if __name__ == "__main__":
    main()
