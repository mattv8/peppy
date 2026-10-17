#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
fake_bin="$root/bin"
log="$root/pnpm.log"
mkdir -p "$fake_bin"

cat > "$fake_bin/pnpm" <<'PNPM'
#!/usr/bin/env bash
set -euo pipefail
printf 'ENV|%s|%s|%s|%s|%s|%s\n' "${APPLE_CERTIFICATE+x}" "${TAURI_SIGNING_PRIVATE_KEY+x}" "${PEPPY_UPDATE_PUBLIC_KEY-}" "${RUNNER_OS-}" "${TRUSTED_UPDATE_BUILD-}" "${BUNDLES-}" >> "$PNPM_LOG"
printf 'ARGV' >> "$PNPM_LOG"
printf '|%s' "$@" >> "$PNPM_LOG"
printf '\n' >> "$PNPM_LOG"
PNPM
chmod +x "$fake_bin/pnpm"

run_bundles() {
  : > "$log"
  PATH="$fake_bin:$PATH" PNPM_LOG="$log" RUNNER_OS="$1" BUNDLES="$2" VERSION="$3" \
    TRUSTED_UPDATE_BUILD="$4" PEPPY_UPDATE_CHANNEL="${5-}" \
    PEPPY_UPDATE_PUBLIC_KEY="${6-}" TAURI_SIGNING_PRIVATE_KEY="${7-}" \
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
  "$script_dir/build-desktop-bundles.sh"
grep -Fq 'ENV||||macOS|false|app,dmg' "$log"

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
printf 'desktop bundle orchestration tests passed\n'
