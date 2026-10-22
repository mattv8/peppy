#!/usr/bin/env bash
set -euo pipefail

if (($# != 1)); then echo "usage: $0 <version>" >&2; exit 2; fi
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
case $1 in *-*) kind=prerelease ;; *) kind='stable release' ;; esac
if [ "${TRUSTED_UPDATE_BUILD:-false}" = true ]; then
  macos_mode="${PEPPY_MACOS_SIGNING_MODE-developer-id}"
  windows_mode="${PEPPY_WINDOWS_SIGNING_MODE-authenticode}"
  case "$macos_mode" in developer-id|adhoc) ;; *) echo 'PEPPY_MACOS_SIGNING_MODE must be developer-id or adhoc' >&2; exit 1 ;; esac
  case "$windows_mode" in authenticode|none) ;; *) echo 'PEPPY_WINDOWS_SIGNING_MODE must be authenticode or none' >&2; exit 1 ;; esac
  printf 'This %s includes Tauri-signed updater artifacts, a GPG-signed SHA256SUMS, and GPG-signed RPM packages.\n' "$kind"
  if [ "$macos_mode" = adhoc ]; then printf 'macOS is ad-hoc signed and not notarized; it is not a Developer ID or Gatekeeper-approved build and may require the System Settings bypass.\n'; fi
  if [ "$windows_mode" = none ]; then printf 'Windows installers are not Authenticode publisher-signed and can show SmartScreen or UAC warnings on installation and updates.\n'; fi
  printf 'Android artifacts remain unsigned, and the iOS archive remains unsigned and is not an installable IPA.\n\n'
  if [ "$macos_mode" = adhoc ]; then
    printf '### macOS first launch\n\n'
    printf 'Copy Peppy.app from the downloaded DMG to /Applications first, and use only a trusted official release after verifying its download checksum. This command removes the macOS quarantine flag from Peppy.app only; it does not sign or notarize the app and does not disable Gatekeeper globally.\n\n'
    cat <<'EOF'
```sh
xattr -dr com.apple.quarantine "/Applications/Peppy.app" && open "/Applications/Peppy.app"
```

EOF
  fi
else
  printf 'This is an unsigned development-grade %s. It is not notarized, signed, or store-distributed.\n\n' "$kind"
fi
git cliff --config "$script_dir/cliff.toml" --unreleased --tag "v$1" --strip all
