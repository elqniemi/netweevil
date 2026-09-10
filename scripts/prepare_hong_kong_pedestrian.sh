#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/prepare_hong_kong_pedestrian.sh [OPTIONS] HONG_KONG_ANALYSIS_REPO

Reproject the Hong Kong outdoor and indoor 3D pedestrian GeoPackages from
EPSG:2326 horizontally to WGS84 longitude/latitude while preserving the third
coordinate as HKPD metres. Output basenames, layer names, feature ids, fields,
and 3D geometry are retained for NetWeevil's mapping-driven importer. When the
MTR platform join exists under datasets/mtr_platform_join, its enriched indoor
network is selected automatically.

Options:
  --output-dir DIR  Destination directory (default: REPO/prepared)
  --force           Atomically replace existing prepared GeoPackages
  --check-only      Validate GDAL and the source files without converting
  -h, --help        Show this help
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command '$1' was not found"
}

layer_info() {
  ogrinfo -ro -so "$1" "$2"
}

feature_count() {
  layer_info "$1" "$2" | awk -F: '
    /^Feature Count:/ {gsub(/[[:space:]]/, "", $2); count=$2}
    END {print count}
  '
}

field_schema() {
  layer_info "$1" "$2" | awk '
    /^Geometry Column =/ {in_fields=1; next}
    in_fields && /^[[:alnum:]_]+:/ {print}
  '
}

sample_z() {
  local dataset="$1"
  local layer="$2"
  ogrinfo -ro -q "$dataset" -dialect SQLite \
    -sql "SELECT ST_Z(ST_PointN(ST_GeometryN(\"Shape\",1),1)) AS sample_z FROM \"$layer\" WHERE \"Shape\" IS NOT NULL LIMIT 1" \
    | awk -F'= ' '/sample_z \(Real\)/ {sample=$2} END {print sample}'
}

validate_source() {
  local source="$1"
  local layer="$2"
  [[ -f "$source" ]] || die "missing source GeoPackage: $source"
  local info
  info="$(layer_info "$source" "$layer")" || die "source '$source' is missing layer '$layer'"
  grep -q '^Geometry: 3D ' <<<"$info" || die "layer '$layer' in '$source' is not 3D"
  grep -q 'ID\["EPSG",2326\]' <<<"$info" || die "layer '$layer' in '$source' is not horizontally referenced to EPSG:2326"
  grep -q 'ID\["EPSG",5738\]' <<<"$info" || die "layer '$layer' in '$source' does not declare HKPD height (EPSG:5738)"
  [[ "$(feature_count "$source" "$layer")" =~ ^[1-9][0-9]*$ ]] \
    || die "layer '$layer' in '$source' contains no features"
  [[ -n "$(sample_z "$source" "$layer")" ]] \
    || die "layer '$layer' in '$source' has no readable Z coordinate"
}

validate_platform_source() {
  local source="$1"
  local info
  info="$(layer_info "$source" indoor_pedestrian_route)"
  for field in \
    MTRVenueID MTRStationCode MTRStationID MTRLineCodes \
    MTRPlatformLevel MTRPlatformLineCodes MTRPlatformSourceIDs MTRPlatformStopIDs; do
    grep -q "^${field}:" <<<"$info" \
      || die "platform-enriched indoor network is missing required field '$field': $source"
  done
}

validate_output() {
  local source="$1"
  local output="$2"
  local layer="$3"
  local info
  info="$(layer_info "$output" "$layer")" || die "prepared output is missing layer '$layer'"
  grep -q '^Geometry: 3D ' <<<"$info" || die "prepared layer '$layer' is not 3D"
  grep -q '^Geometry Column = Shape$' <<<"$info" || die "prepared layer '$layer' did not preserve geometry column 'Shape'"

  local source_count output_count
  source_count="$(feature_count "$source" "$layer")"
  output_count="$(feature_count "$output" "$layer")"
  [[ "$source_count" == "$output_count" ]] \
    || die "feature count changed for '$layer': $source_count -> $output_count"

  local source_fields output_fields
  source_fields="$(field_schema "$source" "$layer")"
  output_fields="$(field_schema "$output" "$layer")"
  [[ "$source_fields" == "$output_fields" ]] \
    || die "field names, types, or domains changed while preparing '$layer'"

  local source_z output_z
  source_z="$(sample_z "$source" "$layer")"
  output_z="$(sample_z "$output" "$layer")"
  awk -v source_z="$source_z" -v output_z="$output_z" 'BEGIN {
    difference = source_z - output_z;
    if (difference < 0) difference = -difference;
    exit !(difference <= 1e-9)
  }' || die "sample HKPD Z changed for '$layer': $source_z -> $output_z"

  local extent
  extent="$(sed -n 's/^Extent: (\([^,]*\), \([^)]*\)) - (\([^,]*\), \([^)]*\)).*/\1 \2 \3 \4/p' <<<"$info")"
  [[ -n "$extent" ]] || die "could not read prepared extent for '$layer'"
  awk -v extent="$extent" 'BEGIN {
    split(extent, values, " ");
    exit !(values[1] >= -180 && values[3] <= 180 && values[2] >= -90 && values[4] <= 90)
  }' || die "prepared layer '$layer' is outside WGS84 longitude/latitude ranges: $extent"
}

