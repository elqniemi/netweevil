"""Shared street-routing time, scenario, calendar, and overlay controls."""

from qgis.PyQt.QtWidgets import (
    QFileDialog,
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


class TemporalControlsMixin:
    """Build and serialize the flattened ``TemporalRequestOptions`` fields."""

    def _build_street_temporal_group(self, prefix):
        group = QGroupBox("Time, scenario, and continuous overlays")
        form = QFormLayout(group)

        departure_edit = QLineEdit("")
        departure_edit.setPlaceholderText("2026-07-10T09:59:00+08:00")
        departure_edit.setToolTip(
            "Optional RFC 3339 street departure datetime with an explicit UTC "
            "offset. Leave blank to use the static routing engine."
        )

        scenario_edit = QLineEdit("")
        scenario_edit.setPlaceholderText("scenarios/escalator-direction.yml")
        scenario_edit.setToolTip(
            "Optional runtime scenario overlay (JSON or YAML), keyed by source "
            "feature id. Relative paths are passed through unchanged."
        )

        holiday_edit = QLineEdit("")
        holiday_edit.setPlaceholderText("calendars/public-holidays.txt")
        holiday_edit.setToolTip(
            "Optional holiday calendar used to resolve public-holiday temporal rules."
        )

        overlays_edit = QPlainTextEdit()
        overlays_edit.setPlaceholderText(
            "overlays/shade-morning.csv\noverlays/heat-exposure.parquet"
        )
        overlays_edit.setToolTip(
            "Optional continuous temporal overlay files, one CSV or Parquet path "
            "per line. All paths are passed through as the request's overlay list."
        )
        overlays_edit.setMaximumHeight(82)

        setattr(self, "{}_departure_time_edit".format(prefix), departure_edit)
        setattr(self, "{}_scenario_edit".format(prefix), scenario_edit)
        setattr(self, "{}_holiday_calendar_edit".format(prefix), holiday_edit)
        setattr(self, "{}_overlays_edit".format(prefix), overlays_edit)

        note = QLabel(
            "Scenario and continuous overlay evaluation requires a departure "
            "datetime. Requests with no temporal values keep the accelerated "
            "static routing path."
        )
        note.setWordWrap(True)

        form.addRow("Departure datetime", departure_edit)
        form.addRow(
            "Scenario overlay",
            self._temporal_path_row(scenario_edit, "Select scenario overlay"),
        )
        form.addRow(
            "Holiday calendar",
            self._temporal_path_row(holiday_edit, "Select holiday calendar"),
        )
        form.addRow("Temporal overlays", self._temporal_overlay_row(overlays_edit))
        form.addRow("", note)
        return group

    def _temporal_path_row(self, line_edit, title):
        row = QWidget()
        layout = QHBoxLayout(row)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(line_edit)
        browse = QPushButton("Browse")
        browse.clicked.connect(lambda: self._browse_temporal_path(line_edit, title))
        layout.addWidget(browse)
        return row

    def _browse_temporal_path(self, line_edit, title):
        path, _selected_filter = QFileDialog.getOpenFileName(
            self,
            title,
            self.file_dialog_root(),
            "Supported files (*.json *.yml *.yaml *.txt *.csv *.parquet);;All files (*)",
        )
        if path:
            line_edit.setText(path)

    def _temporal_overlay_row(self, overlays_edit):
        row = QWidget()
        layout = QHBoxLayout(row)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(overlays_edit)
        buttons = QWidget()
        button_layout = QVBoxLayout(buttons)
        button_layout.setContentsMargins(0, 0, 0, 0)
        add_button = QPushButton("Add")
        add_button.setToolTip("Append one or more CSV/Parquet temporal overlays.")
        add_button.clicked.connect(lambda: self._add_temporal_overlays(overlays_edit))
        clear_button = QPushButton("Clear")
        clear_button.clicked.connect(overlays_edit.clear)
        button_layout.addWidget(add_button)
        button_layout.addWidget(clear_button)
        button_layout.addStretch(1)
        layout.addWidget(buttons)
        return row

    def _add_temporal_overlays(self, overlays_edit):
        paths, _selected_filter = QFileDialog.getOpenFileNames(
            self,
            "Select temporal overlays",
            self.file_dialog_root(),
            "Temporal overlays (*.csv *.parquet);;All files (*)",
        )
        if not paths:
            return
        current = self._temporal_overlay_paths(overlays_edit.toPlainText())
        for path in paths:
            if path not in current:
                current.append(path)
        overlays_edit.setPlainText("\n".join(current))

    def _temporal_overlay_paths(self, text):
        paths = []
        for raw_line in text.splitlines():
            path = raw_line.strip()
            if path and path not in paths:
                paths.append(path)
        return paths

    def build_street_temporal_options(self, prefix):
        departure_time = getattr(
            self, "{}_departure_time_edit".format(prefix)
        ).text().strip()
        scenario = getattr(self, "{}_scenario_edit".format(prefix)).text().strip()
        holiday_calendar = getattr(
            self, "{}_holiday_calendar_edit".format(prefix)
        ).text().strip()
        overlays = self._temporal_overlay_paths(
            getattr(self, "{}_overlays_edit".format(prefix)).toPlainText()
        )

        if (scenario or holiday_calendar or overlays) and not departure_time:
            raise ValueError(
                "set a departure datetime when using a scenario, holiday calendar, "
                "or temporal overlay"
            )

        options = {}
        if departure_time:
            options["departure_time"] = departure_time
        if scenario:
            options["scenario"] = scenario
        if holiday_calendar:
            options["holiday_calendar"] = holiday_calendar
        if overlays:
            options["overlay"] = overlays
        return options
