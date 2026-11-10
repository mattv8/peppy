#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
notice_content=$(cat "$script_dir/../../apps/android/app/src/gmlinkOn/assets/third_party/NOTICE-google-messages.txt")
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
    --mode=universal) mode=universal ;;
    --bundle=*) bundle=${argument#--bundle=} ;;
    --output=*) output=${argument#--output=} ;;
  esac
done
if [ "$mode" = dump ]; then
  cat "${bundle}.manifest.xml"
elif [ "$mode" = universal ]; then
  printf '%s\n' universal >> "$BUNDLETOOL_CALLS"
  cp "${bundle}.apks" "$output"
else
  printf '%s\n' default >> "$BUNDLETOOL_CALLS"
  cp "${bundle}.split.apks" "$output"
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
  local output=$1
  shift
  python3 - "$output" "$@" <<'PY'
import pathlib
import sys
import zipfile

with zipfile.ZipFile(sys.argv[1], "w") as output:
    for item in sys.argv[2:]:
        name, apk = item.split("=", 1)
        output.write(apk, name)
PY
}

license_assets=$(python3 - "$script_dir/../../apps/android/googlemessages/license-inventory.json" <<'PY'
import json
import sys

inventory = json.load(open(sys.argv[1]))
assets = {
    asset
    for module in inventory["modules"]
    if module["distributed"]
    for asset in (module.get("asset"), module.get("licenseExceptions", {}).get("asset"))
    if asset
}
print(*sorted(assets), sep="\n")
PY
)

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

verify_distribution() {
  env PEPPY_GOOGLE_MESSAGES_LINK_EXPECTED="$1" ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
    "$script_dir/verify-android-distribution.sh" "$2" "$3" "$4"
}

make_play_bundle() {
  local output=$1
  shift
  write_archive "$output" "$@"
  write_manifest "$output.manifest.xml" play
  write_apks "$output.apks" "universal.apk=$clean_play_apk"
  write_apks "$output.split.apks" \
    "splits/base-master.apk=$clean_split_apk" \
    "standalones/standalone.apk=$clean_standalone_apk"
}

link_entries_without() {
  local excluded=$1 entry
  without_link_entries=()
  for entry in "${play_link_entries[@]}"; do
    [ "${entry%%=*}" = "$excluded" ] || without_link_entries+=("$entry")
  done
}

direct="$root/direct.apk"
symbols="$root/native-debug-symbols.zip"
touch "$root/bundletool.jar"
direct_entries=(
  'lib/arm64-v8a/libgojni.so=fixture'
  'lib/x86_64/libgojni.so=fixture'
  'classes.dex=Ldev/peppy/gmlink/Gmlink;'
  "assets/third_party/NOTICE-google-messages.txt=$notice_content"
)
for asset in $license_assets; do
  direct_entries+=("assets/third_party/$asset=fixture")
done
write_archive "$direct" "${direct_entries[@]}"
write_manifest "$direct.manifest.xml" direct

clean_play_apk="$root/clean-play.apk"
write_archive "$clean_play_apk" 'lib/arm64-v8a/libpeppy_mobile_bindings.so=fixture'
clean_split_apk="$root/clean-split.apk"
clean_standalone_apk="$root/clean-standalone.apk"
write_archive "$clean_split_apk" 'lib/arm64-v8a/libpeppy_mobile_bindings.so=fixture'
write_archive "$clean_standalone_apk" 'lib/x86_64/libpeppy_mobile_bindings.so=fixture'

play_link_entries=(
  'base/lib/arm64-v8a/libgojni.so=fixture'
  'base/lib/x86_64/libgojni.so=fixture'
  'base/lib/arm64-v8a/libpeppy_mobile_bindings.so=fixture'
  'base/dex/classes.dex=Ldev/peppy/gmlink/Gmlink;Ldev/peppy/mobile/restrictedsetup/RestrictedSetupEntryKt;'
  'base/assets/third_party/AGPL-3.0.txt=GNU AFFERO GENERAL PUBLIC LICENSE'
  "base/assets/third_party/NOTICE-google-messages.txt=$notice_content"
)
for asset in $license_assets; do
  case "$asset" in
    AGPL-3.0.txt|NOTICE-google-messages.txt) continue ;;
  esac
  play_link_entries+=("base/assets/third_party/$asset=fixture")
