#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

DATASET_ID="${DATASET_ID:-ile_2026_03}"
PROFILE_PATH="${PROFILE_PATH:-examples/profiles/car_research_v1.yml}"
API_BIND="${API_BIND:-127.0.0.1:8080}"
API_BASE_URL="${API_BASE_URL:-http://${API_BIND}}"
REQUEST_COUNT="${REQUEST_COUNT:-20}"
CONNECT_TIMEOUT_SECONDS="${CONNECT_TIMEOUT_SECONDS:-2}"
MAX_TIME_SECONDS="${MAX_TIME_SECONDS:-120}"
START_API="${START_API:-1}"
KEEP_API_RUNNING="${KEEP_API_RUNNING:-0}"
SERVER_LOG_PATH="${SERVER_LOG_PATH:-${ROOT_DIR}/.netan/runs/perf-api-server.log}"
RESULTS_PATH="${RESULTS_PATH:-${ROOT_DIR}/.netan/runs/perf-random-routes-results.jsonl}"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required" >&2
  exit 1
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required" >&2
  exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required" >&2
  exit 1
fi

mkdir -p "$(dirname "$SERVER_LOG_PATH")" "$(dirname "$RESULTS_PATH")"

SERVER_PID=""

cleanup() {
  if [[ -n "${SERVER_PID}" && "${KEEP_API_RUNNING}" != "1" ]]; then
    kill "${SERVER_PID}" >/dev/null 2>&1 || true
    wait "${SERVER_PID}" 2>/dev/null || true
  fi
}

trap cleanup EXIT

wait_for_api() {
  local attempts=0
  until curl --silent --show-error --fail "${API_BASE_URL}/healthz" >/dev/null 2>&1; do
    attempts=$((attempts + 1))
    if (( attempts > 120 )); then
      echo "API did not become ready at ${API_BASE_URL}" >&2
      exit 1
    fi
    sleep 0.5
  done
}

start_api_if_needed() {
  if curl --silent --show-error --fail "${API_BASE_URL}/healthz" >/dev/null 2>&1; then
    echo "Using existing API at ${API_BASE_URL}"
    return
  fi

  if [[ "${START_API}" != "1" ]]; then
    echo "API is not reachable at ${API_BASE_URL} and START_API=0" >&2
    exit 1
  fi

  echo "Starting API for dataset ${DATASET_ID} on ${API_BIND}"
  (
    cd "${ROOT_DIR}"
    cargo run -p netan-cli -- api serve \
      --dataset "${DATASET_ID}" \
      --default-profile "${PROFILE_PATH}" \
      --bind "${API_BIND}"
  ) >"${SERVER_LOG_PATH}" 2>&1 &
  SERVER_PID=$!
  wait_for_api
}

start_api_if_needed

SERVICE_JSON="$(curl --silent --show-error --fail "${API_BASE_URL}/v1/service")"

export SERVICE_JSON API_BASE_URL REQUEST_COUNT CONNECT_TIMEOUT_SECONDS MAX_TIME_SECONDS RESULTS_PATH DATASET_ID

python3 <<'PY'
import json
import os
import random
import statistics
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

service = json.loads(os.environ["SERVICE_JSON"])
dataset = service.get("dataset", {})
dataset_id = dataset.get("dataset_id", "")
expected_dataset_id = os.environ["DATASET_ID"]
if dataset_id != expected_dataset_id:
    raise SystemExit(
        f"API dataset_id mismatch: expected '{expected_dataset_id}', got '{dataset_id or 'unknown'}'"
    )

bounds = dataset.get("topology_bounds") or {}
required_keys = ["min_lon", "min_lat", "max_lon", "max_lat"]
missing = [key for key in required_keys if key not in bounds]
if missing:
    raise SystemExit(
        "API service did not return topology_bounds; missing keys: {}".format(", ".join(missing))
    )

