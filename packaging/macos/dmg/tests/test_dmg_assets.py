from __future__ import annotations

import hashlib
import json
import re
import subprocess
import unittest
from pathlib import Path


DMG_DIRECTORY = Path(__file__).parents[1]
ARTWORK = DMG_DIRECTORY / "background.svg"
BACKGROUND = DMG_DIRECTORY / "background.tiff"
GENERATOR = DMG_DIRECTORY / "generate_assets.py"
TAURI_CONFIG = DMG_DIRECTORY.parents[2] / "apps/desktop/src-tauri/tauri.macos.conf.json"


class DmgAssetTests(unittest.TestCase):
    def test_generator_creates_retina_finder_background(self) -> None:
        result = subprocess.run(
            ["python3", str(GENERATOR), "--check"],
            check=False,
            capture_output=True,
            text=True,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(ARTWORK.is_file())
        self.assertTrue(BACKGROUND.is_file())
        self.assertIn("../../../packages/desktop-ui/src/fonts/DroidSans.ttf", ARTWORK.read_text())

        dimensions = subprocess.run(
            ["sips", "-g", "pixelWidth", "-g", "pixelHeight", str(BACKGROUND)],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        self.assertIn("pixelWidth: 700", dimensions)
        self.assertIn("pixelHeight: 410", dimensions)

        tiff_info = subprocess.run(
            ["tiffutil", "-info", str(BACKGROUND)],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        self.assertIn("Image Width: 700 Image Length: 410", tiff_info)
        self.assertIn("Image Width: 1400 Image Length: 820", tiff_info)

    def test_svg_has_the_approved_artwork_without_finder_owned_content(self) -> None:
        artwork = ARTWORK.read_text(encoding="utf-8")

        self.assertIn('width="700"', artwork)
        self.assertIn('height="410"', artwork)
        self.assertIn("#ff8a73", artwork)
        self.assertIn("#f5f4f0", artwork)
        self.assertIn("Your phone. Your desktop. Connected.", artwork)
        self.assertIn("Drag Peppy to Applications to install.", artwork)
        self.assertNotIn("data-finder-icon", artwork)
        self.assertNotIn("<image", artwork)
        self.assertNotIn("icns", artwork.lower())

    def test_svg_uses_the_approved_left_panel_composition_and_copy_baselines(self) -> None:
        artwork = ARTWORK.read_text(encoding="utf-8")

        self.assertIn('clipPath id="left-panel-clip"', artwork)
        self.assertIn('translate(26 74) rotate(-8 42.5 84.5)', artwork)
        self.assertIn('width="85" height="169"', artwork)
        self.assertIn('translate(83 191) rotate(5 79 51)', artwork)
        self.assertIn('width="158" height="102"', artwork)
        self.assertIn('stroke="#e56f5c"', artwork)
        self.assertIn('x="270" y="77"', artwork)
        self.assertIn('x="270" y="107"', artwork)
        self.assertIn('x="270" y="364"', artwork)

    def test_svg_geometry_keeps_finder_icons_and_labels_inside_the_content_area(self) -> None:
        artwork = ARTWORK.read_text(encoding="utf-8")
        metadata = re.search(r"<metadata>([^<]+)</metadata>", artwork)
        self.assertIsNotNone(metadata)
        geometry = dict(re.findall(r'([a-z-]+)=([0-9]+)', metadata.group(1)))

        self.assertEqual(geometry, {
            "content-width": "700",
            "content-height": "410",
            "app-icon-x": "326",
            "app-icon-y": "218",
            "applications-icon-x": "558",
            "applications-icon-y": "222",
            "icon-size": "128",
            "label-clearance": "42",
        })

        for icon_x, icon_y in ((326, 218), (558, 222)):
            self.assertGreaterEqual(icon_x - 64, 0)
            self.assertLessEqual(icon_x + 64, 700)
            self.assertGreaterEqual(icon_y - 64, 0)
            self.assertLessEqual(icon_y + 64 + 42, 410)

    def test_svg_references_the_pinned_project_droid_sans_source(self) -> None:
        self.assertEqual(
            hashlib.sha256(
                (DMG_DIRECTORY.parents[2] / "packages/desktop-ui/src/fonts/DroidSans.ttf").read_bytes()
            ).hexdigest(),
            "f51b88945f4c1b236f44b8d55a2d304316869127e95248c435c23f1e4142a7db",
        )

    def test_tauri_config_matches_the_finder_layout(self) -> None:
        config = json.loads(TAURI_CONFIG.read_text(encoding="utf-8"))
        dmg = config["bundle"]["macOS"]["dmg"]

        self.assertEqual(dmg["background"], "../../../packaging/macos/dmg/background.tiff")
        self.assertEqual(dmg["appPosition"], {"x": 326, "y": 218})
        self.assertEqual(dmg["applicationFolderPosition"], {"x": 558, "y": 222})
        self.assertEqual(dmg["windowSize"], {"width": 700, "height": 442})


if __name__ == "__main__":
    unittest.main()
