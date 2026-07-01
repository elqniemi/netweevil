"""Connection bar, settings tab, service discovery, and HTTP client."""

import json
import shutil
import urllib.error
import urllib.request
from pathlib import Path

from qgis.PyQt.QtWidgets import (
    QComboBox,
    QFormLayout,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QVBoxLayout,
    QWidget,
)
from qgis.core import QgsCoordinateTransform, QgsPointXY, QgsProject

from .compat import MSG_INFO, MSG_WARNING
from .constants import ResponseFormat

# Service discovery should fail fast so an unreachable API never freezes the
# UI for the full analysis timeout.
SERVICE_PROBE_TIMEOUT_S = 4.0


class ConnectionMixin:
    def _build_connection_bar(self):
        bar = QWidget()
        layout = QVBoxLayout(bar)
        layout.setContentsMargins(0, 0, 0, 4)
        layout.setSpacing(2)

        url_row = QHBoxLayout()
        self.connection_status_dot = QLabel("●")
        self.connection_status_dot.setStyleSheet("color: #888888; font-size: 14px;")
        self.api_base_url_edit = QLineEdit("http://127.0.0.1:8080")
        self.api_base_url_edit.setPlaceholderText("http://127.0.0.1:8080")
        self.api_base_url_edit.setToolTip(
            "Base URL of a running 'netweevil api serve' instance."
        )
        connect_button = QPushButton("Connect")
        connect_button.setToolTip(
            "Check the API, then load its dataset, profiles, and transit feeds."
        )
        connect_button.clicked.connect(lambda: self.refresh_service(manual=True))
        self.api_base_url_edit.returnPressed.connect(
            lambda: self.refresh_service(manual=True)
        )
        url_row.addWidget(self.connection_status_dot)
        url_row.addWidget(QLabel("API"))
        url_row.addWidget(self.api_base_url_edit, stretch=1)
        url_row.addWidget(connect_button)
        layout.addLayout(url_row)

        profile_row = QHBoxLayout()
        profile_label = QLabel("Profile")
        self.profile_combo = QComboBox()
        self.profile_combo.setToolTip(
            "Routing profile used for road-network analyses. "
            "'Service default' uses whatever the API was started with."
        )
        self.profile_combo.addItem("Connect to load profiles", "")
        profile_row.addWidget(profile_label)
        profile_row.addWidget(self.profile_combo, stretch=1)
        layout.addLayout(profile_row)

        self.connection_status_label = QLabel(
            "Not connected. Start the API with 'netweevil api serve ...' and press Connect."
        )
        self.connection_status_label.setWordWrap(True)
        layout.addWidget(self.connection_status_label)
        return bar

    def _set_connection_status(self, connected, message):
        color = "#2b8a3e" if connected else "#c92a2a"
        self.connection_status_dot.setStyleSheet(
            "color: {}; font-size: 14px;".format(color)
        )
        self.connection_status_label.setText(message)

    def _build_settings_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        files_form = QFormLayout()
        self.workspace_root_edit = QLineEdit(str(Path.home()))
        self.workspace_root_edit.setToolTip(
            "Folder used to resolve relative request and response paths "
            "(for example '.netweevil/runs/...'). Point it at the folder where "
            "you run the netweevil API if you want outputs side by side with "
            "the workspace."
        )
        self.timeout_seconds_edit = QLineEdit("30")
        self.timeout_seconds_edit.setToolTip(
            "Maximum seconds to wait for an analysis response. "
            "Connection checks always use a short timeout."
        )

        self.response_format_combo = QComboBox()
        self.response_format_combo.addItem("JSON", ResponseFormat.JSON)
        self.response_format_combo.addItem("GeoJSON", ResponseFormat.GEOJSON)
        self.response_format_combo.setToolTip(
            "File format for saved OD, Matrix, and Service Area responses. "
            "Route and Transit always save JSON so detail tables stay available. "
            "Map layers load the same either way."
        )

        files_form.addRow(
            "Workspace root",
            self._line_with_browse(self.workspace_root_edit, browse_dir=True),
        )
        files_form.addRow("Timeout seconds", self.timeout_seconds_edit)
        files_form.addRow("Saved OD/Matrix/Area format", self.response_format_combo)
        layout.addLayout(files_form)

        service_form = QFormLayout()
        self.dataset_id_edit = QLineEdit()
        self.dataset_id_edit.setReadOnly(True)
        self.default_profile_edit = QLineEdit()
        self.default_profile_edit.setReadOnly(True)
        self.loaded_transit_feeds_edit = QLineEdit()
        self.loaded_transit_feeds_edit.setReadOnly(True)
        self.dataset_bounds_edit = QLineEdit()
        self.dataset_bounds_edit.setReadOnly(True)
        service_form.addRow("Dataset", self.dataset_id_edit)
        service_form.addRow("Service default profile", self.default_profile_edit)
        service_form.addRow("Loaded transit feeds", self.loaded_transit_feeds_edit)
        service_form.addRow("Dataset bounds", self.dataset_bounds_edit)
        layout.addLayout(service_form)

        button_row = QHBoxLayout()
        zoom_button = QPushButton("Zoom To Dataset")
        zoom_button.setToolTip("Zoom the map canvas to the loaded dataset bounds.")
        zoom_button.clicked.connect(self.zoom_to_dataset_bounds)
        button_row.addWidget(zoom_button)
        button_row.addStretch(1)
        layout.addLayout(button_row)

        description = QLabel(
            "The plugin talks directly to the running netweevil API; it never "
            "shells out to the CLI. All analysis settings are sent as explicit "
            "request fields and saved responses keep full provenance metadata."
        )
        description.setWordWrap(True)
        layout.addWidget(description)
        layout.addStretch(1)
        return tab

    def ensure_service(self):
        """Connect lazily so a freshly started API works without manual steps."""
        if self.service_info is None:
            self.refresh_service(manual=False)
        if self.service_info is None:
            self.alert(
                "Not connected to the netweevil API at {}. Start it with "
                "'netweevil api serve ...' and press Connect.".format(
                    self.api_base_url() or "the configured URL"
                )
            )
            return False
        return True

    def refresh_service(self, manual=False):
        try:
            service = self.http_get_json(
                self.service_url("/v1/service"),
                timeout=min(self.timeout_seconds(), SERVICE_PROBE_TIMEOUT_S),
            )
        except Exception as exc:
            self.service_info = None
            self.dataset_id_edit.setText("")
            self.default_profile_edit.setText("")
            self.loaded_transit_feeds_edit.setText("")
            self.dataset_bounds_edit.setText("")
            self.profile_combo.clear()
            self.profile_combo.addItem("Connect to load profiles", "")
            if hasattr(self, "transit_feed_combo"):
                self.transit_feed_combo.clear()
                self.transit_feed_combo.addItem("No transit feeds loaded", "")
            self._set_connection_status(
                False,
                "Not connected to {}. Start the API with 'netweevil api serve ...' "
                "and press Connect.".format(self.api_base_url() or "the configured URL"),
            )
            if manual:
                self.log("Failed to connect: {}".format(exc), MSG_WARNING)
            else:
                self.log(
                    "netweevil API not reachable yet: {}".format(exc), MSG_INFO
                )
            return

        self.service_info = service
        dataset = service.get("dataset", {})
        bounds = dataset.get("topology_bounds") or {}
        bounds_text = ""
        if bounds:
            bounds_text = "{min_lon:.6f}, {min_lat:.6f}, {max_lon:.6f}, {max_lat:.6f}".format(
                min_lon=bounds.get("min_lon", 0.0),
                min_lat=bounds.get("min_lat", 0.0),
                max_lon=bounds.get("max_lon", 0.0),
                max_lat=bounds.get("max_lat", 0.0),
            )

        previous_profile_id = self.selected_profile_id() or self.read_setting("profile_id", "")

        self.dataset_id_edit.setText(dataset.get("dataset_id", ""))
        self.default_profile_edit.setText(service.get("default_profile_id", ""))
        self.dataset_bounds_edit.setText(bounds_text)

        profiles = service.get("loaded_profiles", [])
        transit_feeds = service.get("loaded_transit_feeds", [])
        self.profile_combo.clear()
        default_profile_id = service.get("default_profile_id", "")
        self.profile_combo.addItem(
            "Service default ({})".format(default_profile_id or "none"), ""
        )
        for profile in profiles:
            label = "{} [{}]".format(
                profile.get("profile_id", "unknown"),
                profile.get("mode", "unknown"),
            )
            self.profile_combo.addItem(label, profile.get("profile_id", ""))

        desired_index = self.profile_combo.findData(previous_profile_id)
        if desired_index >= 0:
            self.profile_combo.setCurrentIndex(desired_index)

        if hasattr(self, "sim_profile_combo"):
            previous_sim_profile = self.sim_profile_combo.currentData()
            self.sim_profile_combo.clear()
            for profile in profiles:
                label = "{} [{}]".format(
                    profile.get("profile_id", "unknown"),
                    profile.get("mode", "unknown"),
                )
                self.sim_profile_combo.addItem(label, profile.get("profile_id", ""))
            sim_profile_index = self.sim_profile_combo.findData(previous_sim_profile)
            if sim_profile_index >= 0:
                self.sim_profile_combo.setCurrentIndex(sim_profile_index)

        previous_feed_id = self.read_setting("transit_feed_id", "")
        if hasattr(self, "transit_feed_combo"):
            self.transit_feed_combo.clear()
            if transit_feeds:
                for feed in transit_feeds:
                    label = "{} ({} stops, {} routes)".format(
                        feed.get("feed_id", "unknown"),
                        feed.get("stop_count", 0),
                        feed.get("route_count", 0),
                    )
                    self.transit_feed_combo.addItem(label, feed.get("feed_id", ""))
                desired_feed_index = self.transit_feed_combo.findData(previous_feed_id)
                if desired_feed_index >= 0:
                    self.transit_feed_combo.setCurrentIndex(desired_feed_index)
            else:
                self.transit_feed_combo.addItem("No transit feeds loaded", "")
        self.loaded_transit_feeds_edit.setText(
            ", ".join(feed.get("feed_id", "unknown") for feed in transit_feeds)
            if transit_feeds
            else "none"
        )

        self._set_connection_status(
            True,
            "Connected to {} — dataset '{}', {} profile(s), {} transit feed(s).".format(
                self.api_base_url(),
                dataset.get("dataset_id", "unknown"),
                len(profiles),
                len(transit_feeds),
            ),
        )
        self.log(
            "Connected to {}. Dataset '{}' with {} loaded profile(s).".format(
                self.api_base_url(), dataset.get("dataset_id", "unknown"), len(profiles)
            )
        )

    def zoom_to_dataset_bounds(self):
        if not self.ensure_service():
            return
        dataset = self.service_info.get("dataset", {})
        bounds = dataset.get("topology_bounds") or {}
        if not bounds:
            self.alert("The service did not report dataset bounds.")
            return

        try:
            transform = QgsCoordinateTransform(
                self.wgs84,
                self.iface.mapCanvas().mapSettings().destinationCrs(),
                QgsProject.instance(),
            )
            lower_left = transform.transform(
                QgsPointXY(bounds["min_lon"], bounds["min_lat"])
            )
            upper_right = transform.transform(
                QgsPointXY(bounds["max_lon"], bounds["max_lat"])
            )
        except Exception as exc:
            self.alert("Failed to transform dataset bounds: {}".format(exc))
            return

        extent = self.iface.mapCanvas().extent()
        extent.setXMinimum(min(lower_left.x(), upper_right.x()))
        extent.setYMinimum(min(lower_left.y(), upper_right.y()))
        extent.setXMaximum(max(lower_left.x(), upper_right.x()))
        extent.setYMaximum(max(lower_left.y(), upper_right.y()))
        self.iface.mapCanvas().setExtent(extent)
        self.iface.mapCanvas().refresh()

    def read_json(self, path):
        with path.open("r", encoding="utf-8") as handle:
            return json.load(handle)

    def http_get_json(self, url, timeout=None):
        request = urllib.request.Request(url, headers={"Accept": "application/json"})
        with urllib.request.urlopen(
            request, timeout=timeout or self.timeout_seconds()
        ) as response:
            body = response.read().decode("utf-8")
            return json.loads(body)

    def http_post_json(self, url, payload):
        body = json.dumps(payload).encode("utf-8")
        request = urllib.request.Request(
            url,
            data=body,
            headers={
                "Content-Type": "application/json",
                "Accept": "application/geo+json, application/json",
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout_seconds()) as response:
                return response.headers.get_content_type(), response.read()
        except urllib.error.HTTPError as exc:
            self.raise_http_error(exc)

    def http_post_json_to_file(self, url, payload, output_path):
        body = json.dumps(payload).encode("utf-8")
        request = urllib.request.Request(
            url,
            data=body,
            headers={
                "Content-Type": "application/json",
                "Accept": "application/geo+json, application/json",
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout_seconds()) as response:
                content_type = response.headers.get_content_type()
                saved_path = self.response_output_path(output_path, content_type)
                saved_path.parent.mkdir(parents=True, exist_ok=True)
                with saved_path.open("wb") as target:
                    shutil.copyfileobj(response, target, length=1024 * 1024)
                return content_type, saved_path
        except urllib.error.HTTPError as exc:
            self.raise_http_error(exc)

    def raise_http_error(self, exc):
        error_body = exc.read().decode("utf-8", errors="replace")
        try:
            parsed = json.loads(error_body)
            message = parsed.get("error") or error_body
            diagnostics = parsed.get("diagnostics") or []
            if diagnostics:
                detail_lines = []
                for diagnostic in diagnostics:
                    detail_lines.append(
                        "{}: {}".format(
                            diagnostic.get("code", "diagnostic"),
                            diagnostic.get("message", ""),
                        ).strip()
                    )
                    for action in diagnostic.get("suggested_actions") or []:
                        detail_lines.append("next: {}".format(action))
                message = "{}\n{}".format(message, "\n".join(detail_lines))
        except Exception:
            message = error_body or str(exc)
        raise RuntimeError(message)

    def response_output_path(self, output_path, content_type):
        output_path = Path(output_path)
        if "geo+json" in content_type:
            if output_path.suffix.lower() != ".geojson":
                return output_path.with_suffix(".geojson")
            return output_path

        if output_path.suffix.lower() != ".json":
            return output_path.with_suffix(".json")
        return output_path

    def save_response(self, output_path, content_type, body):
        output_path = self.response_output_path(output_path, content_type)
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_bytes(body)
        return output_path
