#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(git -C "$script_dir/../.." rev-parse --show-toplevel)
# shellcheck source=infra/release/lib.sh
# shellcheck disable=SC1091
source "$repo_root/infra/release/lib.sh"

fail() {
  printf '%s\n' "$1" >&2
  exit "${2:-1}"
}

tag_for() {
  local component=$1 version=$2 prefix
  prefix=$(peppy_tag_prefix "$component") || fail 'invalid component' 2
  printf '%s%s\n' "$prefix" "$version"
}

discover() {
  [ "$#" -eq 3 ] || fail 'usage: discover <desktop|android> <sha> <requested-version>' 2
  local component=$1 sha=$2 requested_version=$3 prefix regex tag version candidates=()
  case "$component" in desktop|android) ;; *) fail 'invalid component' 2 ;; esac
  [ "$sha" = "$(git rev-parse HEAD)" ] || fail 'discovery SHA must be the checked-out HEAD'
  prefix=$(peppy_tag_prefix "$component")
  regex=$(peppy_stable_tag_regex "$prefix")
  while IFS= read -r tag; do
    [[ "$tag" =~ $regex ]] && candidates+=("$tag")
  done < <(git tag --points-at "$sha")
  [ "${#candidates[@]}" -eq 1 ] || fail 'expected exactly one canonical stable component tag'
  version=${candidates[0]#"$prefix"}
  if [ -n "$requested_version" ] && [ "$requested_version" != "$version" ]; then
    fail 'requested version does not match the existing component tag'
  fi
  printf 'version=%s\nexisting=true\n' "$version"
}

expected_assets() {
  local component=$1 version=$2 _target asset
  case "$component" in
    desktop)
      while IFS='|' read -r _target asset; do
        printf '%s\n%s.sig\n' "$asset" "$asset"
      done < <(release_update_targets "$version")
      printf 'latest.json\npeppy-%s-macos-aarch64.dmg\n' "$version"
      ;;
    android)
      printf 'peppy-%s-android.apk\npeppy-%s-android.aab\n' "$version" "$version"
      ;;
    *) fail 'invalid component' 2 ;;
  esac
  printf 'SHA256SUMS\nSHA256SUMS.sig\n'
}

hashed_assets() {
  expected_assets "$@" | awk '$0 != "SHA256SUMS" && $0 != "SHA256SUMS.sig"'
}

verify_remote_tag() {
  local tag=$1 sha=$2 lines remote_sha peeled_sha
  lines=$(git ls-remote --tags origin "refs/tags/$tag" "refs/tags/$tag^{}") || fail 'could not read the authoritative remote tag'
  remote_sha=$(printf '%s\n' "$lines" | awk -v ref="refs/tags/$tag" '$2 == ref { print $1 }')
  peeled_sha=$(printf '%s\n' "$lines" | awk -v ref="refs/tags/$tag^{}" '$2 == ref { print $1 }')
  [ "$(printf '%s\n' "$remote_sha" | grep -c . || true)" -le 1 ] || fail 'remote tag is ambiguous'
  [ "$(printf '%s\n' "$peeled_sha" | grep -c . || true)" -le 1 ] || fail 'remote peeled tag is ambiguous'
  [ -n "$remote_sha" ] || fail 'remote component tag is missing'
  if [ -n "$peeled_sha" ]; then remote_sha=$peeled_sha; fi
  [ "$remote_sha" = "$sha" ] || fail 'remote component tag does not point at the requested SHA'
}

verify_release_metadata() {
  local tag=$1 expected_json=$2 release_json actual_json
  release_json=$(gh release view "$tag" --json tagName,isDraft,isPrerelease,targetCommitish,assets) || fail 'could not read published release metadata'
  actual_json=$(printf '%s' "$release_json" | jq -c '[.assets[].name] | sort') || fail 'published release metadata is invalid'
  [ "$actual_json" = "$expected_json" ] || fail 'published release assets are incomplete or unexpected'
  printf '%s' "$release_json" | jq -e --arg tag "$tag" '.tagName == $tag and .isDraft == false and .isPrerelease == false' >/dev/null || fail 'release is not a published stable release'
}

download_expected_assets() {
  local tag=$1 directory=$2 asset
  while IFS= read -r asset; do
    gh release download "$tag" --pattern "$asset" --dir "$directory" >/dev/null || fail 'could not download an expected release asset'
    [ -f "$directory/$asset" ] || fail 'release download did not produce the expected asset'
  done
}

