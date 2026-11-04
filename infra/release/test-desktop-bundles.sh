#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
fake_bin="$root/bin"
log="$root/pnpm.log"
mkdir -p "$fake_bin"
mkdir -p "$root/macos/Peppy.app/Contents"
mkdir -p "$root/dmg"
touch "$root/dmg/Peppy.dmg"
printf '%s\n' '<plist/>' > "$root/macos/Peppy.app/Contents/Info.plist"

cat > "$fake_bin/pnpm" <<'PNPM'
#!/usr/bin/env bash
set -euo pipefail
printf 'ENV|%s|%s|%s|%s|%s|%s|%s\n' "${APPLE_CERTIFICATE+x}" "${APPLE_SIGNING_IDENTITY-}" "${APPLE_ID+x}" "${TAURI_SIGNING_PRIVATE_KEY+x}" "${PEPPY_UPDATE_PUBLIC_KEY-}" "${RUNNER_OS-}" "${TRUSTED_UPDATE_BUILD-}" >> "$PNPM_LOG"
printf 'ARGV' >> "$PNPM_LOG"
printf '|%s' "$@" >> "$PNPM_LOG"
printf '\n' >> "$PNPM_LOG"
PNPM
chmod +x "$fake_bin/pnpm"

cat > "$fake_bin/codesign" <<'CODESIGN'
#!/usr/bin/env bash
printf 'codesign|%s\n' "$*" >> "$PNPM_LOG"
if [[ "$*" == *-dvvv* ]]; then
  if [[ "$*" == *'.app'* ]]; then
    authority="${CODESIGN_APP_AUTHORITY-Developer ID Application: Peppy (team)}"
    team_id="${CODESIGN_APP_TEAM_ID-team}"
    flags="${CODESIGN_APP_FLAGS-0x10000(runtime)}"
  else
    authority="${CODESIGN_DMG_AUTHORITY-Developer ID Application: Peppy (team)}"
    team_id="${CODESIGN_DMG_TEAM_ID-team}"
    flags="${CODESIGN_DMG_FLAGS-0x0(none)}"
  fi
  printf 'Authority=%s\n' "$authority" >&2
  printf 'TeamIdentifier=%s\n' "$team_id" >&2
  printf 'CodeDirectory v=20500 size=123 flags=%s hashes=4+7 location=embedded\n' "$flags" >&2
fi
if [[ "$*" == *'--entitlements :-'* ]]; then
  printf '<plist><dict><key>com.apple.security.app-sandbox</key><true/><key>com.apple.application-identifier</key><string>%s</string><key>com.apple.developer.team-identifier</key><string>%s</string></dict></plist>\n' "${CODESIGN_APPLICATION_IDENTIFIER-team.org.peppy.desktop}" "${CODESIGN_TEAM_IDENTIFIER-team}"
fi
if [[ "$*" == *'-d -r-'* ]]; then printf 'designated => certificate leaf[subject.OU] = "team"\n' >&2; fi
CODESIGN
chmod +x "$fake_bin/codesign"

cat > "$fake_bin/spctl" <<'SPCTL'
#!/usr/bin/env bash
printf 'spctl|%s\n' "$*" >> "$PNPM_LOG"
if [[ "$*" == *'--type open'* && "$*" == *.dmg ]] && [ "${SPCTL_DMG_EXIT-0}" != 0 ]; then exit "${SPCTL_DMG_EXIT}"; fi
SPCTL
chmod +x "$fake_bin/spctl"

cat > "$fake_bin/xcrun" <<'XCRUN'
#!/usr/bin/env bash
printf 'xcrun|%s\n' "$*" >> "$PNPM_LOG"
if [ "$1" = notarytool ] && [ "$2" = submit ]; then
  if [ "${NOTARY_MALFORMED-false}" = true ]; then
    printf '{bad json\n'
  else
    printf '{"id":"%s","status":"%s"}\n' "${NOTARY_ID-stub-submission-id}" "${NOTARY_STATUS-Accepted}"
  fi
  [ "${NOTARY_EXIT-0}" = 0 ] || exit "${NOTARY_EXIT}"
fi
if [ "$1" = stapler ] && [ "$2" = staple ] && [[ "$3" == *.dmg ]]; then
  [ "${STAPLER_STAPLE_EXIT-0}" = 0 ] || exit "${STAPLER_STAPLE_EXIT}"
fi
if [ "$1" = stapler ] && [ "$2" = validate ] && [[ "$3" == *.dmg ]]; then
  [ "${STAPLER_VALIDATE_EXIT-0}" = 0 ] || exit "${STAPLER_VALIDATE_EXIT}"
