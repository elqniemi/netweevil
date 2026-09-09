#!/usr/bin/env python3
"""Compare every HTTP matrix cell with independent exact CLI routes."""

import argparse
import csv
import json
import math
import subprocess
import urllib.request
from pathlib import Path


def execute(url, points, profile_id):
    document = {"points": points, "returns": {"geometry": "none"}}
    payload = {
        "engine_mode": "auto",
        "request": {"origins": document, "destinations": document},
    }
    if profile_id:
        payload["profile_id"] = profile_id
    request = urllib.request.Request(
        url.rstrip("/") + "/v1/matrix",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=3600) as response:
        return json.load(response)["result"]


def compare(candidate, reference, expected_cells, tolerance):
    failures = []
    key = lambda cell: (cell["origin_id"], cell["destination_id"])
    actual = {key(cell): cell for cell in candidate["cells"]}
    exact = {tuple(row["id"].split("_")): row for row in reference["requests"]}
    if len(actual) != expected_cells or len(exact) != expected_cells or actual.keys() != exact.keys():
        return [{"error": "missing, duplicate or unexpected matrix cells"}]
    for pair, cell in actual.items():
        other = exact[pair]
        errors = []
        if (cell["outcome"] != "unreachable" and cell.get("error") is None) != other["ok"]:
            errors.append("reachability differs")
        if cell["fallback_used"] or cell["violation_count"]:
            errors.append("route uses fallback or violates restrictions")
        for field, exact_field in [
            ("total_distance_m", "distance_m"),
            ("total_travel_time_s", "duration_s"),
            ("total_generalized_cost", "generalized_cost"),
        ]:
            value, expected = cell.get(field), other.get(exact_field)
            if value is None or expected is None:
                if value != expected:
                    errors.append(field)
            elif not math.isclose(value, expected, abs_tol=tolerance, rel_tol=0):
                errors.append(f"{field}: {value} versus exact {expected}")
        if errors:
            failures.append({"origin_id": pair[0], "destination_id": pair[1], "errors": errors})
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:8080")
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--size", type=int, default=20)
    parser.add_argument("--profile-id")
    parser.add_argument("--dataset", required=True)
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=Path("target/release/netweevil"))
    parser.add_argument("--tolerance", type=float, default=1e-6)
    parser.add_argument("--json", type=Path, required=True)
    args = parser.parse_args()
    if args.size <= 0 or not math.isfinite(args.tolerance) or args.tolerance < 0:
        parser.error("size must be positive and tolerance must be finite and nonnegative")
    with args.corpus.open(newline="") as source:
        rows = list(csv.DictReader(source))[:args.size]
    if not rows:
        parser.error("empty corpus")
    points = [
        {"id": str(index), "lon": float(row["source_lon"]), "lat": float(row["source_lat"])}
        for index, row in enumerate(rows)
    ]
    args.json.parent.mkdir(parents=True, exist_ok=True)
    corpus_path = args.json.with_suffix(".corpus.csv")
    exact_path = args.json.with_suffix(".exact.json")
    with corpus_path.open("w", newline="") as output:
        writer = csv.writer(output)
        writer.writerow(["id", "source_lon", "source_lat", "target_lon", "target_lat"])
        for origin in points:
            for destination in points:
                writer.writerow([
                    origin["id"] + "_" + destination["id"],
                    origin["lon"], origin["lat"], destination["lon"], destination["lat"],
                ])
    print(f"checking {len(points)}x{len(points)} matrix", flush=True)
    candidate = execute(args.url, points, args.profile_id)
    subprocess.run([
        str(args.binary.resolve()), "bench", "run", "--engine", "exact",
        "--dataset", args.dataset, "--profile", str(args.profile),
        "--corpus", str(corpus_path), "--json", str(exact_path),
    ], check=True)
    reference = json.loads(exact_path.read_text())
    failures = compare(candidate, reference, len(points) ** 2, args.tolerance)
    report = {
        "checked": len(points) ** 2,
        "tolerance": args.tolerance,
        "failures": failures,
        "accelerated": candidate,
        "exact": reference,
    }
    args.json.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"checked": report["checked"], "failed": len(failures)}))
    raise SystemExit(bool(failures))


if __name__ == "__main__":
    main()
