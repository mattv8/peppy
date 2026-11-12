#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
helper="$script_dir/release-component-state.sh"
root=$(mktemp -d "${TMPDIR:-/tmp}/t41.XXXXXX")
cleanup_fixture() {
  local status=$?
  gpgconf --homedir "$root/gpg-authority" --kill gpg-agent >/dev/null 2>&1 || :
  gpgconf --homedir "$root/gpg-wrong" --kill gpg-agent >/dev/null 2>&1 || :
  rm -rf "$root" || :
  exit "$status"
}
trap cleanup_fixture EXIT

failures=0
checks=0
expect_success() {
  checks=$((checks + 1))
  if ! "$@"; then
    printf 'expected success in check %s\n' "$checks" >&2
    failures=$((failures + 1))
  fi
}

expect_failure() {
  checks=$((checks + 1))
  if "$@" >/dev/null 2>&1; then
    printf 'expected failure in check %s\n' "$checks" >&2
    failures=$((failures + 1))
  fi
}

repo="$root/repo"
git init -q "$repo"
git -C "$repo" config user.email test@example.invalid
git -C "$repo" config user.name test
printf fixture > "$repo/fixture"
git -C "$repo" add fixture
git -C "$repo" commit -qm fixture
sha=$(git -C "$repo" rev-parse HEAD)
git -C "$repo" tag v1.2.3 "$sha"

output=$(cd "$repo" && bash "$helper" discover desktop "$sha" 1.2.3)
[ "$output" = $'version=1.2.3\nexisting=true' ] || { printf 'unexpected discovery output: %s\n' "$output" >&2; failures=$((failures + 1)); }
checks=$((checks + 1))
# shellcheck disable=SC2016 # Single quotes keep $1-$3 for the child bash, which receives them as arguments.
expect_failure bash -c 'cd "$1" && bash "$2" discover android "$3" ""' _ "$repo" "$helper" "$sha"
git -C "$repo" tag v1.2.4 "$sha"
# shellcheck disable=SC2016 # Single quotes keep $1-$3 for the child bash, which receives them as arguments.
expect_failure bash -c 'cd "$1" && bash "$2" discover desktop "$3" ""' _ "$repo" "$helper" "$sha"
git -C "$repo" tag -d v1.2.4 >/dev/null
# shellcheck disable=SC2016 # Single quotes keep $1-$3 for the child bash, which receives them as arguments.
expect_failure bash -c 'cd "$1" && bash "$2" discover desktop "$3" 9.9.9' _ "$repo" "$helper" "$sha"
if (cd "$repo" && bash "$helper" discover desktop "$sha" 9.9.9) >"$root/discover.out" 2>"$root/discover.err"; then
  echo 'explicit mismatch unexpectedly succeeded' >&2
  failures=$((failures + 1))
fi
expect_success grep -Fxq 'requested version does not match the existing component tag' "$root/discover.err"

verify_repo="$root/verify-repo"
remote="$root/remote.git"
git init -q --bare "$remote"
git init -q "$verify_repo"
git -C "$verify_repo" config user.email test@example.invalid
git -C "$verify_repo" config user.name test
printf verify > "$verify_repo/fixture"
git -C "$verify_repo" add fixture
git -C "$verify_repo" commit -qm verify
verify_sha=$(git -C "$verify_repo" rev-parse HEAD)
git -C "$verify_repo" tag v2.3.4 "$verify_sha"
git -C "$verify_repo" remote add origin "$remote"
git -C "$verify_repo" push -q origin HEAD refs/tags/v2.3.4
mkdir -p "$root/bin" "$root/assets" "$root/gpg-authority" "$root/gpg-wrong"
cat > "$root/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$GH_LOG"
case "$1 $2" in
  'release view') [ "${RELEASE_MODE:-valid}" = unknown ] && exit 1; cat "$RELEASE_JSON" ;;
  'release download')
    pattern= directory=
    while (($#)); do
      case "$1" in --pattern) pattern=$2; shift 2 ;; --dir) directory=$2; shift 2 ;; *) shift ;; esac
    done
    printf '%s\n' "$pattern" >> "$DOWNLOAD_LOG"
    cp "$FIXTURE_ASSETS/$pattern" "$directory/$pattern"
    ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$root/bin/gh"
