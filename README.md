# netan

`netan` is a Rust-first local network analysis tool for OSM-based routing research.

This repository now includes:

- a shared Rust routing core
- a CLI for reproducible local runs
- a preloadable HTTP API for interactive execution
- an in-repo QGIS plugin for QGIS 3 and QGIS 4 that talks to the API and loads spatial outputs
- explicit manifests and bundles under `.netan/`
- local exports to `json`, `csv`, `geojson`, `gpkg`, `parquet`, and `geoparquet`

Via-way turn restriction handling is present. Turn penalties, acceleration structures, and broader packaging are still pending. See [`PROGRESS.md`](/home/elmeriniemi/stuff/netan/PROGRESS.md).

## Workspace Layout

- [`crates/cli`](/home/elmeriniemi/stuff/netan/crates/cli): `netan` CLI entry point
- [`crates/api`](/home/elmeriniemi/stuff/netan/crates/api): preloadable HTTP API
- [`examples/profiles`](/home/elmeriniemi/stuff/netan/examples/profiles): reusable example profiles
- [`examples/requests`](/home/elmeriniemi/stuff/netan/examples/requests): route, OD, and matrix inputs
- [`qgis_plugin/netan_qgis`](/home/elmeriniemi/stuff/netan/qgis_plugin/netan_qgis): QGIS plugin package

## Build

```bash
cargo fmt --all
cargo check
cargo test
```

## CLI Quickstart

Import a dataset:

```bash
cargo run -p netan-cli -- dataset import datasets/groningen-260317.osm.pbf --name groningen_2026_03
```

Validate and compile a profile:

```bash
cargo run -p netan-cli -- profile validate examples/profiles/car_research_v1.yml
cargo run -p netan-cli -- profile compile --dataset groningen_2026_03 --profile examples/profiles/car_research_v1.yml
```

Run a route and write a spatial result QGIS can open directly:

```bash
cargo run -p netan-cli -- analyze route \
  --dataset groningen_2026_03 \
  --profile examples/profiles/car_research_v1.yml \
  --request examples/requests/route.json \
  --out .netan/runs/example-route.geojson
```

Run OD and matrix examples:

```bash
cargo run -p netan-cli -- analyze od \
  --dataset groningen_2026_03 \
  --profile examples/profiles/car_research_v1.yml \
  --pairs examples/requests/od_pairs.csv \
  --out .netan/runs/example-od.gpkg

cargo run -p netan-cli -- analyze matrix \
  --dataset groningen_2026_03 \
  --profile examples/profiles/car_research_v1.yml \
  --origins examples/requests/matrix_origins.csv \
  --destinations examples/requests/matrix_destinations.csv \
  --out .netan/runs/example-matrix.gpkg
```

Inspect persistent state:

```bash
cargo run -p netan-cli -- cache list
```

## Current Dataset Format

The current fast path expects freshly imported datasets in the new bundle layout:

- `.netan/bundles/topology/*.bin`
- `.netan/bundles/names/*.bin`
- `.netan/bundles/acceleration/*.bin`
- `.netan/bundles/metrics/*.bin`

Older gzip topology bundles and older dataset imports are no longer part of the supported execution path. Rebuild cached datasets before using the API or running new analyses:

```bash
rm -rf .netan/datasets .netan/bundles/topology .netan/bundles/names .netan/bundles/acceleration .netan/bundles/metrics .netan/compiled_profiles
mkdir -p .netan/bundles/topology .netan/bundles/names .netan/bundles/acceleration .netan/bundles/metrics .netan/datasets .netan/compiled_profiles

cargo run -p netan-cli -- dataset import datasets/groningen-260317.osm.pbf --name groningen_2026_03
cargo run -p netan-cli -- profile compile --dataset groningen_2026_03 --profile examples/profiles/car_research_v1.yml
```

## Fast API Flow

This is now the recommended interactive workflow, including QGIS use.

For the lowest warm-query latency on the current exact engine:

- import the dataset again with the current format
- compile every profile you want to use before startup
- start the API with `--default-profile` plus extra `--profile` flags to preload profiles
- use JSON responses, not `format=geojson`
- keep route requests summary-only unless you explicitly need geometry or segment rows

