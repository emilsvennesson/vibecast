import unittest
import xml.etree.ElementTree as ElementTree
from pathlib import Path
from re import findall


SKIN_ROOT = Path(__file__).resolve().parents[1] / "resources" / "skins" / "Default"


class SkinTests(unittest.TestCase):
    def test_app_settings_textures_are_bundled(self):
        root = ElementTree.parse(SKIN_ROOT / "1080i" / "AppSettings.xml").getroot()
        textures = {
            element.text.strip()
            for element in root.iter()
            if element.tag.startswith("texture") and element.text and element.text.strip()
        }

        self.assertGreater(len(textures), 0)
        for texture in textures:
            with self.subTest(texture=texture):
                self.assertTrue((SKIN_ROOT / "media" / texture).is_file())

    def test_app_settings_uses_addon_localization(self):
        xml = (SKIN_ROOT / "1080i" / "AppSettings.xml").read_text()
        strings = (
            SKIN_ROOT.parents[1]
            / "language"
            / "resource.language.en_gb"
            / "strings.po"
        ).read_text()

        self.assertNotIn("$LOCALIZE[300", xml)
        string_ids = findall(r"\$ADDON\[service\.vibecast (\d+)\]", xml)
        self.assertGreater(len(string_ids), 0)
        for string_id in string_ids:
            with self.subTest(string_id=string_id):
                self.assertIn(f'msgctxt "#{string_id}"', strings)

    def test_list_focus_surfaces_only_show_with_keyboard_focus(self):
        root = ElementTree.parse(SKIN_ROOT / "1080i" / "AppSettings.xml").getroot()

        for control_id in ("100", "200"):
            container = root.find(f".//control[@type='list'][@id='{control_id}']")
            self.assertIsNotNone(container)
            focus_images = container.findall("./focusedlayout/control[@type='image']")
            self.assertGreater(len(focus_images), 0)
            for image in focus_images:
                with self.subTest(control_id=control_id):
                    self.assertEqual(
                        image.findtext("visible"),
                        f"Control.HasFocus({control_id})",
                    )


if __name__ == "__main__":
    unittest.main()
