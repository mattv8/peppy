# Peppy vendored `block`

This directory is a maintained local copy of crates.io package
[`block` 0.1.6](https://crates.io/crates/block/0.1.6), licensed under MIT. The
downloaded crate archive SHA-256, matching the desktop `Cargo.lock` entry, is:

`0d8c1fef690941d3e7788d328517591fecc684c084084702d6ff1641e993699a`

`README.md`, `src/lib.rs`, and `src/test_utils.rs` are from that archive.
`Cargo.toml.orig` preserves the archive's unmodified `Cargo.toml` under a new
name. The archive omits a license file, so `LICENSE` is the authoritative
MIT text with `Copyright (c) Steven Sheldon`, fetched from the upstream author's
[`rust-objc` repository](https://raw.githubusercontent.com/SSheldon/rust-objc/master/LICENSE.txt).
The attribution matches the package author in `Cargo.toml.orig` and the package's
crates.io MIT license declaration.

`Cargo.toml` retains the upstream package name, version, metadata, and Rust 2015
edition. It intentionally omits only upstream's `objc_test_utils` development
dependency, whose local `test_utils` package is unavailable in the registry
archive; the retained `src/test_utils.rs` is otherwise unchanged.

The only source deltas in `src/lib.rs` are:

- replace the private uninhabited `Class` enum with the ABI-equivalent
  `#[repr(C)]` opaque zero-sized struct used by maintained block-rs forks;
- make every previously implicit `extern` ABI explicit as `extern "C"`, which
  preserves Rust's historical default ABI and avoids local-path `missing_abi`
  warnings.

This patch maintains `block` 0.1 compatibility only for the `cocoa` 0.26
dependency chain. Remove this vendored crate and its desktop patch when a future
objc2 migration removes that chain.

## Audit coverage

`cargo-deny` skips advisories for path dependencies, so the vendored local copy
is not covered by root and desktop advisory graphs. The `upstream-audit.lock`
file pins the upstream registry package version and is validated and audited
separately by `infra/audit/check-vendored.py`, which is run in CI after
`cargo-deny`. This ensures upstream advisories are still checked automatically
without suppression.
