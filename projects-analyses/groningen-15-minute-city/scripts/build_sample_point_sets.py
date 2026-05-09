#!/usr/bin/env python3
"""Build deterministic small point sets for fast study smoke runs."""

from __future__ import annotations

import argparse
import csv
from pathlib import Path


STUDY_DIR = Path(__file__).resolve().parents[1]
ORIGIN_SOURCE = STUDY_DIR / "requests/origins/residential_h3_r9_points.csv"
DESTINATION_DIR = STUDY_DIR / "requests/destinations"
SAMPLE_DIR = STUDY_DIR / "requests/samples"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--origin-count", type=int, default=24)
    parser.add_argument("--destination-count", type=int, default=24)
    parser.add_argument("--out-dir", type=Path, default=SAMPLE_DIR)
    args = parser.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)
    write_rows(args.out_dir / "residential_h3_r9_points_smoke.csv", sample_rows(read_rows(ORIGIN_SOURCE), args.origin_count))
    for path in sorted(DESTINATION_DIR.glob("*.csv")):
        write_rows(args.out_dir / f"{path.stem}_smoke.csv", sample_rows(read_rows(path), args.destination_count))


def read_rows(path: Path) -> list[dict[str, str]]:
    with path.open(newline="") as handle:
        return list(csv.DictReader(handle))


def sample_rows(rows: list[dict[str, str]], count: int) -> list[dict[str, str]]:
    if count <= 0 or len(rows) <= count:
        return rows
    if count == 1:
        return [rows[len(rows) // 2]]
    step = (len(rows) - 1) / (count - 1)
    indexes = sorted({round(index * step) for index in range(count)})
    return [rows[index] for index in indexes]


def write_rows(path: Path, rows: list[dict[str, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if not rows:
        path.write_text("id,x,y\n")
        return
    with path.open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


if __name__ == "__main__":
    main()
