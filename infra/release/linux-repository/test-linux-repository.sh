#!/usr/bin/env bash
set -euo pipefail
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
setup=(bash "$script_dir/setup-peppy-updates.sh")
build=(bash "$script_dir/build-repository.sh")
root=$(mktemp -d); trap 'rm -rf "$root"' EXIT

expect_failure() { if "$@" >/dev/null 2>&1; then echo "expected failure: $*" >&2; exit 1; fi; }
setup_args=(--distro debian --channel stable --repo-url https://packages.example.invalid/peppy --key-url https://packages.example.invalid/peppy/key.asc --key-fingerprint 0123456789abcdef0123456789abcdef01234567 --dry-run)
dry_run=$("${setup[@]}" "${setup_args[@]}")
grep -Fqx 'Pin: origin packages.example.invalid' <<<"$dry_run"
grep -Fq 'Pin-Priority: -1' <<<"$dry_run"
grep -Fq 'Pin-Priority: 990' <<<"$dry_run"
grep -Fq 'Wants=network-online.target' <<<"$dry_run"
grep -Fq 'install --only-upgrade --yes peppy' <<<"$dry_run"
if grep -Eq 'unattended-upgrades|dnf-automatic|sh -c' <<<"$dry_run"; then exit 1; fi
port_dry_run=$("${setup[@]}" --distro debian --channel stable --repo-url https://packages.example.invalid:8443/peppy --key-url https://packages.example.invalid:8443/peppy/key.asc --key-fingerprint 0123456789abcdef0123456789abcdef01234567 --dry-run)
grep -Fqx 'Pin: origin packages.example.invalid' <<<"$port_dry_run"
if grep -Fqx 'Pin: origin packages.example.invalid:8443' <<<"$port_dry_run"; then exit 1; fi
fedora_dry_run=$("${setup[@]}" --distro fedora --channel staging --repo-url https://packages.example.invalid/peppy --key-url https://packages.example.invalid/peppy/key.asc --key-fingerprint 0123456789abcdef0123456789abcdef01234567 --dry-run)
grep -Fq 'includepkgs=peppy' <<<"$fedora_dry_run"
grep -Fq 'dnf --assumeyes upgrade peppy' <<<"$fedora_dry_run"
expect_failure "${setup[@]}" --distro debian --channel stable --repo-url http://invalid --key-url https://invalid --key-fingerprint 0123456789abcdef0123456789abcdef01234567 --dry-run
expect_failure "${setup[@]}" --distro debian --channel stable --repo-url https://one.invalid --key-url https://two.invalid/key --key-fingerprint 0123456789abcdef0123456789abcdef01234567 --dry-run
expect_failure "${setup[@]}" --distro debian --channel stable --repo-url "https://valid.invalid/\$(bad)" --key-url https://valid.invalid/key --key-fingerprint 0123456789abcdef0123456789abcdef01234567 --dry-run
expect_failure "${setup[@]}" --distro debian --channel stable --repo-url https://valid.invalid --key-url https://valid.invalid/key --key-fingerprint short --dry-run

setup_fixture="$root/setup-peppy-updates.sh"
sed "s|/etc/|$root/etc/|g" "$script_dir/setup-peppy-updates.sh" > "$setup_fixture"
chmod +x "$setup_fixture"
mkdir -p "$root/bin"
cat > "$root/bin/id" <<'EOF'
#!/usr/bin/env bash
printf '0\n'
EOF
cat > "$root/bin/curl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$PEPPY_TEST_CURL_LOG"
url=''
while [ "$#" -gt 0 ]; do
  case "$1" in https://*) url=$1;; -o) case "$url" in *fetch-fail.asc) exit 1;; *bad-rpm-key.asc) printf bad-rpm-key > "$2";; *zero-primary.asc) printf zero-primary > "$2";; *multiple-primary.asc) printf multiple-primary > "$2";; *gpg-fail.asc) printf gpg-fail > "$2";; *rpm-key.asc) printf rpm-key > "$2";; *) printf key > "$2";; esac; exit 0;; esac
  shift
done
exit 1
EOF
cat > "$root/bin/gpg" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$PEPPY_TEST_GPG_LOG"
case "$(cat "${!#}")" in
  zero-primary) exit 0;;
  multiple-primary) printf 'pub:::::::::\nfpr:::::::::0123456789ABCDEF0123456789ABCDEF01234567:\npub:::::::::\nfpr:::::::::89ABCDEF0123456789ABCDEF0123456789ABCDEF:\n'; exit 0;;
  gpg-fail) exit 1;;
  rpm-key) fingerprint=89ABCDEF0123456789ABCDEF0123456789ABCDEF;;
  bad-rpm-key) fingerprint=FEDCBA9876543210FEDCBA9876543210FEDCBA98;;
  *) fingerprint=0123456789ABCDEF0123456789ABCDEF01234567;;
