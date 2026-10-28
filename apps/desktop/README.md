# Peppy desktop client

Follow the canonical [contributing workflow](../../CONTRIBUTING.md) for first run, checks, and security boundaries.

The desktop client uses Tauri with native Rust code and a web UI. It supports native development on macOS and native Windows development from WSL. The helper does not provide Linux native development; Linux packaging remains CI-only.

## Server setup and phone connections

Enter your server address to discover its setup state. A fresh community instance
shows whether first claim is open or token-protected. An open instance lets the
first device that completes setup become owner; a token-protected instance also
requires the operator setup code. The setup code authorizes initial claim only,
the admin password authorizes server management, and the generated vault shared
secret protects vault data. Existing instances support admin sign-in and
shared-secret enrollment. Passwords and shared secrets are handled by native
dialogs rather than React fields or Tauri command arguments.

Operators configure token-protected first claim with a nonempty
`PEPPY_SETUP_TOKEN` of at least 32 UTF-8 bytes, using the same token on every
replica. An absent or empty value is open first claim; no token is generated or
available in logs. Unclaimed deployments upgraded without the setting become
open. If an unwanted stranger first claim occurs, follow the root
[recovery procedure](../../README.md#recover-from-an-unwanted-first-claim): it
destroys only the affected deployment's Peppy database and attachment state and
is not password recovery.

An enrolled owner uses **Add phone** from the main workbench. Scan the single
**Connect your phone** QR, compare the verification codes, and approve the phone.
The selected mode transfers keys securely or requires the phone to enter the
shared secret. QR codes are not shown in Settings. The old reversed computer QR,
credential-file setup and Save QR action have been removed.

The local **App lock** setting is off by default. Enabling it requires a native
password confirmation and applies on the next start. It does not change the
server's admin password or the vault shared secret.

## MMS messages

MMS requires an enabled, permission-ready Android gateway advertising MMS content version 2 or later. Upgrade paired clients together. Groups and attachments select MMS; replies in an MMS conversation retain that transport. A missing capability blocks sending while preserving the editable draft. Incoming group replies may require own-number confirmation on the phone.

Attachment removal preserves draft revisions. Retry controls retry the encrypted file transfer, not the carrier message. Save uses a native file dialog and never exposes filesystem paths or encryption keys to the webview. Creating a public image link remains a separate, explicitly confirmed plaintext-sharing action.

Version-3 MMS capture adds separately encrypted previews. Previews download for
browsing; opening or saving an original requests that media on demand. Legacy
originals up to 2 MiB retain automatic downloading. The shared desktop/browser
conversation view groups consecutive images from the same known sender within
three minutes, preserves each message's status, and provides a paged **Media**
view and keyboard-navigable viewer. Video and audio playback starts only after a
user action; unsupported formats retain the Save fallback. Viewing private media
never creates a public link.

The displayed byte estimate is a lower bound: carrier limits apply to the complete encoded MMS, including headers. A fallback limit is labeled as an application limit. Gateway validation can still reject a message that passed this estimate. An unknown carrier outcome is not automatically retried.

Incomplete phone acquisition remains visible in the phone's health view until all parts are available. After acquisition completes, the event remains in the outbox until its parts upload; upload failures appear in transfer health. Android MMS is experimental pending physical-carrier acceptance. RCS is unavailable through the current companion integration.

## Contacts

Enable contact sync on each phone to browse its book in **Contacts**. Search and
page through contacts, edit supported fields, crop a photo, create or delete a
contact, and inspect the owning phone's result. A saved request stays pending
until that phone applies it; phone permissions, edit policy and background
scheduling can delay or reject it. New-contact destination and approvals are
configured on the phone. iOS notes are not editable.

Photos use private encrypted 256×256 JPEG attachments, at most 64 KiB. Contact
names and avatars resolve for messaging and recipient discovery without changing
stored delivery addresses. Ambiguous shared numbers fall back to the number.
Hidden native notification previews remain generic.

Unsaved contact edits are kept when switching contacts, books or the main rail
until you save or explicitly discard them. **Recently deleted** offers retained
contacts for 90 days; restore requests a new OS contact on the owning phone.
**Forget** requires confirmation and permanently hides a book on this desktop,
including its name/avatar matches. It does not delete contacts on the phone.

Use **Repair** in the book-list header to rebuild this desktop's remote contact
cache from a fenced server snapshot. It preserves phone-owned state and pending
effects; it cannot recover an upload that never reached the server. The view
reports missing keys, incomplete repair and paused retention. See the root
[compaction and recovery guidance](../../README.md#compaction-and-recovery).

## Background mode and floating conversations

On macOS and Windows, closing the main window keeps Peppy in the menu bar or system tray. Sync and floating conversations continue running. Use **Open Peppy** to return to the main window or **Quit Peppy** to exit. Quit waits for draft saves; a failed save keeps the app available for recovery.

The native tray badge totals unread messages across all conversations while Peppy runs in the background. It shows `1`–`99` or `99+`; its tooltip retains the exact total. App or vault-key lock shows a lock badge instead, and an unavailable or revoked account restores the plain Peppy icon. Peppy retains its tray status item for its full process lifetime. Windows DPI rasterization refreshes when the badge changes or a failed apply retries; an unchanged state event does not recreate the icon.

Use the popout button on a conversation row or in its header to open a circular chat head and its compact conversation panel. **–** collapses to the bubble; **×** saves the draft and closes the bubble. The panel resizes from its edges and composer grip, and its size and position are remembered per conversation and re-fitted to the current screen. Drag the circle to move it and its expanded panel. **Dismiss head** removes the pin without deleting the conversation. Up to eight conversations can be pinned; saved pins restore collapsed when the local account is available. Floating conversations work offline and do not require unlocking device sync.

In Settings, **Start at login** opts into quiet menu-bar/tray startup. It is off by default. A manual launch opens the main window in the existing app instance. If the tray cannot initialize, Peppy shows the main window instead of launching invisibly.

Heads are included in normal macOS and Windows builds. Browser fixtures demonstrate panel layout and controls, but do not demonstrate native circle input, focus, or multi-monitor behavior; verify those on each target OS.

## macOS

Install a stable Node 26 release (the major version in `../../.node-version`), pnpm 12.8.1, and Rust from `../../rust-toolchain.toml`. Minor and patch updates within Node 26 are supported. From the repository root:

```sh
just desktop-dev
just desktop-bundle
just desktop-run
just desktop-open
```

`desktop-dev` runs Tauri's hot-reload development server using the `tauri.dev.conf.json` overlay with dev identity `org.peppy.desktop.dev`. `desktop-bundle` and `desktop-run` build release bundles with the same dev overlay; they generate only the `.app` bundle at `apps/desktop/src-tauri/target/release/bundle/macos/Peppy_dev.app` with no DMG installer created locally. `desktop-bundle` builds without opening; `desktop-run` builds then opens the app in place. `desktop-open` opens an existing `Peppy_dev.app` in place without installing it to `/Applications`. Set `CARGO_TARGET_DIR` to choose a different target directory.

`desktop-dev`, `desktop-bundle`, and `desktop-run` run `pnpm install --frozen-lockfile` before building to synchronize dependencies with the selected source tree's lockfile. An install failure stops the build; `desktop-run` does not open an older bundle. These commands do not upgrade packages beyond the lockfile.

On macOS, `desktop-bundle` and `desktop-run` fully ad-hoc sign the app bundle using its `CFBundleIdentifier` unless `PEPPY_MACOS_SIGNING_IDENTITY` is set, then strictly verify the bundle and nested signatures. Ad-hoc bundles have sealed resources but no TeamIdentifier; development builds are not notarized.

Development uses a separate Keychain namespace. The first dev launch creates an empty profile. Any prior app data and Keychain records stored under the production identity `org.peppy.desktop` remain unchanged; there is no automatic migration. Legacy Keychain records copy only within the current namespace, never from production into dev.

Each ad-hoc rebuild has a new code identity, so macOS asks again before the dev app can read its Keychain record. To stop the prompts across rebuilds, set `PEPPY_MACOS_SIGNING_IDENTITY` to the exact name or SHA-1 of an Apple Development identity (Xcode, signed in with your Apple ID) or a Developer ID identity, then choose **Always Allow** once. This applies when `just desktop-bundle` or `just desktop-run` builds the bundle; `just desktop-open` only opens the existing bundle, and `just desktop-dev` does not use it. Switching identities asks once more, and Apple Development certificates expire after a year.

The base `tauri.conf.json` defaults to the development identity (`productName: Peppy_dev`, identifier: `org.peppy.desktop.dev`) so direct Tauri commands do not collide with an installed release. The supported macOS commands retain the explicit `tauri.dev.conf.json` overlay for compatibility. Trusted production installers override that base configuration to `productName: Peppy` and identifier `org.peppy.desktop`; they are downloaded from [GitHub releases](https://github.com/mattv8/peppy/releases).

## Windows from WSL

Keep the current checkout and any `CARGO_TARGET_DIR` on a drive-letter NTFS path. The helper rejects ext4 and UNC paths. Install a stable Node 26 release, pnpm 12.8.1, Rust at the pinned version, Visual Studio C++ tools with the Windows SDK, WebView2, and native Windows Perl with `IPC::Cmd`. Do not use Git/MSYS Perl.

```sh
just desktop-dev
just desktop-bundle
just desktop-open
```

The helper calls `powershell.exe` or `pwsh.exe` with `-NoProfile -ExecutionPolicy Bypass`. Bypass applies only to that process; the helper does not change machine or user execution policy. It does not use WSLg, Linux Tauri, a mirrored checkout, global PATH changes, or profile changes. The default Windows outputs are `apps/desktop/src-tauri/target/release/bundle/{nsis,msi}`; `desktop-open` starts `apps/desktop/src-tauri/target/release/peppy-desktop.exe` after it exists. Set `CARGO_TARGET_DIR` to choose a different target output directory.

These outputs are development artifacts. The macOS bundle is not Developer ID signed or notarized. They do not demonstrate installer signing, store readiness, native connection behavior, WebSocket reconnect, attachment/public-copy behavior, or transparent-window input behavior.