request_count = int(os.environ["REQUEST_COUNT"])
api_base_url = os.environ["API_BASE_URL"].rstrip("/")
results_path = Path(os.environ["RESULTS_PATH"])
connect_timeout = float(os.environ["CONNECT_TIMEOUT_SECONDS"])
max_time = float(os.environ["MAX_TIME_SECONDS"])

min_lon = float(bounds["min_lon"])
min_lat = float(bounds["min_lat"])
max_lon = float(bounds["max_lon"])
max_lat = float(bounds["max_lat"])

random.seed()
latencies_ms = []
successes = 0
failures = 0

results_path.parent.mkdir(parents=True, exist_ok=True)

def random_point(point_id: str) -> dict:
    return {
        "id": point_id,
        "lon": random.uniform(min_lon, max_lon),
        "lat": random.uniform(min_lat, max_lat),
    }

print(
    "Running {} random route requests against {} using dataset '{}'.".format(
        request_count, api_base_url, dataset_id
    )
)
print(
    "Bounds: min_lon={:.6f}, min_lat={:.6f}, max_lon={:.6f}, max_lat={:.6f}".format(
        min_lon, min_lat, max_lon, max_lat
    )
)

with results_path.open("w", encoding="utf-8") as handle:
    for index in range(1, request_count + 1):
        payload = {
            "request": {
                "route_id": f"perf_route_{index:04d}",
                "origin": random_point(f"origin_{index:04d}"),
                "destination": random_point(f"destination_{index:04d}"),
                "snap": {"max_distance_m": 500.0},
                "returns": {"geometry": "full"},
            }
        }
        encoded = json.dumps(payload)
        started = time.perf_counter()
        completed = subprocess.run(
            [
                "curl",
                "--silent",
                "--show-error",
                "--output",
                "/dev/null",
                "--write-out",
                "%{http_code}",
                "--connect-timeout",
                str(connect_timeout),
                "--max-time",
                str(max_time),
                "-H",
                "content-type: application/json",
                "-X",
                "POST",
                f"{api_base_url}/v1/route",
                "--data",
                encoded,
            ],
            text=True,
            capture_output=True,
        )
        elapsed_ms = (time.perf_counter() - started) * 1000.0

        curl_ok = completed.returncode == 0
        status_code = (completed.stdout or "").strip()
        success = curl_ok and status_code == "200"
        if success:
            successes += 1
            latencies_ms.append(elapsed_ms)
        else:
            failures += 1

        record = {
            "request_index": index,
            "status_code": status_code,
            "curl_return_code": completed.returncode,
            "latency_ms": round(elapsed_ms, 3),
            "success": success,
            "origin": payload["request"]["origin"],
            "destination": payload["request"]["destination"],
            "stderr": completed.stderr.strip(),
        }
        handle.write(json.dumps(record) + "\n")

        if index == 1 or index % 5 == 0 or index == request_count:
            print(
                "Completed {:>3}/{} requests; successes={}, failures={}".format(
                    index, request_count, successes, failures
                )
            )

summary = {
    "dataset_id": dataset_id,
    "request_count": request_count,
    "successes": successes,
    "failures": failures,
    "results_path": str(results_path),
}

if latencies_ms:
    sorted_latencies = sorted(latencies_ms)

    def percentile(values, pct):
        if len(values) == 1:
            return values[0]
        rank = (len(values) - 1) * pct
        lower = int(rank)
        upper = min(lower + 1, len(values) - 1)
        weight = rank - lower
        return values[lower] * (1.0 - weight) + values[upper] * weight

    summary.update(
        {
            "min_latency_ms": round(sorted_latencies[0], 3),
            "mean_latency_ms": round(statistics.mean(sorted_latencies), 3),
            "median_latency_ms": round(statistics.median(sorted_latencies), 3),
            "p95_latency_ms": round(percentile(sorted_latencies, 0.95), 3),
            "p99_latency_ms": round(percentile(sorted_latencies, 0.99), 3),
            "max_latency_ms": round(sorted_latencies[-1], 3),
        }
    )

print(json.dumps(summary, indent=2))
PY
