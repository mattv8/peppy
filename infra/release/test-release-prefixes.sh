#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT

guard() {
  local prefix=$1 version=$2 tags=$3 expected=$4
  actual=$(printf '%s\n' "$tags" | "$script_dir/prerelease-guard.sh" --tags-from-stdin --prefix "$prefix" "$version")
  test "$actual" = "skip=$expected"
}

PEPPY_ANDROID_BOOTSTRAP_TAG=v0.17.1 guard android-v 0.17.1-staging.900 '' true
PEPPY_ANDROID_BOOTSTRAP_TAG=v0.17.1 guard android-v 0.4.5-staging.7 '' true
PEPPY_ANDROID_BOOTSTRAP_TAG=v0.17.1 guard android-v 0.17.2-staging.0 '' false
if (cd "$root" && PEPPY_ANDROID_BOOTSTRAP_TAG=v0.17.1 VERSION=0.17.1 bash "$script_dir/check-stable-prerelease-floor.sh" --prefix android-v) >/dev/null 2>&1; then
  printf 'expected Android bootstrap floor failure\n' >&2
  exit 1
fi
export PEPPY_ANDROID_BOOTSTRAP_TAG=v0.0.1

guard v 0.2.0-staging.5 $'android-v9.0.0\nv0.1.0-staging.9' false
guard android-v 0.2.0-staging.5 $'v9.0.0\nandroid-v0.2.0-staging.6' true
guard android-v 0.2.0-staging.5 v9.0.0 false
test "$(printf '%s\n' v0.2.0-staging.6 | "$script_dir/prerelease-guard.sh" --tags-from-stdin 0.2.0-staging.5)" = skip=true

repo="$root/repo"
git init -q "$repo"
git -C "$repo" config user.name release-test
git -C "$repo" config user.email release-test@example.com
git -C "$repo" commit --allow-empty -qm init
git -C "$repo" tag v0.1.0
git -C "$repo" tag android-v0.3.0-staging.1
(cd "$repo" && VERSION=0.2.0 bash "$script_dir/check-stable-prerelease-floor.sh" --prefix v)
if (cd "$repo" && VERSION=0.2.0 bash "$script_dir/check-stable-prerelease-floor.sh" --prefix android-v) >/dev/null 2>&1; then
  printf 'expected Android prerelease floor failure\n' >&2
  exit 1
fi
git -C "$repo" tag v0.3.0-staging.1
if (cd "$repo" && VERSION=0.2.0 bash "$script_dir/check-stable-prerelease-floor.sh") >/dev/null 2>&1; then
  printf 'expected default prerelease floor failure\n' >&2
  exit 1
fi

android_selected=$(printf '%s\n' android-v0.2.0-staging.9 android-v0.2.0-staging.10 android-v0.1.0-staging.99 v0.0.1-staging.1 | "$script_dir/prune-prereleases.sh" --keep 2 --select --prefix android-v)
test "$android_selected" = android-v0.1.0-staging.99
default_selected=$(printf '%s\n' v0.2.0-staging.9 v0.2.0-staging.10 v0.1.0-staging.99 android-v0.0.1-staging.1 | "$script_dir/prune-prereleases.sh" --keep 2 --select)
test "$default_selected" = v0.1.0-staging.99

mkdir "$root/bin"
cat > "$root/bin/gh" <<'EOF'
#!/usr/bin/env bash
if [ "$1 $2" = 'release list' ]; then
  printf '%s\n' '[{"tagName":"v0.1.0-staging.1","isPrerelease":true,"isDraft":true},{"tagName":"android-v0.1.0-staging.1","isPrerelease":true,"isDraft":true}]'
else
  printf '%s\n' "$*" >> "$GH_LOG"
fi
EOF
chmod +x "$root/bin/gh"
GH_LOG="$root/gh.log" PATH="$root/bin:$PATH" "$script_dir/prune-prereleases.sh" --keep 10 --prefix android-v
test "$(cat "$root/gh.log")" = 'release delete android-v0.1.0-staging.1 --yes'

printf 'release prefix tests passed\n'
