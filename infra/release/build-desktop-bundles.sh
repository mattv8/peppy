#!/usr/bin/env bash
set -euo pipefail

config=''
if [ "$TRUSTED_UPDATE_BUILD" = true ]; then
  case "$PEPPY_UPDATE_CHANNEL" in stable|staging) ;; *) echo 'trusted update builds require stable or staging channel' >&2; exit 1 ;; esac
  for required in VERSION PEPPY_UPDATE_PUBLIC_KEY TAURI_SIGNING_PRIVATE_KEY; do
    [ -n "${!required:-}" ] || { echo "$required is required for trusted update builds" >&2; exit 1; }
  done
  if [ "$RUNNER_OS" = macOS ]; then
    for required in APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do
      [ -n "${!required:-}" ] || { echo "$required is required for trusted macOS builds" >&2; exit 1; }
    done
  fi
  config=$(node -e 'process.stdout.write(JSON.stringify({bundle:{createUpdaterArtifacts:true},plugins:{updater:{pubkey:process.env.PEPPY_UPDATE_PUBLIC_KEY}}}))')
else
  # Tauri interprets even empty Apple credentials as a request to sign.
  unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY
  unset APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID
  unset TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD
fi

if [[ -n "$VERSION" ]]; then
  if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
    echo 'version must match X.Y.Z or X.Y.Z-prerelease' >&2
    exit 1
  fi
  if [[ -n "${WIX_VERSION:-}" && ! "$WIX_VERSION" =~ ^[0-9]+(\.[0-9]+){3}$ ]]; then
    echo 'wix_version must match X.Y.Z.W' >&2
    exit 1
  fi
  json=$(printf '{"version":"%s"' "$VERSION")
  if [[ -n "${WIX_VERSION:-}" ]]; then
    json="$json$(printf ',"bundle":{"windows":{"wix":{"version":"%s"}}}' "$WIX_VERSION")"
  fi
  json="$json}"
  # shellcheck disable=SC2016
  if [ -n "$config" ]; then json=$(node -e 'const [base, overlay] = process.argv.slice(1).map(JSON.parse); const bundle={...base.bundle,...overlay.bundle}; if (process.env.RUNNER_OS === "Windows") { const script = `${process.env.GITHUB_WORKSPACE}\\infra\\release\\sign-windows-artifact.ps1`; bundle.windows={...(base.bundle?.windows ?? {}),signCommand:`powershell -NoProfile -ExecutionPolicy Bypass -File \"${script}\" %1`}; } console.log(JSON.stringify({...base,...overlay,bundle}));' "$json" "$config"); fi
else
  json=''
fi

is_linux_prerelease_rpm=false
if [ "$RUNNER_OS" = Linux ] && [[ "$VERSION" == *-* ]] && [[ ",$BUNDLES," == *,rpm,* ]]; then
  prerelease=${VERSION#*-}
  if [[ ! "$prerelease" =~ ^(dev|staging)\.[0-9]+$ ]]; then
    echo 'Linux RPM prereleases support only dev.N or staging.N' >&2
    exit 1
  fi
  is_linux_prerelease_rpm=true
fi

if [ "$is_linux_prerelease_rpm" = true ]; then
  build_bundles=$(printf '%s' "$BUNDLES" | tr ',' '\n' | awk '$0 != "rpm"' | paste -sd, -)
  if [ -n "$build_bundles" ]; then
    pnpm --dir apps/desktop exec tauri build --bundles "$build_bundles" --config "$json" -- --locked
  else
    pnpm --dir apps/desktop exec tauri build --no-bundle --config "$json" -- --locked
  fi
  core_version=${VERSION%%-*}
  rpm_release="0~$prerelease"
  rpm_config=$(node -e 'const base=JSON.parse(process.argv[1]); base.version=process.argv[2]; base.bundle={...(base.bundle ?? {}),createUpdaterArtifacts:false,linux:{...(base.bundle?.linux ?? {}),rpm:{...(base.bundle?.linux?.rpm ?? {}),release:process.argv[3]}}}; console.log(JSON.stringify(base));' "$json" "$core_version" "$rpm_release")
  pnpm --dir apps/desktop exec tauri bundle --bundles rpm --config "$rpm_config"
else
  if [ -n "$json" ]; then
    pnpm --dir apps/desktop exec tauri build --bundles "$BUNDLES" --config "$json" -- --locked
  else
    pnpm --dir apps/desktop exec tauri build --bundles "$BUNDLES" -- --locked
  fi
fi

if [ "$TRUSTED_UPDATE_BUILD" = true ] && [ "$RUNNER_OS" = macOS ]; then
  while IFS= read -r -d '' application; do
    codesign --verify --deep --strict --verbose=2 "$application"
    spctl --assess --type execute --verbose=4 "$application"
    xcrun stapler validate "$application"
  done < <(find apps/desktop/src-tauri/target/release/bundle/macos -name '*.app' -type d -print0)
fi