fi
XCRUN
chmod +x "$fake_bin/xcrun"

cat > "$fake_bin/security" <<'SECURITY'
#!/usr/bin/env bash
printf 'security|%s\n' "$*" >> "$PNPM_LOG"
if [ "$1" = cms ]; then
  printf '<plist><dict><key>Entitlements</key><dict><key>com.apple.developer.team-identifier</key><string>%s</string><key>application-identifier</key><string>%s</string></dict></dict></plist>\n' "${SECURITY_PROFILE_TEAM-team}" "${SECURITY_PROFILE_APPLICATION_IDENTIFIER-team.org.peppy.desktop}"
fi
SECURITY
chmod +x "$fake_bin/security"

cat > "$fake_bin/productbuild" <<'PRODUCTBUILD'
#!/usr/bin/env bash
printf 'productbuild|%s\n' "$*" >> "$PNPM_LOG"
touch "${!#}"
PRODUCTBUILD
chmod +x "$fake_bin/productbuild"

cat > "$fake_bin/python3" <<'PYTHON'
#!/usr/bin/env bash
if [ "$1" = -m ] && [ "$2" = venv ]; then
  mkdir -p "$3/bin"
  cat > "$3/bin/pip" <<'PIP'
#!/usr/bin/env bash
exit 0
PIP
cat > "$3/bin/python" <<'PY'
#!/usr/bin/env bash
printf 'repair|%s\n' "$*" >> "$PNPM_LOG"
[ "${PEPPY_DMG_REPAIR_TEST_FAIL:-}" != true ] || exit 1
PY
  chmod +x "$3/bin/pip" "$3/bin/python"
  exit 0
fi
if [[ "$*" == *repair_dmg.py* ]]; then
  printf 'repair|%s\n' "$*" >> "$PNPM_LOG"
  exit 0
fi
exec /usr/bin/python3 "$@"
PYTHON
chmod +x "$fake_bin/python3"

cat > "$fake_bin/plutil" <<'PLUTIL'
#!/usr/bin/env bash
if [ "$1" = -replace ]; then exit 0; fi
case "$*" in
  *com.apple.security.app-sandbox*) printf '%s\n' 1 ;;
  *CFBundleIdentifier*) printf '%s\n' "${PLUTIL_BUNDLE_IDENTIFIER-org.peppy.desktop}" ;;
  *CFBundleName*) printf '%s\n' "${PLUTIL_BUNDLE_NAME-Peppy}" ;;
  *Entitlements.com.apple.developer.team-identifier*) printf '%s\n' "${SECURITY_PROFILE_TEAM-team}" ;;
  *Entitlements.application-identifier*) printf '%s\n' "${SECURITY_PROFILE_APPLICATION_IDENTIFIER-team.org.peppy.desktop}" ;;
  *com.apple.developer.team-identifier*) printf '%s\n' "${CODESIGN_TEAM_IDENTIFIER-team}" ;;
  *com.apple.application-identifier*) printf '%s\n' "${CODESIGN_APPLICATION_IDENTIFIER-team.org.peppy.desktop}" ;;
esac
PLUTIL
chmod +x "$fake_bin/plutil"

run_bundles() {
  local macos_mode="${8-developer-id}"
  local windows_mode="${9-authenticode}"
  : > "$log"
  PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS="$1" BUNDLES="$2" VERSION="$3" \
    TRUSTED_UPDATE_BUILD="$4" PEPPY_UPDATE_CHANNEL="${5-}" \
    PEPPY_UPDATE_PUBLIC_KEY="${6-}" TAURI_SIGNING_PRIVATE_KEY="${7-}" \
    PEPPY_MACOS_SIGNING_MODE="$macos_mode" PEPPY_WINDOWS_SIGNING_MODE="$windows_mode" \
    PEPPY_DMG_OUTPUT_DIR="$root/dmg" \
    "$script_dir/build-desktop-bundles.sh"
}

assert_trusted_updater_config() {
  node -e 'const updater = JSON.parse(process.argv[1]).plugins?.updater; if (updater?.pubkey !== "public-key" || JSON.stringify(updater?.endpoints) !== JSON.stringify(["https://github.com/mattv8/peppy/releases/latest/download/latest.json"])) process.exit(1)' "$1" || {
    echo 'trusted updater config must keep the public key and stable endpoint' >&2
    exit 1
  }
}

