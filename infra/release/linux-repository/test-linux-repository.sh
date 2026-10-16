#!/usr/bin/env bash
set -euo pipefail
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
setup=(bash "$script_dir/setup-peppy-updates.sh")
build=(bash "$script_dir/build-repository.sh")
root=$(mktemp -d); trap 'rm -rf "$root"' EXIT

expect_failure() { if "$@" >/dev/null 2>&1; then echo "expected failure: $*" >&2; exit 1; fi; }
setup_args=(--distro debian --channel stable --repo-url https://packages.example.invalid/peppy --key-url https://packages.example.invalid/peppy/key.asc --key-fingerprint 0123456789abcdef0123456789abcdef01234567 --dry-run)
dry_run=$("${setup[@]}" "${setup_args[@]}")
grep -Fq 'Pin: origin packages.example.invalid' <<<"$dry_run"
grep -Fq 'Pin-Priority: -1' <<<"$dry_run"
grep -Fq 'Pin-Priority: 990' <<<"$dry_run"
grep -Fq 'Wants=network-online.target' <<<"$dry_run"
grep -Fq 'install --only-upgrade --yes peppy' <<<"$dry_run"
if grep -Eq 'unattended-upgrades|dnf-automatic|sh -c' <<<"$dry_run"; then exit 1; fi
fedora_dry_run=$("${setup[@]}" --distro fedora --channel staging --repo-url https://packages.example.invalid/peppy --key-url https://packages.example.invalid/peppy/key.asc --key-fingerprint 0123456789abcdef0123456789abcdef01234567 --dry-run)
grep -Fq 'includepkgs=peppy' <<<"$fedora_dry_run"
grep -Fq 'dnf --assumeyes upgrade peppy' <<<"$fedora_dry_run"
expect_failure "${setup[@]}" --distro debian --channel stable --repo-url http://invalid --key-url https://invalid --key-fingerprint 0123456789abcdef0123456789abcdef01234567 --dry-run
expect_failure "${setup[@]}" --distro debian --channel stable --repo-url https://one.invalid --key-url https://two.invalid/key --key-fingerprint 0123456789abcdef0123456789abcdef01234567 --dry-run
expect_failure "${setup[@]}" --distro debian --channel stable --repo-url "https://valid.invalid/\$(bad)" --key-url https://valid.invalid/key --key-fingerprint 0123456789abcdef0123456789abcdef01234567 --dry-run
expect_failure "${setup[@]}" --distro debian --channel stable --repo-url https://valid.invalid --key-url https://valid.invalid/key --key-fingerprint short --dry-run

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
  *' --checksig '*) [ "${FAIL_RPM_SIGNATURE:-0}" = 0 ];;
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
make_fixture; expect_failure run_build "${build[@]}" staging 1.2.3 "$root/staged" "$root/channel-mismatch"
echo 'linux repository tests passed'
