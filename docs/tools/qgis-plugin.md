# QGIS plugin

The NetWeevil QGIS plugin sends requests to a running NetWeevil HTTP API. It
loads the results into the QGIS project as layers.

The plugin does not contain the routing engine and does not run the CLI. It
sends JSON requests and reads JSON or GeoJSON results.

## Compatibility

The plugin supports QGIS 3.28 to 4.99:

- QGIS 3.28 and later 3.x versions (Qt 5),
- QGIS 4.x (Qt 6).

The metadata sets `supportsQt6`. The module `netweevil_qgis/compat.py`
contains all Qt and QGIS version differences.

## Install the plugin

### Install from a ZIP file

1. Go to the repository root.
2. Make the plugin package:

    ```bash
    scripts/package_qgis_plugin.sh
    ```

    The script reads `version` from `qgis_plugin/netweevil_qgis/metadata.txt`.
    It writes `dist/netweevil_qgis-<version>.zip`.

3. In QGIS, open **Plugins → Manage and Install Plugins → Install from ZIP**.
4. Select `dist/netweevil_qgis-<version>.zip`.
5. Click **Install Plugin**.
6. Enable **netweevil** in the plugin list.

### Link the source for development

Make a symbolic link from the QGIS profile to the plugin source. Use `QGIS4`
in place of `QGIS3` for QGIS 4.

macOS:

```bash
mkdir -p "$HOME/Library/Application Support/QGIS/QGIS3/profiles/default/python/plugins"
ln -s "$(pwd)/qgis_plugin/netweevil_qgis" \
  "$HOME/Library/Application Support/QGIS/QGIS3/profiles/default/python/plugins/netweevil_qgis"
```

Linux:

```bash
mkdir -p "$HOME/.local/share/QGIS/QGIS3/profiles/default/python/plugins"
ln -s "$(pwd)/qgis_plugin/netweevil_qgis" \
  "$HOME/.local/share/QGIS/QGIS3/profiles/default/python/plugins/netweevil_qgis"
```

Windows PowerShell:

```powershell
New-Item -ItemType Directory -Force "$env:APPDATA\QGIS\QGIS3\profiles\default\python\plugins"
cmd /c mklink /D "$env:APPDATA\QGIS\QGIS3\profiles\default\python\plugins\netweevil_qgis" "C:\path\to\netweevil\qgis_plugin\netweevil_qgis"
```

Restart QGIS after you make the link.

## Start the API

1. Start the API with the dataset and the profiles that you need:

    ```bash
    netweevil api serve \
      --dataset groningen_2026_05 \
      --default-profile examples/profiles/car_research_v1.yml \
      --profile examples/profiles/pedestrian_research_v1.yml \
      --transit-feed openov_groningen \
      --bind 127.0.0.1:8080
    ```

2. Remove `--transit-feed` if you do not need transit analyses.

From a source checkout, use `cargo run -p netweevil-cli -- api serve ...`.

## Connect

1. Click the netweevil button on the toolbar. The dock opens.
2. Make sure that the API URL in the connection bar is correct. The default is
   `http://127.0.0.1:8080`.
3. Click **Connect**. The plugin also connects automatically when the dock
   opens and before each run.
4. Read the status line. It shows the dataset, the number of profiles and the
   number of transit feeds.
5. Select a profile in the connection bar, or keep the service default.
6. Optional: set **Workspace root** in the Settings tab.

The plugin uses the workspace root to resolve relative paths, for example
`.netweevil/runs/...`. Set it to the folder in which the API runs. This keeps
the QGIS outputs with the other workspace files. The default is your home
folder.

The connection check uses a short timeout. An API that is not available does
not stop QGIS for the full analysis timeout.

## Tabs

The dock has a connection bar at the top: API URL, **Connect**, a status
indicator and the active profile. The tabs are below the bar.

| Tab | Purpose |
|---|---|
| Route | Street route between two points |
| Transit | Walk access and scheduled GTFS transit route |
| Service Area | Reachable network or polygons around one or more origins |
| OD / Matrix | Origin-destination pairs and travel matrices |
| Trip | Best visit order of many stops, with service times and time windows |
| Simulation | Agent-based traffic simulation with playback |
| Runs | Load saved results again without a new request |
| Settings | Workspace root, timeout, saved file format and dataset information |

Each tab shows the points and inputs first. Advanced options are in sections
that you can expand.

Unsafe degraded-routing modes are off by default. The plugin does not save
them between sessions. They show in red when they are on. The plugin asks for
confirmation before it sends a request with them. The plugin saves all other
connection and analysis settings between sessions.

## Workflows

### Route

1. In the Route tab, click **Pick On Map** under Start.
2. Click the map. Click the button again to stop picking.
3. Do steps 1 and 2 for End.
4. Optional: click **From Selected Feature** to use a selected point of the
   active layer.
