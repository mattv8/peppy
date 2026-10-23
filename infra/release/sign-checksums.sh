#!/usr/bin/env bash
set -euo pipefail

[ "$#" -eq 1 ] || { echo "usage: $0 <asset-dir>" >&2; exit 2; }
asset_dir=$1
[ -s "$asset_dir/SHA256SUMS" ] || { echo 'SHA256SUMS is required' >&2; exit 1; }
[ -n "${PEPPY_RELEASE_GPG_KEY:-}" ] || { echo 'PEPPY_RELEASE_GPG_KEY is required' >&2; exit 1; }
[ -n "${PEPPY_RELEASE_GPG_KEY_ID:-}" ] || { echo 'PEPPY_RELEASE_GPG_KEY_ID is required' >&2; exit 1; }
gpg_home=$(mktemp -d)
trap 'rm -rf "$gpg_home"' EXIT
export GNUPGHOME=$gpg_home
printf '%s' "$PEPPY_RELEASE_GPG_KEY" | gpg --batch --import
gpg --batch --yes --pinentry-mode loopback --passphrase-fd 3 --local-user "$PEPPY_RELEASE_GPG_KEY_ID" --detach-sign --output "$asset_dir/SHA256SUMS.sig" "$asset_dir/SHA256SUMS" 3<<<"${PEPPY_RELEASE_GPG_PASSPHRASE:-}"
[ -s "$asset_dir/SHA256SUMS.sig" ] || { echo 'GPG did not produce SHA256SUMS.sig' >&2; exit 1; }
gpg --batch --verify "$asset_dir/SHA256SUMS.sig" "$asset_dir/SHA256SUMS"
