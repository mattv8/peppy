#!/usr/bin/env bash
set -euo pipefail
set +x

[ "$#" -eq 3 ] || {
  echo "usage: $0 <direct-apk> <play-aab> <native-symbols-zip>" >&2
  exit 2
}

google_messages_link_expected=${PEPPY_GOOGLE_MESSAGES_LINK_EXPECTED:-}
case "$google_messages_link_expected" in
  true|false) ;;
  *) echo 'PEPPY_GOOGLE_MESSAGES_LINK_EXPECTED must be true or false' >&2; exit 2 ;;
esac

direct_apk=$1
play_aab=$2
symbols_archive=$3
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
license_inventory="$script_dir/../../apps/android/googlemessages/license-inventory.json"
[ -f "$license_inventory" ] || { echo "license inventory not found: $license_inventory" >&2; exit 1; }
for artifact in "$direct_apk" "$play_aab" "$symbols_archive"; do
  [ -f "$artifact" ] || { echo "artifact not found: $artifact" >&2; exit 1; }
done

sdk_root=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}
[ -n "$sdk_root" ] || { echo 'ANDROID_HOME or ANDROID_SDK_ROOT is required' >&2; exit 1; }
apkanalyzer="$sdk_root/cmdline-tools/latest/bin/apkanalyzer"
[ -x "$apkanalyzer" ] || { echo "apkanalyzer not found: $apkanalyzer" >&2; exit 1; }

bundletool_jar=${BUNDLETOOL_JAR:-}
if [ -z "$bundletool_jar" ] || [ ! -f "$bundletool_jar" ]; then
  echo 'BUNDLETOOL_JAR must name the verified bundletool JAR' >&2
  exit 1
fi

temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/peppy-android-distribution.XXXXXX")
trap 'rm -rf "$temporary_dir"' EXIT

"$apkanalyzer" manifest print "$direct_apk" > "$temporary_dir/direct-manifest.xml"
java -jar "$bundletool_jar" dump manifest --bundle="$play_aab" --module=base > "$temporary_dir/play-manifest.xml"

python3 - "$temporary_dir/direct-manifest.xml" "$temporary_dir/play-manifest.xml" <<'PY'
import sys
import xml.etree.ElementTree as ET

ANDROID = "{http://schemas.android.com/apk/res/android}"
EXPECTED = ((sys.argv[1], "direct"), (sys.argv[2], "play"))

for manifest_path, expected_distribution in EXPECTED:
    try:
        root = ET.parse(manifest_path).getroot()
    except ET.ParseError as error:
        raise SystemExit(f"could not parse manifest emitted for {expected_distribution}: {error}")
    application = root.find("application")
    if application is None:
        raise SystemExit(f"{expected_distribution} manifest has no application element")
    distributions = [
        element.get(ANDROID + "value")
        for element in application.findall("meta-data")
        if element.get(ANDROID + "name") == "dev.peppy.mobile.distribution"
    ]
    if distributions != [expected_distribution]:
        raise SystemExit(
            f"{expected_distribution} manifest distribution metadata must be exactly "
            f"{expected_distribution!r}; got {distributions!r}"
        )
PY

