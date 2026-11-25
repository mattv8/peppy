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

## Live typing and floating heads

The live typing supervisor in [live_typing.rs](../../apps/desktop/src-tauri/src/live_typing.rs) reconnects with backoff capped at 60 seconds (`LIVE_MAX_BACKOFF`). A context-change wake waits no sooner than 5 seconds after the previous attempt. Each socket close prints one content-free `peppy: live typing ...` counter line.

Floating heads on macOS and Windows show a contact's typing as a pill in the unread badge slot. The rule is in the [frontend style guide](../frontend-style-guide.md#windows-drafts-and-layout). It is unit-tested only and has not been seen running.

## Diagnostics

Diagnostics is off by default. Turn on **Verbose logging** in **Settings → Diagnostics**; it is the last category in the Settings navigation. It appears only where the host provides the four diagnostics commands, so the desktop app has it and the web app does not. The commands are not on the locked-app allowlist in [lib.rs](../../apps/desktop/src-tauri/src/lib.rs), so the section works only while the app is unlocked. Android keeps its Diagnostics entry on a locked screen; the desktop does not.

When recording is on, the recorder writes content-free numeric records for the live socket, typing sends, inbound typing, floating-head typing, the core's typing cause counters, and database faults. The records hold numbers only: no message text, contacts, identifiers, or credentials. The core counters are read only while recording is on, so turn it on before the test.

The log lives in the `diagnostics` folder under the app's local data directory. Tauri resolves that directory as the platform's local data folder joined with the bundle identifier. For the base development identifier in [tauri.conf.json](../../apps/desktop/src-tauri/tauri.conf.json), that gives:

- macOS: `~/Library/Application Support/org.peppy.desktop.dev/diagnostics/`
- Windows: `%LOCALAPPDATA%\org.peppy.desktop.dev\diagnostics\`

Trusted production installers from GitHub Releases use the identifier `org.peppy.desktop`, so their folder is `org.peppy.desktop/diagnostics/` under the same parent. The App Store build is sandboxed ([entitlements.plist](../../packaging/macos/app-store/entitlements.plist)) and uses the same identifier, so its folder is inside the app container: `~/Library/Containers/org.peppy.desktop/Data/Library/Application Support/org.peppy.desktop/diagnostics/`. That path was not checked on a running build. The folder holds `current.log` and `previous.log`, 2 MiB each and 4 MiB in total. When a record would take `current.log` past 2 MiB, `previous.log` is replaced by it and a new `current.log` starts. The switch is saved as `enabled` (`1` or `0`). No log files are created while recording is off. On first use, the recorder drops invalid, oversize, and unterminated lines and trims the files to their caps. On macOS and other Unix systems the files are created with mode `0600`.

Settings → Diagnostics shows, in order:

- **Verbose logging** turns recording on or off. Turning it off drops records still queued. The help dialog says it adds overhead.
- A status list with **Recording**, **Retained log size**, and **Dropped events**. Dropped events counts records the recorder did not keep: invalid or oversize records, records refused by a full queue of 256, and records queued when the switch changed.
- **Export log** stages a snapshot, then opens a native save dialog titled "Export diagnostic log" with the suggested name `peppy-diagnostics-YYYYMMDDTHHMMSSZ.log.txt` (UTC). The file holds the header line, one state record (event 24), and the newest retained records within 4 MiB. Export works whether recording is on or off. Cancelling writes nothing. The button stays disabled until a log exists.
- **Clear logs** deletes `current.log`, `previous.log`, and any staged export. Recording keeps its current state. Clearing cannot erase copies that were already exported.

Each record is one line:

```text
event=<code> session=<token> timestamp_ms=<ms> trace=0 <field>=<value> ...
```

`session` is random per app process, `timestamp_ms` counts from app start, and `trace` is always `0` on desktop. Values are integers. The state record (event 24) writes `session` and `timestamp_ms` as `0`, `34` as the dropped count, and `35` as `1` when recording is on or `0` when it is off. A line is at most 512 bytes, including its newline.

The first line of an export is `schema=2 platform=desktop version=<app version>`, followed by the event, field, and value tables, `live_typing_causes=`, and `database_fault_kinds=`. The desktop header records the app version but no source commit, so the commit check used for phone exports does not apply here.

| Event | Name | Fields | Recorded when |
| --- | --- | --- | --- |
| 24 | `diagnostic_state` | `34` dropped, `35` enabled | Once per export, after the header |
| 29 | `live_typing` | `29` cause code, `19` count increase | A core cause counter rose since its last read. Reads happen at a typing send, a live-typing context change (enable or disable), and an inbound typing frame |
| 30 | `live_socket` | `29` reason; `4` delay_ms and `5` attempt with reason 6 | Socket lifecycle, backoff, context wake, typing enable or disable, and suspend |
| 31 | `typing_send` | `28` outcome | Each outbound typing decision |
| 32 | `typing_inbound` | `28` outcome; `19` count with outcome 4 | Each inbound frame's result: a display change, a reply sent or refused, follow-ups sent, nothing to do, or not current |
| 33 | `head_typing` | `28` outcome | A floating head's displayed typing flag changes |
| 36 | `database_fault` | `29` kind, `70` SQLite code when present, `71` site, `72` line, `73` age_ms | A database error reaches the bridge; kind 0 carries only kind |

Values by event:

| Event | Field | Values |
| --- | --- | --- |
| 30 | `29` reason | 1 `connecting`, 2 `hello_ok`, 3 `hello_rejected`, 4 `connect_failed`, 5 `closed`, 6 `backoff`, 7 `context_wake`, 8 `typing_enabled`, 9 `typing_no_context`, 10 `typing_disabled`, 11 `suspend_detected` |
| 31 | `28` outcome | 1 `sealed`, 2 `not_sealed`, 3 `no_target`, 4 `transmit_refused`, 5 `not_current` |
| 32 | `28` outcome | 1 `display_started`, 2 `display_stopped`, 3 `reply_sent`, 4 `follow_ups_sent`, 5 `nothing`, 6 `not_current`, 7 `reply_refused` |
| 33 | `28` outcome | 0 `hidden`, 1 `shown` |
| 36 | `29` kind | 0 `none`, 1 `sqlite_failure`, 2 `no_rows`, 3 `decode`, 4 `other` |
| 36 | `70` sqlite_code | SQLite extended result code, only for `sqlite_failure` |
| 36 | `71` site | Source path hash |
| 36 | `72` line | Source line of the conversion |
| 36 | `73` age_ms | Milliseconds since the conversion, at least 0 |

Event 29 cause codes and names are listed in the export header's `live_typing_causes` table and in the [typing indicators notes](../ops/google-messages-link-triage.md#typing-indicators).

Event 36 records the last SQLite failure converted on that thread before a `database unavailable or corrupt` error reached the bridge, with its age. It can belong to a nearby error that was handled; kind 0 means no SQLite failure was converted on that thread since the last bridge error mapping. To decode `site`, use `database_fault_site` to hash source paths after `src/` (for example `draft_sync.rs`) under `crates/client-core/src`.

The recorder and the Settings section have unit tests. Nothing here has been seen in a running desktop app.
