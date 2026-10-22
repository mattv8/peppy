from __future__ import annotations

import base64
import tempfile
import json
import re
import unittest
from pathlib import Path
from unittest.mock import patch

from mac_alias import Alias

from ds_store_layout import APP_POSITION, APPLICATIONS_POSITION, WINDOW_BOUNDS, assert_layout, background_alias, read_layout, write_layout

PORTABLE_BACKGROUND_ALIAS = base64.b64decode(
    "AAAAAAFWAAIAAAVQZXBweQAAAAAAAAAAAAAAAAAAAAAAAAAAAADm/cXUSCsAAAAAABsPYmFja2dyb3VuZC50aWZmAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAHOb9xeIAAAAAAAAAAP////8AAAAAAAAAAAAAAAAAAAAAAAAACy5iYWNrZ3JvdW5kAAAQAAgAAOb9xdQAAAARAAgAAOb9xeIAAAABAAQAAAAbAAIAIlBlcHB5Oi5iYWNrZ3JvdW5kOgBiYWNrZ3JvdW5kLnRpZmYADgAgAA8AYgBhAGMAawBnAHIAbwB1AG4AZAAuAHQAaQBmAGYADwAMAAUAUABlAHAAcAB5ABIAHC8uYmFja2dyb3VuZC9iYWNrZ3JvdW5kLnRpZmYAEwAOL1ZvbHVtZXMvUGVwcHn//wAA"
)


class DsStoreLayoutTests(unittest.TestCase):
    def test_background_alias_preserves_relative_target_and_normalizes_volume_path(self) -> None:
        alias = Alias.from_bytes(PORTABLE_BACKGROUND_ALIAS)
        alias.volume.posix_path = "/private/var/folders/ci/Peppy"
        alias.target.posix_path = "/private/var/folders/ci/Peppy/.background/background.tiff"
        with patch("ds_store_layout.Alias.for_file", return_value=alias):
            background = Path("/private/var/folders/ci/Peppy/.background/background.tiff")
            alias = background_alias(background)

        self.assertEqual(alias.target.posix_path, "/.background/background.tiff")
        self.assertEqual(alias.volume.posix_path, "/Volumes/Peppy")
        self.assertIn(b".background:\x00background.tiff", alias.to_bytes())
        self.assertNotIn(b"/private/var/folders/ci/Peppy", alias.to_bytes())

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
                alias.return_value.target.posix_path = "/.background/background.tiff"
                alias.return_value.volume.name = volume.name
                alias.return_value.volume.posix_path = f"/Volumes/{volume.name}"
                assert_layout(volume)
                layout = read_layout(volume)
                self.assertEqual(layout["bwsp"]["WindowBounds"], WINDOW_BOUNDS)
                layout["icvp"].pop("scrollPositionX")
                from ds_store import DSStore
                with DSStore.open(str(volume / ".DS_Store"), "w+") as store:
                    store["."]["icvp"] = layout["icvp"]
                with self.assertRaises(ValueError):
                    assert_layout(volume)

    def test_assert_layout_rejects_background_alias_without_portable_posix_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            volume = Path(temporary) / "Peppy"
            background = volume / ".background" / "background.tiff"
            background.parent.mkdir(parents=True)
            background.write_bytes(b"fixture")
            (volume / "Peppy.app").mkdir()
            (volume / "Applications").symlink_to("/Applications")
            alias = Alias.from_bytes(PORTABLE_BACKGROUND_ALIAS)
            alias.volume.posix_path = None
            alias.target.posix_path = None

            with patch("ds_store_layout.background_alias") as generated_alias:
                generated_alias.return_value.to_bytes.return_value = alias.to_bytes()
                write_layout(volume)

            with self.assertRaises(ValueError):
                assert_layout(volume)


if __name__ == "__main__":
    unittest.main()
