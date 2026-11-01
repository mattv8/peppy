#!/usr/bin/env bash
set -euo pipefail
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=infra/release/lib.sh
# shellcheck disable=SC1091
source "${PEPPY_RELEASE_LIB:-$script_dir/lib.sh}"
from_stdin=false
prefix=v
while [ "$#" -gt 0 ]; do
  case "$1" in
    --tags-from-stdin) from_stdin=true ;;
    --prefix) shift; [ "$#" -gt 0 ] || release_die "--prefix requires a value" 2; prefix=$1 ;;
    *) break ;;
  esac
  shift
done
[ "$#" -eq 1 ] || release_die "usage: $0 [--tags-from-stdin] [--prefix PREFIX] <version>" 2
version=$1
stable_regex=$(peppy_stable_tag_regex "$prefix") || release_die "invalid tag prefix: $prefix" 2
prerelease_regex=$(peppy_prerelease_tag_regex "$prefix") || release_die "invalid tag prefix: $prefix" 2
if ! printf '%s\n' "$prefix$version" | grep -Eq "$stable_regex|$prerelease_regex"; then release_die "invalid version: $version" 2; fi
if [ "$prefix" = android-v ] && [ "$(semver_compare "$version" "$PEPPY_ANDROID_BOOTSTRAP_TAG")" -le 0 ]; then
  printf 'skip=true\n'; printf 'Android version %s is not newer than shared release %s\n' "$version" "$PEPPY_ANDROID_BOOTSTRAP_TAG" >&2; exit 0
fi
if "$from_stdin"; then tags=$(cat); else tags=$(git tag --list); fi
while IFS= read -r tag; do
  [ -n "$tag" ] || continue
  if printf '%s\n' "$tag" | grep -Eq "$stable_regex|$prerelease_regex"; then
    comparison=$(semver_compare "$tag" "$version") || release_die "could not compare tag: $tag"
    if [ "$comparison" -ge 0 ]; then printf 'skip=true\n'; printf 'existing tag %s is not older than %s\n' "$tag" "$version" >&2; exit 0; fi
  fi
done <<EOF_TAGS
$tags
EOF_TAGS
printf 'skip=false\n'
