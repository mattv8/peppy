#!/usr/bin/env bash

set -Eeuo pipefail

: "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"

normalize_bool() {
  local name="$1"
  local value="${2:-}"

  if [ -z "$value" ]; then
    printf 'false\n'
    return
  fi

  case "$value" in
    true|false) printf '%s\n' "$value" ;;
    *)
      echo "invalid boolean for $name: $value" >&2
      exit 1
      ;;
  esac
}

bool_or() {
  local value

  for value in "$@"; do
    if [ "$value" = true ]; then
      printf 'true\n'
      return
    fi
  done

  printf 'false\n'
}

FORCE_FULL="$(normalize_bool FILTER_FORCE_FULL "${FILTER_FORCE_FULL:-}")"
PRODUCTION_PUSH="$(normalize_bool PRODUCTION_PUSH "${PRODUCTION_PUSH:-}")"
RUST_SHARED_FILTER="$(normalize_bool FILTER_RUST_SHARED "${FILTER_RUST_SHARED:-}")"
RUST_MOBILE_FILTER="$(normalize_bool FILTER_RUST_MOBILE "${FILTER_RUST_MOBILE:-}")"
RUST_DESKTOP_FILTER="$(normalize_bool FILTER_RUST_DESKTOP "${FILTER_RUST_DESKTOP:-}")"
RUST_BROWSER_FILTER="$(normalize_bool FILTER_RUST_BROWSER "${FILTER_RUST_BROWSER:-}")"
RUST_SERVER_FILTER="$(normalize_bool FILTER_RUST_SERVER "${FILTER_RUST_SERVER:-}")"
ANDROID_BINDINGS_FILTER="$(normalize_bool FILTER_ANDROID_BINDINGS "${FILTER_ANDROID_BINDINGS:-}")"
DESKTOP_FILTER="$(normalize_bool FILTER_DESKTOP "${FILTER_DESKTOP:-}")"
WEB_FILTER="$(normalize_bool FILTER_WEB "${FILTER_WEB:-}")"
ANDROID_FILTER="$(normalize_bool FILTER_ANDROID "${FILTER_ANDROID:-}")"
IOS_FILTER="$(normalize_bool FILTER_IOS "${FILTER_IOS:-}")"
SERVER_FILTER="$(normalize_bool FILTER_SERVER "${FILTER_SERVER:-}")"
BUILD_FILTER="$(normalize_bool FILTER_BUILD "${FILTER_BUILD:-}")"
CI_FILTER="$(normalize_bool FILTER_CI "${FILTER_CI:-}")"
UNCLASSIFIED_FILTER="$(normalize_bool FILTER_UNCLASSIFIED "${FILTER_UNCLASSIFIED:-}")"

ALL="$(bool_or "$FORCE_FULL" "$CI_FILTER" "$UNCLASSIFIED_FILTER")"
RUST_FILTER="$(bool_or "$RUST_SHARED_FILTER" "$RUST_MOBILE_FILTER" "$RUST_DESKTOP_FILTER" "$RUST_BROWSER_FILTER" "$RUST_SERVER_FILTER")"
RUST="$(bool_or "$ALL" "$RUST_FILTER")"
INTEGRATION="$(bool_or "$ALL" "$RUST_FILTER" "$DESKTOP_FILTER" "$SERVER_FILTER")"
# The Linux desktop job hosts ignored integration tests, so integration requires desktop.
DESKTOP_FULL="$(bool_or "$ALL" "$RUST_SHARED_FILTER" "$RUST_DESKTOP_FILTER" "$DESKTOP_FILTER")"
DESKTOP="$(bool_or "$DESKTOP_FULL" "$INTEGRATION" "$WEB_FILTER" "$RUST_BROWSER_FILTER")"
if [ "$DESKTOP_FULL" = true ]; then
  DESKTOP_MATRIX='["ubuntu-24.04","macos-15","windows-2025"]'
else
  DESKTOP_MATRIX='["ubuntu-24.04"]'
fi
ANDROID="$(bool_or "$ALL" "$RUST_SHARED_FILTER" "$RUST_MOBILE_FILTER" "$ANDROID_FILTER")"
SWIFT_BINDINGS="$(bool_or "$ALL" "$RUST_SHARED_FILTER" "$RUST_MOBILE_FILTER" "$IOS_FILTER" "$ANDROID_BINDINGS_FILTER")"
CONTAINER_IMAGE="$(bool_or "$ALL" "$RUST_SHARED_FILTER" "$RUST_DESKTOP_FILTER" "$RUST_BROWSER_FILTER" "$RUST_SERVER_FILTER" "$SERVER_FILTER" "$WEB_FILTER")"
ARTIFACTS_ANDROID="$(bool_or "$ANDROID" "$BUILD_FILTER")"
ARTIFACTS_DESKTOP="$(bool_or "$DESKTOP_FULL" "$BUILD_FILTER")"
ARTIFACTS_IOS="$(bool_or "$ALL" "$RUST_SHARED_FILTER" "$RUST_MOBILE_FILTER" "$IOS_FILTER" "$BUILD_FILTER")"
DEVELOPMENT_ARTIFACTS="$(bool_or "$ARTIFACTS_ANDROID" "$ARTIFACTS_DESKTOP" "$ARTIFACTS_IOS")"
if [ "$PRODUCTION_PUSH" = true ]; then
  ARTIFACTS_ANDROID=false
  ARTIFACTS_DESKTOP=false
  ARTIFACTS_IOS=false
  DEVELOPMENT_ARTIFACTS=false
fi

cat >> "$GITHUB_OUTPUT" <<EOF
rust=$RUST
integration=$INTEGRATION
desktop=$DESKTOP
desktop_matrix=$DESKTOP_MATRIX
android=$ANDROID
swift_bindings=$SWIFT_BINDINGS
container_image=$CONTAINER_IMAGE
artifacts_android=$ARTIFACTS_ANDROID
artifacts_desktop=$ARTIFACTS_DESKTOP
artifacts_ios=$ARTIFACTS_IOS
development_artifacts=$DEVELOPMENT_ARTIFACTS
EOF
