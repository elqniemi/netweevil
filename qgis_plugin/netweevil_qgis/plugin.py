"""QGIS plugin entry point: toolbar action and dock lifecycle."""

from qgis.PyQt.QtGui import QIcon

from .compat import QAction, qt_dock_area
from .constants import PLUGIN_ICON, PLUGIN_MENU


class NetweevilPlugin:
    def __init__(self, iface):
        self.iface = iface
        self.action = None
        self.dock = None

    def initGui(self):
        self.action = QAction(
            QIcon(str(PLUGIN_ICON)), "netweevil", self.iface.mainWindow()
        )
        self.action.setCheckable(True)
        self.action.triggered.connect(self.toggle_dock)
        self.iface.addPluginToMenu(PLUGIN_MENU, self.action)
        self.iface.addToolBarIcon(self.action)

    def unload(self):
        if self.dock is not None:
            self.dock.save_settings()
            self.dock.cleanup()
            self.iface.removeDockWidget(self.dock)
            self.dock.deleteLater()
            self.dock = None
        if self.action is not None:
            self.iface.removePluginMenu(PLUGIN_MENU, self.action)
            self.iface.removeToolBarIcon(self.action)

    def toggle_dock(self, checked=False):
        if self.dock is None:
            from .dock import NetweevilDock

            self.dock = NetweevilDock(self.iface, self.action)
            self.dock.visibilityChanged.connect(self.sync_action_state)
            self.iface.addDockWidget(qt_dock_area("RightDockWidgetArea"), self.dock)
        self.dock.setVisible(bool(checked))
        if checked:
            self.dock.raise_()

    def sync_action_state(self, visible):
        if self.action is None:
            return
        was_blocked = self.action.blockSignals(True)
        self.action.setChecked(bool(visible))
        self.action.blockSignals(was_blocked)
