# netweevil QGIS Plugin

This plugin talks directly to the running `netweevil` API.

It does not shell out to the CLI. QGIS sends JSON requests to the API, receives JSON or GeoJSON responses, and loads the returned geometries into the map immediately.

## What It Does

- connects to a running `netweevil api serve` instance
- reads the loaded dataset and available profiles from `/v1/service`
- lets you pick route start and end points directly from the QGIS map canvas
- can pull route points from a selected point feature in the active layer
- submits route, OD, matrix, and service-area requests to the API
- submits scheduled transit route requests when the API has preloaded GTFS feeds
- loads route, transit, OD, matrix, and service-area geometries into QGIS from API responses
- reloads completed JSON/GeoJSON runs and succeeded run manifests without rerunning the API request
- can build matrix origins and destinations from loaded QGIS point layers
- can build service-area origins from map picks, selected point features, or the current route endpoints
- exposes disconnected-network policies and opt-in degraded-routing controls for route and batch requests
- supports both normal JSON responses and direct GeoJSON responses
- remembers connection and analysis settings between QGIS sessions

## Install In QGIS

1. Start the API:

```bash
cargo run -p netweevil-cli -- api serve \
  --dataset ile_de_france_2026_03 \
  --default-profile examples/profiles/car_research_v1.yml \
  --profile examples/profiles/pedestrian_research_v1.yml \
  --transit-feed openov_groningen \
  --bind 127.0.0.1:8080
```

Omit `--transit-feed` if you only need road-network analyses.

2. Copy or symlink [`netweevil_qgis`](/Users/elmeriniemi/programming/netweevil/qgis_plugin/netweevil_qgis) into your QGIS profile plugin directory.

macOS example:

```bash
mkdir -p ~/Library/Application\ Support/QGIS/QGIS3/profiles/default/python/plugins
ln -s /Users/elmeriniemi/programming/netweevil/qgis_plugin/netweevil_qgis ~/Library/Application\ Support/QGIS/QGIS3/profiles/default/python/plugins/netweevil_qgis
```

Windows QGIS 4 example:

```powershell
New-Item -ItemType Directory -Force "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins"
cmd /c mklink /D "$env:APPDATA\QGIS\QGIS4\profiles\default\python\plugins\netweevil_qgis" "\\wsl$\Ubuntu\home\elmeriniemi\stuff\netweevil\qgis_plugin\netweevil_qgis"
```

3. In QGIS, enable the plugin from `Plugins -> Manage and Install Plugins`.

## Package ZIP

Build a QGIS-installable ZIP from the repository root:

```bash
./scripts/package_qgis_plugin.sh
```

The script reads `version` from `qgis_plugin/netweevil_qgis/metadata.txt` and writes:

```text
dist/netweevil_qgis-<version>.zip
```

For manual packaging, the ZIP must contain `netweevil_qgis/metadata.txt`, `netweevil_qgis/__init__.py`, and `netweevil_qgis/plugin.py` at the archive root. Do not include `__pycache__` or `.pyc` files.

## First Run

Set these fields in the dock:

- `Workspace root`: local root used only for browsing and saving request/response files
- `API base URL`: for example `http://127.0.0.1:8080`
- `Timeout seconds`
- `Response format`: `JSON` or `GeoJSON`
- `Profile`: either the service default or any profile preloaded by the API
- `Loaded transit feeds`: feed ids available to the Transit tab

Then press `Refresh Service`.

The plugin will show:

- the dataset currently loaded by the API
- the API default profile
- the available profile list
- the loaded transit feed list
- the loaded dataset bounds when available

## Windows QGIS + WSL

This is simpler now than the old CLI integration. Run the API wherever `netweevil` lives, then point QGIS at that HTTP endpoint.

Examples:

- API inside WSL, QGIS on Windows:
  - `API base URL`: `http://127.0.0.1:8080` if you expose it locally from WSL
  - `Workspace root`: any Windows-accessible folder you want to use for request and response files
- Native Linux QGIS:
  - `API base URL`: `http://127.0.0.1:8080`
  - `Workspace root`: `/home/elmeriniemi/stuff/netweevil`

## Example Flows

### Route

