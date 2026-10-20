#!/usr/bin/env python3
"""Repair Finder metadata in a completed Tauri DMG, fail-closed."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import os
import plistlib
import re
import shutil
import subprocess
import tempfile
from pathlib import Path
from typing import Iterator

from ds_store_layout import assert_layout, write_layout
from dmg_signing import SigningError, SigningIdentity, developer_id_signing


def run(*command: str, capture: bool = False) -> str:
    completed = subprocess.run(command, check=True, text=True, capture_output=capture)
    return completed.stdout


def device_root(device: str) -> str:
    """Return the whole-disk node for an hdiutil system entity."""
    return re.sub(r"s\d+$", "", device)


def devices_for_image(image: Path) -> set[str]:
    """Find attached devices only when hdiutil attributes them to this image."""
    data = plistlib.loads(run("hdiutil", "info", "-plist", capture=True).encode())
    devices: set[str] = set()
    for attached in data.get("images", []):
        image_path = attached.get("image-path")
        if not isinstance(image_path, str):
            continue
        if Path(image_path).resolve() != image.resolve():
            continue
        for entity in attached.get("system-entities", []):
            device = entity.get("dev-entry")
            if isinstance(device, str):
                devices.add(device_root(device))
    return devices


def detach(device: str) -> None:
    try:
        run("hdiutil", "detach", device)
        return
    except subprocess.CalledProcessError:
        pass
    for _ in range(2):
        try:
            run("hdiutil", "detach", "-force", device)
            return
        except subprocess.CalledProcessError:
            pass
    raise RuntimeError(f"could not detach owned DMG device {device}")


def detach_devices(devices: set[str]) -> None:
    for device in sorted({device_root(device) for device in devices}, key=len, reverse=True):
        detach(device)


def attached_devices(image: Path, mountpoint: Path, writable: bool) -> tuple[str, Path]:
    before = devices_for_image(image)
    command = ["hdiutil", "attach", "-plist", "-nobrowse", "-mountpoint", str(mountpoint)]
    if writable:
        command.extend(("-shadow", str(mountpoint.parent / "layout.shadow")))
    else:
        command.append("-readonly")
    command.append(str(image))
    output = run(*command, capture=True)
    try:
        data = plistlib.loads(output.encode())
    except plistlib.InvalidFileException as error:
        detach_devices(devices_for_image(image) - before)
        raise RuntimeError("DMG attach returned malformed metadata") from error
    entities = data.get("system-entities", [])
    devices = {entry["dev-entry"] for entry in entities if isinstance(entry.get("dev-entry"), str)}
    mounted = [entry for entry in entities if entry.get("mount-point")]
    if len(mounted) != 1:
        detach_devices(devices)
        raise RuntimeError("DMG attach did not produce exactly one mount point")
    device = mounted[0].get("dev-entry")
    if not isinstance(device, str):
        detach_devices(devices)
        raise RuntimeError("DMG attach did not return a mounted device")
    return device, Path(mounted[0]["mount-point"])


@contextlib.contextmanager
def mounted_volume(image: Path, root: Path, writable: bool) -> Iterator[tuple[str, Path]]:
    mountpoint = root / ("write-mount" if writable else "verify-mount")
    device, volume = attached_devices(image, mountpoint, writable)
    try:
        yield device, volume
    finally:
        detach(device_root(device))


def app_manifest(volume: Path) -> dict[str, str]:
    applications = list(volume.glob("*.app"))
    if len(applications) != 1:
        raise ValueError(f"DMG must contain exactly one application bundle; found {len(applications)}")
    manifest: dict[str, str] = {}
    for entry in sorted(applications[0].rglob("*")):
        relative = str(entry.relative_to(volume))
        mode = entry.lstat().st_mode & 0o7777
        if entry.is_symlink():
            manifest[relative] = f"link:{mode:o}:{os.readlink(entry)}"
        elif entry.is_file():
            manifest[relative] = f"file:{mode:o}:{hashlib.sha256(entry.read_bytes()).hexdigest()}"
    return manifest


def verify_candidate(candidate: Path) -> None:
    run("hdiutil", "verify", str(candidate))
    data = plistlib.loads(run("hdiutil", "imageinfo", "-plist", str(candidate), capture=True).encode())
    if data.get("Format") != "UDZO":
        raise ValueError("repaired DMG is not UDZO")


def repair_with_signing(image: Path, signing: SigningIdentity | None) -> None:
    candidate_directory = Path(tempfile.mkdtemp(dir=image.parent, prefix=".peppy-dmg-"))
    try:
        with tempfile.TemporaryDirectory(prefix="peppy-dmg-repair-") as temporary:
            root = Path(temporary)
            with mounted_volume(image, root, writable=True) as (device, volume):
                before = app_manifest(volume)
                write_layout(volume)
                assert_layout(volume, device)
                if app_manifest(volume) != before:
                    raise ValueError("Finder repair changed application payload")

            candidate = candidate_directory / "candidate.dmg"
            try:
                run("hdiutil", "convert", str(image), "-format", "UDZO", "-shadow", str(root / "layout.shadow"), "-o", str(candidate))
                if not candidate.is_file():
                    raise RuntimeError("hdiutil did not create the requested DMG candidate")
                verify_candidate(candidate)
                if signing is not None:
                    command = ["codesign", "--force", "--timestamp", "--sign", signing.identity]
                    if signing.keychain is not None:
                        command.extend(("--keychain", str(signing.keychain)))
                    command.append(str(candidate))
                    run(*command)
                    run("codesign", "--verify", "--verbose=2", str(candidate))
                with mounted_volume(candidate, root, writable=False) as (device, volume):
                    assert_layout(volume, device)
                    if app_manifest(volume) != before:
                        raise ValueError("repaired DMG application payload differs from original")
                os.replace(candidate, image)
            finally:
                candidate.unlink(missing_ok=True)
    finally:
        shutil.rmtree(candidate_directory, ignore_errors=True)


def repair(image: Path, signing_mode: str, identity: str | None) -> None:
    if not image.is_file():
        raise ValueError(f"DMG does not exist: {image}")
    if signing_mode == "adhoc":
        repair_with_signing(image, None)
        return
    if signing_mode != "developer-id":
        raise ValueError(f"unknown macOS signing mode: {signing_mode}")
    try:
        with developer_id_signing(identity) as signing:
            repair_with_signing(image, signing)
    except SigningError as error:
        raise ValueError(str(error)) from None


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dmg", type=Path, required=True)
    parser.add_argument("--signing-mode", choices=("developer-id", "adhoc"), required=True)
    parser.add_argument("--identity")
    arguments = parser.parse_args()
    repair(arguments.dmg, arguments.signing_mode, arguments.identity)


if __name__ == "__main__":
    main()
