#!/usr/bin/env python3
"""Generate the original macOS Finder background and Retina TIFF asset."""

from __future__ import annotations

import argparse
import hashlib
import subprocess
import tempfile
from pathlib import Path


ASSET_DIRECTORY = Path(__file__).parent
PROJECT_ROOT = ASSET_DIRECTORY.parents[2]
SOURCE_FONT = PROJECT_ROOT / "packages/desktop-ui/src/fonts/DroidSans.ttf"
SVG = ASSET_DIRECTORY / "background.svg"
TIFF = ASSET_DIRECTORY / "background.tiff"
PREVIEW = ASSET_DIRECTORY / "background-preview.png"
FONT_SHA256 = "f51b88945f4c1b236f44b8d55a2d304316869127e95248c435c23f1e4142a7db"
CONTENT_WIDTH = 700
CONTENT_HEIGHT = 410


def svg_artwork() -> str:
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{CONTENT_WIDTH}" height="{CONTENT_HEIGHT}" viewBox="0 0 {CONTENT_WIDTH} {CONTENT_HEIGHT}">
  <title>Peppy macOS installer background</title>
  <style>
    @font-face {{ font-family: 'Droid Sans'; src: url('../../../packages/desktop-ui/src/fonts/DroidSans.ttf'); }}
    .heading {{ fill: #1f1f1f; font-family: 'Droid Sans', sans-serif; font-size: 28px; font-weight: 700; }}
    .tagline {{ fill: #45433f; font-family: 'Droid Sans', sans-serif; font-size: 14px; }}
    .instruction {{ fill: #45433f; font-family: 'Droid Sans', sans-serif; font-size: 13px; }}
  </style>
  <defs><clipPath id="left-panel-clip"><rect width="230" height="410"/></clipPath></defs>
  <rect width="700" height="410" fill="#f5f4f0"/>
  <path d="M0 0H230V410H0Z" fill="#ff8a73"/>
  <g clip-path="url(#left-panel-clip)">
    <path d="M-36 382C28 326 96 336 154 419" fill="none" stroke="#e56f5c" stroke-width="19" opacity=".18"/>
    <path d="M-28 417C28 365 73 369 116 422" fill="none" stroke="#e56f5c" stroke-width="12" opacity=".13"/>
    <path d="M104 171C141 163 172 177 190 212" fill="none" stroke="#1f1f1f" stroke-linecap="round" stroke-width="2.5" stroke-dasharray="1 9" opacity=".7"/>
    <g transform="translate(26 74) rotate(-8 42.5 84.5)">
      <rect x="0" y="0" width="85" height="169" rx="15" fill="#1f1f1f"/>
      <rect x="6" y="10" width="73" height="148" rx="10" fill="#fffdf9"/>
      <rect x="13" y="24" width="49" height="26" rx="13" fill="#e56f5c"/>
      <path d="M23 34H51M23 40H43" fill="none" stroke="#fffdf9" stroke-linecap="round" stroke-width="2"/>
      <rect x="27" y="62" width="45" height="32" rx="12" fill="#1f1f1f" opacity=".12"/>
      <rect x="13" y="105" width="53" height="26" rx="13" fill="#e56f5c"/>
      <path d="M23 115H54M23 121H46" fill="none" stroke="#fffdf9" stroke-linecap="round" stroke-width="2"/>
      <rect x="31" y="162" width="23" height="3" rx="1.5" fill="#fffdf9" opacity=".72"/>
    </g>
    <g transform="translate(83 191) rotate(5 79 51)">
      <rect x="0" y="0" width="158" height="102" rx="6" fill="#1f1f1f"/>
      <rect x="8" y="8" width="142" height="78" rx="3" fill="#fffdf9"/>
      <rect x="20" y="22" width="64" height="22" rx="11" fill="#e56f5c"/>
      <path d="M31 31H69M31 37H58" fill="none" stroke="#fffdf9" stroke-linecap="round" stroke-width="2"/>
      <rect x="66" y="52" width="60" height="16" rx="8" fill="#1f1f1f" opacity=".18"/>
      <path d="M64 102H93L101 116H56Z" fill="#1f1f1f"/>
      <rect x="38" y="116" width="82" height="7" rx="3.5" fill="#1f1f1f"/>
    </g>
  </g>
  <text class="heading" x="270" y="77">Peppy</text>
  <text class="tagline" x="270" y="107">Your phone. Your desktop. Connected.</text>
  <path d="M418 221H465" fill="none" stroke="#ff8a73" stroke-linecap="round" stroke-width="3"/>
  <path d="M458 213L467 221L458 229" fill="none" stroke="#ff8a73" stroke-linecap="round" stroke-linejoin="round" stroke-width="3"/>
  <text class="instruction" x="270" y="364">Drag Peppy to Applications to install.</text>
  <metadata>content-width=700; content-height=410; app-icon-x=326; app-icon-y=218; applications-icon-x=558; applications-icon-y=222; icon-size=128; label-clearance=42</metadata>
</svg>
'''


def run(command: list[str]) -> None:
    subprocess.run(command, check=True)


def verify_pinned_font() -> None:
    actual_hash = hashlib.sha256(SOURCE_FONT.read_bytes()).hexdigest()
    if actual_hash != FONT_SHA256:
        raise RuntimeError("packages/desktop-ui/src/fonts/DroidSans.ttf does not match the pinned hash")


def build_assets(preview: Path | None = None) -> None:
    verify_pinned_font()
    SVG.write_text(svg_artwork(), encoding="utf-8")
    with tempfile.TemporaryDirectory() as temporary_directory:
        temporary = Path(temporary_directory)
        one_x = temporary / "background-1x.tiff"
        two_x = temporary / "background-2x.tiff"
        run(["sips", "-s", "format", "tiff", "-z", "410", "700", str(SVG), "--out", str(one_x)])
        run([
            "sips", "-s", "format", "tiff", "-z", "820", "1400",
            "-s", "dpiWidth", "144", "-s", "dpiHeight", "144",
            str(SVG), "--out", str(two_x),
        ])
        run(["tiffutil", "-cat", str(one_x), str(two_x), "-out", str(TIFF)])
    if preview is not None:
        run(["sips", "-s", "format", "png", str(SVG), "--out", str(preview)])


def verify_assets() -> None:
    if not all(path.is_file() for path in (SVG, TIFF)):
        raise RuntimeError("run generate_assets.py before checking the DMG artwork")
    if SVG.read_text(encoding="utf-8") != svg_artwork():
        raise RuntimeError("background.svg is stale; rerun generate_assets.py")
    expected_tiff_hash = "9a622815e3e12c00645b37bc50c09bbf4c8d3e30eb9674947512919083f7f7cf"
    if hashlib.sha256(TIFF.read_bytes()).hexdigest() != expected_tiff_hash:
        raise RuntimeError("background.tiff is stale; rerun generate_assets.py")
    verify_pinned_font()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--preview", type=Path)
    arguments = parser.parse_args()
    if arguments.check:
        verify_assets()
    else:
        build_assets(arguments.preview)


if __name__ == "__main__":
    main()
