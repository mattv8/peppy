#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
fake_bin="$root/bin"
log="$root/pnpm.log"
mkdir -p "$fake_bin"
mkdir -p "$root/macos/Peppy.app"

cat > "$fake_bin/pnpm" <<'PNPM'
#!/usr/bin/env bash
set -euo pipefail
printf 'ENV|%s|%s|%s|%s|%s|%s|%s|%s\n' "${APPLE_CERTIFICATE+x}" "${APPLE_SIGNING_IDENTITY-}" "${APPLE_ID+x}" "${TAURI_SIGNING_PRIVATE_KEY+x}" "${PEPPY_UPDATE_PUBLIC_KEY-}" "${RUNNER_OS-}" "${TRUSTED_UPDATE_BUILD-}" "${BUNDLES-}" >> "$PNPM_LOG"
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

run_bundles() {
  local macos_mode="${8-developer-id}"
  local windows_mode="${9-authenticode}"
  : > "$log"
  PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS="$1" BUNDLES="$2" VERSION="$3" \
    TRUSTED_UPDATE_BUILD="$4" PEPPY_UPDATE_CHANNEL="${5-}" \
    PEPPY_UPDATE_PUBLIC_KEY="${6-}" TAURI_SIGNING_PRIVATE_KEY="${7-}" \
    PEPPY_MACOS_SIGNING_MODE="$macos_mode" PEPPY_WINDOWS_SIGNING_MODE="$windows_mode" \
    "$script_dir/build-desktop-bundles.sh"
}

run_bundles Linux 'appimage,deb,rpm' 1.2.3 false
test "$(grep -c '^ARGV' "$log")" = 1
grep -Fq '|build|--bundles|appimage,deb,rpm|--config|{"version":"1.2.3"}|--|--locked' "$log"

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

APPLE_CERTIFICATE=stale TAURI_SIGNING_PRIVATE_KEY=stale PATH="$fake_bin:$PATH" PNPM_LOG="$log" \
  RUNNER_OS=macOS BUNDLES=app,dmg VERSION=1.2.3 TRUSTED_UPDATE_BUILD=false \
  PEPPY_MACOS_SIGNING_MODE='' PEPPY_WINDOWS_SIGNING_MODE='' \
  "$script_dir/build-desktop-bundles.sh"
grep -Fq 'ENV||||||macOS|false|app,dmg' "$log"

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
  PEPPY_BUNDLE_OUTPUT_DIR="$root/macos" \
  "$script_dir/build-desktop-bundles.sh"
grep -Fq 'ENV||-||x|public-key|macOS|true|app,dmg' "$log"
grep -Fq '"createUpdaterArtifacts":true' "$log"
test "$(grep -c '^spctl|' "$log")" = 0
test "$(grep -c '^xcrun|' "$log")" = 0
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
PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=macOS BUNDLES=app,dmg VERSION=1.2.3 TRUSTED_UPDATE_BUILD=true \
  PEPPY_UPDATE_CHANNEL=stable PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key \
  PEPPY_MACOS_SIGNING_MODE=developer-id APPLE_CERTIFICATE=certificate APPLE_CERTIFICATE_PASSWORD=password \
  APPLE_SIGNING_IDENTITY=identity APPLE_ID=id APPLE_PASSWORD=notary-password APPLE_TEAM_ID=team \
  PEPPY_BUNDLE_OUTPUT_DIR="$root/macos" "$script_dir/build-desktop-bundles.sh"
test "$(grep -c '^codesign|' "$log")" = 1
test "$(grep -c '^spctl|' "$log")" = 1
test "$(grep -c '^xcrun|' "$log")" = 1

: > "$log"
if PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS=Linux BUNDLES=rpm VERSION=1.2.3-staging.5 \
  TRUSTED_UPDATE_BUILD=true PEPPY_UPDATE_CHANNEL=staging "$script_dir/build-desktop-bundles.sh" >"$root/missing-credentials.log" 2>&1; then
  printf 'expected trusted build without credentials to fail\n' >&2
  exit 1
fi
test ! -s "$log"
grep -Fq 'is required for trusted update builds' "$root/missing-credentials.log"

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

GITHUB_WORKSPACE='/fixture path' run_bundles Windows 'nsis,msi' 1.2.3 true stable public-key private-key developer-id none
config=$(grep '^ARGV' "$log" | sed -n 's/.*|--config|\(.*\)|--|--locked/\1/p' | head -n 1)
node -e 'const config = JSON.parse(process.argv[1]); if (config.bundle.createUpdaterArtifacts !== true || config.bundle.windows?.signCommand) process.exit(1)' "$config"

: > "$log"
env -u PEPPY_MACOS_SIGNING_MODE -u PEPPY_WINDOWS_SIGNING_MODE PATH="$fake_bin:$PATH" PNPM_LOG="$log" \
  RUNNER_OS=Windows BUNDLES=nsis,msi VERSION=1.2.3 TRUSTED_UPDATE_BUILD=true PEPPY_UPDATE_CHANNEL=stable \
  PEPPY_UPDATE_PUBLIC_KEY=public-key TAURI_SIGNING_PRIVATE_KEY=private-key GITHUB_WORKSPACE='/fixture path' \
  "$script_dir/build-desktop-bundles.sh"
config=$(grep '^ARGV' "$log" | sed -n 's/.*|--config|\(.*\)|--|--locked/\1/p' | head -n 1)
node -e 'const config = JSON.parse(process.argv[1]); if (!config.bundle.windows?.signCommand) process.exit(1)' "$config"
printf 'desktop bundle orchestration tests passed\n'
