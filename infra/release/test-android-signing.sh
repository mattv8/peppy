#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
"$script_dir/test-android-abi-apks.sh"
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
[ "${ZIPALIGN_FAIL:-}" != 1 ] || { printf 'zipalign failed intentionally\n' >&2; exit 1; }
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
    if grep -aFq wrong "$artifact"; then fingerprint=BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB; else fingerprint=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA; fi
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
    if grep -aFq unsigned "$artifact"; then
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
if grep -aFq mixed "$artifact"; then
  printf 'Signer #1:\nSHA256: BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB\nSigner #2:\nSHA256: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n'
  exit 0
fi
if grep -aFq wrong "$artifact"; then fingerprint=BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB; else fingerprint=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA; fi
printf 'Signer #1:\nSHA256: %s\n' "$fingerprint"
SH
chmod +x "$tools/zipalign" "$tools/apksigner" "$bin/jarsigner" "$bin/keytool"
cat > "$bin/mv" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${PEPPY_TEST_MV_FAIL:-}" = 1 ]; then
  count=0
  [ ! -f "$PEPPY_TEST_MV_COUNT" ] || count=$(cat "$PEPPY_TEST_MV_COUNT")
  count=$((count + 1))
  printf '%s\n' "$count" > "$PEPPY_TEST_MV_COUNT"
  [ "$count" -ne 2 ] || { echo 'mv failed intentionally' >&2; exit 1; }
fi
exec /bin/mv "$@"
SH
chmod +x "$bin/mv"

expect_failure_reason() {
  local expected=$1
  shift
  local output
  if output=$("$@" 2>&1); then
    printf 'expected command to fail: %s\n' "$*" >&2
    exit 1
  fi
  if [[ $output != *"$expected"* ]]; then
    printf 'expected failure reason %q, got:\n%s\n' "$expected" "$output" >&2
    exit 1
  fi
}

make_aligned_archive() {
  local artifact=$1 marker=${2:-}
  local library="$root/libfixture.so"
  "$script_dir/create-elf-fixture.py" "$library"
  python3 - "$artifact" "$library" "$marker" <<'PY'
import pathlib
import sys
import zipfile

with zipfile.ZipFile(sys.argv[1], 'w') as archive:
    archive.writestr('base/lib/arm64-v8a/libfixture.so', pathlib.Path(sys.argv[2]).read_bytes())
    archive.writestr('lib/arm64-v8a/libfixture.so', pathlib.Path(sys.argv[2]).read_bytes())
    archive.writestr('lib/x86_64/libfixture.so', pathlib.Path(sys.argv[2]).read_bytes() + b'x86')
    archive.writestr('classes.dex', b'fixture dex')
    if sys.argv[3]:
        archive.writestr(sys.argv[3], sys.argv[3])
PY
}

unsigned_apk="$root/unsigned.apk"
unsigned_aab="$root/unsigned.aab"
make_aligned_archive "$unsigned_apk"
make_aligned_archive "$unsigned_aab"
export ANDROID_HOME="$sdk" PEPPY_TEST_LOG="$log" PEPPY_TEST_KEYSTORE_PATH="$root/keystore-path"
export PATH="$bin:$PATH"

mkdir -p "$root/out"
printf stale > "$root/out/peppy-android.apk"
printf stale > "$root/out/peppy-android.aab"
expect_failure_reason 'PEPPY_ANDROID_KEYSTORE_BASE64 is required' "$script_dir/sign-android-artifacts.sh" "$unsigned_apk" "$unsigned_aab" "$root/out"
test ! -e "$root/out/peppy-android.apk"
test ! -e "$root/out/peppy-android.aab"
expect_failure_reason 'must contain exactly 64 hexadecimal digits' env PEPPY_ANDROID_SIGNING_CERT_SHA256=bad "$script_dir/verify-android-artifacts.sh" "$unsigned_apk" "$unsigned_aab"

export PEPPY_ANDROID_KEYSTORE_BASE64
PEPPY_ANDROID_KEYSTORE_BASE64=$(printf test-keystore | base64 | tr -d '\n')
export PEPPY_ANDROID_KEYSTORE_PASSWORD=store-secret
export PEPPY_ANDROID_KEY_ALIAS=release
export PEPPY_ANDROID_KEY_PASSWORD=key-secret
export PEPPY_ANDROID_SIGNING_CERT_SHA256="AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA"
export PEPPY_TEST_MV_COUNT="$root/mv-count"