cat > "$root/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${PEPPY_RELEASE_GPG_KEY+x}" = x ] || [ "${PEPPY_RELEASE_GPG_KEY_ID+x}" = x ]; then
  echo environment-leak >&2
  exit 97
fi
printf 'cargo\n' >> "$PLATFORM_LOG"
if [ "${PLATFORM_FAILURE:-}" = cargo ]; then echo 'fixture updater rejected' >&2; exit 42; fi
EOF
chmod +x "$root/bin/cargo"
while IFS= read -r asset; do printf '%s\n' "$asset" > "$root/assets/$asset"; done <<'EOF'
peppy-2.3.4-linux-x86_64.AppImage
peppy-2.3.4-linux-x86_64.AppImage.sig
peppy-2.3.4-linux-x86_64.deb
peppy-2.3.4-linux-x86_64.deb.sig
peppy-2.3.4-linux-x86_64.rpm
peppy-2.3.4-linux-x86_64.rpm.sig
peppy-2.3.4-darwin-aarch64.app.tar.gz
peppy-2.3.4-darwin-aarch64.app.tar.gz.sig
peppy-2.3.4-windows-x86_64-setup.exe
peppy-2.3.4-windows-x86_64-setup.exe.sig
peppy-2.3.4-windows-x86_64.msi
peppy-2.3.4-windows-x86_64.msi.sig
latest.json
peppy-2.3.4-macos-aarch64.dmg
SHA256SUMS
SHA256SUMS.sig
EOF
chmod 700 "$root/gpg-authority" "$root/gpg-wrong"
generate_key() {
  local home=$1 name=$2 params
  params="$root/${name}.params"
  cat > "$params" <<EOF
Key-Type: RSA
Key-Length: 2048
Name-Real: $name
Name-Email: ${name}@example.invalid
Expire-Date: 0
%no-protection
%commit
EOF
  GNUPGHOME="$home" gpg --batch --generate-key "$params" >/dev/null 2>&1
}
generate_key "$root/gpg-authority" authority
generate_key "$root/gpg-wrong" wrong
authority_id=$(GNUPGHOME="$root/gpg-authority" gpg --batch --with-colons --list-keys | awk -F: '$1 == "fpr" { print $10; exit }')
wrong_id=$(GNUPGHOME="$root/gpg-wrong" gpg --batch --with-colons --list-keys | awk -F: '$1 == "fpr" { print $10; exit }')
authority_key=$(GNUPGHOME="$root/gpg-authority" gpg --batch --armor --export-secret-keys "$authority_id")
wrong_key=$(GNUPGHOME="$root/gpg-wrong" gpg --batch --armor --export-secret-keys "$wrong_id")

jq -n --arg version 2.3.4 '
  {version:$version, channel:"stable", platforms:{
    "linux-x86_64-appimage": {url:"https://github.com/mattv8/peppy/releases/download/v2.3.4/peppy-2.3.4-linux-x86_64.AppImage", signature:"signature"},
    "linux-x86_64-deb": {url:"https://github.com/mattv8/peppy/releases/download/v2.3.4/peppy-2.3.4-linux-x86_64.deb", signature:"signature"},
    "linux-x86_64-rpm": {url:"https://github.com/mattv8/peppy/releases/download/v2.3.4/peppy-2.3.4-linux-x86_64.rpm", signature:"signature"},
    "darwin-aarch64-app": {url:"https://github.com/mattv8/peppy/releases/download/v2.3.4/peppy-2.3.4-darwin-aarch64.app.tar.gz", signature:"signature"},
    "windows-x86_64-nsis": {url:"https://github.com/mattv8/peppy/releases/download/v2.3.4/peppy-2.3.4-windows-x86_64-setup.exe", signature:"signature"},
    "windows-x86_64-msi": {url:"https://github.com/mattv8/peppy/releases/download/v2.3.4/peppy-2.3.4-windows-x86_64.msi", signature:"signature"}
  }}' > "$root/assets/latest.json"
