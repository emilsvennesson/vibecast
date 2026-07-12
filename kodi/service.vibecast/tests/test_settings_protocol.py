import unittest
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from protocol import decode_server_message, registration_message, settings_update_message
from settings_model import SettingsCatalog


def app_wire(revision=3, value="balanced", writable=True):
    return {
        "appKey": "example",
        "displayName": "Example",
        "revision": revision,
        "settings": [
            {
                "key": "quality",
                "label": "Quality",
                "description": "Preferred playback quality",
                "kind": "select",
                "default": "balanced",
                "value": value,
                "writable": writable,
                "options": [
                    {"value": "balanced", "label": "Balanced"},
                    {"value": "highest", "label": "Highest"},
                ],
            }
        ],
    }


class ProtocolTests(unittest.TestCase):
    def test_registration_uses_player_envelope(self):
        message = registration_message("player-1", "Kodi", {"platform": "linux"})

        self.assertEqual(
            message,
            {
                "type": "register",
                "player": {
                    "playerId": "player-1",
                    "name": "Kodi",
                    "capabilities": {"platform": "linux"},
                },
            },
        )
        self.assertNotIn("playerId", message)

    def test_server_parser_retains_top_level_playback_payload(self):
        message = decode_server_message('{"type":"seek","sessionId":"s","position":12}')

        self.assertEqual(message.message_type, "seek")
        self.assertEqual(message.payload["sessionId"], "s")

    def test_update_serialization_includes_revision_and_null_reset(self):
        message = settings_update_message("request-1", "example", 7, {"quality": None})

        self.assertEqual(
            message,
            {
                "type": "settingsUpdate",
                "requestId": "request-1",
                "appKey": "example",
                "expectedRevision": 7,
                "changes": {"quality": None},
            },
        )


class SettingsCatalogTests(unittest.TestCase):
    def setUp(self):
        self.catalog = SettingsCatalog()
        self.catalog.replace_snapshot({"type": "settingsSnapshot", "apps": [app_wire()]})

    def test_update_uses_authoritative_revision_without_mutating_value(self):
        self.catalog.set_connected(True)

        message = self.catalog.begin_update(
            "request-1", "example", "quality", "highest"
        )

        self.assertEqual(message["expectedRevision"], 3)
        self.assertEqual(message["changes"], {"quality": "highest"})
        self.assertEqual(self.catalog.app("example").setting("quality").value, "balanced")

    def test_result_replaces_app_with_server_value(self):
        self.catalog.set_connected(True)
        self.catalog.begin_update("request-1", "example", "quality", "highest")

        status = self.catalog.apply_update_result(
            {
                "type": "settingsUpdateResult",
                "requestId": "request-1",
                "status": "ok",
                "app": app_wire(revision=4, value="highest"),
            }
        )

        self.assertEqual(status, "ok")
        self.assertEqual(self.catalog.app("example").revision, 4)
        self.assertEqual(self.catalog.app("example").setting("quality").value, "highest")
        self.assertFalse(self.catalog.app_has_pending_update("example"))

    def test_disconnect_disables_updates_but_retains_snapshot_for_display(self):
        self.catalog.set_connected(False)

        with self.assertRaisesRegex(ValueError, "read-only"):
            self.catalog.begin_update("request-1", "example", "quality", "highest")

        self.assertEqual(self.catalog.app("example").setting("quality").value, "balanced")

    def test_installation_scoped_setting_is_read_only(self):
        self.catalog.replace_snapshot(
            {"type": "settingsSnapshot", "apps": [app_wire(writable=False)]}
        )
        self.catalog.set_connected(True)

        with self.assertRaisesRegex(ValueError, "read-only"):
            self.catalog.begin_update("request-1", "example", "quality", "highest")

    def test_second_update_waits_for_authoritative_result(self):
        self.catalog.set_connected(True)
        self.catalog.begin_update("request-1", "example", "quality", "highest")

        with self.assertRaisesRegex(ValueError, "already has an update pending"):
            self.catalog.begin_update(
                "request-2", "example", "quality", "balanced"
            )

    def test_new_snapshot_replaces_remote_values(self):
        self.catalog.replace_snapshot(
            {"type": "settingsSnapshot", "apps": [app_wire(revision=8, value=None)]}
        )

        self.assertEqual(self.catalog.app("example").revision, 8)
        self.assertIsNone(self.catalog.app("example").setting("quality").value)


if __name__ == "__main__":
    unittest.main()
