"""Shared street-routing time, scenario, calendar, and overlay controls.

The API never reads files named in a request, so the scenario, holiday
calendar, and overlay files picked here are read locally and their content is
sent inline (``scenario``, ``holidays``, ``overlays``).
"""

import csv
import datetime
import json
import re
from pathlib import Path

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


_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")


def _read_structured(path, what):
    """Parse a local JSON or YAML file; YAML needs PyYAML (bundled with QGIS)."""
    suffix = path.suffix.lower()
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise ValueError("cannot read {} {}: {}".format(what, path, exc))
    if suffix == ".json":
        try:
            return json.loads(raw)
        except ValueError as exc:
            raise ValueError("{} {} is not valid JSON: {}".format(what, path, exc))
    if suffix in (".yml", ".yaml"):
        try:
            import yaml
        except ImportError:
            raise ValueError(
                "{} {} is YAML, but PyYAML is not available in this QGIS Python; "
                "install PyYAML or convert the file to JSON".format(what, path)
            )
        try:
            return yaml.safe_load(raw)
        except yaml.YAMLError as exc:
            raise ValueError("{} {} is not valid YAML: {}".format(what, path, exc))
    return None


def load_scenario_overlay(path):
    """Read a scenario overlay (JSON/YAML) into the inline ``scenario`` object."""
    path = Path(path)
    if path.suffix.lower() not in (".json", ".yml", ".yaml"):
        raise ValueError(
            "scenario overlay {} must be a .json, .yml, or .yaml file".format(path)
        )
    document = _read_structured(path, "scenario overlay")
    if not isinstance(document, dict):
        raise ValueError(
            "scenario overlay {} must be an object with a 'features' list".format(path)
        )
    return document


def load_holiday_calendar(path):
    """Read holiday dates from JSON/YAML (list or ``{dates: [...]}``) or text."""
    path = Path(path)
    suffix = path.suffix.lower()
    if suffix == ".txt":
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except OSError as exc:
            raise ValueError("cannot read holiday calendar {}: {}".format(path, exc))
        values = [
            line.strip()
            for line in lines
            if line.strip() and not line.strip().startswith("#")
        ]
    elif suffix in (".json", ".yml", ".yaml"):
        document = _read_structured(path, "holiday calendar")
        if isinstance(document, dict):
            document = document.get("dates", [])
        if not isinstance(document, list):
            raise ValueError(
                "holiday calendar {} must be a list of dates or {{dates: [...]}}".format(
                    path
                )
            )
        values = document
    else:
        raise ValueError(
            "holiday calendar {} must be a .json, .yml, .yaml, or .txt file".format(path)
        )
    dates = []
    for value in values:
        # Unquoted YAML dates parse as date objects.
        if isinstance(value, (datetime.date, datetime.datetime)):
            value = value.strftime("%Y-%m-%d")
        text = str(value).strip()
        try:
            if not _DATE_RE.match(text):
                raise ValueError(text)
            datetime.date(int(text[0:4]), int(text[5:7]), int(text[8:10]))
        except ValueError:
            raise ValueError(
                "holiday calendar {} has an invalid date '{}'; use YYYY-MM-DD".format(
                    path, text
                )
            )
        dates.append(text)
    return dates


def _take_column(row, names):
    for name in names:
        if name in row:
            return row.pop(name)
    raise ValueError("missing required column '{}'".format(names[0]))


def load_temporal_overlay(path):
    """Read a CSV temporal overlay into ``{"id": <stem>, "rows": [...]}``.

    Mirrors the CLI loader: ``feature_id`` (or ``source_feature_id``),
    ``t_start`` (or ``start``), ``t_end`` (or ``end``), and every other
    non-empty column as a named numeric value.
    """
    path = Path(path)
    suffix = path.suffix.lower()
    if suffix == ".parquet":
        raise ValueError(
            "temporal overlay {} is Parquet, which the plugin cannot read; export it "
            "to CSV or run the analysis with the netweevil CLI (--overlay)".format(path)
        )
    if suffix != ".csv":
        raise ValueError("temporal overlay {} must be a .csv file".format(path))
    rows = []
    try:
        with path.open(newline="", encoding="utf-8-sig") as handle:
            for index, record in enumerate(csv.DictReader(handle), start=1):
                row = {
                    key.strip(): (value or "").strip()
                    for key, value in record.items()
                    if key is not None
                }
                try:
                    raw_id = _take_column(row, ("feature_id", "source_feature_id"))
                    try:
                        feature_id = int(raw_id)
                    except ValueError:
                        raise ValueError("invalid feature_id '{}'".format(raw_id))
                    t_start = _take_column(row, ("t_start", "start"))
                    t_end = _take_column(row, ("t_end", "end"))
                    entry = {"feature_id": feature_id, "t_start": t_start, "t_end": t_end}
                    for name, value in row.items():
                        if not value or value.lower() == "null":
                            continue
                        try:
                            entry[name] = float(value)
                        except ValueError:
                            raise ValueError(
                                "value '{}' is not numeric: '{}'".format(name, value)
                            )
                except ValueError as exc:
                    raise ValueError(
                        "temporal overlay {} row {}: {}".format(path, index, exc)
                    )
                rows.append(entry)
    except (OSError, csv.Error) as exc:
        raise ValueError("cannot read temporal overlay {}: {}".format(path, exc))
    return {"id": path.stem, "rows": rows}


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
            "Optional local runtime scenario overlay (JSON or YAML), keyed by "
            "source feature id. The file is read here and sent inline; relative "
            "paths resolve from the workspace root."
        )

        holiday_edit = QLineEdit("")
        holiday_edit.setPlaceholderText("calendars/public-holidays.txt")
        holiday_edit.setToolTip(
            "Optional local holiday calendar used to resolve public-holiday "
            "temporal rules: JSON/YAML (a list or {dates: [...]}) or text with one "
            "YYYY-MM-DD date per line. The dates are sent inline."
        )

        overlays_edit = QPlainTextEdit()
        overlays_edit.setPlaceholderText(
            "overlays/shade-morning.csv\noverlays/heat-exposure.csv"
        )
        overlays_edit.setToolTip(
            "Optional local continuous temporal overlay CSV files, one path per "
            "line (feature_id, t_start, t_end, plus numeric value columns). Each "
            "file is read here and sent inline; later files take precedence. "
            "Use the CLI for Parquet overlays."
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
            "Supported files (*.json *.yml *.yaml *.txt);;All files (*)",
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
        add_button.setToolTip("Append one or more CSV temporal overlays.")
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
            "Temporal overlays (*.csv);;All files (*)",
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
            options["scenario"] = load_scenario_overlay(
                self.resolve_local_path(scenario)
            )
        if holiday_calendar:
            options["holidays"] = load_holiday_calendar(
                self.resolve_local_path(holiday_calendar)
            )
        if overlays:
            options["overlays"] = [
                load_temporal_overlay(self.resolve_local_path(path))
                for path in overlays
            ]
        return options
