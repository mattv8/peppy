#!/usr/bin/env bash
set -euo pipefail

config=''
if [ "$TRUSTED_UPDATE_BUILD" = true ]; then
  macos_mode="${PEPPY_MACOS_SIGNING_MODE-developer-id}"
  windows_mode="${PEPPY_WINDOWS_SIGNING_MODE-authenticode}"
  case "$macos_mode" in developer-id|adhoc) ;; *) echo 'PEPPY_MACOS_SIGNING_MODE must be developer-id or adhoc' >&2; exit 1 ;; esac
  case "$windows_mode" in authenticode|none) ;; *) echo 'PEPPY_WINDOWS_SIGNING_MODE must be authenticode or none' >&2; exit 1 ;; esac
  case "$PEPPY_UPDATE_CHANNEL" in stable|staging) ;; *) echo 'trusted update builds require stable or staging channel' >&2; exit 1 ;; esac
  for required in VERSION PEPPY_UPDATE_PUBLIC_KEY TAURI_SIGNING_PRIVATE_KEY PEPPY_DESKTOP_GOOGLE_CLIENT_ID PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET; do
    [[ "${!required:-}" =~ [^[:space:]] ]] || { echo "$required is required for trusted update builds" >&2; exit 1; }
  done
  if [ "$RUNNER_OS" = macOS ] && [ "$macos_mode" = developer-id ]; then
    for required in APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do
      [ -n "${!required:-}" ] || { echo "$required is required for trusted macOS builds" >&2; exit 1; }
    done
  fi
  if [ "$RUNNER_OS" = macOS ] && [ "$macos_mode" = adhoc ]; then
    unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID
    export APPLE_SIGNING_IDENTITY='-'
  fi
  # Tauri's updater builder requires an endpoint, and offline restore_update still needs it; the actual fetch uses Peppy's restricted channel-aware feed.
  config=$(node -e 'process.stdout.write(JSON.stringify({productName:"Peppy",identifier:"org.peppy.desktop",bundle:{createUpdaterArtifacts:true},plugins:{updater:{pubkey:process.env.PEPPY_UPDATE_PUBLIC_KEY,endpoints:["https://github.com/mattv8/peppy/releases/latest/download/latest.json"]}}}))')
else
  # Tauri interprets even empty Apple credentials as a request to sign.
  unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY
  unset APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID
  unset PEPPY_DESKTOP_GOOGLE_CLIENT_ID PEPPY_DESKTOP_GOOGLE_CLIENT_SECRET
  unset TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD
  macos_mode=adhoc
fi

repair_macos_dmg() {
  [[ "$RUNNER_OS" = macOS && ",$BUNDLES," == *,dmg,* ]] || return 0
  local output_dir="${PEPPY_DMG_OUTPUT_DIR:-apps/desktop/src-tauri/target/release/bundle/dmg}"
  local -a images
  while IFS= read -r -d '' image; do images+=("$image"); done < <(find "$output_dir" -maxdepth 1 -type f -name '*.dmg' -print0)
  if [ "${#images[@]}" -ne 1 ]; then
    echo "expected exactly one DMG in $output_dir; found ${#images[@]}" >&2
    return 1
  fi
  (
    local scratch
    scratch=$(mktemp -d)
    trap 'rm -rf "$scratch"' EXIT
    python3 -m venv "$scratch/venv"
    "$scratch/venv/bin/pip" install --disable-pip-version-check --only-binary=:all: --require-hashes -r packaging/macos/dmg/requirements.txt
    local -a repair_arguments=(--dmg "${images[0]}" --signing-mode "$macos_mode")
    if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then repair_arguments+=(--identity "$APPLE_SIGNING_IDENTITY"); fi
    "$scratch/venv/bin/python" packaging/macos/dmg/repair_dmg.py "${repair_arguments[@]}"
  )
}

assert_trusted_identity() {
  node -e 'const config = JSON.parse(process.argv[1]); if (config.productName !== "Peppy" || config.identifier !== "org.peppy.desktop") { process.exit(1); }' "$1" || {
    echo 'trusted builds must use the Peppy/org.peppy.desktop identity' >&2
    exit 1
  }
}

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
  if [ -n "$config" ]; then json=$(node -e 'const [base, overlay] = process.argv.slice(1).map(JSON.parse); const bundle={...base.bundle,...overlay.bundle}; if (process.env.RUNNER_OS === "Windows" && process.env.PEPPY_WINDOWS_SIGNING_MODE !== "none") { const script = `${process.env.GITHUB_WORKSPACE}\\infra\\release\\sign-windows-artifact.ps1`; bundle.windows={...(base.bundle?.windows ?? {}),signCommand:`powershell -NoProfile -ExecutionPolicy Bypass -File \"${script}\" %1`}; } console.log(JSON.stringify({...base,...overlay,bundle}));' "$json" "$config"); fi
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

if [ "$TRUSTED_UPDATE_BUILD" = true ]; then assert_trusted_identity "$json"; fi

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
  if [ "$TRUSTED_UPDATE_BUILD" = true ]; then assert_trusted_identity "$rpm_config"; fi
  pnpm --dir apps/desktop exec tauri bundle --bundles rpm --config "$rpm_config"
else
  if [ -n "$json" ]; then
    pnpm --dir apps/desktop exec tauri build --bundles "$BUNDLES" --config "$json" -- --locked
  else
    pnpm --dir apps/desktop exec tauri build --bundles "$BUNDLES" -- --locked
  fi
fi

repair_macos_dmg

if [ "$TRUSTED_UPDATE_BUILD" = true ] && [ "$RUNNER_OS" = macOS ]; then
  bundle_output_dir="${PEPPY_BUNDLE_OUTPUT_DIR:-apps/desktop/src-tauri/target/release/bundle/macos}"
  while IFS= read -r -d '' application; do
    codesign --verify --deep --strict --verbose=2 "$application"
    info_plist="$application/Contents/Info.plist"
    identifier=$(plutil -extract CFBundleIdentifier raw -o - "$info_plist")
    name=$(plutil -extract CFBundleName raw -o - "$info_plist")
    if [[ "$identifier" != org.peppy.desktop || "$name" != Peppy ]]; then
      echo 'trusted macOS bundle identity must be Peppy/org.peppy.desktop' >&2
      exit 1
    fi
    if [ "$macos_mode" = developer-id ]; then
      spctl --assess --type execute --verbose=4 "$application"
      xcrun stapler validate "$application"
    fi
  done < <(find "$bundle_output_dir" -name '*.app' -type d -print0)
fi
