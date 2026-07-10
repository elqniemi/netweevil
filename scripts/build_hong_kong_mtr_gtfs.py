#!/usr/bin/env python3
"""Build a GTFS-compatible MTR topology feed from the audited platform join."""

from __future__ import annotations

import argparse
import csv
import io
import json
import zipfile
from collections import defaultdict
from datetime import date
from pathlib import Path
from typing import Any


LINE_NAMES = {
    "AEL": "Airport Express",
    "DRL": "Disneyland Resort Line",
    "EAL": "East Rail Line",
    "ISL": "Island Line",
    "KTL": "Kwun Tong Line",
    "SIL": "South Island Line",
    "TCL": "Tung Chung Line",
    "TKL": "Tseung Kwan O Line",
    "TML": "Tuen Ma Line",
    "TWL": "Tsuen Wan Line",
}

LINE_COLOURS = {
    "AEL": "00888A",
    "DRL": "E777B7",
    "EAL": "5EB6E4",
    "ISL": "007DC5",
    "KTL": "00AB4E",
    "SIL": "B5BD00",
    "TCL": "F7943E",
    "TKL": "7D499D",
    "TML": "9A3820",
    "TWL": "ED1D24",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform-join-dir", type=Path, required=True)
    parser.add_argument("--mtr-lines", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--headway-seconds", type=int, default=300)
    parser.add_argument("--segment-seconds", type=int, default=180)
    parser.add_argument("--first-service", default="05:30:00")
    parser.add_argument("--last-service", default="25:00:00")
    parser.add_argument("--calendar-start", default="2026-01-01")
    parser.add_argument("--calendar-end", default="2027-12-31")
    return parser.parse_args()


def read_csv(path: Path) -> list[dict[str, str]]:
    with path.open("r", encoding="utf-8-sig", newline="") as handle:
        return list(csv.DictReader(handle))


def csv_bytes(fieldnames: list[str], rows: list[dict[str, Any]]) -> bytes:
    buffer = io.StringIO(newline="")
    writer = csv.DictWriter(buffer, fieldnames=fieldnames, lineterminator="\n")
    writer.writeheader()
    writer.writerows(rows)
    return buffer.getvalue().encode("utf-8")


def gtfs_date(value: str) -> str:
    return date.fromisoformat(value).strftime("%Y%m%d")


def seconds_to_gtfs(value: int) -> str:
    hours, remainder = divmod(value, 3600)
    minutes, seconds = divmod(remainder, 60)
    return f"{hours:02d}:{minutes:02d}:{seconds:02d}"


def candidate_priority(row: dict[str, str]) -> tuple[Any, ...]:
    line_method = {
        "level_name": 0,
        "single_line_station": 1,
        "interchange_shared_or_unlabelled": 2,
    }.get(row["line_match_method"], 9)
    source_kind = 0 if row["platform_source_kind"] == "unit" else 1
    return (
        line_method,
        source_kind,
        float(row["centroid_to_network_m"]),
        row["stop_id"],
    )


def select_platform_stops(join_rows: list[dict[str, str]]) -> tuple[dict[tuple[str, str], str], list[dict[str, Any]]]:
    candidates: dict[tuple[str, str], list[dict[str, str]]] = defaultdict(list)
    for row in join_rows:
        candidates[(row["line_code"], row["station_code"])].append(row)
    selected: dict[tuple[str, str], str] = {}
    audit: list[dict[str, Any]] = []
    for key, rows in sorted(candidates.items()):
        rows.sort(key=candidate_priority)
        selected[key] = rows[0]["stop_id"]
        audit.append(
            {
                "line_code": key[0],
                "station_code": key[1],
                "selected_stop_id": rows[0]["stop_id"],
                "candidate_stop_ids": [row["stop_id"] for row in rows],
                "selection_method": "deterministic_best_platform_candidate",
                "direction_specific_geometry": False,
            }
        )
    return selected, audit


def build_files(args: argparse.Namespace) -> tuple[dict[str, bytes], dict[str, Any]]:
    join_rows = read_csv(args.platform_join_dir / "mtr_platform_join.csv")
    selected, selection_audit = select_platform_stops(join_rows)
    source_rows = [
        row
        for row in read_csv(args.mtr_lines)
        if row.get("Line Code", "").strip() and row.get("English Name", "").strip()
    ]

    services: dict[tuple[str, str], list[dict[str, str]]] = defaultdict(list)
    for row in source_rows:
        services[(row["Line Code"].strip(), row["Direction"].strip())].append(row)
    for rows in services.values():
        rows.sort(key=lambda row: float(row["Sequence"]))

    route_rows = [
        {
            "route_id": line_code,
            "agency_id": "MTR",
            "route_short_name": line_code,
            "route_long_name": LINE_NAMES.get(line_code, line_code),
            "route_type": 1,
            "route_color": LINE_COLOURS.get(line_code, "666666"),
            "route_text_color": "FFFFFF",
        }
        for line_code in sorted({key[0] for key in services})
    ]
    trip_rows: list[dict[str, Any]] = []
    stop_time_rows: list[dict[str, Any]] = []
    frequency_rows: list[dict[str, Any]] = []
    missing: list[dict[str, str]] = []
    for (line_code, direction), rows in sorted(services.items()):
        trip_id = f"{line_code}_{direction}_FREQUENCY"
        trip_rows.append(
            {
                "route_id": line_code,
                "service_id": "MTR_DAILY_SYNTHETIC",
                "trip_id": trip_id,
                "trip_headsign": rows[-1]["English Name"].strip(),
                "direction_id": 0 if direction == "DT" else 1,
            }
        )
        for sequence, row in enumerate(rows, start=1):
            station_code = row["Station Code"].strip()
            stop_id = selected.get((line_code, station_code))
            if stop_id is None:
                missing.append({"line_code": line_code, "station_code": station_code})
                continue
            timestamp = seconds_to_gtfs((sequence - 1) * args.segment_seconds)
            stop_time_rows.append(
                {
                    "trip_id": trip_id,
                    "arrival_time": timestamp,
                    "departure_time": timestamp,
                    "stop_id": stop_id,
                    "stop_sequence": sequence,
                    "pickup_type": 0,
                    "drop_off_type": 0,
                    "timepoint": 1,
                }
            )
        frequency_rows.append(
            {
                "trip_id": trip_id,
                "start_time": args.first_service,
                "end_time": args.last_service,
                "headway_secs": args.headway_seconds,
                "exact_times": 0,
            }
        )
    if missing:
        raise RuntimeError(f"platform join lacks {len(missing)} scheduled station-line stops: {missing[:5]}")

    files = {
        "agency.txt": csv_bytes(
            ["agency_id", "agency_name", "agency_url", "agency_timezone", "agency_lang"],
            [
                {
                    "agency_id": "MTR",
                    "agency_name": "MTR Corporation Limited (synthetic topology feed)",
                    "agency_url": "https://www.mtr.com.hk/",
                    "agency_timezone": "Asia/Hong_Kong",
                    "agency_lang": "en",
                }
            ],
        ),
        "stops.txt": (args.platform_join_dir / "gtfs_stops.txt").read_bytes(),
        "routes.txt": csv_bytes(
            [
                "route_id",
                "agency_id",
                "route_short_name",
                "route_long_name",
                "route_type",
                "route_color",
                "route_text_color",
            ],
            route_rows,
        ),
        "trips.txt": csv_bytes(
            ["route_id", "service_id", "trip_id", "trip_headsign", "direction_id"],
            trip_rows,
        ),
        "stop_times.txt": csv_bytes(
            [
                "trip_id",
                "arrival_time",
                "departure_time",
                "stop_id",
                "stop_sequence",
                "pickup_type",
                "drop_off_type",
                "timepoint",
            ],
            stop_time_rows,
        ),
        "calendar.txt": csv_bytes(
            [
                "service_id",
                "monday",
                "tuesday",
                "wednesday",
                "thursday",
                "friday",
                "saturday",
                "sunday",
                "start_date",
                "end_date",
            ],
            [
                {
                    "service_id": "MTR_DAILY_SYNTHETIC",
                    "monday": 1,
                    "tuesday": 1,
                    "wednesday": 1,
                    "thursday": 1,
                    "friday": 1,
                    "saturday": 1,
                    "sunday": 1,
                    "start_date": gtfs_date(args.calendar_start),
                    "end_date": gtfs_date(args.calendar_end),
                }
            ],
        ),
        "frequencies.txt": csv_bytes(
            ["trip_id", "start_time", "end_time", "headway_secs", "exact_times"],
            frequency_rows,
        ),
    }
    manifest = {
        "feed_kind": "synthetic_mtr_topology_and_headway_validation",
        "scientific_timetable_ready": False,
        "warning": (
            "Headways and interstation running times are explicit placeholders. "
            "Replace them with calibrated MTR service data before scientific travel-time analysis."
        ),
        "route_count": len(route_rows),
        "trip_template_count": len(trip_rows),
        "stop_time_count": len(stop_time_rows),
        "platform_stop_count": len(join_rows),
        "selected_station_line_stop_count": len(selected),
        "headway_seconds": args.headway_seconds,
        "segment_seconds": args.segment_seconds,
        "first_service": args.first_service,
        "last_service": args.last_service,
        "calendar_start": args.calendar_start,
        "calendar_end": args.calendar_end,
        "platform_selection": selection_audit,
    }
    return files, manifest


def main() -> int:
    args = parse_args()
    if args.headway_seconds <= 0 or args.segment_seconds <= 0:
        raise ValueError("headway and segment seconds must be positive")
    files, manifest = build_files(args)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(args.output, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, content in sorted(files.items()):
            info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            archive.writestr(info, content)
    manifest_path = args.output.with_suffix(".manifest.json")
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: value for key, value in manifest.items() if key != "platform_selection"}, indent=2))
    print(f"GTFS: {args.output}")
    print(f"Manifest: {manifest_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
