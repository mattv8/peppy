#!/usr/bin/env bash
set -euo pipefail

[ "$#" -eq 1 ] || { echo "usage: $0 <asset-dir>" >&2; exit 2; }
asset_dir=$1
[ -d "$asset_dir" ] || { echo "asset directory not found: $asset_dir" >&2; exit 1; }
script_dir=${BASH_SOURCE[0]%/*}
if [ "$script_dir" = "${BASH_SOURCE[0]}" ]; then script_dir=.; fi
script_dir=$(CDPATH='' cd -- "$script_dir" && pwd)
# shellcheck source=infra/release/lib.sh
# shellcheck disable=SC1091
source "$script_dir/lib.sh"
if command -v sha256sum >/dev/null 2>&1; then
  hash_command=(sha256sum)
  check_command=(sha256sum -c)
else
  hash_command=(shasum -a 256)
  check_command=(shasum -a 256 -c)
fi
# Writer outputs (manifest, its signature, and the staging file) are never listed.
CDPATH='' cd -- "$asset_dir"
_release_checksum_transform yes "${hash_command[@]}"
mv SHA256SUMS.tmp SHA256SUMS
"${check_command[@]}" SHA256SUMS >/dev/null
