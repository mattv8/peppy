#!/usr/bin/env bash
set -euo pipefail
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=infra/release/lib.sh
source "${PEPPY_RELEASE_LIB:-$script_dir/lib.sh}"

[ "$#" -eq 2 ] || { echo "usage: $0 <canonical-version> <bundle-dir>" >&2; exit 2; }
canonical_version=$1
bundle_dir=$2
package_version=$(release_linux_package_version "$canonical_version") || { echo "invalid canonical version: $canonical_version" >&2; exit 2; }
rpm_release=$(release_linux_rpm_release "$canonical_version")
work_dir=''
output_dir=''
trap 'rm -rf "${work_dir:-}" "${output_dir:-}"' EXIT

for deb in "$bundle_dir"/deb/*.deb; do
  [ -e "$deb" ] || { echo 'required deb bundle missing' >&2; exit 1; }
  work_dir=$(mktemp -d)
  dpkg-deb -R "$deb" "$work_dir/package"
  sed -i.bak -E "s/^Version: .*/Version: $package_version/" "$work_dir/package/DEBIAN/control"
  rm -f "$work_dir/package/DEBIAN/control.bak"
  dpkg-deb --root-owner-group --build "$work_dir/package" "$deb"
  dpkg-deb -c "$deb" | awk 'NF >= 2 && $2 != "root/root" { exit 1 }' || { echo "rebuilt DEB contains non-root-owned members: $deb" >&2; exit 1; }
  rm -rf "$work_dir"
  work_dir=''
done

for rpm in "$bundle_dir"/rpm/*.rpm; do
  [ -e "$rpm" ] || { echo 'required rpm bundle missing' >&2; exit 1; }
  command -v rpmrebuild >/dev/null || { echo 'rpmrebuild is required to normalize RPM metadata' >&2; exit 1; }
  original_name=$(rpm -qp --qf '%{NAME}' "$rpm")
  original_arch=$(rpm -qp --qf '%{ARCH}' "$rpm")
  output_dir=$(mktemp -d)
  rpmrebuild --package --notest --directory "$output_dir" --change-spec-preamble="sed -e 's/^Version:.*/Version: $package_version/' -e 's/^Release:.*/Release: $rpm_release/'" "$rpm"
  mapfile -t rebuilt < <(find "$output_dir" -type f -name '*.rpm' -print)
  [ "${#rebuilt[@]}" -eq 1 ] || { echo "rpmrebuild must produce exactly one RPM; found ${#rebuilt[@]}" >&2; exit 1; }
  rebuilt_metadata=$(rpm -qp --qf '%{NAME}|%{VERSION}|%{RELEASE}|%{ARCH}' "${rebuilt[0]}")
  expected_metadata="$original_name|$package_version|$rpm_release|$original_arch"
  [ "$rebuilt_metadata" = "$expected_metadata" ] || { echo "rebuilt RPM metadata mismatch: $rebuilt_metadata" >&2; exit 1; }
  mv -f "${rebuilt[0]}" "$rpm"
  rm -rf "$output_dir"
  output_dir=''
done
