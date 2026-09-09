"""OD / Matrix tab: UI, CSV/JSON/layer inputs, and run handling."""

import csv

from qgis.PyQt.QtWidgets import (
    QCheckBox,
    QComboBox,
    QFormLayout,
    QGroupBox,
    QLabel,
    QLineEdit,
    QPushButton,
    QVBoxLayout,
    QWidget,
)
from qgis.core import QgsCoordinateTransform, QgsProject, QgsWkbTypes
from qgis.gui import QgsMapLayerComboBox

from .compat import GEOM_POINT, LAYER_FILTER_POINT
from .constants import MatrixSourceMode


class BatchTabMixin:
    def _build_batch_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        summary = QLabel(
            "Batch analyses: run origin-destination pairs from a CSV/JSON file, "
            "or build a full travel matrix from QGIS point layers or files."
        )
        summary.setWordWrap(True)
        layout.addWidget(summary)

        od_group = QGroupBox("OD (origin-destination pairs)")
        od_form = QFormLayout(od_group)
        self.od_pairs_path_edit = QLineEdit("examples/requests/od_pairs.csv")
        self.od_pairs_path_edit.setToolTip(
            "CSV with columns id, source_lon, source_lat, target_lon, target_lat "
            "or a JSON pairs document."
        )
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

        batch_advanced_widget = self._build_advanced_controls(
            "batch", include_failure_modes=True
        )
        self.batch_advanced_toggle = self._make_toggle_section(
            "Advanced Controls", batch_advanced_widget
        )
        layout.addWidget(self.batch_advanced_toggle)
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
        layer_combo.setFilters(LAYER_FILTER_POINT)
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

    def run_od(self):
        if not self.ensure_service():
            return
        try:
            document = self.load_od_document(
                self.resolve_local_path(self.od_pairs_path_edit.text())
            )
            document["connectivity"] = self.build_connectivity_policy("batch")
            document["fallback"] = self.build_fallback_policy("batch")
            document["alternatives"] = self.build_alternative_options("batch")
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
        if not self.ensure_service():
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
            alternatives = self.build_alternative_options("batch")
        except Exception as exc:
            self.alert("Failed to load matrix input: {}".format(exc))
            return
        origins["connectivity"] = connectivity
        origins["fallback"] = fallback
        origins["alternatives"] = alternatives
        destinations["connectivity"] = connectivity
        destinations["fallback"] = fallback
        destinations["alternatives"] = alternatives
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
        if QgsWkbTypes.geometryType(layer.wkbType()) != GEOM_POINT:
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
        raise ValueError("OD input must be a CSV or a JSON object containing pairs.")

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
        raise ValueError("Point-set input must be a CSV or a JSON object containing points.")

    def load_od_csv(self, path):
        with path.open("r", encoding="utf-8", newline="") as handle:
            reader = csv.DictReader(handle)
            pairs = []
            for row in reader:
                pair_id = self.csv_value(row, "id")
                source_lon = float(self.csv_value(row, "source_lon"))
                source_lat = float(self.csv_value(row, "source_lat"))
                target_lon = float(self.csv_value(row, "target_lon"))
                target_lat = float(self.csv_value(row, "target_lat"))
                pairs.append(
                    {
                        "pair_id": pair_id,
                        "origin": {
                            "id": "{}:source".format(pair_id),
                            "lon": source_lon,
                            "lat": source_lat,
                            "z": float(row["source_z"]) if (row.get("source_z") or "").strip() else None,
                        },
                        "destination": {
                            "id": "{}:target".format(pair_id),
                            "lon": target_lon,
                            "lat": target_lat,
                            "z": float(row["target_z"]) if (row.get("target_z") or "").strip() else None,
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
                        "id": self.csv_value(row, "id"),
                        "lon": float(self.csv_value(row, "lon")),
                        "lat": float(self.csv_value(row, "lat")),
                        "z": float(row["z"]) if (row.get("z") or "").strip() else None,
                    }
                )
        return {
            "points": points,
            "snap": {"max_distance_m": 500.0},
            "returns": {"geometry": "full"},
        }

    def csv_value(self, row, column):
        value = row.get(column)
        if value is not None and value.strip():
            return value.strip()
        raise ValueError("Missing required CSV value for column '{}'".format(column))
