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
for argument in "$@"; do
  case "$argument" in --bundle=*) bundle=${argument#--bundle=} ;; esac
done
cat "${bundle}.manifest.xml"
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
touch "$direct" "$play" "$root/bundletool.jar"
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
