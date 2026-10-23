#!/usr/bin/env bash
set -euo pipefail
set +x

[ "$#" -eq 3 ] || { echo "usage: $0 <unsigned-apk> <unsigned-aab> <output-dir>" >&2; exit 2; }
unsigned_apk=$1
unsigned_aab=$2
output_dir=$3
[ -f "$unsigned_apk" ] || { echo "unsigned APK not found: $unsigned_apk" >&2; exit 1; }
[ -f "$unsigned_aab" ] || { echo "unsigned AAB not found: $unsigned_aab" >&2; exit 1; }
mkdir -p "$output_dir"
output_dir=$(CDPATH='' cd -- "$output_dir" && pwd -P)
final_apk="$output_dir/peppy-android.apk"
final_aab="$output_dir/peppy-android.aab"
rm -f -- "$final_apk" "$final_aab"

for required_variable in PEPPY_ANDROID_KEYSTORE_BASE64 PEPPY_ANDROID_KEYSTORE_PASSWORD PEPPY_ANDROID_KEY_ALIAS PEPPY_ANDROID_KEY_PASSWORD PEPPY_ANDROID_SIGNING_CERT_SHA256; do
  [ -n "${!required_variable:-}" ] || { echo "$required_variable is required" >&2; exit 1; }
done
sdk_root=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}
[ -n "$sdk_root" ] || { echo 'ANDROID_HOME or ANDROID_SDK_ROOT is required' >&2; exit 1; }
build_tools="$sdk_root/build-tools/35.0.0"
zipalign="$build_tools/zipalign"
apksigner="$build_tools/apksigner"
[ -x "$zipalign" ] || { echo "zipalign 35.0.0 not found: $zipalign" >&2; exit 1; }
[ -x "$apksigner" ] || { echo "apksigner 35.0.0 not found: $apksigner" >&2; exit 1; }
command -v jarsigner >/dev/null || { echo 'jarsigner is required' >&2; exit 1; }
umask 077
temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/peppy-android-signing.XXXXXX")
trap 'rm -rf "$temporary_dir"' EXIT
keystore="$temporary_dir/release.keystore"
encoded_keystore=$(printf '%s' "$PEPPY_ANDROID_KEYSTORE_BASE64" | tr -d '[:space:]')
if [ -z "$encoded_keystore" ] ||
  [[ ! "$encoded_keystore" =~ ^[A-Za-z0-9+/]*={0,2}$ ]] ||
  [ $(( ${#encoded_keystore} % 4 )) -ne 0 ]; then
  echo 'PEPPY_ANDROID_KEYSTORE_BASE64 is not valid base64' >&2
  exit 1
fi

if base64 --decode > "$keystore" 2>/dev/null <<<"$encoded_keystore"; then
  :
elif base64 -D > "$keystore" 2>/dev/null <<<"$encoded_keystore"; then
  :
else
  echo 'PEPPY_ANDROID_KEYSTORE_BASE64 is not valid base64' >&2
  exit 1
fi
[ -s "$keystore" ] || { echo 'decoded Android keystore is empty' >&2; exit 1; }
chmod 600 "$keystore"

"$zipalign" -f -P 16 4 "$unsigned_apk" "$temporary_dir/aligned.apk"
"$apksigner" sign --ks "$keystore" --ks-key-alias "$PEPPY_ANDROID_KEY_ALIAS" \
  --ks-pass env:PEPPY_ANDROID_KEYSTORE_PASSWORD --key-pass env:PEPPY_ANDROID_KEY_PASSWORD \
  --out "$temporary_dir/peppy-android.apk" "$temporary_dir/aligned.apk"
jarsigner -keystore "$keystore" -storepass:env PEPPY_ANDROID_KEYSTORE_PASSWORD \
  -keypass:env PEPPY_ANDROID_KEY_PASSWORD -signedjar "$temporary_dir/peppy-android.aab" \
  "$unsigned_aab" "$PEPPY_ANDROID_KEY_ALIAS"

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
"$script_dir/verify-android-artifacts.sh" "$temporary_dir/peppy-android.apk" "$temporary_dir/peppy-android.aab"
mv -- "$temporary_dir/peppy-android.apk" "$final_apk"
mv -- "$temporary_dir/peppy-android.aab" "$final_aab"
