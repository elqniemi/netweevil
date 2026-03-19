# netan QGIS Plugin

This plugin wraps the local `netan` CLI from inside QGIS.

It is meant for the current repository layout:

- one local workspace root
- one `.netan/` state directory
- one `netan` executable built from this repo

The plugin is now written to be usable in both QGIS 3 and QGIS 4, and it supports two execution modes:

- `Native`: QGIS and `netan` run on the same OS
- `WSL`: Windows QGIS launches the Linux `netan` binary through `wsl.exe`

## What It Does

- refreshes dataset manifests from `.netan/datasets/`
- validates and compiles profiles against the selected dataset
- writes route request JSON from typed coordinates
- runs `route`, `od`, and `matrix` analyses through the CLI
- loads `.geojson`, `.gpkg`, and `.geoparquet` outputs into QGIS after a successful run

## Install In QGIS

1. Build the CLI:

```bash
cargo build -p netan-cli
```

2. Copy or symlink [`netan_qgis`](/home/elmeriniemi/stuff/netan/qgis_plugin/netan_qgis) into your QGIS profile plugin directory.

Linux example:

```bash
mkdir -p ~/.local/share/QGIS/QGIS3/profiles/default/python/plugins
ln -s /home/elmeriniemi/stuff/netan/qgis_plugin/netan_qgis ~/.local/share/QGIS/QGIS3/profiles/default/python/plugins/netan_qgis
```

Windows QGIS 4 example:

```powershell
New-Item -ItemType Directory -Force "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins"
cmd /c mklink /D "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins\netan_qgis" "\\wsl$\Ubuntu\home\elmeriniemi\stuff\netan\qgis_plugin\netan_qgis"
```

3. In QGIS, enable the plugin from `Plugins -> Manage and Install Plugins`.

## First Run

Set these fields in the dock:

- `Execution mode`: `Native` or `WSL`
- `QGIS-visible workspace root`:
  - Linux native example: `/home/elmeriniemi/stuff/netan`
  - Windows + WSL example: `\\wsl$\Ubuntu\home\elmeriniemi\stuff\netan`
- `Native netan executable` for native mode, usually `/home/elmeriniemi/stuff/netan/target/debug/netan`
- `WSL distro`, `WSL workspace root`, and `WSL netan executable` for WSL mode
- `Dataset`: choose an imported dataset after pressing `Refresh`
- `Profile`: point at a YAML/TOML profile such as `examples/profiles/car_research_v1.yml`

If the dataset has not been imported yet, do that once from the CLI or the native GUI first.

## Windows QGIS + WSL

Yes, you can use this from Windows QGIS even if you build and run `netan` inside WSL.

Use these settings:

- `Execution mode`: `WSL`
- `QGIS-visible workspace root`: `\\wsl$\Ubuntu\home\elmeriniemi\stuff\netan`
- `WSL distro`: `Ubuntu`
- `WSL workspace root`: `/home/elmeriniemi/stuff/netan`
- `WSL netan executable`: `/home/elmeriniemi/stuff/netan/target/debug/netan`

How it works:

- QGIS reads manifests and output layers through the Windows-visible `\\wsl$` path.
- The plugin launches `wsl.exe` and runs the Linux `netan` binary inside your distro.
- Relative paths like `examples/profiles/car_research_v1.yml` or `.netan/runs/qgis-route.geojson` are translated automatically.

This is the simplest path if your repo and build artifacts live inside WSL.

If you want the cleanest Windows integration, build a native Windows `netan.exe` and use `Native` mode instead.

## Full Windows Example

This example shows both supported Windows flows for QGIS 4.

### A. Windows QGIS 4 calling into WSL

1. Build the CLI in WSL:

```bash
cd /home/elmeriniemi/stuff/netan
cargo build -p netan-cli
```

2. Optionally copy the repo to Windows without the Rust build directory:

