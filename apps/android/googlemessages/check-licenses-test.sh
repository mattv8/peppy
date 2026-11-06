#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
"$root/check-licenses.sh"

temporary_inventory=$(mktemp)
trap 'rm -f "$temporary_inventory"' EXIT
python3 - "$root/license-inventory.json" "$temporary_inventory" <<'PY'
import json
import sys

inventory = json.load(open(sys.argv[1]))
inventory["modules"].pop()
json.dump(inventory, open(sys.argv[2], "w"), indent=2)
PY

if LICENSE_INVENTORY="$temporary_inventory" "$root/check-licenses.sh"; then
    printf '%s\n' 'expected missing inventory module to fail' >&2
    exit 1
fi

python3 - "$root/license-inventory.json" "$temporary_inventory" <<'PY'
import json
import sys

inventory = json.load(open(sys.argv[1]))
inventory["modules"].append({
    "path": "example.invalid/unresolved",
    "version": "v0.0.0",
    "licenseFile": "LICENSE",
    "licenseFamily": "MIT",
    "distributed": False,
})
json.dump(inventory, open(sys.argv[2], "w"), indent=2)
PY

if LICENSE_INVENTORY="$temporary_inventory" "$root/check-licenses.sh"; then
    printf '%s\n' 'expected unresolved inventory module to fail' >&2
    exit 1
fi

python3 - "$root/license-inventory.json" "$temporary_inventory" <<'PY'
import json
import sys

inventory = json.load(open(sys.argv[1]))
entry = next(entry for entry in inventory["modules"] if entry["path"] == "golang.org/x/mobile")
entry["distributed"] = False
entry.pop("asset", None)
entry.pop("licenseSha256", None)
json.dump(inventory, open(sys.argv[2], "w"), indent=2)
PY

if LICENSE_INVENTORY="$temporary_inventory" "$root/check-licenses.sh"; then
    printf '%s\n' 'expected linked module marked non-distributed to fail' >&2
    exit 1
fi

missing_aar=$(mktemp)
error_output=$(mktemp)
mv "$root/build/gmlink.aar" "$missing_aar"
if "$root/check-licenses.sh" 2>"$error_output"; then
    mv "$missing_aar" "$root/build/gmlink.aar"
    printf '%s\n' 'expected missing AAR to fail' >&2
    exit 1
fi
mv "$missing_aar" "$root/build/gmlink.aar"
grep -qx 'build the AAR first' "$error_output"
rm -f "$error_output"