done

play="$root/play.aab"
make_play_bundle "$play" "${play_link_entries[@]}"
clean_play="$root/clean-play.aab"
make_play_bundle "$clean_play" 'base/lib/arm64-v8a/libpeppy_mobile_bindings.so=fixture'

printf fixture > "$root/symbols/arm64-v8a/libpeppy_mobile_bindings.so.sym"
printf fixture > "$root/symbols/x86_64/libpeppy_mobile_bindings.so.sym"
(cd "$root/symbols" && zip -q "$symbols" arm64-v8a/libpeppy_mobile_bindings.so.sym x86_64/libpeppy_mobile_bindings.so.sym)

expect_failure_reason 'BUNDLETOOL_JAR must name the verified bundletool JAR' \
  env -u BUNDLETOOL_JAR PEPPY_GOOGLE_MESSAGES_LINK_EXPECTED=true ANDROID_HOME="$sdk" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$symbols"
expect_failure_reason 'BUNDLETOOL_JAR must name the verified bundletool JAR' \
  env PEPPY_GOOGLE_MESSAGES_LINK_EXPECTED=true ANDROID_HOME="$sdk" BUNDLETOOL_JAR= PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$symbols"
expect_failure_reason 'BUNDLETOOL_JAR must name the verified bundletool JAR' \
  env PEPPY_GOOGLE_MESSAGES_LINK_EXPECTED=true ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/missing-bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$symbols"
expect_failure_reason 'PEPPY_GOOGLE_MESSAGES_LINK_EXPECTED must be true or false' \
  env ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct" "$play" "$symbols"

export BUNDLETOOL_CALLS="$root/bundletool-calls"
verify_distribution true "$direct" "$play" "$symbols"
grep -qx universal "$BUNDLETOOL_CALLS"
grep -qx default "$BUNDLETOOL_CALLS"

clean_direct="$root/clean-direct.apk"
write_archive "$clean_direct" 'lib/arm64-v8a/libpeppy_mobile_bindings.so=fixture'
cp "$direct.manifest.xml" "$clean_direct.manifest.xml"
verify_distribution false "$clean_direct" "$clean_play" "$symbols"
expect_failure_reason 'direct APK contains libgojni.so' \
  verify_distribution false "$direct" "$clean_play" "$symbols"

missing_direct_library="$root/missing-direct-library.apk"
write_archive "$missing_direct_library" \
  'lib/arm64-v8a/libgojni.so=fixture' \
  'classes.dex=Ldev/peppy/gmlink/Gmlink;' \
  'assets/third_party/AGPL-3.0.txt=GNU AFFERO GENERAL PUBLIC LICENSE' \
  "assets/third_party/NOTICE-google-messages.txt=$notice_content"
cp "$direct.manifest.xml" "$missing_direct_library.manifest.xml"
expect_failure_reason 'direct APK is missing lib/x86_64/libgojni.so' \
  env PEPPY_GOOGLE_MESSAGES_LINK_EXPECTED=true ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$missing_direct_library" "$play" "$symbols"

direct_without_dex="$root/direct-without-dex.apk"
write_archive "$direct_without_dex" \
  'lib/arm64-v8a/libgojni.so=fixture' \
  'lib/x86_64/libgojni.so=fixture' \
  'assets/third_party/AGPL-3.0.txt=GNU AFFERO GENERAL PUBLIC LICENSE' \
  "assets/third_party/NOTICE-google-messages.txt=$notice_content"
cp "$direct.manifest.xml" "$direct_without_dex.manifest.xml"
expect_failure_reason 'direct APK is missing dev/peppy/gmlink/ dex classes' \
  env PEPPY_GOOGLE_MESSAGES_LINK_EXPECTED=true ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct_without_dex" "$play" "$symbols"

