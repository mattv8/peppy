#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
guard="$script_dir/release-context-guard.sh"
sha=0123456789abcdef0123456789abcdef01234567

expect_allowed() {
  env GITHUB_EVENT_NAME="$1" GITHUB_REF="$2" GITHUB_SHA="$sha" REQUESTED_SHA="$sha" CHANNEL="$3" bash "$guard"
}

expect_denied() {
  if env GITHUB_EVENT_NAME="$1" GITHUB_REF="$2" GITHUB_SHA="$sha" REQUESTED_SHA="${4:-$sha}" CHANNEL="$3" bash "$guard" >/dev/null 2>&1; then
    echo "unexpectedly allowed: $1 $2 $3" >&2
    exit 1
  fi
}

# A wrong event, ref, channel, or SHA would expose protected signing/publishing
# paths to an untrusted caller.
expect_allowed workflow_dispatch refs/heads/production stable
expect_allowed push refs/heads/production stable
expect_allowed push refs/heads/staging staging
expect_denied pull_request refs/heads/production stable
expect_denied push refs/heads/staging stable
expect_denied push refs/heads/production staging
expect_denied workflow_dispatch refs/heads/staging stable
expect_denied push refs/heads/production stable deadbeef

# Image publication derives CHANNEL in the shell; pass it explicitly into the
# helper process instead of relying on a non-exported local variable.
image_channel=stable
env GITHUB_EVENT_NAME=push GITHUB_REF=refs/heads/production GITHUB_SHA="$sha" \
  CHANNEL="$image_channel" REQUESTED_SHA="$sha" bash "$guard"

echo 'release context guard tests passed'
