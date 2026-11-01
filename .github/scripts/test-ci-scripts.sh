#!/usr/bin/env bash

set -Eeuo pipefail

unset GITHUB_STEP_SUMMARY

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCOPE="$SCRIPT_DIR/ci-scope.sh"
GATE="$SCRIPT_DIR/ci-gate.sh"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT

expect_output() {
  local file="$1"
  local expected="$2"

  if ! grep -Fqx "$expected" "$file"; then
    echo "missing output: $expected" >&2
    exit 1
  fi
}

expect_failure() {
  if "$@"; then
    echo "expected command to fail: $*" >&2
    exit 1
  fi
}

scope_cases=(force_full production_push docs_only rust_shared rust_mobile rust_desktop rust_browser rust_server android_bindings desktop web android ios mobile_design server build ci unclassified)
for case_name in "${scope_cases[@]}"; do
  output="$temp_dir/$case_name"
  : > "$output"
  case "$case_name" in
    force_full) env GITHUB_OUTPUT="$output" FILTER_FORCE_FULL=true "$SCOPE" ;;
    production_push) env GITHUB_OUTPUT="$output" FILTER_FORCE_FULL=true PRODUCTION_PUSH=true "$SCOPE" ;;
    docs_only) env GITHUB_OUTPUT="$output" "$SCOPE" ;;
    rust_shared) env GITHUB_OUTPUT="$output" FILTER_RUST_SHARED=true "$SCOPE" ;;
    rust_mobile) env GITHUB_OUTPUT="$output" FILTER_RUST_MOBILE=true "$SCOPE" ;;
    rust_desktop) env GITHUB_OUTPUT="$output" FILTER_RUST_DESKTOP=true "$SCOPE" ;;
    rust_browser) env GITHUB_OUTPUT="$output" FILTER_RUST_BROWSER=true "$SCOPE" ;;
    rust_server) env GITHUB_OUTPUT="$output" FILTER_RUST_SERVER=true "$SCOPE" ;;
    android_bindings) env GITHUB_OUTPUT="$output" FILTER_ANDROID_BINDINGS=true "$SCOPE" ;;
    desktop) env GITHUB_OUTPUT="$output" FILTER_DESKTOP=true "$SCOPE" ;;
    web) env GITHUB_OUTPUT="$output" FILTER_WEB=true "$SCOPE" ;;
    android) env GITHUB_OUTPUT="$output" FILTER_ANDROID=true "$SCOPE" ;;
    ios) env GITHUB_OUTPUT="$output" FILTER_IOS=true "$SCOPE" ;;
    mobile_design) env GITHUB_OUTPUT="$output" FILTER_ANDROID=true FILTER_IOS=true "$SCOPE" ;;
    server) env GITHUB_OUTPUT="$output" FILTER_SERVER=true "$SCOPE" ;;
    build) env GITHUB_OUTPUT="$output" FILTER_BUILD=true "$SCOPE" ;;
    ci) env GITHUB_OUTPUT="$output" FILTER_CI=true "$SCOPE" ;;
    unclassified) env GITHUB_OUTPUT="$output" FILTER_UNCLASSIFIED=true "$SCOPE" ;;
  esac
done

expect_output "$temp_dir/production_push" 'rust=true'
expect_output "$temp_dir/production_push" 'container_image=true'
expect_output "$temp_dir/production_push" 'artifacts_android=false'
expect_output "$temp_dir/production_push" 'artifacts_desktop=false'
expect_output "$temp_dir/production_push" 'artifacts_ios=false'
expect_output "$temp_dir/production_push" 'development_artifacts=false'

for case_name in "${scope_cases[@]}"; do
  output="$temp_dir/$case_name"
  if grep -Fqx 'integration=true' "$output"; then
    expect_output "$output" 'desktop=true'
  fi
  if grep -Fqx 'rust=true' "$output"; then
    expect_output "$output" 'integration=true'
  fi
done

for name in rust integration desktop android swift_bindings container_image artifacts_android artifacts_desktop artifacts_ios development_artifacts; do
  expect_output "$temp_dir/force_full" "$name=true"
  expect_output "$temp_dir/docs_only" "$name=false"
  expect_output "$temp_dir/ci" "$name=true"
  expect_output "$temp_dir/unclassified" "$name=true"
done

for case_name in force_full desktop rust_shared rust_desktop; do
  expect_output "$temp_dir/$case_name" 'desktop_matrix=["ubuntu-24.04","macos-15","windows-2025"]'