Example:

```bash
cargo run -p netan-cli -- api serve \
  --dataset groningen_2026_03 \
  --default-profile examples/profiles/car_research_v1.yml \
  --profile examples/profiles/pedestrian_research_v1.yml \
  --bind 127.0.0.1:8080
```

Fast summary-only route request:

```bash
curl -X POST http://127.0.0.1:8080/v1/route \
  -H 'content-type: application/json' \
  --data '{
    "request": {
      "route_id": "fast_route_001",
      "origin": { "id": "a", "lon": 6.5665, "lat": 53.2194 },
      "destination": { "id": "b", "lon": 6.5716, "lat": 53.2148 },
      "returns": {
        "geometry": "none",
        "segment_rows": false,
        "road_type_breakdown": [],
        "surface_breakdown": [],
        "penalty_breakdown": false,
        "explain_cost_derivation": false
      }
    }
  }'
```

That path keeps edge names cold, avoids geometry materialization, and uses the prepared in-memory routing engine plus scratch reuse.
Pure summary-only route responses now also omit `node_path` and `edge_path`, so the API does not serialize full path ID arrays unless you request richer route detail.

## QGIS Plugin

The QGIS plugin lives under [`qgis_plugin/netan_qgis`](/home/elmeriniemi/stuff/netan/qgis_plugin/netan_qgis). Full setup instructions are in [`qgis_plugin/README.md`](/home/elmeriniemi/stuff/netan/qgis_plugin/README.md).

Minimal install flow:

1. Start the API:

```bash
cargo run -p netan-cli -- api serve \
  --dataset groningen_2026_03 \
  --default-profile examples/profiles/car_research_v1.yml \
  --profile examples/profiles/pedestrian_research_v1.yml \
  --bind 127.0.0.1:8080
```

2. Symlink the plugin into your QGIS plugin directory.

Linux QGIS example:

```bash
mkdir -p ~/.local/share/QGIS/QGIS3/profiles/default/python/plugins
ln -s /home/elmeriniemi/stuff/netan/qgis_plugin/netan_qgis ~/.local/share/QGIS/QGIS3/profiles/default/python/plugins/netan_qgis
```

Windows QGIS 4 example for a repo living in WSL:

```powershell
New-Item -ItemType Directory -Force "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins"
cmd /c mklink /D "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins\netan_qgis" "\\wsl$\Ubuntu\home\elmeriniemi\stuff\netan\qgis_plugin\netan_qgis"
```

3. In QGIS, point the plugin at the API.

- `Workspace root`: `/home/elmeriniemi/stuff/netan` on Linux, or any Windows-visible folder you want to use for request and response files
- `API base URL`: `http://127.0.0.1:8080`
- `Response format`: `JSON` or `GeoJSON`
- `Profile`: the service default or any profile preloaded by the API

4. Press `Refresh Service`, then use:

- `Run Route` with output `.netan/runs/qgis-route.geojson`
- `Run OD` with output `.netan/runs/qgis-od.gpkg`
- `Run Matrix` with output `.netan/runs/qgis-matrix.gpkg`

The plugin auto-loads spatial outputs into the current QGIS project after successful runs.

## Full Windows Example

This section assumes:

- your source repo lives in WSL at `/home/elmeriniemi/stuff/netan`
- you have a Windows-visible mount or symlink at `/windownloads`
- you want to use Windows QGIS 4

### Option A: Use Windows QGIS 4 with the WSL build

This is the simplest path if you are already developing in WSL.

1. Build `netan` in WSL:

```bash
cd /home/elmeriniemi/stuff/netan
cargo build -p netan-cli
```

2. Make sure the workspace has the data and outputs you want available to Windows QGIS.

If you want to copy the repo to Windows but skip Rust build output:

```bash
cd /home/elmeriniemi/stuff/netan
mkdir -p /windownloads/netan
for p in .* *; do
  [ "$p" = "." ] || [ "$p" = ".." ] || [ "$p" = "target" ] || cp -a -- "$p" /windownloads/netan/
done
```

