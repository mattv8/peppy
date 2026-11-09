#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
guard="$script_dir/release-context-guard.sh"
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
git init --quiet "$root"
git -C "$root" config user.email fixture@example.invalid
git -C "$root" config user.name fixture
printf 'trusted\n' > "$root/fixture"
git -C "$root" add fixture
git -C "$root" commit --quiet -m fixture
sha=$(git -C "$root" rev-parse HEAD)

expect_allowed() {
  (cd "$root" && env GITHUB_EVENT_NAME="$1" GITHUB_REF="$2" GITHUB_SHA="$sha" REQUESTED_SHA="$sha" CHANNEL="$3" bash "$guard")
}

expect_denied() {
  if (cd "$root" && env GITHUB_EVENT_NAME="$1" GITHUB_REF="$2" GITHUB_SHA="$sha" REQUESTED_SHA="${4:-$sha}" CHANNEL="$3" bash "$guard") >/dev/null 2>&1; then
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
if (cd "$root" && env GITHUB_EVENT_NAME=push GITHUB_REF=refs/heads/production GITHUB_SHA=deadbeef REQUESTED_SHA=deadbeef CHANNEL=stable bash "$guard") >/dev/null 2>&1; then
  echo 'unexpectedly allowed matching requested/GitHub SHA that differs from checkout HEAD' >&2
  exit 1
fi

# Image publication derives CHANNEL in the shell; pass it explicitly into the
# helper process instead of relying on a non-exported local variable.
image_channel=stable
(cd "$root" && env GITHUB_EVENT_NAME=push GITHUB_REF=refs/heads/production GITHUB_SHA="$sha" \
  CHANNEL="$image_channel" REQUESTED_SHA="$sha" bash "$guard")

echo 'release context guard tests passed'
