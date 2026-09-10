#!/usr/bin/env bash
# Prepare the full local Hong Kong example and start the frontend/API.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
ANALYSIS="${NETWEEVIL_HK_ANALYSIS:-}"
PREPARED=""
SERVICE_START="$(date +%F)"
SERVICE_DAYS=7
SERVE=1
SERVE_ONLY=0
PROFILES_ONLY=0
TERRAIN_ONLY=0
DEM=""
BUILD=1
ROADS=0
BIND="${NETWEEVIL_BIND:-127.0.0.1:8080}"
STATE="$ROOT/.netweevil/hong-kong"
usage() {
  cat <<'EOF'
Usage: scripts/setup_hong_kong.sh [options]
  --analysis-repo DIR  Repository containing datasets/3D_* and mtr_platform_join
  --prepared-dir DIR   Already-reprojected outdoor and enriched indoor GeoPackages
  --service-start DATE Feed service start, YYYY-MM-DD (default: today in local time)
  --service-days N     Service horizon (default: 7)
  --no-serve           Prepare/import/audit without starting the API
  --serve-only         Start the already-prepared example without rebuilding
  --profiles-only      Rebuild profiles and transfer tables using existing imports
  --terrain-only       Prepare local DEM tiles without reimporting routing data
  --dem FILE           DEM ASCII raster (default: analysis datasets/dtm/Whole_HK_DTM_5m.asc)
  --roads              Start the OSM road view instead of pedestrian/transit view
  --skip-build         Use the existing CLI and frontend build
  --bind HOST:PORT     Listen address (default: 127.0.0.1:8080)

Routing data is stored under .netweevil/hong-kong; DEM tiles under .netweevil/terrain.
See docs/hong-kong-tutorial.md for source downloads and data limitations.
EOF
}
while (($#)); do
  case "$1" in
    --analysis-repo|--prepared-dir|--service-start|--service-days|--bind|--dem)
      (($# >= 2)) || { echo "Missing value for $1" >&2; exit 2; }
      case "$1" in
        --analysis-repo) ANALYSIS="$2";;
        --prepared-dir) PREPARED="$2";;
        --service-start) SERVICE_START="$2";;
        --service-days) SERVICE_DAYS="$2";;
        --bind) BIND="$2";;
        --dem) DEM="$2";;
      esac
      shift 2;;
    --no-serve) SERVE=0; shift;;
    --serve-only) SERVE_ONLY=1; BUILD=0; shift;;
    --profiles-only) PROFILES_ONLY=1; shift;;
    --terrain-only) TERRAIN_ONLY=1; shift;;
    --skip-build) BUILD=0; shift;;
    --roads) ROADS=1; shift;;
    -h|--help) usage; exit 0;;
    *) echo "Unknown option: $1" >&2; usage >&2; exit 2;;
  esac
done
if ((TERRAIN_ONLY && (SERVE_ONLY || PROFILES_ONLY))); then
  echo '--terrain-only cannot be combined with --serve-only or --profiles-only' >&2
  exit 2
fi
prepare_terrain() {
  python3 scripts/prepare_hong_kong_terrain.py --input "$1" --output-dir "$ROOT/.netweevil/terrain/hong_kong_5m"
}
if ((TERRAIN_ONLY)); then
  if [[ -z "$DEM" ]]; then
    for candidate in "${ANALYSIS:-$ROOT/../analyses/hong-kong-analysis}" "$ROOT/../hong-kong-analysis"; do
      if [[ -f "$candidate/datasets/dtm/Whole_HK_DTM_5m.asc" ]]; then DEM="$candidate/datasets/dtm/Whole_HK_DTM_5m.asc"; break; fi
    done
  fi
  [[ -f "$DEM" ]] || { echo 'Supply --dem /path/Whole_HK_DTM_5m.asc; download instructions are in docs/terrain-viewer.md.' >&2; exit 1; }
  prepare_terrain "$DEM"
fi
BIN="$ROOT/target/release/netweevil"
if ((BUILD)); then
  (cd frontend && pnpm install --frozen-lockfile && pnpm build)
  cargo build --release -p netweevil-cli
