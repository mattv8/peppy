#!/usr/bin/env bash

set -Eeuo pipefail

usage() {
  echo "usage: $0 <write-manifest|validate-manifest> <artifact-dir> <manifest> <revision> <debug|release>" >&2
  echo "       $0 package-artifact <source-root> <artifact-dir> <revision> <debug|release>" >&2
  echo "       $0 install-artifact <artifact-dir> <checkout-root> <revision> <debug|release>" >&2
  exit 2
}

command_name=${1:-}

required_files=(
  target/debug/libpeppy_mobile_bindings.so
  apps/android/app/src/main/jniLibs/arm64-v8a/libpeppy_mobile_bindings.so
  apps/android/app/src/main/jniLibs/x86_64/libpeppy_mobile_bindings.so
  apps/android/app/src/main/java/uniffi/peppy_mobile_bindings/peppy_mobile_bindings.kt
  apps/android/googlemessages/build/gmlink.aar
  apps/android/googlemessages/build/gmlink.aar.provenance
)

validate_arguments() {
  local artifact_dir=$1 revision=$2 native_profile=$3
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] || { echo 'revision must be a full lowercase SHA' >&2; exit 2; }
  [[ "$native_profile" == debug || "$native_profile" == release ]] || usage
  [[ -d "$artifact_dir" ]] || { echo "artifact directory is missing: $artifact_dir" >&2; exit 1; }
}

write_expected_manifest() {
  local artifact_dir=$1 revision=$2 native_profile=$3
  printf 'schema=1\nrevision=%s\nnative_profile=%s\nhost_os=Linux\nhost_arch=x86_64\ngo_version=1.27.3\nndk_version=27.2.12479018\n' \
    "$revision" "$native_profile"
  local relative_path file_hash
  for relative_path in "${required_files[@]}"; do
    [[ "$relative_path" != /* && "$relative_path" != *'..'* ]] || exit 1
    [[ -f "$artifact_dir/$relative_path" && ! -L "$artifact_dir/$relative_path" ]] || {
      echo "required native artifact is missing or unsafe: $relative_path" >&2
      exit 1
    }
    file_hash=$(shasum -a 256 "$artifact_dir/$relative_path" | awk '{print $1}')
    printf 'sha256 %s %s\n' "$file_hash" "$relative_path"
  done
}

write_manifest() {
  local artifact_dir=$1 manifest=$2 revision=$3 native_profile=$4
  validate_arguments "$artifact_dir" "$revision" "$native_profile"
  mkdir -p "$(dirname "$manifest")"
  write_expected_manifest "$artifact_dir" "$revision" "$native_profile" > "$manifest"
}

validate_manifest() {
  local artifact_dir=$1 manifest=$2 revision=$3 native_profile=$4
  validate_arguments "$artifact_dir" "$revision" "$native_profile"
  [[ -f "$manifest" && ! -L "$manifest" ]] || { echo "native artifact manifest is missing or unsafe: $manifest" >&2; exit 1; }
  [[ "$manifest" == "$artifact_dir/android-native-artifacts.manifest" ]] || {
      echo 'native artifact manifest must be packaged at the native artifact root' >&2
      exit 1
  }
  find "$artifact_dir" -type l -print -quit | grep -q . && {
    echo 'native artifact inventory contains a symlink' >&2
    exit 1
  }
  local expected_inventory actual_inventory expected
  expected_inventory=$(printf '%s\n' "${required_files[@]}" android-native-artifacts.manifest | LC_ALL=C sort)
  actual_inventory=$(cd "$artifact_dir" && find . -type f -print | sed 's|^./||' | LC_ALL=C sort)
  [[ "$actual_inventory" == "$expected_inventory" ]] || {
    echo 'native artifact inventory contains missing or unexpected files' >&2
    exit 1
  }
  expected=$(mktemp)
  write_expected_manifest "$artifact_dir" "$revision" "$native_profile" > "$expected"
  if ! cmp -s "$expected" "$manifest"; then
    rm -f "$expected"
    echo 'native artifact manifest does not match this revision, configuration, or inventory' >&2
    exit 1
  fi
  rm -f "$expected"
}

package_artifact() {
  local source_root=$1 artifact_dir=$2 revision=$3 native_profile=$4 relative_path
  [[ -d "$source_root" ]] || { echo "source root is missing: $source_root" >&2; exit 1; }
  rm -rf "$artifact_dir"
  for relative_path in "${required_files[@]}"; do
    [[ -f "$source_root/$relative_path" && ! -L "$source_root/$relative_path" ]] || {
      echo "required native producer output is missing or unsafe: $relative_path" >&2
      exit 1
    }
    mkdir -p "$artifact_dir/$(dirname "$relative_path")"
    cp "$source_root/$relative_path" "$artifact_dir/$relative_path"
  done
  write_manifest "$artifact_dir" "$artifact_dir/android-native-artifacts.manifest" "$revision" "$native_profile"
}

install_artifact() {
  local artifact_dir=$1 checkout_root=$2 revision=$3 native_profile=$4 relative_path
  [[ -d "$checkout_root" ]] || { echo "checkout root is missing: $checkout_root" >&2; exit 1; }
  validate_manifest "$artifact_dir" "$artifact_dir/android-native-artifacts.manifest" "$revision" "$native_profile"
  for relative_path in "${required_files[@]}"; do
    mkdir -p "$checkout_root/$(dirname "$relative_path")"
    cp "$artifact_dir/$relative_path" "$checkout_root/$relative_path"
  done
}

case "$command_name" in
  write-manifest) write_manifest "${2:-}" "${3:-}" "${4:-}" "${5:-}" ;;
  validate-manifest) validate_manifest "${2:-}" "${3:-}" "${4:-}" "${5:-}" ;;
  package-artifact) package_artifact "${2:-}" "${3:-}" "${4:-}" "${5:-}" ;;
  install-artifact) install_artifact "${2:-}" "${3:-}" "${4:-}" "${5:-}" ;;
  *) usage ;;
esac
