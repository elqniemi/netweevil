import csv
import json
import tempfile
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from qgis.PyQt.QtCore import Qt
from qgis.PyQt.QtWidgets import (
    QAction,
    QComboBox,
    QDockWidget,
    QFileDialog,
    QFormLayout,
    QGridLayout,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPushButton,
    QPlainTextEdit,
    QTabWidget,
    QVBoxLayout,
    QWidget,
)
from qgis.core import Qgis, QgsMessageLog, QgsProject, QgsVectorLayer


PLUGIN_MENU = "&netan"


class ResponseFormat:
    JSON = "json"
    GEOJSON = "geojson"


class NetanPlugin:
    def __init__(self, iface):
        self.iface = iface
        self.action = None
        self.dock = None

    def initGui(self):
        self.action = QAction("netan", self.iface.mainWindow())
        self.action.triggered.connect(self.toggle_dock)
        self.iface.addPluginToMenu(PLUGIN_MENU, self.action)
        self.iface.addToolBarIcon(self.action)

    def unload(self):
        if self.action is not None:
            self.iface.removePluginMenu(PLUGIN_MENU, self.action)
            self.iface.removeToolBarIcon(self.action)
        if self.dock is not None:
            self.iface.removeDockWidget(self.dock)
            self.dock.deleteLater()
            self.dock = None

    def toggle_dock(self):
        if self.dock is None:
            self.dock = NetanDock(self.iface)
            self.iface.addDockWidget(Qt.RightDockWidgetArea, self.dock)
        self.dock.show()
        self.dock.raise_()


