#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=infra/release/lib.sh
# shellcheck disable=SC1091
. "${PEPPY_RELEASE_LIB:-$script_dir/lib.sh}"

: "${VERSION:?VERSION is required}"
prefix=v
while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix) shift; [ "$#" -gt 0 ] || release_die "--prefix requires a value" 2; prefix=$1 ;;
        *) release_die "usage: $0 [--prefix PREFIX]" 2 ;;
    esac
    shift
done
prerelease_regex=$(peppy_prerelease_tag_regex "$prefix") || release_die "invalid tag prefix: $prefix" 2
if [[ $prefix == android-v && $(semver_compare "$VERSION" "$PEPPY_ANDROID_BOOTSTRAP_TAG") -le 0 ]]; then
    echo "chosen Android version $VERSION is not newer than shared release $PEPPY_ANDROID_BOOTSTRAP_TAG" >&2
    exit 1
fi

highest=
while IFS= read -r tag; do
    if ! printf '%s\n' "$tag" | grep -Eq "$prerelease_regex"; then
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
