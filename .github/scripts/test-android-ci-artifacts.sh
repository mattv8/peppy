#!/usr/bin/env bash

set -Eeuo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
helper="$root/.github/scripts/android-ci-artifacts.sh"
workflow="$root/.github/workflows/test-android.yml"
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT

expect_failure() {
  if "$@"; then
    echo "expected command to fail: $*" >&2
    exit 1
  fi
}

fixture="$temp_dir/native-artifacts"
mkdir -p "$fixture/target/debug" \
  "$fixture/apps/android/app/src/main/jniLibs/arm64-v8a" \
  "$fixture/apps/android/app/src/main/jniLibs/x86_64" \
  "$fixture/apps/android/app/src/main/java/uniffi/peppy_mobile_bindings" \
  "$fixture/apps/android/googlemessages/build"
printf 'host binding\n' > "$fixture/target/debug/libpeppy_mobile_bindings.so"
printf 'arm64 binding\n' > "$fixture/apps/android/app/src/main/jniLibs/arm64-v8a/libpeppy_mobile_bindings.so"
printf 'x86_64 binding\n' > "$fixture/apps/android/app/src/main/jniLibs/x86_64/libpeppy_mobile_bindings.so"
printf 'generated Kotlin\n' > "$fixture/apps/android/app/src/main/java/uniffi/peppy_mobile_bindings/peppy_mobile_bindings.kt"
printf 'AAR\n' > "$fixture/apps/android/googlemessages/build/gmlink.aar"
printf 'provenance\n' > "$fixture/apps/android/googlemessages/build/gmlink.aar.provenance"

revision=deadbeefdeadbeefdeadbeefdeadbeefdeadbeef
manifest="$fixture/android-native-artifacts.manifest"
"$helper" write-manifest "$fixture" "$manifest" "$revision" debug
"$helper" validate-manifest "$fixture" "$manifest" "$revision" debug
expect_failure "$helper" validate-manifest "$fixture" "$manifest" fedcba9876543210fedcba9876543210fedcba98 debug
printf 'stale\n' >> "$fixture/apps/android/googlemessages/build/gmlink.aar"
expect_failure "$helper" validate-manifest "$fixture" "$manifest" "$revision" debug

producer="$temp_dir/producer"
checkout="$temp_dir/checkout"
mkdir -p "$producer" "$checkout"
cp -R "$fixture/apps" "$fixture/target" "$producer/"
rm -f "$producer/unlisted-output"
"$helper" package-artifact "$producer" "$temp_dir/upload/android-native-artifacts" "$revision" debug
mkdir -p "$temp_dir/download"
cp -R "$temp_dir/upload/android-native-artifacts" "$temp_dir/download/"
"$helper" install-artifact "$temp_dir/download/android-native-artifacts" "$checkout" "$revision" debug
cmp "$producer/target/debug/libpeppy_mobile_bindings.so" "$checkout/target/debug/libpeppy_mobile_bindings.so"
cmp "$producer/apps/android/googlemessages/build/gmlink.aar" "$checkout/apps/android/googlemessages/build/gmlink.aar"
"$helper" write-manifest "$fixture" "$manifest" "$revision" debug
printf 'unexpected\n' > "$fixture/unlisted-output"
expect_failure "$helper" validate-manifest "$fixture" "$manifest" "$revision" debug

grep -Fqx 'on:' "$workflow"
grep -Fq 'workflow_call:' "$workflow"
grep -Fq 'android-native-preparation:' "$workflow"
grep -Fq 'android-debug-unit-tests:' "$workflow"
grep -Fq 'android-sideload-unit-tests:' "$workflow"
grep -Fq 'android-lint-and-assembly:' "$workflow"
grep -Fq 'android-aggregate:' "$workflow"
# shellcheck disable=SC2016
grep -Fq 'if: ${{ always() }}' "$workflow"
grep -Fq ':app:testDebugUnitTest' "$workflow"
grep -Fq ':app:testSideloadUnitTest' "$workflow"
grep -Fq ':app:lintSideload' "$workflow"
grep -Fq ':app:testReleaseUnitTest' "$workflow"
# shellcheck disable=SC2016
grep -Fq 'PREPARATION_RESULT: ${{ needs.android-native-preparation.result }}' "$workflow"
# shellcheck disable=SC2016
grep -Fq 'DEBUG_RESULT: ${{ needs.android-debug-unit-tests.result }}' "$workflow"
# shellcheck disable=SC2016
grep -Fq 'SIDELOAD_RESULT: ${{ needs.android-sideload-unit-tests.result }}' "$workflow"
# shellcheck disable=SC2016
grep -Fq 'LINT_ASSEMBLY_RESULT: ${{ needs.android-lint-and-assembly.result }}' "$workflow"

job_block() {
  awk -v job="$1" '
    $0 == "  " job ":" { selected = 1 }
    selected && $0 ~ /^  [a-z][a-z-]*:$/ && $0 != "  " job ":" { exit }
    selected { print }
  ' "$workflow"
}

for job in android-debug-unit-tests android-sideload-unit-tests android-lint-and-assembly; do
  block=$(job_block "$job")
  grep -Fq 'path: android-native-artifacts' <<< "$block"
  # shellcheck disable=SC2016
  grep -Fq 'install-artifact android-native-artifacts . "$GITHUB_SHA" debug' <<< "$block"
  grep -Fq 'platforms;android-37.0' <<< "$block"
  grep -Fq 'build-tools;36.0.0' <<< "$block"
  grep -Fq 'ndk;27.2.12479018' <<< "$block"
  grep -Fq 'build-essential pkg-config libssl-dev libsodium-dev' <<< "$block"
  # shellcheck disable=SC2016
  grep -Fq 'sdkmanager_status=${PIPESTATUS[1]}' <<< "$block"
done

debug_block=$(job_block android-debug-unit-tests)
sideload_block=$(job_block android-sideload-unit-tests)
lint_block=$(job_block android-lint-and-assembly)
if grep -Fq 'peppyGoogleMessagesLink' <<< "$debug_block" || grep -Fq 'peppyGoogleMessagesLink' <<< "$sideload_block"; then
  echo 'debug and sideload unit test jobs must use the default-off Google Messages link setting' >&2
  exit 1
fi
test "$(grep -Foc -- '-PpeppyGoogleMessagesLink=true' <<< "$lint_block")" -eq 2
if grep -Fq 'peppyGoogleMessagesLink=false' "$workflow"; then
  echo 'workflow must not pass a default-off Google Messages link flag' >&2
  exit 1
fi

trusted_cache_guard="github.event_name == 'push' && github.ref == 'refs/heads/production'"
test "$(grep -Foc "$trusted_cache_guard" "$workflow")" -eq 3
test "$(grep -Foc 'actions/cache/save@' "$workflow")" -eq 1
test "$(grep -Foc 'actions/cache/restore@' "$workflow")" -eq 4

# shellcheck disable=SC2016
test "$(grep -Fxc '          name: android-native-${{ github.sha }}' "$workflow")" -eq 4
# shellcheck disable=SC2016
if grep -Fq 'android-native-${{ github.sha }}-${{ github.run_attempt }}' "$workflow"; then
  echo 'native prerequisite artifact names must survive consumer-only reruns' >&2
  exit 1
fi
producer_block=$(job_block android-native-preparation)
grep -Fq 'overwrite: true' <<< "$producer_block"

echo 'Android CI artifact tests passed'
