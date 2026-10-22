#!/usr/bin/env bash
set -euo pipefail
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=infra/release/lib.sh
# shellcheck disable=SC1091
source "${PEPPY_RELEASE_LIB:-$script_dir/lib.sh}"
test "$(release_linux_package_version 0.2.0)" = 0.2.0
test "$(release_linux_package_version 0.2.0-staging.5)" = 0.2.0~staging.5
test "$(release_linux_rpm_release 0.2.0-staging.5)" = 1
if release_linux_package_version 0.2.0-dev.1 >/dev/null 2>&1; then printf 'expected invalid Linux package version failure\n' >&2; exit 1; fi
root=$(mktemp -d); trap 'rm -rf "$root"' EXIT
fixture="$root/download"
mkdir -p "$fixture/android-release-artifacts" "$fixture/desktop-linux-release-artifacts" "$fixture/desktop-macos-release-artifacts/apps/desktop/src-tauri/target/release/bundle/dmg" "$fixture/desktop-windows-release-artifacts/nsis" "$fixture/desktop-windows-release-artifacts/msi"
printf release > "$fixture/android-release-artifacts/peppy-android.apk"
printf bundle > "$fixture/android-release-artifacts/peppy-android.aab"
mkdir -p "$root/linux/appimage" "$root/linux/deb" "$root/linux/rpm"
printf appimage > "$root/linux/appimage/Peppy.AppImage"; printf deb > "$root/linux/deb/peppy.deb"; printf rpm > "$root/linux/rpm/peppy.rpm"
tar -C "$root/linux" -czf "$fixture/desktop-linux-release-artifacts/desktop-linux-bundles.tar.gz" appimage deb rpm
printf mac > "$fixture/desktop-macos-release-artifacts/apps/desktop/src-tauri/target/release/bundle/dmg/Peppy_X_aarch64.dmg"
printf archive > "$fixture/desktop-macos-release-artifacts/Peppy.app.tar.gz"
printf exe > "$fixture/desktop-windows-release-artifacts/nsis/Peppy_X_x64-setup.exe"; printf msi > "$fixture/desktop-windows-release-artifacts/msi/Peppy_X_x64_en-US.msi"
"$script_dir/stage-assets.sh" 0.2.0-staging.5 staging "$fixture" "$root/prerelease"
test -f "$root/prerelease/peppy-0.2.0-staging.5-linux-x86_64.deb"; test -f "$root/prerelease/peppy-0.2.0-staging.5-linux-x86_64.rpm"; test -s "$root/prerelease/SHA256SUMS"
test -f "$root/prerelease/peppy-0.2.0-staging.5-android.apk"; test -f "$root/prerelease/peppy-0.2.0-staging.5-android.aab"
test ! -e "$root/prerelease/peppy-0.2.0-staging.5-android-debug.apk"
test "$(find "$root/prerelease" -type f | wc -l | tr -d ' ')" = 10; test "$(grep -c . "$root/prerelease/SHA256SUMS")" = 9
grep -Fq 'peppy-0.2.0-staging.5-android.apk' "$root/prerelease/SHA256SUMS"; grep -Fq 'peppy-0.2.0-staging.5-android.aab' "$root/prerelease/SHA256SUMS"
"$script_dir/stage-assets.sh" 0.2.0 stable "$fixture" "$root/stable"; test -f "$root/stable/peppy-0.2.0-linux-x86_64.rpm"
if "$script_dir/sign-update-assets.sh" 0.2.0 stable "$root/stable" >/dev/null 2>&1; then printf 'expected unsigned signing failure\n' >&2; exit 1; fi
key="$root/test-updater.key"; alternate_key="$root/alternate-updater.key"
CI=true pnpm --dir apps/desktop exec tauri signer generate --ci --write-keys "$key"
CI=true pnpm --dir apps/desktop exec tauri signer generate --ci --write-keys "$alternate_key"
export TAURI_SIGNING_PRIVATE_KEY
TAURI_SIGNING_PRIVATE_KEY=$(<"$key")
export PEPPY_UPDATE_PUBLIC_KEY
PEPPY_UPDATE_PUBLIC_KEY=$(<"$key.pub")
"$script_dir/sign-update-assets.sh" 0.2.0 stable "$root/stable"
"$script_dir/generate-update-manifest.sh" 0.2.0 stable "$root/stable"
"$script_dir/verify-update-assets.sh" 0.2.0 stable "$root/stable"
relative_stable="$root/relative assets/stable"
mkdir -p "$relative_stable"
while IFS='|' read -r _ asset; do cp "$root/stable/$asset" "$relative_stable/$asset"; done < <(release_update_targets 0.2.0)
relative_stable_from_caller=$(python3 - "$relative_stable" "$PWD" <<'PY'
import os
import sys

print(os.path.relpath(sys.argv[1], sys.argv[2]))
PY
)
mutated_signer="$root/sign-update-assets-without-path-normalization.sh"
# shellcheck disable=SC2016
sed '/^asset_dir=$(CDPATH=.*pwd -P)$/d' "$script_dir/sign-update-assets.sh" > "$mutated_signer"
chmod +x "$mutated_signer"
if PEPPY_RELEASE_LIB="$script_dir/lib.sh" "$mutated_signer" 0.2.0 stable "$relative_stable_from_caller" >/dev/null 2>&1; then printf 'expected relative signing failure without asset path normalization\n' >&2; exit 1; fi
"$script_dir/sign-update-assets.sh" 0.2.0 stable "$relative_stable_from_caller"
test "$(find "$relative_stable" -maxdepth 1 -type f -name '*.sig' | wc -l | tr -d ' ')" = 6
"$script_dir/generate-update-manifest.sh" 0.2.0 stable "$relative_stable_from_caller"
"$script_dir/verify-update-assets.sh" 0.2.0 stable "$relative_stable_from_caller"
grep -Fq 'linux-x86_64-appimage' "$root/stable/latest.json"
grep -Fq '"signature"' "$root/stable/latest.json"
cp "$root/stable/latest.json" "$root/latest.json"
jq '.platforms["linux-x86_64-appimage"].url = "https://invalid.example/payload"' "$root/latest.json" > "$root/stable/latest.json"
if "$script_dir/verify-update-assets.sh" 0.2.0 stable "$root/stable" >/dev/null 2>&1; then printf 'expected invalid manifest URL failure\n' >&2; exit 1; fi
jq 'del(.platforms["linux-x86_64-rpm"])' "$root/latest.json" > "$root/stable/latest.json"
if "$script_dir/verify-update-assets.sh" 0.2.0 stable "$root/stable" >/dev/null 2>&1; then printf 'expected missing manifest target failure\n' >&2; exit 1; fi
jq '.version = "0.2.1"' "$root/latest.json" > "$root/stable/latest.json"
if "$script_dir/verify-update-assets.sh" 0.2.0 stable "$root/stable" >/dev/null 2>&1; then printf 'expected invalid manifest version failure\n' >&2; exit 1; fi
jq '.platforms["linux-x86_64-appimage"].signature = "invalid"' "$root/latest.json" > "$root/stable/latest.json"
if "$script_dir/verify-update-assets.sh" 0.2.0 stable "$root/stable" >/dev/null 2>&1; then printf 'expected invalid embedded signature failure\n' >&2; exit 1; fi
cp "$root/latest.json" "$root/stable/latest.json"
PEPPY_UPDATE_PUBLIC_KEY=$(<"$alternate_key.pub")
if "$script_dir/verify-update-assets.sh" 0.2.0 stable "$relative_stable_from_caller" >/dev/null 2>&1; then printf 'expected wrong-key verification failure\n' >&2; exit 1; fi
PEPPY_UPDATE_PUBLIC_KEY=$(<"$key.pub")
cp "$relative_stable/peppy-0.2.0-linux-x86_64.AppImage" "$root/original-appimage"
printf tampered >> "$relative_stable/peppy-0.2.0-linux-x86_64.AppImage"
if "$script_dir/verify-update-assets.sh" 0.2.0 stable "$relative_stable_from_caller" >/dev/null 2>&1; then printf 'expected tampered-asset verification failure\n' >&2; exit 1; fi
cp "$root/original-appimage" "$relative_stable/peppy-0.2.0-linux-x86_64.AppImage"
if cargo run --quiet --locked --manifest-path "$script_dir/manifest-verifier/Cargo.toml" -- 0.2.0 "$relative_stable_from_caller" 0.2.1 >/dev/null 2>&1; then printf 'expected version-binding verification failure\n' >&2; exit 1; fi
floor_repo="$root/prerelease-floor"
git init -q "$floor_repo"
git -C "$floor_repo" config user.name release-test
git -C "$floor_repo" config user.email release-test@example.com
git -C "$floor_repo" commit --allow-empty -qm init
git -C "$floor_repo" tag v0.1.0
branch=$(git -C "$floor_repo" branch --show-current)
git -C "$floor_repo" checkout -q --orphan staging-only
git -C "$floor_repo" commit --allow-empty -qm 'feat: staging only'
git -C "$floor_repo" tag v0.3.0-staging.1
git -C "$floor_repo" checkout -q "$branch"
(cd "$floor_repo" && VERSION=0.2.0 bash "$script_dir/check-stable-prerelease-floor.sh")
git -C "$floor_repo" tag v0.3.0-staging.2
if (cd "$floor_repo" && VERSION=0.2.0 bash "$script_dir/check-stable-prerelease-floor.sh") >/dev/null 2>&1; then printf 'expected reachable prerelease floor failure\n' >&2; exit 1; fi
rm "$root/stable/peppy-0.2.0-linux-x86_64.rpm.sig"
if "$script_dir/generate-update-manifest.sh" 0.2.0 stable "$root/stable" >/dev/null 2>&1; then printf 'expected missing signature failure\n' >&2; exit 1; fi
rm "$fixture/android-release-artifacts/peppy-android.apk"
if "$script_dir/stage-assets.sh" 0.2.0 stable "$fixture" "$root/missing" >/dev/null 2>&1; then printf 'expected missing asset failure\n' >&2; exit 1; fi
printf release > "$fixture/android-release-artifacts/peppy-android.apk"; mkdir "$fixture/android-release-artifacts/duplicate"; cp "$fixture/android-release-artifacts/peppy-android.apk" "$fixture/android-release-artifacts/duplicate/peppy-android.apk"
if "$script_dir/stage-assets.sh" 0.2.0 stable "$fixture" "$root/duplicate" >/dev/null 2>&1; then printf 'expected duplicate asset failure\n' >&2; exit 1; fi
rm -rf "$fixture/android-release-artifacts/duplicate"
grep -Fq 'shell: bash' "$script_dir/../../.github/workflows/build-artifacts.yml"
grep -Fq 'refs/heads/production:stable|push:refs/heads/staging:staging' "$script_dir/../../.github/workflows/build-artifacts.yml"
grep -Fq "[ \"\$REQUESTED_REF\" = \"\$GITHUB_SHA\" ]" "$script_dir/../../.github/workflows/build-artifacts.yml"
grep -Fq 'Windows Kits\10\bin' "$script_dir/sign-windows-artifact.ps1"
grep -Fq 'dpkg-deb --root-owner-group --build' "$script_dir/normalize-linux-package-metadata.sh"
grep -Fq "dpkg-deb -c \"\$deb\"" "$script_dir/normalize-linux-package-metadata.sh"
grep -Fq "rpmrebuild --package --notest --directory \"\$output_dir\"" "$script_dir/normalize-linux-package-metadata.sh"
grep -Fq "'%{NAME}|%{VERSION}|%{RELEASE}|%{ARCH}'" "$script_dir/normalize-linux-package-metadata.sh"
grep -Fq -- '--passphrase-fd 3' "$script_dir/sign-checksums.sh"
selected=$(printf '%s\n' '' v0.2.0-staging.9 v0.2.0-staging.10 v0.1.0-staging.99 v0.2.0 other | "$script_dir/prune-prereleases.sh" --keep 2 --select)
test "$selected" = v0.1.0-staging.99
test "$(printf '%s\n' 'v0.2.0-staging.6' | "$script_dir/prune-prereleases.sh" --keep 10 --select)" = ''
check_guard() { expected=$1; tags=$2; actual=$(printf '%s\n' "$tags" | "$script_dir/prerelease-guard.sh" --tags-from-stdin 0.2.0-staging.5); test "$actual" = "skip=$expected"; }
check_guard true v0.2.0-staging.6; check_guard true v0.2.0-staging.5; check_guard true v0.2.0; check_guard false v0.1.9-staging.99; check_guard false ''
checksum_root="$root/checksum"
checksum_dir="$checksum_root/checksum assets"
checksum_names=(peppy-0.2.0-linux-x86_64.AppImage peppy-0.2.0-linux-x86_64.AppImage.sig peppy-0.2.0-linux-x86_64.rpm)
verify_checksum_hashes() {
  (cd "$1" && if command -v sha256sum >/dev/null 2>&1; then sha256sum -c SHA256SUMS; else shasum -a 256 -c SHA256SUMS; fi) >/dev/null
}
assert_checksum_manifest() {
  local dir=$1
  test "$(grep -c . "$dir/SHA256SUMS")" = 3
  diff <(sed -E 's/^[0-9a-f]{64}  //' "$dir/SHA256SUMS" | LC_ALL=C sort) <(printf '%s\n' "${checksum_names[@]}" | LC_ALL=C sort)
  test ! -e "$dir/SHA256SUMS.tmp"
  verify_checksum_hashes "$dir"
}
mkdir -p "$checksum_dir"
echo appimage > "$checksum_dir/${checksum_names[0]}"
echo minisig > "$checksum_dir/${checksum_names[1]}"
echo rpm > "$checksum_dir/${checksum_names[2]}"
echo stale > "$checksum_dir/SHA256SUMS"
echo stale > "$checksum_dir/SHA256SUMS.sig"
echo stale > "$checksum_dir/SHA256SUMS.tmp"
"$script_dir/write-checksums.sh" "$checksum_dir"
assert_checksum_manifest "$checksum_dir"
cp "$checksum_dir/SHA256SUMS" "$checksum_root/first-SHA256SUMS"
echo stale > "$checksum_dir/SHA256SUMS.tmp"
echo stale > "$checksum_dir/SHA256SUMS.sig"
"$script_dir/write-checksums.sh" "$checksum_dir"
assert_checksum_manifest "$checksum_dir"
cmp "$checksum_dir/SHA256SUMS" "$checksum_root/first-SHA256SUMS"
echo stale > "$checksum_dir/SHA256SUMS.tmp"
(cd "$checksum_root" && "$script_dir/write-checksums.sh" "checksum assets")
assert_checksum_manifest "$checksum_dir"
echo stale > "$checksum_dir/SHA256SUMS.tmp"
(cd "$checksum_dir" && "$script_dir/write-checksums.sh" .)
assert_checksum_manifest "$checksum_dir"
cmp "$checksum_dir/SHA256SUMS" "$checksum_root/first-SHA256SUMS"
decoy_root="$checksum_root/cdpath-decoy"
mkdir -p "$decoy_root/checksum assets"
echo decoy > "$decoy_root/checksum assets/decoy.bin"
rm -f "$checksum_dir/SHA256SUMS"
(cd "$checksum_root" && CDPATH="$decoy_root" "$script_dir/write-checksums.sh" "checksum assets")
assert_checksum_manifest "$checksum_dir"
cmp "$checksum_dir/SHA256SUMS" "$checksum_root/first-SHA256SUMS"
test ! -e "$decoy_root/checksum assets/SHA256SUMS"
test ! -e "$decoy_root/checksum assets/SHA256SUMS.tmp"
dash_dir="$checksum_root/-dash-assets"
mkdir -p "$dash_dir"
echo dashed > "$dash_dir/only.bin"
(cd "$checksum_root" && "$script_dir/write-checksums.sh" "-dash-assets")
test "$(grep -c . "$dash_dir/SHA256SUMS")" = 1
verify_checksum_hashes "$dash_dir"
bash_bin=$(command -v bash)
fallback_bin="$checksum_root/shasum-only-bin"
mkdir -p "$fallback_bin"
for tool in find sed sort mv shasum; do ln -s "$(command -v "$tool")" "$fallback_bin/$tool"; done
test -z "$(PATH="$fallback_bin" "$bash_bin" -c 'command -v sha256sum || true')"
echo stale > "$checksum_dir/SHA256SUMS.tmp"
PATH="$fallback_bin" "$bash_bin" "$script_dir/write-checksums.sh" "$checksum_dir"
assert_checksum_manifest "$checksum_dir"
cmp "$checksum_dir/SHA256SUMS" "$checksum_root/first-SHA256SUMS"
real_shasum=$(command -v shasum)
mismatch_bin="$checksum_root/shasum-mismatch-bin"
mkdir -p "$mismatch_bin"
for tool in find sed sort mv; do ln -s "$(command -v "$tool")" "$mismatch_bin/$tool"; done
cat > "$mismatch_bin/shasum" <<SH
#!/bin/sh
for arg in "\$@"; do if [ "\$arg" = -c ]; then echo 'simulated checksum mismatch' >&2; exit 1; fi; done
exec "$real_shasum" "\$@"
SH
chmod +x "$mismatch_bin/shasum"
test -z "$(PATH="$mismatch_bin" "$bash_bin" -c 'command -v sha256sum || true')"
if err=$(PATH="$mismatch_bin" "$bash_bin" "$script_dir/write-checksums.sh" "$checksum_dir" 2>&1 >/dev/null); then printf 'expected checksum self-validation failure\n' >&2; exit 1; fi
grep -Fq 'simulated checksum mismatch' <<<"$err"
printf 'release script tests passed\n'
