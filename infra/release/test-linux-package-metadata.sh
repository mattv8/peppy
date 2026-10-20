#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
command -v python3 >/dev/null || { echo 'python3 with tomllib is required' >&2; exit 1; }
python3 -c 'import tomllib' || { echo 'python3 with tomllib is required' >&2; exit 1; }
python3 - "$script_dir/../../apps/desktop/src-tauri/Cargo.toml" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as manifest_file:
    manifest = tomllib.load(manifest_file)

assert manifest["package"]["license"] == "Apache-2.0"
PY

assert_missing_rpm_license_is_rejected() {
  local license=$1 root bundle_dir tool_dir status output
  root=$(mktemp -d)
  bundle_dir="$root/bundle"
  tool_dir="$root/bin"
  mkdir -p "$bundle_dir/deb" "$bundle_dir/rpm" "$tool_dir"
  : > "$bundle_dir/deb/fixture.deb"
  : > "$bundle_dir/rpm/fixture.rpm"
  cat > "$tool_dir/dpkg-deb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = '-R' ]; then
  mkdir -p "$3/DEBIAN"
  printf 'Version: 0\n' > "$3/DEBIAN/control"
fi
exit 0
EOF
  cat > "$tool_dir/rpm" <<EOF
#!/usr/bin/env bash
if [ "\$3" = '%{LICENSE}' ]; then
  printf '%s' '$license'
fi
EOF
  cat > "$tool_dir/rpmrebuild" <<EOF
#!/usr/bin/env bash
touch '$root/rpmrebuild-invoked'
EOF
  chmod +x "$tool_dir/dpkg-deb" "$tool_dir/rpm" "$tool_dir/rpmrebuild"
  set +e
  output=$(PATH="$tool_dir:$PATH" "$script_dir/normalize-linux-package-metadata.sh" 1.2.3 "$bundle_dir" 2>&1)
  status=$?
  set -e
  test "$status" -ne 0
  printf '%s\n' "$output" | grep -Fq 'RPM License metadata is empty; set apps/desktop/src-tauri/Cargo.toml package.license or tauri.conf.json bundle.license before trusted packaging'
  test ! -e "$root/rpmrebuild-invoked"
  rm -rf "$root"
}

assert_missing_rpm_license_is_rejected ''
assert_missing_rpm_license_is_rejected '   '
assert_missing_rpm_license_is_rejected '(none)'

assert_rebuilt_rpm_license_is_preserved() {
  local root bundle_dir tool_dir status output
  root=$(mktemp -d)
  bundle_dir="$root/bundle"
  tool_dir="$root/bin"
  mkdir -p "$bundle_dir/deb" "$bundle_dir/rpm" "$tool_dir"
  : > "$bundle_dir/deb/fixture.deb"
  : > "$bundle_dir/rpm/fixture.rpm"
  cat > "$tool_dir/dpkg-deb" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = '-R' ]; then
  mkdir -p "$3/DEBIAN"
  printf 'Version: 0\n' > "$3/DEBIAN/control"
fi
exit 0
EOF
  cat > "$tool_dir/rpm" <<'EOF'
#!/usr/bin/env bash
case "$3" in
  '%{NAME}') printf 'fixture' ;;
  '%{ARCH}') printf 'noarch' ;;
  '%{LICENSE}')
    case "$4" in
      *rebuilt.rpm) printf 'GPL-3.0-only' ;;
      *) printf 'MIT' ;;
    esac
    ;;
  '%{NAME}|%{VERSION}|%{RELEASE}|%{ARCH}') printf 'fixture|1.2.3|1|noarch' ;;
esac
EOF
  cat > "$tool_dir/rpmrebuild" <<'EOF'
#!/usr/bin/env bash
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--directory' ]; then
    touch "$2/rebuilt.rpm"
    exit 0
  fi
  shift
done
exit 1
EOF
  chmod +x "$tool_dir/dpkg-deb" "$tool_dir/rpm" "$tool_dir/rpmrebuild"
  set +e
  output=$(PATH="$tool_dir:$PATH" "$script_dir/normalize-linux-package-metadata.sh" 1.2.3 "$bundle_dir" 2>&1)
  status=$?
  set -e
  test "$status" -ne 0
  printf '%s\n' "$output" | grep -Fq 'rebuilt RPM metadata mismatch: fixture|1.2.3|1|noarch|GPL-3.0-only'
  rm -rf "$root"
}

assert_rebuilt_rpm_license_is_preserved

for command in cpio curl dpkg-deb file gpg rpm rpm2cpio rpmbuild rpmsign tar; do
  command -v "$command" >/dev/null || { echo "$command is required" >&2; exit 1; }
done

root=$(mktemp -d)
gpg_home=''
cleanup() {
  if [ -n "$gpg_home" ]; then GNUPGHOME="$gpg_home" gpgconf --kill gpg-agent || true; fi
  rm -rf "$root"
}
trap cleanup EXIT
bundle_dir="$root/bundle"
deb_root="$root/deb-root"
rpm_topdir="$root/rpmbuild"
tool_dir="$root/bin"

