from __future__ import annotations

import json
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ElementTree
from pathlib import Path
from unittest import mock


INSTALLER_DIRECTORY = Path(__file__).parents[1]
sys.path.insert(0, str(INSTALLER_DIRECTORY))

import generate_assets  # noqa: E402

CONFIGURATION = Path(__file__).parents[4] / "apps/desktop/src-tauri/tauri.windows.conf.json"
GENERATOR = INSTALLER_DIRECTORY / "generate_assets.py"
NSIS_SIDEBAR = INSTALLER_DIRECTORY / "nsis-sidebar.bmp"
NSIS_HEADER = INSTALLER_DIRECTORY / "nsis-header.bmp"
WIX_DIALOG = INSTALLER_DIRECTORY / "wix-dialog.bmp"
WIX_BANNER = INSTALLER_DIRECTORY / "wix-banner.bmp"
ARTWORK = INSTALLER_DIRECTORY / "installer-art.svg"


def bitmap_header(path: Path) -> tuple[int, int, int, int]:
    data = path.read_bytes()
    if data[:2] != b"BM":
        raise AssertionError(f"{path} is not a BMP")
    _, _, _, _, pixel_offset = struct.unpack_from("<2sIHHI", data)
    header_size, width, height, _, bits_per_pixel = struct.unpack_from("<IiiHH", data, 14)
    compression = struct.unpack_from("<I", data, 30)[0]
    if header_size != 40:
        raise AssertionError(f"{path} does not have a BITMAPINFOHEADER")
    return width, abs(height), bits_per_pixel, compression