find "$root/assets" -name '*.sig' ! -name SHA256SUMS.sig -exec sh -c 'printf signature > "$1"' _ {} \;
# shellcheck disable=SC2094 # SHA256SUMS and SHA256SUMS.sig are excluded from the enumeration, so the output is never read.
(cd "$root/assets" && find . -maxdepth 1 -type f ! -name SHA256SUMS ! -name SHA256SUMS.sig -exec shasum -a 256 {} \; | sed 's#  \./#  #' | LC_ALL=C sort > SHA256SUMS)
GNUPGHOME="$root/gpg-authority" gpg --batch --yes --detach-sign --output "$root/assets/SHA256SUMS.sig" "$root/assets/SHA256SUMS"
find "$root/assets" -type f -exec basename {} \; | jq -R . | jq -s '{tagName:"v2.3.4",isDraft:false,isPrerelease:false,assets:map({name:.})}' > "$root/release.json"
# Preserve the valid signed oracle; each mutation below restores only its own input.
cp "$root/assets/SHA256SUMS" "$root/valid-sums"
cp "$root/assets/SHA256SUMS.sig" "$root/valid-signature"
cp "$root/release.json" "$root/valid-release.json"
find "$root/assets" -type f -exec basename {} \; | LC_ALL=C sort > "$root/desktop.names"
mkdir -p "$root/android-assets" "$root/sdk/build-tools/35.0.0" "$root/runner"
# Synthetic archives contain one minimal aligned ELF, not a signed/installable app.
python3 - "$root/android-assets" <<'PY'
import pathlib
import struct
import sys
import zipfile

header = bytearray(120)
header[:7] = b"\x7fELF\x02\x01\x01"
struct.pack_into("<Q", header, 32, 64)
struct.pack_into("<HHH", header, 52, 64, 56, 1)
struct.pack_into("<I", header, 64, 1)
struct.pack_into("<Q", header, 112, 16384)
for extension in ("apk", "aab"):
    with zipfile.ZipFile(pathlib.Path(sys.argv[1]) / f"peppy-2.3.4-android.{extension}", "w") as archive:
        archive.writestr("lib/arm64-v8a/libfixture.so", header)
PY
(cd "$root/android-assets" && shasum -a 256 peppy-* > SHA256SUMS)
GNUPGHOME="$root/gpg-authority" gpg --batch --yes --detach-sign --output "$root/android-assets/SHA256SUMS.sig" "$root/android-assets/SHA256SUMS"
find "$root/android-assets" -type f -exec basename {} \; | LC_ALL=C sort > "$root/android.names"
jq -Rn '[inputs] | {tagName:"android-v2.3.4",isDraft:false,isPrerelease:false,assets:map({name:.})}' < "$root/android.names" > "$root/android-release.json"
git -C "$verify_repo" tag -a android-v2.3.4 -m fixture "$verify_sha"
git -C "$verify_repo" push -q origin refs/tags/android-v2.3.4

cat > "$root/bin/platform-tool" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${PEPPY_RELEASE_GPG_KEY+x}" = x ] || [ "${PEPPY_RELEASE_GPG_KEY_ID+x}" = x ]; then
  echo environment-leak >&2
  exit 97
fi
tool=${0##*/}
printf '%s\n' "$tool" >> "$PLATFORM_LOG"
case "$tool" in
  apksigner) printf 'Signer #1 certificate SHA-256 digest: %s\n' "$PEPPY_ANDROID_SIGNING_CERT_SHA256" ;;
  zipalign) : ;;
  keytool)
    case " $* " in
      *' -importcert '*) : ;;
      *' -rfc '*) printf '%s\n' '-----BEGIN CERTIFICATE-----' fixture '-----END CERTIFICATE-----' ;;
      *) printf 'Signer #1:\n  SHA256: %s\n' "$PEPPY_ANDROID_SIGNING_CERT_SHA256" ;;
    esac
    ;;
  jarsigner) [ "${PLATFORM_FAILURE:-}" != jarsigner ] || exit 43 ;;
  *) exit 98 ;;
