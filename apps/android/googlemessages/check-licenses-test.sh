#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
backup_directory=
error_output=
temporary_notice=
cleanup() {
    rm -f "$temporary_inventory" "$temporary_notice" "$error_output"
    [ -z "$backup_directory" ] || rm -rf "$backup_directory"
}

temporary_inventory=$(mktemp)
trap cleanup EXIT INT TERM
backup_directory=$(mktemp -d)
temporary_aar="$backup_directory/gmlink.aar"
cp "${GOOGLE_MESSAGES_AAR:-$root/build/gmlink.aar}" "$temporary_aar"
GOOGLE_MESSAGES_AAR="$temporary_aar" "$root/check-licenses.sh"

python3 - "$root/license-inventory.json" "$temporary_inventory" <<'PY'
import json
import sys

inventory = json.load(open(sys.argv[1]))
inventory["modules"].pop()
json.dump(inventory, open(sys.argv[2], "w"), indent=2)
PY

if LICENSE_INVENTORY="$temporary_inventory" GOOGLE_MESSAGES_AAR="$temporary_aar" "$root/check-licenses.sh"; then
    printf '%s\n' 'expected missing inventory module to fail' >&2
    exit 1
fi

temporary_notice=$(mktemp)
python3 - "$root/../app/src/sideloadGmlinkOn/assets/third_party/NOTICE-google-messages.txt" "$temporary_notice" <<'PY'
import pathlib
import sys

notice = pathlib.Path(sys.argv[1]).read_text().splitlines()
pathlib.Path(sys.argv[2]).write_text(
    "\n".join(line for line in notice if not line.startswith("google.golang.org/protobuf ")) + "\n"
)
PY
if GOOGLE_MESSAGES_NOTICE="$temporary_notice" GOOGLE_MESSAGES_AAR="$temporary_aar" "$root/check-licenses.sh"; then
    printf '%s\n' 'expected notice module list mismatch to fail' >&2
    exit 1
fi

python3 - "$root/../app/src/sideloadGmlinkOn/assets/third_party/NOTICE-google-messages.txt" "$temporary_notice" <<'PY'
import pathlib
import sys

notice = pathlib.Path(sys.argv[1]).read_text().splitlines()
pathlib.Path(sys.argv[2]).write_text(
    "\n".join(line for line in notice if "Go standard library and runtime" not in line) + "\n"
)
PY
if GOOGLE_MESSAGES_NOTICE="$temporary_notice" GOOGLE_MESSAGES_AAR="$temporary_aar" "$root/check-licenses.sh"; then
    printf '%s\n' 'expected missing Go standard library attribution to fail' >&2
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

if LICENSE_INVENTORY="$temporary_inventory" GOOGLE_MESSAGES_AAR="$temporary_aar" "$root/check-licenses.sh"; then
    printf '%s\n' 'expected unresolved inventory module to fail' >&2
    exit 1
fi

python3 - "$root/license-inventory.json" "$temporary_inventory" <<'PY'
import json
import sys

inventory = json.load(open(sys.argv[1]))
entry = next(entry for entry in inventory["modules"] if entry["path"] == "golang.org/x/mobile")
entry["distributed"] = False
json.dump(inventory, open(sys.argv[2], "w"), indent=2)
PY

python3 - "$root/../app/src/sideloadGmlinkOn/assets/third_party/NOTICE-google-messages.txt" "$temporary_notice" <<'PY'
import pathlib
import sys

notice = pathlib.Path(sys.argv[1]).read_text().splitlines()
pathlib.Path(sys.argv[2]).write_text(
    "\n".join(line for line in notice if not line.startswith("golang.org/x/mobile ")) + "\n"
)
PY
if error=$(LICENSE_INVENTORY="$temporary_inventory" GOOGLE_MESSAGES_NOTICE="$temporary_notice" GOOGLE_MESSAGES_AAR="$temporary_aar" "$root/check-licenses.sh" 2>&1); then
    printf '%s\n' 'expected linked module marked non-distributed to fail' >&2
    exit 1
fi
printf '%s\n' "$error" | grep -qx 'inventory distribution marker does not match the built native library'

error_output=$(mktemp)
rm "$temporary_aar"
if GOOGLE_MESSAGES_AAR="$temporary_aar" "$root/check-licenses.sh" 2>"$error_output"; then
    printf '%s\n' 'expected missing AAR to fail' >&2
    exit 1
fi
grep -qx 'build the AAR first' "$error_output"
