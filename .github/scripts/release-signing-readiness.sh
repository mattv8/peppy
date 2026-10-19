#!/usr/bin/env bash

set -Eeuo pipefail

validate_mode() {
  local name="$1"
  local value="$2"
  shift 2

  for allowed in "$@"; do
    [ "$value" = "$allowed" ] && return 0
  done
  printf 'invalid %s: %s\n' "$name" "${value:-<empty>}" >&2
  exit 1
}

macos_mode="${PEPPY_MACOS_SIGNING_MODE-developer-id}"
windows_mode="${PEPPY_WINDOWS_SIGNING_MODE-authenticode}"
validate_mode PEPPY_MACOS_SIGNING_MODE "$macos_mode" developer-id adhoc
validate_mode PEPPY_WINDOWS_SIGNING_MODE "$windows_mode" authenticode none

if [ "${1:-}" = --validate-modes ]; then
  exit 0
fi

: "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"

required_inputs=(
  PEPPY_UPDATER_PRIVATE_KEY
  PEPPY_UPDATE_PUBLIC_KEY
  PEPPY_RELEASE_RPM_GPG_KEY
  PEPPY_RELEASE_RPM_GPG_KEY_ID
  PEPPY_RELEASE_GPG_KEY
  PEPPY_RELEASE_GPG_KEY_ID
)

if [ "$macos_mode" = developer-id ]; then
  required_inputs+=(APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID)
fi
if [ "$windows_mode" = authenticode ]; then
  required_inputs+=(WINDOWS_CERTIFICATE WINDOWS_CERTIFICATE_PASSWORD)
fi

missing=()
for input in "${required_inputs[@]}"; do
  if [ -z "${!input:-}" ]; then
    missing+=("$input")
  fi
done

if [ "${#missing[@]}" -eq 0 ]; then
  echo 'ready=true' >> "$GITHUB_OUTPUT"
  exit 0
fi

if [ "${#missing[@]}" -ne "${#required_inputs[@]}" ]; then
  printf 'partial stable release signing configuration; missing: %s\n' "${missing[*]}" >&2
  exit 1
fi

if [ "${GITHUB_EVENT_NAME:-}" = push ]; then
  echo 'ready=false' >> "$GITHUB_OUTPUT"
  echo '::warning::Stable release signing is unconfigured; skipping automatic release until signing is provisioned'
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
      echo '## Stable release skipped'
      echo
      echo 'Signing is unconfigured. This automatic release will resume after stable signing inputs are provisioned.'
    } >> "$GITHUB_STEP_SUMMARY"
  fi
  exit 0
fi

echo 'stable release signing is unconfigured; configure signing before a manual release' >&2
exit 1
