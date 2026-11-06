#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(mktemp -d "${TMPDIR:-/tmp}/peppy-android-distribution-test.XXXXXX")
trap 'rm -rf "$root"' EXIT

sdk="$root/sdk"
bin="$root/bin"
mkdir -p "$sdk/cmdline-tools/latest/bin" "$bin" "$root/symbols/arm64-v8a" "$root/symbols/x86_64"

cat > "$sdk/cmdline-tools/latest/bin/apkanalyzer" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[ "$1" = manifest ] && [ "$2" = print ]
cat "${3}.manifest.xml"
SH
cat > "$bin/java" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
mode=
for argument in "$@"; do
  case "$argument" in
    dump) mode=dump ;;
    --bundle=*) bundle=${argument#--bundle=} ;;
    --output=*) output=${argument#--output=} ;;
  esac
done
if [ "$mode" = dump ]; then
  cat "${bundle}.manifest.xml"
else
  cp "${bundle}.apks" "$output"
fi
SH
cat > "$bin/llvm-nm" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if grep -q no-symbol "$3"; then
  printf '00000000 T unrelated_fixture\n'
else
  printf '00000000 T uniffi_peppy_mobile_bindings_fixture\n'
fi
SH
chmod +x "$sdk/cmdline-tools/latest/bin/apkanalyzer" "$bin/java" "$bin/llvm-nm"

write_manifest() {
  local output=$1 distribution=$2
  cat > "$output" <<XML
<manifest xmlns:android="http://schemas.android.com/apk/res/android"><application><meta-data android:name="dev.peppy.mobile.distribution" android:value="$distribution" /></application></manifest>
XML
}

write_duplicate_metadata_manifest() {
  local output=$1 distribution=$2
  cat > "$output" <<XML
<manifest xmlns:android="http://schemas.android.com/apk/res/android"><application><meta-data android:name="dev.peppy.mobile.distribution" android:value="$distribution" /><meta-data android:name="dev.peppy.mobile.distribution" android:value="$distribution" /></application></manifest>
XML
}

write_missing_metadata_manifest() {
  local output=$1
  cat > "$output" <<'XML'
<manifest><application /></manifest>
XML
}

write_archive() {
  local output=$1
  shift
  python3 - "$output" "$@" <<'PY'
import pathlib
import sys
import zipfile

archive = pathlib.Path(sys.argv[1])
with zipfile.ZipFile(archive, "w") as output:
    for item in sys.argv[2:]:
        name, content = item.split("=", 1)
        output.writestr(name, content.encode())
PY
}

write_apks() {
  local output=$1 apk=$2
  python3 - "$output" "$apk" <<'PY'
import pathlib
import sys
import zipfile

with zipfile.ZipFile(sys.argv[1], "w") as output:
    output.write(sys.argv[2], "universal.apk")
PY
}

expect_failure_reason() {
  local expected=$1
  shift
  local output
  if output=$("$@" 2>&1); then
    printf 'expected command to fail: %s\n' "$*" >&2
    exit 1
  fi
  if [[ $output != *"$expected"* ]]; then
    printf 'expected failure reason %q, got:\n%s\n' "$expected" "$output" >&2
    exit 1
  fi
}

direct="$root/direct.apk"
play="$root/play.aab"
symbols="$root/native-debug-symbols.zip"
touch "$root/bundletool.jar"
write_archive "$direct" \
  'lib/arm64-v8a/libgojni.so=fixture' \
  'lib/x86_64/libgojni.so=fixture' \
  'classes.dex=Ldev/peppy/gmlink/Gmlink;' \
  'assets/third_party/AGPL-3.0.txt=GNU AFFERO GENERAL PUBLIC LICENSE' \
  'assets/third_party/NOTICE-google-messages.txt=Google Messages link notices'
write_archive "$play" 'base/lib/arm64-v8a/libpeppy_mobile_bindings.so=fixture'
clean_play_apk="$root/clean-play.apk"
write_archive "$clean_play_apk" 'lib/arm64-v8a/libpeppy_mobile_bindings.so=fixture'
write_apks "$play.apks" "$clean_play_apk"
write_manifest "$direct.manifest.xml" direct
write_manifest "$play.manifest.xml" play
printf fixture > "$root/symbols/arm64-v8a/libpeppy_mobile_bindings.so.sym"
printf fixture > "$root/symbols/x86_64/libpeppy_mobile_bindings.so.sym"
(cd "$root/symbols" && zip -q "$symbols" arm64-v8a/libpeppy_mobile_bindings.so.sym x86_64/libpeppy_mobile_bindings.so.sym)

expect_failure_reason 'BUNDLETOOL_JAR must name the verified bundletool JAR' \
  env -u BUNDLETOOL_JAR ANDROID_HOME="$sdk" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$symbols"
expect_failure_reason 'BUNDLETOOL_JAR must name the verified bundletool JAR' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR= PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$symbols"
expect_failure_reason 'BUNDLETOOL_JAR must name the verified bundletool JAR' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/missing-bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$symbols"

env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$symbols"

missing_direct_library="$root/missing-direct-library.apk"
write_archive "$missing_direct_library" \
  'lib/arm64-v8a/libgojni.so=fixture' \
  'classes.dex=Ldev/peppy/gmlink/Gmlink;' \
  'assets/third_party/AGPL-3.0.txt=GNU AFFERO GENERAL PUBLIC LICENSE' \
  'assets/third_party/NOTICE-google-messages.txt=Google Messages link notices'
cp "$direct.manifest.xml" "$missing_direct_library.manifest.xml"
expect_failure_reason 'direct APK is missing lib/x86_64/libgojni.so' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$missing_direct_library" "$play" "$symbols"

direct_without_dex="$root/direct-without-dex.apk"
write_archive "$direct_without_dex" \
  'lib/arm64-v8a/libgojni.so=fixture' \
  'lib/x86_64/libgojni.so=fixture' \
  'assets/third_party/AGPL-3.0.txt=GNU AFFERO GENERAL PUBLIC LICENSE' \
  'assets/third_party/NOTICE-google-messages.txt=Google Messages link notices'
cp "$direct.manifest.xml" "$direct_without_dex.manifest.xml"
expect_failure_reason 'direct APK is missing dev/peppy/gmlink/ dex classes' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct_without_dex" "$play" "$symbols"

direct_without_agpl="$root/direct-without-agpl.apk"
write_archive "$direct_without_agpl" \
  'lib/arm64-v8a/libgojni.so=fixture' \
  'lib/x86_64/libgojni.so=fixture' \
  'classes.dex=Ldev/peppy/gmlink/Gmlink;' \
  'assets/third_party/NOTICE-google-messages.txt=Google Messages link notices'
cp "$direct.manifest.xml" "$direct_without_agpl.manifest.xml"
expect_failure_reason 'direct APK is missing assets/third_party/AGPL-3.0.txt' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct_without_agpl" "$play" "$symbols"

direct_without_notice="$root/direct-without-notice.apk"
write_archive "$direct_without_notice" \
  'lib/arm64-v8a/libgojni.so=fixture' \
  'lib/x86_64/libgojni.so=fixture' \
  'classes.dex=Ldev/peppy/gmlink/Gmlink;' \
  'assets/third_party/AGPL-3.0.txt=GNU AFFERO GENERAL PUBLIC LICENSE'
cp "$direct.manifest.xml" "$direct_without_notice.manifest.xml"
expect_failure_reason 'direct APK is missing assets/third_party/NOTICE-google-messages.txt' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct_without_notice" "$play" "$symbols"

play_with_link="$root/play-with-link.aab"
write_archive "$play_with_link" 'base/lib/arm64-v8a/libgojni.so=fixture'
cp "$play.manifest.xml" "$play_with_link.manifest.xml"
cp "$play.apks" "$play_with_link.apks"
expect_failure_reason 'Play AAB contains libgojni.so' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play_with_link" "$symbols"

play_with_dex="$root/play-with-dex.aab"
write_archive "$play_with_dex" 'base/dex/classes.dex=Ldev/peppy/gmlink/Gmlink;'
cp "$play.manifest.xml" "$play_with_dex.manifest.xml"
cp "$play.apks" "$play_with_dex.apks"
expect_failure_reason 'Play AAB contains dev/peppy/gmlink/ dex classes' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play_with_dex" "$symbols"

play_apk_with_notice="$root/play-with-notice.apk"
write_archive "$play_apk_with_notice" 'assets/third_party/NOTICE-google-messages.txt=Google Messages link notices'
play_with_notice="$root/play-with-notice.aab"
write_archive "$play_with_notice" 'base/lib/arm64-v8a/libpeppy_mobile_bindings.so=fixture'
cp "$play.manifest.xml" "$play_with_notice.manifest.xml"
write_apks "$play_with_notice.apks" "$play_apk_with_notice"
expect_failure_reason 'Play APK universal.apk contains Google Messages notice content' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play_with_notice" "$symbols"

play_apk_with_agpl="$root/play-with-agpl.apk"
write_archive "$play_apk_with_agpl" 'assets/third_party/AGPL-3.0.txt=GNU AFFERO GENERAL PUBLIC LICENSE'
play_with_agpl="$root/play-with-agpl.aab"
write_archive "$play_with_agpl" 'base/lib/arm64-v8a/libpeppy_mobile_bindings.so=fixture'
cp "$play.manifest.xml" "$play_with_agpl.manifest.xml"
write_apks "$play_with_agpl.apks" "$play_apk_with_agpl"
expect_failure_reason 'Play APK universal.apk contains AGPL text' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play_with_agpl" "$symbols"

write_manifest "$play.manifest.xml" direct
expect_failure_reason "play manifest distribution metadata must be exactly 'play'" \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$symbols"

write_manifest "$play.manifest.xml" play
write_missing_metadata_manifest "$direct.manifest.xml"
expect_failure_reason "direct manifest distribution metadata must be exactly 'direct'" \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$symbols"

write_duplicate_metadata_manifest "$direct.manifest.xml" direct
expect_failure_reason "direct manifest distribution metadata must be exactly 'direct'" \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$symbols"

write_manifest "$direct.manifest.xml" direct
missing_abi_symbols="$root/missing-abi-symbols.zip"
(cd "$root/symbols" && zip -q "$missing_abi_symbols" arm64-v8a/libpeppy_mobile_bindings.so.sym)
expect_failure_reason 'native symbols archive is missing x86_64 peppy mobile bindings symbols' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$missing_abi_symbols"

empty_symbols="$root/empty-symbols.zip"
mkdir -p "$root/empty-symbols/arm64-v8a" "$root/empty-symbols/x86_64"
: > "$root/empty-symbols/arm64-v8a/libpeppy_mobile_bindings.so.sym"
printf fixture > "$root/empty-symbols/x86_64/libpeppy_mobile_bindings.so.sym"
(cd "$root/empty-symbols" && zip -q "$empty_symbols" arm64-v8a/libpeppy_mobile_bindings.so.sym x86_64/libpeppy_mobile_bindings.so.sym)
expect_failure_reason 'native symbols archive has an empty library: arm64-v8a/libpeppy_mobile_bindings.so.sym' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$empty_symbols"

missing_symbol_archive="$root/missing-symbol.zip"
mkdir -p "$root/missing-symbol/arm64-v8a" "$root/missing-symbol/x86_64"
printf no-symbol > "$root/missing-symbol/arm64-v8a/libpeppy_mobile_bindings.so.sym"
printf no-symbol > "$root/missing-symbol/x86_64/libpeppy_mobile_bindings.so.sym"
(cd "$root/missing-symbol" && zip -q "$missing_symbol_archive" arm64-v8a/libpeppy_mobile_bindings.so.sym x86_64/libpeppy_mobile_bindings.so.sym)
expect_failure_reason 'native symbols archive has no exported UniFFI symbols: arm64-v8a/libpeppy_mobile_bindings.so.sym' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$missing_symbol_archive"

corrupt_symbols="$root/corrupt-symbols.zip"
printf corrupt > "$corrupt_symbols"
expect_failure_reason 'could not list native symbols archive:' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$corrupt_symbols"

printf 'android distribution verification tests passed\n'
