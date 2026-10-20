#!/usr/bin/env bash
set -euo pipefail

[ "$#" -eq 1 ] || { echo "usage: $0 <rpm-dir>" >&2; exit 2; }
rpm_dir=$1
[ -n "${PEPPY_RELEASE_RPM_GPG_KEY:-}" ] || { echo 'PEPPY_RELEASE_RPM_GPG_KEY is required' >&2; exit 1; }
[ -n "${PEPPY_RELEASE_RPM_GPG_KEY_ID:-}" ] || { echo 'PEPPY_RELEASE_RPM_GPG_KEY_ID is required' >&2; exit 1; }
[ -z "${PEPPY_RELEASE_RPM_GPG_PASSPHRASE:-}" ] || { echo 'RPM signing requires an unprotected, isolated CI key; rpmsign does not support a secure passphrase argument' >&2; exit 1; }
gpg_home=''
rpm_db=''
staging_dir=''
cleanup() {
  if [ -n "$gpg_home" ]; then
    gpgconf --homedir "$gpg_home" --kill gpg-agent >/dev/null 2>&1 || true
    rm -rf -- "$gpg_home"
  fi
  [ -z "$rpm_db" ] || rm -rf -- "$rpm_db"
  [ -z "$staging_dir" ] || rm -rf -- "$staging_dir"
}
trap cleanup EXIT
gpg_home=$(mktemp -d)
export GNUPGHOME=$gpg_home
printf '%s' "$PEPPY_RELEASE_RPM_GPG_KEY" | gpg --batch --import
rpm_db=$(mktemp -d)
public_key="$gpg_home/public-key.asc"
gpg --batch --armor --export "$PEPPY_RELEASE_RPM_GPG_KEY_ID" > "$public_key"
rpm --dbpath "$rpm_db" --import "$public_key"
gpg_binary=$(command -v gpg)
key_id=${PEPPY_RELEASE_RPM_GPG_KEY_ID: -8}
staging_dir=$(mktemp -d "$rpm_dir/.peppy-rpm-sign.XXXXXX")
for rpm in "$rpm_dir"/*.rpm; do
  [ -e "$rpm" ] || { echo 'required RPM bundle missing' >&2; exit 1; }
  staged_rpm="$staging_dir/$(basename -- "$rpm")"
  cp -p -- "$rpm" "$staged_rpm"
  rpmsign --addsign --define "__gpg $gpg_binary" --define "_gpg_name $PEPPY_RELEASE_RPM_GPG_KEY_ID" "$staged_rpm"
  verification=$(rpm --dbpath "$rpm_db" -Kv "$staged_rpm")
  printf '%s\n' "$verification" | grep -Eiq "(RSA|DSA).*key ID ${key_id}: OK" || { echo "RPM signature key verification failed: $rpm" >&2; exit 1; }
  printf '%s\n' "$verification" | grep -Eq 'digest.*OK' || { echo "RPM digest verification failed: $rpm" >&2; exit 1; }
  mv -f -- "$staged_rpm" "$rpm"
done