output_dir=""
force=0
check_only=0
analysis_repo=""

while (($#)); do
  case "$1" in
    --output-dir)
      (($# >= 2)) || die "--output-dir requires a directory"
      output_dir="$2"
      shift 2
      ;;
    --force)
      force=1
      shift
      ;;
    --check-only)
      check_only=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --*)
      die "unknown option: $1"
      ;;
    *)
      [[ -z "$analysis_repo" ]] || die "only one Hong Kong analysis repository may be supplied"
      analysis_repo="$1"
      shift
      ;;
  esac
done

[[ -n "$analysis_repo" ]] || { usage >&2; exit 2; }
analysis_repo="$(cd "$analysis_repo" 2>/dev/null && pwd)" \
  || die "Hong Kong analysis repository does not exist: $analysis_repo"
source_dir="$analysis_repo/datasets"
output_dir="${output_dir:-$analysis_repo/prepared}"

require_command ogr2ogr
require_command ogrinfo
require_command awk
require_command sha256sum

outdoor_source="$source_dir/3D_Pedestrian_Network.gpkg"
indoor_source="$source_dir/3D_Indoor_Network.gpkg"
platform_indoor_source=""
for candidate in \
  "$source_dir/mtr_platform_join/3D_Indoor_Network_MTR_Platforms.gpkg" \
  "$source_dir/3D_Indoor_Network_MTR_Platforms.gpkg"; do
  if [[ -f "$candidate" ]]; then
    platform_indoor_source="$candidate"
    break
  fi
done
if [[ -n "$platform_indoor_source" ]]; then
  indoor_source="$platform_indoor_source"
elif [[ -f "$source_dir/mtr_platform_join/qa_report.json" ]]; then
  die "MTR platform join metadata exists, but 3D_Indoor_Network_MTR_Platforms.gpkg was not found beside it or in datasets/"
fi
validate_source "$outdoor_source" pedestrian_route
validate_source "$indoor_source" indoor_pedestrian_route
if [[ -n "$platform_indoor_source" ]]; then
  validate_platform_source "$indoor_source"
fi

echo "GDAL: $(ogr2ogr --version)"
echo "Validated source: $outdoor_source"
echo "Validated source: $indoor_source"
if ((check_only)); then
  echo "Source validation complete; no files written (--check-only)."
  exit 0
fi

mkdir -p "$output_dir"
temporary_dir="$(mktemp -d "$output_dir/.netweevil-hk-prepare.XXXXXX")"
cleanup() {
  rm -rf "$temporary_dir"
}
trap cleanup EXIT

prepare_one() {
  local source="$1"
  local layer="$2"
  local output_basename="${3:-$(basename "$source")}"
  local basename temporary_output final_output
  basename="$(basename "$source")"
  temporary_output="$temporary_dir/$output_basename"
  final_output="$output_dir/$output_basename"
  if [[ -e "$final_output" && "$force" -ne 1 ]]; then
    die "prepared output already exists: $final_output (use --force to replace it)"
  fi

  echo "Preparing $basename ($layer)..."
  ogr2ogr \
    --config OGR_CT_FORCE_TRADITIONAL_GIS_ORDER YES \
    -f GPKG \
    "$temporary_output" \
    "$source" \
    "$layer" \
    -nln "$layer" \
    -s_srs EPSG:2326 \
    -t_srs OGC:CRS84 \
    -dim XYZ \
    -nlt PROMOTE_TO_MULTI \
    -preserve_fid \
    -lco FID=OBJECTID \
    -lco GEOMETRY_NAME=Shape \
    -lco SPATIAL_INDEX=YES
  validate_output "$source" "$temporary_output" "$layer"
  mv -f "$temporary_output" "$final_output"
  echo "Prepared and validated: $final_output"
}

prepare_one "$outdoor_source" pedestrian_route
prepare_one "$indoor_source" indoor_pedestrian_route 3D_Indoor_Network.gpkg

manifest="$temporary_dir/preprocessing-manifest.txt"
{
  echo "netweevil_hong_kong_preprocessing=1"
  echo "created_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "gdal=$(ogr2ogr --version)"
  echo "horizontal_transform=EPSG:2326_to_OGC:CRS84"
  echo "vertical_transform=none; third coordinate preserved as EPSG:5738 HKPD metres"
  echo "outdoor_source=$outdoor_source"
  echo "outdoor_sha256=$(sha256sum "$outdoor_source" | awk '{print $1}')"
  echo "indoor_source=$indoor_source"
  echo "indoor_sha256=$(sha256sum "$indoor_source" | awk '{print $1}')"
} >"$manifest"
mv -f "$manifest" "$output_dir/netweevil-preprocessing-manifest.txt"

echo "Hong Kong pedestrian preprocessing complete: $output_dir"