direct_without_agpl="$root/direct-without-agpl.apk"
write_archive "$direct_without_agpl" \
  'lib/arm64-v8a/libgojni.so=fixture' \
  'lib/x86_64/libgojni.so=fixture' \
  'classes.dex=Ldev/peppy/gmlink/Gmlink;' \
  "assets/third_party/NOTICE-google-messages.txt=$notice_content"
cp "$direct.manifest.xml" "$direct_without_agpl.manifest.xml"
expect_failure_reason 'direct APK is missing assets/third_party/AGPL-3.0.txt' \
  env PEPPY_GOOGLE_MESSAGES_LINK_EXPECTED=true ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct_without_agpl" "$play" "$symbols"

direct_without_notice="$root/direct-without-notice.apk"
write_archive "$direct_without_notice" \
  'lib/arm64-v8a/libgojni.so=fixture' \
  'lib/x86_64/libgojni.so=fixture' \
  'classes.dex=Ldev/peppy/gmlink/Gmlink;' \
  'assets/third_party/AGPL-3.0.txt=GNU AFFERO GENERAL PUBLIC LICENSE'
cp "$direct.manifest.xml" "$direct_without_notice.manifest.xml"
expect_failure_reason 'direct APK is missing assets/third_party/NOTICE-google-messages.txt' \
  env PEPPY_GOOGLE_MESSAGES_LINK_EXPECTED=true ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct_without_notice" "$play" "$symbols"

direct_without_mpl="$root/direct-without-mpl.apk"
missing_mpl_entries=()
for entry in "${direct_entries[@]}"; do
  [ "$entry" = 'assets/third_party/MPL-2.0.txt=fixture' ] || missing_mpl_entries+=("$entry")
done
write_archive "$direct_without_mpl" "${missing_mpl_entries[@]}"
cp "$direct.manifest.xml" "$direct_without_mpl.manifest.xml"
expect_failure_reason 'direct APK is missing assets/third_party/MPL-2.0.txt' \
  env PEPPY_GOOGLE_MESSAGES_LINK_EXPECTED=true ANDROID_HOME="$sdk" BUNDLETOOL_JAR="$root/bundletool.jar" PATH="$bin:$PATH" \
  "$script_dir/verify-android-distribution.sh" "$direct_without_mpl" "$play" "$symbols"

# Trusted Play AAB must carry the Google Messages link; each required piece is checked.
expect_missing_play_link() {
  local excluded=$1 reason=$2 bundle="$root/play-missing-${1//[^A-Za-z0-9]/-}.aab"
  link_entries_without "$excluded"
  make_play_bundle "$bundle" "${without_link_entries[@]}"
  expect_failure_reason "$reason" verify_distribution true "$direct" "$bundle" "$symbols"
}
expect_missing_play_link base/lib/x86_64/libgojni.so 'Play AAB is missing lib/x86_64/libgojni.so'
expect_missing_play_link base/dex/classes.dex 'Play AAB is missing dev/peppy/gmlink/ dex classes'
expect_missing_play_link base/assets/third_party/AGPL-3.0.txt 'Play AAB is missing assets/third_party/AGPL-3.0.txt'
expect_missing_play_link base/assets/third_party/NOTICE-google-messages.txt 'Play AAB is missing assets/third_party/NOTICE-google-messages.txt'
expect_missing_play_link base/assets/third_party/MPL-2.0.txt 'Play AAB is missing assets/third_party/MPL-2.0.txt'

# Restricted setup content never reaches the Play AAB or its APKs.
restricted_dex_bundle="$root/play-restricted-dex.aab"
make_play_bundle "$restricted_dex_bundle" "${play_link_entries[@]}" \
  'base/dex/classes2.dex=Ldev/peppy/mobile/restrictedsetup/RestrictedSetupController;'
expect_failure_reason 'Play AAB contains restricted setup dex classes' \
  verify_distribution true "$direct" "$restricted_dex_bundle" "$symbols"

