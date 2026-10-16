#!/usr/bin/env bash
set -euo pipefail

[ "$#" -eq 1 ] || { echo "usage: $0 <rpm-dir>" >&2; exit 2; }
rpm_dir=$1
[ -n "${PEPPY_RELEASE_RPM_GPG_KEY:-}" ] || { echo 'PEPPY_RELEASE_RPM_GPG_KEY is required' >&2; exit 1; }
[ -n "${PEPPY_RELEASE_RPM_GPG_KEY_ID:-}" ] || { echo 'PEPPY_RELEASE_RPM_GPG_KEY_ID is required' >&2; exit 1; }
[ -z "${PEPPY_RELEASE_RPM_GPG_PASSPHRASE:-}" ] || { echo 'RPM signing requires an unprotected, isolated CI key; rpmsign does not support a secure passphrase argument' >&2; exit 1; }
gpg_home=$(mktemp -d)
trap 'rm -rf "$gpg_home"' EXIT
export GNUPGHOME=$gpg_home
printf '%s' "$PEPPY_RELEASE_RPM_GPG_KEY" | gpg --batch --import
rpm_db=$(mktemp -d)
trap 'rm -rf "$gpg_home" "$rpm_db"' EXIT
gpg --batch --armor --export "$PEPPY_RELEASE_RPM_GPG_KEY_ID" | rpm --dbpath "$rpm_db" --import -
key_id=${PEPPY_RELEASE_RPM_GPG_KEY_ID: -8}
for rpm in "$rpm_dir"/*.rpm; do
  [ -e "$rpm" ] || { echo 'required RPM bundle missing' >&2; exit 1; }
  rpmsign --addsign --define "_gpg_name $PEPPY_RELEASE_RPM_GPG_KEY_ID" "$rpm"
  verification=$(rpm --dbpath "$rpm_db" -Kv "$rpm")
  printf '%s\n' "$verification" | grep -Eiq "(RSA|DSA).*key ID ${key_id}: OK" || { echo "RPM signature key verification failed: $rpm" >&2; exit 1; }
  printf '%s\n' "$verification" | grep -Eq 'digest.*OK' || { echo "RPM digest verification failed: $rpm" >&2; exit 1; }
done
