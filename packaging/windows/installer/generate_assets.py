#!/usr/bin/env python3
"""Generate anti-aliased Windows installer artwork from maintained SVG sources."""

from __future__ import annotations

import argparse
import hashlib
import json
import struct
import subprocess
import tempfile
from pathlib import Path


ASSET_DIRECTORY = Path(__file__).parent
ASSETS = {
    "nsis-sidebar.svg": "nsis-sidebar.bmp",
    "nsis-header.svg": "nsis-header.bmp",
    "installer-art.svg": "wix-dialog.bmp",
    "wix-banner.svg": "wix-banner.bmp",
}
MANIFEST_NAME = "asset-manifest.json"
BITMAP_DIMENSIONS = {
    "nsis-sidebar.bmp": (164, 314),
    "nsis-header.bmp": (150, 57),
    "wix-dialog.bmp": (493, 312),
    "wix-banner.bmp": (493, 58),
}
BITMAP_PIXEL_OFFSET = 54
CORAL_BGR = b"s\x8a\xff"
WHITE_BGR = b"\xff\xff\xff"
NATIVE_TEXT_AREA_START = 164
# Inclusive (left, top, right, bottom) pixel bounds for illustration content, in image coordinates.
CONTENT_SAFE_AREAS = {"nsis-sidebar.bmp": (8, 8, 142, 306)}


def illustration(phone_x: int, phone_y: int, phone_scale: float, monitor_x: int, monitor_y: int, monitor_scale: float) -> str:
    return f'''<path d="M{phone_x + 75 * phone_scale:.1f} {phone_y + 105 * phone_scale:.1f}C{phone_x + 112 * phone_scale:.1f} {phone_y + 98 * phone_scale:.1f} {monitor_x + 72 * monitor_scale:.1f} {monitor_y + 8 * monitor_scale:.1f} {monitor_x + 88 * monitor_scale:.1f} {monitor_y + 25 * monitor_scale:.1f}" fill="none" stroke="#1f1f1f" stroke-linecap="round" stroke-width="2.5" stroke-dasharray="1 9" opacity=".7"/>
  <g transform="translate({phone_x} {phone_y}) rotate(-8 {42 * phone_scale:.1f} {79 * phone_scale:.1f}) scale({phone_scale})">
    <rect width="84" height="158" rx="14" fill="#1f1f1f"/>
    <rect x="6" y="10" width="72" height="136" rx="9" fill="#fffdf9"/>
    <rect x="14" y="24" width="47" height="23" rx="11.5" fill="#ff8a73"/>
    <rect x="26" y="57" width="43" height="29" rx="11" fill="#1f1f1f" opacity=".14"/>
    <rect x="14" y="97" width="51" height="23" rx="11.5" fill="#ff8a73"/>
    <rect x="31" y="150" width="22" height="3" rx="1.5" fill="#d7d2ca"/>
  </g>
  <g transform="translate({monitor_x} {monitor_y}) rotate(-1 {58 * monitor_scale:.1f} {45 * monitor_scale:.1f}) scale({monitor_scale})">
    <rect width="117" height="76" rx="5" fill="#1f1f1f"/>
    <rect x="7" y="7" width="103" height="61" rx="2" fill="#fffdf9"/>
    <rect x="17" y="18" width="47" height="18" rx="9" fill="#ff8a73"/>
    <rect x="49" y="43" width="46" height="14" rx="7" fill="#1f1f1f" opacity=".14"/>
    <path d="M47 76H70L78 89H39Z" fill="#1f1f1f"/>
    <rect x="28" y="89" width="60" height="6" rx="3" fill="#1f1f1f"/>
  </g>'''


def dialog_svg() -> str:
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="493" height="312" viewBox="0 0 493 312">
  <title>Peppy Windows installer illustration</title>
  <rect width="493" height="312" fill="#ffffff"/>
  <rect width="164" height="312" fill="#ff8a73"/>
  {illustration(26, 74, .75, 38, 195, .98)}
</svg>
'''


def sidebar_svg() -> str:
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="164" height="314" viewBox="0 0 164 314">
  <title>Peppy NSIS sidebar illustration</title>
  <rect width="164" height="314" fill="#ff8a73"/>
  {illustration(26, 64, .76, 25, 207, .94)}
</svg>
'''