expect_failure_reason 'is not valid base64' env PEPPY_ANDROID_KEYSTORE_BASE64='not base64!' "$script_dir/sign-android-artifacts.sh" "$unsigned_apk" "$unsigned_aab" "$root/out"
printf stale > "$root/out/peppy-android.apk"
printf stale > "$root/out/peppy-android.aab"
expect_failure_reason 'zipalign failed intentionally' env ZIPALIGN_FAIL=1 "$script_dir/sign-android-artifacts.sh" "$unsigned_apk" "$unsigned_aab" "$root/out"
test ! -e "$root/out/peppy-android.apk"
test ! -e "$root/out/peppy-android.aab"

expect_failure_reason 'mv failed intentionally' env signing_complete=1 PEPPY_TEST_MV_FAIL=1 "$script_dir/sign-android-artifacts.sh" "$unsigned_apk" "$unsigned_aab" "$root/out"
for artifact in peppy-android.apk peppy-android.aab peppy-android-arm64-v8a.apk peppy-android-x86_64.apk; do
  test ! -e "$root/out/$artifact"
done
if compgen -G "$root/out/.peppy-android-signing-stage.*" >/dev/null; then
  printf 'failed signing left a staging directory\n' >&2
  exit 1
fi

mkdir -p "$root/read-only-parent/out"
(
  trap 'chmod 0755 "$root/read-only-parent"' EXIT
  chmod 0555 "$root/read-only-parent"
  "$script_dir/sign-android-artifacts.sh" "$unsigned_apk" "$unsigned_aab" "$root/read-only-parent/out"
  for artifact in peppy-android.apk peppy-android.aab peppy-android-arm64-v8a.apk peppy-android-x86_64.apk; do
    test -f "$root/read-only-parent/out/$artifact"
  done
  if compgen -G "$root/read-only-parent/out/.peppy-android-signing-stage.*" >/dev/null; then
    printf 'successful signing left a staging directory\n' >&2
    exit 1
  fi
)

"$script_dir/sign-android-artifacts.sh" "$unsigned_apk" "$unsigned_aab" "$root/out"
test -f "$root/out/peppy-android.apk"
test -f "$root/out/peppy-android.aab"
test -f "$root/out/peppy-android-arm64-v8a.apk"
test -f "$root/out/peppy-android-x86_64.apk"
test ! -e "$(<"$root/keystore-path")"
grep -Fq 'zipalign -f -P 16 4' "$log"
grep -Fq 'apksigner sign' "$log"
if grep -Fq "$PEPPY_ANDROID_KEYSTORE_PASSWORD" "$log" || grep -Fq "$PEPPY_ANDROID_KEY_PASSWORD" "$log"; then
  printf 'secret appeared in tool arguments\n' >&2
  exit 1
fi

"$script_dir/verify-android-artifacts.sh" "$root/out/peppy-android.apk" "$root/out/peppy-android.aab"
"$script_dir/verify-android-artifacts.sh" "$root/out/peppy-android.apk" "$root/out/peppy-android.aab" \
  "$root/out/peppy-android-arm64-v8a.apk" "$root/out/peppy-android-x86_64.apk"
python3 - "$root/out/peppy-android-arm64-v8a.apk" "$root" <<'PY'
import pathlib
import sys
import zipfile

source = pathlib.Path(sys.argv[1])
root = pathlib.Path(sys.argv[2])
for variant in ("extra", "modified", "wrong-abi", "both"):
    with zipfile.ZipFile(source) as original, zipfile.ZipFile(root / f"{variant}.apk", "w") as target:
        for entry in original.infolist():
            data = original.read(entry)
            if variant == "modified" and entry.filename == "classes.dex":
                data = data[:-1] + bytes([data[-1] ^ 1])
            if variant == "wrong-abi" and entry.filename.startswith("lib/arm64-v8a/"):
                entry.filename = entry.filename.replace("arm64-v8a", "x86_64")
            target.writestr(entry, data)
        if variant == "extra":
            target.writestr("extra", "extra")
        if variant == "both":
            target.writestr("lib/x86_64/libfixture.so", original.read("lib/arm64-v8a/libfixture.so"))
PY
for variant in extra modified wrong-abi both; do
  expect_failure_reason 'ABI APK verification failed' "$script_dir/verify-android-artifacts.sh" \
    "$root/out/peppy-android.apk" "$root/out/peppy-android.aab" "$root/$variant.apk" \
    "$root/out/peppy-android-x86_64.apk"
done
expect_failure_reason 'expected ABI' "$script_dir/verify-android-artifacts.sh" \
  "$root/out/peppy-android.apk" "$root/out/peppy-android.aab" \
  "$root/out/peppy-android-arm64-v8a.apk" "$root/out/peppy-android-arm64-v8a.apk"
expect_failure_reason 'expected ABI' "$script_dir/verify-android-artifacts.sh" \
  "$root/out/peppy-android.apk" "$root/out/peppy-android.aab" \
  "$root/out/peppy-android-x86_64.apk" "$root/out/peppy-android-arm64-v8a.apk"
