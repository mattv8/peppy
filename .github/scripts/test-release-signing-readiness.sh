#!/usr/bin/env bash

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
READINESS="$SCRIPT_DIR/release-signing-readiness.sh"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT

required_env=(
  PEPPY_UPDATER_PRIVATE_KEY=updater-key
  PEPPY_UPDATE_PUBLIC_KEY=updater-public-key
  PEPPY_RELEASE_RPM_GPG_KEY=rpm-gpg-key
  PEPPY_RELEASE_RPM_GPG_KEY_ID=rpm-gpg-key-id
  PEPPY_RELEASE_GPG_KEY=release-gpg-key
  PEPPY_RELEASE_GPG_KEY_ID=release-gpg-key-id
  PEPPY_ANDROID_KEYSTORE_BASE64=android-keystore
  PEPPY_ANDROID_KEYSTORE_PASSWORD=android-keystore-password
  PEPPY_ANDROID_KEY_ALIAS=android-key-alias
  PEPPY_ANDROID_KEY_PASSWORD=android-key-password
  PEPPY_ANDROID_SIGNING_CERT_SHA256=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
)

strict_macos_env=(
  APPLE_CERTIFICATE=apple-certificate
  APPLE_CERTIFICATE_PASSWORD=apple-certificate-password
  APPLE_SIGNING_IDENTITY=apple-signing-identity
  APPLE_ID=apple-id
  APPLE_PASSWORD=apple-password
  APPLE_TEAM_ID=apple-team-id
)
strict_windows_env=(
  WINDOWS_CERTIFICATE=windows-certificate
  WINDOWS_CERTIFICATE_PASSWORD=windows-password
)

expect_output() {
  local file="$1"
  local expected="$2"

  grep -Fqx "$expected" "$file" || { echo "missing output: $expected" >&2; exit 1; }
}

run_readiness() {
  local name="$1"
  shift
  local output="$temp_dir/$name.output"
  local stdout="$temp_dir/$name.stdout"
  local summary="$temp_dir/$name.summary"

  : > "$output"
  : > "$summary"
  env GITHUB_OUTPUT="$output" GITHUB_STEP_SUMMARY="$summary" "$@" "$READINESS" >"$stdout" 2>&1
}

run_readiness complete GITHUB_EVENT_NAME=push "${required_env[@]}" "${strict_macos_env[@]}" "${strict_windows_env[@]}"
expect_output "$temp_dir/complete.output" 'ready=true'
run_readiness complete-dispatch GITHUB_EVENT_NAME=workflow_dispatch "${required_env[@]}" "${strict_macos_env[@]}" "${strict_windows_env[@]}"
expect_output "$temp_dir/complete-dispatch.output" 'ready=true'

if run_readiness partial GITHUB_EVENT_NAME=push "${required_env[@]:1}" "${strict_macos_env[@]}" "${strict_windows_env[@]}"; then
  echo 'partial signing configuration must fail' >&2
  exit 1
fi
grep -Fq 'PEPPY_UPDATER_PRIVATE_KEY' "$temp_dir/partial.stdout" || { echo 'partial configuration must list missing names' >&2; exit 1; }
if grep -Fq 'updater-key' "$temp_dir/partial.stdout"; then
  echo 'readiness helper printed a credential value' >&2
  exit 1
fi
if run_readiness partial-dispatch GITHUB_EVENT_NAME=workflow_dispatch "${required_env[@]:1}" "${strict_macos_env[@]}" "${strict_windows_env[@]}"; then
  echo 'partial manual signing configuration must fail' >&2
  exit 1
fi
if run_readiness android-partial GITHUB_EVENT_NAME=push "${required_env[@]:0:6}" "${required_env[@]:7}" "${strict_macos_env[@]}" "${strict_windows_env[@]}"; then
  echo 'partial Android signing configuration must fail' >&2
  exit 1
fi
grep -Fq 'PEPPY_ANDROID_KEYSTORE_BASE64' "$temp_dir/android-partial.stdout" || { echo 'partial Android configuration must list missing names' >&2; exit 1; }

run_readiness free-modes GITHUB_EVENT_NAME=push "${required_env[@]}" PEPPY_MACOS_SIGNING_MODE=adhoc PEPPY_WINDOWS_SIGNING_MODE=none
expect_output "$temp_dir/free-modes.output" 'ready=true'

run_readiness adhoc-authenticode GITHUB_EVENT_NAME=push "${required_env[@]}" PEPPY_MACOS_SIGNING_MODE=adhoc "${strict_windows_env[@]}"
expect_output "$temp_dir/adhoc-authenticode.output" 'ready=true'
run_readiness developer-id-none GITHUB_EVENT_NAME=push "${required_env[@]}" "${strict_macos_env[@]}" PEPPY_WINDOWS_SIGNING_MODE=none
expect_output "$temp_dir/developer-id-none.output" 'ready=true'
if run_readiness free-modes-partial GITHUB_EVENT_NAME=push "${required_env[@]:1}" PEPPY_MACOS_SIGNING_MODE=adhoc PEPPY_WINDOWS_SIGNING_MODE=none; then
  echo 'partial cryptographic signing configuration must fail in free modes' >&2
  exit 1
fi

if run_readiness strict-macos-missing GITHUB_EVENT_NAME=push "${required_env[@]}" "${strict_windows_env[@]}"; then
  echo 'developer-id mode without Apple credentials must fail' >&2
  exit 1
fi
if run_readiness strict-windows-missing GITHUB_EVENT_NAME=push "${required_env[@]}" "${strict_macos_env[@]}"; then
  echo 'authenticode mode without Windows credentials must fail' >&2
  exit 1
fi

for invalid in PEPPY_MACOS_SIGNING_MODE=invalid PEPPY_MACOS_SIGNING_MODE= PEPPY_WINDOWS_SIGNING_MODE=invalid PEPPY_WINDOWS_SIGNING_MODE=; do
  if run_readiness invalid-mode GITHUB_EVENT_NAME=push "$invalid"; then
    echo 'invalid explicit signing mode must fail before credential checks' >&2
    exit 1
  fi
done

run_readiness none-push GITHUB_EVENT_NAME=push
expect_output "$temp_dir/none-push.output" 'ready=false'
grep -Fq '::warning::' "$temp_dir/none-push.stdout" || { echo 'automatic skip must warn' >&2; exit 1; }
grep -Fq 'Signing is unconfigured' "$temp_dir/none-push.summary" || { echo 'automatic skip must summarize' >&2; exit 1; }

if run_readiness none-dispatch GITHUB_EVENT_NAME=workflow_dispatch; then
  echo 'manual release without signing configuration must fail' >&2
  exit 1
fi

for credential in "${required_env[@]#*=}"; do
  if grep -RFq -- "$credential" "$temp_dir"; then
    echo 'readiness helper printed a credential value' >&2
    exit 1
  fi
done

echo 'release signing readiness tests passed'