assert_effective_macos_metadata() {
  node -e 'const fs = require("fs"); const object = value => value && typeof value === "object" && !Array.isArray(value); const merge = (target, patch) => { if (!object(patch)) return patch; const result = object(target) ? {...target} : {}; for (const [key, value] of Object.entries(patch)) { if (value === null) delete result[key]; else result[key] = object(value) ? merge(result[key], value) : value; } return result; }; const base = JSON.parse(fs.readFileSync("apps/desktop/src-tauri/tauri.conf.json", "utf8")); const macos = JSON.parse(fs.readFileSync("apps/desktop/src-tauri/tauri.macos.conf.json", "utf8")); const release = JSON.parse(process.argv[1]); const config = merge(merge(base, macos), release); if (config.bundle.category !== "SocialNetworking" || config.bundle.macOS?.minimumSystemVersion !== "11.0") process.exit(1);' "$1"
}

run_macos_developer_id() {
  local bundle_targets="${BUNDLE_TARGETS:-app,dmg}"
  env "$@" PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES="$bundle_targets" VERSION=1.2.3 TRUSTED_UPDATE_BUILD=true \
    PEPPY_UPDATE_CHANNEL=stable PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key \
    PEPPY_MACOS_SIGNING_MODE=developer-id APPLE_CERTIFICATE=certificate APPLE_CERTIFICATE_PASSWORD=password \
    APPLE_SIGNING_IDENTITY=identity APPLE_ID=id APPLE_PASSWORD=notary-password APPLE_TEAM_ID=team \
    PEPPY_BUNDLE_OUTPUT_DIR="$root/macos" PEPPY_DMG_OUTPUT_DIR="$root/dmg" "$script_dir/build-desktop-bundles.sh"
}

assert_developer_id_failure() {
  local expected_diagnostic="$1"
  shift
  local output="$root/developer-id-failure.log"
  : > "$log"
  if run_macos_developer_id "$@" >"$output" 2>&1; then
    printf 'expected Developer ID validation to fail: %s\n' "$expected_diagnostic" >&2
    exit 1
  fi
  grep -Fqx "$expected_diagnostic" "$output"
}

run_bundles Linux 'appimage,deb,rpm' 1.2.3 false
test "$(grep -c '^ARGV' "$log")" = 1
grep -Fq '|build|--bundles|appimage,deb,rpm|--config|{"version":"1.2.3"}|--|--locked' "$log"
grep -Fq 'Linux|false' "$log"
config=$(grep '^ARGV' "$log" | sed -n 's/.*|--config|\(.*\)|--|--locked/\1/p')
node -e 'const config = JSON.parse(process.argv[1]); if (config.productName || config.identifier || config.plugins?.updater?.pubkey) process.exit(1)' "$config"

run_bundles Linux 'appimage,deb,rpm' 1.2.3-dev.4 false
test "$(grep -c '^ARGV' "$log")" = 2
grep -Fq '|build|--bundles|appimage,deb|--config|{"version":"1.2.3-dev.4"}|--|--locked' "$log"
grep -Fq '|bundle|--bundles|rpm|--config|' "$log"
grep -Fq '"version":"1.2.3"' "$log"
grep -Fq '"release":"0~dev.4"' "$log"
grep -Fq '"createUpdaterArtifacts":false' "$log"
if grep -Fq -- '--locked' <(grep '|bundle|' "$log") || grep -Fq -- '--no-binary-patching' "$log"; then
  printf 'RPM packaging must use default binary patching without Cargo arguments\n' >&2
  exit 1
fi

run_bundles Linux rpm 1.2.3-staging.5 true staging public-key private-key
grep -Fq '|build|--no-bundle|--config|' "$log"
grep -Fq '"version":"1.2.3-staging.5"' "$log"
grep -Fq '"createUpdaterArtifacts":true' "$log"
grep -Fq '|bundle|--bundles|rpm|--config|' "$log"
grep -Fq '"version":"1.2.3"' "$log"
grep -Fq '"release":"0~staging.5"' "$log"
grep -Fq '"createUpdaterArtifacts":false' "$log"
staging_build_config=$(grep '^ARGV' "$log" | sed -n 's/.*|--config|\(.*\)|--|--locked/\1/p' | head -n 1)
assert_trusted_updater_config "$staging_build_config"
staging_bundle_config=$(grep '^ARGV' "$log" | grep '|bundle|' | sed -n 's/.*|--config|\(.*\)$/\1/p' | head -n 1)
assert_trusted_updater_config "$staging_bundle_config"
node -e 'const entries = require("fs").readFileSync(process.argv[1], "utf8").split("\n").filter(line => line.startsWith("ARGV")); if (!entries.every(line => line.includes("\"productName\":\"Peppy\"") && line.includes("\"identifier\":\"org.peppy.desktop\""))) process.exit(1)' "$log"

