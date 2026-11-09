#!/usr/bin/env bash

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091 # The helper is sourced relative to this script.
source "$SCRIPT_DIR/lib/platform-dispatch.sh"

readonly summary_title='Platform companion dispatch'
readonly dispatch_url='https://api.github.com/repos/mattv8/peppy-platform/actions/workflows/companion-sync.yml/dispatches'
payload_file=''

cleanup() {
  [[ -z "$payload_file" ]] || rm -f "$payload_file"
}

trap cleanup EXIT

[[ -n "${PEPPY_PLATFORM_DISPATCH_TOKEN:-}" ]] || platform_dispatch_fail "$summary_title" 'PEPPY_PLATFORM_DISPATCH_TOKEN is unavailable'
platform_dispatch_require_match "$summary_title" public_pr "${PUBLIC_PR:-}" '^[1-9][0-9]{0,9}$'

payload_file="$(mktemp)"
printf '{"ref":"production","inputs":{"public_pr":"%s"}}' "$PUBLIC_PR" > "$payload_file"

platform_dispatch_post "$summary_title" PEPPY_PLATFORM_DISPATCH_TOKEN "$dispatch_url" "$payload_file" \
  'private workflow dispatch request failed' \
  'private workflow dispatch did not queue the private workflow'
platform_dispatch_success_summary "$summary_title" "Queued private platform companion workflow dispatch for public PR #$PUBLIC_PR."
