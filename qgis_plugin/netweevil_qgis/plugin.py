import csv
import json
import re
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
    QScrollArea,
    QTabWidget,
    QVBoxLayout,
    QWidget,
)
from qgis.core import (
    Qgis,
    QgsCategorizedSymbolRenderer,
    QgsCoordinateReferenceSystem,
    QgsCoordinateTransform,
    QgsDistanceArea,
    QgsFillSymbol,
    QgsFeatureRequest,
    QgsLineSymbol,
    QgsMarkerSymbol,
    QgsMapLayerProxyModel,
    QgsMessageLog,
    QgsPointXY,
    QgsProject,
    QgsRendererCategory,
    QgsVectorLayer,
    QgsWkbTypes,
)
from qgis.gui import QgsMapLayerComboBox, QgsMapToolEmitPoint, QgsVertexMarker


PLUGIN_MENU = "&netweevil"
SETTINGS_PREFIX = "netweevil_qgis"


class ResponseFormat:
    JSON = "json"
    GEOJSON = "geojson"


class MatrixSourceMode:
    LAYER = "layer"
    FILE = "file"


class PickTarget:
    ORIGIN = "origin"
    DESTINATION = "destination"
    SERVICE_AREA_ORIGIN = "service_area_origin"
    TRANSIT_ORIGIN = "transit_origin"
    TRANSIT_DESTINATION = "transit_destination"


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


def message_box_button(member_name):
    return qt_enum_value(QMessageBox, "StandardButton", member_name)


class NetweevilPlugin:
    def __init__(self, iface):
        self.iface = iface
        self.action = None
        self.dock = None

    def initGui(self):
        self.action = QAction("netweevil", self.iface.mainWindow())
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
            self.dock = NetweevilDock(self.iface, self.action)
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


