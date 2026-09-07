#!/usr/bin/env python3
"""Measure prepared routes and optional request-profile preparation on a local API.

Starts and stops its own server. Run baseline and candidate binaries separately,
without other builds/benchmarks running. RSS sampling requires Linux /proc.
"""

import argparse
import csv
import hashlib
import http.client
import json
import os
from pathlib import Path
import signal
import statistics
import subprocess
import threading
import time


def memory(pid):
    try:
        fields = dict(line.split(":", 1) for line in Path(f"/proc/{pid}/status").read_text().splitlines())
        return {key: int(fields[key].split()[0]) * 1024 for key in ("VmRSS", "VmHWM")}
    except (FileNotFoundError, KeyError):
        return {}


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--dataset", required=True)
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, default=Path("examples/perf/north_nl_routes.csv"))
    parser.add_argument("--limit", type=int, default=150)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--port", type=int, default=18081)
    parser.add_argument("--overrides", type=Path, help="JSON overrides; omit for the baseline binary")
    parser.add_argument("--reference", type=Path, help="Assert prepared results equal a previous report")
    parser.add_argument("--json", type=Path, required=True)
    args = parser.parse_args()
    if args.limit < 1 or args.rounds < 1:
        parser.error("limit and rounds must be positive")
    with args.corpus.open(newline="") as source:
        rows = list(csv.DictReader(source))[:args.limit]
    if not rows:
        parser.error("empty corpus")
    payloads = [{"request": {
        "route_id": row["id"],
        "origin": {"id": "o", "lon": float(row["source_lon"]), "lat": float(row["source_lat"])},
        "destination": {"id": "d", "lon": float(row["target_lon"]), "lat": float(row["target_lat"])},
        "snap": {"max_distance_m": 500},
        "returns": {"geometry": "none", "segment_rows": False},
    }} for row in rows]
    args.json.parent.mkdir(parents=True, exist_ok=True)
    command = [str(args.binary.resolve()), "api", "serve", "--dataset", args.dataset,
               "--default-profile", str(args.profile), "--bind", f"127.0.0.1:{args.port}"]
    report = {"command": command, "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              "corpus_sha256": hashlib.sha256(args.corpus.read_bytes()).hexdigest(),
              "requests": len(rows), "rounds": args.rounds, "stages": {}}
    stop = threading.Event()
    samples = []
    with args.json.with_suffix(".server.log").open("w") as log:
        server = subprocess.Popen(command, stdout=log, stderr=log, env={**os.environ, "RUST_LOG": "warn"})
        connection = None
        try:
            deadline = time.monotonic() + 600
            while True:
                if server.poll() is not None:
                    raise RuntimeError(f"API exited; see {log.name}")
                try:
                    connection = http.client.HTTPConnection("127.0.0.1", args.port, timeout=600)
                    connection.request("GET", "/readyz")
                    response = connection.getresponse()
                    report["service"] = json.loads(response.read())
                    if response.status == 200:
                        break
                except OSError:
                    if connection:
                        connection.close()
                if time.monotonic() > deadline:
                    raise TimeoutError("API did not become ready")
                time.sleep(0.25)
            connection.request("GET", "/healthz")
            health = connection.getresponse()
            assert health.status == 200 and json.loads(health.read())["status"] == "ok"

            def sample():
                while not stop.wait(0.05):
                    samples.append(memory(server.pid).get("VmRSS", 0))

            sampler = threading.Thread(target=sample, daemon=True)
            sampler.start()

            def post(payload):
                encoded = json.dumps(payload).encode()
                started = time.perf_counter()
                connection.request("POST", "/v1/route", body=encoded, headers={"Content-Type": "application/json"})
                response = connection.getresponse()
                raw = response.read()
                elapsed = (time.perf_counter() - started) * 1000
                if response.status != 200:
                    raise RuntimeError(f"route returned {response.status}: {raw.decode()}")
                body = json.loads(raw)
                if body["result"]["outcome"] == "unreachable":
                    raise RuntimeError(f"unreachable route: {payload['request']['route_id']}")
                return elapsed, dict(response.getheaders()), body

            def stage(name, overrides=None):
                latencies, results = [], {}
                for _ in range(args.rounds):
                    for payload in payloads:
                        request = dict(payload)
                        if overrides is not None:
                            request["profile_overrides"] = overrides
                        elapsed, headers, body = post(request)
                        if overrides is not None:
                            assert headers["x-netweevil-profile-cache"] == "hit", headers
                        else:
                            assert "x-netweevil-profile-cache" not in headers
                        latencies.append(elapsed)
                        route_id = payload["request"]["route_id"]
                        signature = digest(body["result"])
                        assert results.get(route_id, signature) == signature, f"unstable route {route_id}"
                        results[route_id] = signature
                ordered = sorted(latencies)
                stats = {"count": len(latencies), "p50_ms": statistics.median(latencies),
                         "p95_ms": ordered[int((len(ordered) - 1) * 0.95)],
                         "mean_ms": statistics.mean(latencies), "memory": memory(server.pid),
                         "result_hashes": results}
                report["stages"][name] = stats
                print(name, json.dumps({k: v for k, v in stats.items() if k != "result_hashes"}), flush=True)
                return results

            # Warm every route and worker scratch before timing prepared routes.
            for payload in payloads:
                post(payload)
            before = stage("prepared_before")
            if args.reference:
                reference = json.loads(args.reference.read_text())
                assert before == reference["stages"]["prepared_before"]["result_hashes"], "prepared routes changed"
            if args.overrides:
                overrides = json.loads(args.overrides.read_text())
                report["overrides"] = overrides
                samples.clear()
                rss_before = memory(server.pid)
                elapsed, headers, body = post({**payloads[0], "profile_overrides": overrides})
                assert headers["x-netweevil-profile-cache"] == "miss", headers
                report["cold"] = {"elapsed_ms": elapsed,
                    "prepare_ms": float(headers["x-netweevil-profile-prepare-ms"]),
                    "memory_before": rss_before, "memory_after": memory(server.pid),
                    "peak_sampled_rss": max(samples, default=0), "service": body["service"]}
                print("cold", json.dumps(report["cold"]), flush=True)
                for payload in payloads:
                    post({**payload, "profile_overrides": overrides})
                stage("dynamic_warm", overrides)
                after = stage("prepared_after")
                assert before == after, "dynamic compilation changed prepared results"
            args.json.write_text(json.dumps(report, indent=2) + "\n")
        finally:
            stop.set()
            if connection:
                connection.close()
            if server.poll() is None:
                server.send_signal(signal.SIGINT)
                try:
                    server.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


if __name__ == "__main__":
    main()
