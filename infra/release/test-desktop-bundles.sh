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
printf 'ENV|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s\n' "${APPLE_CERTIFICATE+x}" "${APPLE_SIGNING_IDENTITY-}" "${APPLE_ID+x}" "${TAURI_SIGNING_PRIVATE_KEY+x}" "${PEPPY_UPDATE_PUBLIC_KEY-}" "${PEPPY_DESKTOP_GOOGLE_CLIENT_ID-}" "${PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET-}" "${RUNNER_OS-}" "${TRUSTED_UPDATE_BUILD-}" "${BUNDLES-}" >> "$PNPM_LOG"
printf 'ARGV' >> "$PNPM_LOG"
printf '|%s' "$@" >> "$PNPM_LOG"
printf '\n' >> "$PNPM_LOG"
PNPM
chmod +x "$fake_bin/pnpm"

for tool in codesign spctl xcrun; do
  cat > "$fake_bin/$tool" <<TOOL
#!/usr/bin/env bash
printf '$tool|%s\n' "\$*" >> "\$PNPM_LOG"
TOOL
  chmod +x "$fake_bin/$tool"
done

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
case "$*" in
  *CFBundleIdentifier*) printf '%s\n' "${PLUTIL_BUNDLE_IDENTIFIER-org.peppy.desktop}" ;;
  *CFBundleName*) printf '%s\n' "${PLUTIL_BUNDLE_NAME-Peppy}" ;;
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
  PEPPY_DESKTOP_GOOGLE_CLIENT_ID=desktop-client-id PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET=installed-client-secret \
    "$script_dir/build-desktop-bundles.sh"
}

assert_trusted_updater_config() {
  node -e 'const updater = JSON.parse(process.argv[1]).plugins?.updater; if (updater?.pubkey !== "public-key" || JSON.stringify(updater?.endpoints) !== JSON.stringify(["https://github.com/mattv8/peppy/releases/latest/download/latest.json"])) process.exit(1)' "$1" || {
    echo 'trusted updater config must keep the public key and stable endpoint' >&2
    exit 1
  }
}

run_bundles Linux 'appimage,deb,rpm' 1.2.3 false
test "$(grep -c '^ARGV' "$log")" = 1
grep -Fq '|build|--bundles|appimage,deb,rpm|--config|{"version":"1.2.3"}|--|--locked' "$log"
grep -Fq 'ENV||||||||Linux|false|appimage,deb,rpm' "$log"
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
grep -Fq 'ENV||||||||macOS|false|app,dmg' "$log"
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
  PEPPY_DESKTOP_GOOGLE_CLIENT_ID=desktop-client-id PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET=installed-client-secret \
  PEPPY_MACOS_SIGNING_MODE=adhoc APPLE_CERTIFICATE=stale APPLE_ID=stale APPLE_PASSWORD=stale APPLE_TEAM_ID=stale \
  PEPPY_BUNDLE_OUTPUT_DIR="$root/macos" PEPPY_DMG_OUTPUT_DIR="$root/dmg" \
  "$script_dir/build-desktop-bundles.sh"
grep -Fq 'ENV||-||x|public-key|desktop-client-id|installed-client-secret|macOS|true|app,dmg' "$log"
grep -Fq "repair|packaging/macos/dmg/repair_dmg.py --dmg $root/dmg/Peppy.dmg --signing-mode adhoc" "$log"
grep -Fq '"createUpdaterArtifacts":true' "$log"
test "$(grep -c '^spctl|' "$log")" = 0
test "$(grep -c '^xcrun|' "$log")" = 0
macos_adhoc_config=$(grep '^ARGV' "$log" | sed -n 's/.*|--config|\(.*\)|--|--locked/\1/p' | head -n 1)
assert_trusted_updater_config "$macos_adhoc_config"
test "$(grep -c '^codesign|' "$log")" = 1

if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES=app,dmg VERSION=1.2.3 TRUSTED_UPDATE_BUILD=true \
  PEPPY_UPDATE_CHANNEL=stable PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key \
  PEPPY_DESKTOP_GOOGLE_CLIENT_ID=desktop-client-id PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET=installed-client-secret \
  PEPPY_MACOS_SIGNING_MODE=developer-id "$script_dir/build-desktop-bundles.sh" >/dev/null 2>&1; then
  printf 'expected strict macOS mode without credentials to fail\n' >&2
  exit 1
