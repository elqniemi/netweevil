def classFactory(iface):
    from .plugin import NetanPlugin

    return NetanPlugin(iface)
