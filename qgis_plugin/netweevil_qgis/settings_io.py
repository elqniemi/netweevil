"""QSettings persistence for every dock control."""

from qgis.PyQt.QtCore import QSettings
from qgis.core import QgsProject

from .constants import SETTINGS_PREFIX, ResponseFormat, MatrixSourceMode


class SettingsIoMixin:
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
            "route_departure_time": self.route_departure_time_edit.text().strip(),
            "route_scenario": self.route_scenario_edit.text().strip(),
            "route_holiday_calendar": self.route_holiday_calendar_edit.text().strip(),
            "route_overlays": self.route_overlays_edit.toPlainText().strip(),
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
            "transit_access_mode": self.transit_access_mode_combo.currentData(),
            "transit_separate_egress": self.transit_separate_egress_check.isChecked(),
            "transit_egress_mode": self.transit_egress_mode_combo.currentData(),
            "transit_walk_speed": self.transit_walk_speed_edit.text().strip(),
            "transit_bicycle_speed": self.transit_bicycle_speed_edit.text().strip(),
            "transit_car_speed": self.transit_car_speed_edit.text().strip(),
            "transit_max_access_distance": self.transit_max_access_distance_edit.text().strip(),
            "transit_max_egress_distance": self.transit_max_egress_distance_edit.text().strip(),
            "transit_max_transfer_distance": self.transit_max_transfer_distance_edit.text().strip(),
            "transit_board_slack": self.transit_board_slack_edit.text().strip(),
            "transit_transfer_slack": self.transit_transfer_slack_edit.text().strip(),
            "transit_max_transfers": self.transit_max_transfers_edit.text().strip(),
            "transit_min_leg_duration": self.transit_min_leg_duration_edit.text().strip(),
            "transit_min_leg_distance": self.transit_min_leg_distance_edit.text().strip(),
            "transit_alternative_count": self.transit_alternative_count_edit.text().strip(),
            "transit_alternative_time_ratio": self.transit_alternative_time_ratio_edit.text().strip(),
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
            "service_area_mode": self.service_area_mode_combo.currentData(),
            "service_area_transit_datetime": self.service_area_transit_datetime_edit.text().strip(),
            "service_area_transit_max_time": self.service_area_transit_max_time_edit.text().strip(),
            "service_area_access_mode": self.service_area_access_mode_combo.currentData(),
            "service_area_access_distance": self.service_area_access_distance_edit.text().strip(),
            "service_area_access_speed": self.service_area_access_speed_edit.text().strip(),
            "service_area_snap_distance": self.service_area_snap_distance_edit.text().strip(),
            "service_area_output_path": self.service_area_output_path_edit.text().strip(),
            "service_area_request_path": self.service_area_request_path_edit.text().strip(),
            "service_area_output_mode": self.service_area_output_mode_combo.currentData(),
            "service_area_band_mode": self.service_area_band_mode_combo.currentData(),
            "service_area_segments": self.service_area_segments_check.isChecked(),
            "service_area_boundary_mode": self.service_area_boundary_mode_combo.currentData(),
            "service_area_multi_origin_mode": self.service_area_multi_origin_mode_combo.currentData(),
            "service_area_thresholds": self.service_area_thresholds_edit.text().strip(),
            "service_area_threshold_metric": self.service_area_threshold_metric_combo.currentData(),
            "service_area_hull_preset": self.service_area_hull_preset_combo.currentData(),
            "service_area_hull_aggressiveness": self.service_area_hull_aggressiveness_edit.text().strip(),
            "service_area_simplification": self.service_area_simplification_edit.text().strip(),
            "service_area_origins": self.service_area_origins_edit.toPlainText().strip(),
            "service_area_departure_time": self.service_area_departure_time_edit.text().strip(),
            "service_area_scenario": self.service_area_scenario_edit.text().strip(),
            "service_area_holiday_calendar": self.service_area_holiday_calendar_edit.text().strip(),
            "service_area_overlays": self.service_area_overlays_edit.toPlainText().strip(),
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
        self.route_departure_time_edit.setText(
            self.read_setting("route_departure_time", "")
        )
        self.route_scenario_edit.setText(self.read_setting("route_scenario", ""))
        self.route_holiday_calendar_edit.setText(
            self.read_setting("route_holiday_calendar", "")
        )
        self.route_overlays_edit.setPlainText(self.read_setting("route_overlays", ""))
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
        self.set_combo_by_data(
            self.transit_access_mode_combo,
            self.read_setting("transit_access_mode", "walk"),
        )
        self.transit_separate_egress_check.setChecked(
            self.read_bool_setting("transit_separate_egress", False)
        )
        self.set_combo_by_data(
            self.transit_egress_mode_combo,
            self.read_setting("transit_egress_mode", "walk"),
        )
        self.update_transit_egress_mode_state()
        self.transit_walk_speed_edit.setText(
            self.read_setting("transit_walk_speed", self.transit_walk_speed_edit.text())
        )
        self.transit_bicycle_speed_edit.setText(
            self.read_setting(
                "transit_bicycle_speed", self.transit_bicycle_speed_edit.text()
            )
        )
        self.transit_car_speed_edit.setText(
            self.read_setting("transit_car_speed", self.transit_car_speed_edit.text())
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
        self.transit_min_leg_duration_edit.setText(
            self.read_setting(
                "transit_min_leg_duration",
                self.transit_min_leg_duration_edit.text(),
            )
        )
        self.transit_min_leg_distance_edit.setText(
            self.read_setting(
                "transit_min_leg_distance",
                self.transit_min_leg_distance_edit.text(),
            )
        )
        self.transit_alternative_count_edit.setText(
            self.read_setting("transit_alternative_count", self.transit_alternative_count_edit.text())
        )
        self.transit_alternative_time_ratio_edit.setText(
            self.read_setting(
                "transit_alternative_time_ratio",
                self.transit_alternative_time_ratio_edit.text(),
            )
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
        self.service_area_transit_datetime_edit.setText(
            self.read_setting(
                "service_area_transit_datetime",
                self.service_area_transit_datetime_edit.text(),
            )
        )
        self.service_area_transit_max_time_edit.setText(
            self.read_setting(
                "service_area_transit_max_time",
                self.service_area_transit_max_time_edit.text(),
            )
        )
        self.set_combo_by_data(
            self.service_area_access_mode_combo,
            self.read_setting("service_area_access_mode", "walk"),
        )
        self.service_area_access_distance_edit.setText(
            self.read_setting(
                "service_area_access_distance",
                self.service_area_access_distance_edit.text(),
            )
        )
        self.service_area_access_speed_edit.setText(
            self.read_setting(
                "service_area_access_speed",
                self.service_area_access_speed_edit.text(),
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
        self.service_area_departure_time_edit.setText(
            self.read_setting("service_area_departure_time", "")
        )
        self.service_area_scenario_edit.setText(
            self.read_setting("service_area_scenario", "")
        )
        self.service_area_holiday_calendar_edit.setText(
            self.read_setting("service_area_holiday_calendar", "")
        )
        self.service_area_overlays_edit.setPlainText(
            self.read_setting("service_area_overlays", "")
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
            self.service_area_mode_combo,
            self.read_setting("service_area_mode", "road"),
        )
        self.service_area_segments_check.setChecked(
            self.read_bool_setting("service_area_segments", False)
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
        self.update_service_area_mode_visibility()
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
