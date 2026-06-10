"""Shared plugin constants and small value enums."""

from pathlib import Path

PLUGIN_MENU = "&netweevil"
SETTINGS_PREFIX = "netweevil_qgis"
PLUGIN_DIR = Path(__file__).resolve().parent
PLUGIN_ICON = PLUGIN_DIR / "logo.svg"


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