fi
[[ -x "$BIN" ]] || { echo 'Build the CLI first: cargo build --release -p netweevil-cli' >&2; exit 1; }
if ((!SERVE_ONLY && !PROFILES_ONLY && !TERRAIN_ONLY)); then
  if [[ -z "$ANALYSIS" ]]; then
    for candidate in "$ROOT/../analyses/hong-kong-analysis" "$ROOT/../hong-kong-analysis"; do
      if [[ -f "$candidate/datasets/3D_Pedestrian_Network.gpkg" ]]; then ANALYSIS="$candidate"; break; fi
    done
  fi
  [[ -n "$ANALYSIS" ]] || { echo 'Supply --analysis-repo; see docs/hong-kong-tutorial.md for the full spatial exports.' >&2; exit 1; }
  ANALYSIS="$(cd "$ANALYSIS" && pwd)"
  terrain_input="${DEM:-$ANALYSIS/datasets/dtm/Whole_HK_DTM_5m.asc}"
  if [[ -f "$terrain_input" ]]; then prepare_terrain "$terrain_input"; fi
  mkdir -p "$STATE/prepared" "$STATE/verification"
  if [[ -n "$PREPARED" ]]; then
    PREPARED="$(cd "$PREPARED" && pwd)"
    for file in 3D_Pedestrian_Network.gpkg 3D_Indoor_Network.gpkg; do
      input="$PREPARED/$file"
      if [[ "$file" == 3D_Indoor_Network.gpkg && -f "$PREPARED/3D_Indoor_Network_MTR_Platforms.gpkg" ]]; then
        input="$PREPARED/3D_Indoor_Network_MTR_Platforms.gpkg"
      fi
      [[ "$(dirname "$input")" == "$STATE/prepared" && "$(basename "$input")" == "$file" ]] || cp "$input" "$STATE/prepared/$file"
    done
  else
    scripts/prepare_hong_kong_pedestrian.sh --force --output-dir "$STATE/prepared" "$ANALYSIS"
  fi
  python3 scripts/download_hong_kong_sources.py --output-dir "$STATE/raw" --only surface-gtfs rail-gtfs platforms
  python3 scripts/connect_hong_kong_precision_gaps.py --prepared-dir "$STATE/prepared"
  python3 scripts/audit_hong_kong_network.py --prepared-dir "$STATE/prepared" --catalogue "$STATE/raw/mtr_data_complete.json" --output "$STATE/verification/network-connectivity.json"
  python3 scripts/build_hong_kong_transit.py \
    --surface-gtfs "$STATE/raw/hk-surface.gtfs.zip" --rail-gtfs "$STATE/raw/hk-community.gtfs.zip" \
    --platform-join-dir "$ANALYSIS/datasets/mtr_platform_join" \
    --platform-catalog "$STATE/raw/mtr_data_complete.json" --output-dir "$STATE/transit"
  python3 scripts/export_hong_kong_station_surfaces.py \
    --platform-join-dir "$ANALYSIS/datasets/mtr_platform_join" \
    --platform-catalog "$STATE/raw/mtr_data_complete.json" \
    --output "$STATE/transit/mtr_platform_surfaces.geojson" \
    --gtfs "$STATE/transit/hong_kong_multimodal.gtfs.zip"
  sources=("$STATE/prepared/3D_Pedestrian_Network.gpkg" "$STATE/prepared/3D_Indoor_Network.gpkg" "$STATE/prepared/Station_Precision_Connectors.gpkg")
  "$BIN" dataset import "${sources[@]}" --name hk_pedestrian_3d --format gpkg --mapping examples/ingest/hong_kong_connected_mapping.yml
  "$BIN" dataset audit --dataset hk_pedestrian_3d --against "${sources[@]}" --mapping examples/ingest/hong_kong_connected_mapping.yml --json > "$STATE/verification/source-retention.json"
  ROAD_SOURCE="$ROOT/datasets/hong-kong-260508.osm.pbf"
  if [[ ! -f "$ROAD_SOURCE" ]]; then
    python3 scripts/download_hong_kong_sources.py --output-dir "$STATE/raw" --only roads
    ROAD_SOURCE="$STATE/raw/hong-kong-latest.osm.pbf"
  fi
  "$BIN" dataset import "$ROAD_SOURCE" --name hk_roads
  "$BIN" transit import "$STATE/transit/hong_kong_multimodal.gtfs.zip" --name hong_kong_multimodal \
    --service-start "$SERVICE_START" --service-days "$SERVICE_DAYS" --stop-bindings "$STATE/transit/stop_bindings.json"
fi
if (((!SERVE_ONLY || PROFILES_ONLY) && !TERRAIN_ONLY)); then
  for profile in pedestrian_fastest_multilayer pedestrian_step_free_fastest_multilayer pedestrian_multilayer pedestrian_step_free_multilayer; do
    "$BIN" profile compile --dataset hk_pedestrian_3d --profile "examples/profiles/$profile.yml"
    "$BIN" transit transfers build --feed hong_kong_multimodal --dataset hk_pedestrian_3d \
      --profile "examples/profiles/$profile.yml" --max-transfer-distance-m 500 --max-candidates-per-stop 256
  done
  "$BIN" profile compile --dataset hk_roads --profile examples/profiles/car_research_v3.yml
  echo "Hong Kong profiles and transfer tables ready. Reports: $STATE/verification"
fi
if ((SERVE)); then
  echo "Open http://$BIND/ in your browser. Data stays in $STATE"
  if ((ROADS)); then
    exec "$BIN" api serve --bind "$BIND" --dataset hk_roads \
      --default-profile examples/profiles/car_research_v3.yml --console-dir frontend/dist
  fi
  exec "$BIN" api serve --bind "$BIND" --dataset hk_pedestrian_3d \
    --default-profile examples/profiles/pedestrian_fastest_multilayer.yml \
    --profile examples/profiles/pedestrian_step_free_fastest_multilayer.yml \
    --profile examples/profiles/pedestrian_multilayer.yml \
    --profile examples/profiles/pedestrian_step_free_multilayer.yml \
    --transit-feed hong_kong_multimodal --console-dir frontend/dist
fi
