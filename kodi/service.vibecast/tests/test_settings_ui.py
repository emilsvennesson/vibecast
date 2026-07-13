import sys
import types
import unittest
from pathlib import Path


ADDON_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ADDON_ROOT))


class WindowXMLDialog:
    def __init__(self):
        self.closed = False

    def close(self):
        self.closed = True

    def getFocusId(self):
        raise AssertionError("Back actions must close before checking focus")


class Action:
    def __init__(self, action_id):
        self._action_id = action_id

    def getId(self):
        return self._action_id


xbmcgui = types.ModuleType("xbmcgui")
xbmcgui.WindowXMLDialog = WindowXMLDialog
sys.modules["xbmcgui"] = xbmcgui

from settings_ui import ACTION_NAV_BACK, ACTION_PREVIOUS_MENU, AppSettingsDialog


class AppSettingsDialogTests(unittest.TestCase):
    def test_back_actions_close_before_initialization_and_focus_guards(self):
        for action_id in (ACTION_PREVIOUS_MENU, ACTION_NAV_BACK):
            with self.subTest(action_id=action_id):
                dialog = AppSettingsDialog()

                dialog.onAction(Action(action_id))

                self.assertTrue(dialog.closed)


if __name__ == "__main__":
    unittest.main()
