#!/usr/bin/env python3
"""Build multi-scenario sensitivity summaries from reduced accessibility outputs."""

from __future__ import annotations

import argparse
import csv
import glob
import json
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path


STUDY_DIR = Path(__file__).resolve().parents[1]
RAW_GLOB = STUDY_DIR / "outputs/raw/*_accessibility_reduced.csv"
ORIGINS = STUDY_DIR / "requests/origins/residential_h3_r9_points.csv"
OUT_CSV = STUDY_DIR / "outputs/processed/sensitivity_scenario_scores.csv"
OUT_JSON = STUDY_DIR / "metadata/generated/sensitivity_scenario_summary.json"

BASELINE_CATEGORIES = [
    "food_retail",
    "education",
    "healthcare",
    "public_services",
    "parks_recreation",
    "transit_stops",
]
EXTENDED_CATEGORIES = BASELINE_CATEGORIES + ["social_life", "bicycle_support"]
BUNDLES = {
    "essential_services": BASELINE_CATEGORIES,
    "extended_daily_life": EXTENDED_CATEGORIES,
}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--raw-glob", default=str(RAW_GLOB))
    parser.add_argument("--origins", type=Path, default=ORIGINS)
    parser.add_argument("--thresholds-s", default="600,720,900,1200")
    parser.add_argument("--snap-distances-m", default="100,250,500")
    parser.add_argument("--out-csv", type=Path, default=OUT_CSV)
    parser.add_argument("--out-json", type=Path, default=OUT_JSON)
    args = parser.parse_args()

    thresholds = parse_float_list(args.thresholds_s)
    snap_distances = parse_float_list(args.snap_distances_m)
    origin_weights = load_origin_weights(args.origins)
    raw_rows = load_raw_rows(args.raw_glob)
    scenario_rows = build_scenarios(raw_rows, origin_weights, thresholds, snap_distances)
    write_rows(args.out_csv, scenario_rows)
    summary = summarize_scenarios(scenario_rows)
    args.out_json.parent.mkdir(parents=True, exist_ok=True)
    args.out_json.write_text(json.dumps(summary, indent=2) + "\n")


def parse_float_list(text: str) -> list[float]:
    return [float(value.strip()) for value in text.split(",") if value.strip()]


def load_origin_weights(path: Path) -> dict[str, float]:
    weights = {}
    with path.open(newline="") as handle:
        for row in csv.DictReader(handle):
            origin_id = row.get("id", "")
            if origin_id:
                weights[origin_id] = parse_float(row.get("weight")) or 1.0
    return weights


def load_raw_rows(pattern: str) -> dict[tuple[str, str, str], dict[str, str]]:
    rows = {}
    for path_text in sorted(glob.glob(pattern)):
        path = Path(path_text)
        mode = infer_mode(path)
        if mode is None:
            continue
        with path.open(newline="") as handle:
            for row in csv.DictReader(handle):
                origin_id = row.get("origin_id", "")
                category = row.get("category_id", "")
                if origin_id and category:
                    rows[(origin_id, mode, category)] = row
    return rows


def infer_mode(path: Path) -> str | None:
    name = path.name
    if not name.endswith("_accessibility_reduced.csv"):
        return None
    return name.removesuffix("_accessibility_reduced.csv")


def build_scenarios(
    raw_rows: dict[tuple[str, str, str], dict[str, str]],
    origin_weights: dict[str, float],
    thresholds: list[float],
    snap_distances: list[float],
) -> list[dict[str, str]]:
    origins = sorted(origin_weights)
    modes = sorted({mode for _origin_id, mode, _category in raw_rows})
    scenario_rows: list[dict[str, str]] = []

    scenario_specs = []
    for threshold in thresholds:
        scenario_specs.append(
            {
                "family": "threshold",
                "id": f"{int(threshold / 60)}min",
                "threshold_s": threshold,
                "time_factor": 1.0,
                "mode_filter": None,
                "snap_distance_m": None,
                "notes": "Direct threshold test from reduced nearest-service times.",
            }
        )
    for scenario_id, factor in [
        ("conservative_speed", 1.15),
        ("baseline_speed", 1.0),
        ("optimistic_speed", 0.90),
    ]:
        scenario_specs.append(
            {
                "family": "cycling_speed",
                "id": scenario_id,
                "threshold_s": 900.0,
                "time_factor": factor,
                "mode_filter": "cycling",
                "snap_distance_m": None,
                "notes": "Analytic cycling travel-time scaling; route legality and geometry are unchanged.",
            }
        )
    for snap_distance in snap_distances:
        scenario_specs.append(
            {
                "family": "snap_acceptance",
                "id": f"{int(snap_distance)}m",
                "threshold_s": 900.0,
                "time_factor": 1.0,
                "mode_filter": None,
                "snap_distance_m": snap_distance,
                "notes": "Conservative post-processing test requiring the observed origin and destination snaps to be within the distance limit.",
            }
        )

    for spec in scenario_specs:
        selected_modes = [spec["mode_filter"]] if spec["mode_filter"] else modes
        for mode in selected_modes:
            for bundle_id, categories in BUNDLES.items():
                origin_scores = [
                    score_origin(raw_rows, origin_id, mode, categories, spec)
                    for origin_id in origins
                ]
                for weighting in ["unweighted", "residential_proxy_weighted"]:
                    scenario_rows.append(
                        summarize_origin_scores(
                            spec,
                            mode,
                            bundle_id,
                            categories,
                            origin_scores,
                            origin_weights,
                            weighting,
                        )
                    )
    return scenario_rows


