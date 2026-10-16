# Peppy Linux package repository

This directory builds separate `stable` and `staging` APT/DNF repositories from
final staged GitHub Release assets. It does not publish them. Operators serve
the generated tree and signing key from an HTTPS URL they control.

## Release operator prerequisites

- Exact staged assets: `peppy-<version>-linux-x86_64.deb`,
  `peppy-<version>-linux-x86_64.rpm`, `SHA256SUMS`, and detached
  `SHA256SUMS.sig`. These are the names emitted by `stage-assets.sh`.
- `PEPPY_RELEASE_VERIFY_KEYRING`: trusted public keyring for the staged release
  checksum signature.
- `PEPPY_RPM_VERIFY_KEYRING`: RPM public-key file imported into a temporary,
  dedicated RPM database for payload verification. Installed host RPM keys are
  never used as the trust source.
- `PEPPY_REPOSITORY_SIGNING_KEY`: pre-provisioned GPG signing identity. The
  scripts never create or export keys.
- `gpgv`, `gpg`, `apt-ftparchive`, `createrepo_c`, `rpm`, `dpkg-deb`, and
  `sha256sum`.

Run `build-repository.sh stable <version> <staged-dir> <out-dir>` (or
`staging`). Stable accepts only `X.Y.Z`; staging only `X.Y.Z-staging.N`.
Checks cover manifest signature, exact checksum line, package name,
architecture, and normalized package version before repository metadata is
generated. The output directory must be new and cannot contain (or be
contained by) the staged directory. APT Release/InRelease and DNF
`repomd.xml` are signed. APT Release metadata has an expiry (`Valid-Until`).

## Explicit administrator opt-in

After hosting a repository and scoped public key, an administrator may run:

```sh
sudo ./setup-peppy-updates.sh --distro debian --channel stable \
  --repo-url https://packages.example.invalid/peppy \
  --key-url https://packages.example.invalid/peppy/peppy.asc \
  --key-fingerprint 0123456789ABCDEF0123456789ABCDEF01234567 --dry-run
```

`--dry-run` is fixture-only: it performs no downloads or system changes.
Without it, setup downloads through an HTTPS-only redirect chain and verifies
the supplied 40-hex OpenPGP fingerprint before trusting the key. Repository
and key URLs must use the same HTTPS authority, preventing a repository setup
from trusting a key supplied by another host.

The setup creates a `peppy-update.timer` that invokes package management for
the `peppy` package only. It neither enables `unattended-upgrades` or
`dnf-automatic`, changes another repository, grants blanket Polkit permissions,
nor schedules a reboot. The service wants `network-online.target`.

APT preferences pin the explicit configured hostname and deny every package
from it except `peppy`; they do not trust attacker-changeable Release `Origin`
alone. The APT source is amd64-only. DNF uses `includepkgs=peppy`, not a global
automatic-update policy. RPM key trust is host-global by RPM design, so this
package restriction is policy scope, not per-repository cryptographic
isolation. Existing Peppy setup files are never overwritten. To disable, run
`systemctl disable --now peppy-update.timer`, then remove the Peppy source,
preferences/repo file, key, and service/timer files. APT lock behavior and
distribution package policy remain system-controlled; test lock contention,
authorization denial, and dependency failure on each supported distribution
before making a distribution-readiness claim.
