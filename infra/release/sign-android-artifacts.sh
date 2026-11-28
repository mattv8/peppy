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
final_arm64="$output_dir/peppy-android-arm64-v8a.apk"
final_x86="$output_dir/peppy-android-x86_64.apk"
rm -f -- "$final_apk" "$final_aab" "$final_arm64" "$final_x86"

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
temporary_dir=$(mktemp -d "$output_dir/.peppy-android-signing-stage.XXXXXX")
created_finals=()
signing_complete=0
cleanup() {
  if [ "${signing_complete:-}" != 1 ]; then
    for final in ${created_finals[@]+"${created_finals[@]}"}; do rm -f -- "$final"; done
  fi
  rm -rf "$temporary_dir"
}
trap cleanup EXIT
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
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
"$script_dir/split-android-apk-abis.py" "$unsigned_apk" "$temporary_dir"

align_and_sign_apk() {
  local input=$1 aligned=$2 output=$3
  "$zipalign" -f -P 16 4 "$input" "$aligned"
  "$apksigner" sign --ks "$keystore" --ks-key-alias "$PEPPY_ANDROID_KEY_ALIAS" \
    --ks-pass env:PEPPY_ANDROID_KEYSTORE_PASSWORD --key-pass env:PEPPY_ANDROID_KEY_PASSWORD \
    --out "$output" "$aligned"
}
align_and_sign_apk "$unsigned_apk" "$temporary_dir/aligned.apk" "$temporary_dir/peppy-android.apk"
for abi in arm64-v8a x86_64; do
  align_and_sign_apk "$temporary_dir/peppy-android-$abi-unsigned.apk" "$temporary_dir/aligned-$abi.apk" \
    "$temporary_dir/peppy-android-$abi.apk"
done
jarsigner -keystore "$keystore" -storepass:env PEPPY_ANDROID_KEYSTORE_PASSWORD \
  -keypass:env PEPPY_ANDROID_KEY_PASSWORD -signedjar "$temporary_dir/peppy-android.aab" \
  "$unsigned_aab" "$PEPPY_ANDROID_KEY_ALIAS"

"$script_dir/verify-android-artifacts.sh" "$temporary_dir/peppy-android.apk" "$temporary_dir/peppy-android.aab" \
  "$temporary_dir/peppy-android-arm64-v8a.apk" "$temporary_dir/peppy-android-x86_64.apk"
mv -- "$temporary_dir/peppy-android.apk" "$final_apk"
created_finals+=("$final_apk")
mv -- "$temporary_dir/peppy-android.aab" "$final_aab"
created_finals+=("$final_aab")
mv -- "$temporary_dir/peppy-android-arm64-v8a.apk" "$final_arm64"
created_finals+=("$final_arm64")
mv -- "$temporary_dir/peppy-android-x86_64.apk" "$final_x86"
created_finals+=("$final_x86")
signing_complete=1
