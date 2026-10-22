#!/usr/bin/env bash
set -euo pipefail
set +x
export LC_ALL=C
umask 077

[ "$#" -eq 2 ] || { echo "usage: $0 <signed-apk> <signed-aab>" >&2; exit 2; }
apk=$1
aab=$2
[ -f "$apk" ] || { echo "signed APK not found: $apk" >&2; exit 1; }
[ -f "$aab" ] || { echo "signed AAB not found: $aab" >&2; exit 1; }

normalize_fingerprint() {
  local value
  value=$(printf '%s' "$1" | tr -d '[:space:]:' | tr '[:lower:]' '[:upper:]')
  [[ "$value" =~ ^[0-9A-F]{64}$ ]] || return 1
  printf '%s\n' "$value"
}

expected_fingerprint=$(normalize_fingerprint "${PEPPY_ANDROID_SIGNING_CERT_SHA256:-}") || {
  echo 'PEPPY_ANDROID_SIGNING_CERT_SHA256 must contain exactly 64 hexadecimal digits' >&2
  exit 1
}
sdk_root=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}
[ -n "$sdk_root" ] || { echo 'ANDROID_HOME or ANDROID_SDK_ROOT is required' >&2; exit 1; }
build_tools="$sdk_root/build-tools/35.0.0"
apksigner="$build_tools/apksigner"
zipalign="$build_tools/zipalign"
[ -x "$apksigner" ] || { echo "apksigner 35.0.0 not found: $apksigner" >&2; exit 1; }
[ -x "$zipalign" ] || { echo "zipalign 35.0.0 not found: $zipalign" >&2; exit 1; }
jarsigner=$(command -v jarsigner) || { echo 'jarsigner is required' >&2; exit 1; }
keytool=$(command -v keytool) || { echo 'keytool is required' >&2; exit 1; }

apk_verification=$("$apksigner" verify --verbose --print-certs "$apk") || {
  echo 'APK signature verification failed' >&2
  exit 1
}
"$zipalign" -c -P 16 -v 4 "$apk" >/dev/null || {
  echo 'APK is not aligned for 16-KiB native libraries' >&2
  exit 1
}
apk_fingerprints=$(printf '%s\n' "$apk_verification" | awk '/Signer #[0-9]+ certificate SHA-256 digest:/{print $NF}')
[ "$(printf '%s\n' "$apk_fingerprints" | grep -c .)" -eq 1 ] || {
  echo 'APK must have exactly one signing certificate' >&2
  exit 1
}
actual_apk_fingerprint=$(normalize_fingerprint "$apk_fingerprints") || {
  echo 'APK signer did not provide a valid SHA-256 certificate fingerprint' >&2
  exit 1
}
[ "$actual_apk_fingerprint" = "$expected_fingerprint" ] || {
  echo 'APK signer certificate does not match PEPPY_ANDROID_SIGNING_CERT_SHA256' >&2
  exit 1
}

aab_certificate=$("$keytool" -J-Duser.language=en -J-Duser.country=US -printcert -jarfile "$aab") || {
  echo 'AAB signer certificate could not be read' >&2
  exit 1
}
aab_signers=$(printf '%s\n' "$aab_certificate" | grep -Ec '^Signer #[0-9]+:$' || true)
[ "$aab_signers" -eq 1 ] || {
  echo 'AAB must have exactly one signer' >&2
  exit 1
}
aab_fingerprint=$(printf '%s\n' "$aab_certificate" | awk '/^[[:space:]]*SHA256: / {sub(/^[[:space:]]*SHA256: /, ""); print; exit}')
[ -n "$aab_fingerprint" ] || {
  echo 'AAB has no signer certificate fingerprint' >&2
  exit 1
}
actual_aab_fingerprint=$(normalize_fingerprint "$aab_fingerprint") || {
  echo 'AAB signer did not provide a valid SHA-256 certificate fingerprint' >&2
  exit 1
}
[ "$actual_aab_fingerprint" = "$expected_fingerprint" ] || {
  echo 'AAB signer certificate does not match PEPPY_ANDROID_SIGNING_CERT_SHA256' >&2
  exit 1
}

temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/peppy-android-verification.XXXXXX")
trap 'rm -rf "$temporary_dir"' EXIT
signer_certificate="$temporary_dir/signer.pem"
truststore="$temporary_dir/truststore.p12"
"$keytool" -J-Duser.language=en -J-Duser.country=US -printcert -rfc -jarfile "$aab" | awk '
  /-----BEGIN CERTIFICATE-----/ { capturing = 1 }
  capturing { print }
  /-----END CERTIFICATE-----/ { exit }
' > "$signer_certificate"
[ -s "$signer_certificate" ] || { echo 'AAB signer certificate could not be exported' >&2; exit 1; }
export PEPPY_ANDROID_TRUSTSTORE_PASSWORD=peppy-temporary-truststore
"$keytool" -J-Duser.language=en -J-Duser.country=US -importcert -noprompt -alias release-signer -file "$signer_certificate" \
  -keystore "$truststore" -storetype PKCS12 -storepass:env PEPPY_ANDROID_TRUSTSTORE_PASSWORD >/dev/null || {
  echo 'AAB signer certificate could not be trusted' >&2
  exit 1
}
"$jarsigner" -J-Duser.language=en -J-Duser.country=US -verify -strict -certs -keystore "$truststore" \
  -storepass:env PEPPY_ANDROID_TRUSTSTORE_PASSWORD "$aab" >/dev/null || {
  echo 'AAB strict signature verification failed' >&2
  exit 1
}
