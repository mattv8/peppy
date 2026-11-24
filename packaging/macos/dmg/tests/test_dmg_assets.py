from __future__ import annotations

import hashlib
import json
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
