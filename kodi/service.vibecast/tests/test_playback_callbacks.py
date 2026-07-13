import sys
import types
import unittest
from pathlib import Path


ADDON_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ADDON_ROOT))

STUBBED_MODULES = ("xbmc", "xbmcaddon", "xbmcgui", "xbmcvfs", "websocket", "settings_ui")
missing = object()
original_modules = {name: sys.modules.get(name, missing) for name in STUBBED_MODULES}


class Player:
    def __init__(self):
        pass


class Monitor:
    def __init__(self):
        pass


xbmc = types.ModuleType("xbmc")
xbmc.LOGINFO = 1
xbmc.Player = Player
xbmc.Monitor = Monitor
sys.modules["xbmc"] = xbmc

xbmcaddon = types.ModuleType("xbmcaddon")
xbmcaddon.Addon = object
sys.modules["xbmcaddon"] = xbmcaddon

xbmcgui = types.ModuleType("xbmcgui")
xbmcgui.NOTIFICATION_INFO = 1
xbmcgui.NOTIFICATION_WARNING = 2
sys.modules["xbmcgui"] = xbmcgui

xbmcvfs = types.ModuleType("xbmcvfs")
sys.modules["xbmcvfs"] = xbmcvfs

websocket = types.ModuleType("websocket")
websocket.WebSocketApp = object
websocket.WebSocketException = Exception
sys.modules["websocket"] = websocket

settings_ui = types.ModuleType("settings_ui")
settings_ui.AppSettingsDialog = object
sys.modules["settings_ui"] = settings_ui

from service import VibecastPlayer

for module_name, original in original_modules.items():
    if original is missing:
        sys.modules.pop(module_name, None)
    else:
        sys.modules[module_name] = original


class RecordingService:
    def __init__(self):
        self.callbacks = []

    def on_av_started(self):
        self.callbacks.append("av_started")


class PlaybackCallbackTests(unittest.TestCase):
    def test_startup_is_forwarded_only_when_av_is_ready(self):
        service = RecordingService()
        player = VibecastPlayer(service)

        player.onPlayBackStarted()
        self.assertEqual(service.callbacks, [])

        player.onAVStarted()
        self.assertEqual(service.callbacks, ["av_started"])


if __name__ == "__main__":
    unittest.main()
