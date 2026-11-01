#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
fixture="$root/download"

mkdir -p "$fixture/desktop-linux-release-artifacts" "$fixture/desktop-macos-release-artifacts/apps/desktop/src-tauri/target/release/bundle/dmg" "$fixture/desktop-windows-release-artifacts/nsis" "$fixture/desktop-windows-release-artifacts/msi"
mkdir -p "$root/linux/appimage" "$root/linux/deb" "$root/linux/rpm"
printf appimage > "$root/linux/appimage/Peppy.AppImage"
printf deb > "$root/linux/deb/peppy.deb"
printf rpm > "$root/linux/rpm/peppy.rpm"
tar -C "$root/linux" -czf "$fixture/desktop-linux-release-artifacts/desktop-linux-bundles.tar.gz" appimage deb rpm
printf mac > "$fixture/desktop-macos-release-artifacts/apps/desktop/src-tauri/target/release/bundle/dmg/Peppy_X_aarch64.dmg"
printf archive > "$fixture/desktop-macos-release-artifacts/Peppy.app.tar.gz"
printf exe > "$fixture/desktop-windows-release-artifacts/nsis/Peppy_X_x64-setup.exe"
printf msi > "$fixture/desktop-windows-release-artifacts/msi/Peppy_X_x64_en-US.msi"

desktop_out="$root/desktop"
"$script_dir/stage-assets.sh" --group desktop 0.2.0 stable "$fixture" "$desktop_out"
test "$(find "$desktop_out" -maxdepth 1 -type f | wc -l | tr -d ' ')" = 8
test ! -e "$desktop_out/peppy-0.2.0-android.apk"

key="$root/test-updater.key"
CI=true pnpm --dir apps/desktop exec tauri signer generate --ci --write-keys "$key"
export TAURI_SIGNING_PRIVATE_KEY PEPPY_UPDATE_PUBLIC_KEY
TAURI_SIGNING_PRIVATE_KEY=$(<"$key")
PEPPY_UPDATE_PUBLIC_KEY=$(<"$key.pub")
"$script_dir/sign-update-assets.sh" 0.2.0 stable "$desktop_out"
"$script_dir/generate-update-manifest.sh" 0.2.0 stable "$desktop_out"
test -s "$desktop_out/latest.json"
test "$(find "$desktop_out" -maxdepth 1 -type f -name '*.sig' | wc -l | tr -d ' ')" = 6

android_fixture="$root/android-download"
mkdir -p "$android_fixture/android-release-artifacts"
printf apk > "$android_fixture/android-release-artifacts/peppy-android.apk"
printf aab > "$android_fixture/android-release-artifacts/peppy-android.aab"
android_out="$root/android"
"$script_dir/stage-assets.sh" --group android 0.2.0 stable "$android_fixture" "$android_out"
test "$(find "$android_out" -maxdepth 1 -type f | wc -l | tr -d ' ')" = 3
test -f "$android_out/peppy-0.2.0-android.apk"
test -f "$android_out/peppy-0.2.0-android.aab"
test ! -e "$android_out/latest.json"

printf 'release asset group tests passed\n'