def non_background_bounds(path: Path, background: bytes) -> tuple[int, int, int, int]:
    width, height, _, _ = bitmap_header(path)
    data = path.read_bytes()
    pixel_offset = struct.unpack_from("<I", data, 10)[0]
    row_size = ((width * 3 + 3) // 4) * 4
    points = []
    for y in range(height):
        row_start = pixel_offset + y * row_size
        for x in range(width):
            pixel = data[row_start + x * 3:row_start + x * 3 + 3]
            if pixel != background:
                points.append((x, y))
    return min(x for x, _ in points), min(y for _, y in points), max(x for x, _ in points), max(y for _, y in points)


class WindowsInstallerAssetTests(unittest.TestCase):
    def test_generator_outputs_windows_compatible_bitmaps(self) -> None:
        result = subprocess.run(
            ["python3", str(GENERATOR), "--check"],
            check=False,
            capture_output=True,
            text=True,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(bitmap_header(NSIS_SIDEBAR), (164, 314, 24, 0))
        self.assertEqual(bitmap_header(NSIS_HEADER), (150, 57, 24, 0))
        self.assertEqual(bitmap_header(WIX_DIALOG), (493, 312, 24, 0))
        self.assertEqual(bitmap_header(WIX_BANNER), (493, 58, 24, 0))

    def test_wix_dialog_leaves_its_native_text_area_white(self) -> None:
        width, height, bits_per_pixel, _ = bitmap_header(WIX_DIALOG)
        self.assertEqual(bits_per_pixel, 24)
        data = WIX_DIALOG.read_bytes()
        pixel_offset = struct.unpack_from("<I", data, 10)[0]
        row_size = ((width * 3 + 3) // 4) * 4

        for y in range(height):
            row_start = pixel_offset + y * row_size
            for x in range(164, width):
                self.assertEqual(data[row_start + x * 3:row_start + x * 3 + 3], b"\xff\xff\xff")

    def test_nsis_sidebar_illustration_keeps_safe_margins(self) -> None:
        left, top, right, bottom = non_background_bounds(NSIS_SIDEBAR, b"s\x8a\xff")

        self.assertGreaterEqual(left, 8)
        self.assertGreaterEqual(top, 8)
        self.assertLessEqual(right, 142)
        self.assertLessEqual(bottom, 306)

    def test_source_illustration_has_no_installer_copy_or_controls(self) -> None:
        artwork = ARTWORK.read_text(encoding="utf-8")
        document = ElementTree.fromstring(artwork)

        self.assertIn('width="164"', artwork)
        self.assertIn("#ff8a73", artwork)
        self.assertIn("#1f1f1f", artwork)
        self.assertIn("#fffdf9", artwork)
        self.assertFalse(list(document.iter("{http://www.w3.org/2000/svg}text")))

    def test_overlay_uses_native_tauri_windows_paths_without_replacing_controls(self) -> None:
        configuration = json.loads(CONFIGURATION.read_text(encoding="utf-8"))
        windows = configuration["bundle"]["windows"]

        self.assertEqual(windows["nsis"], {
            "sidebarImage": "../../../packaging/windows/installer/nsis-sidebar.bmp",
            "headerImage": "../../../packaging/windows/installer/nsis-header.bmp",
        })
        self.assertEqual(windows["wix"], {
            "dialogImagePath": "../../../packaging/windows/installer/wix-dialog.bmp",
            "bannerPath": "../../../packaging/windows/installer/wix-banner.bmp",
        })
        self.assertNotIn("template", windows["nsis"])
        self.assertNotIn("version", windows["wix"])
        self.assertNotIn("signCommand", windows["wix"])

        tauri_directory = CONFIGURATION.parent
        for path in (*windows["nsis"].values(), *windows["wix"].values()):
            self.assertTrue((tauri_directory / path).resolve().is_file())

    def test_check_never_invokes_sips(self) -> None:
        with mock.patch.object(generate_assets.subprocess, "run", side_effect=AssertionError("sips must not run")):
            generate_assets.verify_assets(INSTALLER_DIRECTORY)

    def test_check_rejects_modified_svg_source(self) -> None:
        directory = self.copy_assets()
        with (directory / "nsis-header.svg").open("a", encoding="utf-8") as source:
            source.write("<!-- edited -->\n")

        with self.assertRaisesRegex(RuntimeError, "nsis-header.svg is stale"):
            generate_assets.verify_assets(directory)

    def test_check_rejects_outdated_manifest(self) -> None:
        directory = self.copy_assets()
        manifest = json.loads((directory / generate_assets.MANIFEST_NAME).read_text(encoding="utf-8"))
        manifest["bitmaps"]["wix-banner.bmp"]["sha256"] = "0" * 64
        self.write_manifest(directory, manifest)

        with self.assertRaisesRegex(RuntimeError, "wix-banner.bmp does not match"):
            generate_assets.verify_assets(directory)

    def test_check_rejects_missing_or_malformed_manifest(self) -> None:
        directory = self.copy_assets()
        (directory / generate_assets.MANIFEST_NAME).unlink()
        with self.assertRaisesRegex(RuntimeError, "is missing"):
            generate_assets.verify_assets(directory)

        (directory / generate_assets.MANIFEST_NAME).write_text("[]", encoding="utf-8")
        with self.assertRaisesRegex(RuntimeError, "is malformed"):
            generate_assets.verify_assets(directory)

    def test_check_rejects_corrupt_bitmap_bytes(self) -> None:
        directory = self.copy_assets()
        bitmap = directory / "wix-dialog.bmp"
        data = bytearray(bitmap.read_bytes())
        data[-1] ^= 0xFF
        bitmap.write_bytes(bytes(data))

        with self.assertRaisesRegex(RuntimeError, "wix-dialog.bmp does not match"):
            generate_assets.verify_assets(directory)

    def test_structural_checks_reject_corrupt_bitmaps_even_with_a_matching_manifest(self) -> None:
        cases = {
            "not a BMP": ("nsis-header.bmp", lambda data: b"XX" + data[2:], "is not a BMP"),
            "wrong dimensions": ("nsis-header.bmp", lambda data: data[:18] + struct.pack("<i", 151) + data[22:], "unexpected dimensions"),
            "compressed pixels": ("nsis-header.bmp", lambda data: data[:30] + struct.pack("<I", 1) + data[34:], "uncompressed BI_RGB"),
            "illustration in white text area": ("wix-dialog.bmp", lambda data: self.paint_pixel(data, 493, 312, 200, 0), "native text area"),
            "illustration outside margins": ("nsis-sidebar.bmp", lambda data: self.paint_pixel(data, 164, 314, 0, 0), "safe margins"),
        }
        for label, (bitmap_name, corrupt, message) in cases.items():
            with self.subTest(label), tempfile.TemporaryDirectory() as temporary_directory:
                directory = self.copy_assets(Path(temporary_directory))
                bitmap = directory / bitmap_name
                bitmap.write_bytes(corrupt(bitmap.read_bytes()))
                self.write_manifest(directory, generate_assets.build_manifest(directory))

                with self.assertRaisesRegex(RuntimeError, message):
                    generate_assets.verify_assets(directory)

    def copy_assets(self, directory: Path | None = None) -> Path:
        if directory is None:
            directory = Path(tempfile.mkdtemp())
            self.addCleanup(shutil.rmtree, directory, True)
        names = [*generate_assets.source_artwork(), *generate_assets.ASSETS.values(), generate_assets.MANIFEST_NAME]
        for name in names:
            shutil.copy2(INSTALLER_DIRECTORY / name, directory / name)
        return directory

    @staticmethod
    def write_manifest(directory: Path, manifest: dict) -> None:
        (directory / generate_assets.MANIFEST_NAME).write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    @staticmethod
    def paint_pixel(data: bytes, width: int, height: int, x: int, top_down_y: int) -> bytes:
        row_size = ((width * 3 + 3) // 4) * 4
        start = 54 + (height - 1 - top_down_y) * row_size + x * 3
        return data[:start] + b"\x00\x00\x00" + data[start + 3:]


if __name__ == "__main__":
    unittest.main()
