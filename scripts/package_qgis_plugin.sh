#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
plugin_dir="$repo_root/qgis_plugin/netweevil_qgis"
metadata_path="$plugin_dir/metadata.txt"

if [[ ! -f "$metadata_path" ]]; then
  echo "missing plugin metadata: $metadata_path" >&2
  exit 1
fi

version="$(awk -F= '/^version=/{print $2; exit}' "$metadata_path" | tr -d '[:space:]')"
if [[ -z "$version" ]]; then
  echo "missing version=... in $metadata_path" >&2
  exit 1
fi

mkdir -p "$repo_root/dist"
archive="$repo_root/dist/netweevil_qgis-$version.zip"

python3 - "$repo_root" "$archive" <<'PY'
import os
import sys
import zipfile
from pathlib import Path

repo_root = Path(sys.argv[1])
archive = Path(sys.argv[2])
plugin_root = repo_root / "qgis_plugin" / "netweevil_qgis"

excluded_dirs = {"__pycache__"}
excluded_suffixes = {".pyc", ".pyo"}
excluded_names = {".DS_Store"}

with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as zf:
    for path in sorted(plugin_root.rglob("*")):
        relative = path.relative_to(plugin_root.parent)
        parts = set(relative.parts)
        if parts & excluded_dirs:
            continue
        if path.name in excluded_names or path.suffix in excluded_suffixes:
            continue
        if path.is_dir():
            continue
        zf.write(path, relative.as_posix())

print(archive)
PY