: > "$log"
APPLE_CERTIFICATE=stale TAURI_SIGNING_PRIVATE_KEY=stale PATH="$fake_bin:$PATH" PNPM_LOG="$log" \
  RUNNER_OS=macOS BUNDLES=app,dmg VERSION=1.2.3 TRUSTED_UPDATE_BUILD=false \
  PEPPY_MACOS_SIGNING_MODE='' PEPPY_WINDOWS_SIGNING_MODE='' \
  PEPPY_DMG_OUTPUT_DIR="$root/dmg" \
  "$script_dir/build-desktop-bundles.sh"
grep -Fq 'macOS|false' "$log"
grep -Fq 'repair|packaging/macos/dmg/repair_dmg.py --dmg' "$log"

before_repair_failure=$(shasum -a 256 "$root/dmg/Peppy.dmg")
if PEPPY_DMG_REPAIR_TEST_FAIL=true PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES=dmg VERSION=1.2.3 \
  TRUSTED_UPDATE_BUILD=false PEPPY_DMG_OUTPUT_DIR="$root/dmg" "$script_dir/build-desktop-bundles.sh" >/dev/null 2>&1; then
  printf 'expected failed DMG repair to fail the build\n' >&2
  exit 1
fi
test "$before_repair_failure" = "$(shasum -a 256 "$root/dmg/Peppy.dmg")"

PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=Windows BUNDLES=nsis,msi VERSION=1.2.3 TRUSTED_UPDATE_BUILD=false \
  PEPPY_MACOS_SIGNING_MODE=invalid PEPPY_WINDOWS_SIGNING_MODE=none APPLE_CERTIFICATE=stale TAURI_SIGNING_PRIVATE_KEY=stale \
  "$script_dir/build-desktop-bundles.sh"
if grep -Fq 'sign-windows-artifact.ps1' "$log"; then
  printf 'untrusted builds must ignore stale signing policy and omit Authenticode configuration\n' >&2
  exit 1
fi

: > "$log"
PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES=app,dmg VERSION=1.2.3 TRUSTED_UPDATE_BUILD=true \
  PEPPY_UPDATE_CHANNEL=stable PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key \
  PEPPY_MACOS_SIGNING_MODE=adhoc APPLE_CERTIFICATE=stale APPLE_ID=stale APPLE_PASSWORD=stale APPLE_TEAM_ID=stale \
  PEPPY_BUNDLE_OUTPUT_DIR="$root/macos" PEPPY_DMG_OUTPUT_DIR="$root/dmg" \
  "$script_dir/build-desktop-bundles.sh"
grep -Fq '|-||x|public-key|macOS|true' "$log"
grep -Fq "repair|packaging/macos/dmg/repair_dmg.py --dmg $root/dmg/Peppy.dmg --signing-mode adhoc" "$log"
grep -Fq '"createUpdaterArtifacts":true' "$log"
test "$(grep -c '^spctl|' "$log")" = 0
test "$(grep -c '^xcrun|' "$log")" = 0
macos_adhoc_config=$(grep '^ARGV' "$log" | sed -n 's/.*|--config|\(.*\)|--|--locked/\1/p' | head -n 1)
assert_trusted_updater_config "$macos_adhoc_config"
test "$(grep -c '^codesign|' "$log")" = 1

if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES=app,dmg VERSION=1.2.3 TRUSTED_UPDATE_BUILD=true \
  PEPPY_UPDATE_CHANNEL=stable PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key \
  PEPPY_MACOS_SIGNING_MODE=developer-id "$script_dir/build-desktop-bundles.sh" >/dev/null 2>&1; then
  printf 'expected strict macOS mode without credentials to fail\n' >&2
  exit 1
fi

if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=Windows BUNDLES=nsis,msi VERSION=1.2.3 TRUSTED_UPDATE_BUILD=true \
  PEPPY_UPDATE_CHANNEL=stable PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key \
  PEPPY_MACOS_SIGNING_MODE=invalid PEPPY_WINDOWS_SIGNING_MODE=none "$script_dir/build-desktop-bundles.sh" >/dev/null 2>&1; then
  printf 'expected trusted unknown mode to fail\n' >&2
  exit 1
fi

