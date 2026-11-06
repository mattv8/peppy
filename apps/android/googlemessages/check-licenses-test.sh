#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
saved_aar=
error_output=
temporary_notice=
cleanup() {
    rm -f "$temporary_inventory" "$temporary_notice" "$error_output"
    if [ -n "$saved_aar" ] && [ -f "$saved_aar" ]; then
        mv "$saved_aar" "$root/build/gmlink.aar"
    fi
}

temporary_inventory=$(mktemp)
trap cleanup EXIT INT TERM
"$root/check-licenses.sh"

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

temporary_notice=$(mktemp)
python3 - "$root/../app/src/sideload/assets/third_party/NOTICE-google-messages.txt" "$temporary_notice" <<'PY'
import pathlib
import sys

notice = pathlib.Path(sys.argv[1]).read_text().splitlines()
pathlib.Path(sys.argv[2]).write_text(
    "\n".join(line for line in notice if not line.startswith("* google.golang.org/protobuf ")) + "\n"
)
PY
if GOOGLE_MESSAGES_NOTICE="$temporary_notice" "$root/check-licenses.sh"; then
    printf '%s\n' 'expected notice module list mismatch to fail' >&2
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

saved_aar=$(mktemp)
error_output=$(mktemp)
mv "$root/build/gmlink.aar" "$saved_aar"
if "$root/check-licenses.sh" 2>"$error_output"; then
    printf '%s\n' 'expected missing AAR to fail' >&2
    exit 1
fi
mv "$saved_aar" "$root/build/gmlink.aar"
saved_aar=
grep -qx 'build the AAR first' "$error_output"
