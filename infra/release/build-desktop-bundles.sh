#!/usr/bin/env bash
set -euo pipefail

config=''
macos_distribution="${PEPPY_MACOS_DISTRIBUTION-direct}"
case "$macos_distribution" in direct|app-store) ;; *) echo 'PEPPY_MACOS_DISTRIBUTION must be direct or app-store' >&2; exit 1 ;; esac

if [ "$macos_distribution" = app-store ]; then
  [ "$RUNNER_OS" = macOS ] || { echo 'app-store distribution requires a macOS runner' >&2; exit 1; }
  [ "${GITHUB_ACTIONS:-}" = true ] || { echo 'app-store distribution must run in GitHub Actions' >&2; exit 1; }
  [ "$BUNDLES" = app ] || { echo 'app-store distribution builds only the app bundle' >&2; exit 1; }
  for required in VERSION MACOS_BUILD_NUMBER APPLE_MAS_CERTIFICATE_P12 APPLE_MAS_CERTIFICATE_PASSWORD APPLE_MAS_INSTALLER_CERTIFICATE APPLE_MAS_PROVISIONING_PROFILE_BASE64 APPLE_MAS_SIGNING_IDENTITY APPLE_MAS_INSTALLER_IDENTITY; do
    [[ "${!required:-}" =~ [^[:space:]] ]] || { echo "$required is required for app-store builds" >&2; exit 1; }
  done
  [[ "$MACOS_BUILD_NUMBER" =~ ^[1-9][0-9]*$ ]] || { echo 'MACOS_BUILD_NUMBER must be a positive integer' >&2; exit 1; }
  # Tauri recognizes these standard names; keep App Store credentials separate from Developer ID inputs.
  export APPLE_CERTIFICATE="$APPLE_MAS_CERTIFICATE_P12"
  export APPLE_CERTIFICATE_PASSWORD="$APPLE_MAS_CERTIFICATE_PASSWORD"
  export APPLE_SIGNING_IDENTITY="$APPLE_MAS_SIGNING_IDENTITY"
  unset APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD
  config=$(cat packaging/macos/app-store/tauri.app-store.json)
elif [ "$TRUSTED_UPDATE_BUILD" = true ]; then
  macos_mode="${PEPPY_MACOS_SIGNING_MODE-developer-id}"
  windows_mode="${PEPPY_WINDOWS_SIGNING_MODE-authenticode}"
  case "$macos_mode" in developer-id|adhoc) ;; *) echo 'PEPPY_MACOS_SIGNING_MODE must be developer-id or adhoc' >&2; exit 1 ;; esac
  case "$windows_mode" in authenticode|none) ;; *) echo 'PEPPY_WINDOWS_SIGNING_MODE must be authenticode or none' >&2; exit 1 ;; esac
  case "$PEPPY_UPDATE_CHANNEL" in stable|staging) ;; *) echo 'trusted update builds require stable or staging channel' >&2; exit 1 ;; esac
  for required in VERSION PEPPY_UPDATE_PUBLIC_KEY TAURI_SIGNING_PRIVATE_KEY; do
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

# GNU coreutils spells decode `--decode`; older macOS base64 only accepts `-D`.
decode_base64() {
  local flag=--decode
  base64 --decode </dev/null >/dev/null 2>&1 || flag=-D
  printf '%s' "$1" | base64 "$flag"
}

