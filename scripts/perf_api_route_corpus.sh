#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

DATASET_ID="${DATASET_ID:-ile_2026_03}"
PROFILE_PATH="${PROFILE_PATH:-examples/profiles/car_research_v1.yml}"
CORPUS_PATH="${CORPUS_PATH:-examples/perf/ile_de_france_routes.csv}"
API_BIND="${API_BIND:-127.0.0.1:8080}"
API_BASE_URL="${API_BASE_URL:-http://${API_BIND}}"
REPEATS="${REPEATS:-3}"
WARMUP_PASSES="${WARMUP_PASSES:-1}"
GEOMETRY_MODE="${GEOMETRY_MODE:-none}"
PROFILE_ID="${PROFILE_ID:-}"
CONNECT_TIMEOUT_SECONDS="${CONNECT_TIMEOUT_SECONDS:-2}"
MAX_TIME_SECONDS="${MAX_TIME_SECONDS:-120}"
START_API="${START_API:-1}"
KEEP_API_RUNNING="${KEEP_API_RUNNING:-0}"
SERVER_LOG_PATH="${SERVER_LOG_PATH:-${ROOT_DIR}/.netan/runs/perf-api-server.log}"
RESULTS_PATH="${RESULTS_PATH:-${ROOT_DIR}/.netan/runs/perf-route-corpus-results.jsonl}"

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

export SERVICE_JSON API_BASE_URL DATASET_ID PROFILE_ID CORPUS_PATH REPEATS WARMUP_PASSES GEOMETRY_MODE CONNECT_TIMEOUT_SECONDS MAX_TIME_SECONDS RESULTS_PATH

python3 <<'PY'
import csv
import json
import os
import statistics
import time
import urllib.error
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

profile_id = os.environ["PROFILE_ID"].strip() or None
corpus_path = Path(os.environ["CORPUS_PATH"])
repeats = int(os.environ["REPEATS"])
warmup_passes = int(os.environ["WARMUP_PASSES"])
geometry_mode = os.environ["GEOMETRY_MODE"]
connect_timeout = float(os.environ["CONNECT_TIMEOUT_SECONDS"])
max_time = float(os.environ["MAX_TIME_SECONDS"])
timeout = max(connect_timeout, max_time)
results_path = Path(os.environ["RESULTS_PATH"])
api_base_url = os.environ["API_BASE_URL"].rstrip("/")

if not corpus_path.exists():
    raise SystemExit(f"route corpus does not exist: {corpus_path}")

rows = list(csv.DictReader(corpus_path.open("r", encoding="utf-8", newline="")))
required = {"id", "source_x", "source_y", "target_x", "target_y"}
if not rows:
    raise SystemExit(f"route corpus is empty: {corpus_path}")
if not required.issubset(rows[0].keys()):
    missing = ", ".join(sorted(required.difference(rows[0].keys())))
    raise SystemExit(f"route corpus is missing required columns: {missing}")

results_path.parent.mkdir(parents=True, exist_ok=True)

total_requests = 0
successes = 0
failures = 0
latencies_ms = []
latency_by_route = {}

def percentile(values, pct):
    if len(values) == 1:
        return values[0]
    rank = (len(values) - 1) * pct
    lower = int(rank)
    upper = min(lower + 1, len(values) - 1)
    weight = rank - lower
    return values[lower] * (1.0 - weight) + values[upper] * weight

def route_payload(row):
    request = {
        "route_id": row["id"],
        "origin": {
            "id": f"{row['id']}_origin",
            "lon": float(row["source_x"]),
            "lat": float(row["source_y"]),
        },
        "destination": {
            "id": f"{row['id']}_destination",
            "lon": float(row["target_x"]),
            "lat": float(row["target_y"]),
        },
        "snap": {"max_distance_m": 500.0},
        "returns": {"geometry": geometry_mode},
    }
    payload = {"request": request}
    if profile_id is not None:
        payload["profile_id"] = profile_id
    return payload

def execute(payload):
    body = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        f"{api_base_url}/v1/route",
        data=body,
        headers={"content-type": "application/json"},
        method="POST",
    )
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as response:
            status_code = response.status
            response.read()
            error_message = ""
    except urllib.error.HTTPError as error:
        status_code = error.code
        error_message = error.read().decode("utf-8", errors="replace")
    elapsed_ms = (time.perf_counter() - started) * 1000.0
    return status_code, elapsed_ms, error_message

print(
    "Running route corpus benchmark against {} using dataset '{}'.".format(
        api_base_url, dataset_id
    )
)
print(
    "Corpus: {} routes, repeats={}, warmup_passes={}, geometry={}".format(
        len(rows), repeats, warmup_passes, geometry_mode
    )
)

for warmup_pass in range(warmup_passes):
    for row in rows:
        execute(route_payload(row))
    print(f"Completed warmup pass {warmup_pass + 1}/{warmup_passes}")

with results_path.open("w", encoding="utf-8") as handle:
    for repeat in range(repeats):
        for row in rows:
            payload = route_payload(row)
            status_code, elapsed_ms, error_message = execute(payload)
            success = status_code == 200
            total_requests += 1
            if success:
                successes += 1
                latencies_ms.append(elapsed_ms)
                latency_by_route.setdefault(row["id"], []).append(elapsed_ms)
            else:
                failures += 1

            record = {
                "repeat": repeat + 1,
                "route_id": row["id"],
                "status_code": status_code,
                "latency_ms": round(elapsed_ms, 3),
                "success": success,
                "error": error_message,
            }
            handle.write(json.dumps(record) + "\n")

        print(
            "Completed repeat {:>2}/{}; successes={}, failures={}".format(
                repeat + 1, repeats, successes, failures
            )
        )

summary = {
    "dataset_id": dataset_id,
    "corpus_path": str(corpus_path),
    "route_count": len(rows),
    "repeats": repeats,
    "warmup_passes": warmup_passes,
    "requests": total_requests,
    "successes": successes,
    "failures": failures,
    "results_path": str(results_path),
    "geometry_mode": geometry_mode,
}

if latencies_ms:
    sorted_latencies = sorted(latencies_ms)
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

per_route = []
for route_id in sorted(latency_by_route):
    samples = sorted(latency_by_route[route_id])
    per_route.append(
        {
            "route_id": route_id,
            "count": len(samples),
            "min_latency_ms": round(samples[0], 3),
            "median_latency_ms": round(statistics.median(samples), 3),
            "max_latency_ms": round(samples[-1], 3),
        }
    )
summary["per_route"] = per_route

print(json.dumps(summary, indent=2))
PY