fi

if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=Windows BUNDLES=nsis,msi VERSION=1.2.3 TRUSTED_UPDATE_BUILD=true \
  PEPPY_UPDATE_CHANNEL=stable PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key \
  PEPPY_DESKTOP_GOOGLE_CLIENT_ID=desktop-client-id PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET=installed-client-secret \
  PEPPY_MACOS_SIGNING_MODE=invalid PEPPY_WINDOWS_SIGNING_MODE=none "$script_dir/build-desktop-bundles.sh" >/dev/null 2>&1; then
  printf 'expected trusted unknown mode to fail\n' >&2
  exit 1
fi

: > "$log"
PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES=app,dmg VERSION=1.2.3 TRUSTED_UPDATE_BUILD=true \
  PEPPY_UPDATE_CHANNEL=stable PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key \
  PEPPY_DESKTOP_GOOGLE_CLIENT_ID=desktop-client-id PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET=installed-client-secret \
  PEPPY_MACOS_SIGNING_MODE=developer-id APPLE_CERTIFICATE=certificate APPLE_CERTIFICATE_PASSWORD=password \
  APPLE_SIGNING_IDENTITY=identity APPLE_ID=id APPLE_PASSWORD=notary-password APPLE_TEAM_ID=team \
  PEPPY_BUNDLE_OUTPUT_DIR="$root/macos" PEPPY_DMG_OUTPUT_DIR="$root/dmg" "$script_dir/build-desktop-bundles.sh"
test "$(grep -c '^codesign|' "$log")" = 1
test "$(grep -c '^spctl|' "$log")" = 1
test "$(grep -c '^xcrun|' "$log")" = 1
grep -Fq "repair|packaging/macos/dmg/repair_dmg.py --dmg $root/dmg/Peppy.dmg --signing-mode developer-id --identity identity" "$log"
config=$(grep '^ARGV' "$log" | sed -n 's/.*|--config|\(.*\)|--|--locked/\1/p' | head -n 1)
node -e 'const config = JSON.parse(process.argv[1]); if (config.productName !== "Peppy" || config.identifier !== "org.peppy.desktop") process.exit(1)' "$config"

: > "$log"
if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES=app,dmg VERSION=1.2.3 TRUSTED_UPDATE_BUILD=true \
  PEPPY_UPDATE_CHANNEL=stable PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key \
  PEPPY_DESKTOP_GOOGLE_CLIENT_ID=desktop-client-id PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET=installed-client-secret \
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
  TRUSTED_UPDATE_BUILD=true PEPPY_UPDATE_CHANNEL=staging PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key \
  PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET=installed-client-secret "$script_dir/build-desktop-bundles.sh" >"$root/missing-credentials.log" 2>&1; then
  printf 'expected trusted build without credentials to fail\n' >&2
  exit 1
fi
test ! -s "$log"
grep -Fq 'PEPPY_DESKTOP_GOOGLE_CLIENT_ID is required for trusted update builds' "$root/missing-credentials.log"

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
  PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key PEPPY_DESKTOP_GOOGLE_CLIENT_ID=desktop-client-id PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET=installed-client-secret GITHUB_WORKSPACE='/fixture path' \
  "$script_dir/build-desktop-bundles.sh"
config=$(grep '^ARGV' "$log" | sed -n 's/.*|--config|\(.*\)|--|--locked/\1/p' | head -n 1)
node -e 'const config = JSON.parse(process.argv[1]); if (!config.bundle.windows?.signCommand) process.exit(1)' "$config"

: > "$log"
if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=Linux BUNDLES=appimage,deb,rpm VERSION=1.2.3 TRUSTED_UPDATE_BUILD=true \
  PEPPY_UPDATE_CHANNEL=stable PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key \
  PEPPY_DESKTOP_GOOGLE_CLIENT_ID='   ' PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET=installed-client-secret \
  "$script_dir/build-desktop-bundles.sh" >/dev/null 2>&1; then
  printf 'expected whitespace desktop client ID to fail before build\n' >&2
  exit 1
fi
test ! -s "$log"
printf 'desktop bundle orchestration tests passed\n'
