#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
inventory=${LICENSE_INVENTORY:-"$root/license-inventory.json"}
gmlink_assets="$root/../app/src/gmlinkOn/assets/third_party"
notice=${GOOGLE_MESSAGES_NOTICE:-"$gmlink_assets/NOTICE-google-messages.txt"}
aar=${GOOGLE_MESSAGES_AAR:-"$root/build/gmlink.aar"}
if [ ! -f "$aar" ]; then
    printf '%s\n' 'build the AAR first' >&2
    exit 1
fi
modules=$(mktemp)
trap 'rm -f "$modules"' EXIT

cd "$root"
go list -m -json all > "$modules"
python3 - "$inventory" "$modules" "$aar" "$gmlink_assets" "$notice" "$root/go.mod" <<'PY'
import hashlib
import json
import pathlib
import re
import subprocess
import sys
import tempfile
import zipfile

inventory_path, modules_path, aar_path, assets_path, notice_path, go_mod_path = map(pathlib.Path, sys.argv[1:])
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

notice_text = notice_path.read_text()
notice_modules = {}
for line in notice_text.splitlines():
    if line.startswith("* "):
        module_and_version, *details = line[2:].split(" — ")
        if len(details) != 2:
            raise SystemExit("Google Messages notice has an invalid module entry")
    elif line.startswith("Package: "):
        module_and_version = line.removeprefix("Package: ")
    else:
        module_and_version = line
    match = re.fullmatch(r"([A-Za-z0-9./-]+) (v\S+)", module_and_version)
    if match is None:
        continue
    path, version = match.groups()
    if path in notice_modules and notice_modules[path] != version:
        raise SystemExit("Google Messages notice has duplicate module entries")
    notice_modules[path] = version

distributed_modules = {
    path: entry["version"]
    for path, entry in entries.items()
    if entry["distributed"]
}
if notice_modules != distributed_modules:
    raise SystemExit("Google Messages notice module list does not match distributed inventory")

toolchain = re.search(r"^toolchain (go\S+)$", go_mod_path.read_text(), re.MULTILINE)
if toolchain is None:
    raise SystemExit("Google Messages module has no toolchain declaration")
expected_attribution = (
    f"The Go standard library and runtime from toolchain {toolchain.group(1)} are "
    "statically linked into libgojni.so; see the BSD-3-Clause-go-x-and-mobile.txt text."
)
if expected_attribution not in " ".join(notice_text.split()):
    raise SystemExit("Google Messages notice lacks Go standard library attribution")


def notice_asset(asset_name):
    asset = assets_path / asset_name
    return asset if asset.is_file() else None

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
        asset = notice_asset(asset_name)
        if asset is None:
            raise SystemExit("linked module has no notice asset")
        if hashlib.sha256(asset.read_bytes()).hexdigest() != entry["licenseSha256"]:
            raise SystemExit("notice asset differs from the resolved upstream license text")
    if "licenseExceptions" in entry:
        exception = entry["licenseExceptions"]
        asset = notice_asset(exception["asset"])
        if asset is None or hashlib.sha256(asset.read_bytes()).hexdigest() != exception["sha256"]:
            raise SystemExit("notice asset differs from the resolved upstream license exception text")
PY
