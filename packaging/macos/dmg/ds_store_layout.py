"""Deterministic Finder metadata for the Peppy disk image."""

from __future__ import annotations

import plistlib
import subprocess
from pathlib import Path

from ds_store import DSStore
from mac_alias import Alias

WINDOW_BOUNDS = "{{100, 100}, {700, 442}}"
APP_POSITION = (326, 218)
APPLICATIONS_POSITION = (558, 222)
ICON_SIZE = 128.0
TEXT_SIZE = 16.0
GRID_SPACING = 54.0
BACKGROUND_PATH = ".background/background.tiff"
HIDDEN_WINDOW_FIELDS = (
    "ContainerShowSidebar", "ShowStatusBar", "ShowToolbar", "ShowPathbar",
    "ShowTabView", "ShowSidebar", "PreviewPaneVisibility",
)


def finder_records(volume: Path) -> dict[str, dict[str, object]]:
    """Return the complete records Finder needs for this mounted volume."""
    background = volume / BACKGROUND_PATH
    if not background.is_file():
        raise ValueError(f"missing DMG background: {background}")
    application = volume_application(volume)
    applications = volume / "Applications"
    if not applications.is_symlink() or applications.resolve() != Path("/Applications"):
        raise ValueError("DMG Applications entry must link to /Applications")
    return {
        ".": {
            "bwsp": {**{field: False for field in HIDDEN_WINDOW_FIELDS}, "SidebarWidth": 0, "WindowBounds": WINDOW_BOUNDS},
            "icvp": {
                "backgroundType": 2, "backgroundImageAlias": background_alias(background).to_bytes(),
                "backgroundColorRed": 1.0, "backgroundColorGreen": 1.0, "backgroundColorBlue": 1.0,
                "arrangeBy": "none", "gridOffsetX": 0.0, "gridOffsetY": 0.0,
                "scrollPositionX": 0.0, "scrollPositionY": 0.0, "iconSize": ICON_SIZE,
                "textSize": TEXT_SIZE, "gridSpacing": GRID_SPACING, "labelOnBottom": True,
                "showIconPreview": False, "showItemInfo": False, "viewOptionsVersion": 1,
            },
            "vSrn": ("long", 1),
        },
        application.name: {"Iloc": APP_POSITION}, "Applications": {"Iloc": APPLICATIONS_POSITION},
    }


def volume_application(volume: Path) -> Path:
    applications = list(volume.glob("*.app"))
    if len(applications) != 1:
        raise ValueError(f"DMG must contain exactly one application bundle; found {len(applications)}")
    return applications[0]


def background_alias(background: Path) -> Alias:
    alias = Alias.for_file(str(background))
    alias.volume.posix_path = None
    alias.target.posix_path = None
    return alias


def write_layout(volume: Path) -> None:
    with DSStore.open(str(volume / ".DS_Store"), "w+") as store:
        for filename, values in finder_records(volume).items():
            for code, value in values.items():
                store[filename][code] = value


def read_layout(volume: Path) -> dict[str, object]:
    with DSStore.open(str(volume / ".DS_Store"), "r") as store:
        return {"bwsp": store["."]["bwsp"], "icvp": store["."]["icvp"], "vSrn": store["."]["vSrn"],
                "app": store[volume_application(volume).name]["Iloc"], "applications": store["Applications"]["Iloc"]}


def volume_name(device: str) -> str:
    data = plistlib.loads(subprocess.run(("diskutil", "info", "-plist", device), check=True, capture_output=True).stdout)
    name = data.get("VolumeName")
    if not isinstance(name, str) or not name:
        raise ValueError("could not determine DMG volume name")
    return name


def assert_layout(volume: Path, device: str | None = None) -> None:
    try:
        layout = read_layout(volume)
    except KeyError as error:
        raise ValueError("DMG .DS_Store is missing required Finder layout") from error
    icvp = layout["icvp"]
    alias = icvp.get("backgroundImageAlias") if isinstance(icvp, dict) else None
    expected_name = volume_name(device) if device else volume.name
    alias_valid = isinstance(alias, bytes) and (decoded := Alias.from_bytes(alias)).target.filename == Path(BACKGROUND_PATH).name and decoded.volume.name == expected_name and b"/Users/" not in alias and b"/private/" not in alias
    if not isinstance(icvp, dict) or any((
        icvp.get("backgroundType") != 2, icvp.get("arrangeBy") != "none", icvp.get("iconSize") != ICON_SIZE,
        any(icvp.get(f"backgroundColor{channel}") != 1.0 for channel in ("Red", "Green", "Blue")),
        icvp.get("textSize") != TEXT_SIZE, icvp.get("gridOffsetX") != 0.0, icvp.get("gridOffsetY") != 0.0,
        icvp.get("scrollPositionX") != 0.0, icvp.get("scrollPositionY") != 0.0, icvp.get("gridSpacing") != GRID_SPACING,
        icvp.get("labelOnBottom") is not True, icvp.get("showIconPreview") is not False,
        icvp.get("showItemInfo") is not False, not alias_valid, layout["app"] != APP_POSITION,
        layout["applications"] != APPLICATIONS_POSITION, layout["bwsp"].get("WindowBounds") != WINDOW_BOUNDS,
        any(layout["bwsp"].get(field) is not False for field in HIDDEN_WINDOW_FIELDS),
        layout["bwsp"].get("SidebarWidth") != 0, layout["vSrn"] != (b"long", 1),
    )):
        raise ValueError("DMG .DS_Store does not contain the required Finder layout")
