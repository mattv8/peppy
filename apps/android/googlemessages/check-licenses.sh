#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
inventory=${LICENSE_INVENTORY:-"$root/license-inventory.json"}
notice=${GOOGLE_MESSAGES_NOTICE:-"$root/../app/src/sideload/assets/third_party/NOTICE-google-messages.txt"}
modules=$(mktemp)
if [ ! -f "$root/build/gmlink.aar" ]; then
    printf '%s\n' 'build the AAR first' >&2
    exit 1
fi
trap 'rm -f "$modules"' EXIT

cd "$root"
go list -m -json all > "$modules"
python3 - "$inventory" "$modules" "$root/build/gmlink.aar" "$root/../app/src/sideload/assets/third_party" "$notice" <<'PY'
import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile
import zipfile

inventory_path, modules_path, aar_path, assets_path, notice_path = map(pathlib.Path, sys.argv[1:])
inventory = json.loads(inventory_path.read_text())
decoder = json.JSONDecoder()
modules_text = modules_path.read_text()
position = 0
resolved = {}

while position < len(modules_text):
    while position < len(modules_text) and modules_text[position].isspace():
        position += 1
    if position == len(modules_text):
        break
    module, position = decoder.raw_decode(modules_text, position)
    if not module.get("Main"):
        resolved[module["Path"]] = module.get("Version", "")

entries = {entry["path"]: entry for entry in inventory["modules"]}
if set(entries) != set(resolved):
    raise SystemExit("license inventory does not match the resolved module graph")

notice_modules = {}
for line in notice_path.read_text().splitlines():
    if not line.startswith("* "):
        continue
    parts = line[2:].split(" — ")
    if len(parts) != 3:
        raise SystemExit("Google Messages notice has an invalid module entry")
    module_and_version, license_family, asset = parts
    try:
        path, version = module_and_version.rsplit(" ", 1)
    except ValueError as error:
        raise SystemExit("Google Messages notice has an invalid module entry") from error
    if path in notice_modules:
        raise SystemExit("Google Messages notice has duplicate module entries")
    notice_modules[path] = (version, license_family, asset)

distributed_modules = {
    path: (entry["version"], entry["licenseFamily"], entry["asset"])
    for path, entry in entries.items()
    if entry["distributed"]
}
if notice_modules != distributed_modules:
    raise SystemExit("Google Messages notice module list does not match distributed inventory")

linked = {}
with zipfile.ZipFile(aar_path) as aar, tempfile.TemporaryDirectory() as temporary_directory:
    for abi in ("arm64-v8a", "x86_64"):
        library = f"jni/{abi}/libgojni.so"
        try:
            content = aar.read(library)
        except KeyError as error:
            raise SystemExit("AAR is missing a required native library") from error
        path = pathlib.Path(temporary_directory) / f"{abi}-libgojni.so"
        path.write_bytes(content)
        metadata = subprocess.check_output(["go", "version", "-m", path], text=True)
        for line in metadata.splitlines():
            fields = line.split()
            if len(fields) >= 3 and fields[0] == "dep":
                module_path, version = fields[1:3]
                if module_path in linked and linked[module_path] != version:
                    raise SystemExit("AAR ABIs report inconsistent module versions")
                linked[module_path] = version

linked.pop("dev.peppy/googlemessages", None)
if not set(linked).issubset(entries):
    raise SystemExit("built native library has no inventory mapping")
for path, version in resolved.items():
    entry = entries[path]
    if entry["version"] != version:
        raise SystemExit("license inventory version does not match the resolved module graph")
    linked_version = linked.get(path)
    if bool(entry["distributed"]) != (linked_version is not None):
        raise SystemExit("inventory distribution marker does not match the built native library")
    if linked_version is not None:
        if linked_version != version:
            raise SystemExit("built native library version does not match the resolved module graph")
        asset_name = entry.get("asset")
        if not asset_name:
            raise SystemExit("linked module has no notice asset")
        asset = assets_path / asset_name
        if not asset.is_file():
            raise SystemExit("linked module has no notice asset")
        if hashlib.sha256(asset.read_bytes()).hexdigest() != entry["licenseSha256"]:
            raise SystemExit("notice asset differs from the resolved upstream license text")
    if "licenseExceptions" in entry:
        exception = entry["licenseExceptions"]
        asset = assets_path / exception["asset"]
        if not asset.is_file() or hashlib.sha256(asset.read_bytes()).hexdigest() != exception["sha256"]:
            raise SystemExit("notice asset differs from the resolved upstream license exception text")
PY