rebuild_fixture_packages() {
  rm -f "$bundle_dir/deb"/*.deb "$bundle_dir/rpm"/*.rpm
  dpkg-deb --build "$deb_root" "$bundle_dir/deb/peppy-package-metadata-fixture.deb" >/dev/null
  cp "$rpm_topdir/RPMS/noarch/peppy-package-metadata-fixture-9.8.7-7.noarch.rpm" "$bundle_dir/rpm/"
}

assert_packages() {
  local expected_version=$1 deb rpm extracted
  deb=$(echo "$bundle_dir/deb"/*.deb)
  rpm=$(echo "$bundle_dir/rpm"/*.rpm)
  test "$(dpkg-deb -f "$deb" Version)" = "$expected_version"
  dpkg-deb -c "$deb" | awk 'NF >= 2 && $2 != "root/root" { exit 1 }'
  extracted="$root/deb-extracted-$expected_version"
  dpkg-deb -x "$deb" "$extracted"
  test "$(stat -c '%a' "$extracted/usr/bin/peppy-package-metadata-fixture")" = 755
  test "$(cat "$extracted/usr/bin/peppy-package-metadata-fixture")" = 'fixture payload'
  dpkg-deb -e "$deb" "$extracted/control"
  test -x "$extracted/control/postinst"

  test "$(rpm -qp --qf '%{NAME}|%{VERSION}|%{RELEASE}|%{ARCH}|%{LICENSE}' "$rpm")" = "peppy-package-metadata-fixture|$expected_version|1|noarch|MIT"
  rpm -qp --scripts "$rpm" | grep -Fq "/bin/sh -c 'true'"
  rpm -qp --qf '[%{FILEMODES:perms}|%{FILEUSERNAME}|%{FILEGROUPNAME}|%{FILENAMES}\n]' "$rpm" | grep -Fxq -- '-rwxr-xr-x|root|root|/usr/bin/peppy-package-metadata-fixture'
  extracted="$root/rpm-extracted-$expected_version"
  mkdir "$extracted"
  (cd "$extracted" && rpm2cpio "$rpm" | cpio -idmu >/dev/null)
  test "$(stat -c '%a' "$extracted/usr/bin/peppy-package-metadata-fixture")" = 755
  test "$(cat "$extracted/usr/bin/peppy-package-metadata-fixture")" = 'fixture payload'
}

mkdir -p "$bundle_dir/deb" "$bundle_dir/rpm" "$deb_root/DEBIAN" "$deb_root/usr/bin" "$rpm_topdir/SPECS"

cat > "$deb_root/DEBIAN/control" <<'EOF'
Package: peppy-package-metadata-fixture
Version: 9.8.7
Architecture: all
Maintainer: Peppy Fixture <fixture@example.invalid>
Description: package metadata fixture
EOF
cat > "$deb_root/DEBIAN/postinst" <<'EOF'
#!/bin/sh
exit 0
EOF
printf 'fixture payload\n' > "$deb_root/usr/bin/peppy-package-metadata-fixture"
chmod 0755 "$deb_root/DEBIAN/postinst" "$deb_root/usr/bin/peppy-package-metadata-fixture"
dpkg-deb --build "$deb_root" "$bundle_dir/deb/peppy-package-metadata-fixture.deb" >/dev/null
if ((EUID != 0)); then
  dpkg-deb -c "$bundle_dir/deb/peppy-package-metadata-fixture.deb" | awk '$NF == "./usr/bin/peppy-package-metadata-fixture" { found=1; root_owned=($2 == "root/root") } END { exit !found || root_owned }'
fi

cat > "$rpm_topdir/SPECS/fixture.spec" <<'EOF'
Name: peppy-package-metadata-fixture
Version: 9.8.7
Release: 7
Summary: Package metadata fixture
License: MIT
BuildArch: noarch

%description
Package metadata fixture.

%install
install -Dpm 0755 /dev/stdin %{buildroot}/usr/bin/peppy-package-metadata-fixture <<'PAYLOAD'
fixture payload
PAYLOAD

%post
/bin/sh -c 'true'

%files
/usr/bin/peppy-package-metadata-fixture
EOF
rpmbuild --define "_topdir $rpm_topdir" -bb "$rpm_topdir/SPECS/fixture.spec" >/dev/null
cp "$rpm_topdir/RPMS/noarch/peppy-package-metadata-fixture-9.8.7-7.noarch.rpm" "$bundle_dir/rpm/"

"$script_dir/install-rpmrebuild.sh" "$tool_dir"
PATH="$tool_dir:$PATH" "$script_dir/normalize-linux-package-metadata.sh" 1.2.3 "$bundle_dir"
assert_packages 1.2.3

rebuild_fixture_packages
PATH="$tool_dir:$PATH" "$script_dir/normalize-linux-package-metadata.sh" 1.2.3-staging.4 "$bundle_dir"
assert_packages 1.2.3~staging.4

gpg_home="$root/gpg"
mkdir -m 0700 "$gpg_home"
export GNUPGHOME=$gpg_home
gpg --batch --passphrase '' --quick-generate-key 'Peppy fixture <fixture@example.invalid>' rsa2048 sign 0
key_id=$(gpg --batch --with-colons --list-secret-keys | awk -F: '$1 == "fpr" { print $10; exit }')
export PEPPY_RELEASE_RPM_GPG_KEY PEPPY_RELEASE_RPM_GPG_KEY_ID
PEPPY_RELEASE_RPM_GPG_KEY=$(gpg --batch --armor --export-secret-keys "$key_id")
PEPPY_RELEASE_RPM_GPG_KEY_ID=$key_id
"$script_dir/sign-rpm.sh" "$bundle_dir/rpm"
public_key="$root/fixture-public-key.asc"
gpg --batch --armor --export "$key_id" > "$public_key"
rpm_db="$root/rpm-db"
mkdir "$rpm_db"
rpm --dbpath "$rpm_db" --import "$public_key"
rpm --dbpath "$rpm_db" -Kv "$bundle_dir/rpm"/*.rpm | grep -Eiq '(RSA|DSA).*: OK'

printf 'Linux package metadata integration tests passed\n'
