#!/usr/bin/env bash
set -euo pipefail
umask 077
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(mktemp -d "${TMPDIR:-/tmp}/peppy-abi-test.XXXXXX")
trap 'rm -rf "$root"' EXIT
python3 - "$root" <<'PY'
import pathlib
import struct
import sys
import zipfile

root = pathlib.Path(sys.argv[1])
for name in ("unsigned", "signature", "block", "one-abi", "directory-only", "non-so-only"):
    path = root / f"{name}.apk"
    with zipfile.ZipFile(path, "w") as archive:
        dex = zipfile.ZipInfo("classes.dex")
        dex.compress_type = zipfile.ZIP_DEFLATED
        dex.extra = b"\xfe\xca\x02\x00xy"
        archive.writestr(dex, b"dex")
        archive.writestr("lib/arm64-v8a/lib.so", b"arm", compress_type=zipfile.ZIP_STORED)
        if name == "directory-only":
            archive.writestr("lib/x86_64/", b"")
        elif name == "non-so-only":
            archive.writestr("lib/x86_64/readme.txt", b"x86")
        elif name != "one-abi":
            archive.writestr("lib/x86_64/lib.so", b"x86", compress_type=zipfile.ZIP_STORED)
        if name == "signature":
            archive.writestr("META-INF/TEST.SF", b"signature")
    if name == "block":
        data = path.read_bytes()
        eocd = data.rfind(b"PK\x05\x06")
        offset = struct.unpack_from("<I", data, eocd + 16)[0]
        block = struct.pack("<Q", 24) + struct.pack("<Q", 24) + b"APK Sig Block 42"
        result = bytearray(data[:offset] + block + data[offset:])
        struct.pack_into("<I", result, eocd + len(block) + 16, offset + len(block))
        path.write_bytes(result)
PY
"$script_dir/split-android-apk-abis.py" "$root/unsigned.apk" "$root"
"$script_dir/check-android-abi-apk.py" "$root/unsigned.apk" \
  --abi "arm64-v8a=$root/peppy-android-arm64-v8a-unsigned.apk" \
  --abi "x86_64=$root/peppy-android-x86_64-unsigned.apk"
for labels in duplicate missing; do
  args=(--abi "arm64-v8a=$root/peppy-android-arm64-v8a-unsigned.apk")
  if [ "$labels" = duplicate ]; then
    args+=(--abi "arm64-v8a=$root/peppy-android-arm64-v8a-unsigned.apk")
  fi
  if "$script_dir/check-android-abi-apk.py" "$root/unsigned.apk" "${args[@]}" >"$root/error" 2>&1; then
    printf 'expected label failure: %s\n' "$labels" >&2
    exit 1
  fi
done
python3 - "$root" <<'PY'
import pathlib
import sys
import zipfile

root = pathlib.Path(sys.argv[1])
with zipfile.ZipFile(root / "unsigned.apk") as original:
    for abi in ("arm64-v8a", "x86_64"):
        with zipfile.ZipFile(root / f"peppy-android-{abi}-unsigned.apk") as split:
            for entry in split.infolist():
                source = original.getinfo(entry.filename)
                assert (entry.date_time, entry.external_attr) == (source.date_time, source.external_attr)
                assert entry.extra == b""
with zipfile.ZipFile(root / "peppy-android-arm64-v8a-unsigned.apk") as original:
    entries = [(entry, original.read(entry)) for entry in original.infolist()]
    for variant in ("order", "compression", "directory-only", "non-so-only"):
        with zipfile.ZipFile(root / f"split-{variant}.apk", "w") as target:
            items = list(reversed(entries)) if variant == "order" else entries
            for entry, data in items:
                if entry.filename == "lib/arm64-v8a/lib.so" and variant in ("directory-only", "non-so-only"):
                    name = "lib/arm64-v8a/" if variant == "directory-only" else "lib/arm64-v8a/readme.txt"
                    target.writestr(name, b"")
                elif entry.filename == "classes.dex" and variant == "compression":
                    target.writestr(entry.filename, data, compress_type=zipfile.ZIP_STORED)
                else:
                    target.writestr(entry, data)
PY
for variant in order compression directory-only non-so-only; do
  if "$script_dir/check-android-abi-apk.py" "$root/unsigned.apk" \
    --abi "arm64-v8a=$root/split-$variant.apk" \
    --abi "x86_64=$root/peppy-android-x86_64-unsigned.apk" >"$root/error" 2>&1; then
    printf 'expected comparison failure: %s\n' "$variant" >&2
    exit 1
  fi
done
for variant in unsigned signature block one-abi directory-only non-so-only; do
  mkdir "$root/$variant"
  if [ "$variant" = unsigned ]; then
    touch "$root/$variant/peppy-android-arm64-v8a-unsigned.apk"
  fi
  if "$script_dir/split-android-apk-abis.py" "$root/$variant.apk" "$root/$variant" >"$root/error" 2>&1; then
    printf 'expected split failure: %s\n' "$variant" >&2
    exit 1
  fi
  if [ "$variant" != unsigned ] && [ -n "$(ls -A "$root/$variant")" ]; then
    printf 'failed split left outputs: %s\n' "$variant" >&2
    exit 1
  fi
done
mkdir "$root/rollback"
python3 - "$script_dir/split-android-apk-abis.py" "$root" <<'PY'
import importlib.util
import pathlib
import sys

spec = importlib.util.spec_from_file_location("split_apks", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
real_link = module.os.link
calls = 0

def fail_second_link(source, destination):
    global calls
    calls += 1
    if calls == 2:
        raise OSError("injected publication failure")
    real_link(source, destination)

module.os.link = fail_second_link
root = pathlib.Path(sys.argv[2])
try:
    module.split(root / "unsigned.apk", root / "rollback")
    raise AssertionError("publication unexpectedly succeeded")
except OSError as error:
    assert "injected publication failure" in str(error)
assert not list((root / "rollback").iterdir())
PY
printf 'android ABI APK tests passed\n'
