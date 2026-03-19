# netan QGIS Plugin

This plugin now talks directly to the running `netan` API.

It no longer shells out to the CLI. QGIS sends JSON requests to the API, receives JSON or GeoJSON responses, and loads the returned geometries into the map immediately.

## What It Does

- connects to a running `netan api serve` instance
- reads the loaded dataset and available profiles from `/v1/service`
- writes route request JSON from typed coordinates
- submits route, OD, and matrix requests to the API
- loads route, OD, and matrix geometries into QGIS from API responses
- supports both normal JSON responses and direct GeoJSON responses

## Install In QGIS

1. Start the API:

```bash
cargo run -p netan-cli -- api serve \
  --dataset ile_de_france_2026_03 \
  --default-profile examples/profiles/car_research_v1.yml \
  --profile examples/profiles/pedestrian_research_v1.yml \
  --bind 127.0.0.1:8080
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

- `Workspace root`: local root used only for browsing and saving request/response files
- `API base URL`: for example `http://127.0.0.1:8080`
- `Timeout seconds`
- `Response format`: `JSON` or `GeoJSON`
- `Profile`: either the service default or any profile preloaded by the API

Then press `Refresh Service`.

The plugin will show:

- the dataset currently loaded by the API
- the API default profile
- the available profile list
- the loaded dataset bounds when available

## Windows QGIS + WSL

This is simpler now than the old CLI integration. Run the API wherever `netan` lives, then point QGIS at that HTTP endpoint.

Examples:

- API inside WSL, QGIS on Windows:
  - `API base URL`: `http://127.0.0.1:8080` if you expose it locally from WSL
  - `Workspace root`: any Windows-accessible folder you want to use for request and response files
- Native Linux QGIS:
  - `API base URL`: `http://127.0.0.1:8080`
  - `Workspace root`: `/home/elmeriniemi/stuff/netan`

## Example Flows

### Route

1. Open the plugin dock.
2. Set `Workspace root` and `API base URL`.
3. Press `Refresh Service`.
4. Choose either the service default profile or a specific loaded profile.
5. In the `Route` tab, set:
   - request path: `examples/requests/route_from_qgis.json`
   - response path: `.netan/runs/qgis-route.geojson`
   - origin: `6.5665, 53.2194`
   - destination: `6.5716, 53.2148`
6. Press `Write Request`.
7. Press `Run Route`.

The resulting line layer loads into QGIS automatically.

### OD

Use:

- pairs path: `examples/requests/od_pairs.csv`
- response path: `.netan/runs/qgis-od.geojson`

Then press `Run OD`.

### Matrix

Use:

- origins path: `examples/requests/matrix_origins.csv`
- destinations path: `examples/requests/matrix_destinations.csv`
- response path: `.netan/runs/qgis-matrix.geojson`

Then press `Run Matrix`.

## Notes

- The plugin does not embed the Rust engine; it calls the running HTTP API.
- Relative request/input/output paths are resolved against the configured workspace root.
- OD and matrix CSV inputs are parsed locally and sent to the API as JSON.
- YAML request files are not supported by the plugin in API mode.
- When `Response format` is `JSON`, the plugin still builds a temporary GeoJSON layer locally when geometry is present in the API response.
- In WSL mode, output layers are loaded back into QGIS through `\\wsl$\<distro>\...`.
