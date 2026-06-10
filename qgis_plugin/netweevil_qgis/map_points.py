"""Map canvas point picking, markers, and selected-feature helpers."""

from qgis.PyQt.QtGui import QColor
from qgis.core import (
    QgsCoordinateTransform,
    QgsFeatureRequest,
    QgsPointXY,
    QgsProject,
    QgsWkbTypes,
)
from qgis.gui import QgsMapToolEmitPoint, QgsVertexMarker

from .compat import vertex_marker_icon, GEOM_POINT
from .constants import PickTarget


class MapPointsMixin:
    def pick_status_label_for(self, target):
        if target in [PickTarget.ORIGIN, PickTarget.DESTINATION]:
            return self.pick_status_label
        if target in [PickTarget.TRANSIT_ORIGIN, PickTarget.TRANSIT_DESTINATION]:
            return self.transit_pick_status_label
        return self.service_area_pick_status_label

    def begin_point_pick(self, target):
        self.ensure_point_picker_tool()
        if (
            self.pick_target == target
            and self.iface.mapCanvas().mapTool() == self.point_picker_tool
        ):
            self.finish_point_pick()
            self.pick_status_label_for(target).setText("Cancelled point picking.")
            return
        self.pick_target = target
        current_tool = self.iface.mapCanvas().mapTool()
        if current_tool != self.point_picker_tool:
            self.previous_map_tool = current_tool
        self.iface.mapCanvas().setMapTool(self.point_picker_tool)
        if target == PickTarget.ORIGIN:
            self.pick_status_label.setText(
                "Click the start point on the map. Press Pick On Map again to cancel."
            )
        elif target == PickTarget.DESTINATION:
            self.pick_status_label.setText(
                "Click the end point on the map. Press Pick On Map again to cancel."
            )
        elif target == PickTarget.TRANSIT_ORIGIN:
            self.transit_pick_status_label.setText(
                "Click the transit origin on the map. Press Pick On Map again to cancel."
            )
        elif target == PickTarget.TRANSIT_DESTINATION:
            self.transit_pick_status_label.setText(
                "Click the transit destination on the map. Press Pick On Map again to cancel."
            )
        else:
            self.service_area_pick_status_label.setText(
                "Click one service-area origin on the map. Press Pick On Map again to cancel."
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
        if QgsWkbTypes.geometryType(layer.wkbType()) != GEOM_POINT:
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
            marker.setIconType(vertex_marker_icon("ICON_CROSS"))
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
