"""Core dock widget: layout assembly, shared paths, parsing, and logging."""

import tempfile
from pathlib import Path

from qgis.PyQt.QtCore import QSettings, QTimer
from qgis.PyQt.QtWidgets import (
    QDockWidget,
    QFileDialog,
    QHBoxLayout,
    QPlainTextEdit,
    QPushButton,
    QScrollArea,
    QTabWidget,
    QVBoxLayout,
    QWidget,
)
from qgis.core import QgsCoordinateReferenceSystem, QgsMessageLog, QgsProject

from .advanced import AdvancedControlsMixin
from .batch_tab import BatchTabMixin
from .compat import (
    MSG_INFO,
    MSG_WARNING,
    dock_widget_feature,
    qt_dock_area,
    qt_widget_attribute,
)
from .connection import ConnectionMixin
from .constants import SETTINGS_PREFIX
from .map_points import MapPointsMixin
from .results import ResultsMixin
from .route_tab import RouteTabMixin
from .runs_tab import RunsTabMixin
from .service_area_tab import ServiceAreaTabMixin
from .settings_io import SettingsIoMixin
from .simulation_tab import SimulationTabMixin
from .transit_tab import TransitTabMixin


class NetweevilDock(
    ConnectionMixin,
    MapPointsMixin,
    AdvancedControlsMixin,
    RouteTabMixin,
    TransitTabMixin,
    BatchTabMixin,
    ServiceAreaTabMixin,
    RunsTabMixin,
    ResultsMixin,
    SimulationTabMixin,
    SettingsIoMixin,
    QDockWidget,
):
    def __init__(self, iface, action):
        super().__init__("netweevil", iface.mainWindow())
        self.iface = iface
        self.action = action
        self.service_info = None
        self.temp_layers_dir = Path(tempfile.gettempdir()) / "netweevil_qgis_layers"
        self.temp_layers_dir.mkdir(parents=True, exist_ok=True)
        self.wgs84 = QgsCoordinateReferenceSystem("EPSG:4326")
        self.point_picker_tool = None
        self.previous_map_tool = None
        self.pick_target = None
        self.origin_marker = None
        self.destination_marker = None
        self.last_output_layer_ids = []

        self.setObjectName("netweevilDock")
        self.setAllowedAreas(
            qt_dock_area("LeftDockWidgetArea") | qt_dock_area("RightDockWidgetArea")
        )
        self.setFeatures(
            dock_widget_feature("DockWidgetClosable")
            | dock_widget_feature("DockWidgetMovable")
            | dock_widget_feature("DockWidgetFloatable")
        )
        self.setMinimumWidth(620)
        self.setMinimumHeight(680)
        self.resize(760, 860)
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

        layout.addWidget(self._build_connection_bar())

        self.tabs = QTabWidget()
        self.tabs.addTab(self._wrap_tab_scroll(self._build_route_tab()), "Route")
        self.tabs.addTab(self._wrap_tab_scroll(self._build_transit_tab()), "Transit")
        self.tabs.addTab(
            self._wrap_tab_scroll(self._build_service_area_tab()), "Service Area"
        )
        self.tabs.addTab(self._wrap_tab_scroll(self._build_batch_tab()), "OD / Matrix")
        self.tabs.addTab(
            self._wrap_tab_scroll(self._build_simulation_tab()), "Simulation"
        )
        self.tabs.addTab(self._wrap_tab_scroll(self._build_runs_tab()), "Runs")
        self.tabs.addTab(self._wrap_tab_scroll(self._build_settings_tab()), "Settings")
        layout.addWidget(self.tabs, stretch=4)

        action_row = QHBoxLayout()
        zoom_output_button = QPushButton("Zoom To Last Output")
        zoom_output_button.setToolTip(
            "Zoom the map canvas to the layers loaded by the most recent analysis."
        )
        zoom_output_button.clicked.connect(self.zoom_to_last_output)
        clear_log_button = QPushButton("Clear Log")
        action_row.addWidget(zoom_output_button)
        action_row.addWidget(clear_log_button)
        action_row.addStretch(1)
        layout.addLayout(action_row)

        self.log_output = QPlainTextEdit()
        self.log_output.setReadOnly(True)
        self.log_output.setPlaceholderText("API and plugin log output.")
        self.log_output.setMaximumHeight(170)
        self.log_output.document().setMaximumBlockCount(200)
        clear_log_button.clicked.connect(self.log_output.clear)
        layout.addWidget(self.log_output, stretch=1)

        return container

    def _wrap_tab_scroll(self, widget):
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setWidget(widget)
        return scroll

    def _make_toggle_section(self, label, content_widget, expanded=False):
        """Checkable Show/Hide button controlling a collapsible widget."""
        button = QPushButton()
        button.setCheckable(True)

        def sync(checked):
            content_widget.setVisible(checked)
            button.setText("Hide {}".format(label) if checked else "Show {}".format(label))

        button.toggled.connect(sync)
        button.setChecked(expanded)
        sync(expanded)
        return button

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
            return Path.home()
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
        try:
            return float(raw)
        except ValueError:
            return 30.0

    def selected_profile_id(self):
        return self.profile_combo.currentData()

    def response_format(self):
        return self.response_format_combo.currentData()

    def service_url(self, suffix, include_format=False, response_format=None):
        import urllib.parse

        from .constants import ResponseFormat

        url = "{}{}".format(self.api_base_url(), suffix)
        selected_format = response_format or self.response_format()
        if include_format and selected_format == ResponseFormat.GEOJSON:
            return "{}?{}".format(
                url, urllib.parse.urlencode({"format": ResponseFormat.GEOJSON})
            )
        return url

    def next_numbered_id(self, value, fallback_prefix):
        import re

        text = (value or "").strip()
        if not text:
            return "{}_001".format(fallback_prefix)
        match = re.match(r"^(.*?)(\d+)$", text)
        if match:
            prefix, digits = match.groups()
            return "{}{:0{}d}".format(prefix, int(digits) + 1, len(digits))
        clean = text.rstrip("_- ")
        return "{}_001".format(clean or fallback_prefix)

    def parse_optional_float(self, raw_value, label):
        text = raw_value.strip()
        if not text:
            return None
        try:
            return float(text)
        except ValueError as exc:
            raise ValueError("{} must be a number".format(label)) from exc

    def parse_optional_int(self, raw_value, label):
        text = raw_value.strip()
        if not text:
            return None
        try:
            return int(text)
        except ValueError as exc:
            raise ValueError("{} must be an integer".format(label)) from exc

    def parse_float_with_default(self, raw_value, label, default):
        value = self.parse_optional_float(raw_value, label)
        return default if value is None else value

    def parse_int_with_default(self, raw_value, label, default):
        value = self.parse_optional_int(raw_value, label)
        return default if value is None else value

    def require_point(self, lon_edit, lat_edit, label):
        """Parse a lon/lat pair with an actionable error instead of a float() trace."""
        try:
            return float(lon_edit.text().strip()), float(lat_edit.text().strip())
        except ValueError:
            raise ValueError("Set the {} first.".format(label))

    def set_last_output_layers(self, layers):
        self.last_output_layer_ids = [layer.id() for layer in layers if layer is not None]

    def zoom_to_last_output(self):
        if not self.last_output_layer_ids:
            self.alert("No output layers have been loaded in this session yet.")
            return
        layers = []
        for layer_id in self.last_output_layer_ids:
            layer = QgsProject.instance().mapLayer(layer_id)
            if layer is not None:
                layers.append(layer)
        if not layers:
            self.alert("The last output layers are no longer available in the project.")
            return
        extent = self.combined_extent(layers)
        if extent is None:
            self.alert("The last output only contains non-spatial tables.")
            return
        self.iface.mapCanvas().setExtent(extent)
        self.iface.mapCanvas().refresh()

    def combined_extent(self, layers):
        extent = None
        for layer in layers:
            if hasattr(layer, "isSpatial") and not layer.isSpatial():
                continue
            layer_extent = layer.extent()
            if layer_extent.isEmpty():
                continue
            if extent is None:
                extent = layer_extent
            else:
                extent.combineExtentWith(layer_extent)
        return extent

    def log(self, message, level=MSG_INFO):
        QgsMessageLog.logMessage(message, "netweevil", level)
        self.log_output.appendPlainText(message)

    def alert(self, message):
        self.iface.messageBar().pushMessage(
            "netweevil", message, level=MSG_WARNING, duration=6
        )
        self.log(message, MSG_WARNING)

    def cleanup(self):
        self.finish_point_pick()
        if hasattr(self, "sim_status_timer"):
            self.sim_status_timer.stop()
        if hasattr(self, "sim_playback_timer"):
            self.sim_playback_timer.stop()
        for marker in [self.origin_marker, self.destination_marker]:
            if marker is not None:
                marker.hide()
                marker.setVisible(False)

    def closeEvent(self, event):
        self.save_settings()
        self.cleanup()
        super().closeEvent(event)

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
