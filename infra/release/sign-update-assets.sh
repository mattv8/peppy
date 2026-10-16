#!/usr/bin/env bash
set -euo pipefail
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=infra/release/lib.sh
source "${PEPPY_RELEASE_LIB:-$script_dir/lib.sh}"

[ "$#" -eq 3 ] || { echo "usage: $0 <version> <stable|staging> <asset-dir>" >&2; exit 2; }
version=$1
channel=$2
asset_dir=$3
case "$channel" in stable|staging) ;; *) echo "invalid channel: $channel" >&2; exit 2 ;; esac
[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ] || { echo 'TAURI_SIGNING_PRIVATE_KEY is required for update releases' >&2; exit 1; }
[ -d "$asset_dir" ] || { echo "asset directory not found: $asset_dir" >&2; exit 1; }

while IFS='|' read -r _ asset; do
  path="$asset_dir/$asset"
  [ -f "$path" ] || { echo "required update asset missing: $asset" >&2; exit 1; }
  rm -f "$path.sig"
  pnpm --dir apps/desktop exec tauri signer sign --app-version "$version" "$path"
  [ -s "$path.sig" ] || { echo "signer did not produce a signature for $asset" >&2; exit 1; }
done < <(release_update_targets "$version")