esac
EOF
for tool in apksigner zipalign; do cp "$root/bin/platform-tool" "$root/sdk/build-tools/35.0.0/$tool"; done
for tool in keytool jarsigner; do cp "$root/bin/platform-tool" "$root/bin/$tool"; done

# Record exact helper-owned directories, never glob/remove another process's homes.
real_mktemp=$(command -v mktemp)
real_rm=$(command -v rm)
real_gpgconf=$(command -v gpgconf)
cat > "$root/bin/mktemp" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
directory=$("$REAL_MKTEMP" "$@")
printf '%s\n' "$directory" >> "$TEMP_LOG"
printf '%s\n' "$directory"
EOF
cat > "$root/bin/gpgconf" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[ "$#" -eq 4 ] && [ "$1" = --homedir ] && [ "$3" = --kill ] && [ "$4" = gpg-agent ] || exit 99
printf '%s\n' "$2" >> "$KILL_LOG"
exec "$REAL_GPGCONF" "$@"
EOF
cat > "$root/bin/rm" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
"$REAL_RM" "$@"
if [ "${CLEANUP_FAILURE:-}" = true ] && [ "${2:-}" = -- ]; then
  printf 'controlled cleanup failure\n' >> "$CLEANUP_LOG"
  exit 73
fi
EOF
chmod +x "$root/bin/"* "$root/sdk/build-tools/35.0.0/"*