def header_svg(width: int, height: int, title: str) -> str:
    graphic_x = width - 38
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">
  <title>{title}</title>
  <rect width="{width}" height="{height}" fill="#fffdf9"/>
  <circle cx="{graphic_x + 15}" cy="{height // 2}" r="15" fill="#ff8a73"/>
  <g transform="translate({graphic_x} {height // 2 - 8})">
    <rect width="23" height="16" rx="2" fill="#1f1f1f"/>
    <rect x="3" y="3" width="17" height="10" rx="1" fill="#fffdf9"/>
    <rect x="6" y="5" width="9" height="4" rx="2" fill="#ff8a73"/>
  </g>
</svg>
'''


def source_artwork() -> dict[str, str]:
    return {
        "installer-art.svg": dialog_svg(),
        "nsis-sidebar.svg": sidebar_svg(),
        "nsis-header.svg": header_svg(150, 57, "Peppy NSIS header illustration"),
        "wix-banner.svg": header_svg(493, 58, "Peppy WiX banner illustration"),
    }


def rasterize_svg(svg: Path, bitmap: Path) -> None:
    with tempfile.TemporaryDirectory() as temporary_directory:
        temporary_bitmap = Path(temporary_directory) / "asset.bmp"
        subprocess.run(["sips", "-s", "format", "bmp", str(svg), "--out", str(temporary_bitmap)], check=True, capture_output=True)
        convert_to_24_bit_bitmap(temporary_bitmap, bitmap)


def convert_to_24_bit_bitmap(source: Path, target: Path) -> None:
    data = source.read_bytes()
    pixel_offset = struct.unpack_from("<I", data, 10)[0]
    _, width, signed_height, _, bits_per_pixel, compression = struct.unpack_from("<IiiHHI", data, 14)
    if bits_per_pixel != 32 or compression != 3:
        raise RuntimeError("sips did not produce the expected 32-bit BMP source")
    height = abs(signed_height)
    rows = []
    for row_index in range(height):
        source_row = data[pixel_offset + row_index * width * 4:pixel_offset + (row_index + 1) * width * 4]
        rows.append(b"".join(source_row[offset:offset + 3] for offset in range(0, len(source_row), 4)))
    if signed_height < 0:
        rows.reverse()
    target_row_size = ((width * 3 + 3) // 4) * 4
    image_size = target_row_size * height
    header = struct.pack("<2sIHHI", b"BM", 54 + image_size, 0, 0, 54)
    info = struct.pack("<IiiHHIIiiII", 40, width, height, 1, 24, 0, image_size, 2835, 2835, 0, 0)
    target.write_bytes(header + info + b"".join(row + b"\0" * (target_row_size - len(row)) for row in rows))


def build_assets(output_directory: Path) -> None:
    output_directory.mkdir(parents=True, exist_ok=True)
    for name, source in source_artwork().items():
        (output_directory / name).write_text(source, encoding="utf-8")
    for source_name, bitmap_name in ASSETS.items():
        rasterize_svg(output_directory / source_name, output_directory / bitmap_name)
    manifest = build_manifest(output_directory)
    (output_directory / MANIFEST_NAME).write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def sha256_of(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build_manifest(directory: Path) -> dict:
    return {
        "sources": {name: sha256_of(directory / name) for name in source_artwork()},
        "bitmaps": {
            bitmap_name: {"source": source_name, "sha256": sha256_of(directory / bitmap_name)}
            for source_name, bitmap_name in ASSETS.items()
        },
    }


def verify_sources(directory: Path) -> None:
    for name, source in source_artwork().items():
        path = directory / name
        if not path.is_file() or path.read_text(encoding="utf-8") != source:
            raise RuntimeError(f"{name} is stale; rerun generate_assets.py")


def verify_manifest(directory: Path) -> None:
    manifest_path = directory / MANIFEST_NAME
    if not manifest_path.is_file():
        raise RuntimeError(f"{MANIFEST_NAME} is missing; rerun generate_assets.py")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except ValueError as error:
        raise RuntimeError(f"{MANIFEST_NAME} is not valid JSON; rerun generate_assets.py") from error
    if not isinstance(manifest, dict):
        raise RuntimeError(f"{MANIFEST_NAME} is malformed; rerun generate_assets.py")
    expected = build_manifest(directory)
    for name, digest in expected["sources"].items():
        if manifest.get("sources", {}).get(name) != digest:
            raise RuntimeError(f"{name} does not match {MANIFEST_NAME}; rerun generate_assets.py")
    for bitmap_name, entry in expected["bitmaps"].items():
        if manifest.get("bitmaps", {}).get(bitmap_name) != entry:
            raise RuntimeError(f"{bitmap_name} does not match {MANIFEST_NAME}; rerun generate_assets.py")


def pixel_at(data: bytes, row_size: int, height: int, x: int, y: int) -> bytes:
    """Return the BGR pixel at image coordinates; BMP rows are stored bottom-up."""
    storage_row = height - 1 - y
    start = BITMAP_PIXEL_OFFSET + storage_row * row_size + x * 3
    return data[start:start + 3]


def verify_bitmap(path: Path, name: str) -> None:
    data = path.read_bytes()
    if data[:2] != b"BM" or len(data) < BITMAP_PIXEL_OFFSET:
        raise RuntimeError(f"{name} is not a BMP")
    file_size = struct.unpack_from("<I", data, 2)[0]
    pixel_offset = struct.unpack_from("<I", data, 10)[0]
    header_size, width, height, planes, bits_per_pixel, compression, image_size = struct.unpack_from("<IiiHHII", data, 14)
    if (header_size, planes, bits_per_pixel, compression, pixel_offset) != (40, 1, 24, 0, BITMAP_PIXEL_OFFSET):
        raise RuntimeError(f"{name} is not 24-bit uncompressed BI_RGB")
    if (width, height) != BITMAP_DIMENSIONS[name]:
        raise RuntimeError(f"{name} has unexpected dimensions {width}x{height}")
    row_size = ((width * 3 + 3) // 4) * 4
    if image_size != row_size * height or file_size != BITMAP_PIXEL_OFFSET + image_size or len(data) != file_size:
        raise RuntimeError(f"{name} has inconsistent BMP sizes")
    if name == "wix-dialog.bmp":
        for y in range(height):
            for x in range(NATIVE_TEXT_AREA_START, width):
                if pixel_at(data, row_size, height, x, y) != WHITE_BGR:
                    raise RuntimeError(f"{name} draws into its native text area")
    if name in CONTENT_SAFE_AREAS:
        points = [
            (x, y)
            for y in range(height)
            for x in range(width)
            if pixel_at(data, row_size, height, x, y) != CORAL_BGR
        ]
        if not points:
            raise RuntimeError(f"{name} has no illustration content")
        left = min(x for x, _ in points)
        top = min(y for _, y in points)
        right = max(x for x, _ in points)
        bottom = max(y for _, y in points)
        safe_left, safe_top, safe_right, safe_bottom = CONTENT_SAFE_AREAS[name]
        if left < safe_left or top < safe_top or right > safe_right or bottom > safe_bottom:
            raise RuntimeError(f"{name} illustration leaves its safe margins")


def verify_assets(directory: Path = ASSET_DIRECTORY) -> None:
    """Check committed sources, manifest, and bitmap structure without invoking sips."""
    verify_sources(directory)
    for bitmap_name in ASSETS.values():
        if not (directory / bitmap_name).is_file():
            raise RuntimeError(f"{bitmap_name} is missing; rerun generate_assets.py")
    verify_manifest(directory)
    for bitmap_name in ASSETS.values():
        verify_bitmap(directory / bitmap_name, bitmap_name)


def verify_rendered_bitmaps(directory: Path = ASSET_DIRECTORY) -> None:
    """Maintainer check: re-rasterize with sips and require byte-identical bitmaps (macOS-version sensitive)."""
    with tempfile.TemporaryDirectory() as temporary_directory:
        rendered_directory = Path(temporary_directory)
        build_assets(rendered_directory)
        for bitmap_name in ASSETS.values():
            if (directory / bitmap_name).read_bytes() != (rendered_directory / bitmap_name).read_bytes():
                raise RuntimeError(f"{bitmap_name} differs from this macOS sips rendering; regenerate on the maintainer's macOS")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="verify committed sources, manifest, and bitmap structure")
    parser.add_argument(
        "--check-render",
        action="store_true",
        help="also re-rasterize with macOS sips and require byte-identical bitmaps (opt-in maintainer check)",
    )
    parser.add_argument("--output-directory", type=Path)
    arguments = parser.parse_args()
    if arguments.check or arguments.check_render:
        verify_assets()
        if arguments.check_render:
            verify_rendered_bitmaps()
    else:
        build_assets(arguments.output_directory or ASSET_DIRECTORY)


if __name__ == "__main__":
    main()