esac
printf 'pub:::::::::\nfpr:::::::::%s:\n' "$fingerprint"
EOF
cat > "$root/bin/install" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = -d ]; then mkdir -p "${@: -1}"; else cp "${@: -2:1}" "${@: -1}"; fi
EOF
cat > "$root/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$PEPPY_TEST_SYSTEMCTL_LOG"
EOF
chmod +x "$root/bin"/{id,curl,gpg,install,systemctl}
reset_setup_root() {
  rm -rf "${root:?}/etc" "${root:?}/systemctl.log" "${root:?}/curl.log" "${root:?}/gpg.log" "${root:?}/dangling-target"
  mkdir -p "$root/etc/apt/keyrings" "$root/etc/apt/sources.list.d" "$root/etc/apt/preferences.d" "$root/etc/pki/rpm-gpg" "$root/etc/yum.repos.d" "$root/etc/systemd/system"
}
run_setup() {
  PATH="$root/bin:$PATH" PEPPY_TEST_SYSTEMCTL_LOG="$root/systemctl.log" PEPPY_TEST_CURL_LOG="$root/curl.log" PEPPY_TEST_GPG_LOG="$root/gpg.log" "$setup_fixture" "$@"
}
setup_common=(--channel stable --repo-url https://packages.example.invalid/peppy --key-url https://packages.example.invalid/peppy/key.asc --key-fingerprint 0123456789abcdef0123456789abcdef01234567)
assert_no_setup_effects() {
  test ! -e "$root/etc/apt/keyrings/peppy.asc"
  test ! -e "$root/etc/apt/sources.list.d/peppy.list"
  test ! -e "$root/etc/apt/preferences.d/peppy"
  test ! -e "$root/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy"
  test ! -e "$root/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy-rpm"
  test ! -e "$root/etc/yum.repos.d/peppy.repo"
  test ! -e "$root/etc/systemd/system/peppy-update.service"
  test ! -e "$root/etc/systemd/system/peppy-update.timer"
  test ! -e "$root/systemctl.log"
}
assert_no_argument_fetches() {
  test ! -e "$root/curl.log"
  test ! -e "$root/gpg.log"
}
expect_argument_rejection() {
  reset_setup_root
  if run_setup "$@" >/dev/null 2>&1; then
    echo "expected argument rejection: $*" >&2
    exit 1
  fi
  assert_no_setup_effects
  assert_no_argument_fetches
}
expect_key_rejection() {
  reset_setup_root
  if run_setup "$@" >/dev/null 2>&1; then
    echo "expected key rejection: $*" >&2
    exit 1
  fi
  assert_no_setup_effects
}
rpm_fingerprint=89abcdef0123456789abcdef0123456789abcdef
rpm_key_pair=(--rpm-key-url https://packages.example.invalid/peppy/rpm-key.asc --rpm-key-fingerprint "$rpm_fingerprint")

reset_setup_root
if run_setup --distro fedora "${setup_common[@]}" --rpm-key-url '' --rpm-key-fingerprint '' >/dev/null 2>&1; then
  echo 'expected explicitly empty Fedora RPM key options to be rejected' >&2
  exit 1
fi
assert_no_setup_effects
assert_no_argument_fetches

expect_argument_rejection --distro fedora "${setup_common[@]}" --rpm-key-url '' --rpm-key-fingerprint "$rpm_fingerprint"
expect_argument_rejection --distro fedora "${setup_common[@]}" --rpm-key-url https://packages.example.invalid/peppy/rpm-key.asc --rpm-key-fingerprint ''
expect_argument_rejection --distro fedora "${setup_common[@]}" --rpm-key-url https://packages.example.invalid/peppy/rpm-key.asc
expect_argument_rejection --distro fedora "${setup_common[@]}" --rpm-key-fingerprint "$rpm_fingerprint"
expect_argument_rejection --distro fedora "${setup_common[@]}" --rpm-key-url
expect_argument_rejection --distro fedora "${setup_common[@]}" --rpm-key-fingerprint
expect_argument_rejection --distro fedora "${setup_common[@]}" --rpm-key-url --dry-run
expect_argument_rejection --distro fedora "${setup_common[@]}" --rpm-key-fingerprint --dry-run
expect_argument_rejection --distro debian "${setup_common[@]}" --rpm-key-url '' --rpm-key-fingerprint ''

reset_setup_root
run_setup --distro debian "${setup_common[@]}"
test -f "$root/etc/apt/keyrings/peppy.asc"
test -f "$root/etc/apt/sources.list.d/peppy.list"
test -f "$root/etc/apt/preferences.d/peppy"
test -f "$root/etc/systemd/system/peppy-update.service"
test -f "$root/etc/systemd/system/peppy-update.timer"

reset_setup_root
ln -s "$root/dangling-target" "$root/etc/systemd/system/peppy-update.timer"
atomic_output="$root/debian-atomic-refusal.log"
if run_setup --distro debian "${setup_common[@]}" >"$atomic_output" 2>&1; then
  echo 'expected Debian setup to reject an occupied late destination before writing configuration' >&2
  exit 1
fi
grep -Fqx "refusing to replace existing $root/etc/systemd/system/peppy-update.timer" "$atomic_output"
test -L "$root/etc/systemd/system/peppy-update.timer"
test ! -e "$root/dangling-target"
test ! -e "$root/etc/apt/keyrings/peppy.asc"
test ! -e "$root/etc/apt/sources.list.d/peppy.list"
test ! -e "$root/etc/apt/preferences.d/peppy"
test ! -e "$root/etc/systemd/system/peppy-update.service"
test ! -e "$root/systemctl.log"

reset_setup_root
run_setup --distro fedora "${setup_common[@]}"
test -f "$root/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy"
test -f "$root/etc/yum.repos.d/peppy.repo"
test -f "$root/etc/systemd/system/peppy-update.service"
test -f "$root/etc/systemd/system/peppy-update.timer"

reset_setup_root
run_setup --distro fedora "${setup_common[@]}" "${rpm_key_pair[@]}"
test -f "$root/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy"
test -f "$root/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy-rpm"
grep -Fqx key "$root/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy"
grep -Fqx rpm-key "$root/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy-rpm"
grep -Fqx "gpgkey=file://$root/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy file://$root/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy-rpm" "$root/etc/yum.repos.d/peppy.repo"

for rpm_args in \
  "--rpm-key-url https://packages.example.invalid/peppy/rpm-key.asc" \
  "--rpm-key-fingerprint $rpm_fingerprint" \
  "--rpm-key-url http://packages.example.invalid/peppy/rpm-key.asc --rpm-key-fingerprint $rpm_fingerprint" \
  "--rpm-key-url https://other.invalid/peppy/rpm-key.asc --rpm-key-fingerprint $rpm_fingerprint" \
  "--rpm-key-url https://packages.example.invalid/peppy/bad-rpm-key.asc --rpm-key-fingerprint $rpm_fingerprint"; do
  reset_setup_root
  # shellcheck disable=SC2086 # Each $rpm_args entry is a deliberate "--flag value" pair that must word-split into arguments.
  if run_setup --distro fedora "${setup_common[@]}" $rpm_args >/dev/null 2>&1; then
    echo "expected Fedora RPM key validation failure: $rpm_args" >&2
    exit 1
  fi
  assert_no_setup_effects
done
for key_url in zero-primary.asc multiple-primary.asc gpg-fail.asc fetch-fail.asc; do
  expect_key_rejection --distro fedora "${setup_common[@]}" --key-url "https://packages.example.invalid/peppy/$key_url" "${rpm_key_pair[@]}"
  expect_key_rejection --distro fedora "${setup_common[@]}" --rpm-key-url "https://packages.example.invalid/peppy/$key_url" --rpm-key-fingerprint "$rpm_fingerprint"
done
reset_setup_root
if run_setup --distro fedora "${setup_common[@]}" --key-fingerprint AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA --rpm-key-url https://packages.example.invalid/peppy/rpm-key.asc --rpm-key-fingerprint "$rpm_fingerprint" >/dev/null 2>&1; then
  echo 'expected Fedora metadata key validation failure before configuration writes' >&2
  exit 1
fi
assert_no_setup_effects
expect_failure "${setup[@]}" --distro debian "${setup_common[@]}" --rpm-key-url https://packages.example.invalid/peppy/rpm-key.asc --rpm-key-fingerprint "$rpm_fingerprint" --dry-run

reset_setup_root
ln -s "$root/dangling-target" "$root/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy-rpm"
atomic_output="$root/fedora-rpm-key-atomic-refusal.log"
if run_setup --distro fedora "${setup_common[@]}" --rpm-key-url https://packages.example.invalid/peppy/rpm-key.asc --rpm-key-fingerprint "$rpm_fingerprint" >"$atomic_output" 2>&1; then
  echo 'expected Fedora setup to reject an occupied RPM key destination before writing configuration' >&2
  exit 1
fi
grep -Fqx "refusing to replace existing $root/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy-rpm" "$atomic_output"
test -L "$root/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy-rpm"
test ! -e "$root/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy"
test ! -e "$root/etc/yum.repos.d/peppy.repo"
test ! -e "$root/etc/systemd/system/peppy-update.timer"

reset_setup_root
ln -s "$root/dangling-target" "$root/etc/systemd/system/peppy-update.timer"
atomic_output="$root/fedora-atomic-refusal.log"
if run_setup --distro fedora "${setup_common[@]}" >"$atomic_output" 2>&1; then
  echo 'expected Fedora setup to reject an occupied late destination before writing configuration' >&2
  exit 1
fi
grep -Fqx "refusing to replace existing $root/etc/systemd/system/peppy-update.timer" "$atomic_output"
test -L "$root/etc/systemd/system/peppy-update.timer"
test ! -e "$root/dangling-target"
test ! -e "$root/etc/pki/rpm-gpg/RPM-GPG-KEY-peppy"
test ! -e "$root/etc/yum.repos.d/peppy.repo"
test ! -e "$root/etc/systemd/system/peppy-update.service"
test ! -e "$root/systemctl.log"

mkdir -p "$root/bin" "$root/staged"
make_fixture() {
  rm -f "$root/staged"/*
  printf deb > "$root/staged/peppy-1.2.3-linux-x86_64.deb"
  printf rpm > "$root/staged/peppy-1.2.3-linux-x86_64.rpm"
  sha256sum "$root/staged/peppy-1.2.3-linux-x86_64.deb" "$root/staged/peppy-1.2.3-linux-x86_64.rpm" | sed "s#$root/staged/##" > "$root/staged/SHA256SUMS"
  printf signature > "$root/staged/SHA256SUMS.sig"; printf key > "$root/keyring"; printf rpm-key > "$root/rpm-key"
}
cat > "$root/bin/gpgv" <<'EOF'
#!/usr/bin/env bash
[ "${FAIL_GPGV:-0}" = 0 ]
EOF
cat > "$root/bin/rpm" <<'EOF'
#!/usr/bin/env bash
case " $* " in
  *' -qp '*'%{NAME}'*) printf '%s\n' "${RPM_NAME:-peppy}";;
  *' -qp '*'%{VERSION}'*) printf '%s\n' "${RPM_VERSION:-1.2.3}";;
  *' -qp '*'%{RELEASE}'*) printf '%s\n' "${RPM_RELEASE:-1}";;
  *' -qp '*'%{ARCH}'*) printf '%s\n' "${RPM_ARCH:-x86_64}";;
  *' --checksig '*) [ "${FAIL_RPM_SIGNATURE:-0}" = 0 ] || exit 1; printf '%s\n' "${RPM_CHECKSIG_OUTPUT:-package.rpm: digests signatures OK}";;
  *) exit 0;;
esac
EOF
cat > "$root/bin/dpkg-deb" <<'EOF'
#!/usr/bin/env bash
case "$3" in Package) printf '%s\n' "${DEB_NAME:-peppy}";; Version) printf '%s\n' "${DEB_VERSION:-1.2.3}";; Architecture) printf '%s\n' "${DEB_ARCH:-amd64}";; esac
EOF
cat > "$root/bin/apt-ftparchive" <<'EOF'
#!/usr/bin/env bash
echo metadata
EOF
cat > "$root/bin/createrepo_c" <<'EOF'
#!/usr/bin/env bash
target=${!#}; mkdir -p "$target/repodata"; echo metadata > "$target/repodata/repomd.xml"
EOF
cat > "$root/bin/gpg" <<'EOF'
#!/usr/bin/env bash
while [ "$#" -gt 0 ]; do [ "$1" != --output ] || { touch "$2"; exit 0; }; shift; done
exit 1
EOF
chmod +x "$root/bin"/*
run_build() { PATH="$root/bin:$PATH" PEPPY_RELEASE_VERIFY_KEYRING="$root/keyring" PEPPY_RPM_VERIFY_KEYRING="$root/rpm-key" PEPPY_REPOSITORY_SIGNING_KEY=fixture "$@"; }
make_fixture
run_build "${build[@]}" stable 1.2.3 "$root/staged" "$root/repository"
test -f "$root/repository/apt/dists/stable/InRelease"
grep -Fq 'Valid-Until:' "$root/repository/apt/dists/stable/Release"
test -f "$root/repository/rpm/stable/repodata/repomd.xml.asc"
rm -rf "$root/repository"; rm "$root/staged/SHA256SUMS.sig"; expect_failure run_build "${build[@]}" stable 1.2.3 "$root/staged" "$root/missing-signature"
make_fixture; rm -rf "$root/repository"; printf '.sig\n' >> "$root/staged/SHA256SUMS"; sed -i.bak '/\.deb$/d' "$root/staged/SHA256SUMS"; rm "$root/staged/SHA256SUMS.bak"; expect_failure run_build "${build[@]}" stable 1.2.3 "$root/staged" "$root/only-sig"
make_fixture; DEB_NAME=other expect_failure run_build "${build[@]}" stable 1.2.3 "$root/staged" "$root/wrong-name"
make_fixture; RPM_VERSION=9.9.9 expect_failure run_build "${build[@]}" stable 1.2.3 "$root/staged" "$root/wrong-version"
make_fixture; FAIL_RPM_SIGNATURE=1 expect_failure run_build "${build[@]}" stable 1.2.3 "$root/staged" "$root/unsigned-rpm"
make_fixture; RPM_CHECKSIG_OUTPUT='package.rpm: digests OK' expect_failure run_build "${build[@]}" stable 1.2.3 "$root/staged" "$root/unsigned-digest-rpm"
make_fixture; expect_failure run_build "${build[@]}" staging 1.2.3 "$root/staged" "$root/channel-mismatch"
echo 'linux repository tests passed'