restricted_native_bundle="$root/play-restricted-native.aab"
make_play_bundle "$restricted_native_bundle" "${play_link_entries[@]}" \
  'base/lib/arm64-v8a/libadb.so=fixture'
expect_failure_reason 'Play AAB contains restricted setup native libraries' \
  verify_distribution true "$direct" "$restricted_native_bundle" "$symbols"

restricted_notice_bundle="$root/play-restricted-notice.aab"
make_play_bundle "$restricted_notice_bundle" "${play_link_entries[@]}" \
  'base/assets/third_party/NOTICE.txt=fixture'
expect_failure_reason 'Play AAB contains unexpected third-party asset' \
  verify_distribution true "$direct" "$restricted_notice_bundle" "$symbols"

# Nested restricted notices must not evade the third-party asset inventory in either flag state.
for link_expected in true false; do
  if [ "$link_expected" = true ]; then
    fixture_direct=$direct
    fixture_entries=("${play_link_entries[@]}")
  else
    fixture_direct=$clean_direct
    fixture_entries=('base/lib/arm64-v8a/libpeppy_mobile_bindings.so=fixture')
  fi
  nested_aab="$root/play-nested-notice-$link_expected.aab"
  make_play_bundle "$nested_aab" "${fixture_entries[@]}" \
    'base/assets/third_party/nested/LGPL-2.1.txt=fixture'
  expect_failure_reason 'Play AAB contains unexpected third-party asset' \
    verify_distribution "$link_expected" "$fixture_direct" "$nested_aab" "$symbols"

  nested_apk="$root/play-nested-notice-$link_expected.apk"
  write_archive "$nested_apk" 'assets/third_party/nested/NOTICE.txt=fixture'
  nested_apk_bundle="$root/play-nested-notice-apk-$link_expected.aab"
  make_play_bundle "$nested_apk_bundle" "${fixture_entries[@]}"
  write_apks "$nested_apk_bundle.apks" "universal.apk=$nested_apk"
  expect_failure_reason 'Play APK universal.apk contains unexpected third-party asset' \
    verify_distribution "$link_expected" "$fixture_direct" "$nested_apk_bundle" "$symbols"
done

restricted_apk="$root/play-restricted.apk"
write_archive "$restricted_apk" 'classes.dex=Ldev/peppy/mobile/restrictedsetup/RestrictedSetupController;'
restricted_apk_bundle="$root/play-restricted-apk.aab"
make_play_bundle "$restricted_apk_bundle" "${play_link_entries[@]}"
write_apks "$restricted_apk_bundle.apks" "universal.apk=$restricted_apk"
expect_failure_reason 'Play APK universal.apk contains restricted setup dex classes' \
  verify_distribution true "$direct" "$restricted_apk_bundle" "$symbols"

# Under the link, only the exact reviewed notice names may carry Google Messages license text.
renamed_notice_bundle="$root/play-renamed-notice.aab"
make_play_bundle "$renamed_notice_bundle" "${play_link_entries[@]}" \
  "base/assets/renamed-notice.txt=$notice_content"
expect_failure_reason 'Play AAB contains unexpected Google Messages license text' \
  verify_distribution true "$direct" "$renamed_notice_bundle" "$symbols"

agpl_apk="$root/play-agpl.apk"
write_archive "$agpl_apk" 'assets/AGPL-copy.txt=GNU AFFERO GENERAL PUBLIC LICENSE'
agpl_bundle="$root/play-agpl.aab"
make_play_bundle "$agpl_bundle" "${play_link_entries[@]}"
write_apks "$agpl_bundle.apks" "universal.apk=$agpl_apk"
expect_failure_reason 'Play APK universal.apk contains unexpected Google Messages license text' \
  verify_distribution true "$direct" "$agpl_bundle" "$symbols"

