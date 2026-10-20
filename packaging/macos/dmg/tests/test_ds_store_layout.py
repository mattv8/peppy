from __future__ import annotations

import tempfile
import json
import re
import unittest
from pathlib import Path
from unittest.mock import patch

from ds_store_layout import APP_POSITION, APPLICATIONS_POSITION, WINDOW_BOUNDS, assert_layout, read_layout, write_layout


class DsStoreLayoutTests(unittest.TestCase):
    def test_layout_geometry_matches_the_packaged_tauri_config(self) -> None:
        root = Path(__file__).parents[4]
        config = json.loads((root / "apps/desktop/src-tauri/tauri.macos.conf.json").read_text())
        dmg = config["bundle"]["macOS"]["dmg"]
        bounds = re.fullmatch(r"\{\{-?\d+, -?\d+\}, \{(\d+), (\d+)\}\}", WINDOW_BOUNDS)
        self.assertIsNotNone(bounds)
        self.assertEqual(tuple(map(int, bounds.groups())), (dmg["windowSize"]["width"], dmg["windowSize"]["height"]))
        self.assertEqual(APP_POSITION, (dmg["appPosition"]["x"], dmg["appPosition"]["y"]))
        self.assertEqual(APPLICATIONS_POSITION, (dmg["applicationFolderPosition"]["x"], dmg["applicationFolderPosition"]["y"]))

    def test_written_layout_round_trips_with_volume_relative_background_alias(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            volume = Path(temporary) / "Peppy_dev"
            (volume / ".background").mkdir(parents=True)
            (volume / ".background" / "background.tiff").write_bytes(b"fixture")
            (volume / "Peppy_dev.app").mkdir()
            (volume / "Applications").symlink_to("/Applications")

            with patch("ds_store_layout.background_alias") as background_alias:
                background_alias.return_value.to_bytes.return_value = b"fixture-alias"
                write_layout(volume)

            layout = read_layout(volume)
            self.assertEqual(layout["app"], APP_POSITION)
            self.assertEqual(layout["applications"], APPLICATIONS_POSITION)
            self.assertEqual(layout["icvp"]["backgroundType"], 2)
            for channel in ("Red", "Green", "Blue"):
                self.assertEqual(layout["icvp"][f"backgroundColor{channel}"], 1.0)
            self.assertEqual(layout["icvp"]["arrangeBy"], "none")
            self.assertEqual(layout["bwsp"]["WindowBounds"], WINDOW_BOUNDS)
            self.assertFalse(layout["bwsp"]["ShowToolbar"])
            self.assertEqual(layout["icvp"]["scrollPositionX"], 0.0)
            self.assertEqual(layout["vSrn"], (b"long", 1))

    def test_assert_layout_rejects_missing_chrome_or_scroll_fields(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            volume = Path(temporary) / "Peppy_dev"
            (volume / ".background").mkdir(parents=True)
            (volume / ".background" / "background.tiff").write_bytes(b"fixture")
            (volume / "Peppy_dev.app").mkdir()
            (volume / "Applications").symlink_to("/Applications")
            with patch("ds_store_layout.background_alias") as background_alias:
                background_alias.return_value.to_bytes.return_value = b"fixture-alias"
                write_layout(volume)
            with patch("ds_store_layout.Alias.from_bytes") as alias:
                alias.return_value.target.filename = "background.tiff"
                alias.return_value.volume.name = volume.name
                layout = read_layout(volume)
                self.assertEqual(layout["bwsp"]["WindowBounds"], WINDOW_BOUNDS)
                layout["icvp"].pop("scrollPositionX")
                from ds_store import DSStore
                with DSStore.open(str(volume / ".DS_Store"), "w+") as store:
                    store["."]["icvp"] = layout["icvp"]
                with self.assertRaises(ValueError):
                    assert_layout(volume)


if __name__ == "__main__":
    unittest.main()
