#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=infra/release/lib.sh
. "$script_dir/lib.sh"

: "${VERSION:?VERSION is required}"

highest=
while IFS= read -r tag; do
    if ! printf '%s\n' "$tag" | grep -Eq "$PEPPY_PRERELEASE_TAG_REGEX"; then
        continue
    fi
    if [[ -z "$highest" || $(semver_compare "$tag" "$highest") -gt 0 ]]; then
        highest=$tag
    fi
done < <(git tag --merged HEAD)

if [[ -n "$highest" && $(semver_compare "$VERSION" "$highest") -lt 0 ]]; then
    echo "chosen stable version $VERSION is below reachable prerelease $highest" >&2
    exit 1
fi