done
for case_name in server web rust_browser rust_server; do
  expect_output "$temp_dir/$case_name" 'desktop_matrix=["ubuntu-24.04"]'
done

for name in rust integration desktop android swift_bindings container_image artifacts_android artifacts_desktop artifacts_ios development_artifacts; do
  expect_output "$temp_dir/rust_shared" "$name=true"
done

expect_output "$temp_dir/rust_mobile" 'rust=true'
expect_output "$temp_dir/rust_mobile" 'integration=true'
expect_output "$temp_dir/rust_mobile" 'desktop=true'
expect_output "$temp_dir/rust_mobile" 'android=true'
expect_output "$temp_dir/rust_mobile" 'swift_bindings=true'
expect_output "$temp_dir/rust_mobile" 'artifacts_android=true'
expect_output "$temp_dir/rust_mobile" 'artifacts_desktop=false'
expect_output "$temp_dir/rust_mobile" 'artifacts_ios=true'
expect_output "$temp_dir/rust_desktop" 'rust=true'
expect_output "$temp_dir/rust_desktop" 'container_image=true'
expect_output "$temp_dir/rust_desktop" 'artifacts_desktop=true'
expect_output "$temp_dir/rust_desktop" 'artifacts_android=false'
expect_output "$temp_dir/rust_browser" 'rust=true'
expect_output "$temp_dir/rust_browser" 'container_image=true'
expect_output "$temp_dir/rust_browser" 'development_artifacts=false'
expect_output "$temp_dir/rust_server" 'rust=true'
expect_output "$temp_dir/rust_server" 'container_image=true'
expect_output "$temp_dir/rust_server" 'development_artifacts=false'
expect_output "$temp_dir/android_bindings" 'rust=false'
expect_output "$temp_dir/android_bindings" 'swift_bindings=true'
expect_output "$temp_dir/android_bindings" 'development_artifacts=false'
expect_output "$temp_dir/desktop" 'rust=false'
expect_output "$temp_dir/desktop" 'integration=true'
expect_output "$temp_dir/desktop" 'desktop=true'
expect_output "$temp_dir/desktop" 'android=false'
expect_output "$temp_dir/desktop" 'swift_bindings=false'
expect_output "$temp_dir/desktop" 'container_image=false'
expect_output "$temp_dir/desktop" 'artifacts_android=false'
expect_output "$temp_dir/desktop" 'artifacts_desktop=true'
expect_output "$temp_dir/desktop" 'artifacts_ios=false'
expect_output "$temp_dir/desktop" 'development_artifacts=true'
expect_output "$temp_dir/web" 'rust=false'
expect_output "$temp_dir/web" 'integration=false'
expect_output "$temp_dir/web" 'desktop=true'
expect_output "$temp_dir/web" 'container_image=true'
expect_output "$temp_dir/web" 'development_artifacts=false'
expect_output "$temp_dir/ios" 'rust=false'
expect_output "$temp_dir/ios" 'swift_bindings=true'
expect_output "$temp_dir/ios" 'artifacts_android=false'
expect_output "$temp_dir/ios" 'artifacts_desktop=false'
expect_output "$temp_dir/ios" 'artifacts_ios=true'
expect_output "$temp_dir/ios" 'development_artifacts=true'
expect_output "$temp_dir/android" 'rust=false'
expect_output "$temp_dir/android" 'android=true'
expect_output "$temp_dir/android" 'swift_bindings=false'
expect_output "$temp_dir/android" 'artifacts_android=true'
expect_output "$temp_dir/android" 'artifacts_desktop=false'
expect_output "$temp_dir/android" 'artifacts_ios=false'
expect_output "$temp_dir/android" 'development_artifacts=true'
expect_output "$temp_dir/mobile_design" 'desktop=false'
expect_output "$temp_dir/mobile_design" 'android=true'
expect_output "$temp_dir/mobile_design" 'swift_bindings=true'
expect_output "$temp_dir/mobile_design" 'artifacts_android=true'
expect_output "$temp_dir/mobile_design" 'artifacts_desktop=false'
expect_output "$temp_dir/mobile_design" 'artifacts_ios=true'
expect_output "$temp_dir/mobile_design" 'development_artifacts=true'
expect_output "$temp_dir/server" 'rust=false'
expect_output "$temp_dir/server" 'integration=true'
expect_output "$temp_dir/server" 'desktop=true'
expect_output "$temp_dir/server" 'container_image=true'
expect_output "$temp_dir/server" 'artifacts_android=false'
expect_output "$temp_dir/server" 'artifacts_desktop=false'
expect_output "$temp_dir/server" 'artifacts_ios=false'
expect_output "$temp_dir/server" 'development_artifacts=false'
expect_output "$temp_dir/build" 'rust=false'
expect_output "$temp_dir/build" 'integration=false'
expect_output "$temp_dir/build" 'artifacts_android=true'
expect_output "$temp_dir/build" 'artifacts_desktop=true'
expect_output "$temp_dir/build" 'artifacts_ios=true'
expect_output "$temp_dir/build" 'development_artifacts=true'
expect_failure env GITHUB_OUTPUT="$temp_dir/invalid" FILTER_RUST_SHARED=invalid "$SCOPE"