play_apks=()
materialize_play_apks() {
  local mode=$1 archive="$temporary_dir/$1.apks" directory="$temporary_dir/$1-apks" initial_count=${#play_apks[@]}
  shift
  java -jar "$bundletool_jar" build-apks --bundle="$play_aab" --output="$archive" "$@" || {
    echo "could not materialize $mode APKs from Play AAB" >&2
    exit 1
  }
  mkdir "$directory"
  unzip -q "$archive" -d "$directory" || {
    echo "could not extract $mode APKs materialized from Play AAB" >&2
    exit 1
  }
  while IFS= read -r -d '' apk; do
    play_apks+=("$apk")
  done < <(find "$directory" -type f -name '*.apk' -print0)
  [ "${#play_apks[@]}" -gt "$initial_count" ] || {
    echo "bundletool did not materialize any $mode APKs from Play AAB" >&2
    exit 1
  }
}

materialize_play_apks universal --mode=universal
materialize_play_apks default
python3 - "$direct_apk" "$play_aab" "$google_messages_link_expected" "$license_inventory" "${play_apks[@]}" <<'PY'
import json
import pathlib
import re
import sys
import zipfile

DIRECT_ABIS = ("arm64-v8a", "x86_64")
DEX_PATTERN = re.compile(r"(?:base/)?dex/classes\d*\.dex$|classes\d*\.dex$")
GOJNI_PATTERN = re.compile(r"(?:base/)?lib/[^/]+/libgojni\.so$")
DEX_PREFIX = b"Ldev/peppy/gmlink/"
AGPL_TEXT = b"GNU AFFERO GENERAL PUBLIC LICENSE"
GOOGLE_MESSAGES_NOTICE = b"Third-party notices for the Google Messages link"
GOOGLE_MESSAGES_NOTICE_FILENAME = "NOTICE-google-messages.txt"


def required_license_assets(inventory_path):
    try:
        modules = json.loads(inventory_path.read_text())["modules"]
    except (OSError, json.JSONDecodeError, KeyError, TypeError) as error:
        raise SystemExit(f"could not read Google Messages license inventory: {error}")
    assets = set()
    for module in modules:
        if not module.get("distributed"):
            continue
        asset = module.get("asset")
        if not isinstance(asset, str) or not asset:
            raise SystemExit("distributed Google Messages module has no license asset")
        assets.add(f"assets/third_party/{asset}")
        exception = module.get("licenseExceptions")
        if exception is not None:
            exception_asset = exception.get("asset")
            if not isinstance(exception_asset, str) or not exception_asset:
                raise SystemExit("Google Messages license exception has no asset")
            assets.add(f"assets/third_party/{exception_asset}")
    return assets


LICENSE_ASSETS = required_license_assets(pathlib.Path(sys.argv[4]))
DIRECT_ASSETS = (
    "assets/third_party/AGPL-3.0.txt",
    "assets/third_party/NOTICE-google-messages.txt",
    *sorted(LICENSE_ASSETS - {"assets/third_party/AGPL-3.0.txt"}),
)


def contents(archive_path):
    try:
        with zipfile.ZipFile(archive_path) as archive:
            return {entry.filename: archive.read(entry) for entry in archive.infolist() if not entry.is_dir()}
    except (OSError, zipfile.BadZipFile) as error:
        raise SystemExit(f"could not inspect archive {archive_path}: {error}")


def dex_contains(entries):
    return any(DEX_PATTERN.fullmatch(name) and DEX_PREFIX in content for name, content in entries.items())


def prohibited(entries):
    if any(GOJNI_PATTERN.fullmatch(name) for name in entries):
        return "libgojni.so"
    if dex_contains(entries):
        return "dev/peppy/gmlink/ dex classes"
    if any(AGPL_TEXT in content for content in entries.values()):
        return "AGPL text"
    if any(
        pathlib.PurePosixPath(name).name == GOOGLE_MESSAGES_NOTICE_FILENAME
        or GOOGLE_MESSAGES_NOTICE in content
        for name, content in entries.items()
    ):
        return "Google Messages notice content"
    if any(asset in entries for asset in LICENSE_ASSETS):
        return "Google Messages license asset"
    return None


direct_entries = contents(pathlib.Path(sys.argv[1]))
if sys.argv[3] == "true":
    for abi in DIRECT_ABIS:
        required_library = f"lib/{abi}/libgojni.so"
        if required_library not in direct_entries:
            raise SystemExit(f"direct APK is missing {required_library}")
    if not dex_contains(direct_entries):
        raise SystemExit("direct APK is missing dev/peppy/gmlink/ dex classes")
    for asset in DIRECT_ASSETS:
        if asset not in direct_entries:
            raise SystemExit(f"direct APK is missing {asset}")
else:
    found = prohibited(direct_entries)
    if found:
        raise SystemExit(f"direct APK contains {found}")

play_archives = (("Play AAB", pathlib.Path(sys.argv[2])),) + tuple(
    (f"Play APK {pathlib.Path(path).name}", pathlib.Path(path)) for path in sys.argv[5:]
)
for label, archive_path in play_archives:
    found = prohibited(contents(archive_path))
    if found:
        raise SystemExit(f"{label} contains {found}")
PY

symbol_tool=${LLVM_NM:-}
if [ -z "$symbol_tool" ]; then
  symbol_tool=$(command -v llvm-nm || command -v nm || true)
fi
[ -n "$symbol_tool" ] || { echo 'llvm-nm or nm is required to inspect native symbols' >&2; exit 1; }

archive_entries=$(unzip -Z1 "$symbols_archive") || {
  echo "could not list native symbols archive: $symbols_archive" >&2
  exit 1
}
symbol_libraries=()
while IFS= read -r library; do
  case "$library" in
    arm64-v8a/libpeppy_mobile_bindings.so.sym|x86_64/libpeppy_mobile_bindings.so.sym)
      symbol_libraries+=("$library")
      ;;
  esac
done <<< "$archive_entries"
[ "${#symbol_libraries[@]}" -gt 0 ] || {
  echo 'native symbols archive has no peppy mobile bindings libraries' >&2
  exit 1
}
for abi in arm64-v8a x86_64; do
  found_abi=false
  for library in "${symbol_libraries[@]}"; do
    if [ "$library" = "$abi/libpeppy_mobile_bindings.so.sym" ]; then
      found_abi=true
      break
    fi
  done
  if [ "$found_abi" != true ]; then
    echo "native symbols archive is missing $abi peppy mobile bindings symbols" >&2
    exit 1
  fi
done

for library in "${symbol_libraries[@]}"; do
  output_library="$temporary_dir/$(basename "$(dirname "$library")")-$(basename "$library")"
  unzip -p "$symbols_archive" "$library" > "$output_library" || {
    echo "could not extract native symbols entry: $library" >&2
    exit 1
  }
  [ -s "$output_library" ] || { echo "native symbols archive has an empty library: $library" >&2; exit 1; }
  exported_symbols=$("$symbol_tool" -g --defined-only "$output_library") || {
    echo "could not inspect native symbols entry: $library" >&2
    exit 1
  }
  grep -E '[[:space:]]uniffi_peppy_mobile_bindings_' <<< "$exported_symbols" >/dev/null || {
    echo "native symbols archive has no exported UniFFI symbols: $library" >&2
    exit 1
  }
done

echo 'Android distribution manifests, composition, and native symbols verified'
