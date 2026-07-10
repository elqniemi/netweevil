"""Simulation tab: scenario builder, live control, playback, and layers."""

import json
import re
import urllib.request
from pathlib import Path

from qgis.PyQt.QtCore import QTimer
from qgis.PyQt.QtWidgets import (
    QCheckBox,
    QComboBox,
    QFileDialog,
    QFormLayout,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QListWidget,
    QPushButton,
    QSlider,
    QVBoxLayout,
    QWidget,
)
from qgis.core import (
    QgsCategorizedSymbolRenderer,
    QgsCoordinateTransform,
    QgsFeature,
    QgsGeometry,
    QgsGraduatedSymbolRenderer,
    QgsLineSymbol,
    QgsMarkerSymbol,
    QgsPointXY,
    QgsProject,
    QgsRendererCategory,
    QgsRendererRange,
    QgsVectorLayer,
)
from qgis.gui import QgsMapLayerComboBox

from .compat import (
    qt_orientation,
    MSG_WARNING,
    LAYER_FILTER_POINT,
    LAYER_FILTER_POLYGON,
)


class SimulationTabMixin:
    SIM_FLEET_COLORS = ["#1971c2", "#e8590c", "#2b8a3e", "#9c36b5", "#c92a2a", "#0b7285"]

    def _build_simulation_tab(self):
        self.sim_fleets = []
        self.sim_zones = []
        self.sim_id = None
        self.sim_status = None
        self.sim_frame_cache = {}
        self.sim_playing = False
        self.sim_current_frame = 0
        self.sim_agent_layer_id = None
        self.sim_status_timer = QTimer(self)
        self.sim_status_timer.setInterval(1500)
        self.sim_status_timer.timeout.connect(self.sim_poll_status)
        self.sim_playback_timer = QTimer(self)
        self.sim_playback_timer.timeout.connect(self.sim_playback_tick)

        tab = QWidget()
        layout = QVBoxLayout(tab)

        # --- Scenario settings -----------------------------------------
        scenario_group = QGroupBox("Scenario")
        scenario_form = QFormLayout(scenario_group)
        self.sim_scenario_id_edit = QLineEdit("qgis_simulation_001")
        self.sim_seed_edit = QLineEdit("42")
        self.sim_duration_edit = QLineEdit("1800")
        self.sim_tick_edit = QLineEdit("1.0")
        self.sim_frame_interval_edit = QLineEdit("5.0")
        self.sim_model_combo = QComboBox()
        self.sim_model_combo.addItem("Greenshields (density linear)", "greenshields")
        self.sim_model_combo.addItem("BPR (volume-delay)", "bpr")
        self.sim_background_load_edit = QLineEdit("0.0")
        self.sim_position_model_combo = QComboBox()
        self.sim_position_model_combo.addItem("Linear", "linear")
        self.sim_position_model_combo.addItem("Trapezoidal (accel/decel)", "trapezoidal")
        scenario_form.addRow("Scenario id", self.sim_scenario_id_edit)
        scenario_form.addRow("Seed", self.sim_seed_edit)
        scenario_form.addRow("Duration s", self.sim_duration_edit)
        scenario_form.addRow("Tick s", self.sim_tick_edit)
        scenario_form.addRow("Frame interval s", self.sim_frame_interval_edit)
        scenario_form.addRow("Congestion model", self.sim_model_combo)
        scenario_form.addRow("Global background load 0-1", self.sim_background_load_edit)
        scenario_form.addRow("Playback position model", self.sim_position_model_combo)
        layout.addWidget(scenario_group)

        # --- Fleet builder ----------------------------------------------
        fleet_group = QGroupBox("Fleets (one travel mode each; mix freely)")
        fleet_layout = QVBoxLayout(fleet_group)
        fleet_form = QFormLayout()
        self.sim_fleet_id_edit = QLineEdit("cars")
        self.sim_profile_combo = QComboBox()
        self.sim_agent_count_edit = QLineEdit("500")
        self.sim_origin_source_combo = QComboBox()
        self.sim_dest_source_combo = QComboBox()
        for combo in [self.sim_origin_source_combo, self.sim_dest_source_combo]:
            combo.addItem("Random in dataset bounds", "dataset_bounds")
            combo.addItem("Random in canvas extent", "canvas_extent")
            combo.addItem("Random inside polygon layer", "polygon_layer")
            combo.addItem("Weighted points from point layer", "point_layer")
        self.sim_origin_layer_combo = QgsMapLayerComboBox()
        self.sim_dest_layer_combo = QgsMapLayerComboBox()
        self.sim_origin_source_combo.currentIndexChanged.connect(
            lambda _: self.sim_sync_layer_filter(
                self.sim_origin_source_combo, self.sim_origin_layer_combo
            )
        )
        self.sim_dest_source_combo.currentIndexChanged.connect(
            lambda _: self.sim_sync_layer_filter(
                self.sim_dest_source_combo, self.sim_dest_layer_combo
            )
        )
        self.sim_sync_layer_filter(self.sim_origin_source_combo, self.sim_origin_layer_combo)
        self.sim_sync_layer_filter(self.sim_dest_source_combo, self.sim_dest_layer_combo)
        self.sim_departure_combo = QComboBox()
        self.sim_departure_combo.addItem("Uniform between start/end", "uniform")
        self.sim_departure_combo.addItem("Peak (normal around mean)", "peak")
        self.sim_departure_combo.addItem("Instant at time", "instant")
        self.sim_departure_combo.addItem("Poisson arrivals", "poisson")
        self.sim_departure_a_edit = QLineEdit("0")
        self.sim_departure_b_edit = QLineEdit("600")
        departure_hint = QLabel(
            "uniform: start/end s - peak: mean/std s - instant: at s - poisson: start s / rate per s"
        )
        departure_hint.setWordWrap(True)
        fleet_form.addRow("Fleet id", self.sim_fleet_id_edit)
        fleet_form.addRow("Profile (travel mode)", self.sim_profile_combo)
        fleet_form.addRow("Agent count", self.sim_agent_count_edit)
        fleet_form.addRow("Origins", self.sim_origin_source_combo)
        fleet_form.addRow("Origins layer", self.sim_origin_layer_combo)
        fleet_form.addRow("Destinations", self.sim_dest_source_combo)
        fleet_form.addRow("Destinations layer", self.sim_dest_layer_combo)
        fleet_form.addRow("Departures", self.sim_departure_combo)
        fleet_form.addRow("Departure param A", self.sim_departure_a_edit)
        fleet_form.addRow("Departure param B", self.sim_departure_b_edit)
        fleet_form.addRow("", departure_hint)
        fleet_layout.addLayout(fleet_form)

        behavior_form = QFormLayout()
        self.sim_overtake_edit = QLineEdit("0.4")
        self.sim_reroute_edit = QLineEdit("0.3")
        self.sim_alternative_share_edit = QLineEdit("0.25")
        self.sim_speed_mult_edit = QLineEdit("1.0")
        self.sim_speed_std_edit = QLineEdit("0.08")
        self.sim_signals_check = QCheckBox("Obey traffic signals")
        self.sim_signals_check.setChecked(True)
        self.sim_no_collision_check = QCheckBox("No collision (queueing + spillback)")
        self.sim_no_collision_check.setChecked(True)
        behavior_form.addRow("Overtake eagerness 0-1", self.sim_overtake_edit)
        behavior_form.addRow("Reroute eagerness 0-1", self.sim_reroute_edit)
        behavior_form.addRow("Alternative route share 0-1", self.sim_alternative_share_edit)
        behavior_form.addRow("Speed multiplier mean", self.sim_speed_mult_edit)
        behavior_form.addRow("Speed multiplier std", self.sim_speed_std_edit)
        behavior_form.addRow("", self.sim_signals_check)
        behavior_form.addRow("", self.sim_no_collision_check)
        fleet_layout.addLayout(behavior_form)

        fleet_button_row = QHBoxLayout()
        add_fleet_button = QPushButton("Add Fleet")
        add_fleet_button.clicked.connect(self.sim_add_fleet)
        spawn_fleet_button = QPushButton("Spawn Into Running Sim")
        spawn_fleet_button.clicked.connect(self.sim_spawn_fleet_live)
        remove_fleet_button = QPushButton("Remove Selected")
        remove_fleet_button.clicked.connect(self.sim_remove_fleet)
        fleet_button_row.addWidget(add_fleet_button)
        fleet_button_row.addWidget(spawn_fleet_button)
        fleet_button_row.addStretch(1)
        fleet_button_row.addWidget(remove_fleet_button)
        fleet_layout.addLayout(fleet_button_row)
        self.sim_fleet_list = QListWidget()
        self.sim_fleet_list.setMaximumHeight(90)
        fleet_layout.addWidget(self.sim_fleet_list)
        layout.addWidget(fleet_group)

        # --- Zones --------------------------------------------------------
        zone_group = QGroupBox("Zones (no access / slow / high traffic)")
        zone_layout = QVBoxLayout(zone_group)
        zone_form = QFormLayout()
        self.sim_zone_layer_combo = QgsMapLayerComboBox()
        self.sim_zone_layer_combo.setFilters(LAYER_FILTER_POLYGON)
        self.sim_zone_effect_combo = QComboBox()
        self.sim_zone_effect_combo.addItem("No access (all modes)", "no_access_all")
        self.sim_zone_effect_combo.addItem("No access (car)", "no_access_car")
        self.sim_zone_effect_combo.addItem("No access (bicycle)", "no_access_bicycle")
        self.sim_zone_effect_combo.addItem("No access (foot)", "no_access_foot")
        self.sim_zone_effect_combo.addItem("No access (hgv)", "no_access_hgv")
        self.sim_zone_effect_combo.addItem("Speed factor", "speed_factor")
        self.sim_zone_effect_combo.addItem("Capacity factor", "capacity_factor")
        self.sim_zone_effect_combo.addItem("High traffic (background load)", "high_traffic")
        self.sim_zone_effect_combo.addItem("Spawn zone (origins)", "spawn")
        self.sim_zone_effect_combo.addItem("Attract zone (destinations)", "attract")
        self.sim_zone_value_edit = QLineEdit("0.5")
        zone_form.addRow("Polygon layer", self.sim_zone_layer_combo)
        zone_form.addRow("Effect", self.sim_zone_effect_combo)
        zone_form.addRow("Value (factor/load/weight)", self.sim_zone_value_edit)
        zone_layout.addLayout(zone_form)
        zone_button_row = QHBoxLayout()
        add_zone_button = QPushButton("Add Zones From Layer")
        add_zone_button.clicked.connect(self.sim_add_zones)
        zone_live_add_button = QPushButton("Add Selected To Running Sim")
        zone_live_add_button.clicked.connect(self.sim_send_zone_live)
        zone_live_remove_button = QPushButton("Remove Selected From Running Sim")
        zone_live_remove_button.clicked.connect(self.sim_remove_zone_live)
        remove_zone_button = QPushButton("Remove Selected")
        remove_zone_button.clicked.connect(self.sim_remove_zone)
        zone_button_row.addWidget(add_zone_button)
        zone_button_row.addWidget(zone_live_add_button)
        zone_button_row.addWidget(zone_live_remove_button)
        zone_button_row.addStretch(1)
        zone_button_row.addWidget(remove_zone_button)
        zone_layout.addLayout(zone_button_row)
        self.sim_zone_list = QListWidget()
        self.sim_zone_list.setMaximumHeight(80)
        zone_layout.addWidget(self.sim_zone_list)
        layout.addWidget(zone_group)

        # --- Run + control -------------------------------------------------
        run_group = QGroupBox("Run & live control")
        run_layout = QVBoxLayout(run_group)
        run_button_row = QHBoxLayout()
        export_button = QPushButton("Export Scenario")
        export_button.clicked.connect(self.sim_export_scenario)
        import_button = QPushButton("Import Scenario")
        import_button.clicked.connect(self.sim_import_scenario)
        run_button = QPushButton("Run Simulation")
        run_button.clicked.connect(self.sim_run)
        run_button_row.addWidget(export_button)
        run_button_row.addWidget(import_button)
        run_button_row.addStretch(1)
        run_button_row.addWidget(run_button)
        run_layout.addLayout(run_button_row)

        control_row = QHBoxLayout()
        pause_button = QPushButton("Pause")
        pause_button.clicked.connect(lambda: self.sim_control({"command": "pause"}))
        resume_button = QPushButton("Resume")
        resume_button.clicked.connect(lambda: self.sim_control({"command": "resume"}))
        cancel_button = QPushButton("Cancel")
        cancel_button.clicked.connect(lambda: self.sim_control({"command": "cancel"}))
        self.sim_speed_factor_edit = QLineEdit("1.0")
        self.sim_speed_factor_edit.setMaximumWidth(60)
        speed_factor_button = QPushButton("Apply Global Speed Factor")
        speed_factor_button.clicked.connect(self.sim_apply_speed_factor)
        control_row.addWidget(pause_button)
        control_row.addWidget(resume_button)
        control_row.addWidget(cancel_button)
        control_row.addStretch(1)
        control_row.addWidget(QLabel("Speed factor"))
        control_row.addWidget(self.sim_speed_factor_edit)
        control_row.addWidget(speed_factor_button)
        run_layout.addLayout(control_row)

        self.sim_status_label = QLabel("No simulation running.")
        self.sim_status_label.setWordWrap(True)
        run_layout.addWidget(self.sim_status_label)
        layout.addWidget(run_group)

        # --- Playback ------------------------------------------------------
        playback_group = QGroupBox("Playback & visualization")
        playback_layout = QVBoxLayout(playback_group)
        slider_row = QHBoxLayout()
        self.sim_play_button = QPushButton("Play")
        self.sim_play_button.clicked.connect(self.sim_toggle_play)
        self.sim_frame_slider = QSlider(qt_orientation("Horizontal"))
        self.sim_frame_slider.setMinimum(0)
        self.sim_frame_slider.setMaximum(0)
        self.sim_frame_slider.valueChanged.connect(self.sim_slider_moved)
        self.sim_time_label = QLabel("t = 0 s")
        self.sim_time_label.setMinimumWidth(90)
        slider_row.addWidget(self.sim_play_button)
        slider_row.addWidget(self.sim_frame_slider, stretch=1)
        slider_row.addWidget(self.sim_time_label)
        playback_layout.addLayout(slider_row)

        playback_options_row = QHBoxLayout()
        playback_options_row.addWidget(QLabel("Playback speed"))
        self.sim_playback_speed_combo = QComboBox()
        for label, value in [
            ("0.5x", 0.5),
            ("1x", 1.0),
            ("2x", 2.0),
            ("5x", 5.0),
            ("10x", 10.0),
            ("30x", 30.0),
        ]:
            self.sim_playback_speed_combo.addItem(label, value)
        self.sim_playback_speed_combo.setCurrentIndex(1)
        self.sim_playback_speed_combo.currentIndexChanged.connect(
            lambda _: self.sim_update_playback_interval()
        )
        playback_options_row.addWidget(self.sim_playback_speed_combo)
        playback_options_row.addWidget(QLabel("Max agents drawn"))
        self.sim_max_agents_edit = QLineEdit("5000")
        self.sim_max_agents_edit.setMaximumWidth(70)
        playback_options_row.addWidget(self.sim_max_agents_edit)
        playback_options_row.addStretch(1)
        playback_layout.addLayout(playback_options_row)

        viz_row = QHBoxLayout()
        congestion_button = QPushButton("Load Congestion (window)")
        congestion_button.clicked.connect(self.sim_load_congestion_window)
        busy_button = QPushButton("Load Busy Segments")
        busy_button.clicked.connect(self.sim_load_busy_segments)
        temporal_button = QPushButton("Load Temporal Layer")
        temporal_button.clicked.connect(self.sim_load_temporal_layer)
        viz_row.addWidget(congestion_button)
        viz_row.addWidget(busy_button)
        viz_row.addWidget(temporal_button)
        viz_row.addStretch(1)
        playback_layout.addLayout(viz_row)
        layout.addWidget(playback_group)

        layout.addStretch(1)
        return tab

    def sim_sync_layer_filter(self, source_combo, layer_combo):
        kind = source_combo.currentData()
        if kind == "polygon_layer":
            layer_combo.setFilters(LAYER_FILTER_POLYGON)
            layer_combo.setEnabled(True)
        elif kind == "point_layer":
            layer_combo.setFilters(LAYER_FILTER_POINT)
            layer_combo.setEnabled(True)
        else:
            layer_combo.setEnabled(False)

    def sim_float(self, edit, name, default=None):
        text = edit.text().strip()
        if not text and default is not None:
            return default
        try:
            return float(text)
        except ValueError:
            raise RuntimeError("Invalid number for {}: '{}'".format(name, text))

    def sim_transform_to_wgs84(self, geometry, source_crs):
        transform = QgsCoordinateTransform(
            source_crs, self.wgs84, QgsProject.instance()
        )
        copied = QgsGeometry(geometry)
        copied.transform(transform)
        return copied

    def sim_polygon_rings_from_layer(self, layer):
        if layer is None:
            raise RuntimeError("Select a polygon layer first.")
        features = list(layer.selectedFeatures()) or list(layer.getFeatures())
        rings = []
        for feature in features:
            geometry = feature.geometry()
            if geometry is None or geometry.isEmpty():
                continue
            geometry = self.sim_transform_to_wgs84(geometry, layer.crs())
            if geometry.isMultipart():
                polygons = geometry.asMultiPolygon()
            else:
                polygons = [geometry.asPolygon()]
            for polygon in polygons:
                if polygon and polygon[0]:
                    rings.append([[point.x(), point.y()] for point in polygon[0]])
        if not rings:
            raise RuntimeError(
                "No polygon rings found in layer '{}'.".format(layer.name())
            )
        return rings

    def sim_points_from_layer(self, layer):
        if layer is None:
            raise RuntimeError("Select a point layer first.")
        features = list(layer.selectedFeatures()) or list(layer.getFeatures())
        weight_index = layer.fields().indexOf("weight")
        points = []
        for feature in features:
            geometry = feature.geometry()
            if geometry is None or geometry.isEmpty():
                continue
            geometry = self.sim_transform_to_wgs84(geometry, layer.crs())
            point = geometry.asPoint()
            weight = 1.0
            if weight_index >= 0:
                try:
                    weight = max(0.0, float(feature.attributes()[weight_index]))
                except (TypeError, ValueError):
                    weight = 1.0
            points.append({"lon": point.x(), "lat": point.y(), "weight": weight})
        if not points:
            raise RuntimeError("No points found in layer '{}'.".format(layer.name()))
        return points

    def sim_canvas_extent_bbox(self):
        canvas = self.iface.mapCanvas()
        extent = canvas.extent()
        transform = QgsCoordinateTransform(
            canvas.mapSettings().destinationCrs(), self.wgs84, QgsProject.instance()
        )
        low = transform.transform(QgsPointXY(extent.xMinimum(), extent.yMinimum()))
        high = transform.transform(QgsPointXY(extent.xMaximum(), extent.yMaximum()))
        return [
            min(low.x(), high.x()),
            min(low.y(), high.y()),
            max(low.x(), high.x()),
            max(low.y(), high.y()),
        ]

    def sim_endpoint_distribution(self, source_combo, layer_combo, label):
        kind = source_combo.currentData()
        if kind == "dataset_bounds":
            return {"kind": "random_bounds"}
        if kind == "canvas_extent":
            return {"kind": "random_bbox", "bbox": self.sim_canvas_extent_bbox()}
        if kind == "polygon_layer":
            rings = self.sim_polygon_rings_from_layer(layer_combo.currentLayer())
            return {"kind": "random_polygon", "polygon": rings[0]}
        if kind == "point_layer":
            return {
                "kind": "points",
                "points": self.sim_points_from_layer(layer_combo.currentLayer()),
            }
        raise RuntimeError("Unknown {} source '{}'.".format(label, kind))

    def sim_departure_config(self):
        kind = self.sim_departure_combo.currentData()
        a = self.sim_float(self.sim_departure_a_edit, "departure param A", 0.0)
        b = self.sim_float(self.sim_departure_b_edit, "departure param B", 600.0)
        if kind == "uniform":
            return {"kind": "uniform", "start_s": a, "end_s": b}
        if kind == "peak":
            return {"kind": "peak", "mean_s": a, "std_s": b}
        if kind == "instant":
            return {"kind": "instant", "at_s": a}
        if kind == "poisson":
            return {"kind": "poisson", "start_s": a, "rate_per_s": max(b, 0.001)}
        raise RuntimeError("Unknown departure kind.")

    def sim_build_fleet(self):
        fleet_id = self.sim_fleet_id_edit.text().strip()
        if not fleet_id:
            raise RuntimeError("Fleet id must not be empty.")
        profile_id = self.sim_profile_combo.currentData()
        if not profile_id:
            raise RuntimeError("Refresh the API service and pick a profile.")
        agent_count = int(self.sim_float(self.sim_agent_count_edit, "agent count"))
        if agent_count <= 0:
            raise RuntimeError("Agent count must be positive.")
        return {
            "fleet_id": fleet_id,
            "profile_id": profile_id,
            "agent_count": agent_count,
            "demand": {
                "origins": self.sim_endpoint_distribution(
                    self.sim_origin_source_combo, self.sim_origin_layer_combo, "origin"
                ),
                "destinations": self.sim_endpoint_distribution(
                    self.sim_dest_source_combo, self.sim_dest_layer_combo, "destination"
                ),
            },
            "departures": self.sim_departure_config(),
            "behavior": {
                "overtake_eagerness": self.sim_float(self.sim_overtake_edit, "overtake", 0.4),
                "reroute_eagerness": self.sim_float(self.sim_reroute_edit, "reroute", 0.3),
                "alternative_route_share": self.sim_float(
                    self.sim_alternative_share_edit, "alternative share", 0.25
                ),
                "obey_traffic_signals": self.sim_signals_check.isChecked(),
                "no_collision": self.sim_no_collision_check.isChecked(),
            },
            "speed": {
                "speed_multiplier_mean": self.sim_float(
                    self.sim_speed_mult_edit, "speed multiplier mean", 1.0
                ),
                "speed_multiplier_std": self.sim_float(
                    self.sim_speed_std_edit, "speed multiplier std", 0.08
                ),
            },
        }

    def sim_add_fleet(self):
        try:
            fleet = self.sim_build_fleet()
        except Exception as exc:
            self.alert(str(exc))
            return
        self.sim_fleets = [
            existing for existing in self.sim_fleets
            if existing["fleet_id"] != fleet["fleet_id"]
        ]
        self.sim_fleets.append(fleet)
        self.sim_refresh_fleet_list()
        self.log(
            "Added fleet '{}' ({} agents, profile '{}').".format(
                fleet["fleet_id"], fleet["agent_count"], fleet["profile_id"]
            )
        )

    def sim_refresh_fleet_list(self):
        self.sim_fleet_list.clear()
        for fleet in self.sim_fleets:
            self.sim_fleet_list.addItem(
                "{} - {} agents - profile {} - departures {}".format(
                    fleet["fleet_id"],
                    fleet["agent_count"],
                    fleet["profile_id"],
                    fleet["departures"]["kind"],
                )
            )

    def sim_remove_fleet(self):
        row = self.sim_fleet_list.currentRow()
        if row < 0 or row >= len(self.sim_fleets):
            return
        removed = self.sim_fleets.pop(row)
        self.sim_refresh_fleet_list()
        self.log("Removed fleet '{}'.".format(removed["fleet_id"]))

    def sim_zone_effect(self):
        kind = self.sim_zone_effect_combo.currentData()
        value = self.sim_float(self.sim_zone_value_edit, "zone value", 0.5)
        if kind.startswith("no_access"):
            mode = kind.replace("no_access_", "")
            modes = [] if mode == "all" else [mode]
            return {"kind": "no_access", "modes": modes}
        if kind == "speed_factor":
            return {"kind": "speed_factor", "factor": value}
        if kind == "capacity_factor":
            return {"kind": "capacity_factor", "factor": value}
        if kind == "high_traffic":
            return {"kind": "high_traffic", "background_load": value}
        if kind == "spawn":
            return {"kind": "spawn", "weight": value}
        if kind == "attract":
            return {"kind": "attract", "weight": value}
        raise RuntimeError("Unknown zone effect.")

    def sim_add_zones(self):
        try:
            layer = self.sim_zone_layer_combo.currentLayer()
            rings = self.sim_polygon_rings_from_layer(layer)
            effect = self.sim_zone_effect()
        except Exception as exc:
            self.alert(str(exc))
            return
        base = layer.name().replace(" ", "_").lower()
        for index, ring in enumerate(rings):
            zone_id = "{}_{}_{}".format(base, effect["kind"], len(self.sim_zones) + index + 1)
            self.sim_zones.append(
                {
                    "zone_id": zone_id,
                    "label": "{} ({})".format(layer.name(), effect["kind"]),
                    "polygon": ring,
                    "effect": effect,
                }
            )
        self.sim_refresh_zone_list()
        self.log("Added {} zone(s) from layer '{}'.".format(len(rings), layer.name()))

    def sim_refresh_zone_list(self):
        self.sim_zone_list.clear()
        for zone in self.sim_zones:
            self.sim_zone_list.addItem(
                "{} - {}".format(zone["zone_id"], zone["effect"]["kind"])
            )

    def sim_remove_zone(self):
        row = self.sim_zone_list.currentRow()
        if row < 0 or row >= len(self.sim_zones):
            return
        removed = self.sim_zones.pop(row)
        self.sim_refresh_zone_list()
        self.log("Removed zone '{}'.".format(removed["zone_id"]))

    def sim_build_scenario(self):
        if not self.sim_fleets:
            raise RuntimeError("Add at least one fleet first.")
        scenario_id = self.sim_scenario_id_edit.text().strip() or "qgis_simulation"
        return {
            "scenario": {
                "id": scenario_id,
                "label": "QGIS scenario {}".format(scenario_id),
                "seed": int(self.sim_float(self.sim_seed_edit, "seed", 1.0)),
            },
            "time": {
                "duration_s": self.sim_float(self.sim_duration_edit, "duration", 1800.0),
                "tick_s": self.sim_float(self.sim_tick_edit, "tick", 1.0),
            },
            "fleets": self.sim_fleets,
            "zones": self.sim_zones,
            "traffic": {
                "model": self.sim_model_combo.currentData(),
                "background_load": self.sim_float(
                    self.sim_background_load_edit, "background load", 0.0
                ),
            },
            "output": {
                "frame_interval_s": self.sim_float(
                    self.sim_frame_interval_edit, "frame interval", 5.0
                ),
                "position_model": self.sim_position_model_combo.currentData(),
            },
        }

    def sim_export_scenario(self):
        try:
            scenario = self.sim_build_scenario()
        except Exception as exc:
            self.alert(str(exc))
            return
        path, _ = QFileDialog.getSaveFileName(
            self, "Export scenario", "simulation_scenario.json", "JSON (*.json)"
        )
        if not path:
            return
        Path(path).write_text(json.dumps(scenario, indent=2))
        self.log("Scenario exported to {}".format(path))

    def sim_import_scenario(self):
        path, _ = QFileDialog.getOpenFileName(
            self, "Import scenario", "", "JSON (*.json)"
        )
        if not path:
            return
        try:
            scenario = json.loads(Path(path).read_text())
            self.sim_fleets = scenario.get("fleets", [])
            self.sim_zones = scenario.get("zones", [])
            header = scenario.get("scenario", {})
            self.sim_scenario_id_edit.setText(header.get("id", "imported"))
            self.sim_seed_edit.setText(str(header.get("seed", 1)))
            time_config = scenario.get("time", {})
            self.sim_duration_edit.setText(str(time_config.get("duration_s", 1800)))
            self.sim_tick_edit.setText(str(time_config.get("tick_s", 1.0)))
            output = scenario.get("output", {})
            self.sim_frame_interval_edit.setText(str(output.get("frame_interval_s", 5.0)))
        except Exception as exc:
            self.alert("Failed to import scenario: {}".format(exc))
            return
        self.sim_refresh_fleet_list()
        self.sim_refresh_zone_list()
        self.log("Scenario imported from {}".format(path))

    def sim_run(self):
        if not self.ensure_service():
            return
        try:
            scenario = self.sim_build_scenario()
        except Exception as exc:
            self.alert(str(exc))
            return
        try:
            content_type, body = self.http_post_json(
                self.service_url("/v1/simulation"), {"scenario": scenario}
            )
            response = json.loads(body.decode("utf-8"))
        except Exception as exc:
            self.alert("Failed to start simulation: {}".format(exc))
            return
        self.sim_id = response.get("simulation_id")
        self.sim_frame_cache = {}
        self.sim_current_frame = 0
        self.sim_frame_slider.setMaximum(0)
        self.sim_status = response.get("status")
        self.log("Simulation '{}' started.".format(self.sim_id))
        self.sim_status_timer.start()

    def sim_control(self, command):
        if not self.sim_id:
            self.alert("No simulation is active.")
            return
        try:
            content_type, body = self.http_post_json(
                self.service_url("/v1/simulation/{}/control".format(self.sim_id)),
                command,
            )
            self.log("Command '{}' sent.".format(command.get("command")))
        except Exception as exc:
            self.alert("Control command failed: {}".format(exc))

    def sim_apply_speed_factor(self):
        try:
            factor = self.sim_float(self.sim_speed_factor_edit, "speed factor", 1.0)
        except Exception as exc:
            self.alert(str(exc))
            return
        self.sim_control({"command": "set_global_speed_factor", "factor": factor})

    def sim_spawn_fleet_live(self):
        if not self.sim_id:
            self.alert("Run a simulation first, then spawn extra fleets into it.")
            return
        try:
            fleet = self.sim_build_fleet()
        except Exception as exc:
            self.alert(str(exc))
            return
        self.sim_control({"command": "spawn_fleet", "fleet": fleet})

    def sim_selected_zone(self):
        row = self.sim_zone_list.currentRow()
        if row < 0 or row >= len(self.sim_zones):
            raise RuntimeError("Select a zone in the list first.")
        return self.sim_zones[row]

    def sim_send_zone_live(self):
        try:
            zone = self.sim_selected_zone()
        except Exception as exc:
            self.alert(str(exc))
            return
        self.sim_control({"command": "add_zone", "zone": zone})

    def sim_remove_zone_live(self):
        try:
            zone = self.sim_selected_zone()
        except Exception as exc:
            self.alert(str(exc))
            return
        self.sim_control({"command": "remove_zone", "zone_id": zone["zone_id"]})

    def sim_poll_status(self):
        if not self.sim_id:
            self.sim_status_timer.stop()
            return
        try:
            info = self.http_get_json(
                self.service_url("/v1/simulation/{}".format(self.sim_id))
            )
        except Exception as exc:
            self.sim_status_label.setText("Status poll failed: {}".format(exc))
            return
        status = info.get("status") or {}
        self.sim_status = status
        state = status.get("state", "unknown")
        fleets_text = ", ".join(
            "{}: {}/{} arrived".format(
                fleet.get("fleet_id"), fleet.get("arrived"), fleet.get("dispatched")
            )
            for fleet in status.get("fleets", [])
        )
        self.sim_status_label.setText(
            "{} | t={:.0f}/{:.0f}s | active {} | arrived {}/{} | frames {} | {}".format(
                state,
                status.get("sim_time_s", 0.0),
                status.get("duration_s", 0.0),
                status.get("agents_active", 0),
                status.get("agents_arrived", 0),
                status.get("agents_total", 0),
                status.get("frames_available", 0),
                fleets_text,
            )
        )
        frames_available = int(status.get("frames_available", 0))
        if frames_available > 0:
            self.sim_frame_slider.setMaximum(frames_available - 1)
        if state in ["completed", "cancelled", "failed"]:
            self.sim_status_timer.stop()
            if info.get("error"):
                self.alert("Simulation failed: {}".format(info["error"]))
            else:
                self.log("Simulation '{}' {}.".format(self.sim_id, state))

    def sim_frame_interval(self):
        if self.sim_status and self.sim_status.get("frame_interval_s"):
            return float(self.sim_status["frame_interval_s"])
        try:
            return self.sim_float(self.sim_frame_interval_edit, "frame interval", 5.0)
        except RuntimeError:
            return 5.0

    def sim_update_playback_interval(self):
        speed = self.sim_playback_speed_combo.currentData() or 1.0
        interval_ms = int(self.sim_frame_interval() * 1000.0 / speed)
        self.sim_playback_timer.setInterval(max(40, min(interval_ms, 5000)))

    def sim_toggle_play(self):
        if not self.sim_id:
            self.alert("Run a simulation first.")
            return
        if self.sim_playing:
            self.sim_playing = False
            self.sim_playback_timer.stop()
            self.sim_play_button.setText("Play")
        else:
            self.sim_playing = True
            self.sim_update_playback_interval()
            self.sim_playback_timer.start()
            self.sim_play_button.setText("Pause")

    def sim_playback_tick(self):
        maximum = self.sim_frame_slider.maximum()
        if self.sim_current_frame >= maximum:
            if self.sim_status and self.sim_status.get("state") in [
                "completed",
                "cancelled",
                "failed",
            ]:
                self.sim_toggle_play()
                return
            # Waiting for the running simulation to produce more frames.
            return
        self.sim_frame_slider.setValue(self.sim_current_frame + 1)

    def sim_slider_moved(self, value):
        self.sim_current_frame = value
        self.sim_show_frame(value)

    def sim_fetch_frames(self, start_index, count):
        interval = self.sim_frame_interval()
        start_s = start_index * interval - interval * 0.25
        end_s = (start_index + count) * interval + interval * 0.25
        try:
            max_agents = int(self.sim_float(self.sim_max_agents_edit, "max agents", 5000.0))
        except RuntimeError:
            max_agents = 5000
        url = self.service_url(
            "/v1/simulation/{}/frames".format(self.sim_id)
        ) + "?start_s={:.3f}&end_s={:.3f}&max_agents={}".format(
            max(0.0, start_s), end_s, max(1, max_agents)
        )
        response = self.http_get_json(url)
        for frame in response.get("frames", []):
            index = int(round(float(frame["t"]) / interval))
            self.sim_frame_cache[index] = frame

    def sim_show_frame(self, index):
        if not self.sim_id:
            return
        if index not in self.sim_frame_cache:
            try:
                self.sim_fetch_frames(index, 40)
            except Exception as exc:
                self.log("Frame fetch failed: {}".format(exc), MSG_WARNING)
                return
        frame = self.sim_frame_cache.get(index)
        if frame is None:
            return
        self.sim_time_label.setText("t = {:.0f} s".format(float(frame.get("t", 0.0))))
        self.sim_update_agent_layer(frame)

    def sim_agent_layer(self):
        if self.sim_agent_layer_id:
            layer = QgsProject.instance().mapLayer(self.sim_agent_layer_id)
            if layer is not None:
                return layer
        layer = QgsVectorLayer(
            "Point?crs=EPSG:4326"
            "&field=agent_id:integer&field=fleet:string"
            "&field=speed_kph:double&field=edge:integer",
            "simulation agents",
            "memory",
        )
        QgsProject.instance().addMapLayer(layer, False)
        QgsProject.instance().layerTreeRoot().insertLayer(0, layer)
        self.sim_agent_layer_id = layer.id()
        return layer

    def sim_style_agent_layer(self, layer, fleet_names):
        categories = []
        for index, fleet in enumerate(sorted(fleet_names)):
            symbol = QgsMarkerSymbol.createSimple(
                {
                    "name": "circle",
                    "color": self.SIM_FLEET_COLORS[index % len(self.SIM_FLEET_COLORS)],
                    "outline_color": "#ffffff",
                    "outline_width": "0.2",
                    "size": "1.8",
                }
            )
            categories.append(QgsRendererCategory(fleet, symbol, fleet))
        layer.setRenderer(QgsCategorizedSymbolRenderer("fleet", categories))

    def sim_update_agent_layer(self, frame):
        layer = self.sim_agent_layer()
        agents = frame.get("agents") or {}
        ids = agents.get("id") or []
        fleets = agents.get("fleet") or []
        lons = agents.get("lon") or []
        lats = agents.get("lat") or []
        speeds = agents.get("speed_mps") or []
        edges = agents.get("edge") or []
        provider = layer.dataProvider()
        provider.truncate()
        features = []
        for slot in range(len(ids)):
            feature = QgsFeature(layer.fields())
            feature.setGeometry(
                QgsGeometry.fromPointXY(QgsPointXY(lons[slot], lats[slot]))
            )
            speed = speeds[slot] if slot < len(speeds) else 0.0
            feature.setAttributes(
                [
                    int(ids[slot]),
                    str(fleets[slot]) if slot < len(fleets) else "",
                    round(float(speed) * 3.6, 1),
                    int(edges[slot]) if slot < len(edges) else 0,
                ]
            )
            features.append(feature)
        provider.addFeatures(features)
        fleet_names = set(str(name) for name in fleets)
        renderer = layer.renderer()
        existing = set()
        if isinstance(renderer, QgsCategorizedSymbolRenderer):
            existing = set(category.value() for category in renderer.categories())
        if fleet_names - existing:
            self.sim_style_agent_layer(layer, fleet_names | existing)
        layer.updateExtents()
        layer.triggerRepaint()

    def sim_congestion_ranges(self):
        def line(color, width):
            return QgsLineSymbol.createSimple({"color": color, "line_width": str(width)})

        return [
            QgsRendererRange(0.00, 0.10, line("#74c476", 0.5), "free flow"),
            QgsRendererRange(0.10, 0.30, line("#fdae61", 0.8), "slowing"),
            QgsRendererRange(0.30, 0.60, line("#f46d43", 1.1), "congested"),
            QgsRendererRange(0.60, 1.01, line("#d73027", 1.5), "jammed"),
        ]

    def sim_load_congestion_window(self):
        if not self.sim_id:
            self.alert("Run a simulation first.")
            return
        interval = self.sim_frame_interval()
        center = self.sim_current_frame * interval
        window = max(300.0, interval * 10)
        url = self.service_url(
            "/v1/simulation/{}/edges".format(self.sim_id)
        ) + "?start_s={:.0f}&end_s={:.0f}".format(max(0.0, center - window), center + window)
        try:
            request = urllib.request.Request(url, headers={"Accept": "application/geo+json"})
            with urllib.request.urlopen(request, timeout=self.timeout_seconds()) as response:
                body = response.read()
        except Exception as exc:
            self.alert("Failed to load congestion: {}".format(exc))
            return
        output_path = self.temp_layers_dir / "sim_congestion_{}.geojson".format(
            re.sub(r"[^A-Za-z0-9_-]", "_", self.sim_id)
        )
        output_path.write_bytes(body)
        layer = self.load_output_layer(output_path, "simulation congestion")
        if layer is not None:
            renderer = QgsGraduatedSymbolRenderer("congestion_level", self.sim_congestion_ranges())
            layer.setRenderer(renderer)
            layer.triggerRepaint()
            self.set_last_output_layers([layer])

    def sim_load_busy_segments(self):
        if not self.sim_id:
            self.alert("Run a simulation first.")
            return
        url = self.service_url(
            "/v1/simulation/{}/edges".format(self.sim_id)
        ) + "?mode=summary&min_traversals=1"
        try:
            request = urllib.request.Request(url, headers={"Accept": "application/geo+json"})
            with urllib.request.urlopen(request, timeout=self.timeout_seconds()) as response:
                body = response.read()
        except Exception as exc:
            self.alert(
                "Failed to load busy segments (only available after completion "
                "- use congestion window while running): {}".format(exc)
            )
            return
        output_path = self.temp_layers_dir / "sim_busy_{}.geojson".format(
            re.sub(r"[^A-Za-z0-9_-]", "_", self.sim_id)
        )
        output_path.write_bytes(body)
        layer = self.load_output_layer(output_path, "simulation busy segments")
        if layer is None:
            return
        values = [
            feature.attribute("traversals")
            for feature in layer.getFeatures()
            if feature.attribute("traversals") is not None
        ]
        maximum = max(values) if values else 1

        def line(color, width):
            return QgsLineSymbol.createSimple({"color": color, "line_width": str(width)})

        bounds = [0, maximum * 0.1, maximum * 0.3, maximum * 0.6, maximum + 1]
        colors = ["#c6dbef", "#6baed6", "#2171b5", "#d73027"]
        widths = [0.4, 0.8, 1.2, 1.8]
        ranges = [
            QgsRendererRange(
                bounds[i],
                bounds[i + 1],
                line(colors[i], widths[i]),
                "{}-{} traversals".format(int(bounds[i]), int(bounds[i + 1])),
            )
            for i in range(4)
        ]
        layer.setRenderer(QgsGraduatedSymbolRenderer("traversals", ranges))
        layer.triggerRepaint()
        self.set_last_output_layers([layer])

    def sim_load_temporal_layer(self):
        if not self.sim_id:
            self.alert("Run a simulation first.")
            return
        try:
            max_agents = int(self.sim_float(self.sim_max_agents_edit, "max agents", 2000.0))
        except RuntimeError:
            max_agents = 2000
        url = self.service_url(
            "/v1/simulation/{}/temporal".format(self.sim_id)
        ) + "?max_agents={}".format(min(max_agents, 2000))
        try:
            request = urllib.request.Request(url, headers={"Accept": "application/geo+json"})
            with urllib.request.urlopen(request, timeout=self.timeout_seconds()) as response:
                body = response.read()
        except Exception as exc:
            self.alert("Failed to load temporal frames: {}".format(exc))
            return
        output_path = self.temp_layers_dir / "sim_temporal_{}.geojson".format(
            re.sub(r"[^A-Za-z0-9_-]", "_", self.sim_id)
        )
        output_path.write_bytes(body)
        layer = self.load_output_layer(output_path, "simulation temporal agents")
        if layer is None:
            return
        self.set_last_output_layers([layer])