other_notice_apk="$root/play-other-notice.apk"
write_archive "$other_notice_apk" 'assets/other/NOTICE-google-messages.txt=fixture'
other_notice_bundle="$root/play-other-notice.aab"
make_play_bundle "$other_notice_bundle" "${play_link_entries[@]}"
write_apks "$other_notice_bundle.split.apks" "splits/base-master.apk=$other_notice_apk"
expect_failure_reason 'Play APK base-master.apk contains unexpected Google Messages license text' \
  verify_distribution true "$direct" "$other_notice_bundle" "$symbols"

allowed_notice_apk="$root/play-allowed-notice.apk"
write_archive "$allowed_notice_apk" "assets/third_party/NOTICE-google-messages.txt=$notice_content"
allowed_notice_bundle="$root/play-allowed-notice.aab"
make_play_bundle "$allowed_notice_bundle" "${play_link_entries[@]}"
write_apks "$allowed_notice_bundle.apks" "universal.apk=$allowed_notice_apk"
verify_distribution true "$direct" "$allowed_notice_bundle" "$symbols"

# Flag-off Play distribution stays link-free.
expect_failure_reason 'Play AAB contains libgojni.so' \
  verify_distribution false "$clean_direct" "$play" "$symbols"

play_with_dex="$root/play-with-dex.aab"
make_play_bundle "$play_with_dex" 'base/dex/classes.dex=Ldev/peppy/gmlink/Gmlink;'
expect_failure_reason 'Play AAB contains dev/peppy/gmlink/ dex classes' \
  verify_distribution false "$clean_direct" "$play_with_dex" "$symbols"

play_split_with_link="$root/play-split-with-link.apk"
write_archive "$play_split_with_link" 'lib/arm64-v8a/libgojni.so=fixture'
play_with_split_link="$root/play-with-split-link.aab"
make_play_bundle "$play_with_split_link" 'base/lib/arm64-v8a/libpeppy_mobile_bindings.so=fixture'
write_apks "$play_with_split_link.split.apks" "splits/base-master.apk=$play_split_with_link"
expect_failure_reason 'Play APK base-master.apk contains libgojni.so' \
  verify_distribution false "$clean_direct" "$play_with_split_link" "$symbols"

play_standalone_with_dex="$root/play-standalone-with-dex.apk"
write_archive "$play_standalone_with_dex" 'classes.dex=Ldev/peppy/gmlink/Gmlink;'
play_with_standalone_dex="$root/play-with-standalone-dex.aab"
make_play_bundle "$play_with_standalone_dex" 'base/lib/arm64-v8a/libpeppy_mobile_bindings.so=fixture'
write_apks "$play_with_standalone_dex.split.apks" "standalones/standalone.apk=$play_standalone_with_dex"
expect_failure_reason 'Play APK standalone.apk contains dev/peppy/gmlink/ dex classes' \
  verify_distribution false "$clean_direct" "$play_with_standalone_dex" "$symbols"

for restricted_notice in NOTICE.txt LGPL-2.1.txt GPL-3.0.txt BoringSSL-LICENSE.txt; do
  restricted_notice_bundle="$root/play-flag-off-$restricted_notice.aab"
  make_play_bundle "$restricted_notice_bundle" \
    'base/lib/arm64-v8a/libpeppy_mobile_bindings.so=fixture' \
    "base/assets/third_party/$restricted_notice=fixture"
  expect_failure_reason 'Play AAB contains unexpected third-party asset' \
    verify_distribution false "$clean_direct" "$restricted_notice_bundle" "$symbols"
done

