# Desktop client reference

This reference covers desktop implementation, builds, and verification. For repository-wide workflow, see [CONTRIBUTING.md](../../CONTRIBUTING.md). User setup and capabilities are in the [desktop guide](https://peppy.pro/docs/desktop).

The desktop client uses Tauri with native Rust and a web UI. Native development is supported on macOS and Windows from WSL. Linux native development is unsupported; Linux packaging is CI-only. Passwords and vault secrets must remain in native dialogs, not React fields or Tauri command arguments.

## macOS

Install stable Node 26 from [`../../.node-version`](../../.node-version), pnpm 12.8.1, and Rust from [`../../rust-toolchain.toml`](../../rust-toolchain.toml). Minor and patch Node 26 releases are supported. From the root:

```sh
just desktop-dev
just desktop-bundle
just desktop-run
just desktop-open
```

`desktop-dev` starts hot reload with `tauri.dev.conf.json` and development identity `org.peppy.desktop.dev`. `desktop-bundle` and `desktop-run` build release bundles with that overlay and create only `apps/desktop/src-tauri/target/release/bundle/macos/Peppy_dev.app`; they do not make a local DMG. `desktop-run` opens the in-place bundle; `desktop-open` opens an existing bundle without installing it. Set `CARGO_TARGET_DIR` to choose a different macOS target directory. The commands run `pnpm install --frozen-lockfile`; a failed install stops the build and `desktop-run` does not open an older bundle.

The bundle is fully ad-hoc signed unless `PEPPY_MACOS_SIGNING_IDENTITY` is set, then its nested signatures are verified. Ad-hoc bundles have sealed resources without a TeamIdentifier and are not notarized. Development uses a separate Keychain namespace and never migrates production `org.peppy.desktop` data. Legacy Keychain records copy only within the current namespace, never from production into development. Each ad-hoc rebuild has a new code identity and can prompt again for Keychain access. Set `PEPPY_MACOS_SIGNING_IDENTITY` to an Apple Development or Developer ID identity and choose **Always Allow** once to preserve access across rebuilds. Switching identities prompts once more. Apple Development certificates expire after one year. The base `tauri.conf.json` uses `Peppy_dev` and `org.peppy.desktop.dev`; trusted production installers override them and come from [GitHub Releases](https://github.com/mattv8/peppy/releases).

Mac App Store packaging is opt-in (`PEPPY_MACOS_DISTRIBUTION=app-store`) and
uses `packaging/macos/app-store/tauri.app-store.json`. It targets macOS 13,
uses the App Sandbox, embeds its provisioning profile, and produces a signed
`.pkg`; it does not use the direct-download updater, DMG repair, or notarization.

## Windows from WSL

Keep the checkout and `CARGO_TARGET_DIR` on a drive-letter NTFS path; the helper rejects ext4 and UNC. Install Node 26, pnpm 12.8.1, pinned Rust, Visual Studio C++ tools with the Windows SDK, WebView2, and native Windows Perl with `IPC::Cmd`. Do not use Git/MSYS Perl.

```sh
just desktop-dev
just desktop-bundle
just desktop-open
```

The helper runs `powershell.exe` or `pwsh.exe` with `-NoProfile -ExecutionPolicy Bypass`; it changes neither machine nor user policy. It does not use WSLg, Linux Tauri, a mirrored checkout, global PATH, or profile changes. Outputs are `apps/desktop/src-tauri/target/release/bundle/{nsis,msi}` and `desktop-open` starts `apps/desktop/src-tauri/target/release/peppy-desktop.exe` after it exists. Set `CARGO_TARGET_DIR` to select another output directory.

These are development artifacts. They do not establish installer signing, store readiness, native connection behavior, WebSocket reconnect, attachment/public-copy behavior, or transparent-window input behavior.
