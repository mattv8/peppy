#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
caller_java_home=${JAVA_HOME:-}
caller_sdk_root=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}
caller_path=$PATH
[ -n "$caller_java_home" ] || { printf 'JAVA_HOME is required for real Android signing tests\n' >&2; exit 1; }
[ -n "$caller_sdk_root" ] || { printf 'ANDROID_HOME or ANDROID_SDK_ROOT is required for real Android signing tests\n' >&2; exit 1; }
for tool in "$caller_java_home/bin/jar" "$caller_java_home/bin/jarsigner" "$caller_java_home/bin/keytool" "$caller_sdk_root/build-tools/35.0.0/aapt2" "$caller_sdk_root/build-tools/35.0.0/apksigner" "$caller_sdk_root/build-tools/35.0.0/zipalign"; do
  [ -x "$tool" ] || { printf 'required real Android signing tool missing: %s\n' "$tool" >&2; exit 1; }
done
[ -f "$caller_sdk_root/platforms/android-35/android.jar" ] || { printf 'required Android platform missing: %s\n' "$caller_sdk_root/platforms/android-35/android.jar" >&2; exit 1; }
root=$(mktemp -d "${TMPDIR:-/tmp}/peppy-android-signing-test.XXXXXX")
trap 'rm -rf "$root"' EXIT
sdk="$root/sdk"
tools="$sdk/build-tools/35.0.0"
bin="$root/bin"
log="$root/tool.log"
mkdir -p "$tools" "$bin"

cat > "$tools/zipalign" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'zipalign %s\n' "$*" >> "$PEPPY_TEST_LOG"
[ "${ZIPALIGN_FAIL:-}" != 1 ] || exit 1
[ "$1" = -c ] && exit 0
cp "${@: -2:1}" "${@: -1}"
SH
cat > "$tools/apksigner" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'apksigner %s\n' "$*" >> "$PEPPY_TEST_LOG"
case "$1" in
  sign)
    [ "${APKSIGNER_FAIL:-}" != 1 ] || exit 1
    for ((i = 1; i <= $#; i++)); do
      if [ "${!i}" = --ks ]; then j=$((i + 1)); printf '%s\n' "${!j}" > "$PEPPY_TEST_KEYSTORE_PATH"; fi
      if [ "${!i}" = --out ]; then j=$((i + 1)); out=${!j}; fi
    done
    cp "${@: -1}" "$out"
    ;;
  verify)
    artifact=${@: -1}
    if grep -q wrong "$artifact"; then fingerprint=BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB; else fingerprint=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA; fi
    printf 'Signer #1 certificate SHA-256 digest: %s\n' "$fingerprint"
    ;;
  *) exit 2 ;;
esac
SH
cat > "$bin/jarsigner" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'jarsigner %s\n' "$*" >> "$PEPPY_TEST_LOG"
if [[ " $* " = *' -verify '* ]]; then
  artifact=${@: -1}
  if grep -q unsigned "$artifact"; then
    if [[ " $* " = *' -strict '* ]]; then exit 1; fi
    printf 'jar is unsigned.\n'
  else
    printf 'jar verified.\n'
  fi
  exit 0