class NetanDock(QDockWidget):
    def __init__(self, iface):
        super().__init__("netan", iface.mainWindow())
        self.iface = iface
        self.service_info = None
        self.temp_layers_dir = Path(tempfile.gettempdir()) / "netan_qgis_layers"
        self.temp_layers_dir.mkdir(parents=True, exist_ok=True)
        self.setObjectName("netanDock")
        self.setWidget(self._build_ui())
        self.refresh_service()

    def _build_ui(self):
        container = QWidget()
        layout = QVBoxLayout(container)

        tabs = QTabWidget()
        tabs.addTab(self._build_api_tab(), "API")
        tabs.addTab(self._build_route_tab(), "Route")
        tabs.addTab(self._build_batch_tab(), "Batch")
        layout.addWidget(tabs)

        self.log_output = QPlainTextEdit()
        self.log_output.setReadOnly(True)
        self.log_output.setPlaceholderText("API and plugin log output.")
        layout.addWidget(self.log_output)

        return container

    def _build_api_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        form = QFormLayout()
        default_root = str(Path(__file__).resolve().parents[2])
        self.workspace_root_edit = QLineEdit(default_root)
        self.api_base_url_edit = QLineEdit("http://127.0.0.1:8080")
        self.timeout_seconds_edit = QLineEdit("30")

        self.response_format_combo = QComboBox()
        self.response_format_combo.addItem("JSON", ResponseFormat.JSON)
        self.response_format_combo.addItem("GeoJSON", ResponseFormat.GEOJSON)

        self.profile_combo = QComboBox()
        self.dataset_id_edit = QLineEdit()
        self.dataset_id_edit.setReadOnly(True)
        self.default_profile_edit = QLineEdit()
        self.default_profile_edit.setReadOnly(True)
        self.dataset_bounds_edit = QLineEdit()
        self.dataset_bounds_edit.setReadOnly(True)

        form.addRow(
            "Workspace root",
            self._line_with_browse(self.workspace_root_edit, browse_dir=True),
        )
        form.addRow("API base URL", self.api_base_url_edit)
        form.addRow("Timeout seconds", self.timeout_seconds_edit)
        form.addRow("Response format", self.response_format_combo)
        form.addRow("Profile", self.profile_combo)
        form.addRow("Dataset", self.dataset_id_edit)
        form.addRow("Service default profile", self.default_profile_edit)
        form.addRow("Dataset bounds", self.dataset_bounds_edit)
        layout.addLayout(form)

        button_row = QHBoxLayout()
        refresh_button = QPushButton("Refresh Service")
        refresh_button.clicked.connect(self.refresh_service)
        button_row.addWidget(refresh_button)
        layout.addLayout(button_row)

        layout.addWidget(
            QLabel(
                "The plugin talks directly to the running netan API. "
                "Route, OD, and matrix runs are sent as JSON and loaded back into QGIS "
                "from the API response."
            )
        )
        layout.addStretch(1)
        return tab

    def _build_route_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        request_group = QGroupBox("Route request")
        request_layout = QFormLayout(request_group)
        self.route_request_path_edit = QLineEdit("examples/requests/route_from_qgis.json")
        self.route_output_path_edit = QLineEdit(".netan/runs/qgis-route.geojson")
        request_layout.addRow(
            "Request path",
            self._line_with_browse(self.route_request_path_edit, browse_dir=False),
        )
        request_layout.addRow(
            "Response path",
            self._line_with_browse(
                self.route_output_path_edit, browse_dir=False, save_dialog=True
            ),
        )

        self.route_id_edit = QLineEdit("qgis_route_001")
        self.snap_distance_edit = QLineEdit("500")
        request_layout.addRow("Route id", self.route_id_edit)
        request_layout.addRow("Snap distance m", self.snap_distance_edit)

        grid = QGridLayout()
        grid.addWidget(QLabel("Origin id"), 0, 0)
        self.origin_id_edit = QLineEdit("origin_a")
        grid.addWidget(self.origin_id_edit, 0, 1)
        grid.addWidget(QLabel("Origin lon"), 1, 0)
        self.origin_lon_edit = QLineEdit("6.5665")
        grid.addWidget(self.origin_lon_edit, 1, 1)
        grid.addWidget(QLabel("Origin lat"), 2, 0)
        self.origin_lat_edit = QLineEdit("53.2194")
        grid.addWidget(self.origin_lat_edit, 2, 1)

        grid.addWidget(QLabel("Destination id"), 0, 2)
        self.destination_id_edit = QLineEdit("destination_b")
        grid.addWidget(self.destination_id_edit, 0, 3)
        grid.addWidget(QLabel("Destination lon"), 1, 2)
        self.destination_lon_edit = QLineEdit("6.5716")
        grid.addWidget(self.destination_lon_edit, 1, 3)
        grid.addWidget(QLabel("Destination lat"), 2, 2)
        self.destination_lat_edit = QLineEdit("53.2148")
        grid.addWidget(self.destination_lat_edit, 2, 3)
        request_layout.addRow(grid)

        button_row = QHBoxLayout()
        write_request_button = QPushButton("Write Request")
        write_request_button.clicked.connect(self.write_route_request)
        run_route_button = QPushButton("Run Route")
        run_route_button.clicked.connect(self.run_route)
        button_row.addWidget(write_request_button)
        button_row.addWidget(run_route_button)
        request_layout.addRow(button_row)

        layout.addWidget(request_group)
        layout.addStretch(1)
        return tab

    def _build_batch_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        od_group = QGroupBox("OD")
        od_form = QFormLayout(od_group)
        self.od_pairs_path_edit = QLineEdit("examples/requests/od_pairs.csv")
        self.od_output_path_edit = QLineEdit(".netan/runs/qgis-od.geojson")
        od_form.addRow(
            "Pairs path",
            self._line_with_browse(self.od_pairs_path_edit, browse_dir=False),
        )
        od_form.addRow(
            "Response path",
            self._line_with_browse(self.od_output_path_edit, browse_dir=False, save_dialog=True),
        )
        run_od_button = QPushButton("Run OD")
        run_od_button.clicked.connect(self.run_od)
        od_form.addRow(run_od_button)

        matrix_group = QGroupBox("Matrix")
        matrix_form = QFormLayout(matrix_group)
        self.matrix_origins_path_edit = QLineEdit("examples/requests/matrix_origins.csv")
        self.matrix_destinations_path_edit = QLineEdit("examples/requests/matrix_destinations.csv")
        self.matrix_output_path_edit = QLineEdit(".netan/runs/qgis-matrix.geojson")
        matrix_form.addRow(
            "Origins path",
            self._line_with_browse(self.matrix_origins_path_edit, browse_dir=False),
        )
        matrix_form.addRow(
            "Destinations path",
            self._line_with_browse(self.matrix_destinations_path_edit, browse_dir=False),
        )
        matrix_form.addRow(
            "Response path",
            self._line_with_browse(
                self.matrix_output_path_edit, browse_dir=False, save_dialog=True
            ),
        )
        run_matrix_button = QPushButton("Run Matrix")
        run_matrix_button.clicked.connect(self.run_matrix)
        matrix_form.addRow(run_matrix_button)

        layout.addWidget(od_group)
        layout.addWidget(matrix_group)
        layout.addStretch(1)
        return tab

    def _line_with_browse(self, line_edit, browse_dir=False, save_dialog=False):
        row = QWidget()
        layout = QHBoxLayout(row)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(line_edit)
        button = QPushButton("Browse")
        if browse_dir:
            button.clicked.connect(lambda: self._browse_directory(line_edit))
        elif save_dialog:
            button.clicked.connect(lambda: self._browse_save_file(line_edit))
        else:
            button.clicked.connect(lambda: self._browse_file(line_edit))
        layout.addWidget(button)
        return row

    def _browse_directory(self, line_edit):
        chosen = QFileDialog.getExistingDirectory(
            self, "Select directory", str(self.workspace_root())
        )
        if chosen:
            line_edit.setText(chosen)

    def _browse_file(self, line_edit):
        chosen, _ = QFileDialog.getOpenFileName(
            self, "Select file", str(self.workspace_root())
        )
        if chosen:
            line_edit.setText(chosen)

    def _browse_save_file(self, line_edit):
        chosen, _ = QFileDialog.getSaveFileName(
            self, "Select output path", str(self.workspace_root())
        )
        if chosen:
            line_edit.setText(chosen)

    def workspace_root(self):
        return Path(self.workspace_root_edit.text().strip() or ".").resolve()

    def resolve_local_path(self, raw):
        value = raw.strip()
        if not value:
            return self.workspace_root()
        path = Path(value)
        if path.is_absolute():
            return path
        return self.workspace_root() / path

    def api_base_url(self):
        return self.api_base_url_edit.text().strip().rstrip("/")

    def timeout_seconds(self):
        raw = self.timeout_seconds_edit.text().strip() or "30"
        return float(raw)

    def selected_profile_id(self):
        return self.profile_combo.currentData()

    def response_format(self):
        return self.response_format_combo.currentData()

    def service_url(self, suffix, include_format=False):
        url = "{}{}".format(self.api_base_url(), suffix)
        if include_format and self.response_format() == ResponseFormat.GEOJSON:
            return "{}?{}".format(
                url, urllib.parse.urlencode({"format": ResponseFormat.GEOJSON})
            )
        return url

    def refresh_service(self):
        try:
            service = self.http_get_json(self.service_url("/v1/service"))
        except Exception as exc:
            self.service_info = None
            self.dataset_id_edit.setText("")
            self.default_profile_edit.setText("")
            self.dataset_bounds_edit.setText("")
            self.profile_combo.clear()
            self.log("Failed to refresh service: {}".format(exc), Qgis.Warning)
            return

        self.service_info = service
        dataset = service.get("dataset", {})
        bounds = dataset.get("topology_bounds") or {}
        bounds_text = ""
        if bounds:
            bounds_text = "{min_lon:.6f}, {min_lat:.6f}, {max_lon:.6f}, {max_lat:.6f}".format(
                min_lon=bounds.get("min_lon", 0.0),
                min_lat=bounds.get("min_lat", 0.0),
                max_lon=bounds.get("max_lon", 0.0),
                max_lat=bounds.get("max_lat", 0.0),
            )

        self.dataset_id_edit.setText(dataset.get("dataset_id", ""))
        self.default_profile_edit.setText(service.get("default_profile_id", ""))
        self.dataset_bounds_edit.setText(bounds_text)

        profiles = service.get("loaded_profiles", [])
        self.profile_combo.clear()
        default_profile_id = service.get("default_profile_id", "")
        self.profile_combo.addItem(
            "Service default ({})".format(default_profile_id or "none"), ""
        )
        for profile in profiles:
            label = "{} [{}]".format(
                profile.get("profile_id", "unknown"),
                profile.get("mode", "unknown"),
            )
            self.profile_combo.addItem(label, profile.get("profile_id", ""))

        self.log(
            "Connected to {}. Dataset '{}' with {} loaded profile(s).".format(
                self.api_base_url(), dataset.get("dataset_id", "unknown"), len(profiles)
            )
        )

    def build_route_request(self):
        return {
            "route_id": self.route_id_edit.text().strip(),
            "origin": {
                "id": self.origin_id_edit.text().strip(),
                "lon": float(self.origin_lon_edit.text().strip()),
                "lat": float(self.origin_lat_edit.text().strip()),
            },
            "destination": {
                "id": self.destination_id_edit.text().strip(),
                "lon": float(self.destination_lon_edit.text().strip()),
                "lat": float(self.destination_lat_edit.text().strip()),
            },
            "snap": {"max_distance_m": float(self.snap_distance_edit.text().strip())},
            "returns": {
                "geometry": "full",
                "segment_rows": True,
                "road_type_breakdown": ["time_s", "distance_m"],
                "surface_breakdown": ["time_s", "distance_m"],
                "penalty_breakdown": True,
                "explain_cost_derivation": True,
            },
        }

    def write_route_request(self):
        try:
            request = self.build_route_request()
        except ValueError as exc:
            self.alert("Invalid route request values: {}".format(exc))
            return

        request_path = self.resolve_local_path(self.route_request_path_edit.text())
        request_path.parent.mkdir(parents=True, exist_ok=True)
        request_path.write_text(json.dumps(request, indent=2), encoding="utf-8")
        self.log("Wrote route request to {}".format(request_path))

    def run_route(self):
        if self.service_info is None:
            self.alert("Refresh the API service first.")
            return
        try:
            payload = {"request": self.build_route_request()}
        except ValueError as exc:
            self.alert("Invalid route request values: {}".format(exc))
            return

        profile_id = self.selected_profile_id()
        if profile_id:
            payload["profile_id"] = profile_id

        self.execute_api_request(
            endpoint="/v1/route",
            payload=payload,
            output_path=self.route_output_path_edit.text(),
            layer_name=payload["request"]["route_id"] or "netan_route",
            analysis_kind="route",
        )

    def run_od(self):
        if self.service_info is None:
            self.alert("Refresh the API service first.")
            return
        try:
            document = self.load_od_document(self.resolve_local_path(self.od_pairs_path_edit.text()))
        except Exception as exc:
            self.alert("Failed to load OD input: {}".format(exc))
            return

        payload = {"request": document}
        profile_id = self.selected_profile_id()
        if profile_id:
            payload["profile_id"] = profile_id

        self.execute_api_request(
            endpoint="/v1/od",
            payload=payload,
            output_path=self.od_output_path_edit.text(),
            layer_name="netan_od",
            analysis_kind="od",
        )

    def run_matrix(self):
        if self.service_info is None:
            self.alert("Refresh the API service first.")
            return
        try:
            origins = self.load_point_set_document(
                self.resolve_local_path(self.matrix_origins_path_edit.text())
            )
            destinations = self.load_point_set_document(
                self.resolve_local_path(self.matrix_destinations_path_edit.text())
            )
        except Exception as exc:
            self.alert("Failed to load matrix input: {}".format(exc))
            return

        payload = {"request": {"origins": origins, "destinations": destinations}}
        profile_id = self.selected_profile_id()
        if profile_id:
            payload["profile_id"] = profile_id

        self.execute_api_request(
            endpoint="/v1/matrix",
            payload=payload,
            output_path=self.matrix_output_path_edit.text(),
            layer_name="netan_matrix",
            analysis_kind="matrix",
        )

    def execute_api_request(self, endpoint, payload, output_path, layer_name, analysis_kind):
        url = self.service_url(endpoint, include_format=True)
        self.log("POST {}".format(url))
        try:
            content_type, body = self.http_post_json(url, payload)
        except Exception as exc:
            self.alert("API request failed: {}".format(exc))
            return

        local_output_path = self.resolve_local_path(output_path)
        saved_path = self.save_response(local_output_path, content_type, body)
        self.log("Saved API response to {}".format(saved_path))

        if "geo+json" in content_type or saved_path.suffix.lower() == ".geojson":
            self.load_output_layer(saved_path, layer_name)
            return

        try:
            response_json = json.loads(body.decode("utf-8"))
        except Exception as exc:
            self.alert("Failed to parse API JSON response: {}".format(exc))
            return

        geojson = self.analysis_json_to_geojson(analysis_kind, response_json)
        if geojson is None:
            self.log(
                "Response saved, but no spatial geometry could be built from the API result.",
                Qgis.Warning,
            )
            return

        temp_path = self.write_temp_geojson(layer_name, geojson)
        self.load_output_layer(temp_path, layer_name)

    def load_od_document(self, path):
        suffix = path.suffix.lower()
        if suffix == ".csv":
            return self.load_od_csv(path)
        if suffix == ".json":
            parsed = self.read_json(path)
            if isinstance(parsed, dict):
                if "pairs" in parsed:
                    parsed.setdefault("snap", {"max_distance_m": 500.0})
                    parsed.setdefault("returns", {"geometry": "full"})
                    return parsed
            if isinstance(parsed, list):
                return {
                    "pairs": parsed,
                    "snap": {"max_distance_m": 500.0},
                    "returns": {"geometry": "full"},
                }
        raise ValueError("OD input must be .csv or .json for API mode.")

    def load_point_set_document(self, path):
        suffix = path.suffix.lower()
        if suffix == ".csv":
            return self.load_point_set_csv(path)
        if suffix == ".json":
            parsed = self.read_json(path)
            if isinstance(parsed, dict):
                if "points" in parsed:
                    parsed.setdefault("snap", {"max_distance_m": 500.0})
                    parsed.setdefault("returns", {"geometry": "full"})
                    return parsed
            if isinstance(parsed, list):
                return {
                    "points": parsed,
                    "snap": {"max_distance_m": 500.0},
                    "returns": {"geometry": "full"},
                }
        raise ValueError("Point-set input must be .csv or .json for API mode.")

    def load_od_csv(self, path):
        with path.open("r", encoding="utf-8", newline="") as handle:
            reader = csv.DictReader(handle)
            pairs = []
            for row in reader:
                pair_id = self.first_value(row, ["id", "pair_id"])
                source_x = float(
                    self.first_value(row, ["source_x", "source_lon", "origin_x", "origin_lon"])
                )
                source_y = float(
                    self.first_value(row, ["source_y", "source_lat", "origin_y", "origin_lat"])
                )
                target_x = float(
                    self.first_value(
                        row,
                        ["target_x", "target_lon", "destination_x", "destination_lon"],
                    )
                )
                target_y = float(
                    self.first_value(
                        row,
                        ["target_y", "target_lat", "destination_y", "destination_lat"],
                    )
                )
                pairs.append(
                    {
                        "pair_id": pair_id,
                        "origin": {
                            "id": "{}:source".format(pair_id),
                            "lon": source_x,
                            "lat": source_y,
                        },
                        "destination": {
                            "id": "{}:target".format(pair_id),
                            "lon": target_x,
                            "lat": target_y,
                        },
                    }
                )
        return {
            "pairs": pairs,
            "snap": {"max_distance_m": 500.0},
            "returns": {"geometry": "full"},
        }

    def load_point_set_csv(self, path):
        with path.open("r", encoding="utf-8", newline="") as handle:
            reader = csv.DictReader(handle)
            points = []
            for row in reader:
                points.append(
                    {
                        "id": self.first_value(row, ["id"]),
                        "lon": float(self.first_value(row, ["x", "lon", "longitude"])),
                        "lat": float(self.first_value(row, ["y", "lat", "latitude"])),
                    }
                )
        return {
            "points": points,
            "snap": {"max_distance_m": 500.0},
            "returns": {"geometry": "full"},
        }

    def first_value(self, row, accepted_columns):
        for column in accepted_columns:
            value = row.get(column)
            if value is not None and str(value).strip():
                return str(value).strip()
        raise ValueError(
            "Missing required column. Expected one of: {}".format(
                ", ".join(accepted_columns)
            )
        )

    def read_json(self, path):
        with path.open("r", encoding="utf-8") as handle:
            return json.load(handle)

    def http_get_json(self, url):
        request = urllib.request.Request(url, headers={"Accept": "application/json"})
        with urllib.request.urlopen(request, timeout=self.timeout_seconds()) as response:
            body = response.read().decode("utf-8")
            return json.loads(body)

    def http_post_json(self, url, payload):
        body = json.dumps(payload).encode("utf-8")
        request = urllib.request.Request(
            url,
            data=body,
            headers={
                "Content-Type": "application/json",
                "Accept": "application/geo+json, application/json",
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout_seconds()) as response:
                return response.headers.get_content_type(), response.read()
        except urllib.error.HTTPError as exc:
            error_body = exc.read().decode("utf-8", errors="replace")
            try:
                parsed = json.loads(error_body)
                message = parsed.get("error") or error_body
            except Exception:
                message = error_body or str(exc)
            raise RuntimeError(message)

    def save_response(self, output_path, content_type, body):
        output_path.parent.mkdir(parents=True, exist_ok=True)
        if "geo+json" in content_type:
            if output_path.suffix.lower() != ".geojson":
                output_path = output_path.with_suffix(".geojson")
            output_path.write_bytes(body)
            return output_path

        if output_path.suffix.lower() != ".json":
            output_path = output_path.with_suffix(".json")
        output_path.write_bytes(body)
        return output_path

    def analysis_json_to_geojson(self, analysis_kind, response_json):
        service = response_json.get("service", {})
        result = response_json.get("result", {})
        if analysis_kind == "route":
            geometry = result.get("geometry")
            if not geometry:
                return None
            return {
                "type": "FeatureCollection",
                "features": [
                    {
                        "type": "Feature",
                        "geometry": {"type": "LineString", "coordinates": geometry},
                        "properties": {
                            "dataset_id": service.get("dataset_id"),
                            "profile_id": service.get("profile_id"),
                            "profile_hash": service.get("profile_hash"),
                            "route_id": result.get("route_id"),
                            "total_distance_m": result.get("summary", {}).get("total_distance_m"),
                            "total_travel_time_s": result.get("summary", {}).get(
                                "total_travel_time_s"
                            ),
                            "total_generalized_cost": result.get("summary", {}).get(
                                "total_generalized_cost"
                            ),
                            "segment_count": result.get("summary", {}).get("segment_count"),
                            "origin_point_id": result.get("origin", {}).get("point_id"),
                            "destination_point_id": result.get("destination", {}).get("point_id"),
                            "origin_snap_distance_m": result.get("origin", {}).get(
                                "snap_distance_m"
                            ),
                            "destination_snap_distance_m": result.get("destination", {}).get(
                                "snap_distance_m"
                            ),
                        },
                    }
                ],
            }

        items = result.get("pairs") if analysis_kind == "od" else result.get("cells")
        if not isinstance(items, list):
            return None

        features = []
        for item in items:
            features.append(
                {
                    "type": "Feature",
                    "geometry": self.item_geometry(item.get("geometry")),
                    "properties": {
                        "dataset_id": service.get("dataset_id"),
                        "profile_id": service.get("profile_id"),
                        "profile_hash": service.get("profile_hash"),
                        "pair_id": item.get("pair_id"),
                        "origin_id": item.get("origin_id"),
                        "destination_id": item.get("destination_id"),
                        "status": item.get("status"),
                        "origin_snap_distance_m": item.get("origin_snap_distance_m"),
                        "destination_snap_distance_m": item.get("destination_snap_distance_m"),
                        "total_distance_m": item.get("total_distance_m"),
                        "total_travel_time_s": item.get("total_travel_time_s"),
                        "total_generalized_cost": item.get("total_generalized_cost"),
                        "error": item.get("error"),
                    },
                }
            )

        return {"type": "FeatureCollection", "features": features}

    def item_geometry(self, coordinates):
        if not coordinates:
            return None
        return {"type": "LineString", "coordinates": coordinates}

    def write_temp_geojson(self, layer_name, geojson):
        safe_name = layer_name.replace(" ", "_") or "netan_layer"
        path = self.temp_layers_dir / "{}.geojson".format(safe_name)
        path.write_text(json.dumps(geojson, indent=2), encoding="utf-8")
        return path

    def load_output_layer(self, output_path, layer_name=None):
        output_path = Path(output_path)
        layer = QgsVectorLayer(str(output_path), layer_name or output_path.stem, "ogr")
        if not layer.isValid():
            self.log("Failed to load layer {}".format(output_path), Qgis.Warning)
            return
        QgsProject.instance().addMapLayer(layer)
        self.log("Loaded layer {}".format(output_path))

    def log(self, message, level=Qgis.Info):
        QgsMessageLog.logMessage(message, "netan", level)
        self.log_output.appendPlainText(message)

    def alert(self, message):
        QMessageBox.warning(self, "netan", message)
        self.log(message, Qgis.Warning)
