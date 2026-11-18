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

printf 'later\n' >> "$root/fixture"
git -C "$root" commit --quiet -am later
tip=$(git -C "$root" rev-parse HEAD)
unrelated=$(printf unrelated | git -C "$root" commit-tree "$(git -C "$root" rev-parse 'HEAD^{tree}')")
git -C "$root" checkout --quiet --detach "$sha"
ancestor_guard() {
  (cd "$root" && env RELEASE_CONTEXT_MODE=production-ancestor GITHUB_EVENT_NAME="$1" GITHUB_REF="$2" \
    GITHUB_SHA="$tip" REQUESTED_SHA="$3" CHANNEL=stable bash "$guard")
}
ancestor_denied() {
  if ancestor_guard "$@" >/dev/null 2>&1; then
    echo "unexpected ancestor-mode approval: $*" >&2
    exit 1
  fi
}
ancestor_guard workflow_dispatch refs/heads/production "$sha"
ancestor_guard push refs/heads/production "$sha"
git -C "$root" checkout --quiet --detach "$unrelated"
ancestor_denied push refs/heads/production "$unrelated"
git -C "$root" checkout --quiet --detach "$sha"
ancestor_denied push refs/heads/production deadbeef
ancestor_denied push refs/heads/production zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz
ancestor_denied pull_request refs/heads/production "$sha"
ancestor_denied push refs/heads/staging "$sha"
git -C "$root" checkout --quiet --detach "$tip"
ancestor_denied push refs/heads/production "$sha"
if (cd "$root" && env GITHUB_EVENT_NAME=push GITHUB_REF=refs/heads/production GITHUB_SHA="$tip" REQUESTED_SHA="$sha" CHANNEL=stable bash "$guard") >/dev/null 2>&1; then
  echo 'default mode accepted an ancestor instead of the triggering SHA' >&2
  exit 1
fi

# Exercise the workflow's actual tag-check commands offline, including an annotated tag.
mkdir -p "$root/infra/release"
cp "$script_dir/../../infra/release/macos-build-number.sh" "$root/infra/release/"
git -C "$root" tag -a v1.2.3 "$sha" -m fixture
awk '/          number=\$\(bash infra\/release\/macos-build-number.sh/{copy=1} copy {sub(/^          /, ""); print; if ($0 ~ /^echo "value=\$number"/) exit}' \
  "$script_dir/../workflows/testflight.yml" > "$root/check-tag.sh"
tag_check() {
  (cd "$root" && env VERSION="$1" REF="$2" GITHUB_OUTPUT="$root/output" bash -e "$root/check-tag.sh")
}
tag_check 1.2.3 "$sha"
if tag_check 1.2.3 "$tip" >/dev/null 2>&1 || tag_check 1.2.4 "$sha" >/dev/null 2>&1; then
  echo 'tag check accepted a mismatched or missing tag' >&2
  exit 1
fi

echo 'release context guard tests passed'
