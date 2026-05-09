#!/usr/bin/env python3
"""Summarize netweevil matrix CSVs into 15-minute city accessibility tables."""

from __future__ import annotations

import argparse
import csv
import glob
import json
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path
from statistics import median


STUDY_DIR = Path(__file__).resolve().parents[1]
RAW_DIR = STUDY_DIR / "outputs/raw"
PROCESSED_DIR = STUDY_DIR / "outputs/processed"
META_DIR = STUDY_DIR / "metadata/generated"
ORIGINS_CSV = STUDY_DIR / "requests/origins/residential_h3_r9_points.csv"

MODES = ["walking", "cycling", "car", "transit"]
BASELINE_CATEGORIES = [
    "food_retail",
    "education",
    "healthcare",
    "public_services",
    "parks_recreation",
    "transit_stops",
]
EXTENDED_CATEGORIES = ["social_life", "bicycle_support"]
REACHABILITY_THRESHOLDS = [300, 600, 900, 1200]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--matrix-glob", default=str(RAW_DIR / "*_matrix.csv"))
    parser.add_argument("--accessibility-glob", default=str(RAW_DIR / "*_accessibility_reduced.csv"))
    parser.add_argument("--origins", type=Path, default=ORIGINS_CSV)
    parser.add_argument("--out-dir", type=Path, default=PROCESSED_DIR)
    parser.add_argument("--summary", type=Path)
    parser.add_argument("--primary-threshold-s", type=float, default=900.0)
    args = parser.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)
    META_DIR.mkdir(parents=True, exist_ok=True)
    summary_path = args.summary or default_summary_path(args.out_dir)

    origin_ids = load_origin_ids(args.origins)
    nearest_rows, category_stats = load_nearest_rows(args.matrix_glob, origin_ids)
    reduced_rows, reduced_stats = load_accessibility_rows(args.accessibility_glob, origin_ids)
    nearest_rows.extend(reduced_rows)
    category_stats.extend(reduced_stats)
    write_rows(args.out_dir / "accessibility_nearest_services.csv", nearest_rows)
    if not nearest_rows:
        write_rows(args.out_dir / "accessibility_scores.csv", [])
        write_rows(args.out_dir / "category_bottlenecks.csv", [])
        summary = build_summary([], [], category_stats, args.primary_threshold_s)
        with summary_path.open("w") as handle:
            json.dump(summary, handle, indent=2)
            handle.write("\n")
        return

    score_rows, bottlenecks = build_scores(nearest_rows, origin_ids, args.primary_threshold_s)
    write_rows(args.out_dir / "accessibility_scores.csv", score_rows)
    write_rows(args.out_dir / "category_bottlenecks.csv", bottlenecks)

    summary = build_summary(score_rows, nearest_rows, category_stats, args.primary_threshold_s)
    with summary_path.open("w") as handle:
        json.dump(summary, handle, indent=2)
        handle.write("\n")


def default_summary_path(out_dir: Path) -> Path:
    if out_dir.resolve() == PROCESSED_DIR.resolve():
        return META_DIR / "accessibility_summary.json"
    return out_dir / "accessibility_summary.json"


def load_origin_ids(path: Path) -> list[str]:
    if not path.exists():
        return []
    with path.open(newline="") as handle:
        return [row["id"] for row in csv.DictReader(handle) if row.get("id")]


def load_nearest_rows(matrix_glob: str, origin_ids: list[str]) -> tuple[list[dict[str, str]], list[dict[str, str]]]:
    rows: list[dict[str, str]] = []
    stats: list[dict[str, str]] = []
    for path_text in sorted(glob.glob(matrix_glob)):
        path = Path(path_text)
        mode, category = infer_mode_category(path)
        if mode is None or category is None:
            continue
        by_origin: dict[str, list[dict[str, str]]] = defaultdict(list)
        with path.open(newline="") as handle:
            for row in csv.DictReader(handle):
                origin_id = row.get("origin_id", "")
                if origin_id:
                    by_origin[origin_id].append(row)
        origin_universe = origin_ids or sorted(by_origin)
        for origin_id in origin_universe:
            nearest = nearest_legal_cell(by_origin.get(origin_id, []))
            rows.append(nearest_service_row(mode, category, origin_id, nearest))
        stats.append(category_stat(mode, category, path, by_origin))

    return rows, stats


