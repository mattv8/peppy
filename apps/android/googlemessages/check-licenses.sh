#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
inventory=${LICENSE_INVENTORY:-"$root/license-inventory.json"}
modules=$(mktemp)
trap 'rm -f "$modules"' EXIT

cd "$root"
go list -m -json all > "$modules"
python3 - "$inventory" "$modules" "$root/../app/src/sideload/assets/third_party" <<'PY'
import hashlib
import json
import pathlib
import sys

inventory_path, modules_path, assets_path = map(pathlib.Path, sys.argv[1:])
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

for path, version in resolved.items():
    entry = entries[path]
    if entry["version"] != version:
        raise SystemExit("license inventory version does not match the resolved module graph")
    if entry["distributed"]:
        asset = assets_path / entry["asset"]
        if not asset.is_file():
            raise SystemExit("distributed module has no notice asset")
        if hashlib.sha256(asset.read_bytes()).hexdigest() != entry["licenseSha256"]:
            raise SystemExit("notice asset differs from the resolved upstream license text")
    if "licenseExceptions" in entry:
        exception = entry["licenseExceptions"]
        asset = assets_path / exception["asset"]
        if not asset.is_file() or hashlib.sha256(asset.read_bytes()).hexdigest() != exception["sha256"]:
            raise SystemExit("notice asset differs from the resolved upstream license exception text")
PY