: > "$log"
run_macos_developer_id 'CODESIGN_APP_FLAGS=0x10000(library-validation,runtime)'
test "$(grep -c '^codesign|' "$log")" = 4
test "$(grep -c '^spctl|' "$log")" = 2
test "$(grep -c '^xcrun|' "$log")" = 4
grep -Fq "repair|packaging/macos/dmg/repair_dmg.py --dmg $root/dmg/Peppy.dmg --signing-mode developer-id --identity identity" "$log"
grep -Fq "codesign|-dvvv $root/macos/Peppy.app" "$log"
grep -Fq "codesign|--verify --strict --verbose=2 $root/dmg/Peppy.dmg" "$log"
grep -Fq "codesign|-dvvv $root/dmg/Peppy.dmg" "$log"
grep -Fq "xcrun|notarytool submit $root/dmg/Peppy.dmg --wait --output-format json" "$log"
grep -Fq "xcrun|stapler staple $root/dmg/Peppy.dmg" "$log"
grep -Fq "xcrun|stapler validate $root/dmg/Peppy.dmg" "$log"
grep -Fq "spctl|--assess --type open --verbose=4 $root/dmg/Peppy.dmg" "$log"
config=$(grep '^ARGV' "$log" | sed -n 's/.*|--config|\(.*\)|--|--locked/\1/p' | head -n 1)
node -e 'const config = JSON.parse(process.argv[1]); if (config.productName !== "Peppy" || config.identifier !== "org.peppy.desktop") process.exit(1)' "$config"
assert_effective_macos_metadata "$config"

assert_developer_id_failure "Developer ID signature for $root/macos/Peppy.app does not match APPLE_TEAM_ID" CODESIGN_APP_TEAM_ID=wrong-team
assert_developer_id_failure "$root/macos/Peppy.app is not signed with a Developer ID Application identity" CODESIGN_APP_AUTHORITY='Apple Development: Peppy (team)'
assert_developer_id_failure "$root/macos/Peppy.app is not signed with the hardened runtime" 'CODESIGN_APP_FLAGS=0x0(none)'
assert_developer_id_failure "Developer ID signature for $root/dmg/Peppy.dmg does not match APPLE_TEAM_ID" CODESIGN_DMG_TEAM_ID=wrong-team
assert_developer_id_failure "$root/dmg/Peppy.dmg is not signed with a Developer ID Application identity" CODESIGN_DMG_AUTHORITY='Apple Development: Peppy (team)'
assert_developer_id_failure "repaired DMG notarization status was Invalid, expected Accepted (submission stub-submission-id)" NOTARY_STATUS=Invalid
if grep -Fq "xcrun|stapler staple $root/dmg/Peppy.dmg" "$log" || grep -Fq "xcrun|stapler validate $root/dmg/Peppy.dmg" "$log"; then
  printf 'invalid notarization must not staple the DMG\n' >&2
  exit 1
fi
assert_developer_id_failure "repaired DMG notarization submission failed for $root/dmg/Peppy.dmg (submission stub-submission-id)" NOTARY_EXIT=1
assert_developer_id_failure "repaired DMG notarization returned malformed JSON for $root/dmg/Peppy.dmg" NOTARY_MALFORMED=true
assert_developer_id_failure "could not staple repaired DMG $root/dmg/Peppy.dmg" STAPLER_STAPLE_EXIT=1
assert_developer_id_failure "repaired DMG stapling validation failed for $root/dmg/Peppy.dmg" STAPLER_VALIDATE_EXIT=1
assert_developer_id_failure "Gatekeeper rejected repaired DMG $root/dmg/Peppy.dmg" SPCTL_DMG_EXIT=1

: > "$log"
BUNDLE_TARGETS=app run_macos_developer_id
test "$(grep -c '^codesign|' "$log")" = 2
test "$(grep -c '^spctl|' "$log")" = 1
test "$(grep -c '^xcrun|' "$log")" = 1
if grep -Fq "notarytool submit $root/dmg/Peppy.dmg" "$log" || grep -Fq "codesign|-dvvv $root/dmg/Peppy.dmg" "$log"; then
  printf 'app-only build must not process a stale DMG\n' >&2
  exit 1
fi

: > "$log"
if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES=app,dmg VERSION=1.2.3 TRUSTED_UPDATE_BUILD=true \
  PEPPY_UPDATE_CHANNEL=stable PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key \
  PEPPY_MACOS_SIGNING_MODE=adhoc PEPPY_BUNDLE_OUTPUT_DIR="$root/macos" \
  PEPPY_DMG_OUTPUT_DIR="$root/dmg" \
  PLUTIL_BUNDLE_IDENTIFIER=org.peppy.desktop.dev "$script_dir/build-desktop-bundles.sh" >/dev/null 2>&1; then
  printf 'expected trusted macOS bundle with development identity to fail\n' >&2
  exit 1
