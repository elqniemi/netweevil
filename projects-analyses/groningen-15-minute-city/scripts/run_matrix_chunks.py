#!/usr/bin/env python3
"""Run netweevil matrix analyses in resumable origin chunks.

The full Groningen baseline has enough origin-destination cells that a single
CLI invocation is hard to monitor. This wrapper splits origins into deterministic
CSV chunks, runs one matrix per chunk, and concatenates completed chunk outputs
into the same CSV shape produced by `netweevil analyze matrix`.
"""

from __future__ import annotations

import argparse
import csv
import json
import subprocess
from pathlib import Path


STUDY_DIR = Path(__file__).resolve().parents[1]
REPO_DIR = STUDY_DIR.parents[1]
ORIGINS = STUDY_DIR / "requests/origins/residential_h3_r9_points.csv"
DESTINATIONS_DIR = STUDY_DIR / "requests/destinations"
RAW_DIR = STUDY_DIR / "outputs/raw"

PROFILES = {
    "cycling": STUDY_DIR / "profiles/cycling_15min_groningen_v1.yml",
    "walking": REPO_DIR / "examples/profiles/pedestrian_research_v1.yml",
    "car": REPO_DIR / "examples/profiles/car_research_v3.yml",
}

BASELINE_CATEGORIES = [
    "food_retail",
    "education",
    "healthcare",
    "public_services",
    "parks_recreation",
    "transit_stops",
]

CYCLING_EXTENDED_CATEGORIES = BASELINE_CATEGORIES + ["social_life", "bicycle_support"]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dataset", default="groningen_2026_05_08")
    parser.add_argument("--origins", type=Path, default=ORIGINS)
    parser.add_argument("--mode", choices=sorted(PROFILES), action="append")
    parser.add_argument("--category", action="append")
    parser.add_argument("--chunk-size", type=int, default=16)
    parser.add_argument("--chunk-dir", type=Path, default=RAW_DIR / "chunks")
    parser.add_argument("--out-dir", type=Path, default=RAW_DIR)
    parser.add_argument("--chunk-format", choices=["json", "csv"], default="json")
    parser.add_argument("--max-chunks", type=int)
    parser.add_argument("--force", action="store_true")
    parser.add_argument("--concatenate-only", action="store_true")
    args = parser.parse_args()

    modes = args.mode or ["cycling", "walking", "car"]
    for mode in modes:
        for category in categories_for_mode(mode, args.category):
            run_category(args, mode, category)


def categories_for_mode(mode: str, requested: list[str] | None) -> list[str]:
    if requested:
        return requested
    if mode == "cycling":
        return CYCLING_EXTENDED_CATEGORIES
    return BASELINE_CATEGORIES


def run_category(args: argparse.Namespace, mode: str, category: str) -> None:
    profile = PROFILES[mode]
    destinations = DESTINATIONS_DIR / f"{category}.csv"
    if not destinations.exists():
        raise SystemExit(f"destination CSV not found: {destinations}")

    rows = read_rows(args.origins)
    chunks = list(chunk_rows(rows, args.chunk_size))
    if args.max_chunks is not None:
        chunks = chunks[: args.max_chunks]

    category_chunk_dir = args.chunk_dir / f"{mode}_{category}"
    origin_chunk_dir = category_chunk_dir / "origins"
    result_chunk_dir = category_chunk_dir / "results"
    origin_chunk_dir.mkdir(parents=True, exist_ok=True)
    result_chunk_dir.mkdir(parents=True, exist_ok=True)

    for index, chunk in enumerate(chunks):
        origin_chunk = origin_chunk_dir / f"chunk_{index:04}.csv"
        result_chunk = result_chunk_dir / f"chunk_{index:04}.{args.chunk_format}"
        write_rows(origin_chunk, chunk)
        if result_chunk.exists() and not args.force:
            print(f"skip existing {result_chunk}")
            continue
        if args.concatenate_only:
            continue
        command = [
            "cargo",
            "run",
            "--release",
            "-p",
            "netweevil-cli",
            "--",
            "analyze",
            "matrix",
            "--dataset",
            args.dataset,
            "--profile",
            str(profile),
            "--origins",
            str(origin_chunk),
            "--destinations",
            str(destinations),
            "--out",
            str(result_chunk),
        ]
        print(f"run {mode}/{category} chunk {index + 1}/{len(chunks)} ({len(chunk)} origins)")
        subprocess.run(command, cwd=REPO_DIR, check=True)

    output = args.out_dir / f"{mode}_{category}_matrix.csv"
    if args.chunk_format == "json":
        concatenate_json_chunks(result_chunk_dir, output, expected_count=len(chunks))
    else:
        concatenate_csv_chunks(result_chunk_dir, output, expected_count=len(chunks))
    print(f"wrote {output}")


def read_rows(path: Path) -> list[dict[str, str]]:
    with path.open(newline="") as handle:
        return list(csv.DictReader(handle))


def chunk_rows(rows: list[dict[str, str]], chunk_size: int):
    if chunk_size <= 0:
        raise SystemExit("--chunk-size must be positive")
    for start in range(0, len(rows), chunk_size):
        yield rows[start : start + chunk_size]


def write_rows(path: Path, rows: list[dict[str, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if not rows:
        path.write_text("id,x,y\n")
        return
    with path.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def concatenate_csv_chunks(result_dir: Path, output: Path, expected_count: int) -> None:
    chunk_paths = [result_dir / f"chunk_{index:04}.csv" for index in range(expected_count)]
    missing = [path for path in chunk_paths if not path.exists()]
    if missing:
        raise SystemExit(f"cannot concatenate; missing {len(missing)} chunk result(s), first: {missing[0]}")

    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("w", newline="") as out_handle:
        writer = None
        for path in chunk_paths:
            with path.open(newline="") as in_handle:
                reader = csv.DictReader(in_handle)
                if writer is None:
                    writer = csv.DictWriter(out_handle, fieldnames=reader.fieldnames or [])
                    writer.writeheader()
                for row in reader:
                    writer.writerow(row)


def concatenate_json_chunks(result_dir: Path, output: Path, expected_count: int) -> None:
    chunk_paths = [result_dir / f"chunk_{index:04}.json" for index in range(expected_count)]
    missing = [path for path in chunk_paths if not path.exists()]
    if missing:
        raise SystemExit(f"cannot concatenate; missing {len(missing)} chunk result(s), first: {missing[0]}")

    fieldnames = [
        "origin_id",
        "destination_id",
        "status",
        "outcome",
        "fallback_used",
        "origin_component_id",
        "destination_component_id",
        "origin_hop_distance_m",
        "destination_hop_distance_m",
        "origin_snap_distance_m",
        "destination_snap_distance_m",
        "total_distance_m",
        "total_travel_time_s",
        "total_generalized_cost",
        "illegal_movement_penalty_s",
        "illegal_movement_penalty_cost",
        "violation_count",
        "error",
    ]
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fieldnames)
        writer.writeheader()
        for path in chunk_paths:
            with path.open() as chunk_handle:
                payload = json.load(chunk_handle)
            for cell in payload.get("cells", []):
                writer.writerow({field: csv_value(cell.get(field)) for field in fieldnames})


def csv_value(value) -> str:
    if value is None:
        return ""
    if isinstance(value, bool):
        return str(value).lower()
    return str(value)


if __name__ == "__main__":
    main()
