#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
command -v git-cliff >/dev/null || { echo "git-cliff is required; run $script_dir/install-git-cliff.sh <bin-dir>" >&2; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
assert_eq() { [[ $1 == "$2" ]] || { echo "expected '$2', got '$1'" >&2; exit 1; }; }
run() { (cd "$1" && "$script_dir/compute-version.sh" "${@:2}"); }
field() { printf '%s\n' "$1" | sed -n "s/^$2=//p"; }
new_repo() {
    mkdir -p "$1"; git -C "$1" init -q
    git -C "$1" config user.name release-test; git -C "$1" config user.email release-test@example.com
    git -C "$1" config commit.gpgsign false; git -C "$1" config tag.gpgsign false
    git -C "$1" commit --allow-empty -qm init
}

# shellcheck source=infra/release/lib.sh
. "$script_dir/lib.sh"
assert_eq "$(semver_compare 0.2.0 0.2.0-staging.9)" 1
assert_eq "$(semver_compare 0.2.0-staging.10 0.2.0-staging.9)" 1
assert_eq "$(semver_compare 1.0.0 0.99.99)" 1
assert_eq "$(semver_compare v1.0.0 v1.0.0)" 0
assert_eq "$(semver_compare android-v1.0.0 android-v1.0.0-staging.9)" 1
assert_eq "$(peppy_tag_prefix android)" android-v
assert_eq "$(semver_bump v0.1.9 minor)" 0.2.0
grep -F "tag_pattern = '$PEPPY_STABLE_TAG_REGEX'" "$script_dir/cliff.toml" >/dev/null

repo=$tmp/no-tags; new_repo "$repo"
assert_eq "$(field "$(run "$repo" --channel prerelease)" version)" 0.1.0-staging.1
default_desktop=$(run "$repo" --channel prerelease)
component_desktop=$(run "$repo" --channel prerelease --component desktop)
assert_eq "$component_desktop" "$default_desktop"
assert_eq "$(field "$component_desktop" tag)" v0.1.0-staging.1
assert_eq "$(field "$component_desktop" last_stable)" ''
assert_eq "$(field "$component_desktop" commits_since)" 1
out=$(run "$repo" --channel stable); assert_eq "$(field "$out" version)" 0.1.0
assert_eq "$(field "$out" android_version_code)" 100999
assert_eq "$(field "$(run "$repo" --channel dev)" version)" 0.1.0-dev.1
branch=$(git -C "$repo" branch --show-current)
git -C "$repo" checkout -q --orphan existing-tag
git -C "$repo" commit --allow-empty -qm existing
git -C "$repo" tag v0.2.0
git -C "$repo" checkout -q "$branch"
if run "$repo" --channel stable --version 0.2.0 >/dev/null 2>&1; then exit 1; else [[ $? == 2 ]]; fi

repo=$tmp/bumps; new_repo "$repo"; git -C "$repo" tag v0.1.0
git -C "$repo" commit --allow-empty -qm 'fix: repair'; assert_eq "$(field "$(run "$repo" --channel stable)" version)" 0.1.1
git -C "$repo" tag v0.1.1; git -C "$repo" commit --allow-empty -qm 'perf: faster'; assert_eq "$(field "$(run "$repo" --channel stable)" version)" 0.1.2
git -C "$repo" tag v0.1.2; git -C "$repo" commit --allow-empty -qm 'feat: useful'; assert_eq "$(field "$(run "$repo" --channel stable)" version)" 0.2.0
git -C "$repo" tag v0.2.0; git -C "$repo" commit --allow-empty -qm 'feat!: breaking'; assert_eq "$(field "$(run "$repo" --channel stable)" version)" 0.3.0
git -C "$repo" tag v0.3.0; git -C "$repo" commit --allow-empty -qm $'fix: break footer\n\nBREAKING CHANGE: changed'; assert_eq "$(field "$(run "$repo" --channel stable)" version)" 0.4.0
git -C "$repo" tag v0.4.0; git -C "$repo" commit --allow-empty -qm 'chore!: breaking'; assert_eq "$(field "$(run "$repo" --channel stable)" version)" 0.5.0
git -C "$repo" tag v0.5.0; git -C "$repo" commit --allow-empty -qm 'refactor!: breaking'; assert_eq "$(field "$(run "$repo" --channel stable)" version)" 0.6.0
git -C "$repo" tag v0.6.0; git -C "$repo" commit --allow-empty -qm $'refactor: break footer\n\nBREAKING CHANGE: changed'; assert_eq "$(field "$(run "$repo" --channel stable)" version)" 0.7.0
git -C "$repo" tag v0.7.0; git -C "$repo" commit --allow-empty -qm 'docs!: breaking'; assert_eq "$(field "$(run "$repo" --channel stable)" version)" 0.8.0
git -C "$repo" tag v0.8.0; git -C "$repo" commit --allow-empty -qm 'docs: only'
assert_eq "$(field "$(run "$repo" --channel prerelease)" bump)" none-patch
if run "$repo" --channel stable >/dev/null 2>&1; then exit 1; else [[ $? == 3 ]]; fi
fallback=$(run "$repo" --channel stable --allow-auto-patch-fallback)
assert_eq "$(field "$fallback" version)" 0.8.1
assert_eq "$(field "$fallback" bump)" none-patch
git -C "$repo" commit --allow-empty -qm 'fix: alongside docs'; assert_eq "$(field "$(run "$repo" --channel stable)" version)" 0.8.1
git -C "$repo" tag v0.8.1; git -C "$repo" commit --allow-empty -qm 'unconventional message'; assert_eq "$(field "$(run "$repo" --channel stable)" version)" 0.8.2
git -C "$repo" tag v0.8.2; git -C "$repo" commit --allow-empty -qm 'chore: only'
assert_eq "$(field "$(run "$repo" --channel prerelease)" bump)" none-patch
assert_eq "$(field "$(run "$repo" --channel stable --bump minor)" version)" 0.9.0
assert_eq "$(field "$(run "$repo" --channel stable --version 0.10.0)" bump)" explicit
if run "$repo" --channel dev --version 0.10.0 >/dev/null 2>&1; then exit 1; fi
if run "$repo" --channel stable --version 0.8.2 >/dev/null 2>&1; then exit 1; fi

git -C "$repo" tag v0.8.2-staging.99
assert_eq "$(field "$(run "$repo" --channel prerelease --bump patch)" base_version)" 0.8.3
git -C "$repo" tag v0.8.3
if run "$repo" --channel stable >/dev/null 2>&1; then exit 1; else [[ $? == 4 ]]; fi
out=$(run "$repo" --channel prerelease); assert_eq "$(field "$out" version)" 0.8.4-staging.0
assert_eq "$(field "$out" android_version_code)" 804000
assert_eq "$(field "$out" wix_version)" 0.8.4.0

repo=$tmp/merged-fix; new_repo "$repo"; git -C "$repo" tag v0.1.0
branch=$(git -C "$repo" branch --show-current)
git -C "$repo" checkout -qb topic
git -C "$repo" commit --allow-empty -qm 'fix: merged repair'
git -C "$repo" checkout -q "$branch"
git -C "$repo" merge --no-ff -qm 'Merge pull request #1 from mattv8/topic' topic
assert_eq "$(field "$(run "$repo" --channel stable)" version)" 0.1.1
notes=$(cd "$repo" && "$script_dir/release-notes.sh" 0.1.1)
printf '%s\n' "$notes" | grep -F 'merged repair' >/dev/null
if printf '%s\n' "$notes" | grep -F 'Merge pull request' >/dev/null; then exit 1; fi

repo=$tmp/merged-docs; new_repo "$repo"; git -C "$repo" tag v0.1.0
branch=$(git -C "$repo" branch --show-current)
git -C "$repo" checkout -qb topic
git -C "$repo" commit --allow-empty -qm 'docs: merged guide'
git -C "$repo" checkout -q "$branch"
git -C "$repo" merge --no-ff -qm 'Merge pull request #1 from mattv8/topic' topic
assert_eq "$(field "$(run "$repo" --channel prerelease)" bump)" none-patch
if run "$repo" --channel stable >/dev/null 2>&1; then exit 1; else [[ $? == 3 ]]; fi

repo=$tmp/merged-docs-breaking; new_repo "$repo"; git -C "$repo" tag v0.1.0
branch=$(git -C "$repo" branch --show-current)
git -C "$repo" checkout -qb docs
git -C "$repo" commit --allow-empty -qm 'docs: merged guide'
git -C "$repo" checkout -q "$branch"
git -C "$repo" merge --no-ff -m 'Merge pull request #2 from mattv8/docs' -m 'BREAKING CHANGE: nope' docs
assert_eq "$(field "$(run "$repo" --channel prerelease)" bump)" none-patch
if run "$repo" --channel stable >/dev/null 2>&1; then exit 1; else [[ $? == 3 ]]; fi

repo=$tmp/merged-feature; new_repo "$repo"; git -C "$repo" tag v0.1.0
branch=$(git -C "$repo" branch --show-current)
git -C "$repo" checkout -qb topic
git -C "$repo" commit --allow-empty -qm 'feat: merged capability'
git -C "$repo" checkout -q "$branch"
git -C "$repo" merge --no-ff -qm 'Merge pull request #1 from mattv8/topic' topic
assert_eq "$(field "$(run "$repo" --channel stable)" version)" 0.2.0

repo=$tmp/limits; new_repo "$repo"
if run "$repo" --channel stable --version 210.0.0 >/dev/null 2>&1; then exit 1; else [[ $? == 5 ]]; fi
git -C "$repo" tag v0.1.0
i=0; while ((i < 999)); do git -C "$repo" commit --allow-empty -qm "fix: $i"; i=$((i + 1)); done
if run "$repo" --channel prerelease --bump patch >/dev/null 2>&1; then exit 1; else [[ $? == 5 ]]; fi

touch_commit() {
    local repo=$1 path=$2 message=$3
    mkdir -p "$(dirname "$repo/$path")"
    printf '%s\n' "$message" >>"$repo/$path"
    git -C "$repo" add "$path"
    git -C "$repo" commit -qm "$message"
}

repo=$tmp/android-bootstrap; new_repo "$repo"; git -C "$repo" tag v0.17.1
touch_commit "$repo" apps/android/release.txt 'feat: android release'
android_staging=$(PEPPY_ANDROID_BOOTSTRAP_TAG=v0.17.1 run "$repo" --channel prerelease --component android)
assert_eq "$(field "$android_staging" last_stable)" v0.17.1
assert_eq "$(field "$android_staging" tag)" android-v0.18.0-staging.1
assert_eq "$(field "$android_staging" changed)" true
assert_eq "$(field "$android_staging" budget_refresh)" false
android_stable=$(PEPPY_ANDROID_BOOTSTRAP_TAG=v0.17.1 run "$repo" --channel stable --component android)
assert_eq "$(field "$android_stable" android_version_code)" 1800999

repo=$tmp/android-before-first-tag; new_repo "$repo"; git -C "$repo" tag v0.17.1
touch_commit "$repo" apps/android/release.txt 'fix: android release'
touch_commit "$repo" packages/desktop-ui/release.txt 'feat: desktop release'; git -C "$repo" tag v0.18.0
out=$(PEPPY_ANDROID_BOOTSTRAP_TAG=v0.17.1 run "$repo" --channel stable --component android)
assert_eq "$(field "$out" last_stable)" v0.17.1
assert_eq "$(field "$out" version)" 0.17.2
assert_eq "$(field "$out" changed)" true

repo=$tmp/android-changes; new_repo "$repo"; git -C "$repo" tag v0.17.1
touch_commit "$repo" apps/android/release.txt 'fix: android release'; git -C "$repo" tag android-v0.17.2-staging.1
for path in packages/desktop-ui/release.txt services/server/release.txt apps/web/release.txt; do
    touch_commit "$repo" "$path" 'docs: desktop only'
    out=$(PEPPY_ANDROID_BOOTSTRAP_TAG=v0.17.1 run "$repo" --channel prerelease --component android)
    assert_eq "$(field "$out" changed)" false
done
for path in crates/crypto/release.txt Cargo.lock rust-toolchain.toml crates/protocol/release.txt apps/android/release.txt; do
    touch_commit "$repo" "$path" 'fix: android input'
    out=$(PEPPY_ANDROID_BOOTSTRAP_TAG=v0.17.1 run "$repo" --channel prerelease --component android)
    assert_eq "$(field "$out" changed)" true
    git -C "$repo" tag -f android-v0.17.2-staging.1 HEAD >/dev/null
done

repo=$tmp/android-tags; new_repo "$repo"; git -C "$repo" tag v0.17.1
touch_commit "$repo" apps/android/release.txt 'fix: android release'; git -C "$repo" tag android-v0.17.2
if PEPPY_ANDROID_BOOTSTRAP_TAG=v0.17.1 run "$repo" --channel stable --component android >/dev/null 2>&1; then exit 1; else [[ $? == 4 ]]; fi
assert_eq "$(field "$(run "$repo" --channel prerelease --component desktop)" last_stable)" v0.17.1
touch_commit "$repo" packages/desktop-ui/release.txt 'fix: desktop release'; git -C "$repo" tag v0.99.0
out=$(PEPPY_ANDROID_BOOTSTRAP_TAG=v0.17.1 run "$repo" --channel prerelease --component android)
assert_eq "$(field "$out" last_stable)" android-v0.17.2

repo=$tmp/android-budget; new_repo "$repo"; git -C "$repo" tag v0.17.1
i=0; while ((i < 501)); do git -C "$repo" commit --allow-empty -qm "docs: $i"; i=$((i + 1)); done
out=$(PEPPY_ANDROID_BOOTSTRAP_TAG=v0.17.1 run "$repo" --channel prerelease --component android)
assert_eq "$(field "$out" budget_refresh)" true

repo=$tmp/notes; new_repo "$repo"; git -C "$repo" tag v0.1.0; git -C "$repo" commit --allow-empty -qm 'feat(api): note'; git -C "$repo" tag v0.2.0-staging.1
notes=$(cd "$repo" && "$script_dir/release-notes.sh" 0.2.0-staging.9)
printf '%s\n' "$notes" | grep -F 'unsigned development-grade prerelease' >/dev/null
printf '%s\n' "$notes" | grep -F 'note' >/dev/null
if printf '%s\n' "$notes" | grep -Eq 'xattr|com\.apple\.quarantine|```'; then exit 1; fi
notes=$(cd "$repo" && TRUSTED_UPDATE_BUILD=true PEPPY_MACOS_SIGNING_MODE=adhoc PEPPY_WINDOWS_SIGNING_MODE=none "$script_dir/release-notes.sh" 0.2.0-staging.9)
printf '%s\n' "$notes" | grep -F 'Tauri-signed updater artifacts' >/dev/null
printf '%s\n' "$notes" | grep -F 'GPG-signed SHA256SUMS' >/dev/null
printf '%s\n' "$notes" | grep -F 'GPG-signed RPM packages' >/dev/null
printf '%s\n' "$notes" | grep -F 'macOS is ad-hoc signed and not notarized' >/dev/null
printf '%s\n' "$notes" | grep -F 'Windows installers are not Authenticode publisher-signed' >/dev/null
printf '%s\n' "$notes" | grep -F 'Android releases are published separately under android-v* tags' >/dev/null
printf '%s\n' "$notes" | grep -F 'iOS archive remains unsigned' >/dev/null
if printf '%s\n' "$notes" | grep -F 'Android APK' >/dev/null; then exit 1; fi
android_notes=$(cd "$repo" && TRUSTED_UPDATE_BUILD=true "$script_dir/release-notes.sh" 0.2.0-staging.9 --component android)
printf '%s\n' "$android_notes" | grep -F 'signed Android APK for direct sideloading' >/dev/null
if printf '%s\n' "$android_notes" | grep -Eq 'Tauri|RPM|macOS'; then exit 1; fi
printf '%s\n' "$notes" | grep -F '### macOS first launch' >/dev/null
printf '%s\n' "$notes" | grep -Fx '```sh' >/dev/null
printf '%s\n' "$notes" | grep -Fx 'xattr -dr com.apple.quarantine "/Applications/Peppy.app" && open "/Applications/Peppy.app"' >/dev/null
printf '%s\n' "$notes" | grep -Fx '```' >/dev/null
if [ "$(printf '%s\n' "$notes" | grep -c 'xattr -dr')" != 1 ]; then exit 1; fi
if printf '%s\n' "$notes" | grep -Eq 'sudo|spctl|master-disable|Downloads'; then exit 1; fi
notes=$(cd "$repo" && TRUSTED_UPDATE_BUILD=true PEPPY_MACOS_SIGNING_MODE=developer-id PEPPY_WINDOWS_SIGNING_MODE=authenticode "$script_dir/release-notes.sh" 0.2.0-staging.9)
if printf '%s\n' "$notes" | grep -Eq 'ad-hoc signed|not Authenticode publisher-signed|xattr|com\.apple\.quarantine|```'; then exit 1; fi
if (cd "$repo" && TRUSTED_UPDATE_BUILD=true PEPPY_MACOS_SIGNING_MODE=invalid "$script_dir/release-notes.sh" 0.2.0-staging.9) >/dev/null 2>&1; then exit 1; fi
if (cd "$repo" && TRUSTED_UPDATE_BUILD=true PEPPY_WINDOWS_SIGNING_MODE='' "$script_dir/release-notes.sh" 0.2.0-staging.9) >/dev/null 2>&1; then exit 1; fi
repo=$tmp/groups; new_repo "$repo"; git -C "$repo" tag v0.1.0
git -C "$repo" commit --allow-empty -qm 'fix: thing!'
git -C "$repo" commit --allow-empty -qm 'feat!: x'
git -C "$repo" commit --allow-empty -qm $'fix: footer\n\nBREAKING CHANGE: api'
notes=$(cd "$repo" && "$script_dir/release-notes.sh" 0.2.0)
breaking=$(printf '%s\n' "$notes" | awk '/^### Breaking changes$/{in_group=1; next} /^### /{in_group=0} in_group')
printf '%s\n' "$breaking" | grep -F 'x' >/dev/null
printf '%s\n' "$breaking" | grep -F 'footer' >/dev/null
if printf '%s\n' "$breaking" | grep -F 'thing!' >/dev/null; then exit 1; fi
git clone -q --depth 1 "file://$repo" "$tmp/shallow"
if run "$tmp/shallow" --channel dev >/dev/null 2>&1; then exit 1; else [[ $? == 2 ]]; fi
printf '%s\n' 'compute-version tests passed'