cases=0
case_component=desktop
active_assets="$root/assets"
active_metadata="$root/release.json"
selected_key=$authority_key
selected_id=$authority_id
metadata_mode=valid
platform_failure=
cleanup_failure=
case_failure() {
  printf 'FAIL %s: %s\n' "$case_id" "$1" >&2
  failures=$((failures + 1))
}
run_case() {
  local case_id=$1 expected_status=$2 stage=$3 boundary=$4 downloads=$5 status=0 directory
  cases=$((cases + 1))
  checks=$((checks + 1))
  for log in gh downloads platform temps kills cleanup; do : > "$root/$log.log"; done
  # shellcheck disable=SC2016 # Single quotes keep $1-$4 for the child bash, which receives them as arguments.
  if env PATH="$root/bin:$PATH" RUNNER_TEMP="$root/runner" \
      GH_LOG="$root/gh.log" DOWNLOAD_LOG="$root/downloads.log" PLATFORM_LOG="$root/platform.log" \
      TEMP_LOG="$root/temps.log" KILL_LOG="$root/kills.log" CLEANUP_LOG="$root/cleanup.log" \
      REAL_MKTEMP="$real_mktemp" REAL_RM="$real_rm" REAL_GPGCONF="$real_gpgconf" \
      RELEASE_JSON="$active_metadata" FIXTURE_ASSETS="$active_assets" RELEASE_MODE="$metadata_mode" \
      PLATFORM_FAILURE="$platform_failure" CLEANUP_FAILURE="$cleanup_failure" \
      ANDROID_HOME="$root/sdk" PEPPY_ANDROID_SIGNING_CERT_SHA256="$(printf '%064d' 0)" \
      PEPPY_UPDATE_PUBLIC_KEY=fixture-key PEPPY_RELEASE_GPG_KEY="$selected_key" PEPPY_RELEASE_GPG_KEY_ID="$selected_id" \
      bash -c 'cd "$1" && bash "$2" verify "$3" 2.3.4 "$4"' _ "$verify_repo" "$helper" "$case_component" "$verify_sha" \
      > "$root/case.out" 2> "$root/case.err"; then status=0; else status=$?; fi
  [ "$status" -eq "$expected_status" ] || case_failure "status $status (expected $expected_status)"
  if [ -n "$stage" ]; then
    grep -Fxq "$stage" "$root/case.err" || case_failure 'wrong rejection stage'
  fi
  if [ "$boundary" = none ]; then
    [ ! -s "$root/platform.log" ] || case_failure 'preflight reached platform tools'
    [ ! -s "$root/case.out" ] || case_failure 'preflight emitted stdout'
  elif [ "$boundary" = desktop ]; then
    printf 'cargo\n' > "$root/platform.expected"
    cmp -s "$root/platform.expected" "$root/platform.log" || case_failure 'wrong updater routing'
  else
    printf 'apksigner\nzipalign\nkeytool\nkeytool\nkeytool\njarsigner\n' > "$root/platform.expected"
    cmp -s "$root/platform.expected" "$root/platform.log" || case_failure 'wrong Android routing'
  fi
  if grep -Eq 'environment-leak|PRIVATE KEY|fixture updater private' "$root/case.out" "$root/case.err"; then
    case_failure 'unsafe diagnostics or private environment at platform boundary'
  fi
  if grep -Fq "$authority_id" "$root/case.out" "$root/case.err" || grep -Fq "$wrong_id" "$root/case.out" "$root/case.err"; then
    case_failure 'authority selector exposed in diagnostics'
  fi
  if grep -Eq '(^| )(create|edit|upload|delete)( |$)' "$root/gh.log"; then case_failure 'GH write attempted'; fi
  [ "$(wc -l < "$root/downloads.log" | tr -d ' ')" -eq "$downloads" ] || case_failure 'wrong download count'
  if [ "$downloads" -eq 16 ] || [ "$downloads" -eq 4 ]; then
    LC_ALL=C sort "$root/downloads.log" > "$root/downloads.sorted"
    cmp -s "$root/$case_component.names" "$root/downloads.sorted" || case_failure 'wrong exact download names'
  fi
  [ "$(wc -l < "$root/temps.log" | tr -d ' ')" -ge 3 ] || case_failure 'missing helper temp tracing'
  while IFS= read -r directory; do
    [ ! -e "$directory" ] || case_failure 'owned temporary directory survived'
    case "${directory##*/}" in
      p-gpg.*|p-gpgv.*) grep -Fxq "$directory" "$root/kills.log" || case_failure 'owned GPG home not stopped' ;;
    esac
  done < "$root/temps.log"
  if [ "$cleanup_failure" = true ]; then
    grep -Fxq 'controlled cleanup failure' "$root/cleanup.log" || case_failure 'cleanup failure was not injected'
  fi
  printf 'case %s: observed status=%s downloads=%s boundary=%s\n' "$case_id" "$status" "$downloads" "$boundary"
}

# Both positives traverse real GPG authority isolation, signature, coverage and hashes.
# Downstream platform-tool cryptography is mocked, not claimed as platform proof.
run_case desktop-valid 0 '' desktop 16
case_component=android active_assets="$root/android-assets" active_metadata="$root/android-release.json"
run_case android-valid-annotated 0 '' android 4
case_component=desktop active_assets="$root/assets" active_metadata="$root/release.json"
git -C "$verify_repo" tag -d v2.3.4 >/dev/null
git -C "$verify_repo" tag -a v2.3.4 -m fixture "$verify_sha"
git -C "$verify_repo" push -q --force origin refs/tags/v2.3.4 2>/dev/null
run_case desktop-valid-annotated 0 '' desktop 16
printf other >> "$verify_repo/fixture"
git -C "$verify_repo" commit -qam other
other_sha=$(git -C "$verify_repo" rev-parse HEAD)
git -C "$verify_repo" tag -f v2.3.4 "$other_sha" >/dev/null
git -C "$verify_repo" push -q --force origin refs/tags/v2.3.4 2>/dev/null
run_case wrong-remote-sha 1 'remote component tag does not point at the requested SHA' none 0
git -C "$verify_repo" tag -f v2.3.4 "$verify_sha" >/dev/null
git -C "$verify_repo" push -q --force origin refs/tags/v2.3.4 2>/dev/null

