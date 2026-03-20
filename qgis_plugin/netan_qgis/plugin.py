import csv
import json
import tempfile
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from qgis.PyQt.QtCore import QSettings, Qt, QTimer
from qgis.PyQt.QtGui import QColor
from qgis.PyQt.QtWidgets import (
    QAction,
    QCheckBox,
    QComboBox,
    QDockWidget,
    QFileDialog,
    QFormLayout,
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
from qgis.core import (
    Qgis,
    QgsCoordinateReferenceSystem,
    QgsCoordinateTransform,
    QgsFeatureRequest,
    QgsMapLayerProxyModel,
    QgsMessageLog,
    QgsPointXY,
    QgsProject,
    QgsVectorLayer,
    QgsWkbTypes,
)
from qgis.gui import QgsMapLayerComboBox, QgsMapToolEmitPoint, QgsVertexMarker


PLUGIN_MENU = "&netan"
SETTINGS_PREFIX = "netan_qgis"


class ResponseFormat:
    JSON = "json"
    GEOJSON = "geojson"


class MatrixSourceMode:
    LAYER = "layer"
    FILE = "file"


class PickTarget:
    ORIGIN = "origin"
    DESTINATION = "destination"


def qt_enum_value(owner, scoped_enum_name, member_name):
    if hasattr(owner, member_name):
        return getattr(owner, member_name)
    scoped_enum = getattr(owner, scoped_enum_name, None)
    if scoped_enum is not None and hasattr(scoped_enum, member_name):
        return getattr(scoped_enum, member_name)
    raise AttributeError(
        "{} has no enum member {} or {}.{}".format(
            owner, member_name, scoped_enum_name, member_name
        )
    )


def qt_dock_area(member_name):
    return qt_enum_value(Qt, "DockWidgetArea", member_name)


def qt_widget_attribute(member_name):
    return qt_enum_value(Qt, "WidgetAttribute", member_name)


def dock_widget_feature(member_name):
    return qt_enum_value(QDockWidget, "DockWidgetFeature", member_name)


class NetanPlugin:
    def __init__(self, iface):
        self.iface = iface
        self.action = None
        self.dock = None

    def initGui(self):
        self.action = QAction("netan", self.iface.mainWindow())
        self.action.setCheckable(True)
        self.action.triggered.connect(self.toggle_dock)
        self.iface.addPluginToMenu(PLUGIN_MENU, self.action)
        self.iface.addToolBarIcon(self.action)

    def unload(self):
        if self.dock is not None:
            self.dock.save_settings()
            self.dock.cleanup()
            self.iface.removeDockWidget(self.dock)
            self.dock.deleteLater()
            self.dock = None
        if self.action is not None:
            self.iface.removePluginMenu(PLUGIN_MENU, self.action)
            self.iface.removeToolBarIcon(self.action)

    def toggle_dock(self, checked=False):
        if self.dock is None:
            self.dock = NetanDock(self.iface, self.action)
            self.dock.visibilityChanged.connect(self.sync_action_state)
            self.iface.addDockWidget(qt_dock_area("RightDockWidgetArea"), self.dock)
        self.dock.setVisible(bool(checked))
        if checked:
            self.dock.raise_()

    def sync_action_state(self, visible):
        if self.action is None:
            return
        was_blocked = self.action.blockSignals(True)
        self.action.setChecked(bool(visible))
        self.action.blockSignals(was_blocked)


class NetanDock(QDockWidget):
    def __init__(self, iface, action):
        super().__init__("netan", iface.mainWindow())
        self.iface = iface
        self.action = action
        self.service_info = None
        self.temp_layers_dir = Path(tempfile.gettempdir()) / "netan_qgis_layers"
        self.temp_layers_dir.mkdir(parents=True, exist_ok=True)
        self.wgs84 = QgsCoordinateReferenceSystem("EPSG:4326")
        self.point_picker_tool = None
        self.previous_map_tool = None
        self.pick_target = None
        self.origin_marker = None
        self.destination_marker = None

        self.setObjectName("netanDock")
        self.setAllowedAreas(
            qt_dock_area("LeftDockWidgetArea") | qt_dock_area("RightDockWidgetArea")
        )
        self.setFeatures(
            dock_widget_feature("DockWidgetClosable")
            | dock_widget_feature("DockWidgetMovable")
            | dock_widget_feature("DockWidgetFloatable")
        )
        self.setMinimumWidth(420)
        self.setAutoFillBackground(True)

        container = self._build_ui()
        container.setAutoFillBackground(True)
        container.setAttribute(qt_widget_attribute("WA_StyledBackground"), True)
        container.setStyleSheet("background-color: palette(window);")
        self.setWidget(container)

        self.load_settings()
        self.update_point_markers()
        QTimer.singleShot(0, lambda: self.refresh_service(manual=False))

    def _build_ui(self):
        container = QWidget()
        layout = QVBoxLayout(container)
        layout.setContentsMargins(8, 8, 8, 8)

        self.tabs = QTabWidget()
        self.tabs.addTab(self._build_api_tab(), "API")
        self.tabs.addTab(self._build_route_tab(), "Route")
        self.tabs.addTab(self._build_batch_tab(), "Batch")
        layout.addWidget(self.tabs)

        self.log_output = QPlainTextEdit()
        self.log_output.setReadOnly(True)
        self.log_output.setPlaceholderText("API and plugin log output.")
        self.log_output.document().setMaximumBlockCount(200)
        layout.addWidget(self.log_output, stretch=1)

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
        self.service_status_label = QLabel("Service status: not checked yet.")
        self.service_status_label.setWordWrap(True)

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
        layout.addWidget(self.service_status_label)

        button_row = QHBoxLayout()
        refresh_button = QPushButton("Refresh Service")
        refresh_button.clicked.connect(lambda: self.refresh_service(manual=True))
        zoom_button = QPushButton("Zoom To Dataset")
        zoom_button.clicked.connect(self.zoom_to_dataset_bounds)
        button_row.addWidget(refresh_button)
        button_row.addWidget(zoom_button)
        button_row.addStretch(1)
        layout.addLayout(button_row)

        description = QLabel(
            "This plugin talks directly to the running netan API. "
            "Pick route points from the map canvas, or build batch analyses from QGIS layers."
        )
        description.setWordWrap(True)
        layout.addWidget(description)
        layout.addStretch(1)
        return tab

    def _build_route_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        summary = QLabel(
            "Pick a start and end point from the map canvas or pull them from a selected point feature. "
            "Coordinates are stored and sent to the API in WGS84 longitude and latitude."
        )
        summary.setWordWrap(True)
        layout.addWidget(summary)

        options_group = QGroupBox("Route options")
        options_form = QFormLayout(options_group)
        self.route_id_edit = QLineEdit("qgis_route_001")
        self.snap_distance_edit = QLineEdit("500")
        self.route_output_path_edit = QLineEdit(".netan/runs/qgis-route.geojson")
        self.route_request_path_edit = QLineEdit("examples/requests/route_from_qgis.json")
        options_form.addRow("Route id", self.route_id_edit)
        options_form.addRow("Snap distance m", self.snap_distance_edit)
        options_form.addRow(
            "Response path",
            self._line_with_browse(
                self.route_output_path_edit, browse_dir=False, save_dialog=True
            ),
        )
        options_form.addRow(
            "Optional request JSON",
            self._line_with_browse(self.route_request_path_edit, browse_dir=False, save_dialog=True),
        )
        layout.addWidget(options_group)

        self.pick_status_label = QLabel("Pick Start or Pick End, then click on the map.")
        self.pick_status_label.setWordWrap(True)
        layout.addWidget(self.pick_status_label)

        layout.addWidget(self._build_route_point_group("Start", PickTarget.ORIGIN))
        layout.addWidget(self._build_route_point_group("End", PickTarget.DESTINATION))

        button_row = QHBoxLayout()
        pick_origin_button = QPushButton("Pick Start")
        pick_origin_button.clicked.connect(lambda: self.begin_point_pick(PickTarget.ORIGIN))
        pick_destination_button = QPushButton("Pick End")
        pick_destination_button.clicked.connect(
            lambda: self.begin_point_pick(PickTarget.DESTINATION)
        )
        swap_button = QPushButton("Swap")
        swap_button.clicked.connect(self.swap_route_points)
        clear_button = QPushButton("Clear")
        clear_button.clicked.connect(self.clear_route_points)
        save_request_button = QPushButton("Save Request")
        save_request_button.clicked.connect(self.write_route_request)
        run_route_button = QPushButton("Run Route")
        run_route_button.clicked.connect(self.run_route)
        button_row.addWidget(pick_origin_button)
        button_row.addWidget(pick_destination_button)
        button_row.addWidget(swap_button)
        button_row.addWidget(clear_button)
        button_row.addStretch(1)
        button_row.addWidget(save_request_button)
        button_row.addWidget(run_route_button)
        layout.addLayout(button_row)

        layout.addStretch(1)
        return tab

    def _build_route_point_group(self, title, target):
        group = QGroupBox(title)
        layout = QVBoxLayout(group)

        form = QFormLayout()
        point_id_edit = QLineEdit("origin" if target == PickTarget.ORIGIN else "destination")
        lon_edit = QLineEdit("")
        lat_edit = QLineEdit("")
        if target == PickTarget.ORIGIN:
            self.origin_id_edit = point_id_edit
            self.origin_lon_edit = lon_edit
            self.origin_lat_edit = lat_edit
        else:
            self.destination_id_edit = point_id_edit
            self.destination_lon_edit = lon_edit
            self.destination_lat_edit = lat_edit

        form.addRow("{} id".format(title), point_id_edit)
        form.addRow("{} lon".format(title), lon_edit)
        form.addRow("{} lat".format(title), lat_edit)
        lon_edit.editingFinished.connect(self.update_point_markers)
        lat_edit.editingFinished.connect(self.update_point_markers)
        layout.addLayout(form)

        button_row = QHBoxLayout()
        pick_button = QPushButton("Pick On Map")
        pick_button.clicked.connect(lambda: self.begin_point_pick(target))
        selected_button = QPushButton("From Selected Feature")
        selected_button.clicked.connect(lambda: self.use_selected_feature_for_target(target))
        button_row.addWidget(pick_button)
        button_row.addWidget(selected_button)
        button_row.addStretch(1)
        layout.addLayout(button_row)
        return group

    def _build_batch_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        od_group = QGroupBox("OD")
        od_form = QFormLayout(od_group)
        self.od_pairs_path_edit = QLineEdit("examples/requests/od_pairs.csv")
        self.od_output_path_edit = QLineEdit(".netan/runs/qgis-od.geojson")
        od_form.addRow(
            "Pairs file",
            self._line_with_browse(self.od_pairs_path_edit, browse_dir=False),
        )
        od_form.addRow(
            "Response path",
            self._line_with_browse(
                self.od_output_path_edit, browse_dir=False, save_dialog=True
            ),
        )
        run_od_button = QPushButton("Run OD")
        run_od_button.clicked.connect(self.run_od)
        od_form.addRow(run_od_button)

        matrix_group = QGroupBox("Matrix")
        matrix_layout = QVBoxLayout(matrix_group)
        matrix_note = QLabel(
            "Build matrix origins and destinations from point layers already loaded in QGIS, "
            "or fall back to CSV/JSON files."
        )
        matrix_note.setWordWrap(True)
        matrix_layout.addWidget(matrix_note)
        matrix_layout.addWidget(self._build_matrix_source_group("Origins", is_origin=True))
        matrix_layout.addWidget(self._build_matrix_source_group("Destinations", is_origin=False))

        matrix_form = QFormLayout()
        self.matrix_output_path_edit = QLineEdit(".netan/runs/qgis-matrix.geojson")
        matrix_form.addRow(
            "Response path",
            self._line_with_browse(
                self.matrix_output_path_edit, browse_dir=False, save_dialog=True
            ),
        )
        run_matrix_button = QPushButton("Run Matrix")
        run_matrix_button.clicked.connect(self.run_matrix)
        matrix_form.addRow(run_matrix_button)
        matrix_layout.addLayout(matrix_form)

        layout.addWidget(od_group)
        layout.addWidget(matrix_group)
        layout.addStretch(1)
        return tab

    def _build_matrix_source_group(self, title, is_origin):
        group = QGroupBox(title)
        form = QFormLayout(group)

        mode_combo = QComboBox()
        mode_combo.addItem("Loaded point layer", MatrixSourceMode.LAYER)
        mode_combo.addItem("CSV or JSON file", MatrixSourceMode.FILE)

        layer_combo = QgsMapLayerComboBox()
        layer_combo.setFilters(QgsMapLayerProxyModel.PointLayer)
        id_field_combo = QComboBox()
        selected_only_check = QCheckBox("Use selected features only")
        file_path_edit = QLineEdit(
            "examples/requests/matrix_origins.csv"
            if is_origin
            else "examples/requests/matrix_destinations.csv"
        )
        file_widget = self._line_with_browse(file_path_edit, browse_dir=False)
        point_layer_label = QLabel("Point layer")
        id_field_label = QLabel("ID field")
        file_label = QLabel("File")

        form.addRow(QLabel("Source"), mode_combo)
        form.addRow(point_layer_label, layer_combo)
        form.addRow(id_field_label, id_field_combo)
        form.addRow("", selected_only_check)
        form.addRow(file_label, file_widget)

        if is_origin:
            self.matrix_origins_mode_combo = mode_combo
            self.matrix_origins_layer_combo = layer_combo
            self.matrix_origins_id_field_combo = id_field_combo
            self.matrix_origins_selected_only_check = selected_only_check
            self.matrix_origins_path_edit = file_path_edit
            self.matrix_origins_point_layer_label = point_layer_label
            self.matrix_origins_id_field_label = id_field_label
            self.matrix_origins_file_label = file_label
        else:
            self.matrix_destinations_mode_combo = mode_combo
            self.matrix_destinations_layer_combo = layer_combo
            self.matrix_destinations_id_field_combo = id_field_combo
            self.matrix_destinations_selected_only_check = selected_only_check
            self.matrix_destinations_path_edit = file_path_edit
            self.matrix_destinations_point_layer_label = point_layer_label
            self.matrix_destinations_id_field_label = id_field_label
            self.matrix_destinations_file_label = file_label

        layer_combo.layerChanged.connect(
            lambda _layer=None, combo=layer_combo, field_combo=id_field_combo: self.populate_id_fields(
                combo.currentLayer(), field_combo
            )
        )
        mode_combo.currentIndexChanged.connect(
            lambda _index, combo=mode_combo, layer=layer_combo, layer_label=point_layer_label, field=id_field_combo, field_label=id_field_label, selected=selected_only_check, file_row=file_widget, file_label_widget=file_label: self.update_matrix_source_visibility(
                combo, layer, layer_label, field, field_label, selected, file_row, file_label_widget
            )
        )

        self.populate_id_fields(layer_combo.currentLayer(), id_field_combo)
        self.update_matrix_source_visibility(
            mode_combo,
            layer_combo,
            point_layer_label,
            id_field_combo,
            id_field_label,
            selected_only_check,
            file_widget,
            file_label,
        )
        return group

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
            self, "Select directory", self.file_dialog_root()
        )
        if chosen:
            line_edit.setText(chosen)

    def _browse_file(self, line_edit):
        chosen, _ = QFileDialog.getOpenFileName(
            self, "Select file", self.file_dialog_root()
        )
        if chosen:
            line_edit.setText(chosen)

    def _browse_save_file(self, line_edit):
        chosen, _ = QFileDialog.getSaveFileName(
            self, "Select output path", self.file_dialog_root()
        )
        if chosen:
            line_edit.setText(chosen)

    def file_dialog_root(self):
        root = self.workspace_root()
        return str(root if root.exists() else Path.home())

    def workspace_root(self):
        raw = self.workspace_root_edit.text().strip()
        if not raw:
            return Path(__file__).resolve().parents[2]
        return Path(raw).expanduser()

    def resolve_local_path(self, raw):
        value = raw.strip()
        if not value:
            return self.workspace_root()
        path = Path(value).expanduser()
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

    def refresh_service(self, manual=False):
        try:
            service = self.http_get_json(self.service_url("/v1/service"))
        except Exception as exc:
            self.service_info = None
            self.dataset_id_edit.setText("")
            self.default_profile_edit.setText("")
            self.dataset_bounds_edit.setText("")
            self.profile_combo.clear()
            self.service_status_label.setText(
                "Service status: unavailable at {}.".format(self.api_base_url() or "configured URL")
            )
            if manual:
                self.log("Failed to refresh service: {}".format(exc), Qgis.Warning)
            else:
                self.log(
                    "netan service not reachable during startup: {}".format(exc), Qgis.Info
                )
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

        previous_profile_id = self.selected_profile_id() or self.read_setting("profile_id", "")

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

        desired_index = self.profile_combo.findData(previous_profile_id)
        if desired_index >= 0:
            self.profile_combo.setCurrentIndex(desired_index)

        self.service_status_label.setText(
            "Service status: connected to {} with dataset '{}'.".format(
                self.api_base_url(), dataset.get("dataset_id", "unknown")
            )
        )
        self.log(
            "Connected to {}. Dataset '{}' with {} loaded profile(s).".format(
                self.api_base_url(), dataset.get("dataset_id", "unknown"), len(profiles)
            )
        )

    def zoom_to_dataset_bounds(self):
        if self.service_info is None:
            self.alert("Refresh the API service first.")
            return
        dataset = self.service_info.get("dataset", {})
        bounds = dataset.get("topology_bounds") or {}
        if not bounds:
            self.alert("The service did not report dataset bounds.")
            return

        try:
            transform = QgsCoordinateTransform(
                self.wgs84,
                self.iface.mapCanvas().mapSettings().destinationCrs(),
                QgsProject.instance(),
            )
            lower_left = transform.transform(
                QgsPointXY(bounds["min_lon"], bounds["min_lat"])
            )
            upper_right = transform.transform(
                QgsPointXY(bounds["max_lon"], bounds["max_lat"])
            )
        except Exception as exc:
            self.alert("Failed to transform dataset bounds: {}".format(exc))
            return

        extent = self.iface.mapCanvas().extent()
        extent.setXMinimum(min(lower_left.x(), upper_right.x()))
        extent.setYMinimum(min(lower_left.y(), upper_right.y()))
        extent.setXMaximum(max(lower_left.x(), upper_right.x()))
        extent.setYMaximum(max(lower_left.y(), upper_right.y()))
        self.iface.mapCanvas().setExtent(extent)
        self.iface.mapCanvas().refresh()

    def begin_point_pick(self, target):
        self.ensure_point_picker_tool()
        self.pick_target = target
        current_tool = self.iface.mapCanvas().mapTool()
        if current_tool != self.point_picker_tool:
            self.previous_map_tool = current_tool
        self.iface.mapCanvas().setMapTool(self.point_picker_tool)
        label = "start" if target == PickTarget.ORIGIN else "end"
        self.pick_status_label.setText("Click the {} point on the map.".format(label))

    def ensure_point_picker_tool(self):
        if self.point_picker_tool is not None:
            return
        self.point_picker_tool = QgsMapToolEmitPoint(self.iface.mapCanvas())
        self.point_picker_tool.canvasClicked.connect(self.handle_canvas_click)

    def handle_canvas_click(self, point, _button):
        if self.pick_target is None:
            return
        try:
            transform = QgsCoordinateTransform(
                self.iface.mapCanvas().mapSettings().destinationCrs(),
                self.wgs84,
                QgsProject.instance(),
            )
            wgs84_point = transform.transform(point)
        except Exception as exc:
            self.alert("Failed to capture map point: {}".format(exc))
            self.finish_point_pick()
            return

        self.set_route_point(
            self.pick_target,
            lon=wgs84_point.x(),
            lat=wgs84_point.y(),
            point_id=self.default_point_id(self.pick_target),
        )
        label = "start" if self.pick_target == PickTarget.ORIGIN else "end"
        self.pick_status_label.setText(
            "Set the {} point from the map canvas.".format(label)
        )
        self.finish_point_pick()

    def finish_point_pick(self):
        if self.previous_map_tool is not None:
            self.iface.mapCanvas().setMapTool(self.previous_map_tool)
        self.previous_map_tool = None
        self.pick_target = None

    def use_selected_feature_for_target(self, target):
        layer = self.iface.activeLayer()
        if layer is None:
            self.alert("Select a point layer and one feature first.")
            return
        if QgsWkbTypes.geometryType(layer.wkbType()) != QgsWkbTypes.PointGeometry:
            self.alert("The active layer must be a point layer.")
            return

        selected_ids = layer.selectedFeatureIds()
        if len(selected_ids) != 1:
            self.alert("Select exactly one point feature in the active layer.")
            return

        request = QgsFeatureRequest().setFilterFid(selected_ids[0])
        feature = None
        for candidate in layer.getFeatures(request):
            feature = candidate
            break
        if feature is None:
            self.alert("Failed to read the selected feature.")
            return

        try:
            point = self.feature_point(feature)
            transform = QgsCoordinateTransform(layer.crs(), self.wgs84, QgsProject.instance())
            wgs84_point = transform.transform(point)
        except Exception as exc:
            self.alert("Failed to read selected feature geometry: {}".format(exc))
            return

        point_id = self.feature_label(feature, target)
        self.set_route_point(target, wgs84_point.x(), wgs84_point.y(), point_id)
        label = "start" if target == PickTarget.ORIGIN else "end"
        self.pick_status_label.setText(
            "Set the {} point from the selected feature in '{}'.".format(
                label, layer.name()
            )
        )

    def feature_point(self, feature):
        geometry = feature.geometry()
        if geometry is None or geometry.isEmpty():
            raise ValueError("selected feature has no point geometry")
        if geometry.isMultipart():
            points = geometry.asMultiPoint()
            if not points:
                raise ValueError("selected feature has no point geometry")
            point = points[0]
        else:
            point = geometry.asPoint()
        return QgsPointXY(point.x(), point.y())

    def feature_label(self, feature, fallback_prefix):
        for candidate in ["id", "name", "label", "fid"]:
            index = feature.fields().indexFromName(candidate)
            if index >= 0:
                value = feature.attributes()[index]
                if value is not None and str(value).strip():
                    return str(value).strip()
        return "{}_{}".format(fallback_prefix, feature.id())

    def default_point_id(self, target):
        if target == PickTarget.ORIGIN:
            existing = self.origin_id_edit.text().strip()
            return existing or "origin"
        existing = self.destination_id_edit.text().strip()
        return existing or "destination"

    def set_route_point(self, target, lon, lat, point_id):
        if target == PickTarget.ORIGIN:
            self.origin_id_edit.setText(point_id)
            self.origin_lon_edit.setText("{:.6f}".format(lon))
            self.origin_lat_edit.setText("{:.6f}".format(lat))
        else:
            self.destination_id_edit.setText(point_id)
            self.destination_lon_edit.setText("{:.6f}".format(lon))
            self.destination_lat_edit.setText("{:.6f}".format(lat))
        self.update_point_markers()

    def swap_route_points(self):
        origin = (
            self.origin_id_edit.text(),
            self.origin_lon_edit.text(),
            self.origin_lat_edit.text(),
        )
        destination = (
            self.destination_id_edit.text(),
            self.destination_lon_edit.text(),
            self.destination_lat_edit.text(),
        )
        self.origin_id_edit.setText(destination[0])
        self.origin_lon_edit.setText(destination[1])
        self.origin_lat_edit.setText(destination[2])
        self.destination_id_edit.setText(origin[0])
        self.destination_lon_edit.setText(origin[1])
        self.destination_lat_edit.setText(origin[2])
        self.update_point_markers()

    def clear_route_points(self):
        for widget in [
            self.origin_id_edit,
            self.origin_lon_edit,
            self.origin_lat_edit,
            self.destination_id_edit,
            self.destination_lon_edit,
            self.destination_lat_edit,
        ]:
            widget.clear()
        self.pick_status_label.setText("Pick Start or Pick End, then click on the map.")
        self.update_point_markers()

    def update_point_markers(self):
        self.update_point_marker(
            self.origin_marker,
            self.origin_lon_edit.text().strip(),
            self.origin_lat_edit.text().strip(),
            QColor(33, 150, 83),
            PickTarget.ORIGIN,
        )
        self.update_point_marker(
            self.destination_marker,
            self.destination_lon_edit.text().strip(),
            self.destination_lat_edit.text().strip(),
            QColor(196, 57, 43),
            PickTarget.DESTINATION,
        )

    def update_point_marker(self, existing_marker, lon_text, lat_text, color, target):
        marker = existing_marker
        if marker is None:
            marker = QgsVertexMarker(self.iface.mapCanvas())
            marker.setIconType(QgsVertexMarker.ICON_CROSS)
            marker.setIconSize(16)
            marker.setPenWidth(3)
            marker.setColor(color)
            if target == PickTarget.ORIGIN:
                self.origin_marker = marker
            else:
                self.destination_marker = marker

        try:
            lon = float(lon_text)
            lat = float(lat_text)
        except ValueError:
            marker.hide()
            return

        try:
            transform = QgsCoordinateTransform(
                self.wgs84,
                self.iface.mapCanvas().mapSettings().destinationCrs(),
                QgsProject.instance(),
            )
            canvas_point = transform.transform(QgsPointXY(lon, lat))
        except Exception:
            marker.hide()
            return

        marker.setCenter(canvas_point)
        marker.show()

    def build_route_request(self):
        route_id = self.route_id_edit.text().strip() or "qgis_route"
        return {
            "route_id": route_id,
            "origin": {
                "id": self.origin_id_edit.text().strip() or "origin",
                "lon": float(self.origin_lon_edit.text().strip()),
                "lat": float(self.origin_lat_edit.text().strip()),
            },
            "destination": {
                "id": self.destination_id_edit.text().strip() or "destination",
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
        self.save_settings()

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

        self.save_settings()
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
            document = self.load_od_document(
                self.resolve_local_path(self.od_pairs_path_edit.text())
            )
        except Exception as exc:
            self.alert("Failed to load OD input: {}".format(exc))
            return

        payload = {"request": document}
        profile_id = self.selected_profile_id()
        if profile_id:
            payload["profile_id"] = profile_id

        self.save_settings()
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
            origins = self.load_matrix_source(
                self.matrix_origins_mode_combo,
                self.matrix_origins_layer_combo,
                self.matrix_origins_id_field_combo,
                self.matrix_origins_selected_only_check,
                self.matrix_origins_path_edit,
            )
            destinations = self.load_matrix_source(
                self.matrix_destinations_mode_combo,
                self.matrix_destinations_layer_combo,
                self.matrix_destinations_id_field_combo,
                self.matrix_destinations_selected_only_check,
                self.matrix_destinations_path_edit,
            )
        except Exception as exc:
            self.alert("Failed to load matrix input: {}".format(exc))
            return

        payload = {"request": {"origins": origins, "destinations": destinations}}
        profile_id = self.selected_profile_id()
        if profile_id:
            payload["profile_id"] = profile_id

        self.save_settings()
        self.execute_api_request(
            endpoint="/v1/matrix",
            payload=payload,
            output_path=self.matrix_output_path_edit.text(),
            layer_name="netan_matrix",
            analysis_kind="matrix",
        )

    def load_matrix_source(
        self, mode_combo, layer_combo, id_field_combo, selected_only_check, path_edit
    ):
        if mode_combo.currentData() == MatrixSourceMode.FILE:
            return self.load_point_set_document(self.resolve_local_path(path_edit.text()))
        return self.load_point_set_from_layer(
            layer_combo.currentLayer(),
            id_field_combo.currentText().strip(),
            selected_only_check.isChecked(),
        )

    def populate_id_fields(self, layer, field_combo):
        field_combo.clear()
        field_combo.addItem("(auto)")
        if layer is None:
            return
        for field in layer.fields():
            field_combo.addItem(field.name())
        preferred = field_combo.findText("id")
        if preferred >= 0:
            field_combo.setCurrentIndex(preferred)

    def update_matrix_source_visibility(
        self,
        mode_combo,
        layer_combo,
        layer_label,
        field_combo,
        field_label,
        selected_only_check,
        file_widget,
        file_label,
    ):
        using_layer = mode_combo.currentData() == MatrixSourceMode.LAYER
        layer_label.setVisible(using_layer)
        layer_combo.setVisible(using_layer)
        field_label.setVisible(using_layer)
        field_combo.setVisible(using_layer)
        selected_only_check.setVisible(using_layer)
        file_label.setVisible(not using_layer)
        file_widget.setVisible(not using_layer)

    def load_point_set_from_layer(self, layer, id_field_name, selected_only):
        if layer is None:
            raise ValueError("choose a point layer for the matrix source")
        if QgsWkbTypes.geometryType(layer.wkbType()) != QgsWkbTypes.PointGeometry:
            raise ValueError("matrix source layer '{}' is not a point layer".format(layer.name()))

        if selected_only:
            feature_iter = layer.getSelectedFeatures()
        else:
            feature_iter = layer.getFeatures()

        transform = QgsCoordinateTransform(layer.crs(), self.wgs84, QgsProject.instance())
        points = []
        for feature in feature_iter:
            point = self.feature_point(feature)
            transformed_point = transform.transform(point)
            point_id = self.feature_value(feature, id_field_name) or "{}_{}".format(
                layer.name(), feature.id()
            )
            points.append(
                {
                    "id": point_id,
                    "lon": transformed_point.x(),
                    "lat": transformed_point.y(),
                }
            )

        if not points:
            raise ValueError("matrix source layer '{}' has no usable points".format(layer.name()))

        return {
            "points": points,
            "snap": {"max_distance_m": 500.0},
            "returns": {"geometry": "full"},
        }

    def feature_value(self, feature, field_name):
        if not field_name or field_name == "(auto)":
            return ""
        index = feature.fields().indexFromName(field_name)
        if index < 0:
            return ""
        value = feature.attributes()[index]
        return str(value).strip() if value is not None else ""

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
            if isinstance(parsed, dict) and "pairs" in parsed:
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
            if isinstance(parsed, dict) and "points" in parsed:
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
        display_name = layer_name or output_path.stem
        self.remove_existing_result_layer(display_name)
        layer = QgsVectorLayer(str(output_path), display_name, "ogr")
        if not layer.isValid():
            self.log("Failed to load layer {}".format(output_path), Qgis.Warning)
            return
        QgsProject.instance().addMapLayer(layer)
        self.log("Loaded layer {}".format(output_path))

    def remove_existing_result_layer(self, layer_name):
        project = QgsProject.instance()
        for layer in list(project.mapLayers().values()):
            if layer.name() == layer_name:
                project.removeMapLayer(layer.id())

    def log(self, message, level=Qgis.Info):
        QgsMessageLog.logMessage(message, "netan", level)
        self.log_output.appendPlainText(message)

    def alert(self, message):
        QMessageBox.warning(self, "netan", message)
        self.log(message, Qgis.Warning)

    def cleanup(self):
        self.finish_point_pick()
        for marker in [self.origin_marker, self.destination_marker]:
            if marker is not None:
                marker.hide()
                marker.setVisible(False)

    def closeEvent(self, event):
        self.save_settings()
        self.cleanup()
        super().closeEvent(event)

    def save_settings(self):
        settings = QSettings()
        values = {
            "workspace_root": self.workspace_root_edit.text().strip(),
            "api_base_url": self.api_base_url_edit.text().strip(),
            "timeout_seconds": self.timeout_seconds_edit.text().strip(),
            "response_format": self.response_format(),
            "profile_id": self.selected_profile_id() or "",
            "route_id": self.route_id_edit.text().strip(),
            "route_output_path": self.route_output_path_edit.text().strip(),
            "route_request_path": self.route_request_path_edit.text().strip(),
            "snap_distance": self.snap_distance_edit.text().strip(),
            "origin_id": self.origin_id_edit.text().strip(),
            "origin_lon": self.origin_lon_edit.text().strip(),
            "origin_lat": self.origin_lat_edit.text().strip(),
            "destination_id": self.destination_id_edit.text().strip(),
            "destination_lon": self.destination_lon_edit.text().strip(),
            "destination_lat": self.destination_lat_edit.text().strip(),
            "od_pairs_path": self.od_pairs_path_edit.text().strip(),
            "od_output_path": self.od_output_path_edit.text().strip(),
            "matrix_output_path": self.matrix_output_path_edit.text().strip(),
            "matrix_origins_mode": self.matrix_origins_mode_combo.currentData(),
            "matrix_origins_path": self.matrix_origins_path_edit.text().strip(),
            "matrix_origins_id_field": self.matrix_origins_id_field_combo.currentText().strip(),
            "matrix_origins_selected_only": self.matrix_origins_selected_only_check.isChecked(),
            "matrix_destinations_mode": self.matrix_destinations_mode_combo.currentData(),
            "matrix_destinations_path": self.matrix_destinations_path_edit.text().strip(),
            "matrix_destinations_id_field": self.matrix_destinations_id_field_combo.currentText().strip(),
            "matrix_destinations_selected_only": self.matrix_destinations_selected_only_check.isChecked(),
        }
        for key, value in values.items():
            settings.setValue("{}/{}".format(SETTINGS_PREFIX, key), value)

        if self.matrix_origins_layer_combo.currentLayer() is not None:
            settings.setValue(
                "{}/matrix_origins_layer_id".format(SETTINGS_PREFIX),
                self.matrix_origins_layer_combo.currentLayer().id(),
            )
        if self.matrix_destinations_layer_combo.currentLayer() is not None:
            settings.setValue(
                "{}/matrix_destinations_layer_id".format(SETTINGS_PREFIX),
                self.matrix_destinations_layer_combo.currentLayer().id(),
            )

    def load_settings(self):
        self.workspace_root_edit.setText(
            self.read_setting("workspace_root", self.workspace_root_edit.text())
        )
        self.api_base_url_edit.setText(
            self.read_setting("api_base_url", self.api_base_url_edit.text())
        )
        self.timeout_seconds_edit.setText(
            self.read_setting("timeout_seconds", self.timeout_seconds_edit.text())
        )
        self.set_combo_by_data(
            self.response_format_combo,
            self.read_setting("response_format", ResponseFormat.JSON),
        )
        self.route_id_edit.setText(self.read_setting("route_id", self.route_id_edit.text()))
        self.route_output_path_edit.setText(
            self.read_setting("route_output_path", self.route_output_path_edit.text())
        )
        self.route_request_path_edit.setText(
            self.read_setting("route_request_path", self.route_request_path_edit.text())
        )
        self.snap_distance_edit.setText(
            self.read_setting("snap_distance", self.snap_distance_edit.text())
        )
        self.origin_id_edit.setText(
            self.read_setting("origin_id", self.origin_id_edit.text())
        )
        self.origin_lon_edit.setText(self.read_setting("origin_lon", ""))
        self.origin_lat_edit.setText(self.read_setting("origin_lat", ""))
        self.destination_id_edit.setText(
            self.read_setting("destination_id", self.destination_id_edit.text())
        )
        self.destination_lon_edit.setText(self.read_setting("destination_lon", ""))
        self.destination_lat_edit.setText(self.read_setting("destination_lat", ""))
        self.od_pairs_path_edit.setText(
            self.read_setting("od_pairs_path", self.od_pairs_path_edit.text())
        )
        self.od_output_path_edit.setText(
            self.read_setting("od_output_path", self.od_output_path_edit.text())
        )
        self.matrix_output_path_edit.setText(
            self.read_setting("matrix_output_path", self.matrix_output_path_edit.text())
        )
        self.matrix_origins_path_edit.setText(
            self.read_setting("matrix_origins_path", self.matrix_origins_path_edit.text())
        )
        self.matrix_destinations_path_edit.setText(
            self.read_setting(
                "matrix_destinations_path", self.matrix_destinations_path_edit.text()
            )
        )
        self.matrix_origins_selected_only_check.setChecked(
            self.read_bool_setting("matrix_origins_selected_only", False)
        )
        self.matrix_destinations_selected_only_check.setChecked(
            self.read_bool_setting("matrix_destinations_selected_only", False)
        )
        self.set_combo_by_data(
            self.matrix_origins_mode_combo,
            self.read_setting("matrix_origins_mode", MatrixSourceMode.LAYER),
        )
        self.set_combo_by_data(
            self.matrix_destinations_mode_combo,
            self.read_setting("matrix_destinations_mode", MatrixSourceMode.LAYER),
        )
        self.restore_layer_selection(
            self.matrix_origins_layer_combo,
            self.read_setting("matrix_origins_layer_id", ""),
        )
        self.restore_layer_selection(
            self.matrix_destinations_layer_combo,
            self.read_setting("matrix_destinations_layer_id", ""),
        )
        self.populate_id_fields(
            self.matrix_origins_layer_combo.currentLayer(),
            self.matrix_origins_id_field_combo,
        )
        self.populate_id_fields(
            self.matrix_destinations_layer_combo.currentLayer(),
            self.matrix_destinations_id_field_combo,
        )
        self.set_combo_by_text(
            self.matrix_origins_id_field_combo,
            self.read_setting("matrix_origins_id_field", "(auto)"),
        )
        self.set_combo_by_text(
            self.matrix_destinations_id_field_combo,
            self.read_setting("matrix_destinations_id_field", "(auto)"),
        )
        self.update_matrix_source_visibility(
            self.matrix_origins_mode_combo,
            self.matrix_origins_layer_combo,
            self.matrix_origins_point_layer_label,
            self.matrix_origins_id_field_combo,
            self.matrix_origins_id_field_label,
            self.matrix_origins_selected_only_check,
            self.matrix_origins_path_edit.parentWidget(),
            self.matrix_origins_file_label,
        )
        self.update_matrix_source_visibility(
            self.matrix_destinations_mode_combo,
            self.matrix_destinations_layer_combo,
            self.matrix_destinations_point_layer_label,
            self.matrix_destinations_id_field_combo,
            self.matrix_destinations_id_field_label,
            self.matrix_destinations_selected_only_check,
            self.matrix_destinations_path_edit.parentWidget(),
            self.matrix_destinations_file_label,
        )

    def restore_layer_selection(self, layer_combo, layer_id):
        if not layer_id:
            return
        layer = QgsProject.instance().mapLayer(layer_id)
        if layer is not None:
            if hasattr(layer_combo, "setLayer"):
                layer_combo.setLayer(layer)
            elif hasattr(layer_combo, "setCurrentLayer"):
                layer_combo.setCurrentLayer(layer)

    def read_setting(self, key, default_value):
        settings = QSettings()
        return settings.value("{}/{}".format(SETTINGS_PREFIX, key), default_value, type=str)

    def read_bool_setting(self, key, default_value):
        settings = QSettings()
        return settings.value("{}/{}".format(SETTINGS_PREFIX, key), default_value, type=bool)

    def set_combo_by_data(self, combo, value):
        index = combo.findData(value)
        if index >= 0:
            combo.setCurrentIndex(index)

    def set_combo_by_text(self, combo, value):
        index = combo.findText(value)
        if index >= 0:
            combo.setCurrentIndex(index)
