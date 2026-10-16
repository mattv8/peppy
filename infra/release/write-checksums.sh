#!/usr/bin/env bash
set -euo pipefail

[ "$#" -eq 1 ] || { echo "usage: $0 <asset-dir>" >&2; exit 2; }
asset_dir=$1
[ -d "$asset_dir" ] || { echo "asset directory not found: $asset_dir" >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
  hash_command=(sha256sum)
else
  hash_command=(shasum -a 256)
fi
find "$asset_dir" -maxdepth 1 -type f ! -name SHA256SUMS ! -name SHA256SUMS.sig -exec "${hash_command[@]}" {} \; |
  sed "s#  $asset_dir/#  #" |
  LC_ALL=C sort > "$asset_dir/SHA256SUMS.tmp"
mv "$asset_dir/SHA256SUMS.tmp" "$asset_dir/SHA256SUMS"
