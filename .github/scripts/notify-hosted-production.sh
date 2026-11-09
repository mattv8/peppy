#!/usr/bin/env bash

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091 # The helper is sourced relative to this script.
source "$SCRIPT_DIR/lib/platform-dispatch.sh"

readonly summary_title='Hosted production dispatch'
readonly dispatch_url='https://api.github.com/repos/mattv8/peppy-platform/actions/workflows/build-publish.yml/dispatches'
payload_file=''

cleanup() {
  [[ -z "$payload_file" ]] || rm -f "$payload_file"
}

trap cleanup EXIT

[[ -n "${GH_TOKEN:-}" ]] || platform_dispatch_fail "$summary_title" 'GH_TOKEN is unavailable; configure secrets.PEPPY_AUTOMATION_TOKEN as GH_TOKEN'
platform_dispatch_require_match "$summary_title" source_revision "${SOURCE_REVISION:-}" '^[0-9a-f]{40}$'
platform_dispatch_require_match "$summary_title" public_run_id "${RUN_ID:-}" '^[1-9][0-9]*$'
platform_dispatch_require_match "$summary_title" public_run_attempt "${RUN_ATTEMPT:-}" '^[1-9][0-9]*$'
platform_dispatch_require_match "$summary_title" server_digest "${SERVER_DIGEST:-}" '^sha256:[0-9a-f]{64}$'
platform_dispatch_require_match "$summary_title" relay_digest "${RELAY_DIGEST:-}" '^sha256:[0-9a-f]{64}$'

payload_file="$(mktemp)"
printf '{"ref":"production","inputs":{"environment":"production","source_revision":"%s","public_run_id":"%s","public_run_attempt":"%s","server_digest":"%s","relay_digest":"%s"}}' \
  "$SOURCE_REVISION" "$RUN_ID" "$RUN_ATTEMPT" "$SERVER_DIGEST" "$RELAY_DIGEST" > "$payload_file"

platform_dispatch_post "$summary_title" GH_TOKEN "$dispatch_url" "$payload_file" \
  'private workflow dispatch request failed' \
  'private workflow dispatch did not queue the private workflow'
platform_dispatch_success_summary "$summary_title" \
  "Queued private production workflow dispatch for source $SOURCE_REVISION (run $RUN_ID, attempt $RUN_ATTEMPT)."
