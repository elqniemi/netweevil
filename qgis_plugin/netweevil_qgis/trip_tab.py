"""Trip tab: optimized multi-stop trips (POST /v1/trip)."""

import json

from qgis.PyQt.QtWidgets import (
    QCheckBox,
    QComboBox,
    QFormLayout,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPlainTextEdit,
    QPushButton,
    QVBoxLayout,
    QWidget,
)
from qgis.core import QgsCoordinateTransform, QgsProject, QgsWkbTypes
from qgis.gui import QgsMapLayerComboBox

from .compat import GEOM_POINT, LAYER_FILTER_POINT
from .constants import PickTarget, ResponseFormat, TripStopSource

TRIP_MIN_STOPS = 2
TRIP_MAX_STOPS = 2000
TRIP_MAX_EXACT_STOPS = 18
TRIP_NO_FIELD = "(none)"
TRIP_SERVICE_FIELD_NAMES = ("service_s",)
TRIP_EARLIEST_FIELD_NAMES = ("earliest_s",)
TRIP_LATEST_FIELD_NAMES = ("latest_s",)


class TripTabMixin:
    TRIP_MODE_ITEMS = [
        ("Round trip (start and end at the first stop)", "roundtrip"),
        ("Fixed start (first stop), any end", "fixed_start"),
        ("Any start, fixed end (last stop)", "fixed_end"),
        ("Fixed start and end (first and last stop)", "fixed_endpoints"),
        ("Open (any start, any end)", "open"),
    ]

    def _build_trip_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        summary = QLabel(
            "Order 2 to 2000 stops into the cheapest tour with the selected "
            "profile, honouring optional service times and time windows, then "
            "route the chosen legs. Times are seconds on your own clock (for "
            "example since midnight); H:MM or H:MM:SS is accepted too."
        )
        summary.setWordWrap(True)
        layout.addWidget(summary)

        layout.addWidget(self._build_trip_stops_group())

        options_group = QGroupBox("Trip options")
        options_form = QFormLayout(options_group)
        self.trip_id_edit = QLineEdit("qgis_trip_001")
        self.trip_id_edit.setToolTip(
            "Identifier stored in the request, response, and result layer group."
        )
        trip_id_row = QWidget()
        trip_id_layout = QHBoxLayout(trip_id_row)
        trip_id_layout.setContentsMargins(0, 0, 0, 0)
        trip_id_layout.addWidget(self.trip_id_edit)
        new_trip_id_button = QPushButton("New")
        new_trip_id_button.setToolTip("Advance to the next numbered trip id.")
        new_trip_id_button.clicked.connect(self.prepare_next_trip_defaults)
        trip_id_layout.addWidget(new_trip_id_button)
        self.trip_auto_increment_check = QCheckBox("Prepare a fresh trip id after each run")
        self.trip_auto_increment_check.setChecked(True)

        self.trip_mode_combo = QComboBox()
        for label, value in self.TRIP_MODE_ITEMS:
            self.trip_mode_combo.addItem(label, value)
        self.trip_mode_combo.setToolTip(
            "Which stops are pinned to the ends of the tour. 'First' and 'last' "
            "refer to the order of the stop list or layer."
        )
        self.trip_departure_edit = QLineEdit("0")
        self.trip_departure_edit.setToolTip(
            "Earliest start of the trip, in seconds on your clock (or H:MM[:SS])."
        )
        self.trip_default_service_edit = QLineEdit("0")
        self.trip_default_service_edit.setToolTip(
            "Service time in seconds for stops that do not set their own."
        )
        self.trip_allow_late_check = QCheckBox(
            "Allow late arrivals (return the least-late order instead of failing)"
        )
        self.trip_allow_late_check.setChecked(False)
        self.trip_snap_distance_edit = QLineEdit("500")
        self.trip_snap_distance_edit.setToolTip(
            "Maximum distance in meters from a stop to the nearest routable edge."
        )
        self.trip_geometry_combo = QComboBox()
        self.trip_geometry_combo.addItem("Full leg geometry", "full")
        self.trip_geometry_combo.addItem("Segment-friendly geometry", "segments")
        self.trip_geometry_combo.addItem("No leg geometry", "none")
        self.trip_output_path_edit = QLineEdit(self.trip_output_path_for_id("qgis_trip_001"))
        self.trip_output_path_edit.setToolTip(
            "Where the raw API response is saved. Follows the trip id unless you "
            "edit it."
        )
        self.trip_request_path_edit = QLineEdit(".netweevil/requests/trip_from_qgis.json")
        self.trip_request_path_edit.setToolTip(
            "Only used by Save Request, to reproduce this analysis outside QGIS."
        )
        options_form.addRow("Trip id", trip_id_row)
        options_form.addRow("", self.trip_auto_increment_check)
        options_form.addRow("Mode", self.trip_mode_combo)
        options_form.addRow("Departure", self.trip_departure_edit)
        options_form.addRow("Default service s", self.trip_default_service_edit)
        options_form.addRow("", self.trip_allow_late_check)
        options_form.addRow("Snap distance m", self.trip_snap_distance_edit)
        options_form.addRow("Leg geometry", self.trip_geometry_combo)
        options_form.addRow(
            "Response path",
            self._line_with_browse(
                self.trip_output_path_edit, browse_dir=False, save_dialog=True
            ),
        )
        options_form.addRow(
            "Optional request JSON",
            self._line_with_browse(
                self.trip_request_path_edit, browse_dir=False, save_dialog=True
            ),
        )
        layout.addWidget(options_group)

        optimizer_group = QGroupBox("Optimizer")
        optimizer_form = QFormLayout(optimizer_group)
        self.trip_seed_edit = QLineEdit("0")
        self.trip_seed_edit.setToolTip("Equal seeds give equal orders.")
        self.trip_max_iterations_edit = QLineEdit("")
        self.trip_max_iterations_edit.setPlaceholderText("size-dependent default")
        self.trip_max_iterations_edit.setToolTip("Local-search kicks; leave empty for the default.")
        self.trip_time_budget_edit = QLineEdit("")
        self.trip_time_budget_edit.setPlaceholderText("no wall-clock cap")
        self.trip_time_budget_edit.setToolTip(
            "Wall-clock cap on the search in milliseconds. Orders are only "
            "reproducible when the iteration limit ends the search first."
        )
        self.trip_exact_max_stops_edit = QLineEdit("")
        self.trip_exact_max_stops_edit.setPlaceholderText("16")
        self.trip_exact_max_stops_edit.setToolTip(
            "Tours with at most this many free stops are solved exactly "
            "(at most {}).".format(TRIP_MAX_EXACT_STOPS)
        )
        optimizer_form.addRow("Seed", self.trip_seed_edit)
        optimizer_form.addRow("Max iterations", self.trip_max_iterations_edit)
        optimizer_form.addRow("Time budget ms", self.trip_time_budget_edit)
        optimizer_form.addRow("Exact max stops", self.trip_exact_max_stops_edit)
        layout.addWidget(self._make_toggle_section("Optimizer", optimizer_group))
        layout.addWidget(optimizer_group)

        button_row = QHBoxLayout()
        save_request_button = QPushButton("Save Request")
        save_request_button.setToolTip("Write the request JSON to disk without running it.")
        save_request_button.clicked.connect(self.write_trip_request)
        run_trip_button = QPushButton("Run Trip")
        run_trip_button.clicked.connect(self.run_trip)
        button_row.addWidget(save_request_button)
        button_row.addStretch(1)
        button_row.addWidget(run_trip_button)
        layout.addLayout(button_row)

        self._trip_auto_output_path = self.trip_output_path_edit.text()
        self.trip_id_edit.textChanged.connect(self.sync_trip_output_path_from_trip_id)

        layout.addStretch(1)
        return tab

    def _build_trip_stops_group(self):
        group = QGroupBox("Stops")
        layout = QVBoxLayout(group)

        source_form = QFormLayout()
        self.trip_stop_source_combo = QComboBox()
        self.trip_stop_source_combo.addItem("Stop list", TripStopSource.LIST)
        self.trip_stop_source_combo.addItem("Loaded point layer", TripStopSource.LAYER)
        source_form.addRow("Source", self.trip_stop_source_combo)
        layout.addLayout(source_form)

        self.trip_stop_list_widget = QWidget()
        list_layout = QVBoxLayout(self.trip_stop_list_widget)
        list_layout.setContentsMargins(0, 0, 0, 0)
        list_note = QLabel(
            "One stop per line: id,lon,lat[,service_s[,earliest_s[,latest_s]]]. "
            "Leave a trailing value empty to omit it."
        )
        list_note.setWordWrap(True)
        list_layout.addWidget(list_note)
        self.trip_stops_edit = QPlainTextEdit()
        self.trip_stops_edit.setPlaceholderText(
            "depot,6.566500,53.219400\n"
            "shop_a,6.563600,53.218100,300\n"
            "shop_b,6.570100,53.221000,300,9:00,10:30"
        )
        self.trip_stops_edit.setMaximumBlockCount(TRIP_MAX_STOPS + 10)
        list_layout.addWidget(self.trip_stops_edit)
        self.trip_pick_status_label = QLabel(
            "Pick On Map keeps adding one stop per click until you press it again."
        )
        self.trip_pick_status_label.setWordWrap(True)
        list_layout.addWidget(self.trip_pick_status_label)
        list_buttons = QHBoxLayout()
        pick_button = QPushButton("Pick On Map")
        pick_button.setToolTip(
            "Click this button, then click the map once per stop. Click again to stop."
        )
        pick_button.clicked.connect(lambda: self.begin_point_pick(PickTarget.TRIP_STOP))
        add_selected_button = QPushButton("Add Selected Features")
        add_selected_button.setToolTip(
            "Append every selected feature of the active point layer. Fields "
            "named service_s, earliest_s, and latest_s are copied when present."
        )
        add_selected_button.clicked.connect(self.add_trip_stops_from_selected_features)
        use_route_button = QPushButton("Use Route Start + End")
        use_route_button.clicked.connect(self.use_route_points_for_trip)
        clear_button = QPushButton("Clear Stops")
        clear_button.clicked.connect(self.clear_trip_stops)
        list_buttons.addWidget(pick_button)
        list_buttons.addWidget(add_selected_button)
        list_buttons.addWidget(use_route_button)
        list_buttons.addWidget(clear_button)
        list_buttons.addStretch(1)
        list_layout.addLayout(list_buttons)
        layout.addWidget(self.trip_stop_list_widget)

        self.trip_stop_layer_widget = QWidget()
        layer_form = QFormLayout(self.trip_stop_layer_widget)
        layer_form.setContentsMargins(0, 0, 0, 0)
        self.trip_layer_combo = QgsMapLayerComboBox()
        self.trip_layer_combo.setFilters(LAYER_FILTER_POINT)
        self.trip_id_field_combo = QComboBox()
        self.trip_selected_only_check = QCheckBox("Use selected features only")
        self.trip_service_field_combo = QComboBox()
        self.trip_service_field_combo.setToolTip(
            "Per-stop service seconds; '(none)' uses the default service time."
        )
        self.trip_earliest_field_combo = QComboBox()
        self.trip_earliest_field_combo.setToolTip(
            "Earliest service start (seconds or H:MM[:SS]); the vehicle waits."
        )
        self.trip_latest_field_combo = QComboBox()
        self.trip_latest_field_combo.setToolTip(
            "Latest service start (seconds or H:MM[:SS])."
        )
        layer_note = QLabel(
            "Stops follow the layer's feature order, which matters for the "
            "fixed start/end modes."
        )
        layer_note.setWordWrap(True)
        layer_form.addRow("Point layer", self.trip_layer_combo)
        layer_form.addRow("ID field", self.trip_id_field_combo)
        layer_form.addRow("", self.trip_selected_only_check)
        layer_form.addRow("Service s field", self.trip_service_field_combo)
        layer_form.addRow("Earliest field", self.trip_earliest_field_combo)
        layer_form.addRow("Latest field", self.trip_latest_field_combo)
        layer_form.addRow("", layer_note)
        layout.addWidget(self.trip_stop_layer_widget)

        self.trip_layer_combo.layerChanged.connect(self.populate_trip_layer_fields)
        self.trip_stop_source_combo.currentIndexChanged.connect(
            self.update_trip_stop_source_visibility
        )
        self.populate_trip_layer_fields()
        self.update_trip_stop_source_visibility()
        return group

    def update_trip_stop_source_visibility(self, *_args):
        using_layer = self.trip_stop_source_combo.currentData() == TripStopSource.LAYER
        self.trip_stop_layer_widget.setVisible(using_layer)
        self.trip_stop_list_widget.setVisible(not using_layer)

    def populate_trip_layer_fields(self, *_args):
        layer = self.trip_layer_combo.currentLayer()
        self.populate_id_fields(layer, self.trip_id_field_combo)
        for combo, preferred in [
            (self.trip_service_field_combo, TRIP_SERVICE_FIELD_NAMES),
            (self.trip_earliest_field_combo, TRIP_EARLIEST_FIELD_NAMES),
            (self.trip_latest_field_combo, TRIP_LATEST_FIELD_NAMES),
        ]:
            combo.clear()
            combo.addItem(TRIP_NO_FIELD)
            if layer is None:
                continue
            names = [field.name() for field in layer.fields()]
            for name in names:
                combo.addItem(name)
            for name in names:
                if name.lower() in preferred:
                    combo.setCurrentIndex(combo.findText(name))
                    break

    def trip_output_path_for_id(self, trip_id):
        clean = (trip_id or "qgis_trip_001").strip() or "qgis_trip_001"
        return ".netweevil/runs/trips/{}.json".format(clean)

    def sync_trip_output_path_from_trip_id(self, *_args):
        # Only follow the id while the path still is the one we generated.
        if self.trip_output_path_edit.text().strip() != self._trip_auto_output_path:
            return
        self._trip_auto_output_path = self.trip_output_path_for_id(self.trip_id_edit.text())
        self.trip_output_path_edit.setText(self._trip_auto_output_path)

    def prepare_next_trip_defaults(self):
        self.trip_id_edit.setText(self.next_numbered_id(self.trip_id_edit.text(), "qgis_trip"))

    def trip_stop_line_count(self):
        return len(
            [line for line in self.trip_stops_edit.toPlainText().splitlines() if line.strip()]
        )

    def append_trip_stop(self, point_id, lon, lat, extras=None):
        values = [str(point_id), "{:.6f}".format(lon), "{:.6f}".format(lat)]
        extras = list(extras or [])
        while extras and extras[-1] in [None, ""]:
            extras.pop()
        values.extend("" if value in [None, ""] else str(value) for value in extras)
        lines = [line for line in self.trip_stops_edit.toPlainText().splitlines() if line.strip()]
        lines.append(",".join(values))
        self.trip_stops_edit.setPlainText("\n".join(lines))

    def clear_trip_stops(self):
        self.trip_stops_edit.clear()
        self.trip_pick_status_label.setText("Cleared the trip stop list.")

    def use_route_points_for_trip(self):
        added = 0
        for id_edit, lon_edit, lat_edit, fallback in [
            (self.origin_id_edit, self.origin_lon_edit, self.origin_lat_edit, "origin"),
            (
                self.destination_id_edit,
                self.destination_lon_edit,
                self.destination_lat_edit,
                "destination",
            ),
        ]:
            try:
                lon = float(lon_edit.text().strip())
                lat = float(lat_edit.text().strip())
            except ValueError:
                continue
            self.append_trip_stop(id_edit.text().strip() or fallback, lon, lat)
            added += 1
        if added == 0:
            self.alert("Set the route start or end point first.")
            return
        self.trip_pick_status_label.setText(
            "Appended {} route point(s) as trip stops.".format(added)
        )

    def add_trip_stops_from_selected_features(self):
        layer = self.iface.activeLayer()
        if layer is None:
            self.alert("Select a point layer with one or more selected features first.")
            return
        if QgsWkbTypes.geometryType(layer.wkbType()) != GEOM_POINT:
            self.alert("The active layer must be a point layer.")
            return
        if not layer.selectedFeatureIds():
            self.alert("Select one or more point features in the active layer.")
            return
        field_names = {field.name().lower(): field.name() for field in layer.fields()}
        extra_fields = [
            next((field_names[name] for name in names if name in field_names), None)
            for names in (
                TRIP_SERVICE_FIELD_NAMES,
                TRIP_EARLIEST_FIELD_NAMES,
                TRIP_LATEST_FIELD_NAMES,
            )
        ]
        try:
            transform = QgsCoordinateTransform(layer.crs(), self.wgs84, QgsProject.instance())
            added = 0
            for feature in layer.getSelectedFeatures():
                point = transform.transform(self.feature_point(feature))
                extras = [
                    self.feature_value(feature, field_name) if field_name else ""
                    for field_name in extra_fields
                ]
                self.append_trip_stop(
                    self.feature_label(feature, "stop"), point.x(), point.y(), extras
                )
                added += 1
        except Exception as exc:
            self.alert("Failed to append selected trip stops: {}".format(exc))
            return
        self.trip_pick_status_label.setText(
            "Added {} stop(s) from '{}'.".format(added, layer.name())
        )

    def parse_trip_clock(self, raw_value, label):
        """Seconds as a number, or H:MM / H:MM:SS on the same clock."""
        text = str(raw_value if raw_value is not None else "").strip()
        if not text or text.upper() == "NULL":
            return None
        if ":" in text:
            parts = text.split(":")
            if len(parts) not in (2, 3):
                raise ValueError("{} must be seconds or H:MM[:SS]".format(label))
            try:
                numbers = [float(part) for part in parts]
            except ValueError as exc:
                raise ValueError("{} must be seconds or H:MM[:SS]".format(label)) from exc
            seconds = numbers[0] * 3600.0 + numbers[1] * 60.0
            if len(numbers) == 3:
                seconds += numbers[2]
            return seconds
        try:
            return float(text)
        except ValueError as exc:
            raise ValueError("{} must be seconds or H:MM[:SS]".format(label)) from exc

    def build_trip_stop(self, point_id, lon, lat, service_raw, earliest_raw, latest_raw):
        default_service = self.parse_float_with_default(
            self.trip_default_service_edit.text(), "Default service time", 0.0
        )
        service = self.parse_trip_clock(service_raw, "Service time of '{}'".format(point_id))
        service = default_service if service is None else service
        if service < 0.0:
            raise ValueError("Service time of '{}' cannot be negative".format(point_id))
        stop = {
            "point": {"id": point_id, "lon": lon, "lat": lat},
            "service_s": service,
        }
        earliest = self.parse_trip_clock(earliest_raw, "Earliest time of '{}'".format(point_id))
        latest = self.parse_trip_clock(latest_raw, "Latest time of '{}'".format(point_id))
        if earliest is not None or latest is not None:
            window = {"earliest_s": earliest if earliest is not None else 0.0}
            if latest is not None:
                if latest < window["earliest_s"]:
                    raise ValueError(
                        "Time window of '{}' closes before it opens".format(point_id)
                    )
                window["latest_s"] = latest
            stop["time_window"] = window
        return stop

    def build_trip_stops_from_list(self):
        stops = []
        for raw_line in self.trip_stops_edit.toPlainText().splitlines():
            line = raw_line.strip()
            if not line:
                continue
            parts = [part.strip() for part in line.split(",")]
            if len(parts) < 3 or len(parts) > 6:
                raise ValueError(
                    "Each trip stop line must be id,lon,lat[,service_s[,earliest_s"
                    "[,latest_s]]]. Invalid line: {}".format(raw_line)
                )
            point_id = parts[0] or "stop_{:03d}".format(len(stops) + 1)
            try:
                lon = float(parts[1])
                lat = float(parts[2])
            except ValueError as exc:
                raise ValueError(
                    "Trip stop '{}' has invalid lon/lat values".format(point_id)
                ) from exc
            extras = parts[3:] + [""] * (6 - len(parts))
            stops.append(self.build_trip_stop(point_id, lon, lat, *extras))
        return stops

    def build_trip_stops_from_layer(self):
        layer = self.trip_layer_combo.currentLayer()
        if layer is None:
            raise ValueError("choose a point layer for the trip stops")
        if QgsWkbTypes.geometryType(layer.wkbType()) != GEOM_POINT:
            raise ValueError("trip stop layer '{}' is not a point layer".format(layer.name()))
        if self.trip_selected_only_check.isChecked():
            feature_iter = layer.getSelectedFeatures()
        else:
            feature_iter = layer.getFeatures()
        id_field = self.trip_id_field_combo.currentText().strip()
        extra_fields = [
            combo.currentText().strip()
            for combo in (
                self.trip_service_field_combo,
                self.trip_earliest_field_combo,
                self.trip_latest_field_combo,
            )
        ]
        transform = QgsCoordinateTransform(layer.crs(), self.wgs84, QgsProject.instance())
        stops = []
        for feature in feature_iter:
            point = transform.transform(self.feature_point(feature))
            point_id = self.feature_value(feature, id_field) or "{}_{}".format(
                layer.name(), feature.id()
            )
            extras = [
                self.feature_value(feature, name) if name and name != TRIP_NO_FIELD else ""
                for name in extra_fields
            ]
            stops.append(self.build_trip_stop(point_id, point.x(), point.y(), *extras))
        return stops

    def build_trip_stops(self):
        if self.trip_stop_source_combo.currentData() == TripStopSource.LAYER:
            stops = self.build_trip_stops_from_layer()
        else:
            stops = self.build_trip_stops_from_list()
        if len(stops) < TRIP_MIN_STOPS:
            raise ValueError(
                "A trip needs at least {} stops (Pick On Map, Add Selected "
                "Features, or a point layer).".format(TRIP_MIN_STOPS)
            )
        if len(stops) > TRIP_MAX_STOPS:
            raise ValueError(
                "A trip supports at most {} stops; got {}.".format(TRIP_MAX_STOPS, len(stops))
            )
        return stops

    def build_trip_optimizer(self):
        optimizer = {
            "seed": self.parse_int_with_default(self.trip_seed_edit.text(), "Optimizer seed", 0)
        }
        if optimizer["seed"] < 0:
            raise ValueError("Optimizer seed cannot be negative")
        for key, edit, label in [
            ("max_iterations", self.trip_max_iterations_edit, "Max iterations"),
            ("time_budget_ms", self.trip_time_budget_edit, "Time budget ms"),
            ("exact_max_stops", self.trip_exact_max_stops_edit, "Exact max stops"),
        ]:
            value = self.parse_optional_int(edit.text(), label)
            if value is None:
                continue
            if value < 0:
                raise ValueError("{} cannot be negative".format(label))
            optimizer[key] = value
        if optimizer.get("exact_max_stops", 0) > TRIP_MAX_EXACT_STOPS:
            raise ValueError("Exact max stops is at most {}".format(TRIP_MAX_EXACT_STOPS))
        return optimizer

    def build_trip_request(self):
        departure = self.parse_trip_clock(self.trip_departure_edit.text(), "Departure")
        snap_distance = self.parse_float_with_default(
            self.trip_snap_distance_edit.text(), "Snap distance", 500.0
        )
        return {
            "trip_id": self.trip_id_edit.text().strip() or "qgis_trip",
            "stops": self.build_trip_stops(),
            "mode": self.trip_mode_combo.currentData() or "roundtrip",
            "departure_s": departure if departure is not None else 0.0,
            "allow_late": self.trip_allow_late_check.isChecked(),
            "snap": {"max_distance_m": snap_distance},
            "returns": {"geometry": self.trip_geometry_combo.currentData() or "full"},
            "optimizer": self.build_trip_optimizer(),
        }

    def write_trip_request(self):
        try:
            request = self.build_trip_request()
        except ValueError as exc:
            self.alert("Invalid trip request values: {}".format(exc))
            return
        request_path = self.resolve_local_path(self.trip_request_path_edit.text())
        request_path.parent.mkdir(parents=True, exist_ok=True)
        request_path.write_text(json.dumps(request, indent=2), encoding="utf-8")
        self.log("Wrote trip request to {}".format(request_path))
        self.save_settings()

    def run_trip(self):
        if not self.ensure_service():
            return
        try:
            payload = {"request": self.build_trip_request()}
        except ValueError as exc:
            self.alert("Invalid trip request values: {}".format(exc))
            return
        profile_id = self.selected_profile_id()
        if profile_id:
            payload["profile_id"] = profile_id

        self.save_settings()
        self.log(
            "Optimizing trip '{}' over {} stop(s) ({}).".format(
                payload["request"]["trip_id"],
                len(payload["request"]["stops"]),
                payload["request"]["mode"],
            )
        )
        if self.execute_api_request(
            endpoint="/v1/trip",
            payload=payload,
            output_path=self.trip_output_path_edit.text(),
            layer_name=payload["request"]["trip_id"] or "netweevil_trip",
            analysis_kind="trip",
            response_format_override=ResponseFormat.JSON,
        ):
            if self.trip_auto_increment_check.isChecked():
                self.prepare_next_trip_defaults()
