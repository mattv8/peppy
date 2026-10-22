#!/usr/bin/env bash
set -euo pipefail

[ "$#" -eq 3 ] || { echo "usage: $0 <version> <stable|staging> <asset-dir>" >&2; exit 2; }
version=$1
channel=$2
asset_dir=$3
case "$channel" in stable|staging) ;; *) echo "invalid channel: $channel" >&2; exit 2 ;; esac
[ -n "${PEPPY_UPDATE_PUBLIC_KEY:-}" ] || { echo 'PEPPY_UPDATE_PUBLIC_KEY is required' >&2; exit 1; }
cargo run --quiet --locked --manifest-path infra/release/manifest-verifier/Cargo.toml -- "$version" "$asset_dir"
[ -s "$asset_dir/latest.json" ] || { echo 'latest.json is required' >&2; exit 1; }
command -v jq >/dev/null || { echo 'jq is required to validate latest.json' >&2; exit 1; }
jq -e --arg version "$version" --arg channel "$channel" '
  .version == $version and .channel == $channel and
  (.platforms | type == "object") and
  ((.platforms | keys | sort) == ["darwin-aarch64-app", "linux-x86_64-appimage", "linux-x86_64-deb", "linux-x86_64-rpm", "windows-x86_64-msi", "windows-x86_64-nsis"])
' "$asset_dir/latest.json" >/dev/null || { echo 'latest.json has an invalid version, channel, or target set' >&2; exit 1; }
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=infra/release/lib.sh
# shellcheck disable=SC1091
source "${PEPPY_RELEASE_LIB:-$script_dir/lib.sh}"
while IFS='|' read -r key asset; do
  expected_url="https://github.com/mattv8/peppy/releases/download/v$version/$asset"
  expected_signature=$(<"$asset_dir/$asset.sig")
  jq -e --arg key "$key" --arg url "$expected_url" --arg signature "$expected_signature" '
    .platforms[$key] | type == "object" and keys == ["signature", "url"] and .url == $url and .signature == $signature
  ' "$asset_dir/latest.json" >/dev/null || { echo "latest.json target does not match final asset: $key" >&2; exit 1; }
done < <(release_update_targets "$version")
