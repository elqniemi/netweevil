import json
import sys
from pathlib import Path, PurePosixPath

from qgis.PyQt.QtCore import QProcess, Qt
from qgis.PyQt.QtWidgets import (
    QAction,
    QComboBox,
    QDockWidget,
    QFileDialog,
    QFormLayout,
    QGridLayout,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPushButton,
    QPlainTextEdit,
    QTabWidget,
    QVBoxLayout,
    QWidget,
)
from qgis.core import Qgis, QgsMessageLog, QgsProject, QgsVectorLayer


PLUGIN_MENU = "&netan"


class ExecutionMode:
    NATIVE = "Native"
    WSL = "WSL"


class NetanPlugin:
    def __init__(self, iface):
        self.iface = iface
        self.action = None
        self.dock = None

    def initGui(self):
        self.action = QAction("netan", self.iface.mainWindow())
        self.action.triggered.connect(self.toggle_dock)
        self.iface.addPluginToMenu(PLUGIN_MENU, self.action)
        self.iface.addToolBarIcon(self.action)

    def unload(self):
        if self.action is not None:
            self.iface.removePluginMenu(PLUGIN_MENU, self.action)
            self.iface.removeToolBarIcon(self.action)
        if self.dock is not None:
            self.iface.removeDockWidget(self.dock)
            self.dock.deleteLater()
            self.dock = None

    def toggle_dock(self):
        if self.dock is None:
            self.dock = NetanDock(self.iface)
            self.iface.addDockWidget(Qt.RightDockWidgetArea, self.dock)
        self.dock.show()
        self.dock.raise_()


