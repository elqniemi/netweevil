"""Qt 5/6 and QGIS 3/4 compatibility helpers.

QGIS 4 ships on Qt 6 with scoped enums and removes several deprecated QGIS 3
enum aliases. Everything version-dependent is resolved here once so the rest
of the plugin can stay version-agnostic.
"""

from qgis.PyQt.QtWidgets import QDockWidget, QMessageBox
from qgis.core import Qgis, QgsMapLayerProxyModel, QgsWkbTypes

try:  # Qt 6 moved QAction to QtGui; the qgis.PyQt shim usually re-exports it.
    from qgis.PyQt.QtGui import QAction
except ImportError:  # Qt 5
    from qgis.PyQt.QtWidgets import QAction

__all__ = [
    "QAction",
    "qt_enum_value",
    "qt_dock_area",
    "qt_widget_attribute",
    "qt_orientation",
    "dock_widget_feature",
    "message_box_button",
    "vertex_marker_icon",
    "vector_temporal_mode_instant",
    "MSG_INFO",
    "MSG_WARNING",
    "MSG_CRITICAL",
    "GEOM_POINT",
    "GEOM_LINE",
    "LAYER_FILTER_POINT",
    "LAYER_FILTER_POLYGON",
]


def qt_enum_value(owner, scoped_enum_name, member_name):
    """Return an enum member from either its Qt5 unscoped or Qt6 scoped home."""
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
    from qgis.PyQt.QtCore import Qt

    return qt_enum_value(Qt, "DockWidgetArea", member_name)


def qt_widget_attribute(member_name):
    from qgis.PyQt.QtCore import Qt

    return qt_enum_value(Qt, "WidgetAttribute", member_name)


def qt_orientation(member_name):
    from qgis.PyQt.QtCore import Qt

    return qt_enum_value(Qt, "Orientation", member_name)


def dock_widget_feature(member_name):
    return qt_enum_value(QDockWidget, "DockWidgetFeature", member_name)


def message_box_button(member_name):
    return qt_enum_value(QMessageBox, "StandardButton", member_name)


def vertex_marker_icon(member_name):
    from qgis.gui import QgsVertexMarker

    return qt_enum_value(QgsVertexMarker, "IconType", member_name)


def vector_temporal_mode_instant():
    """Temporal mode 'feature instant from field' across QGIS versions."""
    temporal_enum = getattr(Qgis, "VectorTemporalMode", None)
    if temporal_enum is not None and hasattr(
        temporal_enum, "FeatureDateTimeInstantFromField"
    ):
        return temporal_enum.FeatureDateTimeInstantFromField
    from qgis.core import QgsVectorLayerTemporalProperties

    return QgsVectorLayerTemporalProperties.ModeFeatureDateTimeInstantFromField


def _message_level(member_name):
    return qt_enum_value(Qgis, "MessageLevel", member_name)


def _geometry_type(member_name):
    geometry_enum = getattr(Qgis, "GeometryType", None)
    if geometry_enum is not None and hasattr(geometry_enum, member_name):
        return getattr(geometry_enum, member_name)
    return getattr(QgsWkbTypes, "{}Geometry".format(member_name))


def _layer_filter(member_name):
    filter_enum = getattr(Qgis, "LayerFilter", None)
    if filter_enum is not None and hasattr(filter_enum, member_name):
        return getattr(filter_enum, member_name)
    return getattr(QgsMapLayerProxyModel, member_name)


MSG_INFO = _message_level("Info")
MSG_WARNING = _message_level("Warning")
MSG_CRITICAL = _message_level("Critical")

GEOM_POINT = _geometry_type("Point")
GEOM_LINE = _geometry_type("Line")

LAYER_FILTER_POINT = _layer_filter("PointLayer")
LAYER_FILTER_POLYGON = _layer_filter("PolygonLayer")
