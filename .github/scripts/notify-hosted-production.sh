#!/usr/bin/env bash

set -Eeuo pipefail

readonly dispatch_url='https://api.github.com/repos/mattv8/peppy-platform/actions/workflows/promote-public-production.yml/dispatches'

write_failure_summary() {
  local message="$1"

  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    printf '## Hosted production handoff\n\n**Failed:** %s\n' "$message" >> "$GITHUB_STEP_SUMMARY"
  fi
}

fail() {
  local message="$1"

  write_failure_summary "$message"
  echo "Hosted production handoff failed: $message" >&2
  exit 1
}

require_match() {
  local name="$1"
  local value="$2"
  local pattern="$3"

  [[ "$value" =~ $pattern ]] || fail "invalid $name"
}

[[ -n "${HOSTED_DISPATCH_TOKEN:-}" ]] || fail 'HOSTED_DISPATCH_TOKEN is unavailable'
require_match source_revision "${SOURCE_REVISION:-}" '^[0-9a-f]{40}$'
require_match run_id "${RUN_ID:-}" '^[1-9][0-9]*$'
require_match run_attempt "${RUN_ATTEMPT:-}" '^[1-9][0-9]*$'
require_match server_digest "${SERVER_DIGEST:-}" '^sha256:[0-9a-f]{64}$'
require_match relay_digest "${RELAY_DIGEST:-}" '^sha256:[0-9a-f]{64}$'

payload=$(printf '{"ref":"production","inputs":{"schema_version":"1","source_revision":"%s","run_id":"%s","run_attempt":"%s","server_digest":"%s","relay_digest":"%s"}}' \
  "$SOURCE_REVISION" "$RUN_ID" "$RUN_ATTEMPT" "$SERVER_DIGEST" "$RELAY_DIGEST")

if ! curl --fail --silent --show-error --connect-timeout 10 --max-time 30 \
  --retry 3 --retry-delay 2 --retry-connrefused \
  --request POST "$dispatch_url" \
  --header 'Accept: application/vnd.github+json' \
  --header "Authorization: Bearer $HOSTED_DISPATCH_TOKEN" \
  --header 'X-GitHub-Api-Version: 2022-11-28' \
  --header 'Content-Type: application/json' \
  --data "$payload"; then
  fail 'private workflow dispatch request failed'
fi

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  printf '## Hosted production handoff\n\nRequested promotion of source %s (run %s, attempt %s) with server digest %s and relay digest %s.\n' \
    "$SOURCE_REVISION" "$RUN_ID" "$RUN_ATTEMPT" "$SERVER_DIGEST" "$RELAY_DIGEST" >> "$GITHUB_STEP_SUMMARY"
fi