def score_origin(
    raw_rows: dict[tuple[str, str, str], dict[str, str]],
    origin_id: str,
    mode: str,
    categories: list[str],
    spec: dict[str, object],
) -> dict[str, object]:
    missing = []
    reached = 0
    threshold = float(spec["threshold_s"])
    time_factor = float(spec["time_factor"])
    snap_distance = spec["snap_distance_m"]
    for category in categories:
        row = raw_rows.get((origin_id, mode, category))
        if row is None or not row_reaches(row, threshold, time_factor, snap_distance):
            missing.append(category)
        else:
            reached += 1
    return {
        "origin_id": origin_id,
        "reached": reached,
        "required": len(categories),
        "score": reached / len(categories),
        "strict_complete": reached == len(categories),
        "relaxed_complete": len(missing) <= 1,
        "missing": missing,
    }


def row_reaches(row: dict[str, str], threshold: float, time_factor: float, snap_distance: object) -> bool:
    if row.get("status") != "succeeded":
        return False
    if row.get("outcome") not in {"legal", ""}:
        return False
    if row.get("fallback_used", "false").lower() == "true":
        return False
    if snap_distance is not None:
        limit = float(snap_distance)
        origin_snap = parse_float(row.get("origin_snap_distance_m"))
        destination_snap = parse_float(row.get("nearest_destination_snap_distance_m"))
        if origin_snap is None or destination_snap is None or origin_snap > limit or destination_snap > limit:
            return False
    time_s = parse_float(row.get("nearest_travel_time_s"))
    return time_s is not None and time_s * time_factor <= threshold


def summarize_origin_scores(
    spec: dict[str, object],
    mode: str,
    bundle_id: str,
    categories: list[str],
    origin_scores: list[dict[str, object]],
    origin_weights: dict[str, float],
    weighting: str,
) -> dict[str, str]:
    denominator = 0.0
    strict = 0.0
    relaxed = 0.0
    score_total = 0.0
    missing_counts: dict[str, float] = defaultdict(float)
    for score in origin_scores:
        origin_id = str(score["origin_id"])
        weight = origin_weights.get(origin_id, 1.0) if weighting == "residential_proxy_weighted" else 1.0
        denominator += weight
        if score["strict_complete"]:
            strict += weight
        if score["relaxed_complete"]:
            relaxed += weight
        score_total += float(score["score"]) * weight
        for category in score["missing"]:
            missing_counts[str(category)] += weight
    bottleneck = ""
    if missing_counts:
        bottleneck = max(sorted(missing_counts), key=lambda category: missing_counts[category])
    return {
        "scenario_family": str(spec["family"]),
        "scenario_id": str(spec["id"]),
        "mode": mode,
        "bundle": bundle_id,
        "weighting": weighting,
        "threshold_s": format_number(float(spec["threshold_s"])),
        "time_factor": format_number(float(spec["time_factor"])),
        "snap_distance_m": "" if spec["snap_distance_m"] is None else format_number(float(spec["snap_distance_m"])),
        "origin_count": str(len(origin_scores)),
        "denominator_weight": f"{denominator:.6f}",
        "required_categories": str(len(categories)),
        "strict_complete_share": f"{ratio(strict, denominator):.6f}",
        "relaxed_complete_share": f"{ratio(relaxed, denominator):.6f}",
        "mean_score": f"{ratio(score_total, denominator):.6f}",
        "top_missing_category": bottleneck,
        "notes": str(spec["notes"]),
    }


def summarize_scenarios(rows: list[dict[str, str]]) -> dict[str, object]:
    bands: dict[tuple[str, str, str], dict[str, object]] = {}
    for row in rows:
        if row["weighting"] != "residential_proxy_weighted":
            continue
        key = (row["scenario_family"], row["mode"], row["bundle"])
        current = bands.setdefault(
            key,
            {
                "scenario_family": row["scenario_family"],
                "mode": row["mode"],
                "bundle": row["bundle"],
                "min_strict_complete_share": 1.0,
                "max_strict_complete_share": 0.0,
                "min_relaxed_complete_share": 1.0,
                "max_relaxed_complete_share": 0.0,
                "scenario_count": 0,
            },
        )
        strict = float(row["strict_complete_share"])
        relaxed = float(row["relaxed_complete_share"])
        current["min_strict_complete_share"] = min(float(current["min_strict_complete_share"]), strict)
        current["max_strict_complete_share"] = max(float(current["max_strict_complete_share"]), strict)
        current["min_relaxed_complete_share"] = min(float(current["min_relaxed_complete_share"]), relaxed)
        current["max_relaxed_complete_share"] = max(float(current["max_relaxed_complete_share"]), relaxed)
        current["scenario_count"] = int(current["scenario_count"]) + 1
    return {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "scenario_row_count": len(rows),
        "scenario_families": sorted({row["scenario_family"] for row in rows}),
        "bands": sorted(bands.values(), key=lambda row: (row["scenario_family"], row["mode"], row["bundle"])),
        "notes": [
            "Weighted rows use the residential proxy weight in requests/origins/residential_h3_r9_points.csv.",
            "Cycling-speed sensitivity is analytic scaling of baseline reduced travel times, not a rerouted profile compile.",
            "Snap-acceptance sensitivity is conservative because it rejects observed snaps beyond the limit without searching for alternate snaps.",
        ],
    }


def parse_float(value: str | None) -> float | None:
    if value in {None, ""}:
        return None
    try:
        return float(value)
    except ValueError:
        return None


def ratio(numerator: float, denominator: float) -> float:
    return numerator / denominator if denominator else 0.0


def format_number(value: float) -> str:
    return str(int(value)) if abs(value - round(value)) < 1.0e-9 else f"{value:.6f}"


def write_rows(path: Path, rows: list[dict[str, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0].keys()) if rows else [])
        if rows:
            writer.writeheader()
            writer.writerows(rows)


if __name__ == "__main__":
    main()
