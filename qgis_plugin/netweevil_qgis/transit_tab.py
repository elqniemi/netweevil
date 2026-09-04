"""Transit tab: UI, request building, and run handling."""

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


class TransitTabMixin:
    STREET_MODE_ITEMS = [
        ("Walking", "walk"),
        ("Cycling", "bicycle"),
        ("Car", "car"),
    ]
    STREET_MODE_ACCESS_DISTANCE_KEYS = {
        "walk": "max_access_distance_m",
        "bicycle": "max_bicycle_access_distance_m",
        "car": "max_car_access_distance_m",
    }
    STREET_MODE_EGRESS_DISTANCE_KEYS = {
        "walk": "max_egress_distance_m",
        "bicycle": "max_bicycle_egress_distance_m",
        "car": "max_car_egress_distance_m",
    }
    STREET_MODE_DEFAULT_DISTANCES = {
        "walk": "1200",
        "bicycle": "5000",
        "car": "15000",
    }

    def _build_transit_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        summary = QLabel(
            "Plan a street access + scheduled transit route through a GTFS "
            "feed loaded by the API. The first mile can be walked, cycled, or "
            "driven; pick an origin and destination, set the departure time, "
            "then press Run Transit Route."
        )
        summary.setWordWrap(True)
        layout.addWidget(summary)

        options_group = QGroupBox("Transit route options")
        options_form = QFormLayout(options_group)
        self.transit_feed_combo = QComboBox()
        self.transit_feed_combo.setToolTip(
            "GTFS feeds preloaded by the API ('netweevil api serve --transit-feed <id>')."
        )
        self.transit_feed_combo.addItem("No transit feeds loaded", "")
        self.transit_route_id_edit = QLineEdit("qgis_transit_001")
        self.transit_auto_increment_check = QCheckBox(
            "Prepare a fresh transit route id after each run"
        )
        self.transit_auto_increment_check.setChecked(True)
        self.transit_datetime_edit = QLineEdit("2026-05-11T08:30:00+02:00")
        self.transit_datetime_edit.setToolTip(
            "Departure time as an RFC 3339 timestamp inside the feed's service "
            "calendar, for example 2026-05-11T08:30:00+02:00."
        )
        self.transit_search_window_edit = QLineEdit("7200")
        self.transit_search_window_edit.setToolTip(
            "Seconds after the departure time in which trips may start."
        )
        self.transit_output_path_edit = QLineEdit(
            ".netweevil/runs/transit/qgis_transit_001.json"
        )
        self.transit_request_path_edit = QLineEdit(
            ".netweevil/requests/transit_from_qgis.json"
        )
        self.transit_request_path_edit.setToolTip(
            "Only used by Save Request, to reproduce this analysis outside QGIS."
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

        layout.addWidget(
            self._build_transit_point_group("Origin", PickTarget.TRANSIT_ORIGIN)
        )
        layout.addWidget(
            self._build_transit_point_group("Destination", PickTarget.TRANSIT_DESTINATION)
        )

        point_row = QHBoxLayout()
        swap_button = QPushButton("Swap Origin/Destination")
        swap_button.clicked.connect(self.swap_transit_points)
        clear_button = QPushButton("Clear Points")
        clear_button.clicked.connect(self.clear_transit_points)
        point_row.addWidget(swap_button)
        point_row.addWidget(clear_button)
        point_row.addStretch(1)
        layout.addLayout(point_row)

        self.transit_pick_status_label = QLabel(
            "Use Pick On Map, then click the map canvas. Click the button again to cancel."
        )
        self.transit_pick_status_label.setWordWrap(True)
        layout.addWidget(self.transit_pick_status_label)

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
        self.transit_access_mode_combo = QComboBox()
        for label, value in self.STREET_MODE_ITEMS:
            self.transit_access_mode_combo.addItem(label, value)
        self.transit_access_mode_combo.setToolTip(
            "Street mode used for the first mile from the origin to the first stop."
        )
        self.transit_separate_egress_check = QCheckBox(
            "Advanced: use a different last-mile (egress) mode"
        )
        self.transit_separate_egress_check.setChecked(False)
        self.transit_separate_egress_check.setToolTip(
            "Plan the first and last mile with different street modes, for "
            "example cycle to the station and walk from the final stop."
        )
        self.transit_egress_mode_combo = QComboBox()
        for label, value in self.STREET_MODE_ITEMS:
            self.transit_egress_mode_combo.addItem(label, value)
        self.transit_egress_mode_combo.setToolTip(
            "Street mode used for the last mile from the final stop to the destination."
        )
        self.transit_egress_mode_combo.setEnabled(False)
        self.transit_walk_speed_edit = QLineEdit("4.8")
        self.transit_bicycle_speed_edit = QLineEdit("15")
        self.transit_car_speed_edit = QLineEdit("25")
        self.transit_car_speed_edit.setToolTip(
            "Average door-to-stop car speed in kph, including parking."
        )
        self.transit_max_access_distance_edit = QLineEdit("1200")
        self.transit_max_access_distance_edit.setToolTip(
            "Maximum first-mile distance in meters from the origin to the "
            "first stop, using the selected access mode."
        )
        self.transit_max_egress_distance_edit = QLineEdit("1200")
        self.transit_max_egress_distance_edit.setToolTip(
            "Maximum last-mile distance in meters from the last stop to the "
            "destination, using the selected egress mode."
        )
        self.transit_max_transfer_distance_edit = QLineEdit("500")
        self.transit_board_slack_edit = QLineEdit("30")
        self.transit_board_slack_edit.setToolTip(
            "Minimum seconds between arriving at a stop and boarding a vehicle."
        )
        self.transit_transfer_slack_edit = QLineEdit("120")
        self.transit_transfer_slack_edit.setToolTip(
            "Minimum seconds reserved for each transfer."
        )
        self.transit_max_transfers_edit = QLineEdit("3")
        self.transit_min_leg_duration_edit = QLineEdit("0")
        self.transit_min_leg_distance_edit = QLineEdit("0")
        self.transit_alternative_count_edit = QLineEdit("1")
        self.transit_alternative_time_ratio_edit = QLineEdit("1.5")
        self.transit_include_geometry_check = QCheckBox("Load leg geometry")
        self.transit_include_geometry_check.setChecked(True)
        self.transit_network_walk_geometry_check = QCheckBox(
            "Draw street legs over the road network"
        )
        self.transit_network_walk_geometry_check.setChecked(False)
        self.transit_network_walk_geometry_check.setToolTip(
            "Route walking legs over the road network using the profile selected "
            "in the connection bar (must be a pedestrian profile) instead of "
            "straight lines. Cycling and car first/last-mile legs use a loaded "
            "profile of the matching mode when the API has one."
        )
        self.transit_include_stops_check = QCheckBox("Load transit stops")
        self.transit_include_stops_check.setChecked(True)
        self.transit_include_stop_segments_check = QCheckBox("Load stop-to-stop segments")
        self.transit_include_stop_segments_check.setChecked(True)
        mode_form.addRow("Transit modes", transit_modes_row)
        mode_form.addRow("Access (first mile)", self.transit_access_mode_combo)
        mode_form.addRow("", self.transit_separate_egress_check)
        mode_form.addRow("Egress (last mile)", self.transit_egress_mode_combo)
        mode_form.addRow("Walk speed kph", self.transit_walk_speed_edit)
        mode_form.addRow("Cycling speed kph", self.transit_bicycle_speed_edit)
        mode_form.addRow("Car access speed kph", self.transit_car_speed_edit)
        mode_form.addRow("Max access distance m", self.transit_max_access_distance_edit)
        mode_form.addRow("Max egress distance m", self.transit_max_egress_distance_edit)
        mode_form.addRow("Max transfer distance m", self.transit_max_transfer_distance_edit)
        mode_form.addRow("Board slack s", self.transit_board_slack_edit)
        mode_form.addRow("Transfer slack s", self.transit_transfer_slack_edit)
        mode_form.addRow("Max transfers", self.transit_max_transfers_edit)
        mode_form.addRow("Min transit leg duration s", self.transit_min_leg_duration_edit)
        mode_form.addRow("Min transit leg distance m", self.transit_min_leg_distance_edit)
        mode_form.addRow("Alternative max routes", self.transit_alternative_count_edit)
        mode_form.addRow("Alternative max time ratio", self.transit_alternative_time_ratio_edit)
        mode_form.addRow("", self.transit_include_geometry_check)
        mode_form.addRow("", self.transit_network_walk_geometry_check)
        mode_form.addRow("", self.transit_include_stops_check)
        mode_form.addRow("", self.transit_include_stop_segments_check)
        layout.addWidget(self._make_toggle_section("Modes and Limits", mode_group))
        layout.addWidget(mode_group)

        button_row = QHBoxLayout()
        save_request_button = QPushButton("Save Request")
        save_request_button.setToolTip("Write the request JSON to disk without running it.")
        save_request_button.clicked.connect(self.write_transit_request)
        run_button = QPushButton("Run Transit Route")
        run_button.clicked.connect(self.run_transit_route)
        button_row.addWidget(save_request_button)
        button_row.addStretch(1)
        button_row.addWidget(run_button)
        layout.addLayout(button_row)

        self.transit_route_id_edit.textChanged.connect(
            self.sync_transit_output_path_from_route_id
        )
        self.sync_transit_output_path_from_route_id()
        self.transit_separate_egress_check.toggled.connect(
            self.update_transit_egress_mode_state
        )
        self.transit_access_mode_combo.currentIndexChanged.connect(
            self.sync_transit_access_mode_defaults
        )
        self.transit_egress_mode_combo.currentIndexChanged.connect(
            self.sync_transit_egress_mode_defaults
        )
        self.update_transit_egress_mode_state()

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
        pick_button.setToolTip(
            "Click this button, then click the {} on the map canvas. "
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
            "Use Pick On Map, then click the map canvas. Click the button again to cancel."
        )

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

    def selected_transit_feed_id(self):
        return self.transit_feed_combo.currentData() or ""

    def selected_transit_access_mode(self):
        return self.transit_access_mode_combo.currentData() or "walk"

    def selected_transit_egress_mode(self):
        if self.transit_separate_egress_check.isChecked():
            return self.transit_egress_mode_combo.currentData() or "walk"
        return self.selected_transit_access_mode()

    def update_transit_egress_mode_state(self, *_args):
        separate = self.transit_separate_egress_check.isChecked()
        self.transit_egress_mode_combo.setEnabled(separate)
        if not separate:
            self.set_combo_by_data(
                self.transit_egress_mode_combo, self.selected_transit_access_mode()
            )

    def sync_transit_access_mode_defaults(self, *_args):
        mode = self.selected_transit_access_mode()
        self.transit_max_access_distance_edit.setText(
            self.STREET_MODE_DEFAULT_DISTANCES.get(mode, "1200")
        )
        if not self.transit_separate_egress_check.isChecked():
            self.set_combo_by_data(self.transit_egress_mode_combo, mode)
            self.transit_max_egress_distance_edit.setText(
                self.STREET_MODE_DEFAULT_DISTANCES.get(mode, "1200")
            )

    def sync_transit_egress_mode_defaults(self, *_args):
        if not self.transit_separate_egress_check.isChecked():
            return
        mode = self.transit_egress_mode_combo.currentData() or "walk"
        self.transit_max_egress_distance_edit.setText(
            self.STREET_MODE_DEFAULT_DISTANCES.get(mode, "1200")
        )

    def build_transit_street_modes(self):
        """First/last-mile options shared by transit routes and service areas."""
        access_mode = self.selected_transit_access_mode()
        egress_mode = self.selected_transit_egress_mode()
        modes = {
            "access": [access_mode],
            "egress": [egress_mode],
            "walk_speed_kph": self.parse_float_with_default(
                self.transit_walk_speed_edit.text(), "Walk speed", 4.8
            ),
            "bicycle_speed_kph": self.parse_float_with_default(
                self.transit_bicycle_speed_edit.text(), "Cycling speed", 15.0
            ),
            "car_access_speed_kph": self.parse_float_with_default(
                self.transit_car_speed_edit.text(), "Car access speed", 25.0
            ),
        }
        if access_mode != egress_mode:
            modes["mixed_access_egress"] = True
        modes[self.STREET_MODE_ACCESS_DISTANCE_KEYS[access_mode]] = (
            self.parse_float_with_default(
                self.transit_max_access_distance_edit.text(),
                "Max access distance",
                float(self.STREET_MODE_DEFAULT_DISTANCES[access_mode]),
            )
        )
        modes[self.STREET_MODE_EGRESS_DISTANCE_KEYS[egress_mode]] = (
            self.parse_float_with_default(
                self.transit_max_egress_distance_edit.text(),
                "Max egress distance",
                float(self.STREET_MODE_DEFAULT_DISTANCES[egress_mode]),
            )
        )
        return modes

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
        transit_alternatives = {
            "max_routes": self.parse_optional_int(
                self.transit_alternative_count_edit.text(),
                "Transit alternative max routes",
            )
            or 1
        }
        max_time_ratio = self.parse_optional_float(
            self.transit_alternative_time_ratio_edit.text(),
            "Transit alternative max time ratio",
        )
        if max_time_ratio is not None:
            transit_alternatives["max_time_ratio"] = max_time_ratio
        origin_lon, origin_lat = self.require_point(
            self.transit_origin_lon_edit,
            self.transit_origin_lat_edit,
            "transit origin (Pick On Map)",
        )
        destination_lon, destination_lat = self.require_point(
            self.transit_destination_lon_edit,
            self.transit_destination_lat_edit,
            "transit destination (Pick On Map)",
        )
        departure_datetime = self.transit_datetime_edit.text().strip()
        if not departure_datetime:
            raise ValueError(
                "Set a departure time such as 2026-05-11T08:30:00+02:00 first."
            )
        return {
            "route_id": self.transit_route_id_edit.text().strip() or "qgis_transit",
            "origin": {
                "id": self.transit_origin_id_edit.text().strip() or "origin",
                "lon": origin_lon,
                "lat": origin_lat,
            },
            "destination": {
                "id": self.transit_destination_id_edit.text().strip() or "destination",
                "lon": destination_lon,
                "lat": destination_lat,
            },
            "time": {
                "datetime": departure_datetime,
                "arrive_by": False,
                "search_window_s": self.parse_optional_int(
                    self.transit_search_window_edit.text(),
                    "Transit search window",
                )
                or 7200,
            },
            "modes": dict(
                self.build_transit_street_modes(),
                transit=self.selected_transit_modes(),
                max_transfer_distance_m=self.parse_float_with_default(
                    self.transit_max_transfer_distance_edit.text(),
                    "Max transfer distance",
                    500.0,
                ),
                board_slack_s=self.parse_int_with_default(
                    self.transit_board_slack_edit.text(), "Board slack", 30
                ),
                transfer_slack_s=self.parse_int_with_default(
                    self.transit_transfer_slack_edit.text(), "Transfer slack", 120
                ),
                max_transfers=self.parse_int_with_default(
                    self.transit_max_transfers_edit.text(), "Max transfers", 3
                ),
                min_transit_leg_duration_s=self.parse_optional_int(
                    self.transit_min_leg_duration_edit.text(),
                    "Min transit leg duration",
                )
                or 0,
                min_transit_leg_distance_m=self.parse_optional_float(
                    self.transit_min_leg_distance_edit.text(),
                    "Min transit leg distance",
                )
                or 0.0,
            ),
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
            "alternatives": transit_alternatives,
        }

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
        if not self.ensure_service():
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
