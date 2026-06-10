"""Route tab: UI, request building, and run handling."""

import json

from qgis.PyQt.QtWidgets import (
    QCheckBox,
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

from .constants import ResponseFormat, PickTarget


class RouteTabMixin:
    def _build_route_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        summary = QLabel(
            "Pick a start and end point on the map (or pull them from a selected "
            "point feature), then press Run Route. Points are sent to the API in "
            "WGS84 longitude/latitude."
        )
        summary.setWordWrap(True)
        layout.addWidget(summary)

        layout.addWidget(self._build_route_point_group("Start", PickTarget.ORIGIN))
        layout.addWidget(self._build_route_point_group("End", PickTarget.DESTINATION))

        point_row = QHBoxLayout()
        swap_button = QPushButton("Swap Start/End")
        swap_button.clicked.connect(self.swap_route_points)
        clear_button = QPushButton("Clear Points")
        clear_button.clicked.connect(self.clear_route_points)
        point_row.addWidget(swap_button)
        point_row.addWidget(clear_button)
        point_row.addStretch(1)
        layout.addLayout(point_row)

        self.pick_status_label = QLabel(
            "Use Pick On Map, then click the map canvas. Click the button again to cancel."
        )
        self.pick_status_label.setWordWrap(True)
        layout.addWidget(self.pick_status_label)

        options_group = QGroupBox("Route options")
        options_form = QFormLayout(options_group)
        self.route_id_edit = QLineEdit("qgis_route_001")
        self.route_id_edit.setToolTip(
            "Identifier stored in the request, response, and result layer group."
        )
        self.route_auto_increment_check = QCheckBox("Prepare a fresh route id after each run")
        self.route_auto_increment_check.setChecked(True)
        self.route_auto_increment_check.setToolTip(
            "Advance the route id after each successful run so the next run "
            "never overwrites the previous result layers or files."
        )
        self.route_auto_output_path_check = QCheckBox("Keep response path in sync with route id")
        self.route_auto_output_path_check.setChecked(True)
        self.snap_distance_edit = QLineEdit("500")
        self.snap_distance_edit.setToolTip(
            "Maximum distance in meters from a picked point to the nearest routable edge."
        )
        self.route_output_path_edit = QLineEdit(".netweevil/runs/routes/qgis_route_001.json")
        self.route_output_path_edit.setToolTip(
            "Where the raw API response is saved. Relative paths resolve against "
            "the workspace root configured in Settings."
        )
        self.route_request_path_edit = QLineEdit(".netweevil/requests/route_from_qgis.json")
        self.route_request_path_edit.setToolTip(
            "Only used by Save Request, to reproduce this analysis outside QGIS."
        )
        route_id_row = QWidget()
        route_id_layout = QHBoxLayout(route_id_row)
        route_id_layout.setContentsMargins(0, 0, 0, 0)
        route_id_layout.addWidget(self.route_id_edit)
        new_route_id_button = QPushButton("New")
        new_route_id_button.setToolTip("Advance to the next numbered route id.")
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
        layout.addWidget(self._make_toggle_section("Returned Route Detail", return_group))
        layout.addWidget(return_group)

        route_advanced_widget = self._build_advanced_controls(
            "route", include_failure_modes=True
        )
        self.route_advanced_toggle = self._make_toggle_section(
            "Connectivity + Fallback", route_advanced_widget
        )
        layout.addWidget(self.route_advanced_toggle)
        layout.addWidget(route_advanced_widget)

        button_row = QHBoxLayout()
        save_request_button = QPushButton("Save Request")
        save_request_button.setToolTip("Write the request JSON to disk without running it.")
        save_request_button.clicked.connect(self.write_route_request)
        run_route_button = QPushButton("Run Route")
        run_route_button.clicked.connect(self.run_route)
        button_row.addWidget(save_request_button)
        button_row.addStretch(1)
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
        pick_button.setToolTip(
            "Click this button, then click the {} point on the map canvas. "
            "Click again to cancel.".format(title.lower())
        )
        pick_button.clicked.connect(lambda: self.begin_point_pick(target))
        selected_button = QPushButton("From Selected Feature")
        selected_button.setToolTip(
            "Use the single selected feature of the active point layer."
        )
        selected_button.clicked.connect(lambda: self.use_selected_feature_for_target(target))
        button_row.addWidget(pick_button)
        button_row.addWidget(selected_button)
        button_row.addStretch(1)
        layout.addLayout(button_row)
        return group

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
        self.pick_status_label.setText(
            "Use Pick On Map, then click the map canvas. Click the button again to cancel."
        )
        self.update_point_markers()

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

    def build_route_request(self):
        route_id = self.route_id_edit.text().strip() or "qgis_route"
        origin_lon, origin_lat = self.require_point(
            self.origin_lon_edit, self.origin_lat_edit, "route start point (Pick On Map)"
        )
        destination_lon, destination_lat = self.require_point(
            self.destination_lon_edit,
            self.destination_lat_edit,
            "route end point (Pick On Map)",
        )
        snap_distance = self.parse_optional_float(
            self.snap_distance_edit.text(), "Snap distance"
        )
        return {
            "route_id": route_id,
            "origin": {
                "id": self.origin_id_edit.text().strip() or "origin",
                "lon": origin_lon,
                "lat": origin_lat,
            },
            "destination": {
                "id": self.destination_id_edit.text().strip() or "destination",
                "lon": destination_lon,
                "lat": destination_lat,
            },
            "snap": {"max_distance_m": snap_distance if snap_distance is not None else 500.0},
            "connectivity": self.build_connectivity_policy("route"),
            "fallback": self.build_fallback_policy("route"),
            "returns": self.build_route_returns(),
            "alternatives": self.build_alternative_options("route"),
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
        if not self.ensure_service():
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