package_app_store() {
  local bundle_output_dir profile profile_xml application_certificate installer_certificate entitlements signed_entitlements keychain keychain_password team_id application_identifier signed_team_id signed_application_identifier app pkg_dir pkg
  local -a previous_keychains
  bundle_output_dir="${PEPPY_BUNDLE_OUTPUT_DIR:-apps/desktop/src-tauri/target/release/bundle/macos}"
  profile=$(mktemp)
  profile_xml=$(mktemp)
  application_certificate=$(mktemp)
  installer_certificate=$(mktemp)
  entitlements=$(mktemp)
  signed_entitlements=$(mktemp)
  keychain="${TMPDIR:-/tmp}/peppy-mas-$(uuidgen).keychain-db"
  keychain_password=$(uuidgen)
  previous_keychains=()
  while IFS= read -r previous_keychain; do
    previous_keychain=${previous_keychain#\"}
    previous_keychain=${previous_keychain%\"}
    [ -n "$previous_keychain" ] && previous_keychains+=("$previous_keychain")
  done < <(security list-keychains -d user)
  cleanup_app_store_keychain() {
    trap - RETURN
    if [ "${#previous_keychains[@]}" -gt 0 ]; then
      security list-keychains -d user -s "${previous_keychains[@]}" >/dev/null 2>&1 || true
    fi
    security delete-keychain "$keychain" >/dev/null 2>&1 || true
    rm -f "$profile" "$profile_xml" "$application_certificate" "$installer_certificate" "$entitlements" "$signed_entitlements"
  }
  trap cleanup_app_store_keychain RETURN
  decode_base64 "$APPLE_MAS_PROVISIONING_PROFILE_BASE64" > "$profile"
  decode_base64 "$APPLE_MAS_CERTIFICATE_P12" > "$application_certificate"
  decode_base64 "$APPLE_MAS_INSTALLER_CERTIFICATE" > "$installer_certificate"
  security cms -D -i "$profile" > "$profile_xml"
  team_id=$(plutil -extract 'Entitlements.com\.apple\.developer\.team-identifier' raw -o - "$profile_xml")
  application_identifier=$(plutil -extract 'Entitlements.application-identifier' raw -o - "$profile_xml")
  [ -n "$team_id" ] || { echo 'App Store provisioning profile does not declare a team identifier' >&2; return 1; }
  [ "$application_identifier" = "$team_id.org.peppy.desktop" ] || { echo "App Store provisioning profile application identifier must equal $team_id.org.peppy.desktop" >&2; return 1; }
  security create-keychain -p "$keychain_password" "$keychain"
  security set-keychain-settings -lut 21600 "$keychain"
  security unlock-keychain -p "$keychain_password" "$keychain"
  if [ "${#previous_keychains[@]}" -gt 0 ]; then
    security list-keychains -d user -s "$keychain" "${previous_keychains[@]}"
  else
    security list-keychains -d user -s "$keychain"
  fi
  security import "$application_certificate" -k "$keychain" -P "$APPLE_MAS_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
  security import "$installer_certificate" -k "$keychain" -P "$APPLE_MAS_CERTIFICATE_PASSWORD" -T /usr/bin/productbuild
  cp packaging/macos/app-store/entitlements.plist "$entitlements"
  plutil -replace 'com\.apple\.application-identifier' -string "$application_identifier" "$entitlements"
  plutil -replace 'com\.apple\.developer\.team-identifier' -string "$team_id" "$entitlements"
  while IFS= read -r -d '' app; do
    install -m 0644 "$profile" "$app/Contents/embedded.provisionprofile"
    plutil -replace CFBundleVersion -string "$MACOS_BUILD_NUMBER" "$app/Contents/Info.plist"
    codesign --force --sign "$APPLE_MAS_SIGNING_IDENTITY" --keychain "$keychain" --entitlements "$entitlements" "$app"
    codesign --verify --deep --strict --verbose=2 "$app"
    codesign -d --entitlements :- "$app" > "$signed_entitlements"
    plutil -extract 'com\.apple\.security\.app-sandbox' raw -o - "$signed_entitlements" | grep -Fqx true || { echo 'App Store bundle is missing the app sandbox entitlement' >&2; return 1; }
    signed_team_id=$(plutil -extract 'com\.apple\.developer\.team-identifier' raw -o - "$signed_entitlements")
    signed_application_identifier=$(plutil -extract 'com\.apple\.application-identifier' raw -o - "$signed_entitlements")
    [ "$signed_team_id" = "$team_id" ] || { echo 'App Store bundle team identifier does not match the provisioning profile' >&2; return 1; }
    [ "$signed_application_identifier" = "$application_identifier" ] || { echo 'App Store bundle application identifier does not match the provisioning profile' >&2; return 1; }
    codesign -d -r- "$app" 2>&1 | grep -Fq "$team_id" || { echo 'App Store bundle designated requirement does not match the provisioning-profile team' >&2; return 1; }
    pkg_dir="${PEPPY_PKG_OUTPUT_DIR:-apps/desktop/src-tauri/target/release/bundle/macos}"
    mkdir -p "$pkg_dir"
    pkg="$pkg_dir/Peppy.pkg"
    productbuild --component "$app" /Applications --sign "$APPLE_MAS_INSTALLER_IDENTITY" --keychain "$keychain" "$pkg"
  done < <(find "$bundle_output_dir" -name '*.app' -type d -print0)
}

assert_developer_id_signature() {
  local artifact="$1"
  local require_runtime="$2"
  local details
  details=$(codesign -dvvv "$artifact" 2>&1) || {
    echo "could not inspect Developer ID signature for $artifact" >&2
    return 1
  }
  grep -Fqx "TeamIdentifier=$APPLE_TEAM_ID" <<<"$details" || {
    echo "Developer ID signature for $artifact does not match APPLE_TEAM_ID" >&2
    return 1
  }
  grep -Eq '^Authority=Developer ID Application: .+' <<<"$details" || {
    echo "$artifact is not signed with a Developer ID Application identity" >&2
    return 1
  }
  if [ "$require_runtime" = true ] && ! grep -Eq 'flags=[^[:space:]]*\([^)]*runtime(,|\))' <<<"$details"; then
    echo "$artifact is not signed with the hardened runtime" >&2
    return 1
  fi
}

notarize_repaired_dmg() {
  local image="$1"
  local result
  local status
  local command_status
  local submission_id
  command_status=0
  result=$(xcrun notarytool submit "$image" --wait --output-format json --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID") || command_status=$?
  submission_id=$(node -e 'try { const result = JSON.parse(process.argv[1]); if (typeof result.id === "string" && /^[A-Za-z0-9-]+$/.test(result.id)) process.stdout.write(result.id); } catch {}' "$result")
  if [ "$command_status" -ne 0 ]; then
    echo "repaired DMG notarization submission failed for $image${submission_id:+ (submission $submission_id)}" >&2
    return 1
  fi
  if ! status=$(node -e 'try { const result = JSON.parse(process.argv[1]); if (typeof result.status !== "string") process.exit(1); process.stdout.write(result.status); } catch { process.exit(1); }' "$result"); then
    echo "repaired DMG notarization returned malformed JSON for $image" >&2
    return 1
  fi
  if [ "$status" != Accepted ]; then
    echo "repaired DMG notarization status was $status, expected Accepted${submission_id:+ (submission $submission_id)}" >&2
    return 1
  fi
  command_status=0
  xcrun stapler staple "$image" || command_status=$?
  if [ "$command_status" -ne 0 ]; then
    echo "could not staple repaired DMG $image" >&2
    return 1
  fi
  command_status=0
  xcrun stapler validate "$image" || command_status=$?
  if [ "$command_status" -ne 0 ]; then
    echo "repaired DMG stapling validation failed for $image" >&2
    return 1
  fi
  command_status=0
  spctl --assess --type open --verbose=4 "$image" || command_status=$?
  if [ "$command_status" -ne 0 ]; then
    echo "Gatekeeper rejected repaired DMG $image" >&2
    return 1
  fi
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
  if [ -n "$config" ]; then json=$(node -e 'const object = value => value && typeof value === "object" && !Array.isArray(value); const merge = (base, overlay) => { if (!object(overlay)) return overlay; const result = object(base) ? {...base} : {}; for (const [key, value] of Object.entries(overlay)) result[key] = object(value) ? merge(result[key], value) : value; return result; }; const [base, overlay] = process.argv.slice(1).map(JSON.parse); const merged=merge(base, overlay); if (process.env.RUNNER_OS === "Windows" && process.env.PEPPY_WINDOWS_SIGNING_MODE !== "none") { const bundle=merged.bundle ?? {}; const script = `${process.env.GITHUB_WORKSPACE}\\infra\\release\\sign-windows-artifact.ps1`; bundle.windows={...(bundle.windows ?? {}),signCommand:`powershell -NoProfile -ExecutionPolicy Bypass -File \"${script}\" %1`}; merged.bundle=bundle; } console.log(JSON.stringify(merged));' "$json" "$config"); fi
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

if [ "$macos_distribution" = direct ]; then repair_macos_dmg; fi

if [ "$macos_distribution" = app-store ]; then package_app_store; fi

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
      assert_developer_id_signature "$application" true
      spctl --assess --type execute --verbose=4 "$application"
      xcrun stapler validate "$application"
    fi
  done < <(find "$bundle_output_dir" -name '*.app' -type d -print0)
  if [ "$macos_mode" = developer-id ] && [[ ",$BUNDLES," == *,dmg,* ]]; then
    dmg_output_dir="${PEPPY_DMG_OUTPUT_DIR:-apps/desktop/src-tauri/target/release/bundle/dmg}"
    while IFS= read -r -d '' image; do
      codesign --verify --strict --verbose=2 "$image"
      assert_developer_id_signature "$image" false
      notarize_repaired_dmg "$image"
    done < <(find "$dmg_output_dir" -maxdepth 1 -type f -name '*.dmg' -print0)
  fi
fi