5. Click **Run Route**.

The plugin loads the route as a group of layers and tables: the route, segment
rows, hop segments, violations, and road type and surface breakdowns.

Two options add more data:

- **Countries/states, border crossings, and timezones** (`returns.admin_areas`)
  adds a `regions` line layer.
- **Elevation profile, ascent, and descent** (`returns.elevation`) adds an
  `elevation_profile` table.

The route id increases after each run, so a new run does not overwrite a
result. The **Returned Route Detail** and **Connectivity + Fallback** sections
contain the advanced request fields.

### Transit

Start the API with one or more `--transit-feed <feed_id>` options.

1. In the Transit tab, select a loaded feed.
2. Type a departure time, for example `2026-05-11T08:30:00+02:00`.
3. Pick an origin and a destination on the map, or use selected features.
4. Optional: change the modes and the access and transfer limits in
   **Modes and Limits**.
5. Click **Run Transit Route**.

The plugin saves the JSON result. It loads a route summary and layers for
legs, stops and stop segments. Access, egress, transfer and transit legs have
different styles.

### Service area

1. In the Service Area tab, add origins with **Pick On Map**,
   **Add Selected Features**, **Use Route Start** or **Use Route Start + End**.
2. Type the thresholds, for example `300, 600`.
3. Select the unit: **Distance (m)** or **Travel time (s)**.
4. Select the street network options, or set **Mode** to **Transit feed**. The
   tab shows only the controls of the selected mode.
5. Click **Run Service Area**.

**Direction** selects outbound areas (travel from the origins) or inbound areas
(travel to the origins, static only).

**Polygon generation** has these fields. The plugin does not send empty
fields.

- **Denoise (0-1)** removes small parts.
- **Min part area m2** removes parts below this area.
- **Min hole area m2** fills holes below this area.
- **Cell size m** sets the tracing grid.

The plugin loads the result as layers grouped by threshold and geometry type.

### Trip

1. In the Trip tab, add 2 to 2000 stops. Use one of these methods:
    - **Pick On Map**. Each click adds a stop until you click the button again.
    - **Add Selected Features**.
    - **Use Route Start + End**.
    - Set **Source** to a loaded point layer.
2. Select a mode: round trip, fixed start or end, fixed endpoints, or open.
3. Type a departure time in seconds or as `H:MM[:SS]`.
4. Type a default service time.
5. Select if late arrivals are permitted.
6. Click **Run Trip**.

Stop lines use the format `id,lon,lat[,service_s[,earliest_s[,latest_s]]]`.
The plugin reads layer fields with the names `service_s`, `earliest_s` and
`latest_s`.

The plugin sends the request to `/v1/trip` and loads these layers:

- `visits`: points with labels in visit order. Late stops are red.
- `legs`: lines with from stop, to stop, distance and time.
- `trip_summary`: a table with one row.

The log shows the visit order, totals, time window status and the optimizer
method, proof flags, costs and times.

### OD and matrix

- OD: set **Pairs file** to a CSV or JSON pairs document. Click **Run OD**.
- Matrix: make origins and destinations from loaded point layers or from CSV
  or JSON files. Click **Run Matrix**.

OD CSV files use the columns `id,source_lon,source_lat,target_lon,target_lat`.
The columns `source_z,target_z` are optional. Point CSV files use `id,lon,lat`.
The column `z` (metres) is optional. JSON files contain `pairs` or `points`.
The plugin reads CSV files and sends their contents to the API as JSON.

### Simulation

1. Add one or more fleets: profile, agent count, origin and destination
   distributions, departure curve and behavior.
2. Optional: add polygon zones (no access, speed or capacity factors, high
   traffic, spawn or attract).
3. Click **Run Simulation**.
4. Optional: control the run. You can pause, resume, cancel, change the speed
   factor, add fleets, and add or remove zones.
5. Use the playback slider or **Play** to see the agents.
6. Load congestion windows, busy segments, or a temporal layer for the QGIS
   Temporal Controller.

### Saved runs

1. In the Runs tab, select a file from the runs folder, or browse to a file.
2. Keep **Kind** at **Auto detect**.
3. Click **Load Run**.

The plugin reads API result JSON, GeoJSON, CLI result JSON and run manifests
with a status of succeeded.

## Notes

- The plugin transforms map points and layer features from their CRS to WGS84
  before it sends them.
- Route and transit requests always use JSON results. This keeps segment rows,
  breakdown tables and legs available.
- The **Saved OD/Matrix/Area format** setting changes only the file format of
  saved batch results.
- Saved results keep the `service` metadata of the API result.

## License

The QGIS plugin is available under the MIT license. The plugin folder
contains a `LICENSE` file, so each ZIP package contains the license text.
