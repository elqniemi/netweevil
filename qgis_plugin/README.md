<p align="center">
  <img src="netweevil_qgis/logo.svg" alt="NetWeevil logo" width="120" height="120">
</p>

# NetWeevil QGIS plugin

This folder contains the source of the NetWeevil QGIS plugin. The plugin sends
requests to a running NetWeevil HTTP API and loads the results as QGIS layers.

The user guide is on the documentation site:
[QGIS plugin](https://elqniemi.github.io/netweevil/tools/qgis-plugin/)
([source](../docs/tools/qgis-plugin.md)).

## Compatibility

The plugin supports QGIS 3.28 to 4.99 (Qt 5 and Qt 6). Put all Qt and QGIS
version differences in `netweevil_qgis/compat.py`.

## Package

1. Go to the repository root.
2. Run the package script:

   ```bash
   scripts/package_qgis_plugin.sh
   ```

The script reads `version` from `netweevil_qgis/metadata.txt`. It writes
`dist/netweevil_qgis-<version>.zip`. Install this file in QGIS with
**Plugins → Manage and Install Plugins → Install from ZIP**.

## Develop

Make a symbolic link from your QGIS profile to the plugin source. This example
is for Linux and QGIS 3:

```bash
ln -s "$(pwd)/qgis_plugin/netweevil_qgis" \
  "$HOME/.local/share/QGIS/QGIS3/profiles/default/python/plugins/netweevil_qgis"
```

The user guide gives the paths for macOS, Windows and QGIS 4.

## Check

A full QGIS runtime is not always available. Do these static checks:

1. Compile all modules:

   ```bash
   python3 -m py_compile qgis_plugin/netweevil_qgis/*.py
   ```

2. Make the package with `scripts/package_qgis_plugin.sh`.
3. When you change Qt code, load the plugin in QGIS 3 and in QGIS 4.

## Module layout

| Module | Contents |
|---|---|
| `plugin.py`, `dock.py` | Plugin entry point and dock widget |
| `connection.py` | Connection bar and Settings tab |
| `route_tab.py`, `transit_tab.py`, `service_area_tab.py`, `batch_tab.py`, `trip_tab.py`, `simulation_tab.py`, `runs_tab.py` | Tabs |
| `results.py` | Layer loading for results |
| `map_points.py` | Map point tools |
| `advanced.py` | Advanced request controls |
| `settings_io.py` | Saved plugin settings |
| `temporal_controls.py` | Simulation playback and Temporal Controller layers |
| `constants.py` | Shared constants |
| `compat.py` | Qt 5 and Qt 6 differences |

## License

The plugin is available under the MIT license. See
[`netweevil_qgis/LICENSE`](netweevil_qgis/LICENSE).