fi
grep -Fq 'codesign|' "$log"
test "$(grep -c '^spctl|' "$log")" = 0

: > "$log"
if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=Linux BUNDLES=rpm VERSION=1.2.3-staging.5 \
  TRUSTED_UPDATE_BUILD=true PEPPY_UPDATE_CHANNEL=staging PEPPY_UPDATE_PUBLIC_KEY=public-key \
  "$script_dir/build-desktop-bundles.sh" >"$root/missing-credentials.log" 2>&1; then
  printf 'expected trusted build without credentials to fail\n' >&2
  exit 1
fi
test ! -s "$log"
grep -Fq 'TAURI_SIGNING_PRIVATE_KEY is required for trusted update builds' "$root/missing-credentials.log"

if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=Linux BUNDLES=rpm VERSION=1.2.3-preview.1 \
  TRUSTED_UPDATE_BUILD=false "$script_dir/build-desktop-bundles.sh" >/dev/null 2>&1; then
  printf 'expected unsupported Linux RPM prerelease to fail\n' >&2
  exit 1
fi

GITHUB_WORKSPACE='/fixture path' WIX_VERSION=1.2.3.4 run_bundles Windows 'nsis,msi' 1.2.3 true stable public-key private-key
grep -Fq '"wix":{"version":"1.2.3.4"}' "$log"
grep -Fq 'sign-windows-artifact.ps1' "$log"
grep -Fq '/fixture path' "$log"
config=$(grep '^ARGV' "$log" | sed -n 's/.*|--config|\(.*\)|--|--locked/\1/p' | head -n 1)
node -e 'const config = JSON.parse(process.argv[1]); if (!config.bundle.windows.signCommand) process.exit(1)' "$config"
assert_trusted_updater_config "$config"

GITHUB_WORKSPACE='/fixture path' run_bundles Windows 'nsis,msi' 1.2.3 true stable public-key private-key developer-id none
config=$(grep '^ARGV' "$log" | sed -n 's/.*|--config|\(.*\)|--|--locked/\1/p' | head -n 1)
node -e 'const config = JSON.parse(process.argv[1]); if (config.bundle.createUpdaterArtifacts !== true || config.bundle.windows?.signCommand) process.exit(1)' "$config"
assert_trusted_updater_config "$config"

: > "$log"
env -u PEPPY_MACOS_SIGNING_MODE -u PEPPY_WINDOWS_SIGNING_MODE PATH="$fake_bin:$PATH" PNPM_LOG="$log" \
  RUNNER_OS=Windows BUNDLES=nsis,msi VERSION=1.2.3 TRUSTED_UPDATE_BUILD=true PEPPY_UPDATE_CHANNEL=stable \
  PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key GITHUB_WORKSPACE='/fixture path' \
  "$script_dir/build-desktop-bundles.sh"
config=$(grep '^ARGV' "$log" | sed -n 's/.*|--config|\(.*\)|--|--locked/\1/p' | head -n 1)
node -e 'const config = JSON.parse(process.argv[1]); if (!config.bundle.windows?.signCommand) process.exit(1)' "$config"

: > "$log"
if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=Linux BUNDLES=appimage,deb,rpm VERSION=1.2.3 TRUSTED_UPDATE_BUILD=true \
  PEPPY_UPDATE_CHANNEL=stable PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key \
  TAURI_SIGNING_PRIVATE_KEY='   ' \
  "$script_dir/build-desktop-bundles.sh" >/dev/null 2>&1; then
  printf 'expected whitespace updater key to fail before build\n' >&2
  exit 1
fi
test ! -s "$log"

: > "$log"
PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES=app VERSION=1.2.3 GITHUB_ACTIONS=true MACOS_BUILD_NUMBER=42 \
  TRUSTED_UPDATE_BUILD=false PEPPY_MACOS_DISTRIBUTION=app-store \
  APPLE_MAS_CERTIFICATE_P12=YXBwbGljYXRpb24tY2VydGlmaWNhdGU= APPLE_MAS_CERTIFICATE_PASSWORD=password \
  APPLE_MAS_INSTALLER_CERTIFICATE=aW5zdGFsbGVyLWNlcnRpZmljYXRl APPLE_MAS_PROVISIONING_PROFILE_BASE64=cHJvZmlsZQ== \
  APPLE_MAS_SIGNING_IDENTITY='Apple Distribution: Peppy (team)' APPLE_MAS_INSTALLER_IDENTITY='3rd Party Mac Developer Installer: Peppy (team)' \
  PEPPY_BUNDLE_OUTPUT_DIR="$root/macos" PEPPY_PKG_OUTPUT_DIR="$root/pkg" \
  "$script_dir/build-desktop-bundles.sh"
