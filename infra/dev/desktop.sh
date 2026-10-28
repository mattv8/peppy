#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: bash infra/dev/desktop.sh <dev|build|open>

On macOS, builds development .app bundle without Developer ID signing
or notarization. Set PEPPY_MACOS_SIGNING_IDENTITY to a stable identity for
consistent Keychain access; unset uses ad-hoc signing. From WSL, runs the native
Windows Tauri toolchain against this same NTFS checkout. Linux native desktop
builds are not provided by this helper.
EOF
  exit 64
}

die() {
  printf 'desktop: %s\n' "$*" >&2
  exit 1
}

action=${1:-}
[[ $# -eq 1 ]] || usage
case "$action" in dev|build|open) ;; *) usage ;; esac

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
caller_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd -P)
source_root=$(CDPATH='' cd -- "${PEPPY_SOURCE_TREE:-$caller_root}" && pwd -L)

target_dir=${CARGO_TARGET_DIR:-"$caller_root/apps/desktop/src-tauri/target"}
if [[ "$target_dir" != /* ]]; then
  target_dir="$caller_root/$target_dir"
fi

check_native_pins() {
  local expected_node expected_rust node_version pnpm_version rust_version
  expected_node=$(tr -d '[:space:]' < "$source_root/.node-version")
  expected_rust=$(sed -n 's/^channel = "\([^"]*\)"/\1/p' "$source_root/rust-toolchain.toml")
  command -v node >/dev/null || die "Node $expected_node is required."
  command -v pnpm >/dev/null || die "pnpm 12.8.1 is required."
  command -v rustc >/dev/null || die "Rust $expected_rust is required."
  node_version=$(node --version)
  pnpm_version=$(pnpm --version)
  rust_version=$(rustc --version)
  node_matches_selector "$expected_node" "$node_version" || die "Node $expected_node is required (found $node_version)."
  [[ "$pnpm_version" == "12.8.1" ]] || die "pnpm 12.8.1 is required (found $pnpm_version)."
  [[ "$rust_version" == "rustc $expected_rust "* ]] || die "Rust $expected_rust is required (found $rust_version)."
}

node_matches_selector() {
  local selector="$1" version="$2"
  [[ "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || return 1
  if [[ "$selector" =~ ^[0-9]+$ ]]; then
    [[ "$version" =~ ^v${selector}\.[0-9]+\.[0-9]+$ ]]
  elif [[ "$selector" =~ ^[0-9]+\.[0-9]+$ ]]; then
    [[ "$version" =~ ^v${selector}\.[0-9]+$ ]]
  elif [[ "$selector" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    [[ "$version" == "v$selector" ]]
  else
    return 1
  fi
}

find_homebrew() {
  if command -v brew >/dev/null 2>&1; then
    command -v brew
  elif [[ -x /opt/homebrew/bin/brew ]]; then
    printf '%s\n' /opt/homebrew/bin/brew
  elif [[ -x /usr/local/bin/brew ]]; then
    printf '%s\n' /usr/local/bin/brew
  else
    printf '\n'
  fi
}

select_macos_pinned_tools() {
  local expected_node expected_rust brew prefix major formula
  expected_node=$(tr -d '[:space:]' < "$source_root/.node-version")
  expected_rust=$(sed -n 's/^channel = "\([^"]*\)"/\1/p' "$source_root/rust-toolchain.toml")

  if ! command -v node >/dev/null 2>&1 || ! node_matches_selector "$expected_node" "$(node --version 2>/dev/null)"; then
    brew=$(find_homebrew)
    major=${expected_node%%.*}
    for formula in "node@$major" node; do
      if [[ -n "$brew" ]] && prefix=$("$brew" --prefix "$formula" 2>/dev/null) && [[ -x "$prefix/bin/node" ]] && node_matches_selector "$expected_node" "$("$prefix/bin/node" --version 2>/dev/null)"; then
        PATH="$prefix/bin:$PATH"
        export PATH
        break
      fi
    done
  fi

  if ! command -v rustc >/dev/null 2>&1 || [[ "$(rustc --version 2>/dev/null)" != "rustc $expected_rust "* ]]; then
    if [[ -z ${brew:-} ]]; then
      brew=$(find_homebrew)
    fi
    if [[ -n ${brew:-} ]] && prefix=$("$brew" --prefix rustup 2>/dev/null) && [[ -d "$prefix/bin" ]]; then
      PATH="$prefix/bin:$PATH"
      export PATH
    fi
  fi
}

verify_macos_signing_identity() {
  local identity="$1"
  if [[ -z "$identity" ]]; then
    die "PEPPY_MACOS_SIGNING_IDENTITY is set but empty. Use a stable Apple Development or Developer ID identity, or unset the variable for ad-hoc signing."
  fi
  if [[ "$identity" == "-" ]]; then
    die "Ad-hoc signing (-) does not provide a stable identity. Unset PEPPY_MACOS_SIGNING_IDENTITY or choose an Apple Development (Xcode + Apple ID) or Developer ID identity."
  fi
  # Exact full name ("...") or SHA-1 hash column; partial names would be ambiguous.
  if ! security find-identity -v -p codesigning 2>/dev/null | grep -Fq -e "\"$identity\"" -e ") $identity \""; then
    die "Code signing identity not found: $identity. Run 'security find-identity -v -p codesigning' to list available identities. Install Xcode and sign in with Apple ID to create an Apple Development certificate."
  fi
}

macos() {
  local dev_config="$source_root/apps/desktop/src-tauri/tauri.dev.conf.json"
  local app_bundle="$target_dir/release/bundle/macos/Peppy_dev.app"
  local dev_tls_enabled dev_tls_cert bundle_identifier
  local -a cargo_args=(--locked)
  if [[ "$action" != open ]]; then
    dev_tls_enabled=$(python3 "$caller_root/infra/dev/dev_tls.py" enabled) || die "Could not read the DEBUG TLS setting."
    case "$dev_tls_enabled" in
      true)
        python3 "$caller_root/infra/dev/dev_tls.py" prepare || die "Could not prepare local development TLS."
        dev_tls_cert=$(python3 "$caller_root/infra/dev/dev_tls.py" ca-path) || die "Could not locate the local development TLS CA."
        [[ "$dev_tls_cert" == /* ]] || die "The local development TLS helper returned a non-absolute CA path."
        [[ -f "$dev_tls_cert" ]] || die "Local development TLS did not produce its public CA."
        cargo_args+=(--features dev-local-tls)
        ;;
      false) ;;
      *) die "The local development TLS helper returned an invalid DEBUG setting." ;;
    esac
  fi
  case "$action" in
    open)
      [[ -d "$app_bundle" ]] || die "No current macOS bundle at $app_bundle; run desktop-bundle first."
      open -n "$app_bundle"
      ;;
    dev)
      (cd "$source_root" && select_macos_pinned_tools && check_native_pins && pnpm install --frozen-lockfile && CARGO_TARGET_DIR="$target_dir" PEPPY_DEV_TLS_CERT="${dev_tls_cert:-}" pnpm --dir apps/desktop exec tauri dev --config "$dev_config" -- "${cargo_args[@]}")
      ;;
    build)
      if [[ ${PEPPY_MACOS_SIGNING_IDENTITY+set} == set ]]; then
        verify_macos_signing_identity "$PEPPY_MACOS_SIGNING_IDENTITY"
        (cd "$source_root" && select_macos_pinned_tools && check_native_pins && pnpm install --frozen-lockfile && CARGO_TARGET_DIR="$target_dir" PEPPY_DEV_TLS_CERT="${dev_tls_cert:-}" APPLE_SIGNING_IDENTITY="$PEPPY_MACOS_SIGNING_IDENTITY" pnpm --dir apps/desktop exec tauri build --bundles app --config "$dev_config" -- "${cargo_args[@]}")
      else
        (cd "$source_root" && select_macos_pinned_tools && check_native_pins && pnpm install --frozen-lockfile && CARGO_TARGET_DIR="$target_dir" PEPPY_DEV_TLS_CERT="${dev_tls_cert:-}" pnpm --dir apps/desktop exec tauri build --bundles app --config "$dev_config" -- "${cargo_args[@]}")
      fi
      [[ -d "$app_bundle" ]] || die "Tauri completed without the expected bundle: $app_bundle"
      if [[ ${PEPPY_MACOS_SIGNING_IDENTITY+set} != set ]]; then
        bundle_identifier=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app_bundle/Contents/Info.plist") || die "Could not read the built bundle identifier."
        [[ -n "$bundle_identifier" ]] || die "The built bundle identifier is empty."
        /usr/bin/codesign --force --sign - --identifier "$bundle_identifier" \
          --preserve-metadata=entitlements,flags,runtime "$app_bundle"
      fi
      /usr/bin/codesign --verify --deep --strict "$app_bundle"
      ;;
  esac
}

wsl_windows() {
  local caller_windows source_windows target_windows powershell
  command -v wslpath >/dev/null || die "WSL interop is unavailable; run this from WSL backed by a Windows NTFS drive."
  caller_windows=$(wslpath -w "$caller_root")
  source_windows=$(wslpath -w "$source_root")
  [[ "$caller_windows" =~ ^[A-Za-z]:\\ ]] || die "Native Windows builds require the calling checkout on a drive-letter NTFS path; ext4 and UNC paths are unsupported."
  [[ "$source_windows" =~ ^[A-Za-z]:\\ ]] || die "Native Windows builds require this checkout on a drive-letter NTFS path; ext4 and UNC paths are unsupported."
  target_windows=$(wslpath -w "$target_dir")
  [[ "$target_windows" =~ ^[A-Za-z]:\\ ]] || die "CARGO_TARGET_DIR must resolve to a drive-letter Windows path."
  if command -v powershell.exe >/dev/null; then
    powershell=$(command -v powershell.exe)
  elif command -v pwsh.exe >/dev/null; then
    powershell=$(command -v pwsh.exe)
  else
    die "Windows PowerShell was not found through WSL interop."
  fi
  "$powershell" -NoProfile -ExecutionPolicy Bypass -File "${caller_windows}\\infra\\dev\\windows-desktop.ps1" \
    -Action "$action" -RepoPath "$source_windows" -CargoTargetDir "$target_windows"
}

case "$(uname -s)" in
  Darwin) macos ;;
  Linux)
    if grep -qi microsoft /proc/sys/kernel/osrelease 2>/dev/null || [[ -n ${WSL_INTEROP:-} ]]; then
      wsl_windows
    else
      die "Native desktop development is supported on macOS or from WSL using the Windows toolchain; Linux packaging remains CI-only."
    fi
    ;;
  *) die "Unsupported host; use macOS or WSL with Windows interop." ;;
esac
