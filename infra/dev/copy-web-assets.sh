#!/bin/sh
set -eu

source=/web
output=/output
mountpoint=/output

fail() {
    printf '%s\n' "copy-web-assets: $*" >&2
    exit 1
}

[ -d "$source" ] || fail "Missing source directory: $source"
[ -d "$output" ] || fail "Missing output directory: $output"
[ ! -L "$output" ] || fail "Output directory must not be a symlink: $output"

for asset in index.html worker.js core/peppy-browser-core.js core/peppy_browser_core.wasm; do
    [ -s "$source/$asset" ] || fail "Missing built asset: $asset"
done

if [ -r /proc/self/mountinfo ] && ! awk -v mountpoint="$mountpoint" '$5 == mountpoint { found = 1 } END { exit !found }' /proc/self/mountinfo; then
    fail "Output directory is not a mounted volume: $output"
fi

find "$output" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +
cp -a "$source/." "$output/"
chmod -R a+rX "$output"