class NetweevilDock(QDockWidget):
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

        self.tabs = QTabWidget()
        self.tabs.addTab(self._wrap_tab_scroll(self._build_api_tab()), "API")
        self.tabs.addTab(self._wrap_tab_scroll(self._build_runs_tab()), "Runs")
        self.tabs.addTab(self._wrap_tab_scroll(self._build_route_tab()), "Route")
        self.tabs.addTab(self._wrap_tab_scroll(self._build_transit_tab()), "Transit")
        self.tabs.addTab(self._wrap_tab_scroll(self._build_batch_tab()), "Batch")
        self.tabs.addTab(
            self._wrap_tab_scroll(self._build_service_area_tab()), "Service Area"
        )
        layout.addWidget(self.tabs, stretch=4)

        action_row = QHBoxLayout()
        zoom_output_button = QPushButton("Zoom To Last Output")
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
        self.loaded_transit_feeds_edit = QLineEdit()
        self.loaded_transit_feeds_edit.setReadOnly(True)
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
        form.addRow("Loaded transit feeds", self.loaded_transit_feeds_edit)
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
            "This plugin talks directly to the running netweevil API. "
            "Pick route points from the map canvas, or build batch analyses from QGIS layers. "
            "Route detail layers use JSON internally when needed so segmented rows and breakdown tables remain available in QGIS."
        )
        description.setWordWrap(True)
        layout.addWidget(description)
        layout.addStretch(1)
        return tab

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
        refresh_button.clicked.connect(self.refresh_saved_runs)
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

    def _build_advanced_controls(self, prefix, include_failure_modes):
        container = QWidget()
        layout = QVBoxLayout(container)
        layout.setContentsMargins(0, 0, 0, 0)

        connectivity_group = QGroupBox("Disconnected-network handling")
        connectivity_form = QFormLayout(connectivity_group)
        connectivity_mode_combo = QComboBox()
        connectivity_mode_combo.addItem("Strict", "strict")
        connectivity_mode_combo.addItem("Ignore unreachable", "ignore_unreachable")
        connectivity_mode_combo.addItem(
            "Hop origin to reachable component",
            "hop_origin_to_nearest_reachable_component",
        )
        connectivity_mode_combo.addItem(
            "Hop destination to reachable component",
            "hop_destination_to_nearest_reachable_component",
        )
        connectivity_mode_combo.addItem("Hop either end", "hop_either_end")
        max_hop_distance_edit = QLineEdit("")
        report_hop_distance_check = QCheckBox("Report hop distance separately")
        connectivity_form.addRow("Policy", connectivity_mode_combo)
        connectivity_form.addRow("Max hop distance m", max_hop_distance_edit)
        connectivity_form.addRow("", report_hop_distance_check)
        layout.addWidget(connectivity_group)

        setattr(self, "{}_connectivity_mode_combo".format(prefix), connectivity_mode_combo)
        setattr(self, "{}_max_hop_distance_edit".format(prefix), max_hop_distance_edit)
        setattr(
            self,
            "{}_report_hop_distance_check".format(prefix),
            report_hop_distance_check,
        )

        if include_failure_modes:
            unsafe_note = QLabel(
                "Unsafe failure modes are off by default. Enable them only for explicit degraded-routing analysis."
            )
            unsafe_note.setWordWrap(True)
            layout.addWidget(unsafe_note)

            unsafe_toggle = QPushButton("Show Unsafe Failure Modes")
            unsafe_toggle.setCheckable(True)
            unsafe_widget = QWidget()
            unsafe_widget.hide()
            unsafe_layout = QVBoxLayout(unsafe_widget)
            unsafe_layout.setContentsMargins(0, 0, 0, 0)

            failure_group = QGroupBox("Unsafe failure modes")
            failure_form = QFormLayout(failure_group)
            allow_reverse_oneway_check = QCheckBox("Allow reverse oneway traversal")
            allow_illegal_turn_check = QCheckBox("Allow illegal turns")
            ignore_turn_restrictions_check = QCheckBox("Ignore turn restrictions")
            allow_uturn_check = QCheckBox("Allow normally forbidden U-turns")
            auto_relax_unreachable_check = QCheckBox(
                "Auto resolve unreachable with least-permissive degraded route"
            )
            reverse_penalty_edit = QLineEdit("")
            illegal_turn_penalty_edit = QLineEdit("")
            ignored_restriction_penalty_edit = QLineEdit("")
            forbidden_uturn_penalty_edit = QLineEdit("")
            max_illegal_distance_edit = QLineEdit("")
            max_illegal_turns_edit = QLineEdit("")
            failure_form.addRow("", auto_relax_unreachable_check)
            failure_form.addRow("", allow_reverse_oneway_check)
            failure_form.addRow("", allow_illegal_turn_check)
            failure_form.addRow("", ignore_turn_restrictions_check)
            failure_form.addRow("", allow_uturn_check)
            failure_form.addRow("Reverse oneway penalty s", reverse_penalty_edit)
            failure_form.addRow("Illegal turn penalty s", illegal_turn_penalty_edit)
            failure_form.addRow(
                "Ignored restriction penalty s",
                ignored_restriction_penalty_edit,
            )
            failure_form.addRow("Forbidden U-turn penalty s", forbidden_uturn_penalty_edit)
            failure_form.addRow("Max illegal distance m", max_illegal_distance_edit)
            failure_form.addRow("Max illegal turns", max_illegal_turns_edit)
            unsafe_layout.addWidget(failure_group)
            layout.addWidget(unsafe_toggle)
            layout.addWidget(unsafe_widget)

            unsafe_toggle.toggled.connect(unsafe_widget.setVisible)
            unsafe_toggle.toggled.connect(
                lambda checked, button=unsafe_toggle: button.setText(
                    "Hide Unsafe Failure Modes" if checked else "Show Unsafe Failure Modes"
                )
            )

            setattr(self, "{}_unsafe_toggle".format(prefix), unsafe_toggle)
            setattr(
                self,
                "{}_allow_reverse_oneway_check".format(prefix),
                allow_reverse_oneway_check,
            )
            setattr(
                self,
                "{}_auto_relax_unreachable_check".format(prefix),
                auto_relax_unreachable_check,
            )
            setattr(
                self,
                "{}_allow_illegal_turn_check".format(prefix),
                allow_illegal_turn_check,
            )
            setattr(
                self,
                "{}_ignore_turn_restrictions_check".format(prefix),
                ignore_turn_restrictions_check,
            )
            setattr(self, "{}_allow_uturn_check".format(prefix), allow_uturn_check)
            setattr(
                self,
                "{}_reverse_penalty_edit".format(prefix),
                reverse_penalty_edit,
            )
            setattr(
                self,
                "{}_illegal_turn_penalty_edit".format(prefix),
                illegal_turn_penalty_edit,
            )
            setattr(
                self,
                "{}_ignored_restriction_penalty_edit".format(prefix),
                ignored_restriction_penalty_edit,
            )
            setattr(
                self,
                "{}_forbidden_uturn_penalty_edit".format(prefix),
                forbidden_uturn_penalty_edit,
            )
            setattr(
                self,
                "{}_max_illegal_distance_edit".format(prefix),
                max_illegal_distance_edit,
            )
            setattr(
                self,
                "{}_max_illegal_turns_edit".format(prefix),
                max_illegal_turns_edit,
            )

        return container

    def _build_service_area_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        summary = QLabel(
            "Build one service-area request with multiple origins and thresholds, then load grouped network and polygon outputs back into QGIS."
        )
        summary.setWordWrap(True)
        layout.addWidget(summary)

        options_group = QGroupBox("Service-area options")
        options_form = QFormLayout(options_group)
        self.service_area_analysis_id_edit = QLineEdit("qgis_service_area_001")
        self.service_area_snap_distance_edit = QLineEdit("500")
        self.service_area_output_path_edit = QLineEdit(
            ".netweevil/runs/qgis-service-area.geojson"
        )
        self.service_area_request_path_edit = QLineEdit(
            "examples/requests/service_area_from_qgis.json"
        )
        self.service_area_output_mode_combo = QComboBox()
        self.service_area_output_mode_combo.addItem("Network", "network")
        self.service_area_output_mode_combo.addItem("Polygon", "polygon")
        self.service_area_output_mode_combo.addItem("Both", "both")
        self.service_area_band_mode_combo = QComboBox()
        self.service_area_band_mode_combo.addItem("Cumulative", "cumulative")
        self.service_area_band_mode_combo.addItem("Ring", "ring")
        self.service_area_boundary_mode_combo = QComboBox()
        self.service_area_boundary_mode_combo.addItem("Overlap", "overlap")
        self.service_area_boundary_mode_combo.addItem("Cut At Boundary", "cut_at_boundary")
        self.service_area_multi_origin_mode_combo = QComboBox()
        self.service_area_multi_origin_mode_combo.addItem("Merge", "merge")
        self.service_area_multi_origin_mode_combo.addItem("Overlap", "overlap")
        self.service_area_multi_origin_mode_combo.addItem("Cut", "cut")
        options_form.addRow("Analysis id", self.service_area_analysis_id_edit)
        options_form.addRow("Snap distance m", self.service_area_snap_distance_edit)
        options_form.addRow("Output mode", self.service_area_output_mode_combo)
        options_form.addRow("Band mode", self.service_area_band_mode_combo)
        options_form.addRow("Boundary mode", self.service_area_boundary_mode_combo)
        options_form.addRow("Multi-origin mode", self.service_area_multi_origin_mode_combo)
        options_form.addRow(
            "Response path",
            self._line_with_browse(
                self.service_area_output_path_edit, browse_dir=False, save_dialog=True
            ),
        )
        options_form.addRow(
            "Optional request JSON",
            self._line_with_browse(
                self.service_area_request_path_edit, browse_dir=False, save_dialog=True
            ),
        )
        layout.addWidget(options_group)

        threshold_group = QGroupBox("Thresholds")
        threshold_form = QFormLayout(threshold_group)
        self.service_area_thresholds_edit = QLineEdit("300, 600")
        self.service_area_threshold_metric_combo = QComboBox()
        self.service_area_threshold_metric_combo.addItem("Distance (m)", "distance_m")
        self.service_area_threshold_metric_combo.addItem("Travel time (s)", "travel_time_s")
        threshold_hint = QLabel(
            "Enter comma-separated limits once per request. The selected unit applies to every threshold in this list."
        )
        threshold_hint.setWordWrap(True)
        threshold_form.addRow("Limits", self.service_area_thresholds_edit)
        threshold_form.addRow("Unit", self.service_area_threshold_metric_combo)
        threshold_form.addRow("", threshold_hint)
        layout.addWidget(threshold_group)

        polygon_group = QGroupBox("Polygon generation")
        polygon_form = QFormLayout(polygon_group)
        self.service_area_hull_preset_combo = QComboBox()
        self.service_area_hull_preset_combo.addItem("Conservative", "0.75")
        self.service_area_hull_preset_combo.addItem("Balanced", "1.00")
        self.service_area_hull_preset_combo.addItem("Aggressive", "1.50")
        self.service_area_hull_aggressiveness_edit = QLineEdit("1.0")
        self.service_area_simplification_edit = QLineEdit("20")
        polygon_form.addRow("Hull preset", self.service_area_hull_preset_combo)
        polygon_form.addRow("Hull aggressiveness", self.service_area_hull_aggressiveness_edit)
        polygon_form.addRow(
            "Simplification tolerance m", self.service_area_simplification_edit
        )
        self.service_area_hull_preset_combo.currentIndexChanged.connect(
            self.sync_service_area_hull_preset
        )
        layout.addWidget(polygon_group)

        origins_group = QGroupBox("Origins")
        origins_layout = QVBoxLayout(origins_group)
        origins_note = QLabel(
            "Use one origin per line in the form id,lon,lat. You can append map-picked points, selected point features, or the current route endpoints."
        )
        origins_note.setWordWrap(True)
        origins_layout.addWidget(origins_note)
        self.service_area_origins_edit = QPlainTextEdit()
        self.service_area_origins_edit.setPlaceholderText(
            "origin_a,6.566500,53.219400\norigin_b,6.563600,53.218100"
        )
        self.service_area_origins_edit.setMaximumBlockCount(500)
        origins_layout.addWidget(self.service_area_origins_edit)
        self.service_area_pick_status_label = QLabel(
            "Pick On Map appends one origin. Selected point features are transformed into WGS84 automatically."
        )
        self.service_area_pick_status_label.setWordWrap(True)
        origins_layout.addWidget(self.service_area_pick_status_label)
        origin_button_row = QHBoxLayout()
        pick_origin_button = QPushButton("Pick On Map")
        pick_origin_button.clicked.connect(
            lambda: self.begin_point_pick(PickTarget.SERVICE_AREA_ORIGIN)
        )
        add_selected_button = QPushButton("Add Selected Features")
        add_selected_button.clicked.connect(self.add_service_area_origins_from_selected_features)
        use_route_start_button = QPushButton("Use Route Start")
        use_route_start_button.clicked.connect(self.use_route_start_for_service_area)
        use_route_both_button = QPushButton("Use Route Start + End")
        use_route_both_button.clicked.connect(self.use_route_points_for_service_area)
        reuse_last_origins_button = QPushButton("Reuse Last Origins")
        reuse_last_origins_button.clicked.connect(self.reuse_last_service_area_origins)
        clear_origins_button = QPushButton("Clear Origins")
        clear_origins_button.clicked.connect(self.clear_service_area_origins)
        origin_button_row.addWidget(pick_origin_button)
        origin_button_row.addWidget(add_selected_button)
        origin_button_row.addWidget(use_route_start_button)
        origin_button_row.addWidget(use_route_both_button)
        origin_button_row.addWidget(reuse_last_origins_button)
        origin_button_row.addWidget(clear_origins_button)
        origins_layout.addLayout(origin_button_row)
        layout.addWidget(origins_group)

        service_area_advanced_toggle = QPushButton("Show Connectivity Controls")
        service_area_advanced_toggle.setCheckable(True)
        service_area_advanced_widget = self._build_advanced_controls(
            "service_area", include_failure_modes=False
        )
        service_area_advanced_widget.hide()
        service_area_advanced_toggle.toggled.connect(service_area_advanced_widget.setVisible)
        service_area_advanced_toggle.toggled.connect(
            lambda checked, button=service_area_advanced_toggle: button.setText(
                "Hide Connectivity Controls"
                if checked
                else "Show Connectivity Controls"
            )
        )
        self.service_area_advanced_toggle = service_area_advanced_toggle
        layout.addWidget(service_area_advanced_toggle)
        layout.addWidget(service_area_advanced_widget)

        button_row = QHBoxLayout()
        save_request_button = QPushButton("Save Request")
        save_request_button.clicked.connect(self.write_service_area_request)
        reuse_last_thresholds_button = QPushButton("Reuse Last Thresholds")
        reuse_last_thresholds_button.clicked.connect(self.reuse_last_service_area_thresholds)
        run_service_area_button = QPushButton("Run Service Area")
        run_service_area_button.clicked.connect(self.run_service_area)
        button_row.addWidget(save_request_button)
        button_row.addWidget(reuse_last_thresholds_button)
        button_row.addStretch(1)
        button_row.addWidget(run_service_area_button)
        layout.addLayout(button_row)

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
        self.route_auto_increment_check = QCheckBox("Prepare a fresh route id after each run")
        self.route_auto_increment_check.setChecked(True)
        self.route_auto_output_path_check = QCheckBox("Keep response path in sync with route id")
        self.route_auto_output_path_check.setChecked(True)
        self.snap_distance_edit = QLineEdit("500")
        self.route_output_path_edit = QLineEdit(".netweevil/runs/routes/qgis_route_001.json")
        self.route_request_path_edit = QLineEdit("examples/requests/route_from_qgis.json")
        route_id_row = QWidget()
        route_id_layout = QHBoxLayout(route_id_row)
        route_id_layout.setContentsMargins(0, 0, 0, 0)
        route_id_layout.addWidget(self.route_id_edit)
        new_route_id_button = QPushButton("New")
        new_route_id_button.clicked.connect(self.prepare_next_route_defaults)
        route_id_layout.addWidget(new_route_id_button)
        options_form.addRow("Route id", route_id_row)
        options_form.addRow("", self.route_auto_increment_check)
        options_form.addRow("", self.route_auto_output_path_check)
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

        return_group = QGroupBox("Returned route detail")
        return_form = QFormLayout(return_group)
        self.route_geometry_combo = QComboBox()
        self.route_geometry_combo.addItem("No route geometry", "none")
        self.route_geometry_combo.addItem("Full route geometry", "full")
        self.route_geometry_combo.addItem("Segment-friendly geometry", "segments")
        self.route_geometry_combo.setCurrentIndex(1)
        self.route_segment_rows_check = QCheckBox("Return one row per traversed road segment")
        self.route_segment_rows_check.setChecked(True)
        self.route_road_distance_check = QCheckBox("Road-type distance totals")
        self.route_road_distance_check.setChecked(True)
        self.route_road_time_check = QCheckBox("Road-type time totals")
        self.route_road_time_check.setChecked(True)
        self.route_surface_distance_check = QCheckBox("Surface distance totals")
        self.route_surface_distance_check.setChecked(True)
        self.route_surface_time_check = QCheckBox("Surface time totals")
        self.route_surface_time_check.setChecked(True)
        self.route_penalty_breakdown_check = QCheckBox("Penalty breakdown request flag")
        self.route_penalty_breakdown_check.setChecked(True)
        self.route_explain_cost_derivation_check = QCheckBox(
            "Explain cost derivation request flag"
        )
        self.route_explain_cost_derivation_check.setChecked(True)
        detail_note = QLabel(
            "The plugin will load the route line, segment rows, hop segments, violations, and road/surface breakdown tables when the API returns them. "
            "Penalty and explain flags are exposed here, but the current API only reports penalty totals in the route summary."
        )
        detail_note.setWordWrap(True)
        return_form.addRow("Geometry", self.route_geometry_combo)
        return_form.addRow("", self.route_segment_rows_check)
        return_form.addRow("", self.route_road_distance_check)
        return_form.addRow("", self.route_road_time_check)
        return_form.addRow("", self.route_surface_distance_check)
        return_form.addRow("", self.route_surface_time_check)
        return_form.addRow("", self.route_penalty_breakdown_check)
        return_form.addRow("", self.route_explain_cost_derivation_check)
        return_form.addRow("", detail_note)
        layout.addWidget(return_group)

        route_advanced_toggle = QPushButton("Show Connectivity + Fallback")
        route_advanced_toggle.setCheckable(True)
        route_advanced_widget = self._build_advanced_controls(
            "route", include_failure_modes=True
        )
        route_advanced_widget.hide()
        route_advanced_toggle.toggled.connect(route_advanced_widget.setVisible)
        route_advanced_toggle.toggled.connect(
            lambda checked, button=route_advanced_toggle: button.setText(
                "Hide Connectivity + Fallback"
                if checked
                else "Show Connectivity + Fallback"
            )
        )
        self.route_advanced_toggle = route_advanced_toggle
        layout.addWidget(route_advanced_toggle)
        layout.addWidget(route_advanced_widget)

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

        self.route_id_edit.textChanged.connect(self.sync_route_output_path_from_route_id)
        self.route_auto_output_path_check.toggled.connect(
            self.sync_route_output_path_from_route_id
        )
        self.sync_route_output_path_from_route_id()

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

    def _build_transit_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        summary = QLabel(
            "Plan a pedestrian access + scheduled transit route through a GTFS feed loaded by the API."
        )
        summary.setWordWrap(True)
        layout.addWidget(summary)

        options_group = QGroupBox("Transit route options")
        options_form = QFormLayout(options_group)
        self.transit_feed_combo = QComboBox()
        self.transit_route_id_edit = QLineEdit("qgis_transit_001")
        self.transit_auto_increment_check = QCheckBox(
            "Prepare a fresh transit route id after each run"
        )
        self.transit_auto_increment_check.setChecked(True)
        self.transit_datetime_edit = QLineEdit("2026-05-11T08:30:00+02:00")
        self.transit_arrive_by_check = QCheckBox("Arrive by this time")
        self.transit_arrive_by_check.setEnabled(False)
        self.transit_search_window_edit = QLineEdit("7200")
        self.transit_output_path_edit = QLineEdit(
            ".netweevil/runs/transit/qgis_transit_001.json"
        )
        self.transit_request_path_edit = QLineEdit(
            "examples/requests/transit_from_qgis.json"
        )
        transit_id_row = QWidget()
        transit_id_layout = QHBoxLayout(transit_id_row)
        transit_id_layout.setContentsMargins(0, 0, 0, 0)
        transit_id_layout.addWidget(self.transit_route_id_edit)
        new_transit_id_button = QPushButton("New")
        new_transit_id_button.clicked.connect(self.prepare_next_transit_defaults)
        transit_id_layout.addWidget(new_transit_id_button)
        options_form.addRow("Feed", self.transit_feed_combo)
        options_form.addRow("Route id", transit_id_row)
        options_form.addRow("", self.transit_auto_increment_check)
        options_form.addRow("Departure time", self.transit_datetime_edit)
        options_form.addRow("", self.transit_arrive_by_check)
        options_form.addRow("Search window s", self.transit_search_window_edit)
        options_form.addRow(
            "Response path",
            self._line_with_browse(
                self.transit_output_path_edit, browse_dir=False, save_dialog=True
            ),
        )
        options_form.addRow(
            "Optional request JSON",
            self._line_with_browse(
                self.transit_request_path_edit, browse_dir=False, save_dialog=True
            ),
        )
        layout.addWidget(options_group)

        mode_group = QGroupBox("Modes and limits")
        mode_form = QFormLayout(mode_group)
        self.transit_mode_checks = {}
        transit_modes_row = QWidget()
        transit_modes_layout = QHBoxLayout(transit_modes_row)
        transit_modes_layout.setContentsMargins(0, 0, 0, 0)
        for label, value, checked in [
            ("Bus", "bus", True),
            ("Tram", "tram", True),
            ("Rail", "rail", True),
            ("Subway", "subway", True),
            ("Ferry", "ferry", True),
            ("Coach", "coach", False),
        ]:
            check = QCheckBox(label)
            check.setChecked(checked)
            transit_modes_layout.addWidget(check)
            self.transit_mode_checks[value] = check
        transit_modes_layout.addStretch(1)
        self.transit_walk_speed_edit = QLineEdit("4.8")
        self.transit_max_access_distance_edit = QLineEdit("1200")
        self.transit_max_egress_distance_edit = QLineEdit("1200")
        self.transit_max_transfer_distance_edit = QLineEdit("500")
        self.transit_board_slack_edit = QLineEdit("30")
        self.transit_transfer_slack_edit = QLineEdit("120")
        self.transit_max_transfers_edit = QLineEdit("3")
        self.transit_include_geometry_check = QCheckBox("Load leg geometry")
        self.transit_include_geometry_check.setChecked(True)
        self.transit_network_walk_geometry_check = QCheckBox(
            "Use selected pedestrian profile for walking legs"
        )
        self.transit_network_walk_geometry_check.setChecked(False)
        self.transit_include_stops_check = QCheckBox("Load transit stops")
        self.transit_include_stops_check.setChecked(True)
        self.transit_include_stop_segments_check = QCheckBox("Load stop-to-stop segments")
        self.transit_include_stop_segments_check.setChecked(True)
        mode_form.addRow("Transit modes", transit_modes_row)
        mode_form.addRow("Walk speed kph", self.transit_walk_speed_edit)
        mode_form.addRow("Max access distance m", self.transit_max_access_distance_edit)
        mode_form.addRow("Max egress distance m", self.transit_max_egress_distance_edit)
        mode_form.addRow("Max transfer distance m", self.transit_max_transfer_distance_edit)
        mode_form.addRow("Board slack s", self.transit_board_slack_edit)
        mode_form.addRow("Transfer slack s", self.transit_transfer_slack_edit)
        mode_form.addRow("Max transfers", self.transit_max_transfers_edit)
        mode_form.addRow("", self.transit_include_geometry_check)
        mode_form.addRow("", self.transit_network_walk_geometry_check)
        mode_form.addRow("", self.transit_include_stops_check)
        mode_form.addRow("", self.transit_include_stop_segments_check)
        layout.addWidget(mode_group)

        self.transit_pick_status_label = QLabel(
            "Pick Origin or Pick Destination, then click on the map."
        )
        self.transit_pick_status_label.setWordWrap(True)
        layout.addWidget(self.transit_pick_status_label)

        layout.addWidget(
            self._build_transit_point_group("Origin", PickTarget.TRANSIT_ORIGIN)
        )
        layout.addWidget(
            self._build_transit_point_group("Destination", PickTarget.TRANSIT_DESTINATION)
        )

        button_row = QHBoxLayout()
        pick_origin_button = QPushButton("Pick Origin")
        pick_origin_button.clicked.connect(
            lambda: self.begin_point_pick(PickTarget.TRANSIT_ORIGIN)
        )
        pick_destination_button = QPushButton("Pick Destination")
        pick_destination_button.clicked.connect(
            lambda: self.begin_point_pick(PickTarget.TRANSIT_DESTINATION)
        )
        swap_button = QPushButton("Swap")
        swap_button.clicked.connect(self.swap_transit_points)
        clear_button = QPushButton("Clear")
        clear_button.clicked.connect(self.clear_transit_points)
        save_request_button = QPushButton("Save Request")
        save_request_button.clicked.connect(self.write_transit_request)
        run_button = QPushButton("Run Transit Route")
        run_button.clicked.connect(self.run_transit_route)
        button_row.addWidget(pick_origin_button)
        button_row.addWidget(pick_destination_button)
        button_row.addWidget(swap_button)
        button_row.addWidget(clear_button)
        button_row.addStretch(1)
        button_row.addWidget(save_request_button)
        button_row.addWidget(run_button)
        layout.addLayout(button_row)

        self.transit_route_id_edit.textChanged.connect(
            self.sync_transit_output_path_from_route_id
        )
        self.sync_transit_output_path_from_route_id()

        layout.addStretch(1)
        return tab

    def _build_transit_point_group(self, title, target):
        group = QGroupBox(title)
        layout = QVBoxLayout(group)

        form = QFormLayout()
        point_id_edit = QLineEdit(
            "origin" if target == PickTarget.TRANSIT_ORIGIN else "destination"
        )
        lon_edit = QLineEdit("")
        lat_edit = QLineEdit("")
        if target == PickTarget.TRANSIT_ORIGIN:
            self.transit_origin_id_edit = point_id_edit
            self.transit_origin_lon_edit = lon_edit
            self.transit_origin_lat_edit = lat_edit
        else:
            self.transit_destination_id_edit = point_id_edit
            self.transit_destination_lon_edit = lon_edit
            self.transit_destination_lat_edit = lat_edit

        form.addRow("{} id".format(title), point_id_edit)
        form.addRow("{} lon".format(title), lon_edit)
        form.addRow("{} lat".format(title), lat_edit)
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
        self.od_output_path_edit = QLineEdit(".netweevil/runs/qgis-od.geojson")
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
        self.matrix_output_path_edit = QLineEdit(".netweevil/runs/qgis-matrix.geojson")
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

        batch_advanced_toggle = QPushButton("Show Advanced Controls")
        batch_advanced_toggle.setCheckable(True)
        batch_advanced_widget = self._build_advanced_controls(
            "batch", include_failure_modes=True
        )
        batch_advanced_widget.hide()
        batch_advanced_toggle.toggled.connect(batch_advanced_widget.setVisible)
        batch_advanced_toggle.toggled.connect(
            lambda checked, button=batch_advanced_toggle: button.setText(
                "Hide Advanced Controls" if checked else "Show Advanced Controls"
            )
        )
        self.batch_advanced_toggle = batch_advanced_toggle
        layout.addWidget(batch_advanced_toggle)
        layout.addWidget(batch_advanced_widget)

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

    def service_url(self, suffix, include_format=False, response_format=None):
        url = "{}{}".format(self.api_base_url(), suffix)
        selected_format = response_format or self.response_format()
        if include_format and selected_format == ResponseFormat.GEOJSON:
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
            self.loaded_transit_feeds_edit.setText("")
            self.dataset_bounds_edit.setText("")
            self.profile_combo.clear()
            self.transit_feed_combo.clear()
            self.transit_feed_combo.addItem("No transit feeds loaded", "")
            self.service_status_label.setText(
                "Service status: unavailable at {}.".format(self.api_base_url() or "configured URL")
            )
            if manual:
                self.log("Failed to refresh service: {}".format(exc), Qgis.Warning)
            else:
                self.log(
                    "netweevil service not reachable during startup: {}".format(exc), Qgis.Info
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
        transit_feeds = service.get("loaded_transit_feeds", [])
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

        previous_feed_id = self.read_setting("transit_feed_id", "")
        self.transit_feed_combo.clear()
        if transit_feeds:
            for feed in transit_feeds:
                label = "{} ({} stops, {} routes)".format(
                    feed.get("feed_id", "unknown"),
                    feed.get("stop_count", 0),
                    feed.get("route_count", 0),
                )
                self.transit_feed_combo.addItem(label, feed.get("feed_id", ""))
            desired_feed_index = self.transit_feed_combo.findData(previous_feed_id)
            if desired_feed_index >= 0:
                self.transit_feed_combo.setCurrentIndex(desired_feed_index)
        else:
            self.transit_feed_combo.addItem("No transit feeds loaded", "")
        self.loaded_transit_feeds_edit.setText(
            ", ".join(feed.get("feed_id", "unknown") for feed in transit_feeds)
            if transit_feeds
            else "none"
        )

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
        if target == PickTarget.ORIGIN:
            self.pick_status_label.setText("Click the start point on the map.")
        elif target == PickTarget.DESTINATION:
            self.pick_status_label.setText("Click the end point on the map.")
        elif target == PickTarget.TRANSIT_ORIGIN:
            self.transit_pick_status_label.setText("Click the transit origin on the map.")
        elif target == PickTarget.TRANSIT_DESTINATION:
            self.transit_pick_status_label.setText(
                "Click the transit destination on the map."
            )
        else:
            self.service_area_pick_status_label.setText(
                "Click one service-area origin on the map."
            )

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

        if self.pick_target == PickTarget.SERVICE_AREA_ORIGIN:
            self.append_service_area_origin(
                self.default_point_id(self.pick_target),
                wgs84_point.x(),
                wgs84_point.y(),
            )
            self.service_area_pick_status_label.setText(
                "Added one service-area origin from the map canvas."
            )
        elif self.pick_target in [
            PickTarget.TRANSIT_ORIGIN,
            PickTarget.TRANSIT_DESTINATION,
        ]:
            self.set_transit_point(
                self.pick_target,
                lon=wgs84_point.x(),
                lat=wgs84_point.y(),
                point_id=self.default_point_id(self.pick_target),
            )
            label = (
                "origin"
                if self.pick_target == PickTarget.TRANSIT_ORIGIN
                else "destination"
            )
            self.transit_pick_status_label.setText(
                "Set the transit {} from the map canvas.".format(label)
            )
        else:
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
        if target in [PickTarget.TRANSIT_ORIGIN, PickTarget.TRANSIT_DESTINATION]:
            self.set_transit_point(target, wgs84_point.x(), wgs84_point.y(), point_id)
            label = "origin" if target == PickTarget.TRANSIT_ORIGIN else "destination"
            self.transit_pick_status_label.setText(
                "Set the transit {} from the selected feature in '{}'.".format(
                    label, layer.name()
                )
            )
        else:
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
        if target == PickTarget.SERVICE_AREA_ORIGIN:
            count = len(
                [
                    line
                    for line in self.service_area_origins_edit.toPlainText().splitlines()
                    if line.strip()
                ]
            )
            return "origin_{:03d}".format(count + 1)
        if target == PickTarget.TRANSIT_ORIGIN:
            existing = self.transit_origin_id_edit.text().strip()
            return existing or "origin"
        if target == PickTarget.TRANSIT_DESTINATION:
            existing = self.transit_destination_id_edit.text().strip()
            return existing or "destination"
        existing = self.destination_id_edit.text().strip()
        return existing or "destination"

    def set_route_point(self, target, lon, lat, point_id):
        if target == PickTarget.SERVICE_AREA_ORIGIN:
            return
        if target == PickTarget.ORIGIN:
            self.origin_id_edit.setText(point_id)
            self.origin_lon_edit.setText("{:.6f}".format(lon))
            self.origin_lat_edit.setText("{:.6f}".format(lat))
        else:
            self.destination_id_edit.setText(point_id)
            self.destination_lon_edit.setText("{:.6f}".format(lon))
            self.destination_lat_edit.setText("{:.6f}".format(lat))
        self.update_point_markers()

    def set_transit_point(self, target, lon, lat, point_id):
        if target == PickTarget.TRANSIT_ORIGIN:
            self.transit_origin_id_edit.setText(point_id)
            self.transit_origin_lon_edit.setText("{:.6f}".format(lon))
            self.transit_origin_lat_edit.setText("{:.6f}".format(lat))
        else:
            self.transit_destination_id_edit.setText(point_id)
            self.transit_destination_lon_edit.setText("{:.6f}".format(lon))
            self.transit_destination_lat_edit.setText("{:.6f}".format(lat))

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

    def swap_transit_points(self):
        origin = (
            self.transit_origin_id_edit.text(),
            self.transit_origin_lon_edit.text(),
            self.transit_origin_lat_edit.text(),
        )
        destination = (
            self.transit_destination_id_edit.text(),
            self.transit_destination_lon_edit.text(),
            self.transit_destination_lat_edit.text(),
        )
        self.transit_origin_id_edit.setText(destination[0])
        self.transit_origin_lon_edit.setText(destination[1])
        self.transit_origin_lat_edit.setText(destination[2])
        self.transit_destination_id_edit.setText(origin[0])
        self.transit_destination_lon_edit.setText(origin[1])
        self.transit_destination_lat_edit.setText(origin[2])

    def clear_transit_points(self):
        for widget in [
            self.transit_origin_id_edit,
            self.transit_origin_lon_edit,
            self.transit_origin_lat_edit,
            self.transit_destination_id_edit,
            self.transit_destination_lon_edit,
            self.transit_destination_lat_edit,
        ]:
            widget.clear()
        self.transit_pick_status_label.setText(
            "Pick Origin or Pick Destination, then click on the map."
        )

    def clear_service_area_origins(self):
        self.service_area_origins_edit.clear()
        self.service_area_pick_status_label.setText(
            "Cleared the service-area origin list."
        )

    def service_area_origin_count(self):
        return len(self.parse_service_area_origin_lines())

    def parse_service_area_origin_lines(self):
        origins = []
        for raw_line in self.service_area_origins_edit.toPlainText().splitlines():
            line = raw_line.strip()
            if not line:
                continue
            parts = [part.strip() for part in line.split(",")]
            if len(parts) != 3:
                raise ValueError(
                    "Each service-area origin line must be id,lon,lat. Invalid line: {}".format(
                        raw_line
                    )
                )
            origins.append(parts)
        return origins

    def append_service_area_origin(self, point_id, lon, lat):
        lines = self.service_area_origins_edit.toPlainText().splitlines()
        lines.append("{},{:.6f},{:.6f}".format(point_id, lon, lat))
        self.service_area_origins_edit.setPlainText("\n".join(line for line in lines if line.strip()))

    def add_service_area_origins_from_selected_features(self):
        layer = self.iface.activeLayer()
        if layer is None:
            self.alert("Select a point layer with one or more selected features first.")
            return
        if QgsWkbTypes.geometryType(layer.wkbType()) != QgsWkbTypes.PointGeometry:
            self.alert("The active layer must be a point layer.")
            return
        selected_ids = layer.selectedFeatureIds()
        if not selected_ids:
            self.alert("Select one or more point features in the active layer.")
            return

        try:
            transform = QgsCoordinateTransform(layer.crs(), self.wgs84, QgsProject.instance())
            added = 0
            for feature in layer.getSelectedFeatures():
                point = self.feature_point(feature)
                wgs84_point = transform.transform(point)
                self.append_service_area_origin(
                    self.feature_label(feature, "origin"),
                    wgs84_point.x(),
                    wgs84_point.y(),
                )
                added += 1
        except Exception as exc:
            self.alert("Failed to append selected service-area origins: {}".format(exc))
            return

        self.service_area_pick_status_label.setText(
            "Added {} origin(s) from '{}'.".format(added, layer.name())
        )

    def use_route_start_for_service_area(self):
        try:
            lon = float(self.origin_lon_edit.text().strip())
            lat = float(self.origin_lat_edit.text().strip())
        except ValueError:
            self.alert("Set the route start point first.")
            return
        self.append_service_area_origin(
            self.origin_id_edit.text().strip() or "origin",
            lon,
            lat,
        )
        self.service_area_pick_status_label.setText(
            "Appended the current route start as a service-area origin."
        )

    def use_route_points_for_service_area(self):
        added = 0
        for point_id_widget, lon_widget, lat_widget, fallback_id in [
            (self.origin_id_edit, self.origin_lon_edit, self.origin_lat_edit, "origin"),
            (
                self.destination_id_edit,
                self.destination_lon_edit,
                self.destination_lat_edit,
                "destination",
            ),
        ]:
            try:
                lon = float(lon_widget.text().strip())
                lat = float(lat_widget.text().strip())
            except ValueError:
                continue
            self.append_service_area_origin(
                point_id_widget.text().strip() or fallback_id,
                lon,
                lat,
            )
            added += 1

        if added == 0:
            self.alert("Set the route start or end point first.")
            return

        self.service_area_pick_status_label.setText(
            "Appended {} route point(s) as service-area origins.".format(added)
        )

    def reuse_last_service_area_origins(self):
        raw = self.read_setting("service_area_last_origins", "")
        if not raw.strip():
            self.alert("No prior service-area origins have been saved yet.")
            return
        self.service_area_origins_edit.setPlainText(raw)
        self.service_area_pick_status_label.setText(
            "Restored the last saved service-area origins."
        )

    def reuse_last_service_area_thresholds(self):
        raw_limits = self.read_setting("service_area_last_thresholds", "")
        if not raw_limits.strip():
            self.alert("No prior service-area thresholds have been saved yet.")
            return
        self.service_area_thresholds_edit.setText(raw_limits)
        self.set_combo_by_data(
            self.service_area_threshold_metric_combo,
            self.read_setting("service_area_last_threshold_metric", "travel_time_s"),
        )
        self.log("Restored the last saved service-area threshold list.")

    def sync_service_area_hull_preset(self):
        preset = self.service_area_hull_preset_combo.currentData()
        if preset:
            self.service_area_hull_aggressiveness_edit.setText(preset)

    def next_numbered_id(self, value, fallback_prefix):
        text = (value or "").strip()
        if not text:
            return "{}_001".format(fallback_prefix)
        match = re.match(r"^(.*?)(\d+)$", text)
        if match:
            prefix, digits = match.groups()
            return "{}{:0{}d}".format(prefix, int(digits) + 1, len(digits))
        clean = text.rstrip("_- ")
        return "{}_001".format(clean or fallback_prefix)

    def route_output_path_for_id(self, route_id):
        clean_route_id = (route_id or "qgis_route_001").strip() or "qgis_route_001"
        return ".netweevil/runs/routes/{}.json".format(clean_route_id)

    def sync_route_output_path_from_route_id(self, *_args):
        if not self.route_auto_output_path_check.isChecked():
            return
        self.route_output_path_edit.setText(
            self.route_output_path_for_id(self.route_id_edit.text())
        )

    def prepare_next_route_defaults(self):
        self.route_id_edit.setText(self.next_numbered_id(self.route_id_edit.text(), "qgis_route"))

    def transit_output_path_for_id(self, route_id):
        clean_route_id = (route_id or "qgis_transit_001").strip() or "qgis_transit_001"
        return ".netweevil/runs/transit/{}.json".format(clean_route_id)

    def sync_transit_output_path_from_route_id(self, *_args):
        self.transit_output_path_edit.setText(
            self.transit_output_path_for_id(self.transit_route_id_edit.text())
        )

    def prepare_next_transit_defaults(self):
        self.transit_route_id_edit.setText(
            self.next_numbered_id(self.transit_route_id_edit.text(), "qgis_transit")
        )

    def selected_breakdown_metrics(self, distance_check, time_check):
        metrics = []
        if distance_check.isChecked():
            metrics.append("distance_m")
        if time_check.isChecked():
            metrics.append("time_s")
        return metrics

    def build_route_returns(self):
        return {
            "geometry": self.route_geometry_combo.currentData() or "full",
            "segment_rows": self.route_segment_rows_check.isChecked(),
            "road_type_breakdown": self.selected_breakdown_metrics(
                self.route_road_distance_check,
                self.route_road_time_check,
            ),
            "surface_breakdown": self.selected_breakdown_metrics(
                self.route_surface_distance_check,
                self.route_surface_time_check,
            ),
            "penalty_breakdown": self.route_penalty_breakdown_check.isChecked(),
            "explain_cost_derivation": self.route_explain_cost_derivation_check.isChecked(),
        }

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

    def build_connectivity_policy(self, prefix):
        policy = {
            "disconnected": getattr(self, "{}_connectivity_mode_combo".format(prefix)).currentData()
            or "strict"
        }
        max_hop_distance = self.parse_optional_float(
            getattr(self, "{}_max_hop_distance_edit".format(prefix)).text(),
            "Max hop distance",
        )
        if max_hop_distance is not None:
            policy["max_hop_distance_m"] = max_hop_distance
        if getattr(self, "{}_report_hop_distance_check".format(prefix)).isChecked():
            policy["report_hop_distance_separately"] = True
        return policy

    def build_fallback_policy(self, prefix):
        allow_reverse_oneway = getattr(
            self, "{}_allow_reverse_oneway_check".format(prefix)
        ).isChecked()
        allow_illegal_turn = getattr(
            self, "{}_allow_illegal_turn_check".format(prefix)
        ).isChecked()
        ignore_turn_restrictions = getattr(
            self, "{}_ignore_turn_restrictions_check".format(prefix)
        ).isChecked()
        allow_uturn = getattr(self, "{}_allow_uturn_check".format(prefix)).isChecked()
        penalties = {}
        reverse_penalty = self.parse_optional_float(
            getattr(self, "{}_reverse_penalty_edit".format(prefix)).text(),
            "Reverse oneway penalty",
        )
        illegal_turn_penalty = self.parse_optional_float(
            getattr(self, "{}_illegal_turn_penalty_edit".format(prefix)).text(),
            "Illegal turn penalty",
        )
        ignored_restriction_penalty = self.parse_optional_float(
            getattr(self, "{}_ignored_restriction_penalty_edit".format(prefix)).text(),
            "Ignored restriction penalty",
        )
        forbidden_uturn_penalty = self.parse_optional_float(
            getattr(self, "{}_forbidden_uturn_penalty_edit".format(prefix)).text(),
            "Forbidden U-turn penalty",
        )
        if reverse_penalty is not None:
            penalties["reverse_oneway_penalty_s"] = reverse_penalty
        if illegal_turn_penalty is not None:
            penalties["illegal_turn_penalty_s"] = illegal_turn_penalty
        if ignored_restriction_penalty is not None:
            penalties["ignored_turn_restriction_penalty_s"] = ignored_restriction_penalty
        if forbidden_uturn_penalty is not None:
            penalties["forbidden_uturn_penalty_s"] = forbidden_uturn_penalty

        policy = {
            "allow_reverse_oneway": allow_reverse_oneway,
            "allow_illegal_turn": allow_illegal_turn,
            "ignore_turn_restrictions": ignore_turn_restrictions,
            "allow_uturn_where_normally_forbidden": allow_uturn,
            "auto_relax_unreachable": getattr(
                self, "{}_auto_relax_unreachable_check".format(prefix)
            ).isChecked(),
        }
        if penalties:
            policy["penalties"] = penalties

        max_illegal_distance = self.parse_optional_float(
            getattr(self, "{}_max_illegal_distance_edit".format(prefix)).text(),
            "Max illegal distance",
        )
        if max_illegal_distance is not None:
            policy["max_illegal_distance_m"] = max_illegal_distance

        max_illegal_turns = self.parse_optional_int(
            getattr(self, "{}_max_illegal_turns_edit".format(prefix)).text(),
            "Max illegal turns",
        )
        if max_illegal_turns is not None:
            policy["max_illegal_turns"] = max_illegal_turns

        return policy

    def has_unsafe_failure_modes(self, prefix):
        policy = self.build_fallback_policy(prefix)
        return any(
            policy.get(flag)
            for flag in [
                "allow_reverse_oneway",
                "allow_illegal_turn",
                "ignore_turn_restrictions",
                "allow_uturn_where_normally_forbidden",
                "auto_relax_unreachable",
            ]
        )

    def confirm_unsafe_failure_modes(self, prefix, analysis_label):
        if not self.has_unsafe_failure_modes(prefix):
            return True

        policy = self.build_fallback_policy(prefix)
        enabled = []
        for key, label in [
            (
                "auto_relax_unreachable",
                "auto least-permissive degraded route recovery",
            ),
            ("allow_reverse_oneway", "reverse oneway"),
            ("allow_illegal_turn", "illegal turns"),
            ("ignore_turn_restrictions", "ignored turn restrictions"),
            ("allow_uturn_where_normally_forbidden", "forbidden U-turns"),
        ]:
            if policy.get(key):
                enabled.append(label)

        reply = QMessageBox.warning(
            self,
            "netweevil unsafe analysis",
            "This {} request enables unsafe degraded-routing modes: {}.\n\n"
            "These options are explicit fallback analysis only. Continue?".format(
                analysis_label, ", ".join(enabled)
            ),
            message_box_button("Yes") | message_box_button("No"),
            message_box_button("No"),
        )
        return reply == message_box_button("Yes")

    def build_service_area_thresholds(self):
        metric = self.service_area_threshold_metric_combo.currentData() or "travel_time_s"
        thresholds = []
        for index, value in enumerate(self.service_area_thresholds_edit.text().split(","), start=1):
            raw = value.strip()
            if not raw:
                continue
            try:
                limit = float(raw)
            except ValueError as exc:
                raise ValueError(
                    "Service-area threshold '{}' is not a number".format(raw)
                ) from exc
            threshold_id = "{}_{:02d}".format(metric, index)
            thresholds.append({"id": threshold_id, "limit": limit, "metric": metric})

        if not thresholds:
            raise ValueError("Enter at least one service-area threshold.")
        return thresholds

    def build_service_area_origins(self):
        origins = []
        for point_id, lon_text, lat_text in self.parse_service_area_origin_lines():
            try:
                lon = float(lon_text)
                lat = float(lat_text)
            except ValueError as exc:
                raise ValueError(
                    "Service-area origin '{}' has invalid lon/lat values".format(point_id)
                ) from exc
            origins.append({"id": point_id, "lon": lon, "lat": lat})

        if not origins:
            raise ValueError("Enter at least one service-area origin.")
        return origins

    def build_service_area_request(self):
        output_mode = self.service_area_output_mode_combo.currentData() or "both"
        request = {
            "analysis_id": self.service_area_analysis_id_edit.text().strip()
            or "qgis_service_area",
            "origins": self.build_service_area_origins(),
            "thresholds": self.build_service_area_thresholds(),
            "snap": {
                "max_distance_m": float(self.service_area_snap_distance_edit.text().strip())
            },
            "connectivity": self.build_connectivity_policy("service_area"),
            "fallback": {
                "allow_reverse_oneway": False,
                "allow_illegal_turn": False,
                "ignore_turn_restrictions": False,
                "allow_uturn_where_normally_forbidden": False,
            },
            "output_mode": output_mode,
            "band_mode": self.service_area_band_mode_combo.currentData() or "cumulative",
            "boundary_mode": self.service_area_boundary_mode_combo.currentData() or "overlap",
            "multi_origin_mode": self.service_area_multi_origin_mode_combo.currentData()
            or "merge",
            "polygon": {
                "hull_aggressiveness": float(
                    self.service_area_hull_aggressiveness_edit.text().strip() or "1.0"
                )
            },
            "returns": {
                "geometry": True,
                "attributes": True,
                "per_threshold_summary": True,
                "diagnostics": True,
            },
        }
        simplification_tolerance = self.parse_optional_float(
            self.service_area_simplification_edit.text(),
            "Service-area simplification tolerance",
        )
        if simplification_tolerance is not None:
            request["polygon"]["simplification_tolerance_m"] = simplification_tolerance
        return request

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
            "connectivity": self.build_connectivity_policy("route"),
            "fallback": self.build_fallback_policy("route"),
            "returns": self.build_route_returns(),
        }

    def selected_transit_feed_id(self):
        return self.transit_feed_combo.currentData() or ""

    def selected_transit_modes(self):
        modes = [
            mode
            for mode, check in sorted(self.transit_mode_checks.items())
            if check.isChecked()
        ]
        if not modes:
            raise ValueError("Choose at least one transit mode.")
        return modes

    def build_transit_request(self):
        return {
            "route_id": self.transit_route_id_edit.text().strip() or "qgis_transit",
            "origin": {
                "id": self.transit_origin_id_edit.text().strip() or "origin",
                "lon": float(self.transit_origin_lon_edit.text().strip()),
                "lat": float(self.transit_origin_lat_edit.text().strip()),
            },
            "destination": {
                "id": self.transit_destination_id_edit.text().strip() or "destination",
                "lon": float(self.transit_destination_lon_edit.text().strip()),
                "lat": float(self.transit_destination_lat_edit.text().strip()),
            },
            "time": {
                "datetime": self.transit_datetime_edit.text().strip(),
                "arrive_by": False,
                "search_window_s": self.parse_optional_int(
                    self.transit_search_window_edit.text(),
                    "Transit search window",
                )
                or 7200,
            },
            "modes": {
                "access": ["walk"],
                "egress": ["walk"],
                "transit": self.selected_transit_modes(),
                "walk_speed_kph": float(self.transit_walk_speed_edit.text().strip()),
                "max_access_distance_m": float(
                    self.transit_max_access_distance_edit.text().strip()
                ),
                "max_egress_distance_m": float(
                    self.transit_max_egress_distance_edit.text().strip()
                ),
                "max_transfer_distance_m": float(
                    self.transit_max_transfer_distance_edit.text().strip()
                ),
                "board_slack_s": int(self.transit_board_slack_edit.text().strip()),
                "transfer_slack_s": int(self.transit_transfer_slack_edit.text().strip()),
                "max_transfers": int(self.transit_max_transfers_edit.text().strip()),
            },
            "returns": {
                "include_geometry": self.transit_include_geometry_check.isChecked(),
                "walking_geometry": (
                    "network"
                    if self.transit_network_walk_geometry_check.isChecked()
                    else "straight_line"
                ),
                "include_stops": self.transit_include_stops_check.isChecked(),
                "include_stop_segments": self.transit_include_stop_segments_check.isChecked(),
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
        try:
            allowed = self.confirm_unsafe_failure_modes("route", "route")
        except ValueError as exc:
            self.alert("Invalid route advanced options: {}".format(exc))
            return
        if not allowed:
            self.log("Cancelled the route request before sending unsafe fallback options.")
            return

        profile_id = self.selected_profile_id()
        if profile_id:
            payload["profile_id"] = profile_id

        self.save_settings()
        if self.response_format() == ResponseFormat.GEOJSON:
            self.log(
                "Route requests use JSON internally so segmented rows, hops, violations, and breakdown tables remain available."
            )
        if self.execute_api_request(
            endpoint="/v1/route",
            payload=payload,
            output_path=self.route_output_path_edit.text(),
            layer_name=payload["request"]["route_id"] or "netweevil_route",
            analysis_kind="route",
            response_format_override=ResponseFormat.JSON,
        ):
            if self.route_auto_increment_check.isChecked():
                self.prepare_next_route_defaults()

    def write_transit_request(self):
        try:
            request = self.build_transit_request()
        except ValueError as exc:
            self.alert("Invalid transit request values: {}".format(exc))
            return

        request_path = self.resolve_local_path(self.transit_request_path_edit.text())
        request_path.parent.mkdir(parents=True, exist_ok=True)
        request_path.write_text(json.dumps(request, indent=2), encoding="utf-8")
        self.log("Wrote transit route request to {}".format(request_path))
        self.save_settings()

    def run_transit_route(self):
        if self.service_info is None:
            self.alert("Refresh the API service first.")
            return
        feed_id = self.selected_transit_feed_id()
        if not feed_id:
            self.alert(
                "The API has no loaded transit feed. Start it with --transit-feed <feed_id>."
            )
            return
        try:
            request = self.build_transit_request()
        except ValueError as exc:
            self.alert("Invalid transit request values: {}".format(exc))
            return

        payload = {"feed_id": feed_id, "request": request}
        if self.transit_network_walk_geometry_check.isChecked():
            profile_id = self.selected_profile_id()
            if not profile_id:
                self.alert("Choose a loaded pedestrian profile for transit walking geometry.")
                return
            payload["pedestrian_profile_id"] = profile_id
        self.save_settings()
        if self.execute_api_request(
            endpoint="/v1/transit-route",
            payload=payload,
            output_path=self.transit_output_path_edit.text(),
            layer_name=request["route_id"] or "netweevil_transit",
            analysis_kind="transit_route",
            response_format_override=ResponseFormat.JSON,
        ):
            if self.transit_auto_increment_check.isChecked():
                self.prepare_next_transit_defaults()

    def run_od(self):
        if self.service_info is None:
            self.alert("Refresh the API service first.")
            return
        try:
            document = self.load_od_document(
                self.resolve_local_path(self.od_pairs_path_edit.text())
            )
            document["connectivity"] = self.build_connectivity_policy("batch")
            document["fallback"] = self.build_fallback_policy("batch")
        except Exception as exc:
            self.alert("Failed to load OD input: {}".format(exc))
            return
        try:
            allowed = self.confirm_unsafe_failure_modes("batch", "OD")
        except ValueError as exc:
            self.alert("Invalid batch advanced options: {}".format(exc))
            return
        if not allowed:
            self.log("Cancelled the OD request before sending unsafe fallback options.")
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
            layer_name="netweevil_od",
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
            connectivity = self.build_connectivity_policy("batch")
            fallback = self.build_fallback_policy("batch")
        except Exception as exc:
            self.alert("Failed to load matrix input: {}".format(exc))
            return
        origins["connectivity"] = connectivity
        origins["fallback"] = fallback
        destinations["connectivity"] = connectivity
        destinations["fallback"] = fallback
        try:
            allowed = self.confirm_unsafe_failure_modes("batch", "matrix")
        except ValueError as exc:
            self.alert("Invalid batch advanced options: {}".format(exc))
            return
        if not allowed:
            self.log("Cancelled the matrix request before sending unsafe fallback options.")
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
            layer_name="netweevil_matrix",
            analysis_kind="matrix",
        )

    def write_service_area_request(self):
        try:
            request = self.build_service_area_request()
        except ValueError as exc:
            self.alert("Invalid service-area request values: {}".format(exc))
            return

        request_path = self.resolve_local_path(self.service_area_request_path_edit.text())
        request_path.parent.mkdir(parents=True, exist_ok=True)
        request_path.write_text(json.dumps(request, indent=2), encoding="utf-8")
        self.log("Wrote service-area request to {}".format(request_path))
        self.remember_service_area_request(request)
        self.save_settings()

    def run_service_area(self):
        if self.service_info is None:
            self.alert("Refresh the API service first.")
            return
        try:
            request = self.build_service_area_request()
        except ValueError as exc:
            self.alert("Invalid service-area request values: {}".format(exc))
            return

        payload = {"request": request}
        profile_id = self.selected_profile_id()
        if profile_id:
            payload["profile_id"] = profile_id

        self.remember_service_area_request(request)
        self.save_settings()
        self.execute_api_request(
            endpoint="/v1/service-area",
            payload=payload,
            output_path=self.service_area_output_path_edit.text(),
            layer_name=request["analysis_id"] or "netweevil_service_area",
            analysis_kind="service_area",
        )

    def remember_service_area_request(self, request):
        settings = QSettings()
        origin_lines = [
            "{id},{lon:.6f},{lat:.6f}".format(**origin) for origin in request.get("origins", [])
        ]
        threshold_limits = ",".join(
            "{:.6f}".format(threshold["limit"]).rstrip("0").rstrip(".")
            for threshold in request.get("thresholds", [])
        )
        settings.setValue(
            "{}/service_area_last_origins".format(SETTINGS_PREFIX), "\n".join(origin_lines)
        )
        settings.setValue(
            "{}/service_area_last_thresholds".format(SETTINGS_PREFIX), threshold_limits
        )
        if request.get("thresholds"):
            settings.setValue(
                "{}/service_area_last_threshold_metric".format(SETTINGS_PREFIX),
                request["thresholds"][0].get("metric", "travel_time_s"),
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

    def execute_api_request(
        self,
        endpoint,
        payload,
        output_path,
        layer_name,
        analysis_kind,
        response_format_override=None,
    ):
        url = self.service_url(
            endpoint,
            include_format=True,
            response_format=response_format_override,
        )
        self.log("POST {}".format(url))
        try:
            content_type, body = self.http_post_json(url, payload)
        except Exception as exc:
            self.alert("API request failed: {}".format(exc))
            return False

        local_output_path = self.resolve_local_path(output_path)
        saved_path = self.save_response(local_output_path, content_type, body)
        self.log("Saved API response to {}".format(saved_path))

        if "geo+json" in content_type or saved_path.suffix.lower() == ".geojson":
            try:
                geojson = json.loads(body.decode("utf-8"))
            except Exception as exc:
                self.log(
                    "Failed to parse GeoJSON response for logging; loading the raw layer instead: {}".format(
                        exc
                    ),
                    Qgis.Warning,
                )
                loaded_layer = self.load_output_layer(saved_path, layer_name)
                if loaded_layer is not None:
                    self.set_last_output_layers([loaded_layer])
                return True

            self.log_geojson_messages(analysis_kind, geojson)
            if analysis_kind == "service_area":
                self.load_service_area_layers(geojson, layer_name)
                return True

            loaded_layer = self.load_output_layer(saved_path, layer_name)
            if loaded_layer is not None:
                self.set_last_output_layers([loaded_layer])
            return True

        try:
            response_json = json.loads(body.decode("utf-8"))
        except Exception as exc:
            self.alert("Failed to parse API JSON response: {}".format(exc))
            return False

        self.log_analysis_messages(analysis_kind, response_json)

        if analysis_kind == "route":
            self.load_route_layers(response_json, layer_name)
            return True

        if analysis_kind == "transit_route":
            self.load_transit_route_layers(response_json, layer_name)
            return True

        geojson = self.analysis_json_to_geojson(analysis_kind, response_json)
        if geojson is None:
            self.log(
                "Response saved, but no spatial geometry could be built from the API result.",
                Qgis.Warning,
            )
            return True

        if analysis_kind == "service_area":
            self.load_service_area_layers(geojson, layer_name)
            return True

        temp_path = self.write_temp_geojson(layer_name, geojson)
        loaded_layer = self.load_output_layer(temp_path, layer_name)
        if loaded_layer is not None:
            self.set_last_output_layers([loaded_layer])
        return True

    def refresh_saved_runs(self):
        directory = self.resolve_local_path(self.saved_runs_directory_edit.text())
        self.saved_runs_combo.clear()
        if not directory.exists():
            self.saved_runs_combo.addItem("Runs directory does not exist", "")
            self.log("Runs directory does not exist: {}".format(directory), Qgis.Warning)
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
                diagnostics = parsed.get("diagnostics") or []
                if diagnostics:
                    detail_lines = []
                    for diagnostic in diagnostics:
                        detail_lines.append(
                            "{}: {}".format(
                                diagnostic.get("code", "diagnostic"),
                                diagnostic.get("message", ""),
                            ).strip()
                        )
                        for action in diagnostic.get("suggested_actions") or []:
                            detail_lines.append("next: {}".format(action))
                    message = "{}\n{}".format(message, "\n".join(detail_lines))
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

    def json_text(self, value):
        return json.dumps(value, sort_keys=True) if value not in [None, ""] else ""

    def route_summary_feature_collection(self, service, result):
        summary = result.get("summary") or {}
        geometry = result.get("geometry")
        return {
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": self.item_geometry(geometry),
                    "properties": {
                        "dataset_id": service.get("dataset_id"),
                        "profile_id": service.get("profile_id"),
                        "profile_hash": service.get("profile_hash"),
                        "route_id": result.get("route_id"),
                        "outcome": result.get("outcome"),
                        "fallback_used": result.get("fallback_used"),
                        "network_distance_m": summary.get("network_distance_m"),
                        "network_travel_time_s": summary.get("network_travel_time_s"),
                        "network_generalized_cost": summary.get(
                            "network_generalized_cost"
                        ),
                        "illegal_movement_penalty_s": summary.get(
                            "illegal_movement_penalty_s"
                        ),
                        "illegal_movement_penalty_cost": summary.get(
                            "illegal_movement_penalty_cost"
                        ),
                        "violation_count": summary.get("violation_count"),
                        "violation_types_json": self.json_text(
                            summary.get("violation_types") or []
                        ),
                        "total_distance_m": summary.get("total_distance_m"),
                        "total_travel_time_s": summary.get("total_travel_time_s"),
                        "total_generalized_cost": summary.get(
                            "total_generalized_cost"
                        ),
                        "segment_count": summary.get("segment_count"),
                        "origin_point_id": result.get("origin", {}).get("point_id"),
                        "destination_point_id": result.get("destination", {}).get(
                            "point_id"
                        ),
                        "origin_component_id": result.get("origin", {}).get(
                            "component_id"
                        ),
                        "destination_component_id": result.get("destination", {}).get(
                            "component_id"
                        ),
                        "origin_snap_distance_m": result.get("origin", {}).get(
                            "snap_distance_m"
                        ),
                        "destination_snap_distance_m": result.get("destination", {}).get(
                            "snap_distance_m"
                        ),
                        "origin_hop_distance_m": result.get("origin_hop_distance_m"),
                        "destination_hop_distance_m": result.get(
                            "destination_hop_distance_m"
                        ),
                        "warnings_json": self.json_text(result.get("warnings") or []),
                    },
                }
            ],
        }

    def route_distance_area(self):
        distance = QgsDistanceArea()
        try:
            distance.setSourceCrs(
                self.wgs84,
                QgsProject.instance().transformContext(),
            )
        except Exception:
            pass
        try:
            distance.setEllipsoid("WGS84")
        except Exception:
            pass
        return distance

    def point_at_distance(self, coordinates, cumulative_lengths, distance_m):
        if distance_m <= 0.0:
            return list(coordinates[0])
        if distance_m >= cumulative_lengths[-1]:
            return list(coordinates[-1])
        for index in range(len(cumulative_lengths) - 1):
            start_distance = cumulative_lengths[index]
            end_distance = cumulative_lengths[index + 1]
            if distance_m <= end_distance + 1.0e-9:
                if end_distance - start_distance <= 1.0e-9:
                    return list(coordinates[index + 1])
                ratio = (distance_m - start_distance) / (end_distance - start_distance)
                start = coordinates[index]
                end = coordinates[index + 1]
                return [
                    start[0] + (end[0] - start[0]) * ratio,
                    start[1] + (end[1] - start[1]) * ratio,
                ]
        return list(coordinates[-1])

    def dedupe_coordinates(self, coordinates):
        if not coordinates:
            return coordinates
        deduped = [coordinates[0]]
        for coordinate in coordinates[1:]:
            previous = deduped[-1]
            if (
                abs(previous[0] - coordinate[0]) > 1.0e-12
                or abs(previous[1] - coordinate[1]) > 1.0e-12
            ):
                deduped.append(coordinate)
        return deduped

    def slice_route_geometry(self, coordinates, cumulative_lengths, start_m, end_m):
        if not coordinates:
            return None
        if end_m <= start_m + 1.0e-9:
            point = self.point_at_distance(coordinates, cumulative_lengths, start_m)
            return [point, point]
        sliced = [self.point_at_distance(coordinates, cumulative_lengths, start_m)]
        for index in range(1, len(coordinates) - 1):
            distance_m = cumulative_lengths[index]
            if start_m < distance_m < end_m:
                sliced.append(list(coordinates[index]))
        sliced.append(self.point_at_distance(coordinates, cumulative_lengths, end_m))
        return self.dedupe_coordinates(sliced)

    def split_route_geometry_by_distance(self, coordinates, segment_lengths_m):
        if len(coordinates) < 2 or not segment_lengths_m:
            return [None for _ in segment_lengths_m]
        distance = self.route_distance_area()
        cumulative_lengths = [0.0]
        for start, end in zip(coordinates, coordinates[1:]):
            segment_distance = distance.measureLine(
                QgsPointXY(start[0], start[1]),
                QgsPointXY(end[0], end[1]),
            )
            cumulative_lengths.append(cumulative_lengths[-1] + max(segment_distance, 0.0))
        total_geometry_length = cumulative_lengths[-1]
        total_segment_length = sum(max(float(length), 0.0) for length in segment_lengths_m)
        if total_geometry_length <= 0.0 or total_segment_length <= 0.0:
            return [None for _ in segment_lengths_m]
        scale = total_geometry_length / total_segment_length
        segment_geometries = []
        start_m = 0.0
        for index, length_m in enumerate(segment_lengths_m):
            scaled_length = max(float(length_m), 0.0) * scale
            end_m = total_geometry_length if index == len(segment_lengths_m) - 1 else min(
                total_geometry_length, start_m + scaled_length
            )
            segment_geometries.append(
                self.slice_route_geometry(
                    coordinates,
                    cumulative_lengths,
                    start_m,
                    end_m,
                )
            )
            start_m = end_m
        return segment_geometries

    def route_segment_feature_collection(self, service, result):
        segments = result.get("segments") or []
        if not segments:
            return {"type": "FeatureCollection", "features": []}
        route_geometry = result.get("geometry") or []
        segment_geometries = self.split_route_geometry_by_distance(
            route_geometry,
            [segment.get("length_m") or 0 for segment in segments],
        )
        features = []
        for index, segment in enumerate(segments, start=1):
            geometry = None
            if index - 1 < len(segment_geometries):
                geometry = self.item_geometry(segment_geometries[index - 1])
            features.append(
                {
                    "type": "Feature",
                    "geometry": geometry,
                    "properties": {
                        "dataset_id": service.get("dataset_id"),
                        "profile_id": service.get("profile_id"),
                        "profile_hash": service.get("profile_hash"),
                        "route_id": result.get("route_id"),
                        "segment_index": index,
                        "edge_id": segment.get("edge_id"),
                        "from_node_id": segment.get("from_node_id"),
                        "to_node_id": segment.get("to_node_id"),
                        "source_way_id": segment.get("source_way_id"),
                        "length_m": segment.get("length_m"),
                        "travel_time_s": segment.get("travel_time_s"),
                        "generalized_cost": segment.get("generalized_cost"),
                        "road_class": segment.get("road_class"),
                        "surface": segment.get("surface"),
                        "name": segment.get("name"),
                        "violation_type": segment.get("violation_type"),
                    },
                }
            )
        return {"type": "FeatureCollection", "features": features}

    def route_hop_feature_collection(self, service, result):
        features = []
        for index, hop in enumerate(result.get("hop_segments") or [], start=1):
            features.append(
                {
                    "type": "Feature",
                    "geometry": self.item_geometry(hop.get("geometry")),
                    "properties": {
                        "dataset_id": service.get("dataset_id"),
                        "profile_id": service.get("profile_id"),
                        "profile_hash": service.get("profile_hash"),
                        "route_id": result.get("route_id"),
                        "hop_index": index,
                        "endpoint": hop.get("endpoint"),
                        "distance_m": hop.get("distance_m"),
                    },
                }
            )
        return {"type": "FeatureCollection", "features": features}

    def route_violation_feature_collection(self, service, result):
        features = []
        for index, violation in enumerate(result.get("violations") or [], start=1):
            features.append(
                {
                    "type": "Feature",
                    "geometry": None,
                    "properties": {
                        "dataset_id": service.get("dataset_id"),
                        "profile_id": service.get("profile_id"),
                        "profile_hash": service.get("profile_hash"),
                        "route_id": result.get("route_id"),
                        "violation_index": index,
                        "violation_type": violation.get("violation_type"),
                        "edge_id": violation.get("edge_id"),
                        "from_edge_id": violation.get("from_edge_id"),
                        "to_edge_id": violation.get("to_edge_id"),
                        "distance_m": violation.get("distance_m"),
                        "penalty_s": violation.get("penalty_s"),
                        "penalty_generalized_cost": violation.get(
                            "penalty_generalized_cost"
                        ),
                    },
                }
            )
        return {"type": "FeatureCollection", "features": features}

    def route_breakdown_feature_collection(
        self,
        service,
        result,
        breakdown_type,
        breakdown_values,
    ):
        features = []
        for category, metrics in sorted((breakdown_values or {}).items()):
            features.append(
                {
                    "type": "Feature",
                    "geometry": None,
                    "properties": {
                        "dataset_id": service.get("dataset_id"),
                        "profile_id": service.get("profile_id"),
                        "profile_hash": service.get("profile_hash"),
                        "route_id": result.get("route_id"),
                        "breakdown_type": breakdown_type,
                        "category": category,
                        "distance_m": (metrics or {}).get("distance_m"),
                        "time_s": (metrics or {}).get("time_s"),
                    },
                }
            )
        return {"type": "FeatureCollection", "features": features}

    def transit_route_coordinates(self, result):
        coordinates = []
        for leg in result.get("legs") or []:
            geometry = leg.get("geometry") or []
            if not geometry:
                continue
            if coordinates and geometry[0] == coordinates[-1]:
                coordinates.extend(geometry[1:])
            else:
                coordinates.extend(geometry)
        return self.dedupe_coordinates(coordinates)

    def transit_summary_feature_collection(self, service, result):
        summary = result.get("summary") or {}
        coordinates = self.transit_route_coordinates(result)
        return {
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "geometry": self.item_geometry(coordinates),
                    "properties": {
                        "feed_id": service.get("feed_id"),
                        "service_start_date": service.get("service_start_date"),
                        "service_days": service.get("service_days"),
                        "route_engine": service.get("route_engine"),
                        "route_id": result.get("route_id"),
                        "outcome": result.get("outcome"),
                        "departure_s": summary.get("departure_s"),
                        "arrival_s": summary.get("arrival_s"),
                        "total_travel_time_s": summary.get("total_travel_time_s"),
                        "transit_time_s": summary.get("transit_time_s"),
                        "access_egress_time_s": summary.get("access_egress_time_s"),
                        "transfer_time_s": summary.get("transfer_time_s"),
                        "wait_time_s": summary.get("wait_time_s"),
                        "boarding_count": summary.get("boarding_count"),
                        "diagnostics_json": self.json_text(result.get("diagnostics") or []),
                    },
                }
            ],
        }

    def transit_leg_feature_collection(self, service, result):
        features = []
        for index, leg in enumerate(result.get("legs") or [], start=1):
            features.append(
                {
                    "type": "Feature",
                    "geometry": self.item_geometry(leg.get("geometry")),
                    "properties": {
                        "feed_id": service.get("feed_id"),
                        "route_id": result.get("route_id"),
                        "leg_index": index,
                        "leg_type": leg.get("leg_type"),
                        "from_id": leg.get("from_id"),
                        "to_id": leg.get("to_id"),
                        "from_name": leg.get("from_name"),
                        "to_name": leg.get("to_name"),
                        "departure_s": leg.get("departure_s"),
                        "arrival_s": leg.get("arrival_s"),
                        "duration_s": (
                            leg.get("arrival_s") - leg.get("departure_s")
                            if leg.get("arrival_s") is not None
                            and leg.get("departure_s") is not None
                            else None
                        ),
                        "mode": leg.get("mode"),
                        "gtfs_route_id": leg.get("route_id"),
                        "route_short_name": leg.get("route_short_name"),
                        "trip_id": leg.get("trip_id"),
                        "headsign": leg.get("headsign"),
                    },
                }
            )
        return {"type": "FeatureCollection", "features": features}

    def transit_stop_feature_collection(self, service, result):
        features = []
        for stop in result.get("stops") or []:
            features.append(
                {
                    "type": "Feature",
                    "geometry": {
                        "type": "Point",
                        "coordinates": [stop.get("lon"), stop.get("lat")],
                    },
                    "properties": {
                        "feed_id": service.get("feed_id"),
                        "route_id": result.get("route_id"),
                        "sequence": stop.get("sequence"),
                        "stop_id": stop.get("stop_id"),
                        "stop_name": stop.get("stop_name"),
                        "arrival_s": stop.get("arrival_s"),
                        "departure_s": stop.get("departure_s"),
                        "mode": stop.get("mode"),
                        "gtfs_route_id": stop.get("route_id"),
                        "route_short_name": stop.get("route_short_name"),
                        "trip_id": stop.get("trip_id"),
                        "headsign": stop.get("headsign"),
                    },
                }
            )
        return {"type": "FeatureCollection", "features": features}

    def transit_stop_segment_feature_collection(self, service, result):
        features = []
        for segment in result.get("stop_segments") or []:
            features.append(
                {
                    "type": "Feature",
                    "geometry": self.item_geometry(segment.get("geometry")),
                    "properties": {
                        "feed_id": service.get("feed_id"),
                        "route_id": result.get("route_id"),
                        "segment_index": segment.get("segment_index"),
                        "from_stop_id": segment.get("from_stop_id"),
                        "to_stop_id": segment.get("to_stop_id"),
                        "from_stop_name": segment.get("from_stop_name"),
                        "to_stop_name": segment.get("to_stop_name"),
                        "departure_s": segment.get("departure_s"),
                        "arrival_s": segment.get("arrival_s"),
                        "duration_s": segment.get("duration_s"),
                        "mode": segment.get("mode"),
                        "gtfs_route_id": segment.get("route_id"),
                        "route_short_name": segment.get("route_short_name"),
                        "trip_id": segment.get("trip_id"),
                        "headsign": segment.get("headsign"),
                    },
                }
            )
        return {"type": "FeatureCollection", "features": features}

    def apply_route_line_style(self, layer, color, width, line_style="solid"):
        if QgsWkbTypes.geometryType(layer.wkbType()) != QgsWkbTypes.LineGeometry:
            return
        symbol = QgsLineSymbol.createSimple(
            {
                "line_color": color,
                "line_width": str(width),
                "line_style": line_style,
            }
        )
        layer.renderer().setSymbol(symbol)
        layer.triggerRepaint()

    def apply_transit_leg_style(self, layer):
        if QgsWkbTypes.geometryType(layer.wkbType()) != QgsWkbTypes.LineGeometry:
            return
        specs = {
            "access": ("#2b8a3e", "dash"),
            "egress": ("#2b8a3e", "dash"),
            "transfer": ("#d9480f", "dot"),
            "transit": ("#1971c2", "solid"),
        }
        categories = []
        for leg_type, (color, line_style) in specs.items():
            symbol = QgsLineSymbol.createSimple(
                {
                    "line_color": color,
                    "line_width": "1.0" if leg_type == "transit" else "0.8",
                    "line_style": line_style,
                }
            )
            categories.append(QgsRendererCategory(leg_type, symbol, leg_type))
        layer.setRenderer(QgsCategorizedSymbolRenderer("leg_type", categories))
        layer.triggerRepaint()

    def apply_transit_stop_style(self, layer):
        if QgsWkbTypes.geometryType(layer.wkbType()) != QgsWkbTypes.PointGeometry:
            return
        symbol = QgsMarkerSymbol.createSimple(
            {
                "name": "circle",
                "color": "#ffffff",
                "outline_color": "#1971c2",
                "outline_width": "0.6",
                "size": "2.8",
            }
        )
        layer.renderer().setSymbol(symbol)
        layer.triggerRepaint()

    def create_result_group(self, name):
        root = QgsProject.instance().layerTreeRoot()
        existing_group = root.findGroup(name)
        if existing_group is not None:
            for tree_layer in existing_group.findLayers():
                QgsProject.instance().removeMapLayer(tree_layer.layerId())
            root.removeChildNode(existing_group)
        return root.insertGroup(0, name)

    def load_route_layers(self, response_json, layer_name):
        service = response_json.get("service") or {}
        result = response_json.get("result") or {}
        route_id = result.get("route_id") or layer_name or "netweevil_route"
        group = self.create_result_group(route_id)

        breakdowns = result.get("breakdowns") or {}
        layer_specs = [
            ("route", self.route_summary_feature_collection(service, result), "#0b7285", 1.4, "solid"),
            (
                "segments",
                self.route_segment_feature_collection(service, result),
                "#2b8a3e",
                0.9,
                "solid",
            ),
            (
                "hop_segments",
                self.route_hop_feature_collection(service, result),
                "#d9480f",
                1.1,
                "dash",
            ),
            (
                "violations",
                self.route_violation_feature_collection(service, result),
                None,
                None,
                None,
            ),
            (
                "road_type_breakdown",
                self.route_breakdown_feature_collection(
                    service,
                    result,
                    "road_class",
                    breakdowns.get("road_class"),
                ),
                None,
                None,
                None,
            ),
            (
                "surface_breakdown",
                self.route_breakdown_feature_collection(
                    service,
                    result,
                    "surface",
                    breakdowns.get("surface"),
                ),
                None,
                None,
                None,
            ),
        ]

        loaded_layers = []
        for sublayer_name, geojson, color, width, line_style in layer_specs:
            features = geojson.get("features") or []
            if not features:
                continue
            temp_path = self.write_temp_geojson(
                "{}_{}".format(route_id, sublayer_name),
                geojson,
            )
            layer = QgsVectorLayer(str(temp_path), sublayer_name, "ogr")
            if not layer.isValid():
                self.log("Failed to load layer {}".format(temp_path), Qgis.Warning)
                continue
            if color is not None:
                self.apply_route_line_style(layer, color, width, line_style)
            QgsProject.instance().addMapLayer(layer, False)
            group.addLayer(layer)
            loaded_layers.append(layer)
            self.log(
                "Loaded route layer '{}' with {} feature(s).".format(
                    sublayer_name, len(features)
                )
            )

        if loaded_layers:
            self.set_last_output_layers(loaded_layers)
        else:
            self.log("Route response had no loadable layers or tables.", Qgis.Warning)

    def load_transit_route_layers(self, response_json, layer_name):
        service = response_json.get("service") or {}
        result = response_json.get("result") or {}
        route_id = result.get("route_id") or layer_name or "netweevil_transit"
        group = self.create_result_group(route_id)

        layer_specs = [
            (
                "transit_route",
                self.transit_summary_feature_collection(service, result),
                "#0b7285",
                1.6,
                "solid",
            ),
            ("legs", self.transit_leg_feature_collection(service, result), None, None, None),
            (
                "stop_segments",
                self.transit_stop_segment_feature_collection(service, result),
                "#7048e8",
                0.8,
                "solid",
            ),
            (
                "stops",
                self.transit_stop_feature_collection(service, result),
                None,
                None,
                None,
            ),
        ]

        loaded_layers = []
        for sublayer_name, geojson, color, width, line_style in layer_specs:
            features = geojson.get("features") or []
            if not features:
                continue
            temp_path = self.write_temp_geojson(
                "{}_{}".format(route_id, sublayer_name),
                geojson,
            )
            layer = QgsVectorLayer(str(temp_path), sublayer_name, "ogr")
            if not layer.isValid():
                self.log("Failed to load layer {}".format(temp_path), Qgis.Warning)
                continue
            if sublayer_name == "legs":
                self.apply_transit_leg_style(layer)
            elif sublayer_name == "stops":
                self.apply_transit_stop_style(layer)
            elif color is not None:
                self.apply_route_line_style(layer, color, width, line_style)
            QgsProject.instance().addMapLayer(layer, False)
            group.addLayer(layer)
            loaded_layers.append(layer)
            self.log(
                "Loaded transit layer '{}' with {} feature(s).".format(
                    sublayer_name, len(features)
                )
            )

        if loaded_layers:
            self.set_last_output_layers(loaded_layers)
        else:
            self.log("Transit route response had no loadable layers.", Qgis.Warning)

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
                            "outcome": result.get("outcome"),
                            "fallback_used": result.get("fallback_used"),
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
                            "origin_component_id": result.get("origin", {}).get("component_id"),
                            "destination_component_id": result.get("destination", {}).get(
                                "component_id"
                            ),
                            "origin_snap_distance_m": result.get("origin", {}).get(
                                "snap_distance_m"
                            ),
                            "destination_snap_distance_m": result.get("destination", {}).get(
                                "snap_distance_m"
                            ),
                            "origin_hop_distance_m": result.get("origin_hop_distance_m"),
                            "destination_hop_distance_m": result.get(
                                "destination_hop_distance_m"
                            ),
                        },
                    }
                ],
            }

        if analysis_kind == "service_area":
            features = []
            for feature in result.get("features") or []:
                features.append(
                    {
                        "type": "Feature",
                        "geometry": feature.get("geometry"),
                        "properties": {
                            "dataset_id": service.get("dataset_id"),
                            "profile_id": service.get("profile_id"),
                            "profile_hash": service.get("profile_hash"),
                            "analysis_id": result.get("analysis_id"),
                            "origin_id": feature.get("origin_id"),
                            "threshold_id": feature.get("threshold_id"),
                            "band_start_limit": feature.get("band_start_limit"),
                            "threshold_limit": feature.get("threshold_limit"),
                            "threshold_metric": feature.get("threshold_metric"),
                            "geometry_type": feature.get("geometry_type"),
                            "fallback_used": feature.get("fallback_used"),
                            "origin_component_id": feature.get("origin_component_id"),
                            "origin_hop_distance_m": feature.get("origin_hop_distance_m"),
                            "reachable_network_length_m": feature.get(
                                "reachable_network_length_m"
                            ),
                            "reachable_edge_count": feature.get("reachable_edge_count"),
                        },
                    }
                )
            return {
                "type": "FeatureCollection",
                "features": features,
                "metadata": {
                    "dataset_id": service.get("dataset_id"),
                    "profile_id": service.get("profile_id"),
                    "profile_hash": service.get("profile_hash"),
                    "analysis_id": result.get("analysis_id"),
                    "outcome": result.get("outcome"),
                    "origin_count": result.get("origin_count"),
                    "processed_origin_count": result.get("processed_origin_count"),
                    "skipped_origin_count": result.get("skipped_origin_count"),
                    "fallback_origin_count": result.get("fallback_origin_count"),
                    "threshold_count": result.get("threshold_count"),
                    "warnings": result.get("warnings") or [],
                },
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
                        "outcome": item.get("outcome"),
                        "fallback_used": item.get("fallback_used"),
                        "origin_component_id": item.get("origin_component_id"),
                        "destination_component_id": item.get("destination_component_id"),
                        "origin_hop_distance_m": item.get("origin_hop_distance_m"),
                        "destination_hop_distance_m": item.get("destination_hop_distance_m"),
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
        safe_name = re.sub(r"[^A-Za-z0-9_.-]+", "_", layer_name).strip("_") or "netweevil_layer"
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
            return None
        QgsProject.instance().addMapLayer(layer, False)
        QgsProject.instance().layerTreeRoot().insertLayer(0, layer)
        self.log("Loaded layer {}".format(output_path))
        return layer

    def load_service_area_layers(self, geojson, layer_name):
        features = geojson.get("features") or []
        if not features:
            self.log("Service-area response had no spatial features to load.", Qgis.Warning)
            return

        metadata = geojson.get("metadata") or {}
        analysis_id = metadata.get("analysis_id") or layer_name or "netweevil_service_area"
        group = self.create_result_group(analysis_id)

        grouped = {}
        threshold_order = []
        for feature in features:
            properties = feature.get("properties") or {}
            geometry_type = properties.get("geometry_type") or "unknown"
            threshold_key = self.service_area_threshold_label(properties)
            key = (threshold_key, geometry_type)
            if key not in grouped:
                grouped[key] = []
                threshold_order.append(key)
            grouped[key].append(feature)

        loaded_layers = []
        for threshold_index, key in enumerate(threshold_order):
            threshold_label, geometry_type = key
            sublayer_name = "{} {}".format(geometry_type, threshold_label).strip()
            temp_geojson = {"type": "FeatureCollection", "features": grouped[key]}
            temp_path = self.write_temp_geojson(
                "{}_{}".format(analysis_id, sublayer_name.replace(" ", "_")),
                temp_geojson,
            )
            layer = QgsVectorLayer(str(temp_path), sublayer_name, "ogr")
            if not layer.isValid():
                self.log("Failed to load layer {}".format(temp_path), Qgis.Warning)
                continue
            self.apply_service_area_style(layer, grouped[key], geometry_type, threshold_index)
            QgsProject.instance().addMapLayer(layer, False)
            group.addLayer(layer)
            loaded_layers.append(layer)
            self.log(
                "Loaded service-area layer '{}' with {} feature(s).".format(
                    sublayer_name, len(grouped[key])
                )
            )

        if loaded_layers:
            self.set_last_output_layers(loaded_layers)

    def service_area_threshold_label(self, properties):
        threshold_id = properties.get("threshold_id")
        if threshold_id:
            return str(threshold_id)
        threshold_limit = properties.get("threshold_limit")
        metric = properties.get("threshold_metric") or "threshold"
        if threshold_limit is None:
            return str(metric)
        band_start = properties.get("band_start_limit")
        if band_start is not None:
            return "{} {}-{}".format(metric, band_start, threshold_limit)
        return "{} {}".format(metric, threshold_limit)

    def apply_service_area_style(self, layer, features, geometry_type, threshold_index):
        base_colors = [
            "#0b6e4f",
            "#137547",
            "#1d6fa5",
            "#9f4f0f",
            "#9a275a",
            "#6358d5",
        ]
        base_color = base_colors[threshold_index % len(base_colors)]
        multiple_origins = sorted(
            {
                str((feature.get("properties") or {}).get("origin_id"))
                for feature in features
                if (feature.get("properties") or {}).get("origin_id") not in [None, ""]
            }
        )
        ring_band = any(
            (feature.get("properties") or {}).get("band_start_limit") is not None
            for feature in features
        )

        if len(multiple_origins) > 1:
            categories = []
            for origin_index, origin_id in enumerate(multiple_origins):
                color = base_colors[(threshold_index + origin_index) % len(base_colors)]
                if geometry_type == "network":
                    symbol = QgsLineSymbol.createSimple(
                        {
                            "line_color": color,
                            "line_width": "0.9",
                            "line_style": "dash" if ring_band else "solid",
                        }
                    )
                else:
                    symbol = QgsFillSymbol.createSimple(
                        {
                            "color": color,
                            "outline_color": color,
                            "outline_style": "dash" if ring_band else "solid",
                            "outline_width": "0.7",
                        }
                    )
                categories.append(QgsRendererCategory(origin_id, symbol, origin_id))
            renderer = QgsCategorizedSymbolRenderer("origin_id", categories)
            layer.setRenderer(renderer)
        else:
            if geometry_type == "network":
                symbol = QgsLineSymbol.createSimple(
                    {
                        "line_color": base_color,
                        "line_width": "1.1",
                        "line_style": "dash" if ring_band else "solid",
                    }
                )
            else:
                symbol = QgsFillSymbol.createSimple(
                    {
                        "color": base_color,
                        "outline_color": base_color,
                        "outline_style": "dash" if ring_band else "solid",
                        "outline_width": "0.7",
                    }
                )
            layer.renderer().setSymbol(symbol)
        layer.triggerRepaint()

    def log_analysis_messages(self, analysis_kind, response_json):
        result = response_json.get("result") or {}
        warnings = result.get("warnings") or []
        for warning in warnings:
            self.log("{} warning: {}".format(analysis_kind, warning), Qgis.Warning)

        diagnostics = result.get("diagnostics") or []
        for diagnostic in diagnostics:
            if not isinstance(diagnostic, dict):
                self.log(
                    "{} diagnostic: {}".format(analysis_kind, diagnostic),
                    Qgis.Warning,
                )
                continue
            level = Qgis.Warning
            if diagnostic.get("severity") == "error":
                level = Qgis.Critical
            self.log(
                "{} diagnostic [{}]: {}".format(
                    analysis_kind,
                    diagnostic.get("code", "diagnostic"),
                    diagnostic.get("message", ""),
                ),
                level,
            )
            for action in diagnostic.get("suggested_actions") or []:
                self.log(
                    "{} next action: {}".format(analysis_kind, action),
                    Qgis.Warning,
                )

        if analysis_kind == "route":
            self.log(
                "route outcome={} fallback_used={} violations={}.".format(
                    result.get("outcome", "unknown"),
                    result.get("fallback_used", False),
                    len(result.get("violations") or []),
                )
            )
            violations = result.get("violations") or []
            for violation in violations:
                self.log(
                    "route violation [{}]: penalty_s={}".format(
                        violation.get("violation_type", "unknown"),
                        violation.get("penalty_s", 0.0),
                    ),
                    Qgis.Warning,
                )
            return

        if analysis_kind == "service_area":
            self.log(
                "service_area outcome={} processed_origins={} skipped_origins={} fallback_origins={} features={}.".format(
                    result.get("outcome", "unknown"),
                    result.get("processed_origin_count", 0),
                    result.get("skipped_origin_count", 0),
                    result.get("fallback_origin_count", 0),
                    len(result.get("features") or []),
                )
            )
            return

        if analysis_kind == "transit_route":
            summary = result.get("summary") or {}
            self.log(
                "transit_route outcome={} legs={} boardings={} total_time_s={}.".format(
                    result.get("outcome", "unknown"),
                    len(result.get("legs") or []),
                    summary.get("boarding_count", 0),
                    summary.get("total_travel_time_s"),
                )
            )
            return

        items = result.get("pairs") if analysis_kind == "od" else result.get("cells")
        if not isinstance(items, list):
            return
        counts = {}
        outcomes = {}
        fallback_count = 0
        for item in items:
            status = item.get("status", "unknown")
            counts[status] = counts.get(status, 0) + 1
            outcome = item.get("outcome", "unknown")
            outcomes[outcome] = outcomes.get(outcome, 0) + 1
            if item.get("fallback_used"):
                fallback_count += 1
        self.log(
            "{} summary: statuses={} outcomes={} fallback_items={}.".format(
                analysis_kind, counts, outcomes, fallback_count
            )
        )
        for item in items:
            item_diagnostics = item.get("diagnostics") or []
            if item_diagnostics:
                item_id = item.get("pair_id") or "{}->{}".format(
                    item.get("origin_id", "?"), item.get("destination_id", "?")
                )
                for diagnostic in item_diagnostics:
                    level = Qgis.Warning
                    if diagnostic.get("severity") == "error":
                        level = Qgis.Critical
                    self.log(
                        "{} item {} diagnostic [{}]: {}".format(
                            analysis_kind,
                            item_id,
                            diagnostic.get("code", "diagnostic"),
                            diagnostic.get("message", ""),
                        ),
                        level,
                    )
                    for action in diagnostic.get("suggested_actions") or []:
                        self.log(
                            "{} item {} next action: {}".format(
                                analysis_kind,
                                item_id,
                                action,
                            ),
                            Qgis.Warning,
                        )

    def log_geojson_messages(self, analysis_kind, geojson):
        metadata = geojson.get("metadata") or {}
        warnings = metadata.get("warnings") or []
        for warning in warnings:
            self.log("{} warning: {}".format(analysis_kind, warning), Qgis.Warning)
        features = geojson.get("features") or []
        if analysis_kind == "route" and features:
            properties = features[0].get("properties") or {}
            self.log(
                "route outcome={} fallback_used={} violations={}.".format(
                    properties.get("outcome", "unknown"),
                    properties.get("fallback_used", False),
                    properties.get("violation_count", 0),
                )
            )
        elif analysis_kind in ["od", "matrix"] and features:
            status_counts = {}
            outcome_counts = {}
            fallback_count = 0
            for feature in features:
                properties = feature.get("properties") or {}
                status = properties.get("status", "unknown")
                outcome = properties.get("outcome", "unknown")
                status_counts[status] = status_counts.get(status, 0) + 1
                outcome_counts[outcome] = outcome_counts.get(outcome, 0) + 1
                if properties.get("fallback_used"):
                    fallback_count += 1
            self.log(
                "{} summary: statuses={} outcomes={} fallback_items={}.".format(
                    analysis_kind, status_counts, outcome_counts, fallback_count
                )
            )
        if metadata:
            summary = ", ".join(
                "{}={}".format(key, value)
                for key, value in metadata.items()
                if key not in ["warnings"] and value not in [None, "", []]
            )
            if summary:
                self.log("{} summary: {}.".format(analysis_kind, summary))

    def remove_existing_result_layer(self, layer_name):
        project = QgsProject.instance()
        for layer in list(project.mapLayers().values()):
            if layer.name() == layer_name:
                project.removeMapLayer(layer.id())

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

    def log(self, message, level=Qgis.Info):
        QgsMessageLog.logMessage(message, "netweevil", level)
        self.log_output.appendPlainText(message)

    def alert(self, message):
        QMessageBox.warning(self, "netweevil", message)
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

    def save_advanced_settings(self, settings, prefix, include_failure_modes):
        values = {
            "{}_advanced_visible".format(prefix): getattr(
                self, "{}_advanced_toggle".format(prefix)
            ).isChecked(),
            "{}_connectivity_mode".format(prefix): getattr(
                self, "{}_connectivity_mode_combo".format(prefix)
            ).currentData(),
            "{}_max_hop_distance".format(prefix): getattr(
                self, "{}_max_hop_distance_edit".format(prefix)
            ).text().strip(),
            "{}_report_hop_distance".format(prefix): getattr(
                self, "{}_report_hop_distance_check".format(prefix)
            ).isChecked(),
        }
        if include_failure_modes:
            values.update(
                {
                    "{}_unsafe_visible".format(prefix): getattr(
                        self, "{}_unsafe_toggle".format(prefix)
                    ).isChecked(),
                    "{}_auto_relax_unreachable".format(prefix): getattr(
                        self, "{}_auto_relax_unreachable_check".format(prefix)
                    ).isChecked(),
                    "{}_allow_reverse_oneway".format(prefix): getattr(
                        self, "{}_allow_reverse_oneway_check".format(prefix)
                    ).isChecked(),
                    "{}_allow_illegal_turn".format(prefix): getattr(
                        self, "{}_allow_illegal_turn_check".format(prefix)
                    ).isChecked(),
                    "{}_ignore_turn_restrictions".format(prefix): getattr(
                        self, "{}_ignore_turn_restrictions_check".format(prefix)
                    ).isChecked(),
                    "{}_allow_uturn".format(prefix): getattr(
                        self, "{}_allow_uturn_check".format(prefix)
                    ).isChecked(),
                    "{}_reverse_penalty".format(prefix): getattr(
                        self, "{}_reverse_penalty_edit".format(prefix)
                    ).text().strip(),
                    "{}_illegal_turn_penalty".format(prefix): getattr(
                        self, "{}_illegal_turn_penalty_edit".format(prefix)
                    ).text().strip(),
                    "{}_ignored_restriction_penalty".format(prefix): getattr(
                        self, "{}_ignored_restriction_penalty_edit".format(prefix)
                    ).text().strip(),
                    "{}_forbidden_uturn_penalty".format(prefix): getattr(
                        self, "{}_forbidden_uturn_penalty_edit".format(prefix)
                    ).text().strip(),
                    "{}_max_illegal_distance".format(prefix): getattr(
                        self, "{}_max_illegal_distance_edit".format(prefix)
                    ).text().strip(),
                    "{}_max_illegal_turns".format(prefix): getattr(
                        self, "{}_max_illegal_turns_edit".format(prefix)
                    ).text().strip(),
                }
            )
        for key, value in values.items():
            settings.setValue("{}/{}".format(SETTINGS_PREFIX, key), value)

    def load_advanced_settings(self, prefix, include_failure_modes):
        self.set_combo_by_data(
            getattr(self, "{}_connectivity_mode_combo".format(prefix)),
            self.read_setting("{}_connectivity_mode".format(prefix), "strict"),
        )
        getattr(self, "{}_max_hop_distance_edit".format(prefix)).setText(
            self.read_setting("{}_max_hop_distance".format(prefix), "")
        )
        getattr(self, "{}_report_hop_distance_check".format(prefix)).setChecked(
            self.read_bool_setting("{}_report_hop_distance".format(prefix), False)
        )
        getattr(self, "{}_advanced_toggle".format(prefix)).setChecked(
            self.read_bool_setting("{}_advanced_visible".format(prefix), False)
        )

        if include_failure_modes:
            getattr(self, "{}_auto_relax_unreachable_check".format(prefix)).setChecked(
                self.read_bool_setting("{}_auto_relax_unreachable".format(prefix), False)
            )
            getattr(self, "{}_allow_reverse_oneway_check".format(prefix)).setChecked(
                self.read_bool_setting("{}_allow_reverse_oneway".format(prefix), False)
            )
            getattr(self, "{}_allow_illegal_turn_check".format(prefix)).setChecked(
                self.read_bool_setting("{}_allow_illegal_turn".format(prefix), False)
            )
            getattr(self, "{}_ignore_turn_restrictions_check".format(prefix)).setChecked(
                self.read_bool_setting("{}_ignore_turn_restrictions".format(prefix), False)
            )
            getattr(self, "{}_allow_uturn_check".format(prefix)).setChecked(
                self.read_bool_setting("{}_allow_uturn".format(prefix), False)
            )
            getattr(self, "{}_reverse_penalty_edit".format(prefix)).setText(
                self.read_setting("{}_reverse_penalty".format(prefix), "")
            )
            getattr(self, "{}_illegal_turn_penalty_edit".format(prefix)).setText(
                self.read_setting("{}_illegal_turn_penalty".format(prefix), "")
            )
            getattr(
                self, "{}_ignored_restriction_penalty_edit".format(prefix)
            ).setText(self.read_setting("{}_ignored_restriction_penalty".format(prefix), ""))
            getattr(self, "{}_forbidden_uturn_penalty_edit".format(prefix)).setText(
                self.read_setting("{}_forbidden_uturn_penalty".format(prefix), "")
            )
            getattr(self, "{}_max_illegal_distance_edit".format(prefix)).setText(
                self.read_setting("{}_max_illegal_distance".format(prefix), "")
            )
            getattr(self, "{}_max_illegal_turns_edit".format(prefix)).setText(
                self.read_setting("{}_max_illegal_turns".format(prefix), "")
            )
            getattr(self, "{}_unsafe_toggle".format(prefix)).setChecked(
                self.read_bool_setting("{}_unsafe_visible".format(prefix), False)
            )

    def save_settings(self):
        settings = QSettings()
        values = {
            "workspace_root": self.workspace_root_edit.text().strip(),
            "api_base_url": self.api_base_url_edit.text().strip(),
            "timeout_seconds": self.timeout_seconds_edit.text().strip(),
            "response_format": self.response_format(),
            "profile_id": self.selected_profile_id() or "",
            "saved_runs_directory": self.saved_runs_directory_edit.text().strip(),
            "saved_run_path": self.saved_run_path_edit.text().strip(),
            "saved_run_kind": self.saved_run_kind_combo.currentData() or "",
            "route_id": self.route_id_edit.text().strip(),
            "route_auto_increment": self.route_auto_increment_check.isChecked(),
            "route_auto_output_path": self.route_auto_output_path_check.isChecked(),
            "route_output_path": self.route_output_path_edit.text().strip(),
            "route_request_path": self.route_request_path_edit.text().strip(),
            "snap_distance": self.snap_distance_edit.text().strip(),
            "route_geometry": self.route_geometry_combo.currentData(),
            "route_segment_rows": self.route_segment_rows_check.isChecked(),
            "route_road_distance": self.route_road_distance_check.isChecked(),
            "route_road_time": self.route_road_time_check.isChecked(),
            "route_surface_distance": self.route_surface_distance_check.isChecked(),
            "route_surface_time": self.route_surface_time_check.isChecked(),
            "route_penalty_breakdown": self.route_penalty_breakdown_check.isChecked(),
            "route_explain_cost_derivation": self.route_explain_cost_derivation_check.isChecked(),
            "origin_id": self.origin_id_edit.text().strip(),
            "origin_lon": self.origin_lon_edit.text().strip(),
            "origin_lat": self.origin_lat_edit.text().strip(),
            "destination_id": self.destination_id_edit.text().strip(),
            "destination_lon": self.destination_lon_edit.text().strip(),
            "destination_lat": self.destination_lat_edit.text().strip(),
            "transit_feed_id": self.selected_transit_feed_id(),
            "transit_route_id": self.transit_route_id_edit.text().strip(),
            "transit_auto_increment": self.transit_auto_increment_check.isChecked(),
            "transit_datetime": self.transit_datetime_edit.text().strip(),
            "transit_arrive_by": self.transit_arrive_by_check.isChecked(),
            "transit_search_window": self.transit_search_window_edit.text().strip(),
            "transit_output_path": self.transit_output_path_edit.text().strip(),
            "transit_request_path": self.transit_request_path_edit.text().strip(),
            "transit_origin_id": self.transit_origin_id_edit.text().strip(),
            "transit_origin_lon": self.transit_origin_lon_edit.text().strip(),
            "transit_origin_lat": self.transit_origin_lat_edit.text().strip(),
            "transit_destination_id": self.transit_destination_id_edit.text().strip(),
            "transit_destination_lon": self.transit_destination_lon_edit.text().strip(),
            "transit_destination_lat": self.transit_destination_lat_edit.text().strip(),
            "transit_walk_speed": self.transit_walk_speed_edit.text().strip(),
            "transit_max_access_distance": self.transit_max_access_distance_edit.text().strip(),
            "transit_max_egress_distance": self.transit_max_egress_distance_edit.text().strip(),
            "transit_max_transfer_distance": self.transit_max_transfer_distance_edit.text().strip(),
            "transit_board_slack": self.transit_board_slack_edit.text().strip(),
            "transit_transfer_slack": self.transit_transfer_slack_edit.text().strip(),
            "transit_max_transfers": self.transit_max_transfers_edit.text().strip(),
            "transit_include_geometry": self.transit_include_geometry_check.isChecked(),
            "transit_network_walk_geometry": self.transit_network_walk_geometry_check.isChecked(),
            "transit_include_stops": self.transit_include_stops_check.isChecked(),
            "transit_include_stop_segments": self.transit_include_stop_segments_check.isChecked(),
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
            "service_area_analysis_id": self.service_area_analysis_id_edit.text().strip(),
            "service_area_snap_distance": self.service_area_snap_distance_edit.text().strip(),
            "service_area_output_path": self.service_area_output_path_edit.text().strip(),
            "service_area_request_path": self.service_area_request_path_edit.text().strip(),
            "service_area_output_mode": self.service_area_output_mode_combo.currentData(),
            "service_area_band_mode": self.service_area_band_mode_combo.currentData(),
            "service_area_boundary_mode": self.service_area_boundary_mode_combo.currentData(),
            "service_area_multi_origin_mode": self.service_area_multi_origin_mode_combo.currentData(),
            "service_area_thresholds": self.service_area_thresholds_edit.text().strip(),
            "service_area_threshold_metric": self.service_area_threshold_metric_combo.currentData(),
            "service_area_hull_preset": self.service_area_hull_preset_combo.currentData(),
            "service_area_hull_aggressiveness": self.service_area_hull_aggressiveness_edit.text().strip(),
            "service_area_simplification": self.service_area_simplification_edit.text().strip(),
            "service_area_origins": self.service_area_origins_edit.toPlainText().strip(),
        }
        for key, value in values.items():
            settings.setValue("{}/{}".format(SETTINGS_PREFIX, key), value)

        for mode, check in self.transit_mode_checks.items():
            settings.setValue(
                "{}/transit_mode_{}".format(SETTINGS_PREFIX, mode),
                check.isChecked(),
            )

        self.save_advanced_settings(settings, "route", include_failure_modes=True)
        self.save_advanced_settings(settings, "batch", include_failure_modes=True)
        self.save_advanced_settings(settings, "service_area", include_failure_modes=False)

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
        self.saved_runs_directory_edit.setText(
            self.read_setting(
                "saved_runs_directory", self.saved_runs_directory_edit.text()
            )
        )
        self.saved_run_path_edit.setText(self.read_setting("saved_run_path", ""))
        self.set_combo_by_data(
            self.saved_run_kind_combo,
            self.read_setting("saved_run_kind", ""),
        )
        self.route_id_edit.setText(self.read_setting("route_id", self.route_id_edit.text()))
        self.route_auto_increment_check.setChecked(
            self.read_bool_setting("route_auto_increment", True)
        )
        self.route_auto_output_path_check.setChecked(
            self.read_bool_setting("route_auto_output_path", True)
        )
        self.route_output_path_edit.setText(
            self.read_setting("route_output_path", self.route_output_path_edit.text())
        )
        self.route_request_path_edit.setText(
            self.read_setting("route_request_path", self.route_request_path_edit.text())
        )
        self.snap_distance_edit.setText(
            self.read_setting("snap_distance", self.snap_distance_edit.text())
        )
        self.set_combo_by_data(
            self.route_geometry_combo,
            self.read_setting("route_geometry", "full"),
        )
        self.route_segment_rows_check.setChecked(
            self.read_bool_setting("route_segment_rows", True)
        )
        self.route_road_distance_check.setChecked(
            self.read_bool_setting("route_road_distance", True)
        )
        self.route_road_time_check.setChecked(
            self.read_bool_setting("route_road_time", True)
        )
        self.route_surface_distance_check.setChecked(
            self.read_bool_setting("route_surface_distance", True)
        )
        self.route_surface_time_check.setChecked(
            self.read_bool_setting("route_surface_time", True)
        )
        self.route_penalty_breakdown_check.setChecked(
            self.read_bool_setting("route_penalty_breakdown", True)
        )
        self.route_explain_cost_derivation_check.setChecked(
            self.read_bool_setting("route_explain_cost_derivation", True)
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
        self.transit_route_id_edit.setText(
            self.read_setting("transit_route_id", self.transit_route_id_edit.text())
        )
        self.transit_auto_increment_check.setChecked(
            self.read_bool_setting("transit_auto_increment", True)
        )
        self.transit_datetime_edit.setText(
            self.read_setting("transit_datetime", self.transit_datetime_edit.text())
        )
        self.transit_arrive_by_check.setChecked(
            False
        )
        self.transit_search_window_edit.setText(
            self.read_setting("transit_search_window", self.transit_search_window_edit.text())
        )
        self.transit_output_path_edit.setText(
            self.read_setting("transit_output_path", self.transit_output_path_edit.text())
        )
        self.transit_request_path_edit.setText(
            self.read_setting("transit_request_path", self.transit_request_path_edit.text())
        )
        self.transit_origin_id_edit.setText(
            self.read_setting("transit_origin_id", self.transit_origin_id_edit.text())
        )
        self.transit_origin_lon_edit.setText(self.read_setting("transit_origin_lon", ""))
        self.transit_origin_lat_edit.setText(self.read_setting("transit_origin_lat", ""))
        self.transit_destination_id_edit.setText(
            self.read_setting(
                "transit_destination_id", self.transit_destination_id_edit.text()
            )
        )
        self.transit_destination_lon_edit.setText(
            self.read_setting("transit_destination_lon", "")
        )
        self.transit_destination_lat_edit.setText(
            self.read_setting("transit_destination_lat", "")
        )
        self.transit_walk_speed_edit.setText(
            self.read_setting("transit_walk_speed", self.transit_walk_speed_edit.text())
        )
        self.transit_max_access_distance_edit.setText(
            self.read_setting(
                "transit_max_access_distance",
                self.transit_max_access_distance_edit.text(),
            )
        )
        self.transit_max_egress_distance_edit.setText(
            self.read_setting(
                "transit_max_egress_distance",
                self.transit_max_egress_distance_edit.text(),
            )
        )
        self.transit_max_transfer_distance_edit.setText(
            self.read_setting(
                "transit_max_transfer_distance",
                self.transit_max_transfer_distance_edit.text(),
            )
        )
        self.transit_board_slack_edit.setText(
            self.read_setting("transit_board_slack", self.transit_board_slack_edit.text())
        )
        self.transit_transfer_slack_edit.setText(
            self.read_setting(
                "transit_transfer_slack", self.transit_transfer_slack_edit.text()
            )
        )
        self.transit_max_transfers_edit.setText(
            self.read_setting("transit_max_transfers", self.transit_max_transfers_edit.text())
        )
        self.transit_include_geometry_check.setChecked(
            self.read_bool_setting("transit_include_geometry", True)
        )
        self.transit_network_walk_geometry_check.setChecked(
            self.read_bool_setting("transit_network_walk_geometry", False)
        )
        self.transit_include_stops_check.setChecked(
            self.read_bool_setting("transit_include_stops", True)
        )
        self.transit_include_stop_segments_check.setChecked(
            self.read_bool_setting("transit_include_stop_segments", True)
        )
        for mode, check in self.transit_mode_checks.items():
            check.setChecked(
                self.read_bool_setting("transit_mode_{}".format(mode), check.isChecked())
            )
        self.od_pairs_path_edit.setText(
            self.read_setting("od_pairs_path", self.od_pairs_path_edit.text())
        )
        self.od_output_path_edit.setText(
            self.read_setting("od_output_path", self.od_output_path_edit.text())
        )
        self.matrix_output_path_edit.setText(
            self.read_setting("matrix_output_path", self.matrix_output_path_edit.text())
        )
        self.service_area_analysis_id_edit.setText(
            self.read_setting(
                "service_area_analysis_id", self.service_area_analysis_id_edit.text()
            )
        )
        self.service_area_snap_distance_edit.setText(
            self.read_setting(
                "service_area_snap_distance", self.service_area_snap_distance_edit.text()
            )
        )
        self.service_area_output_path_edit.setText(
            self.read_setting(
                "service_area_output_path", self.service_area_output_path_edit.text()
            )
        )
        self.service_area_request_path_edit.setText(
            self.read_setting(
                "service_area_request_path", self.service_area_request_path_edit.text()
            )
        )
        self.service_area_thresholds_edit.setText(
            self.read_setting(
                "service_area_thresholds", self.service_area_thresholds_edit.text()
            )
        )
        self.service_area_hull_aggressiveness_edit.setText(
            self.read_setting(
                "service_area_hull_aggressiveness",
                self.service_area_hull_aggressiveness_edit.text(),
            )
        )
        self.service_area_simplification_edit.setText(
            self.read_setting(
                "service_area_simplification", self.service_area_simplification_edit.text()
            )
        )
        self.service_area_origins_edit.setPlainText(
            self.read_setting("service_area_origins", self.service_area_origins_edit.toPlainText())
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
        self.set_combo_by_data(
            self.service_area_output_mode_combo,
            self.read_setting("service_area_output_mode", "both"),
        )
        self.set_combo_by_data(
            self.service_area_band_mode_combo,
            self.read_setting("service_area_band_mode", "cumulative"),
        )
        self.set_combo_by_data(
            self.service_area_boundary_mode_combo,
            self.read_setting("service_area_boundary_mode", "overlap"),
        )
        self.set_combo_by_data(
            self.service_area_multi_origin_mode_combo,
            self.read_setting("service_area_multi_origin_mode", "merge"),
        )
        self.set_combo_by_data(
            self.service_area_threshold_metric_combo,
            self.read_setting("service_area_threshold_metric", "travel_time_s"),
        )
        self.set_combo_by_data(
            self.service_area_hull_preset_combo,
            self.read_setting("service_area_hull_preset", "1.00"),
        )
        self.service_area_hull_aggressiveness_edit.setText(
            self.read_setting(
                "service_area_hull_aggressiveness",
                self.service_area_hull_aggressiveness_edit.text(),
            )
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
        self.load_advanced_settings("route", include_failure_modes=True)
        self.load_advanced_settings("batch", include_failure_modes=True)
        self.load_advanced_settings("service_area", include_failure_modes=False)
        if not self.service_area_hull_aggressiveness_edit.text().strip():
            self.sync_service_area_hull_preset()
        if not self.route_id_edit.text().strip():
            self.route_id_edit.setText("qgis_route_001")
        if (
            self.route_auto_output_path_check.isChecked()
            and not self.route_output_path_edit.text().strip()
        ):
            self.sync_route_output_path_from_route_id()

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