for count in 3 5; do
  args=("$root/out/peppy-android.apk" "$root/out/peppy-android.aab" "$root/out/peppy-android-arm64-v8a.apk")
  if [ "$count" -eq 5 ]; then
    args+=("$root/out/peppy-android-x86_64.apk" "$root/out/peppy-android.apk")
  fi
  status=0
  output=$("$script_dir/verify-android-artifacts.sh" "${args[@]}" 2>&1) || status=$?
  [ "$status" -eq 2 ] && [[ "$output" = *usage:* ]] || { printf 'expected usage exit 2 for %s arguments\n' "$count" >&2; exit 1; }
done
make_aligned_archive "$root/unsigned-signed.aab" unsigned
expect_failure_reason 'AAB strict signature verification failed' "$script_dir/verify-android-artifacts.sh" "$root/out/peppy-android.apk" "$root/unsigned-signed.aab"
make_aligned_archive "$root/wrong.apk" wrong
expect_failure_reason 'APK signer certificate does not match' "$script_dir/verify-android-artifacts.sh" "$root/wrong.apk" "$root/out/peppy-android.aab"
make_aligned_archive "$root/wrong.aab" wrong
expect_failure_reason 'AAB signer certificate does not match' "$script_dir/verify-android-artifacts.sh" "$root/out/peppy-android.apk" "$root/wrong.aab"
make_aligned_archive "$root/mixed.aab" mixed
expect_failure_reason 'AAB must have exactly one signer' "$script_dir/verify-android-artifacts.sh" "$root/out/peppy-android.apk" "$root/mixed.aab"
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
mkdir -p "$real_root/payload/lib/arm64-v8a"
"$script_dir/create-elf-fixture.py" "$real_root/payload/lib/arm64-v8a/libfixture.so"
python3 - "$real_root/unsigned.apk" "$real_root/payload/lib/arm64-v8a/libfixture.so" <<'PY'
import pathlib
import sys
import zipfile

with zipfile.ZipFile(sys.argv[1], 'a', compression=zipfile.ZIP_STORED) as archive:
    archive.writestr('lib/arm64-v8a/libfixture.so', pathlib.Path(sys.argv[2]).read_bytes())
    archive.writestr('lib/x86_64/libfixture.so', pathlib.Path(sys.argv[2]).read_bytes() + b'x86')
PY
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
expect_failure_reason 'AAB signer certificate does not match' "$script_dir/verify-android-artifacts.sh" "$real_root/out/peppy-android.apk" "$real_root/wrong-certificate.aab"
expect_failure_reason 'APK signer certificate does not match' env PEPPY_ANDROID_SIGNING_CERT_SHA256="$alternate_fingerprint" "$script_dir/verify-android-artifacts.sh" "$real_root/out/peppy-android.apk" "$real_root/out/peppy-android.aab"
python3 - "$real_root/out/peppy-android.aab" "$real_root/tampered.aab" <<'PY'
import sys
import zipfile

with zipfile.ZipFile(sys.argv[1]) as source, zipfile.ZipFile(sys.argv[2], "w") as target:
    for entry in source.infolist():
        target.writestr(entry, b"tampered" if entry.filename == "file" else source.read(entry.filename))
PY
expect_failure_reason 'AAB signer certificate could not be read' "$script_dir/verify-android-artifacts.sh" "$real_root/out/peppy-android.apk" "$real_root/tampered.aab"
cp "$real_root/out/peppy-android.aab" "$real_root/unsigned-entry.aab"
python3 - "$real_root/unsigned-entry.aab" <<'PY'
import sys
import zipfile

with zipfile.ZipFile(sys.argv[1], "a") as archive:
    archive.writestr("unsigned-entry", "unsigned")
PY
expect_failure_reason 'AAB strict signature verification failed' "$script_dir/verify-android-artifacts.sh" "$real_root/out/peppy-android.apk" "$real_root/unsigned-entry.aab"
expect_failure_reason 'AAB must have exactly one signer' "$script_dir/verify-android-artifacts.sh" "$real_root/out/peppy-android.apk" "$real_root/unsigned.aab"
cp "$real_root/unsigned.aab" "$real_root/multi-signer.aab"
"$caller_java_home/bin/jarsigner" -keystore "$real_root/release.p12" -storepass changeit -keypass changeit "$real_root/multi-signer.aab" release >/dev/null
"$caller_java_home/bin/jarsigner" -keystore "$real_root/alternate.p12" -storepass changeit -keypass changeit "$real_root/multi-signer.aab" alternate >/dev/null
expect_failure_reason 'AAB must have exactly one signer' "$script_dir/verify-android-artifacts.sh" "$real_root/out/peppy-android.apk" "$real_root/multi-signer.aab"
printf 'android signing tests passed\n'