app_store_config=$(grep '^ARGV' "$log" | sed -n 's/.*|--config|\(.*\)|--|--locked/\1/p')
node -e 'const config = JSON.parse(process.argv[1]); if (config.productName !== "Peppy" || config.identifier !== "org.peppy.desktop" || config.bundle?.createUpdaterArtifacts !== false || config.bundle?.macOS?.minimumSystemVersion !== "13.0" || config.bundle?.macOS?.entitlements !== "../../../packaging/macos/app-store/entitlements.plist" || JSON.stringify(config.plugins?.updater?.endpoints) !== "[]") process.exit(1)' "$app_store_config"
grep -Fq 'productbuild|--component' "$log"
grep -Fq 'security|cms -D' "$log"
grep -Fq 'security|create-keychain' "$log"
grep -Fq 'security|import' "$log"
grep -Fq 'security|list-keychains -d user -s' "$log"
grep -Fq 'security|delete-keychain' "$log"
grep -Fq 'codesign|--force --sign Apple Distribution: Peppy (team) --keychain' "$log"
grep -Fq 'productbuild|--component' "$log"
test -f "$root/pkg/Peppy.pkg"
if grep -Fq 'repair|' "$log" || grep -Fq '^xcrun|' "$log" || grep -Fq 'notarytool' "$log"; then
  printf 'App Store packaging must not repair DMGs or notarize artifacts\n' >&2
  exit 1
fi

if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES=app VERSION=1.2.3 GITHUB_ACTIONS=true MACOS_BUILD_NUMBER=42 \
  TRUSTED_UPDATE_BUILD=false PEPPY_MACOS_DISTRIBUTION=app-store \
  APPLE_MAS_CERTIFICATE_P12=YXBwbGljYXRpb24tY2VydGlmaWNhdGU= APPLE_MAS_CERTIFICATE_PASSWORD=password \
  APPLE_MAS_INSTALLER_CERTIFICATE=aW5zdGFsbGVyLWNlcnRpZmljYXRl APPLE_MAS_PROVISIONING_PROFILE_BASE64=cHJvZmlsZQ== \
  APPLE_MAS_SIGNING_IDENTITY='Apple Distribution: Peppy (team)' APPLE_MAS_INSTALLER_IDENTITY='3rd Party Mac Developer Installer: Peppy (team)' \
  SECURITY_PROFILE_APPLICATION_IDENTIFIER=team.org.peppy.other \
  PEPPY_BUNDLE_OUTPUT_DIR="$root/macos" PEPPY_PKG_OUTPUT_DIR="$root/pkg" \
  "$script_dir/build-desktop-bundles.sh" >"$root/app-store-profile-mismatch.log" 2>&1; then
  printf 'App Store packaging must reject a provisioning profile for another application identifier\n' >&2
  exit 1
fi
grep -Fq 'App Store provisioning profile application identifier must equal team.org.peppy.desktop' "$root/app-store-profile-mismatch.log"

if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES=app VERSION=1.2.3 GITHUB_ACTIONS=true MACOS_BUILD_NUMBER=42 \
  TRUSTED_UPDATE_BUILD=false PEPPY_MACOS_DISTRIBUTION=app-store \
  APPLE_MAS_CERTIFICATE_P12=YXBwbGljYXRpb24tY2VydGlmaWNhdGU= APPLE_MAS_CERTIFICATE_PASSWORD=password \
  APPLE_MAS_INSTALLER_CERTIFICATE=aW5zdGFsbGVyLWNlcnRpZmljYXRl APPLE_MAS_PROVISIONING_PROFILE_BASE64=cHJvZmlsZQ== \
  APPLE_MAS_SIGNING_IDENTITY='Apple Distribution: Peppy (team)' APPLE_MAS_INSTALLER_IDENTITY='3rd Party Mac Developer Installer: Peppy (team)' \
  CODESIGN_TEAM_IDENTIFIER=other-team \
  PEPPY_BUNDLE_OUTPUT_DIR="$root/macos" PEPPY_PKG_OUTPUT_DIR="$root/pkg" \
  "$script_dir/build-desktop-bundles.sh" >"$root/app-store-signed-team-mismatch.log" 2>&1; then
  printf 'App Store packaging must reject a signed bundle with another team identifier\n' >&2
  exit 1