verify_checksums() {
  local directory=$1 expected_json=$2 gpg_private_home=$3 gpg_verify_home=$4 selected_key=$5 primary_keys
  [ -n "${PEPPY_RELEASE_GPG_KEY:-}" ] || fail 'release checksum verification authority is unavailable'
  [ -n "${PEPPY_RELEASE_GPG_KEY_ID:-}" ] || fail 'release checksum verification key selector is unavailable'
  mkdir -p "$gpg_private_home" "$gpg_verify_home"
  chmod 700 "$gpg_private_home" "$gpg_verify_home"
  printf '%s' "$PEPPY_RELEASE_GPG_KEY" | GNUPGHOME="$gpg_private_home" gpg --batch --quiet --import >/dev/null 2>&1 || fail 'release checksum verification authority is invalid'
  primary_keys=$(GNUPGHOME="$gpg_private_home" gpg --batch --with-colons --fingerprint "$PEPPY_RELEASE_GPG_KEY_ID" 2>/dev/null | awk -F: '
    $1 == "pub" { primary_count++; need_fingerprint = 1; next }
    $1 == "fpr" && need_fingerprint { print $10; need_fingerprint = 0 }
  ') || fail 'release checksum verification key selector did not resolve exactly one primary key'
  [ "$(printf '%s\n' "$primary_keys" | grep -c . || true)" -eq 1 ] || fail 'release checksum verification key selector did not resolve exactly one primary key'
  GNUPGHOME="$gpg_private_home" gpg --batch --export "$primary_keys" > "$selected_key" 2>/dev/null || fail 'could not export selected checksum verification key'
  [ -s "$selected_key" ] || fail 'selected checksum verification key is empty'
  GNUPGHOME="$gpg_verify_home" gpg --batch --quiet --import "$selected_key" >/dev/null 2>&1 || fail 'could not isolate checksum verification key'
  gpgconf --homedir "$gpg_private_home" --kill gpg-agent >/dev/null 2>&1 || :
  rm -rf "$gpg_private_home" "$selected_key"
  GNUPGHOME="$gpg_verify_home" gpg --batch --verify "$directory/SHA256SUMS.sig" "$directory/SHA256SUMS" >/dev/null 2>&1 || fail 'release checksums have an invalid signature'
  jq -r '.[]' <<<"$expected_json" | awk '
    /^[A-Za-z0-9][A-Za-z0-9._+-]*$/ { next }
    { exit 1 }
  ' || fail 'expected release asset name is unsafe'
  awk '
    /^[0-9a-f]+  [A-Za-z0-9][A-Za-z0-9._+-]*$/ && length($1) == 64 { print $2; next }
    { exit 1 }
  ' "$directory/SHA256SUMS" | jq -R . | jq -cs 'sort' | cmp -s - <(jq -c 'sort' <<<"$expected_json") || fail 'release checksums do not cover exactly the expected assets'
  (cd "$directory" && sha256sum --check --strict SHA256SUMS >/dev/null) || fail 'release checksum verification failed'
}

verify() (
  [ "$#" -eq 3 ] || fail 'usage: verify <desktop|android> <version> <sha>' 2
  local component=$1 version=$2 sha=$3 tag temporary_dir='' gpg_private_home='' gpg_verify_home='' expected_json hashed_json
  # shellcheck disable=SC2317,SC2329 # Invoked by the EXIT trap; diagnostic differs between ShellCheck 0.9 and 0.11.
  cleanup_verify() {
    local status=$?
    [ -z "$gpg_private_home" ] || [ ! -d "$gpg_private_home" ] || gpgconf --homedir "$gpg_private_home" --kill gpg-agent >/dev/null 2>&1 || :
    [ -z "$gpg_verify_home" ] || [ ! -d "$gpg_verify_home" ] || gpgconf --homedir "$gpg_verify_home" --kill gpg-agent >/dev/null 2>&1 || :
    rm -rf -- "$temporary_dir" "$gpg_private_home" "$gpg_verify_home" || :
    exit "$status"
  }
  trap cleanup_verify EXIT
  case "$component" in desktop|android) ;; *) fail 'invalid component' 2 ;; esac
  [[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || fail 'invalid stable version' 2
  [[ "$sha" =~ ^[0-9a-f]{40}$ ]] || fail 'invalid SHA' 2
  command -v jq >/dev/null || fail 'jq is required'
  tag=$(tag_for "$component" "$version")
  temporary_dir=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/peppy-release-component-state.XXXXXX")
  gpg_private_home=$(mktemp -d "${TMPDIR:-/tmp}/p-gpg.XXXXXX")
  gpg_verify_home=$(mktemp -d "${TMPDIR:-/tmp}/p-gpgv.XXXXXX")
  expected_json=$(expected_assets "$component" "$version" | jq -R . | jq -cs 'sort')
  hashed_json=$(hashed_assets "$component" "$version" | jq -R . | jq -cs 'sort')
  verify_remote_tag "$tag" "$sha"
  verify_release_metadata "$tag" "$expected_json"
  expected_assets "$component" "$version" | download_expected_assets "$tag" "$temporary_dir"
  verify_checksums "$temporary_dir" "$hashed_json" "$gpg_private_home" "$gpg_verify_home" "$temporary_dir/selected-public-key.asc"
  case "$component" in
    desktop)
      [ -n "${PEPPY_UPDATE_PUBLIC_KEY:-}" ] || fail 'updater verification authority is unavailable'
      env -u PEPPY_RELEASE_GPG_KEY -u PEPPY_RELEASE_GPG_KEY_ID "$repo_root/infra/release/verify-update-assets.sh" "$version" stable "$temporary_dir"
      ;;
    android)
      env -u PEPPY_RELEASE_GPG_KEY -u PEPPY_RELEASE_GPG_KEY_ID "$repo_root/infra/release/verify-android-artifacts.sh" "$temporary_dir/peppy-$version-android.apk" "$temporary_dir/peppy-$version-android.aab"
      ;;
  esac
)

[ "$#" -ge 1 ] || fail 'usage: release-component-state.sh <discover|verify> ...' 2
case "$1" in
  discover) shift; discover "$@" ;;
  verify) shift; verify "$@" ;;
  *) fail 'usage: release-component-state.sh <discover|verify> ...' 2 ;;
esac
