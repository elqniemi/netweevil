# netan

`netan` is a Rust-first local network analysis tool for OSM-based routing research.

This repository now includes:

- a shared Rust routing core
- a CLI for reproducible local runs
- a functional native `egui` desktop shell
- an in-repo QGIS plugin for QGIS 3 and QGIS 4 that drives the CLI and loads spatial outputs
- explicit manifests and bundles under `.netan/`
- local exports to `json`, `csv`, `geojson`, `gpkg`, `parquet`, and `geoparquet`

Via-way turn restriction handling is present. Turn penalties, acceleration structures, and broader packaging are still pending. See [`PROGRESS.md`](/home/elmeriniemi/stuff/netan/PROGRESS.md).

## Workspace Layout

- [`crates/cli`](/home/elmeriniemi/stuff/netan/crates/cli): `netan` CLI entry point
- [`crates/gui`](/home/elmeriniemi/stuff/netan/crates/gui): native desktop shell
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

## Native GUI

Launch the desktop app:

```bash
cargo run -p netan-cli -- gui
```

Recommended GUI flow:

1. In `Datasets`, import `datasets/groningen-260317.osm.pbf` as `groningen_2026_03`.
2. In `Profiles`, validate `examples/profiles/car_research_v1.yml` and compile it for the selected dataset.
3. In `Analyses`, use the example request files or write a new route request from typed coordinates.
4. Set an output like `.netan/runs/gui-route.geojson` or `.netan/runs/gui-matrix.gpkg`.
5. Execute the analysis and inspect the result from `Runs`.

Notes:

- GUI paths can be repository-relative or absolute.
- The GUI auto-compiles the selected profile on demand if the needed metric bundle is missing.
- Spatial outputs are still written by the same report/export layer as the CLI.

## QGIS Plugin

The QGIS plugin lives under [`qgis_plugin/netan_qgis`](/home/elmeriniemi/stuff/netan/qgis_plugin/netan_qgis). Full setup instructions are in [`qgis_plugin/README.md`](/home/elmeriniemi/stuff/netan/qgis_plugin/README.md).

Minimal install flow:

1. Build the CLI binary:

```bash
cargo build -p netan-cli
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

3. In QGIS, set either native or WSL mode.

Native mode:

- `Execution mode`: `Native`
- `QGIS-visible workspace root`: `/home/elmeriniemi/stuff/netan`
- `Native netan executable`: `/home/elmeriniemi/stuff/netan/target/debug/netan`
- `Profile`: `examples/profiles/car_research_v1.yml`

Windows QGIS with WSL-built `netan`:

- `Execution mode`: `WSL`
- `QGIS-visible workspace root`: `\\wsl$\Ubuntu\home\elmeriniemi\stuff\netan`
- `WSL distro`: `Ubuntu`
- `WSL workspace root`: `/home/elmeriniemi/stuff/netan`
- `WSL netan executable`: `/home/elmeriniemi/stuff/netan/target/debug/netan`
- `Profile`: `examples/profiles/car_research_v1.yml`

4. Press `Refresh`, choose dataset `groningen_2026_03`, then use:

- `Run Route` with output `.netan/runs/qgis-route.geojson`
- `Run OD` with output `.netan/runs/qgis-od.gpkg`
- `Run Matrix` with output `.netan/runs/qgis-matrix.gpkg`

The plugin auto-loads spatial outputs into the current QGIS project after successful runs, including when Windows QGIS is talking to a WSL-hosted workspace through `\\wsl$`.

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

5. In the plugin dock, use these settings for WSL-backed execution:

- `Execution mode`: `WSL`
- `QGIS-visible workspace root`: `\\wsl$\Ubuntu\home\elmeriniemi\stuff\netan`
- `WSL distro`: `Ubuntu`
- `WSL workspace root`: `/home/elmeriniemi/stuff/netan`
- `WSL netan executable`: `/home/elmeriniemi/stuff/netan/target/debug/netan`
- `Profile`: `examples/profiles/car_research_v1.yml`

6. Press `Refresh`, choose dataset `groningen_2026_03`, then run:

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

2. Open Windows PowerShell, then build the CLI from the Windows copy:

```powershell
cd C:\path\to\netan
cargo build -p netan-cli
```

3. Install the plugin into QGIS 4:

```powershell
New-Item -ItemType Directory -Force "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins"
cmd /c mklink /D "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins\netan_qgis" "C:\path\to\netan\qgis_plugin\netan_qgis"
```

4. In QGIS 4, use:

- `Execution mode`: `Native`
- `QGIS-visible workspace root`: `C:\path\to\netan`
- `Native netan executable`: `C:\path\to\netan\target\debug\netan.exe`
- `Profile`: `examples/profiles/car_research_v1.yml`

5. Import and run from PowerShell or from the native GUI/CLI:

```powershell
cd C:\path\to\netan
.\target\debug\netan.exe dataset import datasets\groningen-260317.osm.pbf --name groningen_2026_03
.\target\debug\netan.exe profile compile --dataset groningen_2026_03 --profile examples\profiles\car_research_v1.yml
.\target\debug\netan.exe analyze route --dataset groningen_2026_03 --profile examples\profiles\car_research_v1.yml --request examples\requests\route.json --out .netan\runs\example-route.geojson
```

6. In the plugin, press `Refresh` and run analyses normally.

### Which Windows setup should you use?

- Use `WSL` mode if your active repo, datasets, and build artifacts live in WSL.
- Use `Native` mode if you want a pure Windows setup and are willing to build `netan.exe` from the Windows copy.

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