fi
grep -Fq 'App Store bundle team identifier does not match the provisioning profile' "$root/app-store-signed-team-mismatch.log"

if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES=app VERSION=1.2.3 GITHUB_ACTIONS=true MACOS_BUILD_NUMBER=42 \
  TRUSTED_UPDATE_BUILD=false PEPPY_MACOS_DISTRIBUTION=app-store \
  APPLE_MAS_CERTIFICATE_P12=YXBwbGljYXRpb24tY2VydGlmaWNhdGU= APPLE_MAS_CERTIFICATE_PASSWORD=password \
  APPLE_MAS_INSTALLER_CERTIFICATE=aW5zdGFsbGVyLWNlcnRpZmljYXRl APPLE_MAS_PROVISIONING_PROFILE_BASE64=cHJvZmlsZQ== \
  APPLE_MAS_SIGNING_IDENTITY='Apple Distribution: Peppy (team)' APPLE_MAS_INSTALLER_IDENTITY='3rd Party Mac Developer Installer: Peppy (team)' \
  CODESIGN_APPLICATION_IDENTIFIER=team.org.peppy.other \
  PEPPY_BUNDLE_OUTPUT_DIR="$root/macos" PEPPY_PKG_OUTPUT_DIR="$root/pkg" \
  "$script_dir/build-desktop-bundles.sh" >"$root/app-store-signed-app-mismatch.log" 2>&1; then
  printf 'App Store packaging must reject a signed bundle with another application identifier\n' >&2
  exit 1
fi
grep -Fq 'App Store bundle application identifier does not match the provisioning profile' "$root/app-store-signed-app-mismatch.log"

if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES=app,dmg VERSION=1.2.3 GITHUB_ACTIONS=true MACOS_BUILD_NUMBER=42 \
  TRUSTED_UPDATE_BUILD=false PEPPY_MACOS_DISTRIBUTION=app-store "$script_dir/build-desktop-bundles.sh" >/dev/null 2>&1; then
  printf 'App Store packaging must reject DMG targets\n' >&2
  exit 1
fi

app_store_env=(
  APPLE_MAS_CERTIFICATE_P12=YXBwbGljYXRpb24tY2VydGlmaWNhdGU= APPLE_MAS_CERTIFICATE_PASSWORD=password
  APPLE_MAS_INSTALLER_CERTIFICATE=aW5zdGFsbGVyLWNlcnRpZmljYXRl APPLE_MAS_PROVISIONING_PROFILE_BASE64=cHJvZmlsZQ==
  APPLE_MAS_SIGNING_IDENTITY='Apple Distribution: Peppy (team)'
  APPLE_MAS_INSTALLER_IDENTITY='3rd Party Mac Developer Installer: Peppy (team)'
)
assert_app_store_failure() {
  local expected_diagnostic="$1"
  shift
  local output="$root/app-store-failure.log"
  : > "$log"
  if env GITHUB_ACTIONS=true MACOS_BUILD_NUMBER=42 "${app_store_env[@]}" "$@" \
    PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES=app VERSION=1.2.3 TRUSTED_UPDATE_BUILD=false \
    PEPPY_MACOS_DISTRIBUTION=app-store PEPPY_BUNDLE_OUTPUT_DIR="$root/macos" PEPPY_PKG_OUTPUT_DIR="$root/pkg" \
    "$script_dir/build-desktop-bundles.sh" >"$output" 2>&1; then
    printf 'expected App Store validation to fail: %s\n' "$expected_diagnostic" >&2
    exit 1
  fi
  grep -Fqx "$expected_diagnostic" "$output"
}

assert_app_store_failure 'app-store distribution must run in GitHub Actions' GITHUB_ACTIONS=
assert_app_store_failure 'app-store distribution must run in GitHub Actions' GITHUB_ACTIONS=false
assert_app_store_failure 'MACOS_BUILD_NUMBER is required for app-store builds' MACOS_BUILD_NUMBER=
assert_app_store_failure 'MACOS_BUILD_NUMBER must be a positive integer' MACOS_BUILD_NUMBER=0
assert_app_store_failure 'MACOS_BUILD_NUMBER must be a positive integer' MACOS_BUILD_NUMBER=1.2
printf 'desktop bundle orchestration tests passed\n'