for notice_kind in filename renamed; do
  if [ "$notice_kind" = filename ]; then
    notice_entry='assets/third_party/NOTICE-google-messages.txt='
  else
    notice_entry="assets/renamed-notice.txt=$notice_content"
  fi
  for artifact_kind in aab universal split standalone; do
    contaminated_bundle="$root/notice-$notice_kind-$artifact_kind.aab"
    make_play_bundle "$contaminated_bundle" 'base/lib/arm64-v8a/libpeppy_mobile_bindings.so=fixture'
    if [ "$artifact_kind" = aab ]; then
      write_archive "$contaminated_bundle" "base/$notice_entry"
      expected_notice_label='Play AAB'
    else
      contaminated_apk="$root/notice-$notice_kind-$artifact_kind.apk"
      write_archive "$contaminated_apk" "$notice_entry"
      case "$artifact_kind" in
        universal)
          write_apks "$contaminated_bundle.apks" "universal.apk=$contaminated_apk"
          expected_notice_label='Play APK universal.apk'
          ;;
        split)
          write_apks "$contaminated_bundle.split.apks" "splits/base-master.apk=$contaminated_apk"
          expected_notice_label='Play APK base-master.apk'
          ;;
        standalone)
          write_apks "$contaminated_bundle.split.apks" "standalones/standalone.apk=$contaminated_apk"
          expected_notice_label='Play APK standalone.apk'
          ;;
      esac
    fi
    expect_failure_reason "$expected_notice_label contains Google Messages notice content" \
      verify_distribution false "$clean_direct" "$contaminated_bundle" "$symbols"
  done
done

write_manifest "$play.manifest.xml" direct
expect_failure_reason "play manifest distribution metadata must be exactly 'play'" \
  verify_distribution true "$direct" "$play" "$symbols"

write_manifest "$play.manifest.xml" play
write_missing_metadata_manifest "$direct.manifest.xml"
expect_failure_reason "direct manifest distribution metadata must be exactly 'direct'" \
  verify_distribution true "$direct" "$play" "$symbols"

write_duplicate_metadata_manifest "$direct.manifest.xml" direct
expect_failure_reason "direct manifest distribution metadata must be exactly 'direct'" \
  verify_distribution true "$direct" "$play" "$symbols"

write_manifest "$direct.manifest.xml" direct
missing_abi_symbols="$root/missing-abi-symbols.zip"
(cd "$root/symbols" && zip -q "$missing_abi_symbols" arm64-v8a/libpeppy_mobile_bindings.so.sym)
expect_failure_reason 'native symbols archive is missing x86_64 peppy mobile bindings symbols' \
  verify_distribution true "$direct" "$play" "$missing_abi_symbols"

empty_symbols="$root/empty-symbols.zip"
mkdir -p "$root/empty-symbols/arm64-v8a" "$root/empty-symbols/x86_64"
: > "$root/empty-symbols/arm64-v8a/libpeppy_mobile_bindings.so.sym"
printf fixture > "$root/empty-symbols/x86_64/libpeppy_mobile_bindings.so.sym"
(cd "$root/empty-symbols" && zip -q "$empty_symbols" arm64-v8a/libpeppy_mobile_bindings.so.sym x86_64/libpeppy_mobile_bindings.so.sym)
expect_failure_reason 'native symbols archive has an empty library: arm64-v8a/libpeppy_mobile_bindings.so.sym' \
  verify_distribution true "$direct" "$play" "$empty_symbols"

missing_symbol_archive="$root/missing-symbol.zip"
mkdir -p "$root/missing-symbol/arm64-v8a" "$root/missing-symbol/x86_64"
printf no-symbol > "$root/missing-symbol/arm64-v8a/libpeppy_mobile_bindings.so.sym"
printf no-symbol > "$root/missing-symbol/x86_64/libpeppy_mobile_bindings.so.sym"
(cd "$root/missing-symbol" && zip -q "$missing_symbol_archive" arm64-v8a/libpeppy_mobile_bindings.so.sym x86_64/libpeppy_mobile_bindings.so.sym)
expect_failure_reason 'native symbols archive has no exported UniFFI symbols: arm64-v8a/libpeppy_mobile_bindings.so.sym' \
  verify_distribution true "$direct" "$play" "$missing_symbol_archive"

corrupt_symbols="$root/corrupt-symbols.zip"
printf corrupt > "$corrupt_symbols"
expect_failure_reason 'could not list native symbols archive:' \
  verify_distribution true "$direct" "$play" "$corrupt_symbols"

printf 'android distribution verification tests passed\n'