def load_accessibility_rows(
    accessibility_glob: str, origin_ids: list[str]
) -> tuple[list[dict[str, str]], list[dict[str, str]]]:
    rows: list[dict[str, str]] = []
    stats: list[dict[str, str]] = []
    for path_text in sorted(glob.glob(accessibility_glob)):
        path = Path(path_text)
        mode, _category = infer_mode_category(path)
        if mode is None:
            continue
        by_origin_category: dict[tuple[str, str], dict[str, str]] = {}
        categories: set[str] = set()
        with path.open(newline="") as handle:
            for row in csv.DictReader(handle):
                origin_id = row.get("origin_id", "")
                category = row.get("category_id", "")
                if not origin_id or not category:
                    continue
                categories.add(category)
                by_origin_category[(origin_id, category)] = row
        origin_universe = origin_ids or sorted({origin_id for origin_id, _ in by_origin_category})
        for category in sorted(categories):
            category_rows = []
            for origin_id in origin_universe:
                source = by_origin_category.get((origin_id, category))
                row = reduced_nearest_service_row(mode, category, origin_id, source)
                rows.append(row)
                category_rows.append(row)
            stats.append(reduced_category_stat(mode, category, path, category_rows))
    return rows, stats


def nearest_legal_cell(cells: list[dict[str, str]]) -> dict[str, str] | None:
    eligible = []
    for cell in cells:
        time_s = parse_float(cell.get("total_travel_time_s", ""))
        if time_s is None:
            continue
        if cell.get("status") != "succeeded":
            continue
        if cell.get("outcome") not in {"legal", ""}:
            continue
        if cell.get("fallback_used", "false").lower() == "true":
            continue
        eligible.append((time_s, cell))
    if not eligible:
        return None
    return min(eligible, key=lambda item: item[0])[1]


def nearest_service_row(mode: str, category: str, origin_id: str, cell: dict[str, str] | None) -> dict[str, str]:
    time_s = parse_float(cell.get("total_travel_time_s", "")) if cell else None
    distance_m = parse_float(cell.get("total_distance_m", "")) if cell else None
    row = {
        "origin_id": origin_id,
        "mode": mode,
        "category": category,
        "nearest_destination_id": cell.get("destination_id", "") if cell else "",
        "nearest_travel_time_s": format_number(time_s),
        "nearest_distance_m": format_number(distance_m),
    }
    for threshold in REACHABILITY_THRESHOLDS:
        row[f"reachable_within_{threshold}s"] = "true" if time_s is not None and time_s <= threshold else "false"
    return row


def reduced_nearest_service_row(
    mode: str, category: str, origin_id: str, source: dict[str, str] | None
) -> dict[str, str]:
    time_s = parse_float(source.get("nearest_travel_time_s", "")) if source else None
    row = {
        "origin_id": origin_id,
        "mode": mode,
        "category": category,
        "nearest_destination_id": source.get("nearest_destination_id", "") if source else "",
        "nearest_travel_time_s": format_number(time_s),
        "nearest_distance_m": "",
    }
    for threshold in REACHABILITY_THRESHOLDS:
        count = parse_float(source.get(f"count_within_{threshold}s", "")) if source else None
        reachable = (count is not None and count > 0) or (time_s is not None and time_s <= threshold)
        row[f"reachable_within_{threshold}s"] = str(reachable).lower()
        row[f"destination_count_within_{threshold}s"] = (
            format_number(count) if count is not None else "0"
        )
    return row


def build_scores(
    nearest_rows: list[dict[str, str]], origin_ids: list[str], primary_threshold_s: float
) -> tuple[list[dict[str, str]], list[dict[str, str]]]:
    by_origin_mode_category = {
        (row["origin_id"], row["mode"], row["category"]): parse_float(row["nearest_travel_time_s"])
        for row in nearest_rows
    }
    modes = sorted({row["mode"] for row in nearest_rows} or MODES)
    origins = origin_ids or sorted({row["origin_id"] for row in nearest_rows})
    score_rows = []
    bottleneck_counter: Counter[tuple[str, str]] = Counter()
    for origin_id in origins:
        for mode in modes:
            reached = []
            missing = []
            for category in BASELINE_CATEGORIES:
                time_s = by_origin_mode_category.get((origin_id, mode, category))
                if time_s is not None and time_s <= primary_threshold_s:
                    reached.append(category)
                else:
                    missing.append(category)
                    bottleneck_counter[(mode, category)] += 1
            extended_reached = []
            extended_missing = []
            for category in EXTENDED_CATEGORIES:
                time_s = by_origin_mode_category.get((origin_id, mode, category))
                if time_s is not None and time_s <= primary_threshold_s:
                    extended_reached.append(category)
                else:
                    extended_missing.append(category)
            score_rows.append(
                {
                    "origin_id": origin_id,
                    "mode": mode,
                    "baseline_reached_categories": str(len(reached)),
                    "baseline_required_categories": str(len(BASELINE_CATEGORIES)),
                    "strict_score": f"{len(reached) / len(BASELINE_CATEGORIES):.6f}",
                    "complete_15m": str(len(missing) == 0).lower(),
                    "relaxed_complete_15m": str(len(missing) <= 1).lower(),
                    "missing_baseline_categories": ";".join(missing),
                    "extended_reached_categories": str(len(extended_reached)),
                    "extended_required_categories": str(len(EXTENDED_CATEGORIES)),
                    "missing_extended_categories": ";".join(extended_missing),
                }
            )
    bottlenecks = [
        {"mode": mode, "category": category, "missing_origin_count": str(count)}
        for (mode, category), count in sorted(bottleneck_counter.items())
    ]
    return score_rows, bottlenecks