gate_env=(
  CHANGES_RESULT=success CHANGES_REQUIRED=true
  SOURCE_AUDIT_RESULT=success SOURCE_AUDIT_REQUIRED=true
  VERSION_RESULT=success VERSION_REQUIRED=true
  RELEASE_TOOLING_RESULT=success RELEASE_TOOLING_REQUIRED=true
  DEVELOPMENT_TOOLS_RESULT=success DEVELOPMENT_TOOLS_REQUIRED=true
  RUST_RESULT=success RUST_REQUIRED=true
  INTEGRATION_RESULT=success INTEGRATION_REQUIRED=true
  DESKTOP_RESULT=success DESKTOP_REQUIRED=true
  ANDROID_RESULT=success ANDROID_REQUIRED=true
  SWIFT_BINDINGS_RESULT=success SWIFT_BINDINGS_REQUIRED=true
  CONTAINER_IMAGE_RESULT=success CONTAINER_IMAGE_REQUIRED=true
  DEVELOPMENT_ARTIFACTS_RESULT=success DEVELOPMENT_ARTIFACTS_REQUIRED=true
)

env "${gate_env[@]}" "$GATE"
env "${gate_env[@]}" RUST_RESULT=skipped RUST_REQUIRED=false "$GATE"
expect_failure env "${gate_env[@]}" RUST_RESULT=skipped RUST_REQUIRED=true "$GATE"
expect_failure env "${gate_env[@]}" RUST_RESULT=failure "$GATE"
expect_failure env "${gate_env[@]}" RUST_RESULT=cancelled "$GATE"
expect_failure env "${gate_env[@]}" CHANGES_RESULT=failure "$GATE"
expect_failure env "${gate_env[@]}" RUST_RESULT=invalid "$GATE"
expect_failure env CHANGES_RESULT=success "$GATE"
expect_failure env "${gate_env[@]}" CHANGES_RESULT=failure RUST_RESULT=skipped RUST_REQUIRED= "$GATE"

for prefix in INTEGRATION DESKTOP DEVELOPMENT_ARTIFACTS; do
  env "${gate_env[@]}" "${prefix}_RESULT=skipped" "${prefix}_REQUIRED=false" "$GATE"
  expect_failure env "${gate_env[@]}" "${prefix}_RESULT=skipped" "${prefix}_REQUIRED=true" "$GATE"
  expect_failure env "${gate_env[@]}" "${prefix}_RESULT=failure" "$GATE"
  expect_failure env "${gate_env[@]}" "${prefix}_RESULT=cancelled" "$GATE"
  expect_failure env "${gate_env[@]}" "${prefix}_RESULT=invalid" "$GATE"
  expect_failure env "${gate_env[@]}" "${prefix}_RESULT=" "$GATE"
  expect_failure env "${gate_env[@]}" "${prefix}_REQUIRED=maybe" "$GATE"
  expect_failure env "${gate_env[@]}" "${prefix}_REQUIRED=" "$GATE"
done

summary="$temp_dir/summary"
expect_failure env "${gate_env[@]}" GITHUB_STEP_SUMMARY="$summary" RUST_REQUIRED= "$GATE"
expect_output "$summary" '| job | required | result |'

echo 'CI script tests passed'
bash "$SCRIPT_DIR/test-hosted-production-handoff.sh"
bash "$SCRIPT_DIR/test-release-context-guard.sh"
ruby "$SCRIPT_DIR/test-release-workflows.rb"
"$SCRIPT_DIR/test-release-signing-readiness.sh"
"$SCRIPT_DIR/test-publish-community-image.sh"
ruby "$SCRIPT_DIR/test-contribution-policy.rb"
