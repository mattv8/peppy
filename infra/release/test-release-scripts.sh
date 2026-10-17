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
printf release > "$fixture/android-release-artifacts/peppy-android-unsigned.apk"
printf bundle > "$fixture/android-release-artifacts/peppy-android-unsigned.aab"
mkdir -p "$root/linux/appimage" "$root/linux/deb" "$root/linux/rpm"
printf appimage > "$root/linux/appimage/Peppy.AppImage"; printf deb > "$root/linux/deb/peppy.deb"; printf rpm > "$root/linux/rpm/peppy.rpm"
tar -C "$root/linux" -czf "$fixture/desktop-linux-release-artifacts/desktop-linux-bundles.tar.gz" appimage deb rpm
printf mac > "$fixture/desktop-macos-release-artifacts/apps/desktop/src-tauri/target/release/bundle/dmg/Peppy_X_aarch64.dmg"
printf archive > "$fixture/desktop-macos-release-artifacts/Peppy.app.tar.gz"
printf exe > "$fixture/desktop-windows-release-artifacts/nsis/Peppy_X_x64-setup.exe"; printf msi > "$fixture/desktop-windows-release-artifacts/msi/Peppy_X_x64_en-US.msi"
"$script_dir/stage-assets.sh" 0.2.0-staging.5 staging "$fixture" "$root/prerelease"
test -f "$root/prerelease/peppy-0.2.0-staging.5-linux-x86_64.deb"; test -f "$root/prerelease/peppy-0.2.0-staging.5-linux-x86_64.rpm"; test -s "$root/prerelease/SHA256SUMS"
test -f "$root/prerelease/peppy-0.2.0-staging.5-android-unsigned.apk"; test -f "$root/prerelease/peppy-0.2.0-staging.5-android-unsigned.aab"
test ! -e "$root/prerelease/peppy-0.2.0-staging.5-android-debug.apk"
test "$(find "$root/prerelease" -type f | wc -l | tr -d ' ')" = 10; test "$(grep -c . "$root/prerelease/SHA256SUMS")" = 9
grep -Fq 'peppy-0.2.0-staging.5-android-unsigned.apk' "$root/prerelease/SHA256SUMS"; grep -Fq 'peppy-0.2.0-staging.5-android-unsigned.aab' "$root/prerelease/SHA256SUMS"
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
if "$script_dir/verify-update-assets.sh" 0.2.0 stable "$root/stable" >/dev/null 2>&1; then printf 'expected wrong-key verification failure\n' >&2; exit 1; fi
PEPPY_UPDATE_PUBLIC_KEY=$(<"$key.pub")
cp "$root/stable/peppy-0.2.0-linux-x86_64.AppImage" "$root/original-appimage"
printf tampered >> "$root/stable/peppy-0.2.0-linux-x86_64.AppImage"
if "$script_dir/verify-update-assets.sh" 0.2.0 stable "$root/stable" >/dev/null 2>&1; then printf 'expected tampered-asset verification failure\n' >&2; exit 1; fi
cp "$root/original-appimage" "$root/stable/peppy-0.2.0-linux-x86_64.AppImage"
if cargo run --quiet --locked --manifest-path "$script_dir/manifest-verifier/Cargo.toml" -- 0.2.0 "$root/stable" 0.2.1 >/dev/null 2>&1; then printf 'expected version-binding verification failure\n' >&2; exit 1; fi
rm "$root/stable/peppy-0.2.0-linux-x86_64.rpm.sig"
if "$script_dir/generate-update-manifest.sh" 0.2.0 stable "$root/stable" >/dev/null 2>&1; then printf 'expected missing signature failure\n' >&2; exit 1; fi
rm "$fixture/android-release-artifacts/peppy-android-unsigned.apk"
if "$script_dir/stage-assets.sh" 0.2.0 stable "$fixture" "$root/missing" >/dev/null 2>&1; then printf 'expected missing asset failure\n' >&2; exit 1; fi
printf release > "$fixture/android-release-artifacts/peppy-android-unsigned.apk"; mkdir "$fixture/android-release-artifacts/duplicate"; cp "$fixture/android-release-artifacts/peppy-android-unsigned.apk" "$fixture/android-release-artifacts/duplicate/peppy-android-unsigned.apk"
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
printf 'release script tests passed\n'