1. Open the plugin dock.
2. Set `Workspace root` and `API base URL`.
3. Press `Refresh Service`.
4. Choose either the service default profile or a specific loaded profile.
5. In the `Route` tab, keep the generated route id or set your own.
6. Click `Pick Start`, then click on the map canvas.
7. Click `Pick End`, then click on the map canvas.
8. Optionally use `From Selected Feature` if you already have a point selected in the active point layer.
9. Choose the returned route detail you want, including segment rows, road-type totals, and surface totals.
10. Optionally press `Save Request` if you want the route JSON on disk.
11. Press `Run Route`.

The plugin loads the route into a grouped set of QGIS layers and tables automatically. When the API returns them, the group includes the main route, segment rows, hop segments, violations, road-type breakdowns, and surface breakdowns. After a successful run, the default route id advances so the next query does not overwrite the previous route name.

### Saved Runs

Use the `Runs` tab to load previous outputs without running an analysis again.

1. Set `Runs directory`, usually `.netweevil/runs`.
2. Press `Refresh Runs`.
3. Choose a saved JSON, GeoJSON, or succeeded run manifest.
4. Press `Use Selected`.
5. Leave `Kind` on `Auto detect`, or choose the analysis type if the file is ambiguous.
6. Press `Load Run`.

The loader handles API response JSON, direct GeoJSON, raw CLI result JSON, and succeeded run manifests that point to a result file.

### Transit

Start the API with one or more `--transit-feed <feed_id>` values, then press `Refresh Service`.

1. In the `Transit` tab, choose a loaded feed.
2. Set a route id and departure time such as `2026-05-11T08:30:00+02:00`.
3. Pick an origin and destination on the map, or use selected point features.
4. Choose the allowed transit modes and access/transfer limits.
5. Press `Save Request` if you want the request JSON on disk.
6. Press `Run Transit Route`.

The plugin saves the raw JSON response and loads a grouped route summary layer plus per-leg layer. Access, egress, transfer, and transit legs are styled separately.

### OD

Use:

- pairs file: `examples/requests/od_pairs.csv`
- response path: `.netweevil/runs/qgis-od.geojson`

Then press `Run OD`.

### Matrix

Use either loaded point layers or files:

- set `Origins` and `Destinations` source to `Loaded point layer` to build the matrix from QGIS layers already in the project
- optionally choose an ID field and `Use selected features only`
- or switch either side to `CSV or JSON file`
- set response path: `.netweevil/runs/qgis-matrix.geojson`

Then press `Run Matrix`.

### Service Area

Use the dedicated `Service Area` tab:

1. Set an `Analysis id`.
2. Enter one threshold list such as `300, 600`.
3. Pick either `Distance (m)` or `Travel time (s)` for the whole threshold list.
4. Append origins with `Pick On Map`, `Add Selected Features`, `Use Route Start`, or `Use Route Start + End`.
5. Choose `Output mode`, `Band mode`, `Boundary mode`, and `Multi-origin mode`.
6. Optionally expand `Connectivity Controls` to set disconnected-network behavior.
7. Press `Run Service Area`.

The plugin saves the raw API response and loads grouped sublayers back into QGIS by threshold and geometry type.

### Advanced Route And Batch Controls

- `Route` exposes a dedicated `Returned route detail` section for geometry mode, segment rows, and route breakdown request flags.
- `Route`, `Batch`, and `Service Area` keep disconnected-network policies under collapsible connectivity controls.
- Unsafe degraded-routing options remain under a separate collapsed `Unsafe Failure Modes` section and are off by default.
- The plugin shows a preflight confirmation before sending a request with unsafe failure modes enabled.
- Diagnostics now include suggested next actions in the dock log when the API returns them.

## Notes

- The plugin does not embed the Rust engine; it calls the running HTTP API.
- Relative request/input/output paths are resolved against the configured workspace root.
- OD and matrix CSV inputs are parsed locally and sent to the API as JSON.
- Route points are transformed from the current map or layer CRS into WGS84 before being sent to the API.
- YAML request files are not supported by the plugin in API mode.
- Route detail layers use JSON responses internally when needed, even if the dock is set to `GeoJSON`, so the plugin can still load segment rows and breakdown tables.
- Transit route requests use JSON responses because the API endpoint returns itinerary legs rather than GeoJSON directly.
- When `Response format` is `JSON`, the plugin still builds temporary GeoJSON layers locally when geometry is present in the API response.
- Service-area outputs are loaded into grouped sublayers so thresholds and geometry modes stay inspectable in QGIS.
- In WSL mode, output layers are loaded back into QGIS through `\\wsl$\<distro>\...`.
