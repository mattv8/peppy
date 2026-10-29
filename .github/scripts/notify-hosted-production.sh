#!/usr/bin/env bash

set -Eeuo pipefail

readonly dispatch_url='https://api.github.com/repos/mattv8/peppy-platform/actions/workflows/build-publish.yml/dispatches'
payload_file=''

cleanup() {
  [[ -z "$payload_file" ]] || rm -f "$payload_file"
}

trap cleanup EXIT

write_failure_summary() {
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    printf '## Hosted production dispatch\n\n**Failed:** %s\n' "$1" >> "$GITHUB_STEP_SUMMARY"
  fi
}

fail() {
  write_failure_summary "$1"
  echo "Hosted production dispatch failed: $1" >&2
  exit 1
}

require_match() {
  [[ "$2" =~ $3 ]] || fail "invalid $1"
}

write_curl_config() {
  local dispatch_token

  IFS= read -r dispatch_token <<< "$PEPPY_PLATFORM_DISPATCH_TOKEN"
  printf '%s\n' \
    'header = "Accept: application/vnd.github+json"' \
    "header = \"Authorization: Bearer $dispatch_token\"" \
    'header = "X-GitHub-Api-Version: 2022-11-28"' \
    'header = "Content-Type: application/json"'
}

[[ -n "${PEPPY_PLATFORM_DISPATCH_TOKEN:-}" ]] || fail 'PEPPY_PLATFORM_DISPATCH_TOKEN is unavailable'
require_match source_revision "${SOURCE_REVISION:-}" '^[0-9a-f]{40}$'
require_match public_run_id "${RUN_ID:-}" '^[1-9][0-9]*$'
require_match public_run_attempt "${RUN_ATTEMPT:-}" '^[1-9][0-9]*$'
require_match server_digest "${SERVER_DIGEST:-}" '^sha256:[0-9a-f]{64}$'
require_match relay_digest "${RELAY_DIGEST:-}" '^sha256:[0-9a-f]{64}$'

payload_file="$(mktemp)"
printf '{"ref":"production","inputs":{"environment":"production","source_revision":"%s","public_run_id":"%s","public_run_attempt":"%s","server_digest":"%s","relay_digest":"%s"}}' \
  "$SOURCE_REVISION" "$RUN_ID" "$RUN_ATTEMPT" "$SERVER_DIGEST" "$RELAY_DIGEST" > "$payload_file"

if ! http_status="$(write_curl_config | curl --fail --silent --show-error --connect-timeout 10 --max-time 30 \
  --retry 3 --retry-delay 2 --retry-connrefused \
  --config - --request POST "$dispatch_url" --data-binary "@$payload_file" --output /dev/null --write-out '%{http_code}')"; then
  fail 'private workflow dispatch request failed'
fi

[[ "$http_status" == '204' ]] || fail 'private workflow dispatch did not queue the private workflow'

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  printf '## Hosted production dispatch\n\nQueued private production workflow dispatch for source %s (run %s, attempt %s).\n' \
    "$SOURCE_REVISION" "$RUN_ID" "$RUN_ATTEMPT" >> "$GITHUB_STEP_SUMMARY"
fi