fi
for ((i = 1; i <= $#; i++)); do
  if [ "${!i}" = -signedjar ]; then j=$((i + 1)); out=${!j}; fi
done
cp "${@: -2:1}" "$out"
SH
cat > "$bin/keytool" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
artifact=${@: -1}
if [[ " $* " = *' -importcert '* ]]; then exit 0; fi
if [[ " $* " = *' -rfc '* ]]; then
  printf '%s\n' '-----BEGIN CERTIFICATE-----' 'ZmFrZQ==' '-----END CERTIFICATE-----'
  exit 0
fi
if grep -q mixed "$artifact"; then
  printf 'Signer #1:\nSHA256: BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB\nSHA256: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n'
  exit 0
fi
if grep -q wrong "$artifact"; then fingerprint=BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB; else fingerprint=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA; fi
printf 'Signer #1:\nSHA256: %s\n' "$fingerprint"
SH
chmod +x "$tools/zipalign" "$tools/apksigner" "$bin/jarsigner" "$bin/keytool"

expect_failure() {
  if "$@" >/dev/null 2>&1; then
    printf 'expected command to fail: %s\n' "$*" >&2
    exit 1
  fi
}

unsigned_apk="$root/unsigned.apk"
unsigned_aab="$root/unsigned.aab"
printf valid > "$unsigned_apk"
printf valid > "$unsigned_aab"
export ANDROID_HOME="$sdk" PEPPY_TEST_LOG="$log" PEPPY_TEST_KEYSTORE_PATH="$root/keystore-path"
export PATH="$bin:$PATH"

mkdir -p "$root/out"
printf stale > "$root/out/peppy-android.apk"
printf stale > "$root/out/peppy-android.aab"
expect_failure "$script_dir/sign-android-artifacts.sh" "$unsigned_apk" "$unsigned_aab" "$root/out"
test ! -e "$root/out/peppy-android.apk"
test ! -e "$root/out/peppy-android.aab"
expect_failure env PEPPY_ANDROID_SIGNING_CERT_SHA256=bad "$script_dir/verify-android-artifacts.sh" "$unsigned_apk" "$unsigned_aab"

export PEPPY_ANDROID_KEYSTORE_BASE64
PEPPY_ANDROID_KEYSTORE_BASE64=$(printf test-keystore | base64 | tr -d '\n')
export PEPPY_ANDROID_KEYSTORE_PASSWORD=store-secret
export PEPPY_ANDROID_KEY_ALIAS=release
export PEPPY_ANDROID_KEY_PASSWORD=key-secret
export PEPPY_ANDROID_SIGNING_CERT_SHA256="AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA"

expect_failure env PEPPY_ANDROID_KEYSTORE_BASE64='not base64!' "$script_dir/sign-android-artifacts.sh" "$unsigned_apk" "$unsigned_aab" "$root/out"
printf stale > "$root/out/peppy-android.apk"
printf stale > "$root/out/peppy-android.aab"
expect_failure env ZIPALIGN_FAIL=1 "$script_dir/sign-android-artifacts.sh" "$unsigned_apk" "$unsigned_aab" "$root/out"
test ! -e "$root/out/peppy-android.apk"
test ! -e "$root/out/peppy-android.aab"

"$script_dir/sign-android-artifacts.sh" "$unsigned_apk" "$unsigned_aab" "$root/out"
test -f "$root/out/peppy-android.apk"
test -f "$root/out/peppy-android.aab"
test ! -e "$(<"$root/keystore-path")"
grep -Fq 'zipalign -f -P 16 4' "$log"
grep -Fq 'apksigner sign' "$log"
if grep -Fq "$PEPPY_ANDROID_KEYSTORE_PASSWORD" "$log" || grep -Fq "$PEPPY_ANDROID_KEY_PASSWORD" "$log"; then
  printf 'secret appeared in tool arguments\n' >&2
  exit 1
fi

"$script_dir/verify-android-artifacts.sh" "$root/out/peppy-android.apk" "$root/out/peppy-android.aab"
printf unsigned > "$root/unsigned-signed.aab"
expect_failure "$script_dir/verify-android-artifacts.sh" "$root/out/peppy-android.apk" "$root/unsigned-signed.aab"
printf wrong > "$root/wrong.apk"
expect_failure "$script_dir/verify-android-artifacts.sh" "$root/wrong.apk" "$root/out/peppy-android.aab"
printf wrong > "$root/wrong.aab"
expect_failure "$script_dir/verify-android-artifacts.sh" "$root/out/peppy-android.apk" "$root/wrong.aab"
printf mixed > "$root/mixed.aab"
expect_failure "$script_dir/verify-android-artifacts.sh" "$root/out/peppy-android.apk" "$root/mixed.aab"
real_root="$root/real-jdk"
mkdir -p "$real_root/payload"
printf release > "$real_root/payload/file"
export PATH="$caller_java_home/bin:$caller_path" ANDROID_HOME="$caller_sdk_root"
cat > "$real_root/AndroidManifest.xml" <<'XML'
<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="dev.peppy.signingtest">
  <uses-sdk android:minSdkVersion="23" />
</manifest>
XML
"$caller_sdk_root/build-tools/35.0.0/aapt2" link -I "$caller_sdk_root/platforms/android-35/android.jar" --manifest "$real_root/AndroidManifest.xml" -o "$real_root/unsigned.apk"
"$caller_java_home/bin/jar" --create --file "$real_root/unsigned.aab" -C "$real_root/payload" .
"$caller_java_home/bin/keytool" -J-Duser.language=en -J-Duser.country=US -genkeypair -alias release -keystore "$real_root/release.p12" -storetype PKCS12 -storepass changeit -keypass changeit -dname 'CN=release' -keyalg RSA -validity 365 >/dev/null
"$caller_java_home/bin/keytool" -J-Duser.language=en -J-Duser.country=US -genkeypair -alias alternate -keystore "$real_root/alternate.p12" -storetype PKCS12 -storepass changeit -keypass changeit -dname 'CN=alternate' -keyalg RSA -validity 365 >/dev/null
real_fingerprint=$("$caller_java_home/bin/keytool" -J-Duser.language=en -J-Duser.country=US -list -v -keystore "$real_root/release.p12" -storepass changeit -alias release | awk -F': ' '/SHA256:/{print $2; exit}')
alternate_fingerprint=$("$caller_java_home/bin/keytool" -J-Duser.language=en -J-Duser.country=US -list -v -keystore "$real_root/alternate.p12" -storepass changeit -alias alternate | awk -F': ' '/SHA256:/{print $2; exit}')
export PEPPY_ANDROID_KEYSTORE_BASE64
PEPPY_ANDROID_KEYSTORE_BASE64=$(base64 < "$real_root/release.p12" | tr -d '\n')
export PEPPY_ANDROID_KEYSTORE_PASSWORD=changeit PEPPY_ANDROID_KEY_ALIAS=release PEPPY_ANDROID_KEY_PASSWORD=changeit PEPPY_ANDROID_SIGNING_CERT_SHA256="$real_fingerprint"
"$script_dir/sign-android-artifacts.sh" "$real_root/unsigned.apk" "$real_root/unsigned.aab" "$real_root/out"
"$script_dir/verify-android-artifacts.sh" "$real_root/out/peppy-android.apk" "$real_root/out/peppy-android.aab"
cp "$real_root/unsigned.aab" "$real_root/wrong-certificate.aab"
"$caller_java_home/bin/jarsigner" -keystore "$real_root/alternate.p12" -storepass changeit -keypass changeit "$real_root/wrong-certificate.aab" alternate >/dev/null
expect_failure "$script_dir/verify-android-artifacts.sh" "$real_root/out/peppy-android.apk" "$real_root/wrong-certificate.aab"
expect_failure env PEPPY_ANDROID_SIGNING_CERT_SHA256="$alternate_fingerprint" "$script_dir/verify-android-artifacts.sh" "$real_root/out/peppy-android.apk" "$real_root/out/peppy-android.aab"
python3 - "$real_root/out/peppy-android.aab" "$real_root/tampered.aab" <<'PY'
import sys
import zipfile

with zipfile.ZipFile(sys.argv[1]) as source, zipfile.ZipFile(sys.argv[2], "w") as target:
    for entry in source.infolist():
        target.writestr(entry, b"tampered" if entry.filename == "file" else source.read(entry.filename))
PY
expect_failure "$script_dir/verify-android-artifacts.sh" "$real_root/out/peppy-android.apk" "$real_root/tampered.aab"
cp "$real_root/out/peppy-android.aab" "$real_root/unsigned-entry.aab"
python3 - "$real_root/unsigned-entry.aab" <<'PY'
import sys
import zipfile

with zipfile.ZipFile(sys.argv[1], "a") as archive:
    archive.writestr("unsigned-entry", "unsigned")
PY
expect_failure "$script_dir/verify-android-artifacts.sh" "$real_root/out/peppy-android.apk" "$real_root/unsigned-entry.aab"
expect_failure "$script_dir/verify-android-artifacts.sh" "$real_root/out/peppy-android.apk" "$real_root/unsigned.aab"
cp "$real_root/unsigned.aab" "$real_root/multi-signer.aab"
"$caller_java_home/bin/jarsigner" -keystore "$real_root/release.p12" -storepass changeit -keypass changeit "$real_root/multi-signer.aab" release >/dev/null
"$caller_java_home/bin/jarsigner" -keystore "$real_root/alternate.p12" -storepass changeit -keypass changeit "$real_root/multi-signer.aab" alternate >/dev/null
expect_failure "$script_dir/verify-android-artifacts.sh" "$real_root/out/peppy-android.apk" "$real_root/multi-signer.aab"
printf 'android signing tests passed\n'
