"""Runs tab: reload saved responses and run manifests."""

from pathlib import Path

from qgis.PyQt.QtCore import QTimer
from qgis.PyQt.QtWidgets import (
    QComboBox,
    QFormLayout,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from .compat import MSG_WARNING


class RunsTabMixin:
    def _build_runs_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        summary = QLabel(
            "Load a saved netweevil API response, GeoJSON output, or succeeded run manifest without executing the analysis again."
        )
        summary.setWordWrap(True)
        layout.addWidget(summary)

        saved_group = QGroupBox("Completed runs")
        saved_form = QFormLayout(saved_group)
        self.saved_runs_directory_edit = QLineEdit(".netweevil/runs")
        self.saved_runs_combo = QComboBox()
        self.saved_runs_combo.addItem("Refresh to list completed runs", "")
        saved_form.addRow(
            "Runs directory",
            self._line_with_browse(self.saved_runs_directory_edit, browse_dir=True),
        )
        saved_form.addRow("Saved run", self.saved_runs_combo)
        layout.addWidget(saved_group)

        file_group = QGroupBox("Load file")
        file_form = QFormLayout(file_group)
        self.saved_run_path_edit = QLineEdit("")
        self.saved_run_kind_combo = QComboBox()
        self.saved_run_kind_combo.addItem("Auto detect", "")
        self.saved_run_kind_combo.addItem("Route", "route")
        self.saved_run_kind_combo.addItem("Transit route", "transit_route")
        self.saved_run_kind_combo.addItem("OD", "od")
        self.saved_run_kind_combo.addItem("Matrix", "matrix")
        self.saved_run_kind_combo.addItem("Service area", "service_area")
        file_form.addRow(
            "File",
            self._line_with_browse(self.saved_run_path_edit, browse_dir=False),
        )
        file_form.addRow("Kind", self.saved_run_kind_combo)
        layout.addWidget(file_group)

        button_row = QHBoxLayout()
        refresh_button = QPushButton("Refresh Runs")
        refresh_button.clicked.connect(lambda: self.refresh_saved_runs(manual=True))
        use_selected_button = QPushButton("Use Selected")
        use_selected_button.clicked.connect(self.use_selected_saved_run)
        load_button = QPushButton("Load Run")
        load_button.clicked.connect(self.load_saved_run)
        button_row.addWidget(refresh_button)
        button_row.addWidget(use_selected_button)
        button_row.addStretch(1)
        button_row.addWidget(load_button)
        layout.addLayout(button_row)

        layout.addStretch(1)
        QTimer.singleShot(0, self.refresh_saved_runs)
        return tab

    def refresh_saved_runs(self, manual=False):
        directory = self.resolve_local_path(self.saved_runs_directory_edit.text())
        self.saved_runs_combo.clear()
        if not directory.exists():
            self.saved_runs_combo.addItem(
                "Runs directory does not exist yet: {}".format(directory), ""
            )
            if manual:
                self.log("Runs directory does not exist: {}".format(directory), MSG_WARNING)
            return

        candidates = []
        for path in directory.rglob("*"):
            if not path.is_file():
                continue
            if path.suffix.lower() not in [".json", ".geojson"]:
                continue
            try:
                modified = path.stat().st_mtime
            except OSError:
                modified = 0
            candidates.append((modified, path))
        candidates.sort(key=lambda item: item[0], reverse=True)

        if not candidates:
            self.saved_runs_combo.addItem("No JSON or GeoJSON runs found", "")
            return

        for _modified, path in candidates:
            try:
                label = path.relative_to(self.workspace_root())
            except ValueError:
                label = path
            self.saved_runs_combo.addItem(str(label), str(path))
        self.log("Found {} saved run file(s).".format(len(candidates)))

    def use_selected_saved_run(self):
        path = self.saved_runs_combo.currentData()
        if not path:
            self.alert("Refresh runs and choose a saved run first.")
            return
        self.saved_run_path_edit.setText(path)

    def load_saved_run(self):
        raw_path = self.saved_run_path_edit.text().strip() or self.saved_runs_combo.currentData()
        if not raw_path:
            self.alert("Choose a saved run file first.")
            return
        path = self.resolve_local_path(raw_path)
        explicit_kind = self.saved_run_kind_combo.currentData() or None
        try:
            self.load_saved_run_path(path, explicit_kind)
        except Exception as exc:
            self.alert("Failed to load saved run: {}".format(exc))

    def load_saved_run_path(self, path, explicit_kind=None):
        path = Path(path).expanduser()
        if not path.exists():
            raise ValueError("file does not exist: {}".format(path))

        suffix = path.suffix.lower()
        if suffix == ".geojson":
            geojson = self.read_json(path)
            kind = explicit_kind or self.infer_geojson_analysis_kind(geojson)
            layer_name = self.saved_layer_name(path, kind, geojson)
            self.log_geojson_messages(kind or "saved_run", geojson)
            if kind == "service_area":
                self.load_service_area_layers(geojson, layer_name)
            else:
                loaded_layer = self.load_output_layer(path, layer_name)
                if loaded_layer is not None:
                    self.set_last_output_layers([loaded_layer])
            self.log("Loaded saved run from {}".format(path))
            self.save_settings()
            return

        if suffix != ".json":
            raise ValueError("saved runs must be .json or .geojson")

        payload, kind, source_path = self.load_saved_json_payload(path, explicit_kind)
        kind = kind or explicit_kind or self.infer_json_analysis_kind(payload)
        if not kind:
            raise ValueError("could not infer run kind; choose one from the Kind field")

        self.log_analysis_messages(kind, payload)
        layer_name = self.saved_json_layer_name(source_path, kind, payload)
        if kind == "route":
            self.load_route_layers(payload, layer_name)
        elif kind == "transit_route":
            self.load_transit_route_layers(payload, layer_name)
        else:
            geojson = self.analysis_json_to_geojson(kind, payload)
            if geojson is None:
                raise ValueError("saved {} result has no loadable geometry".format(kind))
            if kind == "service_area":
                self.load_service_area_layers(geojson, layer_name)
            else:
                temp_path = self.write_temp_geojson(layer_name, geojson)
                loaded_layer = self.load_output_layer(temp_path, layer_name)
                if loaded_layer is not None:
                    self.set_last_output_layers([loaded_layer])
        self.log("Loaded saved {} run from {}".format(kind, source_path))
        self.save_settings()

    def load_saved_json_payload(self, path, explicit_kind=None):
        parsed = self.read_json(path)
        if self.is_run_manifest(parsed):
            result_path = self.resolve_manifest_result_path(parsed)
            result = self.read_json(result_path)
            kind = explicit_kind or parsed.get("run_kind")
            service = {
                "dataset_id": parsed.get("dataset_id"),
                "profile_id": parsed.get("profile_id"),
                "profile_hash": (parsed.get("methods_summary") or {}).get(
                    "locked_profile_hash"
                ),
                "route_engine": (parsed.get("algorithm") or {}).get("engine"),
                "batch_engine": (parsed.get("algorithm") or {}).get("engine"),
                "acceleration": (parsed.get("algorithm") or {}).get("acceleration"),
            }
            return {"service": service, "result": result}, kind, result_path

        if isinstance(parsed, dict) and "service" in parsed and "result" in parsed:
            return parsed, explicit_kind or self.infer_json_analysis_kind(parsed), path

        return (
            {"service": {}, "result": parsed},
            explicit_kind or self.infer_result_analysis_kind(parsed),
            path,
        )

    def is_run_manifest(self, value):
        return (
            isinstance(value, dict)
            and value.get("status") == "succeeded"
            and value.get("run_kind")
            and value.get("result_path")
        )

    def resolve_manifest_result_path(self, manifest):
        raw = str(manifest.get("result_path") or "").strip()
        if not raw:
            raise ValueError("run manifest has no result_path")
        path = Path(raw).expanduser()
        if not path.is_absolute():
            path = self.workspace_root() / path
        if path.exists():
            return path

        alternate = Path(str(path).replace("/.netan/", "/.netweevil/"))
        if alternate.exists():
            return alternate

        fallback = self.workspace_root() / ".netweevil" / "runs" / path.name
        if fallback.exists():
            return fallback
        raise ValueError("run result file does not exist: {}".format(path))

    def infer_json_analysis_kind(self, payload):
        if isinstance(payload, dict) and "result" in payload:
            return self.infer_result_analysis_kind(payload.get("result") or {})
        return self.infer_result_analysis_kind(payload)

    def infer_result_analysis_kind(self, result):
        if not isinstance(result, dict):
            return None
        if "legs" in result and "summary" in result:
            return "transit_route"
        if "pairs" in result:
            return "od"
        if "cells" in result:
            return "matrix"
        if "analysis_id" in result and "features" in result:
            return "service_area"
        if "route_id" in result and "origin" in result and "destination" in result:
            return "route"
        return None

    def infer_geojson_analysis_kind(self, geojson):
        metadata = geojson.get("metadata") if isinstance(geojson, dict) else {}
        if isinstance(metadata, dict) and metadata.get("analysis_id"):
            return "service_area"
        features = geojson.get("features") if isinstance(geojson, dict) else []
        if not features:
            return None
        properties = (features[0] or {}).get("properties") or {}
        if properties.get("geometry_type") in ["network", "polygon"]:
            return "service_area"
        if properties.get("pair_id"):
            return "od"
        if properties.get("origin_id") and properties.get("destination_id"):
            return "matrix"
        if properties.get("route_id"):
            return "route"
        return None

    def saved_layer_name(self, path, kind, geojson):
        metadata = geojson.get("metadata") if isinstance(geojson, dict) else {}
        if isinstance(metadata, dict):
            for key in ["analysis_id", "route_id"]:
                value = metadata.get(key)
                if value:
                    return str(value)
        return "{}_{}".format(kind or "saved_run", path.stem)

    def saved_json_layer_name(self, path, kind, payload):
        result = payload.get("result") or {}
        for key in ["route_id", "analysis_id"]:
            value = result.get(key)
            if value:
                return str(value)
        return "{}_{}".format(kind or "saved_run", Path(path).stem)
