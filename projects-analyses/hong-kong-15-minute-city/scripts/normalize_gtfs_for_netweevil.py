#!/usr/bin/env python3
"""Create a Netweevil-compatible copy of the Hong Kong GTFS feed.

The official feed leaves many non-timepoint intermediate stop_times rows blank.
That is valid GTFS, but Netweevil's current importer expects populated arrival
and departure times for every stop_time. This script fills blank intermediate
rows by linear interpolation between published timepoints.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import zipfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


REPO_DIR = Path(__file__).resolve().parents[3]
DEFAULT_SOURCE = REPO_DIR / "datasets/gtfs-hong-kong-en-260430.zip"
DEFAULT_OUTPUT = REPO_DIR / "datasets/gtfs-hong-kong-en-260430-netweevil.zip"
DEFAULT_METADATA = (
    REPO_DIR
    / "projects-analyses/hong-kong-15-minute-city/metadata/generated/gtfs_hong_kong_en_2026_04_30_netweevil.json"
)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, default=DEFAULT_SOURCE)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--metadata", type=Path, default=DEFAULT_METADATA)
    args = parser.parse_args()

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.metadata.parent.mkdir(parents=True, exist_ok=True)

    with zipfile.ZipFile(args.source, "r") as source_zip, zipfile.ZipFile(
        args.out, "w", compression=zipfile.ZIP_DEFLATED
    ) as output_zip:
        summary: dict[str, Any] = {
            "source_path": str(args.source),
            "output_path": str(args.out),
            "created_at": datetime.now(timezone.utc).isoformat(),
            "normalization": "linear interpolation of blank stop_times arrival_time/departure_time fields",
            "files": {},
        }
        for member in source_zip.infolist():
            raw = source_zip.read(member.filename)
            if member.filename == "stop_times.txt":
                normalized, file_summary = normalize_stop_times(raw.decode("utf-8-sig"))
                output_zip.writestr(member.filename, normalized)
                summary["files"][member.filename] = file_summary
            else:
                output_zip.writestr(member, raw)

    summary["source_sha256"] = sha256_file(args.source)
    summary["output_sha256"] = sha256_file(args.out)
    args.metadata.write_text(json.dumps(summary, indent=2) + "\n")
    print(f"wrote {args.out}")
    print(f"wrote {args.metadata}")


def normalize_stop_times(text: str) -> tuple[str, dict[str, int]]:
    reader = csv.DictReader(io.StringIO(text))
    if reader.fieldnames is None:
        raise SystemExit("stop_times.txt is missing a header")

    trips: dict[str, list[dict[str, str]]] = {}
    order: list[str] = []
    total_rows = 0
    blank_before = 0
    for row in reader:
        total_rows += 1
        trip_id = row.get("trip_id", "")
        if trip_id not in trips:
            trips[trip_id] = []
            order.append(trip_id)
        if not row.get("arrival_time") or not row.get("departure_time"):
            blank_before += 1
        trips[trip_id].append(row)

    filled_rows = 0
    trips_without_timepoints = 0
    output = io.StringIO()
    writer = csv.DictWriter(output, fieldnames=reader.fieldnames, lineterminator="\n")
    writer.writeheader()
    for trip_id in order:
        rows = trips[trip_id]
        rows.sort(key=lambda row: int(row.get("stop_sequence") or 0))
        changed, has_timepoint = fill_trip_times(rows)
        filled_rows += changed
        if not has_timepoint:
            trips_without_timepoints += 1
        writer.writerows(rows)

    return output.getvalue(), {
        "rows": total_rows,
        "rows_with_blank_time_before": blank_before,
        "rows_filled": filled_rows,
        "trips": len(order),
        "trips_without_timepoints": trips_without_timepoints,
    }


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def fill_trip_times(rows: list[dict[str, str]]) -> tuple[int, bool]:
    known: list[tuple[int, int]] = []
    for index, row in enumerate(rows):
        raw_time = row.get("departure_time") or row.get("arrival_time") or ""
        if raw_time:
            seconds = parse_time(raw_time)
            known.append((index, seconds))
            if not row.get("arrival_time"):
                row["arrival_time"] = format_time(seconds)
            if not row.get("departure_time"):
                row["departure_time"] = format_time(seconds)

    if not known:
        for index, row in enumerate(rows):
            seconds = index * 60
            row["arrival_time"] = format_time(seconds)
            row["departure_time"] = format_time(seconds)
        return len(rows), False

    changed = 0
    first_index, first_seconds = known[0]
    for index in range(0, first_index):
        changed += fill_row(rows[index], first_seconds)

    for (left_index, left_seconds), (right_index, right_seconds) in zip(known, known[1:]):
        span = right_index - left_index
        if span <= 0:
            continue
        if right_seconds < left_seconds:
            right_seconds = left_seconds
        for index in range(left_index + 1, right_index):
            ratio = (index - left_index) / span
            seconds = round(left_seconds + (right_seconds - left_seconds) * ratio)
            changed += fill_row(rows[index], seconds)

    last_index, last_seconds = known[-1]
    for index in range(last_index + 1, len(rows)):
        changed += fill_row(rows[index], last_seconds)

    return changed, True


def fill_row(row: dict[str, str], seconds: int) -> int:
    changed = 0
    value = format_time(seconds)
    if not row.get("arrival_time"):
        row["arrival_time"] = value
        changed = 1
    if not row.get("departure_time"):
        row["departure_time"] = value
        changed = 1
    return changed


def parse_time(value: str) -> int:
    hour, minute, second = [int(part) for part in value.split(":")]
    return hour * 3600 + minute * 60 + second


def format_time(seconds: int) -> str:
    hour = seconds // 3600
    minute = (seconds % 3600) // 60
    second = seconds % 60
    return f"{hour:02d}:{minute:02d}:{second:02d}"


if __name__ == "__main__":
    main()
