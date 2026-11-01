#!/usr/bin/env bash
set -euo pipefail
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=infra/release/lib.sh
# shellcheck disable=SC1091
source "${PEPPY_RELEASE_LIB:-$script_dir/lib.sh}"
group=all
if [ "${1:-}" = --group ]; then
  [ "$#" -ge 2 ] || release_die "--group requires a value" 2
  group=$2
  shift 2
fi
[ "$#" -eq 4 ] || release_die "usage: $0 [--group all|desktop|android] <version> <channel> <download-dir> <out-dir>" 2
version=$1; channel=$2; download_dir=$3; out_dir=$4
case "$channel" in stable|staging) ;; *) release_die "invalid channel: $channel" 2 ;; esac
case "$group" in all|desktop|android) ;; *) release_die "invalid asset group: $group" 2 ;; esac
[ -d "$download_dir" ] || release_die "download directory not found: $download_dir" 2
find_exact() {
  matches=$(find "$1" -type f -name "$2" -print 2>/dev/null || true)
  count=$(printf '%s\n' "$matches" | sed '/^$/d' | wc -l | tr -d ' ')
  [ "$count" -eq 1 ] || release_die "expected exactly one $2 under $1; found $count"
  printf '%s\n' "$matches"
}
copy_asset() { cp "$1" "$out_dir/$2"; }
mkdir -p "$out_dir"; rm -f "$out_dir"/*
android="$download_dir/android-release-artifacts"; linux="$download_dir/desktop-linux-release-artifacts"; macos="$download_dir/desktop-macos-release-artifacts"; windows="$download_dir/desktop-windows-release-artifacts"
if [ "$group" = all ] || [ "$group" = android ]; then
  copy_asset "$(find_exact "$android" 'peppy-android.apk')" "peppy-$version-android.apk"
  copy_asset "$(find_exact "$android" 'peppy-android.aab')" "peppy-$version-android.aab"
fi
if [ "$group" = all ] || [ "$group" = desktop ]; then
  work_dir=$(mktemp -d); trap 'rm -rf "$work_dir"' EXIT
  tar -xzf "$(find_exact "$linux" 'desktop-linux-bundles.tar.gz')" -C "$work_dir"
  copy_asset "$(find_exact "$work_dir" '*.AppImage')" "peppy-$version-linux-x86_64.AppImage"
  copy_asset "$(find_exact "$work_dir" '*.deb')" "peppy-$version-linux-x86_64.deb"
  copy_asset "$(find_exact "$work_dir" '*.rpm')" "peppy-$version-linux-x86_64.rpm"
  copy_asset "$(find_exact "$macos" '*.dmg')" "peppy-$version-macos-aarch64.dmg"
  copy_asset "$(find_exact "$macos" '*.app.tar.gz')" "peppy-$version-darwin-aarch64.app.tar.gz"
  copy_asset "$(find_exact "$windows" '*.msi')" "peppy-$version-windows-x86_64.msi"
  copy_asset "$(find_exact "$windows" '*-setup.exe')" "peppy-$version-windows-x86_64-setup.exe"
fi
(cd "$out_dir"; sums=SHA256SUMS.tmp; if command -v sha256sum >/dev/null 2>&1; then find . -maxdepth 1 -type f ! -name SHA256SUMS ! -name SHA256SUMS.tmp -exec sha256sum {} \; | sed 's#  \./#  #' | LC_ALL=C sort > "$sums"; else find . -maxdepth 1 -type f ! -name SHA256SUMS ! -name SHA256SUMS.tmp -exec shasum -a 256 {} \; | sed 's#  \./#  #' | LC_ALL=C sort > "$sums"; fi; mv "$sums" SHA256SUMS)
