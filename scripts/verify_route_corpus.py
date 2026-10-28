#!/usr/bin/env python3
"""Check full HTTP route reconstruction against an exact benchmark report."""

import argparse
import csv
import json
import math
import urllib.request
from pathlib import Path


def verify(result, reference):
    errors = []
    segments = result.get("segments") or []
    summary = result["summary"]
    if result.get("outcome") == "unreachable" or not reference["ok"]:
        return ["route is unreachable"]
    if summary["segment_count"] != len(segments):
        errors.append("segment count differs")
    if (result.get("edge_path") or []) != [s["edge_id"] for s in segments]:
        errors.append("edge path differs from segments")
    if any(a["to_node_id"] != b["from_node_id"] for a, b in zip(segments, segments[1:])):
        errors.append("disconnected reconstructed segments")
    if summary.get("violation_count", 0) or result.get("violations"):
        errors.append("route contains reported violations")
    if result.get("fallback_used"):
        errors.append("route used connectivity fallback")
    geometry = result.get("geometry") or []
    if not geometry or any(not math.isfinite(v) for point in geometry for v in point):
        errors.append("missing or nonfinite geometry")
    else:
        for point, endpoint in [(geometry[0], "origin"), (geometry[-1], "destination")]:
            snapped = result[endpoint]
            if abs(point[0] - snapped["snapped_lon"]) > 1e-7 or abs(point[1] - snapped["snapped_lat"]) > 1e-7:
                errors.append(f"geometry does not end at snapped {endpoint}")
    for field, reference_field in [
        ("total_distance_m", "distance_m"),
        ("total_travel_time_s", "duration_s"),
        ("total_generalized_cost", "generalized_cost"),
    ]:
        expected = reference.get(reference_field)
        if expected is None:
            errors.append(f"reference lacks {reference_field}")
        elif not math.isclose(summary[field], expected, rel_tol=0, abs_tol=1e-6):
            errors.append(f"{field}: {summary[field]} versus exact {expected}")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:8080")
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--exact", type=Path, required=True)
    parser.add_argument("--json", type=Path, required=True)
    args = parser.parse_args()
    reference = {r["id"]: r for r in json.loads(args.exact.read_text())["requests"]}
    failures = []
    checked = 0
    with args.corpus.open(newline="") as source:
        rows = list(csv.DictReader(source))
    if not rows:
        raise SystemExit("empty corpus")
    for row in rows:
        route_id = row["id"]
        payload = {"request": {
            "route_id": route_id,
            "origin": {"id": route_id + "_o", "lon": float(row["source_lon"]), "lat": float(row["source_lat"])},
            "destination": {"id": route_id + "_d", "lon": float(row["target_lon"]), "lat": float(row["target_lat"])},
            "snap": {"max_distance_m": 500.0},
            "returns": {"geometry": "full", "segment_rows": True},
        }}
        request = urllib.request.Request(args.url.rstrip("/") + "/v1/route", data=json.dumps(payload).encode(), headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=300) as response:
            result = json.load(response)["result"]
        errors = verify(result, reference[route_id])
        if errors:
            failures.append({"id": route_id, "errors": errors})
        checked += 1
        if checked % 100 == 0:
            print(f"checked {checked}/{len(rows)}", flush=True)
    report = {"checked": checked, "failures": failures, "tolerance": 1e-6}
    args.json.parent.mkdir(parents=True, exist_ok=True)
    args.json.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"checked": checked, "failed": len(failures)}))
    raise SystemExit(bool(failures))


if __name__ == "__main__":
    main()