def category_stat(mode: str, category: str, path: Path, by_origin: dict[str, list[dict[str, str]]]) -> dict[str, str]:
    nearest_times = []
    failed = 0
    ignored = 0
    for cells in by_origin.values():
        nearest = nearest_legal_cell(cells)
        if nearest:
            value = parse_float(nearest.get("total_travel_time_s", ""))
            if value is not None:
                nearest_times.append(value)
        failed += sum(1 for cell in cells if cell.get("status") == "failed")
        ignored += sum(1 for cell in cells if cell.get("status") == "ignored")
    return {
        "mode": mode,
        "category": category,
        "matrix_path": str(path),
        "origin_count": str(len(by_origin)),
        "median_nearest_travel_time_s": format_number(median(nearest_times) if nearest_times else None),
        "failed_cell_count": str(failed),
        "ignored_cell_count": str(ignored),
    }


def reduced_category_stat(
    mode: str, category: str, path: Path, rows: list[dict[str, str]]
) -> dict[str, str]:
    nearest_times = [
        value
        for row in rows
        if (value := parse_float(row.get("nearest_travel_time_s", ""))) is not None
    ]
    failed = sum(1 for row in rows if parse_float(row.get("nearest_travel_time_s", "")) is None)
    return {
        "mode": mode,
        "category": category,
        "matrix_path": str(path),
        "origin_count": str(len(rows)),
        "median_nearest_travel_time_s": format_number(median(nearest_times) if nearest_times else None),
        "failed_cell_count": str(failed),
        "ignored_cell_count": "0",
    }


def build_summary(
    score_rows: list[dict[str, str]],
    nearest_rows: list[dict[str, str]],
    category_stats: list[dict[str, str]],
    primary_threshold_s: float,
) -> dict[str, object]:
    mode_counts: dict[str, Counter[str]] = defaultdict(Counter)
    for row in score_rows:
        mode_counts[row["mode"]]["origin_count"] += 1
        if row["complete_15m"] == "true":
            mode_counts[row["mode"]]["strict_complete_count"] += 1
        if row["relaxed_complete_15m"] == "true":
            mode_counts[row["mode"]]["relaxed_complete_count"] += 1
    modes = {}
    for mode, counts in mode_counts.items():
        origin_count = counts["origin_count"]
        modes[mode] = {
            "origin_count": origin_count,
            "strict_complete_count": counts["strict_complete_count"],
            "strict_complete_share": safe_ratio(counts["strict_complete_count"], origin_count),
            "relaxed_complete_count": counts["relaxed_complete_count"],
            "relaxed_complete_share": safe_ratio(counts["relaxed_complete_count"], origin_count),
        }
    return {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "primary_threshold_s": primary_threshold_s,
        "nearest_row_count": len(nearest_rows),
        "score_row_count": len(score_rows),
        "modes": modes,
        "category_stats": category_stats,
    }


def infer_mode_category(path: Path) -> tuple[str | None, str | None]:
    stem = path.stem
    if stem.endswith("_matrix"):
        stem = stem[: -len("_matrix")]
    if stem.endswith("_accessibility_reduced"):
        stem = stem[: -len("_accessibility_reduced")]
        return (stem, None) if stem in MODES else (None, None)
    for mode in MODES:
        prefix = f"{mode}_"
        if stem.startswith(prefix):
            return mode, stem[len(prefix) :]
    parts = stem.split("_", 1)
    if len(parts) == 2 and parts[0] in MODES:
        return parts[0], parts[1]
    return None, None


def parse_float(value: str | None) -> float | None:
    if value is None or value == "":
        return None
    try:
        return float(value)
    except ValueError:
        return None


def format_number(value: float | None) -> str:
    if value is None:
        return ""
    if abs(value - round(value)) < 1.0e-9:
        return str(int(round(value)))
    return f"{value:.3f}"


def safe_ratio(numerator: int, denominator: int) -> float:
    if denominator == 0:
        return 0.0
    return round(numerator / denominator, 6)


def write_rows(path: Path, rows: list[dict[str, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if not rows:
        path.write_text("")
        return
    with path.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0].keys()))
        writer.writeheader()
        writer.writerows(rows)


if __name__ == "__main__":
    main()