class NetanDock(QDockWidget):
    def __init__(self, iface):
        super().__init__("netan", iface.mainWindow())
        self.iface = iface
        self.process = None
        self.pending_output_qgis_path = None
        self.setObjectName("netanDock")
        self.setWidget(self._build_ui())
        self._apply_mode_defaults()
        self.refresh_workspace()

    def _build_ui(self):
        container = QWidget()
        layout = QVBoxLayout(container)

        tabs = QTabWidget()
        tabs.addTab(self._build_workspace_tab(), "Workspace")
        tabs.addTab(self._build_route_tab(), "Route")
        tabs.addTab(self._build_batch_tab(), "Batch")
        layout.addWidget(tabs)

        self.log_output = QPlainTextEdit()
        self.log_output.setReadOnly(True)
        self.log_output.setPlaceholderText("Process and plugin log output.")
        layout.addWidget(self.log_output)

        return container

    def _build_workspace_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        form = QFormLayout()
        self.execution_mode_combo = QComboBox()
        self.execution_mode_combo.addItems([ExecutionMode.NATIVE, ExecutionMode.WSL])
        self.execution_mode_combo.currentTextChanged.connect(self._on_mode_changed)

        default_root = str(Path(__file__).resolve().parents[2])
        self.qgis_workspace_root_edit = QLineEdit(default_root)
        self.native_netan_executable_edit = QLineEdit(
            str(Path(__file__).resolve().parents[2] / "target" / "debug" / "netan")
        )
        self.wsl_distro_edit = QLineEdit("Ubuntu")
        self.wsl_workspace_root_edit = QLineEdit("/home/elmeriniemi/stuff/netan")
        self.wsl_netan_executable_edit = QLineEdit(
            "/home/elmeriniemi/stuff/netan/target/debug/netan"
        )
        self.dataset_combo = QComboBox()
        self.profile_path_edit = QLineEdit("examples/profiles/car_research_v1.yml")

        form.addRow("Execution mode", self.execution_mode_combo)
        form.addRow(
            "QGIS-visible workspace root",
            self._line_with_browse(self.qgis_workspace_root_edit, browse_dir=True),
        )
        form.addRow(
            "Native netan executable",
            self._line_with_browse(self.native_netan_executable_edit, browse_dir=False),
        )
        form.addRow("WSL distro", self.wsl_distro_edit)
        form.addRow("WSL workspace root", self.wsl_workspace_root_edit)
        form.addRow("WSL netan executable", self.wsl_netan_executable_edit)
        form.addRow("Dataset", self.dataset_combo)
        form.addRow(
            "Profile",
            self._line_with_browse(self.profile_path_edit, browse_dir=False),
        )
        layout.addLayout(form)

        button_row = QHBoxLayout()
        refresh_button = QPushButton("Refresh")
        refresh_button.clicked.connect(self.refresh_workspace)
        validate_button = QPushButton("Validate Profile")
        validate_button.clicked.connect(self.validate_profile)
        compile_button = QPushButton("Compile Profile")
        compile_button.clicked.connect(self.compile_profile)
        button_row.addWidget(refresh_button)
        button_row.addWidget(validate_button)
        button_row.addWidget(compile_button)
        layout.addLayout(button_row)

        layout.addWidget(
            QLabel(
                "In WSL mode, QGIS reads files through the Windows-visible workspace root, "
                "while the CLI runs inside WSL with translated Linux paths."
            )
        )
        layout.addStretch(1)
        return tab

    def _build_route_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        request_group = QGroupBox("Route request")
        request_layout = QFormLayout(request_group)
        self.route_request_path_edit = QLineEdit("examples/requests/route_from_qgis.json")
        self.route_output_path_edit = QLineEdit(".netan/runs/qgis-route.geojson")
        request_layout.addRow(
            "Request path",
            self._line_with_browse(self.route_request_path_edit, browse_dir=False),
        )
        request_layout.addRow(
            "Output path",
            self._line_with_browse(
                self.route_output_path_edit, browse_dir=False, save_dialog=True
            ),
        )

        self.route_id_edit = QLineEdit("qgis_route_001")
        self.snap_distance_edit = QLineEdit("500")
        request_layout.addRow("Route id", self.route_id_edit)
        request_layout.addRow("Snap distance m", self.snap_distance_edit)

        grid = QGridLayout()
        grid.addWidget(QLabel("Origin id"), 0, 0)
        self.origin_id_edit = QLineEdit("origin_a")
        grid.addWidget(self.origin_id_edit, 0, 1)
        grid.addWidget(QLabel("Origin lon"), 1, 0)
        self.origin_lon_edit = QLineEdit("6.5665")
        grid.addWidget(self.origin_lon_edit, 1, 1)
        grid.addWidget(QLabel("Origin lat"), 2, 0)
        self.origin_lat_edit = QLineEdit("53.2194")
        grid.addWidget(self.origin_lat_edit, 2, 1)

        grid.addWidget(QLabel("Destination id"), 0, 2)
        self.destination_id_edit = QLineEdit("destination_b")
        grid.addWidget(self.destination_id_edit, 0, 3)
        grid.addWidget(QLabel("Destination lon"), 1, 2)
        self.destination_lon_edit = QLineEdit("6.5716")
        grid.addWidget(self.destination_lon_edit, 1, 3)
        grid.addWidget(QLabel("Destination lat"), 2, 2)
        self.destination_lat_edit = QLineEdit("53.2148")
        grid.addWidget(self.destination_lat_edit, 2, 3)
        request_layout.addRow(grid)

        button_row = QHBoxLayout()
        write_request_button = QPushButton("Write Request")
        write_request_button.clicked.connect(self.write_route_request)
        run_route_button = QPushButton("Run Route")
        run_route_button.clicked.connect(self.run_route)
        button_row.addWidget(write_request_button)
        button_row.addWidget(run_route_button)
        request_layout.addRow(button_row)

        layout.addWidget(request_group)
        layout.addStretch(1)
        return tab

    def _build_batch_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)

        od_group = QGroupBox("OD")
        od_form = QFormLayout(od_group)
        self.od_pairs_path_edit = QLineEdit("examples/requests/od_pairs.csv")
        self.od_output_path_edit = QLineEdit(".netan/runs/qgis-od.gpkg")
        od_form.addRow(
            "Pairs path",
            self._line_with_browse(self.od_pairs_path_edit, browse_dir=False),
        )
        od_form.addRow(
            "Output path",
            self._line_with_browse(self.od_output_path_edit, browse_dir=False, save_dialog=True),
        )
        run_od_button = QPushButton("Run OD")
        run_od_button.clicked.connect(self.run_od)
        od_form.addRow(run_od_button)

        matrix_group = QGroupBox("Matrix")
        matrix_form = QFormLayout(matrix_group)
        self.matrix_origins_path_edit = QLineEdit("examples/requests/matrix_origins.csv")
        self.matrix_destinations_path_edit = QLineEdit("examples/requests/matrix_destinations.csv")
        self.matrix_output_path_edit = QLineEdit(".netan/runs/qgis-matrix.gpkg")
        matrix_form.addRow(
            "Origins path",
            self._line_with_browse(self.matrix_origins_path_edit, browse_dir=False),
        )
        matrix_form.addRow(
            "Destinations path",
            self._line_with_browse(self.matrix_destinations_path_edit, browse_dir=False),
        )
        matrix_form.addRow(
            "Output path",
            self._line_with_browse(
                self.matrix_output_path_edit, browse_dir=False, save_dialog=True
            ),
        )
        run_matrix_button = QPushButton("Run Matrix")
        run_matrix_button.clicked.connect(self.run_matrix)
        matrix_form.addRow(run_matrix_button)

        layout.addWidget(od_group)
        layout.addWidget(matrix_group)
        layout.addStretch(1)
        return tab

    def _on_mode_changed(self):
        self._apply_mode_defaults()

    def _apply_mode_defaults(self):
        is_wsl = self.execution_mode() == ExecutionMode.WSL
        self.wsl_distro_edit.setEnabled(is_wsl)
        self.wsl_workspace_root_edit.setEnabled(is_wsl)
        self.wsl_netan_executable_edit.setEnabled(is_wsl)
        self.native_netan_executable_edit.setEnabled(not is_wsl)

        if sys.platform == "win32" and is_wsl:
            distro = self.wsl_distro_edit.text().strip() or "Ubuntu"
            unc_root = self.wsl_to_qgis_path(self.wsl_workspace_root_edit.text().strip(), distro)
            if unc_root:
                self.qgis_workspace_root_edit.setText(unc_root)

    def _line_with_browse(self, line_edit, browse_dir=False, save_dialog=False):
        row = QWidget()
        layout = QHBoxLayout(row)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(line_edit)
        button = QPushButton("Browse")
        if browse_dir:
            button.clicked.connect(lambda: self._browse_directory(line_edit))
        elif save_dialog:
            button.clicked.connect(lambda: self._browse_save_file(line_edit))
        else:
            button.clicked.connect(lambda: self._browse_file(line_edit))
        layout.addWidget(button)
        return row

    def _browse_directory(self, line_edit):
        chosen = QFileDialog.getExistingDirectory(
            self, "Select directory", str(self.qgis_workspace_root())
        )
        if chosen:
            line_edit.setText(chosen)

    def _browse_file(self, line_edit):
        chosen, _ = QFileDialog.getOpenFileName(
            self, "Select file", str(self.qgis_workspace_root())
        )
        if chosen:
            line_edit.setText(chosen)

    def _browse_save_file(self, line_edit):
        chosen, _ = QFileDialog.getSaveFileName(
            self, "Select output path", str(self.qgis_workspace_root())
        )
        if chosen:
            line_edit.setText(chosen)

    def execution_mode(self):
        return self.execution_mode_combo.currentText()

    def qgis_workspace_root(self):
        return Path(self.qgis_workspace_root_edit.text().strip() or ".").resolve()

    def wsl_workspace_root(self):
        return self.wsl_workspace_root_edit.text().strip()

    def wsl_distro(self):
        return self.wsl_distro_edit.text().strip()

    def dataset_id(self):
        return self.dataset_combo.currentText().strip()

    def resolve_qgis_path(self, raw):
        value = raw.strip()
        if not value:
            return self.qgis_workspace_root()
        path = Path(value)
        if path.is_absolute():
            return path
        return self.qgis_workspace_root() / path

    def resolve_cli_path(self, raw):
        value = raw.strip()
        if self.execution_mode() == ExecutionMode.NATIVE:
            return str(self.resolve_qgis_path(value))

        if not value:
            return self.wsl_workspace_root()
        if value.startswith("/"):
            return value

        qgis_root = self.qgis_workspace_root()
        input_path = Path(value)
        if input_path.is_absolute():
            try:
                relative = input_path.relative_to(qgis_root)
            except ValueError:
                self.log(
                    "Absolute path is outside the QGIS-visible workspace root; "
                    "WSL may not be able to access it: {}".format(input_path),
                    Qgis.Warning,
                )
                return str(input_path)
        else:
            relative = input_path

        return str(PurePosixPath(self.wsl_workspace_root()) / relative.as_posix())

    def output_qgis_path(self, raw):
        if self.execution_mode() == ExecutionMode.NATIVE:
            return self.resolve_qgis_path(raw)

        value = raw.strip()
        if not value:
            return self.qgis_workspace_root()
        if value.startswith("/"):
            mapped = self.wsl_to_qgis_path(value, self.wsl_distro())
            return Path(mapped) if mapped else Path(value)

        path = Path(value)
        if path.is_absolute():
            return path
        return self.qgis_workspace_root() / path

    def wsl_to_qgis_path(self, linux_path, distro):
        linux_path = (linux_path or "").strip()
        distro = (distro or "").strip()
        if not linux_path or not distro:
            return ""
        posix_path = PurePosixPath(linux_path)
        if not posix_path.is_absolute():
            posix_path = PurePosixPath("/") / posix_path
        return "\\\\wsl$\\{}\\{}".format(distro, str(posix_path).lstrip("/").replace("/", "\\"))

    def netan_program_and_args(self, cli_args):
        if self.execution_mode() == ExecutionMode.NATIVE:
            executable = self.resolve_qgis_path(self.native_netan_executable_edit.text())
            return str(executable), cli_args

        distro = self.wsl_distro()
        if not distro:
            raise ValueError("WSL distro is required in WSL mode.")
        wsl_args = ["-d", distro, "--cd", self.wsl_workspace_root(), "--exec", self.wsl_netan_executable_edit.text().strip()]
        return "wsl.exe", wsl_args + cli_args

    def refresh_workspace(self):
        self.dataset_combo.clear()
        dataset_dir = self.qgis_workspace_root() / ".netan" / "datasets"
        if not dataset_dir.exists():
            self.log(
                "Dataset directory does not exist yet: {}".format(dataset_dir),
                Qgis.Warning,
            )
            return

        datasets = []
        for manifest_path in sorted(dataset_dir.glob("*.json")):
            try:
                manifest = json.loads(manifest_path.read_text())
                dataset_id = manifest.get("dataset_id")
                if isinstance(dataset_id, dict):
                    dataset_id = dataset_id.get("0") or dataset_id.get("value")
                if isinstance(dataset_id, str):
                    datasets.append(dataset_id)
            except Exception as exc:
                self.log("Failed to read {}: {}".format(manifest_path, exc), Qgis.Warning)

        for dataset_id in datasets:
            self.dataset_combo.addItem(dataset_id)

        self.log(
            "Refreshed workspace in {} mode. {} dataset(s) available.".format(
                self.execution_mode(), len(datasets)
            )
        )

    def validate_profile(self):
        if not self.resolve_qgis_path(self.profile_path_edit.text()).exists():
            self.alert(
                "Profile does not exist: {}".format(
                    self.resolve_qgis_path(self.profile_path_edit.text())
                )
            )
            return
        self.start_process(
            [
                "profile",
                "validate",
                self.resolve_cli_path(self.profile_path_edit.text()),
            ],
            load_output=False,
        )

    def compile_profile(self):
        if not self.dataset_id():
            self.alert("Choose a dataset first.")
            return
        self.start_process(
            [
                "profile",
                "compile",
                "--dataset",
                self.dataset_id(),
                "--profile",
                self.resolve_cli_path(self.profile_path_edit.text()),
            ],
            load_output=False,
        )

    def write_route_request(self):
        try:
            request = {
                "route_id": self.route_id_edit.text().strip(),
                "origin": {
                    "id": self.origin_id_edit.text().strip(),
                    "lon": float(self.origin_lon_edit.text().strip()),
                    "lat": float(self.origin_lat_edit.text().strip()),
                },
                "destination": {
                    "id": self.destination_id_edit.text().strip(),
                    "lon": float(self.destination_lon_edit.text().strip()),
                    "lat": float(self.destination_lat_edit.text().strip()),
                },
                "snap": {"max_distance_m": float(self.snap_distance_edit.text().strip())},
                "returns": {
                    "geometry": "full",
                    "segment_rows": True,
                    "road_type_breakdown": ["time_s", "distance_m"],
                    "surface_breakdown": ["time_s", "distance_m"],
                    "penalty_breakdown": True,
                    "explain_cost_derivation": True,
                },
            }
        except ValueError as exc:
            self.alert("Invalid route request values: {}".format(exc))
            return

        request_path = self.resolve_qgis_path(self.route_request_path_edit.text())
        request_path.parent.mkdir(parents=True, exist_ok=True)
        request_path.write_text(json.dumps(request, indent=2))
        self.log("Wrote route request to {}".format(request_path))

    def run_route(self):
        if not self.dataset_id():
            self.alert("Choose a dataset first.")
            return
        self.pending_output_qgis_path = self.output_qgis_path(self.route_output_path_edit.text())
        self.start_process(
            [
                "analyze",
                "route",
                "--dataset",
                self.dataset_id(),
                "--profile",
                self.resolve_cli_path(self.profile_path_edit.text()),
                "--request",
                self.resolve_cli_path(self.route_request_path_edit.text()),
                "--out",
                self.resolve_cli_path(self.route_output_path_edit.text()),
            ],
            load_output=True,
        )

    def run_od(self):
        if not self.dataset_id():
            self.alert("Choose a dataset first.")
            return
        self.pending_output_qgis_path = self.output_qgis_path(self.od_output_path_edit.text())
        self.start_process(
            [
                "analyze",
                "od",
                "--dataset",
                self.dataset_id(),
                "--profile",
                self.resolve_cli_path(self.profile_path_edit.text()),
                "--pairs",
                self.resolve_cli_path(self.od_pairs_path_edit.text()),
                "--out",
                self.resolve_cli_path(self.od_output_path_edit.text()),
            ],
            load_output=True,
        )

    def run_matrix(self):
        if not self.dataset_id():
            self.alert("Choose a dataset first.")
            return
        self.pending_output_qgis_path = self.output_qgis_path(self.matrix_output_path_edit.text())
        self.start_process(
            [
                "analyze",
                "matrix",
                "--dataset",
                self.dataset_id(),
                "--profile",
                self.resolve_cli_path(self.profile_path_edit.text()),
                "--origins",
                self.resolve_cli_path(self.matrix_origins_path_edit.text()),
                "--destinations",
                self.resolve_cli_path(self.matrix_destinations_path_edit.text()),
                "--out",
                self.resolve_cli_path(self.matrix_output_path_edit.text()),
            ],
            load_output=True,
        )

    def start_process(self, cli_args, load_output):
        try:
            program, args = self.netan_program_and_args(cli_args)
        except ValueError as exc:
            self.alert(str(exc))
            return

        if self.execution_mode() == ExecutionMode.NATIVE:
            if not Path(program).exists():
                self.alert("netan executable does not exist: {}".format(program))
                return
        if self.process is not None and self.process.state() != QProcess.NotRunning:
            self.alert("A netan process is already running.")
            return

        self.process = QProcess(self)
        self.process.setWorkingDirectory(str(self.qgis_workspace_root()))
        self.process.readyReadStandardOutput.connect(self._read_stdout)
        self.process.readyReadStandardError.connect(self._read_stderr)
        self.process.finished.connect(
            lambda exit_code, exit_status: self._process_finished(
                exit_code, exit_status, load_output
            )
        )

        self.log("Running: {} {}".format(program, " ".join(args)))
        self.process.start(program, args)

    def _read_stdout(self):
        if self.process is None:
            return
        text = bytes(self.process.readAllStandardOutput()).decode(
            "utf-8", errors="replace"
        ).strip()
        if text:
            self.log(text)

    def _read_stderr(self):
        if self.process is None:
            return
        text = bytes(self.process.readAllStandardError()).decode(
            "utf-8", errors="replace"
        ).strip()
        if text:
            self.log(text, Qgis.Warning)

    def _process_finished(self, exit_code, exit_status, load_output):
        if exit_code != 0 or exit_status != QProcess.NormalExit:
            self.log(
                "netan process failed with exit code {}".format(exit_code), Qgis.Critical
            )
            return

        self.log("netan process completed successfully.")
        if load_output and self.pending_output_qgis_path is not None:
            self.load_output_layer(self.pending_output_qgis_path)
        self.refresh_workspace()

    def load_output_layer(self, output_path):
        output_path = Path(output_path)
        suffix = output_path.suffix.lower()
        if suffix not in {".geojson", ".gpkg", ".geoparquet", ".gpq"}:
            self.log(
                "Output is not a spatial layer QGIS can auto-load: {}".format(output_path)
            )
            return

        layer = QgsVectorLayer(str(output_path), output_path.stem, "ogr")
        if not layer.isValid():
            self.log("Failed to load layer {}".format(output_path), Qgis.Warning)
            return
        QgsProject.instance().addMapLayer(layer)
        self.log("Loaded layer {}".format(output_path))

    def log(self, message, level=Qgis.Info):
        QgsMessageLog.logMessage(message, "netan", level)
        self.log_output.appendPlainText(message)

    def alert(self, message):
        QMessageBox.warning(self, "netan", message)
        self.log(message, Qgis.Warning)
