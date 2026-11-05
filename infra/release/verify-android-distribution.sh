#!/usr/bin/env bash
set -euo pipefail
set +x

[ "$#" -eq 3 ] || {
  echo "usage: $0 <direct-apk> <play-aab> <native-symbols-zip>" >&2
  exit 2
}

direct_apk=$1
play_aab=$2
symbols_archive=$3
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

echo 'Android distribution manifests and native symbols verified'
