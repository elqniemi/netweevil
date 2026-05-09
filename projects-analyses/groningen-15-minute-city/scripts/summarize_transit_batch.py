#!/usr/bin/env python3
"""Summarize neighbourhood transit time-window batch results."""

from __future__ import annotations

import argparse
import csv
import json
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path
from statistics import median


STUDY_DIR = Path(__file__).resolve().parents[1]
RAW = STUDY_DIR / "outputs/raw/transit_neighbourhood_centres_to_zernike_batch.json"
OUT = STUDY_DIR / "outputs/processed/transit_neighbourhood_time_windows.csv"
SUMMARY = STUDY_DIR / "metadata/generated/transit_neighbourhood_time_windows_summary.json"
WINDOWS = ["weekday_am_peak", "weekday_midday", "weekday_evening", "weekend_midday"]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--raw", type=Path, default=RAW)
    parser.add_argument("--out", type=Path, default=OUT)
    parser.add_argument("--summary", type=Path, default=SUMMARY)
    args = parser.parse_args()

    with args.raw.open() as handle:
        batch = json.load(handle)

    rows = []
    for item in batch.get("items", []):
        route_id = item["route_id"]
        window, neighbourhood_id = parse_route_id(route_id)
        result = item["result"]
        summary = result.get("summary", {})
        rows.append(
            {
                "window": window,
                "neighbourhood_id": neighbourhood_id,
                "route_id": route_id,
                "outcome": result.get("outcome", ""),
                "total_travel_time_s": optional(summary.get("total_travel_time_s")),
                "transit_time_s": optional(summary.get("transit_time_s")),
                "access_egress_time_s": optional(summary.get("access_egress_time_s")),
                "transfer_time_s": optional(summary.get("transfer_time_s")),
                "wait_time_s": optional(summary.get("wait_time_s")),
                "boarding_count": optional(summary.get("boarding_count")),
                "reachable_15m": str(is_reachable(summary, 900)).lower(),
                "reachable_30m": str(is_reachable(summary, 1800)).lower(),
                "reachable_45m": str(is_reachable(summary, 2700)).lower(),
            }
        )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    write_rows(args.out, rows)
    payload = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "source": str(args.raw),
        "route_count": batch.get("route_count", len(rows)),
        "scheduled_count": batch.get("scheduled_count", 0),
        "unreachable_count": batch.get("unreachable_count", 0),
        "failed_count": batch.get("failed_count", 0),
        "window_summary": summarize_windows(rows),
    }
    args.summary.parent.mkdir(parents=True, exist_ok=True)
    args.summary.write_text(json.dumps(payload, indent=2) + "\n")


def parse_route_id(route_id: str) -> tuple[str, str]:
    for window in WINDOWS:
        prefix = f"{window}_"
        if route_id.startswith(prefix) and route_id.endswith("_to_zernike"):
            return window, route_id[len(prefix) : -len("_to_zernike")]
    return "unknown", route_id


def is_reachable(summary: dict, threshold_s: int) -> bool:
    value = summary.get("total_travel_time_s")
    return value is not None and int(value) <= threshold_s


def summarize_windows(rows: list[dict[str, str]]) -> list[dict[str, object]]:
    grouped: dict[str, list[dict[str, str]]] = defaultdict(list)
    for row in rows:
        grouped[row["window"]].append(row)
    output = []
    for window, window_rows in sorted(grouped.items()):
        times = [float(row["total_travel_time_s"]) for row in window_rows if row["total_travel_time_s"]]
        outcomes = Counter(row["outcome"] for row in window_rows)
        output.append(
            {
                "window": window,
                "route_count": len(window_rows),
                "scheduled_count": outcomes["scheduled"],
                "unreachable_count": outcomes["unreachable"],
                "median_total_travel_time_s": number(median(times) if times else None),
                "p90_total_travel_time_s": number(percentile(times, 0.9) if times else None),
                "reachable_15m_count": sum(row["reachable_15m"] == "true" for row in window_rows),
                "reachable_30m_count": sum(row["reachable_30m"] == "true" for row in window_rows),
                "reachable_45m_count": sum(row["reachable_45m"] == "true" for row in window_rows),
            }
        )
    return output


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    if not ordered:
        return 0.0
    index = min(len(ordered) - 1, max(0, round((len(ordered) - 1) * fraction)))
    return ordered[index]


def optional(value: object) -> str:
    return "" if value is None else str(value)


def number(value: float | None) -> float | None:
    return None if value is None else round(value, 3)


def write_rows(path: Path, rows: list[dict[str, str]]) -> None:
    with path.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0].keys()))
        writer.writeheader()
        writer.writerows(rows)


if __name__ == "__main__":
    main()
