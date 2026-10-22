# Peppy vendored `libsodium-sys-stable`

This directory is a maintained local copy of crates.io package
[`libsodium-sys-stable` 1.24.0](https://crates.io/crates/libsodium-sys-stable/1.24.0),
licensed under MIT OR Apache-2.0. The downloaded crate archive SHA-256 is:

`72b04bf6da2c98b727af37ab62cb505f4d751b975b034a9b9ad491d333b0564e`

All retained upstream source, build, license, manifest-original, test, example,
and bundled libsodium source archive and signature files are byte-for-byte copies
of that package. `LATEST.tar.gz` and `LATEST.tar.gz.minisig` are required build
inputs for the maintained bundled libsodium 1.0.22 source. The upstream
`Cargo.lock` is intentionally omitted because its `.gitignore` excludes it.

`Cargo.toml` has one dependency-manifest fix: build dependency `zip` is pinned to
`=9.0.4`, retaining upstream `deflate` and disabled default features. This resolves
RUSTSEC-2026-0367, RUSTSEC-2026-0368, and RUSTSEC-2026-0369 without changing
libsodium behavior.

## Audit coverage

`cargo-deny` skips advisories for path dependencies, so the vendored local copy
is not covered by root and desktop advisory graphs. The `upstream-audit.lock`
file pins the upstream registry package version and is validated and audited
separately by `infra/audit/check-vendored.py`, which is run in CI after
`cargo-deny`. This ensures upstream advisories are still checked automatically
without suppression.

To refresh `upstream-audit.lock` when updating the vendored package:

1. Update `vendor/libsodium-sys-stable/` with a new archive from crates.io and
   verify the new version in the manifest.
2. Reapply the `zip =9.0.4` pin until a suitable upstream release requires a
   patched zip version; remove the Peppy pin when that release is adopted.
3. Edit `upstream-audit.lock` to match the package name and version, and update
   the checksum from the crates.io package details.
4. Run `just audit-dependencies` to validate the lock and audit it.
5. Commit both the updated manifest and the refreshed lock.
