#!/usr/bin/env bash
set -euo pipefail

[ "$#" -eq 1 ] || { echo "usage: $0 <asset-dir>" >&2; exit 2; }
asset_dir=$1
[ -d "$asset_dir" ] || { echo "asset directory not found: $asset_dir" >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
  hash_command=(sha256sum)
  check_command=(sha256sum -c)
else
  hash_command=(shasum -a 256)
  check_command=(shasum -a 256 -c)
fi
# Writer outputs (manifest, its signature, and the staging file) are never listed.
CDPATH='' cd -- "$asset_dir"
# shellcheck disable=SC2094 # SHA256SUMS.tmp is excluded from the enumeration.
find . -maxdepth 1 -type f ! -name SHA256SUMS ! -name SHA256SUMS.sig ! -name SHA256SUMS.tmp -exec "${hash_command[@]}" {} \; |
  sed 's#  \./#  #' |
  LC_ALL=C sort > SHA256SUMS.tmp
mv SHA256SUMS.tmp SHA256SUMS
"${check_command[@]}" SHA256SUMS >/dev/null
