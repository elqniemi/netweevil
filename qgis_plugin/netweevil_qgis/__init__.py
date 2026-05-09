def classFactory(iface):
    from .plugin import NetweevilPlugin

    return NetweevilPlugin(iface)