for mutation in draft prerelease wrong-tag missing-asset unexpected-asset; do
  case "$mutation" in
    draft) filter='.isDraft = true' ;;
    prerelease) filter='.isPrerelease = true' ;;
    wrong-tag) filter='.tagName = "v9.9.9"' ;;
    missing-asset) filter='.assets |= .[1:]' ;;
    unexpected-asset) filter='.assets += [{name:"unexpected"}]' ;;
  esac
  jq "$filter" "$root/valid-release.json" > "$root/release.json"
  case "$mutation" in
    *asset) stage='published release assets are incomplete or unexpected' ;;
    *) stage='release is not a published stable release' ;;
  esac
  run_case "$mutation" 1 "$stage" none 0
  cp "$root/valid-release.json" "$root/release.json"
done
metadata_mode=unknown
run_case metadata-api-error 1 'could not read published release metadata' none 0
metadata_mode=valid
mv "$root/assets/latest.json" "$root/latest.json.saved"
run_case missing-downloaded-asset 1 'could not download an expected release asset' none 13
mv "$root/latest.json.saved" "$root/assets/latest.json"

printf tampered > "$root/assets/peppy-2.3.4-linux-x86_64.AppImage"
run_case tampered-payload 1 'release checksum verification failed' none 16
printf 'peppy-2.3.4-linux-x86_64.AppImage\n' > "$root/assets/peppy-2.3.4-linux-x86_64.AppImage"
selected_key=$wrong_key selected_id=$wrong_id
run_case wrong-authority 1 'release checksums have an invalid signature' none 16
selected_key=$authority_key selected_id=$authority_id
printf malformed > "$root/assets/SHA256SUMS.sig"
run_case malformed-signature 1 'release checksums have an invalid signature' none 16
cp "$root/valid-signature" "$root/assets/SHA256SUMS.sig"

for mutation in unsafe-path duplicate missing-checksum; do
  case "$mutation" in
    unsafe-path) sed '1s#  #  ../#' "$root/valid-sums" > "$root/assets/SHA256SUMS" ;;
    duplicate) cat "$root/valid-sums" > "$root/assets/SHA256SUMS"; head -n 1 "$root/valid-sums" >> "$root/assets/SHA256SUMS" ;;
    missing-checksum) sed '1d' "$root/valid-sums" > "$root/assets/SHA256SUMS" ;;
  esac
  GNUPGHOME="$root/gpg-authority" gpg --batch --yes --detach-sign --output "$root/assets/SHA256SUMS.sig" "$root/assets/SHA256SUMS"
  run_case "$mutation" 1 'release checksums do not cover exactly the expected assets' none 16
  cp "$root/valid-sums" "$root/assets/SHA256SUMS"
  cp "$root/valid-signature" "$root/assets/SHA256SUMS.sig"
done
selected_key=
run_case missing-authority 1 'release checksum verification authority is unavailable' none 16
selected_key=$authority_key selected_id=
run_case missing-selector 1 'release checksum verification key selector is unavailable' none 16
selected_id=0000000000000000000000000000000000000000
run_case invalid-selector 1 'release checksum verification key selector did not resolve exactly one primary key' none 16
selected_id=$authority_id

platform_failure=cargo
run_case updater-failure 42 'fixture updater rejected' desktop 16
platform_failure=jarsigner case_component=android active_assets="$root/android-assets" active_metadata="$root/android-release.json"
run_case android-verifier-failure 1 'AAB strict signature verification failed' android 4
platform_failure='' case_component=desktop active_assets="$root/assets" active_metadata="$root/release.json"
cleanup_failure=true
run_case cleanup-success-status 0 '' desktop 16
platform_failure=cargo
run_case cleanup-failure-status 42 'fixture updater rejected' desktop 16
platform_failure='' cleanup_failure=''
run_case restored-valid 0 '' desktop 16

[ "$failures" -eq 0 ] || exit 1
printf 'release component state checks: %s (%s actual verify cases)\n' "$checks" "$cases"
