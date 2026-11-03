# Peppy vendored `tauri-plugin-single-instance`

This directory is a maintained local copy of crates.io package
[`tauri-plugin-single-instance` 2.5.2](https://crates.io/crates/tauri-plugin-single-instance),
licensed under Apache-2.0 OR MIT.

`src/platform_impl/macos.rs` differs from upstream only to make macOS socket handling safe for
Peppy: it uses a stable SHA-256 bundle-id prefix, keeps the complete socket path within the macOS
`sun_path` limit, and holds a `flock` lock file even when a sandboxed temporary directory is too
long for socket activation forwarding. On a held lock it retries for two seconds before treating a
process as secondary, so a self-update restart can acquire a just-released lock. This preserves the
single-primary guarantee while logging that activation forwarding is unavailable. The first upgrade
from a build without this lock can briefly allow two primaries because the old build held only the
`/tmp` socket. Windows and Linux sources remain upstream-identical.

`Cargo.toml` adds macOS-only `sha2` and `libc` dependencies for the stable socket identifier and
file lock. Its empty `[workspace]` allows the vendored crate's own tests to run without adding it
to Peppy's root workspace.
Remove this vendor patch when upstream provides equivalent bounded, sandbox-safe macOS socket
handling.