That keeps original data such as `datasets/`, `examples/`, and `qgis_plugin/`.

3. Add the QGIS 4 plugin from Windows PowerShell.

If you want QGIS to use the plugin directly from the WSL repo:

```powershell
New-Item -ItemType Directory -Force "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins"
cmd /c mklink /D "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins\netan_qgis" "\\wsl$\Ubuntu\home\elmeriniemi\stuff\netan\qgis_plugin\netan_qgis"
```

If you copied the repo to Windows first, point the symlink at the Windows copy instead:

```powershell
New-Item -ItemType Directory -Force "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins"
cmd /c mklink /D "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins\netan_qgis" "C:\path\to\netan\qgis_plugin\netan_qgis"
```

4. Start QGIS 4 and enable the plugin from `Plugins -> Manage and Install Plugins`.

5. Start the API in WSL:

```bash
cd /home/elmeriniemi/stuff/netan
cargo run -p netan-cli -- api serve \
  --dataset groningen_2026_03 \
  --default-profile examples/profiles/car_research_v1.yml \
  --bind 127.0.0.1:8080
```

6. In the plugin dock, use:

- `Workspace root`: `\\wsl$\Ubuntu\home\elmeriniemi\stuff\netan`
- `API base URL`: `http://127.0.0.1:8080`
- `Profile`: `examples/profiles/car_research_v1.yml`

7. Press `Refresh Service`, then run:

- `Route` with output `.netan/runs/qgis-route.geojson`
- `OD` with output `.netan/runs/qgis-od.gpkg`
- `Matrix` with output `.netan/runs/qgis-matrix.gpkg`

### Option B: Use a native Windows build

Use this if you want QGIS to run `netan.exe` directly without `wsl.exe`.

1. Copy the repo to Windows.

From WSL:

```bash
cd /home/elmeriniemi/stuff/netan
mkdir -p /windownloads/netan
for p in .* *; do
  [ "$p" = "." ] || [ "$p" = ".." ] || [ "$p" = "target" ] || cp -a -- "$p" /windownloads/netan/
done
```

2. Open Windows PowerShell, then start the API from the Windows copy:

```powershell
cd C:\path\to\netan
cargo run -p netan-cli -- api serve --dataset groningen_2026_03 --default-profile examples/profiles/car_research_v1.yml --bind 127.0.0.1:8080
```

3. Install the plugin into QGIS 4:

```powershell
New-Item -ItemType Directory -Force "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins"
cmd /c mklink /D "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins\netan_qgis" "C:\path\to\netan\qgis_plugin\netan_qgis"
```

4. In QGIS 4, use:

- `Workspace root`: `C:\path\to\netan`
- `API base URL`: `http://127.0.0.1:8080`
- `Profile`: `examples/profiles/car_research_v1.yml`

5. Import and run from PowerShell, then use the plugin against that API:

```powershell
cd C:\path\to\netan
.\target\debug\netan.exe dataset import datasets\groningen-260317.osm.pbf --name groningen_2026_03
.\target\debug\netan.exe profile compile --dataset groningen_2026_03 --profile examples\profiles\car_research_v1.yml
```

6. In the plugin, press `Refresh Service` and run analyses normally.

### Which Windows setup should you use?

- Run the API in WSL if your active repo, datasets, and build artifacts live in WSL.
- Run the API natively on Windows if you want a pure Windows setup.

## Input Examples

Route request JSON:

```json
{
  "route_id": "route_demo_001",
  "origin": { "id": "origin_a", "lon": 6.5665, "lat": 53.2194 },
  "destination": { "id": "destination_b", "lon": 6.5716, "lat": 53.2148 },
  "snap": { "max_distance_m": 500.0 },
  "returns": { "geometry": "full", "segment_rows": true }
}
```

OD CSV:

```csv
id,source_x,source_y,target_x,target_y
od_demo_001,6.5665,53.2194,6.5716,53.2148
od_demo_002,6.5636,53.2181,6.5698,53.2137
```

Matrix point-set CSV:

```csv
id,x,y
origin_a,6.5665,53.2194
origin_c,6.5636,53.2181
```
