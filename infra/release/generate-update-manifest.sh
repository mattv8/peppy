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
[ -d "$asset_dir" ] || { echo "asset directory not found: $asset_dir" >&2; exit 1; }
command -v jq >/dev/null || { echo 'jq is required to generate the update manifest' >&2; exit 1; }

platforms='{}'
while IFS='|' read -r key asset; do
  [ -s "$asset_dir/$asset" ] || { echo "required update asset missing: $asset" >&2; exit 1; }
  [ -s "$asset_dir/$asset.sig" ] || { echo "required update signature missing: $asset.sig" >&2; exit 1; }
  url="https://github.com/mattv8/peppy/releases/download/v$version/$asset"
  signature=$(<"$asset_dir/$asset.sig")
  platforms=$(jq --arg key "$key" --arg url "$url" --arg signature "$signature" '. + {($key): {url: $url, signature: $signature}}' <<<"$platforms")
done < <(release_update_targets "$version")
jq -n --arg version "$version" --arg channel "$channel" --argjson platforms "$platforms" '{version: $version, channel: $channel, platforms: $platforms}' > "$asset_dir/latest.json"