```bash
cd /home/elmeriniemi/stuff/netan
mkdir -p /windownloads/netan
for p in .* *; do
  [ "$p" = "." ] || [ "$p" = ".." ] || [ "$p" = "target" ] || cp -a -- "$p" /windownloads/netan/
done
```

3. Install the plugin into the Windows QGIS 4 profile:

```powershell
New-Item -ItemType Directory -Force "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins"
cmd /c mklink /D "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins\netan_qgis" "\\wsl$\Ubuntu\home\elmeriniemi\stuff\netan\qgis_plugin\netan_qgis"
```

4. Open QGIS 4 and enable the plugin.

5. Configure the dock with:

- `Execution mode`: `WSL`
- `QGIS-visible workspace root`: `\\wsl$\Ubuntu\home\elmeriniemi\stuff\netan`
- `WSL distro`: `Ubuntu`
- `WSL workspace root`: `/home/elmeriniemi/stuff/netan`
- `WSL netan executable`: `/home/elmeriniemi/stuff/netan/target/debug/netan`
- `Profile`: `examples/profiles/car_research_v1.yml`

6. Press `Refresh`, select `groningen_2026_03`, then run:

- route output: `.netan/runs/qgis-route.geojson`
- OD output: `.netan/runs/qgis-od.gpkg`
- matrix output: `.netan/runs/qgis-matrix.gpkg`

### B. Windows QGIS 4 with native `netan.exe`

1. Copy the repo to Windows:

```bash
cd /home/elmeriniemi/stuff/netan
mkdir -p /windownloads/netan
for p in .* *; do
  [ "$p" = "." ] || [ "$p" = ".." ] || [ "$p" = "target" ] || cp -a -- "$p" /windownloads/netan/
done
```

2. Build the Windows executable from PowerShell:

```powershell
cd C:\path\to\netan
cargo build -p netan-cli
```

3. Install the plugin:

```powershell
New-Item -ItemType Directory -Force "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins"
cmd /c mklink /D "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins\netan_qgis" "C:\path\to\netan\qgis_plugin\netan_qgis"
```

4. Configure the dock with:

- `Execution mode`: `Native`
- `QGIS-visible workspace root`: `C:\path\to\netan`
- `Native netan executable`: `C:\path\to\netan\target\debug\netan.exe`
- `Profile`: `examples/profiles/car_research_v1.yml`

5. Import data and compile a profile if needed:

```powershell
cd C:\path\to\netan
.\target\debug\netan.exe dataset import datasets\groningen-260317.osm.pbf --name groningen_2026_03
.\target\debug\netan.exe profile compile --dataset groningen_2026_03 --profile examples\profiles\car_research_v1.yml
```

6. Use the plugin normally from QGIS 4.

## Example Flows

### Route

1. Open the plugin dock.
2. Set the execution mode and the matching workspace/executable fields.
3. Press `Refresh`.
4. Pick `groningen_2026_03` as the dataset.
5. Set profile to `examples/profiles/car_research_v1.yml`.
6. Press `Compile Profile`.
7. In the `Route` tab, set:
   - request path: `examples/requests/route_from_qgis.json`
   - output path: `.netan/runs/qgis-route.geojson`
   - origin: `6.5665, 53.2194`
   - destination: `6.5716, 53.2148`
8. Press `Write Request`.
9. Press `Run Route`.

The resulting line layer loads into QGIS automatically.

### OD

Use:

- pairs path: `examples/requests/od_pairs.csv`
- output path: `.netan/runs/qgis-od.gpkg`

Then press `Run OD`.

### Matrix

Use:

- origins path: `examples/requests/matrix_origins.csv`
- destinations path: `examples/requests/matrix_destinations.csv`
- output path: `.netan/runs/qgis-matrix.gpkg`

Then press `Run Matrix`.

## Notes

- The plugin does not embed the Rust engine; it shells out to the local CLI.
- Relative paths are resolved against the configured workspace root for the active mode.
- Only spatial outputs are auto-loaded into QGIS.
- In WSL mode, output layers are loaded back into QGIS through `\\wsl$\<distro>\...`.
