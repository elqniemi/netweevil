"""Service Area tab: UI, origins, thresholds, and run handling."""

import json

from qgis.PyQt.QtCore import QSettings
from qgis.PyQt.QtWidgets import (
    QCheckBox,
    QComboBox,
    QFormLayout,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QPlainTextEdit,
    QVBoxLayout,
    QWidget,
)
from qgis.core import QgsCoordinateTransform, QgsProject, QgsWkbTypes

from .compat import GEOM_POINT
from .constants import SETTINGS_PREFIX, PickTarget


class ServiceAreaTabMixin:
    def _build_service_area_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        summary = QLabel(
            "Compute reachable network and polygon areas around one or more "
            "origins. Add origins, set thresholds, then press Run Service Area."
        )
        summary.setWordWrap(True)
        layout.addWidget(summary)

        origins_group = QGroupBox("Origins")
        origins_layout = QVBoxLayout(origins_group)
        origins_note = QLabel(
            "One origin per line in the form id,lon,lat. Append map-picked points, "
            "selected point features, or the current route endpoints."
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
            "Pick On Map appends one origin per map click. Selected point features "
            "are transformed into WGS84 automatically."
        )
        self.service_area_pick_status_label.setWordWrap(True)
        origins_layout.addWidget(self.service_area_pick_status_label)
        origin_button_row = QHBoxLayout()
        pick_origin_button = QPushButton("Pick On Map")
        pick_origin_button.setToolTip(
            "Click this button, then click the map canvas to append one origin. "
            "Click again to cancel."
        )
        pick_origin_button.clicked.connect(
            lambda: self.begin_point_pick(PickTarget.SERVICE_AREA_ORIGIN)
        )
        add_selected_button = QPushButton("Add Selected Features")
        add_selected_button.setToolTip(
            "Append every selected feature of the active point layer as an origin."
        )
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

        options_group = QGroupBox("Analysis")
        options_form = QFormLayout(options_group)
        self.service_area_analysis_id_edit = QLineEdit("qgis_service_area_001")
        self.service_area_mode_combo = QComboBox()
        self.service_area_mode_combo.addItem("Road network", "road")
        self.service_area_mode_combo.addItem("Transit feed", "transit")
        self.service_area_mode_combo.setToolTip(
            "Road network reachability with the selected profile, or scheduled "
            "transit reachability through the feed selected in the Transit tab."
        )
        self.service_area_output_path_edit = QLineEdit(
            ".netweevil/runs/qgis-service-area.geojson"
        )
        self.service_area_output_path_edit.setToolTip(
            "Where the raw API response is saved. Relative paths resolve against "
            "the workspace root configured in Settings."
        )
        self.service_area_request_path_edit = QLineEdit(
            ".netweevil/requests/service_area_from_qgis.json"
        )
        self.service_area_request_path_edit.setToolTip(
            "Only used by Save Request, to reproduce this analysis outside QGIS."
        )
        options_form.addRow("Analysis id", self.service_area_analysis_id_edit)
        options_form.addRow("Mode", self.service_area_mode_combo)
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

        self.service_area_threshold_group = QGroupBox("Thresholds")
        threshold_form = QFormLayout(self.service_area_threshold_group)
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
        layout.addWidget(self.service_area_threshold_group)

        self.service_area_road_group = QGroupBox("Road-network options")
        road_form = QFormLayout(self.service_area_road_group)
        self.service_area_snap_distance_edit = QLineEdit("500")
        self.service_area_snap_distance_edit.setToolTip(
            "Maximum distance in meters from an origin to the nearest routable edge."
        )
        self.service_area_output_mode_combo = QComboBox()
        self.service_area_output_mode_combo.addItem("Network", "network")
        self.service_area_output_mode_combo.addItem("Polygon", "polygon")
        self.service_area_output_mode_combo.addItem("Both", "both")
        self.service_area_output_mode_combo.setToolTip(
            "Return reachable network lines, hull polygons, or both."
        )
        self.service_area_band_mode_combo = QComboBox()
        self.service_area_band_mode_combo.addItem("Cumulative", "cumulative")
        self.service_area_band_mode_combo.addItem("Ring", "ring")
        self.service_area_band_mode_combo.addItem("None (segments)", "none")
        self.service_area_band_mode_combo.setToolTip(
            "Cumulative: each threshold includes everything below it. "
            "Ring: each threshold covers only its own band. "
            "None: per-segment costs without banding."
        )
        self.service_area_segments_check = QCheckBox("Return per-segment costs")
        self.service_area_segments_check.setChecked(False)
        self.service_area_boundary_mode_combo = QComboBox()
        self.service_area_boundary_mode_combo.addItem("Overlap", "overlap")
        self.service_area_boundary_mode_combo.addItem("Cut At Boundary", "cut_at_boundary")
        self.service_area_boundary_mode_combo.setToolTip(
            "How edges crossing a threshold boundary are handled: kept whole "
            "(overlap) or cut exactly at the limit."
        )
        self.service_area_multi_origin_mode_combo = QComboBox()
        self.service_area_multi_origin_mode_combo.addItem("Merge", "merge")
        self.service_area_multi_origin_mode_combo.addItem("Overlap", "overlap")
        self.service_area_multi_origin_mode_combo.addItem("Cut", "cut")
        self.service_area_multi_origin_mode_combo.setToolTip(
            "How areas from multiple origins combine: merged into one area, "
            "kept as overlapping per-origin areas, or cut against each other."
        )
        road_form.addRow("Snap distance m", self.service_area_snap_distance_edit)
        road_form.addRow("Output mode", self.service_area_output_mode_combo)
        road_form.addRow("Band mode", self.service_area_band_mode_combo)
        road_form.addRow("", self.service_area_segments_check)
        road_form.addRow("Boundary mode", self.service_area_boundary_mode_combo)
        road_form.addRow("Multi-origin mode", self.service_area_multi_origin_mode_combo)
        layout.addWidget(self.service_area_road_group)

        self.service_area_polygon_group = QGroupBox("Polygon generation")
        polygon_form = QFormLayout(self.service_area_polygon_group)
        self.service_area_hull_preset_combo = QComboBox()
        self.service_area_hull_preset_combo.addItem("Conservative", "0.75")
        self.service_area_hull_preset_combo.addItem("Balanced", "1.00")
        self.service_area_hull_preset_combo.addItem("Aggressive", "1.50")
        self.service_area_hull_aggressiveness_edit = QLineEdit("1.0")
        self.service_area_hull_aggressiveness_edit.setToolTip(
            "Higher values hug the network more tightly; lower values produce "
            "smoother, more generous hulls."
        )
        self.service_area_simplification_edit = QLineEdit("20")
        polygon_form.addRow("Hull preset", self.service_area_hull_preset_combo)
        polygon_form.addRow("Hull aggressiveness", self.service_area_hull_aggressiveness_edit)
        polygon_form.addRow(
            "Simplification tolerance m", self.service_area_simplification_edit
        )
        self.service_area_hull_preset_combo.currentIndexChanged.connect(
            self.sync_service_area_hull_preset
        )
        layout.addWidget(self.service_area_polygon_group)

        self.service_area_transit_group = QGroupBox("Transit options")
        transit_form = QFormLayout(self.service_area_transit_group)
        self.service_area_transit_datetime_edit = QLineEdit("2026-05-11T08:30:00+02:00")
        self.service_area_transit_datetime_edit.setToolTip(
            "Departure time as an RFC 3339 timestamp inside the feed's service calendar."
        )
        self.service_area_transit_max_time_edit = QLineEdit("3600")
        self.service_area_transit_max_time_edit.setToolTip(
            "Maximum total travel time in seconds for the transit service area."
        )
        self.service_area_access_mode_combo = QComboBox()
        for label, value in self.STREET_MODE_ITEMS:
            self.service_area_access_mode_combo.addItem(label, value)
        self.service_area_access_mode_combo.setToolTip(
            "First-mile street mode from each origin to nearby stops: "
            "walking+transit, cycling+transit, or car+transit."
        )
        self.service_area_access_distance_edit = QLineEdit("1200")
        self.service_area_access_distance_edit.setToolTip(
            "Maximum first-mile distance in meters from an origin to a stop "
            "with the selected access mode."
        )
        self.service_area_access_speed_edit = QLineEdit("4.8")
        self.service_area_access_speed_edit.setToolTip(
            "Average first-mile speed in kph for the selected access mode."
        )
        transit_note = QLabel(
            "Uses the feed selected in the Transit tab. Transit mode filters, "
            "transfer limits, and slacks come from the Transit tab's Modes and "
            "Limits; the first mile uses the access mode chosen above."
        )
        transit_note.setWordWrap(True)
        transit_form.addRow("Transit departure", self.service_area_transit_datetime_edit)
        transit_form.addRow("Transit max time s", self.service_area_transit_max_time_edit)
        transit_form.addRow("Access mode", self.service_area_access_mode_combo)
        transit_form.addRow("Access distance m", self.service_area_access_distance_edit)
        transit_form.addRow("Access speed kph", self.service_area_access_speed_edit)
        transit_form.addRow("", transit_note)
        self.service_area_access_mode_combo.currentIndexChanged.connect(
            self.sync_service_area_access_mode_defaults
        )
        layout.addWidget(self.service_area_transit_group)

        service_area_advanced_widget = self._build_advanced_controls(
            "service_area", include_failure_modes=False
        )
        self.service_area_advanced_toggle = self._make_toggle_section(
            "Connectivity Controls", service_area_advanced_widget
        )
        self.service_area_advanced_widget = service_area_advanced_widget
        layout.addWidget(self.service_area_advanced_toggle)
        layout.addWidget(service_area_advanced_widget)

        button_row = QHBoxLayout()
        save_request_button = QPushButton("Save Request")
        save_request_button.setToolTip("Write the request JSON to disk without running it.")
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

        self.service_area_mode_combo.currentIndexChanged.connect(
            self.update_service_area_mode_visibility
        )
        self.update_service_area_mode_visibility()

        layout.addStretch(1)
        return tab

    def update_service_area_mode_visibility(self, *_args):
        """Show only the controls the selected mode actually sends to the API."""
        is_transit = self.service_area_mode_combo.currentData() == "transit"
        self.service_area_transit_group.setVisible(is_transit)
        self.service_area_threshold_group.setVisible(not is_transit)
        self.service_area_road_group.setVisible(not is_transit)
        self.service_area_polygon_group.setVisible(not is_transit)
        self.service_area_advanced_toggle.setVisible(not is_transit)
        self.service_area_advanced_widget.setVisible(
            not is_transit and self.service_area_advanced_toggle.isChecked()
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
        if QgsWkbTypes.geometryType(layer.wkbType()) != GEOM_POINT:
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

    SERVICE_AREA_ACCESS_DEFAULTS = {
        "walk": ("1200", "4.8"),
        "bicycle": ("5000", "15"),
        "car": ("15000", "25"),
    }

    def selected_service_area_access_mode(self):
        return self.service_area_access_mode_combo.currentData() or "walk"

    def sync_service_area_access_mode_defaults(self, *_args):
        distance, speed = self.SERVICE_AREA_ACCESS_DEFAULTS.get(
            self.selected_service_area_access_mode(), ("1200", "4.8")
        )
        self.service_area_access_distance_edit.setText(distance)
        self.service_area_access_speed_edit.setText(speed)

    def sync_service_area_hull_preset(self):
        preset = self.service_area_hull_preset_combo.currentData()
        if preset:
            self.service_area_hull_aggressiveness_edit.setText(preset)

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
            raise ValueError(
                "Add at least one service-area origin (Pick On Map or Add Selected Features)."
            )
        return origins

    def build_service_area_transit_street_modes(self):
        access_mode = self.selected_service_area_access_mode()
        distance_default, speed_default = self.SERVICE_AREA_ACCESS_DEFAULTS[access_mode]
        access_distance = self.parse_float_with_default(
            self.service_area_access_distance_edit.text(),
            "Access distance",
            float(distance_default),
        )
        access_speed = self.parse_float_with_default(
            self.service_area_access_speed_edit.text(),
            "Access speed",
            float(speed_default),
        )
        modes = {
            "access": [access_mode],
            "egress": [access_mode],
            "walk_speed_kph": self.parse_float_with_default(
                self.transit_walk_speed_edit.text(), "Walk speed", 4.8
            )
            if hasattr(self, "transit_walk_speed_edit")
            else 4.8,
        }
        distance_keys = {
            "walk": "max_access_distance_m",
            "bicycle": "max_bicycle_access_distance_m",
            "car": "max_car_access_distance_m",
        }
        speed_keys = {
            "walk": "walk_speed_kph",
            "bicycle": "bicycle_speed_kph",
            "car": "car_access_speed_kph",
        }
        modes[distance_keys[access_mode]] = access_distance
        modes[speed_keys[access_mode]] = access_speed
        return modes

    def build_service_area_transit_returns(self):
        return {
            "include_stops": self.transit_include_stops_check.isChecked()
            if hasattr(self, "transit_include_stops_check")
            else True,
            "include_stop_segments": self.transit_include_stop_segments_check.isChecked()
            if hasattr(self, "transit_include_stop_segments_check")
            else True,
            "include_geometry": self.transit_include_geometry_check.isChecked()
            if hasattr(self, "transit_include_geometry_check")
            else True,
        }

    def build_service_area_request(self):
        if self.service_area_mode_combo.currentData() == "transit":
            transit_modes = []
            for value, check in getattr(self, "transit_mode_checks", {}).items():
                if check.isChecked():
                    transit_modes.append(value)
            if not transit_modes:
                transit_modes = ["tram", "subway", "rail", "bus", "ferry", "coach"]
            return {
                "analysis_id": self.service_area_analysis_id_edit.text().strip()
                or "qgis_transit_service_area",
                "origins": self.build_service_area_origins(),
                "time": {
                    "datetime": self.service_area_transit_datetime_edit.text().strip(),
                    "arrive_by": False,
                    "search_window_s": self.parse_int_with_default(
                        self.transit_search_window_edit.text(),
                        "Transit search window",
                        3600,
                    )
                    if hasattr(self, "transit_search_window_edit")
                    else 3600,
                },
                "modes": dict(
                    self.build_service_area_transit_street_modes(),
                    transit=transit_modes,
                    max_transfer_distance_m=self.parse_float_with_default(
                        self.transit_max_transfer_distance_edit.text(),
                        "Max transfer distance",
                        500.0,
                    )
                    if hasattr(self, "transit_max_transfer_distance_edit")
                    else 500.0,
                    board_slack_s=self.parse_int_with_default(
                        self.transit_board_slack_edit.text(), "Board slack", 30
                    )
                    if hasattr(self, "transit_board_slack_edit")
                    else 30,
                    transfer_slack_s=self.parse_int_with_default(
                        self.transit_transfer_slack_edit.text(), "Transfer slack", 120
                    )
                    if hasattr(self, "transit_transfer_slack_edit")
                    else 120,
                    max_transfers=self.parse_int_with_default(
                        self.transit_max_transfers_edit.text(), "Max transfers", 3
                    )
                    if hasattr(self, "transit_max_transfers_edit")
                    else 3,
                    min_transit_leg_duration_s=self.parse_int_with_default(
                        self.transit_min_leg_duration_edit.text(),
                        "Min transit leg duration",
                        0,
                    )
                    if hasattr(self, "transit_min_leg_duration_edit")
                    else 0,
                    min_transit_leg_distance_m=self.parse_float_with_default(
                        self.transit_min_leg_distance_edit.text(),
                        "Min transit leg distance",
                        0.0,
                    )
                    if hasattr(self, "transit_min_leg_distance_edit")
                    else 0.0,
                ),
                "max_travel_time_s": int(
                    self.parse_float_with_default(
                        self.service_area_transit_max_time_edit.text(),
                        "Transit max time",
                        3600.0,
                    )
                ),
                "returns": self.build_service_area_transit_returns(),
            }

        output_mode = self.service_area_output_mode_combo.currentData() or "both"
        request = {
            "analysis_id": self.service_area_analysis_id_edit.text().strip()
            or "qgis_service_area",
            "origins": self.build_service_area_origins(),
            "thresholds": self.build_service_area_thresholds(),
            "snap": {
                "max_distance_m": self.parse_float_with_default(
                    self.service_area_snap_distance_edit.text(), "Snap distance", 500.0
                )
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
                "hull_aggressiveness": self.parse_float_with_default(
                    self.service_area_hull_aggressiveness_edit.text(),
                    "Hull aggressiveness",
                    1.0,
                )
            },
            "returns": {
                "geometry": True,
                "attributes": True,
                "per_threshold_summary": True,
                "diagnostics": True,
                "segments": self.service_area_segments_check.isChecked()
                or (self.service_area_band_mode_combo.currentData() == "none"),
            },
        }
        simplification_tolerance = self.parse_optional_float(
            self.service_area_simplification_edit.text(),
            "Service-area simplification tolerance",
        )
        if simplification_tolerance is not None:
            request["polygon"]["simplification_tolerance_m"] = simplification_tolerance
        return request

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
        if not self.ensure_service():
            return
        try:
            request = self.build_service_area_request()
        except ValueError as exc:
            self.alert("Invalid service-area request values: {}".format(exc))
            return

        endpoint = "/v1/service-area"
        payload = {"request": request}
        if self.service_area_mode_combo.currentData() == "transit":
            feed_id = self.transit_feed_combo.currentData() if hasattr(self, "transit_feed_combo") else None
            if not feed_id:
                self.alert(
                    "Select a loaded transit feed in the Transit tab first "
                    "(the API must be started with --transit-feed <feed_id>)."
                )
                return
            endpoint = "/v1/transit-service-area"
            payload["feed_id"] = feed_id
        else:
            profile_id = self.selected_profile_id()
            if profile_id:
                payload["profile_id"] = profile_id

        self.remember_service_area_request(request)
        self.save_settings()
        self.execute_api_request(
            endpoint=endpoint,
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
