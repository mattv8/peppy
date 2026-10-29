#!/usr/bin/env bash
set -euo pipefail

# Generate an offline repository from final, signed release assets. This script
# does not publish assets, create keys, or run with elevated privileges.
usage() { echo "usage: $0 <stable|staging> <version> <staged-release-dir> <out-dir>" >&2; exit 2; }
fail() { echo "$*" >&2; exit 1; }
require_one_file() {
  local directory=$1 filename=$2 matches
  matches=$(find "$directory" -maxdepth 1 -type f -name "$filename" -print)
  [ "$(printf '%s\n' "$matches" | sed '/^$/d' | wc -l | tr -d ' ')" = 1 ] || fail "expected exactly one $filename"
  printf '%s\n' "$matches"
}
require_checksum() {
  local file=$1 expected
  expected="$(sha256sum "$file" | awk '{print $1}')  $(basename "$file")"
  grep -Fqx -- "$expected" "$staged_dir/SHA256SUMS" || fail "missing exact checksum for $(basename "$file")"
}
package_version() {
  case "$version" in
    *-staging.*) printf '%s~staging.%s\n' "${version%-staging.*}" "${version##*.}" ;;
    *) printf '%s\n' "$version" ;;
  esac
}
validate_package_metadata() {
  local expected_version
  expected_version=$(package_version)
  [ "$(dpkg-deb -f "$deb" Package)" = peppy ] || fail 'deb package name is not peppy'
  [ "$(dpkg-deb -f "$deb" Version)" = "$expected_version" ] || fail 'deb package version does not match release version'
  [ "$(dpkg-deb -f "$deb" Architecture)" = amd64 ] || fail 'deb package architecture is not amd64'
  [ "$(rpm -qp --qf '%{NAME}\n' "$rpm_file")" = peppy ] || fail 'rpm package name is not peppy'
  [ "$(rpm -qp --qf '%{VERSION}\n' "$rpm_file")" = "$expected_version" ] || fail 'rpm package version does not match release version'
  [ "$(rpm -qp --qf '%{RELEASE}\n' "$rpm_file")" = 1 ] || fail 'rpm package release is not 1'
  [ "$(rpm -qp --qf '%{ARCH}\n' "$rpm_file")" = x86_64 ] || fail 'rpm package architecture is not x86_64'
}
validate_output_directory() {
  local parent base staged_real out_real
  parent=$(dirname "$out_dir"); base=$(basename "$out_dir")
  if [ -z "$base" ] || [ "$base" = . ] || [ "$base" = / ]; then
    fail 'unsafe repository output directory'
  fi
  [ -d "$parent" ] || fail 'repository output parent directory not found'
  [ ! -e "$out_dir" ] || fail 'repository output directory must not already exist'
  staged_real=$(cd "$staged_dir" && pwd -P); out_real=$(cd "$parent" && pwd -P)/$base
  case "$out_real" in "$staged_real"|"$staged_real"/*) fail 'repository output cannot be inside staged release directory' ;; esac
  case "$staged_real" in "$out_real"|"$out_real"/*) fail 'repository output cannot contain staged release directory' ;; esac
}

[ "$#" -eq 4 ] || usage
channel=$1; version=$2; staged_dir=$3; out_dir=$4
case "$channel" in stable) [[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] ;; staging) [[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-staging\.(0|[1-9][0-9]*)$ ]] ;; *) usage ;; esac || { echo 'version does not match channel' >&2; exit 2; }
[ -d "$staged_dir" ] || fail 'staged release directory not found'
[ -n "${PEPPY_RELEASE_VERIFY_KEYRING:-}" ] || fail 'PEPPY_RELEASE_VERIFY_KEYRING is required'
[ -n "${PEPPY_RPM_VERIFY_KEYRING:-}" ] || fail 'PEPPY_RPM_VERIFY_KEYRING is required'
[ -n "${PEPPY_REPOSITORY_SIGNING_KEY:-}" ] || fail 'PEPPY_REPOSITORY_SIGNING_KEY is required'
[ -f "$PEPPY_RELEASE_VERIFY_KEYRING" ] || fail 'release verification keyring not found'
[ -f "$PEPPY_RPM_VERIFY_KEYRING" ] || fail 'RPM verification keyring not found'
command -v gpgv >/dev/null; command -v gpg >/dev/null; command -v apt-ftparchive >/dev/null; command -v createrepo_c >/dev/null; command -v rpm >/dev/null; command -v dpkg-deb >/dev/null
validate_output_directory

deb=$(require_one_file "$staged_dir" "peppy-$version-linux-x86_64.deb")
rpm_file=$(require_one_file "$staged_dir" "peppy-$version-linux-x86_64.rpm")
if [ ! -f "$staged_dir/SHA256SUMS" ] || [ ! -f "$staged_dir/SHA256SUMS.sig" ]; then
  fail 'signed staged SHA256SUMS is required'
fi
gpgv --keyring "$PEPPY_RELEASE_VERIFY_KEYRING" "$staged_dir/SHA256SUMS.sig" "$staged_dir/SHA256SUMS"
(cd "$staged_dir" && sha256sum --check --strict SHA256SUMS)
require_checksum "$deb"; require_checksum "$rpm_file"
validate_package_metadata

temporary_out=''
rpm_db=$(mktemp -d); trap 'rm -rf "$rpm_db" "$temporary_out"' EXIT
rpm --dbpath "$rpm_db" --initdb
rpm --dbpath "$rpm_db" --import "$PEPPY_RPM_VERIFY_KEYRING"
rpm_signature=$(rpm --dbpath "$rpm_db" --checksig "$rpm_file") || fail 'RPM signature is not trusted by the dedicated RPM keyring'
grep -Eq '(^|[[:space:]])signatures? OK([[:space:]]|$)' <<<"$rpm_signature" || fail 'RPM is unsigned or its signature is not trusted by the dedicated RPM keyring'

temporary_out=$(mktemp -d "$(dirname "$out_dir")/.peppy-repository.XXXXXX")
mkdir -p "$temporary_out/apt/pool/$channel" "$temporary_out/rpm/$channel"
cp "$deb" "$temporary_out/apt/pool/$channel/"; cp "$rpm_file" "$temporary_out/rpm/$channel/"
mkdir -p "$temporary_out/apt/dists/$channel/main/binary-amd64"
(cd "$temporary_out/apt"
  apt-ftparchive packages "pool/$channel" > "dists/$channel/main/binary-amd64/Packages"
  gzip -9c "dists/$channel/main/binary-amd64/Packages" > "dists/$channel/main/binary-amd64/Packages.gz"
  apt-ftparchive release "dists/$channel" > "dists/$channel/Release.generated"
  { printf 'Origin: Peppy\nLabel: Peppy\nSuite: %s\nCodename: %s\nArchitectures: amd64\nComponents: main\nValid-Until: %s\n' "$channel" "$channel" "$(date -u -v+14d '+%a, %d %b %Y %H:%M:%S UTC' 2>/dev/null || date -u -d '+14 days' '+%a, %d %b %Y %H:%M:%S UTC')"; cat "dists/$channel/Release.generated"; } > "dists/$channel/Release"
  rm "dists/$channel/Release.generated"
  gpg --batch --yes --local-user "$PEPPY_REPOSITORY_SIGNING_KEY" --armor --detach-sign --output "dists/$channel/Release.gpg" "dists/$channel/Release"
  gpg --batch --yes --local-user "$PEPPY_REPOSITORY_SIGNING_KEY" --clearsign --output "dists/$channel/InRelease" "dists/$channel/Release"
)
createrepo_c --no-database "$temporary_out/rpm/$channel"
gpg --batch --yes --local-user "$PEPPY_REPOSITORY_SIGNING_KEY" --armor --detach-sign --output "$temporary_out/rpm/$channel/repodata/repomd.xml.asc" "$temporary_out/rpm/$channel/repodata/repomd.xml"
mv "$temporary_out" "$out_dir"; temporary_out=''; trap - EXIT; rm -rf "$rpm_db"
echo "repository generated for $channel $version"
