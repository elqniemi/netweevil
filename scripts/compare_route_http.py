#!/usr/bin/env python3
"""Compare two NetWeevil APIs with alternating paired requests on one corpus."""

import argparse
import csv
import hashlib
import http.client
import json
import math
import statistics
import time
from pathlib import Path
from urllib.parse import urlsplit


def percentile(values, fraction):
    values = sorted(values)
    position = (len(values) - 1) * fraction
    lower = int(position)
    upper = min(lower + 1, len(values) - 1)
    return values[lower] + (values[upper] - values[lower]) * (position - lower)


class Endpoint:
    def __init__(self, url):
        parsed = urlsplit(url)
        if parsed.scheme not in ("http", "https") or not parsed.hostname:
            raise ValueError("endpoint must be an HTTP(S) URL")
        cls = http.client.HTTPSConnection if parsed.scheme == "https" else http.client.HTTPConnection
        self.connection = cls(parsed.hostname, parsed.port, timeout=300)
        self.base_path = parsed.path.rstrip("/")
        self.path = self.base_path + "/v1/route"
        self.context = None

    def service_info(self):
        self.connection.request("GET", self.base_path + "/v1/service")
        response = self.connection.getresponse()
        raw = response.read()
        if response.status != 200:
            raise RuntimeError(f"service metadata HTTP {response.status}: {raw.decode()[:1000]}")
        return json.loads(raw)

    def route(self, body):
        start = time.perf_counter_ns()
        self.connection.request("POST", self.path, body, {"Content-Type": "application/json"})
        response = self.connection.getresponse()
        raw = response.read()
        elapsed_ms = (time.perf_counter_ns() - start) / 1e6
        if response.status != 200:
            raise RuntimeError(f"HTTP {response.status}: {raw.decode()[:1000]}")
        document = json.loads(raw)
        context = {key: document["service"][key] for key in ("dataset_id", "profile_id", "profile_hash")}
        if self.context is not None and self.context != context:
            raise RuntimeError("server changed dataset or profile during comparison")
        self.context = context
        result = document["result"]
        if result["outcome"] != "legal" or result["fallback_used"]:
            raise RuntimeError("comparison requires a legal route without fallback")
        return elapsed_ms, result["summary"]

    def close(self):
        self.connection.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True)
    parser.add_argument("--candidate", required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--profile-id")
    parser.add_argument("--iterations", type=int, default=3)
    parser.add_argument("--warmup", type=int, default=20)
    parser.add_argument("--json", type=Path, required=True)
    args = parser.parse_args()
    if args.iterations < 1 or args.warmup < 0:
        parser.error("iterations must be positive and warmup nonnegative")
    with args.corpus.open(newline="") as source:
        rows = list(csv.DictReader(source))
    if not rows:
        parser.error("empty corpus")
    payloads = []
    for row in rows:
        payload = {"request": {
            "route_id": row["id"],
            "origin": {"id": "o", "lon": float(row["source_lon"]), "lat": float(row["source_lat"])},
            "destination": {"id": "d", "lon": float(row["target_lon"]), "lat": float(row["target_lat"])},
            "snap": {"max_distance_m": 500}, "returns": {"geometry": "none"},
        }}
        if args.profile_id:
            payload["profile_id"] = args.profile_id
        payloads.append(json.dumps(payload, allow_nan=False).encode())
    endpoints = [Endpoint(args.baseline), Endpoint(args.candidate)]
    samples = []
    try:
        service_snapshots = [endpoint.service_info() for endpoint in endpoints]
        for key in ("dataset_id", "source_sha256", "node_count", "edge_count", "turn_count"):
            if service_snapshots[0]["dataset"][key] != service_snapshots[1]["dataset"][key]:
                raise RuntimeError(f"baseline and candidate dataset metadata differ: {key}")
        for index in range(args.warmup):
            for endpoint in endpoints:
                endpoint.route(payloads[index % len(payloads)])
        for iteration in range(args.iterations):
            for index, (row, body) in enumerate(zip(rows, payloads)):
                results = [None, None]
                order = [0, 1] if (index + iteration) % 2 == 0 else [1, 0]
                for endpoint in order:
                    results[endpoint] = endpoints[endpoint].route(body)
                if endpoints[0].context != endpoints[1].context:
                    raise RuntimeError("baseline and candidate must select the same dataset and profile hash")
                errors = []
                for key in ("total_distance_m", "total_travel_time_s", "total_generalized_cost"):
                    values = [result[1][key] for result in results]
                    if not all(math.isfinite(value) for value in values) or not math.isclose(*values, abs_tol=1e-6, rel_tol=0):
                        errors.append(key)
                samples.append({"id": row["id"], "iteration": iteration,
                                "baseline_ms": results[0][0], "candidate_ms": results[1][0],
                                "metric_mismatches": errors})
            print(f"paired {len(samples)} requests per endpoint", flush=True)
        for endpoint, before in zip(endpoints, service_snapshots):
            if endpoint.service_info() != before:
                raise RuntimeError("server metadata changed during comparison")
    finally:
        for endpoint in endpoints:
            endpoint.close()
    baseline = [sample["baseline_ms"] for sample in samples]
    candidate = [sample["candidate_ms"] for sample in samples]
    summary = {
        "pairs": len(samples),
        "baseline_p50_ms": percentile(baseline, 0.5), "baseline_p95_ms": percentile(baseline, 0.95),
        "candidate_p50_ms": percentile(candidate, 0.5), "candidate_p95_ms": percentile(candidate, 0.95),
        "median_paired_delta_ms": statistics.median(b - a for a, b in zip(baseline, candidate)),
        "median_paired_ratio": statistics.median(b / a for a, b in zip(baseline, candidate)),
        "metric_mismatch_count": sum(bool(sample["metric_mismatches"]) for sample in samples),
    }
    report = {"baseline": args.baseline, "candidate": args.candidate,
              "corpus": str(args.corpus), "iterations": args.iterations, "warmup": args.warmup,
              "payload_sha256": hashlib.sha256(b"\n".join(payloads)).hexdigest(),
              "service": endpoints[0].context,
              "service_snapshots": {"baseline": service_snapshots[0], "candidate": service_snapshots[1]},
              "timing": "persistent HTTP request through full response body; JSON parsing excluded",
              "summary": summary, "samples": samples}
    args.json.parent.mkdir(parents=True, exist_ok=True)
    args.json.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(summary))
    raise SystemExit(bool(summary["metric_mismatch_count"]))


if __name__ == "__main__":
    main()
