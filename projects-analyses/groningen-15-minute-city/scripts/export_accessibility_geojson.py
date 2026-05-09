#!/usr/bin/env python3
"""Join accessibility score CSVs onto residential H3 GeoJSON geometry."""

from __future__ import annotations

import argparse
import csv
import json
from collections import defaultdict
from pathlib import Path
from typing import Any


STUDY_DIR = Path(__file__).resolve().parents[1]
H3_GEOJSON = STUDY_DIR / "outputs/processed/residential_h3_r9.geojson"
SCORES_CSV = STUDY_DIR / "outputs/processed/accessibility_scores.csv"
OUT_GEOJSON = STUDY_DIR / "outputs/maps/accessibility_scores_h3.geojson"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--h3-geojson", type=Path, default=H3_GEOJSON)
    parser.add_argument("--scores", type=Path, default=SCORES_CSV)
    parser.add_argument("--out", type=Path, default=OUT_GEOJSON)
    args = parser.parse_args()

    scores = read_scores(args.scores)
    with args.h3_geojson.open() as handle:
        source = json.load(handle)

    features = []
    for feature in source.get("features", []):
        origin_id = feature.get("properties", {}).get("id")
        for score in scores.get(origin_id, []):
            joined = dict(feature)
            joined["properties"] = {**feature.get("properties", {}), **score}
            features.append(joined)

    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w") as handle:
        json.dump({"type": "FeatureCollection", "features": features}, handle, indent=2)
        handle.write("\n")


def read_scores(path: Path) -> dict[str, list[dict[str, Any]]]:
    scores: dict[str, list[dict[str, Any]]] = defaultdict(list)
    if not path.exists() or path.stat().st_size == 0:
        return scores
    with path.open(newline="") as handle:
        for row in csv.DictReader(handle):
            origin_id = row.get("origin_id", "")
            if not origin_id:
                continue
            scores[origin_id].append(normalize_score_row(row))
    return scores


def normalize_score_row(row: dict[str, str]) -> dict[str, Any]:
    normalized: dict[str, Any] = {}
    for key, value in row.items():
        if key == "origin_id":
            continue
        if value in {"true", "false"}:
            normalized[key] = value == "true"
        else:
            normalized[key] = parse_number(value)
    return normalized


def parse_number(value: str) -> Any:
    if value == "":
        return value
    try:
        number = float(value)
    except ValueError:
        return value
    if number.is_integer():
        return int(number)
    return number


if __name__ == "__main__":
    main()
