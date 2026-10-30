# Peppy desktop frontend style guide

Reference for the Tauri desktop UI: a dense, bounded workbench in the VS Code
Modern palette, with a coral accent and locally bundled Droid Sans. Ragtime is
visual intent only; do not import from it. Keep the compact 4px rhythm.
Security, data, and evidence rules live in [domain guidance](AGENTS.md).

## Source ownership

| Source | Owns |
|---|---|
| `packages/desktop-ui/src/peppy-tokens.css` | `--peppy-*` tokens, local font, reduced motion |
| `packages/desktop-ui/src/styles.css` | Shared titlebar, panes, composer, resize, status styling (legacy aliases are compatibility only) |
| `packages/desktop-ui/src/index.tsx` | `AppTitlebar`, `Panel`, `StatusPopover`, `ConnectionDot`, composer, recipient panel |
| `packages/desktop-ui/src/phone.ts` | Shared phone validation and formatting |
| `apps/desktop/src/App.tsx`, `status.ts` | Shells, views, drafts, persistence, status details and severity |
| `apps/desktop/src/app.css`, `settings.css`, `vibrancy.css` | Desktop chrome, settings cards, native-material CSS |
| `apps/desktop/src-tauri/src/window_chrome.rs`, `material_state.rs`, `tray.rs` | Native effect lifecycle, success inspection, deferred show, `data-vibrancy` |
| `apps/desktop/src-tauri/tauri.conf.json`, `tauri.macos.conf.json` | Base config keeps the main window opaque; the macOS overlay makes it transparent |

## Surfaces and themes

- **Tier 1, desk:** window background. Solid or subtle tonal fill; translucent
  only under native macOS material.
- **Tier 2, docked:** rail, titlebar, thread list, conversation, contacts, and
  settings. Structural borders, full legibility, no blur. Settings cards are opaque.
- **Tier 3, floating:** composer, recipient rail, popovers, pickers, dialogs.
  CSS glass, on small areas only. Never blur a full scrolling list.

Preserve the composer's opaque gutters and fade, read-visibility geometry,
recipient-rail boundaries, pane resizing, and saved layouts when restyling.

- Use `--peppy-*` tokens for themeable values in touched styles. Read exact
  values from `peppy-tokens.css`; this guide does not copy them.
- Shells carry `theme-system`, `theme-light`, or `theme-dark`. Light values
  appear in both `.theme-light` and the system-light media rule; keep them in sync.
- Glass falls back to opaque with zero blur on native Linux, under
  `prefers-reduced-transparency`, without `backdrop-filter`, and on native
  vibrancy failure or inactive state. Forced colors use system colors and no
  shadows. Reduced motion disables animation.
- Theme overrides target the shell element carrying the theme class, not a
  descendant of it or an assumed theme on the document root.
- A fallback must stay readable. A native fallback must never leave a transparent,
  blank shell.
- Bundle fonts and assets locally. The CSP stays `default-src 'self'`; add no
  remote sources. Treat display timestamps as opaque strings.

## Conversation tone contract

- Conversation identity uses the existing normalized seed and 120-slot palette.
  Manual choices override a slot for that seed; Automatic removes the active
  override through a retained encrypted sync record. Neither choice edits a
  phone's contact book.
- Incoming 1:1 bubbles match the conversation avatar's tone. Group bubbles use
  the sender's normalized address, with the group seed as fallback. Outgoing
  bubbles use the self-bubble theme tokens, which default to the coral accent.
- `packages/desktop-ui/src/tone.tsx` owns tone resolution. Theme palettes retain
  stable slot identities and supply matching foreground colors with at least
  4.5:1 text contrast. Do not resample the existing avatar palette to add a preset.
- `--peppy-tone` and `--peppy-tone-text` are element-local base values. Derive
  bubble surface, foreground and border tokens on the colored element, where
  CSS variable references can resolve its seed-specific values. Set an explicit
  foreground on neutral attachment surfaces nested inside colored bubbles.
- The contact editor's color preference targets the saved primary number and
  saves immediately, independently of contact Save/Cancel. Unsaved number edits
  must not silently retarget the color preference. Disable the picker when no
  saved number is available.
- Color preferences are vault-scoped. Locked snapshots omit the override map;
  absence of the optional map alone is not a lock signal. Use explicit session
  and app-lock guards for mutations, and discard stale UI completion state
  when the session or target changes.

## Titlebar status and chrome

- Main `Panel.status` carries sync, carrier disclosure, and connection state.
  `StatusPopover`, opened from the rail dot, shows full wrapped details. The
  main titlebar has no status pills.
- Composer and floating-head headers show only a read-only `ConnectionDot` with
  the full `Connection: <connectionText>` label. They never show sync or carrier
  disclosures.
- These disclosure strings stay exact and untruncated: `Device sync encrypted`,
  `Device sync key mismatch`, `Device sync not unlocked`, and `Carrier SMS/MMS not
  end-to-end encrypted`.
- App-drawn platform-appropriate chrome. The one exception is the macOS main window, which
  uses native overlay traffic lights (`titleBarStyle: "Overlay"`) so the OS tile menu and
  window managers work; `AppTitlebar` reserves their 76px slot with a non-interactive spacer
  (`nativeWindowControls`). Composer windows, head panels, Windows and Linux stay app-drawn.
  Close paths use `closeAfterSave` or the native `CloseRequested` lifecycle, and a draft
  flush failure keeps the window open. The detached
  composer is not a second main window.

## Icons and compose

- Lucide icons only, through `desktop-ui` re-exports. No direct Lucide dependency
  in app packages. Keep each platform's window-control conventions.
- The in-place compose action is in the thread-list toolbar in side-rail mode;
  it is in the titlebar in titlebar navigation or when the list is hidden,
  collapsed, or another view is selected. With no conversation selected, a
  centered empty-state `New conversation` CTA may coexist with either and
  invokes the same in-place draft action.
- `AppTitlebar.onNewMessage` is passed only in titlebar mode or when the list is
  inaccessible; otherwise it stays `undefined`.
- In-place compose uses `MessageSquarePlus`. The separate detached action,
  labeled `New message window`, uses `SquarePen`.

## Settings

- Category navigation beside one scrolling column of cards. Scroll position
  selects the category through anchors and `aria-current`, not tabs, since several
  sections can be visible together. Omit categories without content. Keep
  `data-settings-section` values.
- Do not add settings search, settings QR, QR save, credential-file onboarding,
  credential export, or database-reset UI.
- Secondary controls sit behind disclosure. Pending drafts and in-flight actions
  survive toggles; async failures and capability states stay visible.
- On submit, reveal validation errors before focusing a hidden field. A known
  host error focuses its matching field; an ambiguous edit error reveals
  populated groups without moving focus.
- Setup follows the server-advertised deployment mode. Never route a community
  browser into hosted sign-in because an outer UI preference defaults to Hosted.

## Enrollment UI

Stable stage and QR hooks live in [SetupLanding](../apps/desktop/src/SetupLanding.tsx).
Protocol rules are in [domain guidance](AGENTS.md).

- Two enrollment QR purposes, each shown by one side and never in Settings.
  **`connect_phone`** is shown by the existing owner on first setup or from
  **Add phone** while the owner has no phone; the new phone scans it.
  **`connect_computer`** is shown by the unconnected desktop or browser in its
  own setup; an existing owner phone scans it, and the owner approves there.
  Neither QR has a Save action or is a credential or approval. The
  `connect_computer` payload necessarily includes its invitation secret, so do
  not describe that secret as absent from the renderer.
- For `connect_phone`, rotate only near or after expiry, or when the requested
  method changes. Never rotate while verifying a claimed phone.
- **Enter Vault key phrase on phone** switches a waiting `connect_phone`
  invitation to the shared-secret method before revealing the Vault key phrase.
  It does not apply to `connect_computer`. **Set up phone later**
  cancels only the invitation and is remembered per origin.
- After claim, show the SAS and require the owner's explicit checkbox before
  approval. After approval, never offer **Start again** or a replacement, even if
  the server reports expiry first.
- A cancellation racing a claim must preserve verification (`SetupLanding.test.tsx`).

Secret handling:

- **Native desktop:** the setup code is requested only for token-protected first
  claim, and the admin password comes from a native secure prompt. The Vault
  key phrase stays native and is shown only in a native dialog (**Copy Vault
  key phrase**). Phone approval uses the active Rust client's verified handoff
  capability without a secret prompt; recovery may use the protected binding
  secret when that capability is unavailable. Desktop computer enrollment, as
  the recipient, must not prompt for the Vault key phrase during activation.
- **Browser:** the generated Vault key phrase is shown once and needs a save
  acknowledgement. DOM prompts are reachable by same-origin JavaScript and must
  not be described as native secure. Keep values out of shared React snapshots.
  Clear secret fields after use.
  Phone approval invokes worker-private Rust capability, not a DOM secret prompt.
  Browser computer enrollment activates from the verified handoff without a
  Vault key phrase prompt. Browser restart asks for the Vault key phrase unless
  the user selected **Remember on this browser** during a manual unlock. Keep that
  checkbox unchecked by default and show the browser-profile access warning even
  when unchecked. Enrollment does not enable it. Lock and sign-out attempt to
  forget recovery; never claim they or device revocation protect an offline or
  copied browser profile.
- App-lock credentials are a separate native gate. See
  [native enrollment](../apps/desktop/src-tauri/src/enrollment.rs),
  [app lock](../apps/desktop/src-tauri/src/app_lock.rs), and
  [browser bridge](../apps/web/src/browser-bridge.ts).

Community first setup shows the server-advertised mode. Open claim shows no
setup-code input; token-protected claim shows it, citing `PEPPY_SETUP_TOKEN`.
Keep the setup code, administrator password, and Vault key phrase distinct.
Show no recovery or reset control. Web-only **Manage server** shows device status
and read-only storage, quota, and retention, with no pairing QR or database reset.

Local lock is off by default, protects local UI access only, and applies on the
next launch. Native dispatch enforces it; a React overlay is not a security gate.
It does not replace account authorization, device credentials, or the vault shared
secret. Native code sanitizes locked snapshots and suppresses plaintext in
notification previews and floating heads.

## Windows, drafts, and layout

- The main window owns rail, list, and conversation. The composer window is a
  focused conversation surface with draft recovery and close behavior of its own.
- `peppy.layout.v1` is shared by both windows. Read it defensively, clamp at
  render time, and persist only explicit resize or rail moves. Merge only changed
  keys so one window does not erase the other's settings. Never persist a
  viewport-clamped value.
- The composer overlays the conversation: `#message-list` reserves space from
  `--composer-overlay-height`. The visibility observer is rooted at the list with
  a negative bottom `rootMargin`, and is recreated on height change without
  resending seen IDs. A message is read only when actually visible.
- The composer resize handle uses `resize-handle resize-handle-vertical
  composer-resize-grip`, which replaces the default handle classes.

## Floating recipient rail

- For a new conversation, `RecipientPanel` is a separate overlay in
  `#conversation-stage`, not a composer child. The clipped `#recipient-rail-layer`
  ends at the composer's top edge and never covers the input. Default to the left
  edge with a gap above it. Otherwise drag freely; there are no docking grids.
- Keep the panel mounted while moving. Observe the boundary layer too, so
  composer resizing re-clamps without saving.
- Persist exact `{ x, y }` pixels from the conversation top-left as
  `recipientPosition`; accept only finite nonnegative values. Ignore
  `recipientAnchor`. Resize-only clamping and pointer cancellation never save.
- Tokens commit on Enter outside IME, delimiters, blur, and separated paste; typing
  alone does not. Trim and deduplicate in first-occurrence order.
- New entry uses the shared phone helpers: US default, E.164 IDs for new numbers,
  synced IDs unchanged. Validation checks numbering-plan structure, not
  ownership or SMS reachability. Never partially commit a mixed batch. Pending
  text blocks Send until committed or cleared. Chip removal uses stored IDs; an
  explicit empty list clears the draft's recipients.
- Established replies resolve conversation recipients at send time; the new-
  conversation rail must not replace that lookup.

## Contacts view

- Keep each phone's contact book distinct. Never silently merge books or shared numbers.
- Edits are requests to the owning phone: show pending, conflict, approval,
  expired, and unavailable states, never an optimistic OS write. Local forget
  hides a book and never deletes OS contacts.
- Display names and photos never rewrite conversation IDs, draft recipients, or
  notification contents.

## Native vibrancy

The macOS contract spans `window_chrome.rs`, `material_state.rs`, `tray.rs`,
`lib.rs`, and the CSS half in `vibrancy.css`. Every native visual state stays
unverified until the runtime checks in [Native and fixture limits](#native-and-fixture-limits) pass.

- Uses Tauri 2 `set_effects`; no window-vibrancy crate. Requested on macOS only,
  for the main window and non-panel composers. Head windows are untouched.
- `set_effects` reports only dispatch, so success is inspected. macOS is active
  only when the content view contains an `NSVisualEffectView` child. Windows
  material is disabled: every window reports opaque, and the WebView background
  is not mutated. A DWM backdrop attribute is not evidence, and Acrylic is not
  used. Linux and other platforms are never active.
- `material_state.rs` caches results per window label. Apply or clear only on
  explicit transitions: first configuration and effective panel-mode change.
  Resize and page-load hooks republish the cached result without reinstalling.
- Reserve a transition in the cache before native calls. Do not borrow the cache
  during AppKit or DWM work. Record a result only if the window is still in the
  reserved mode.
- A failed effect stays opaque and is not retried on refresh. Closing a window
  calls `forget_material`, so a recreated label retries.
- Main and composer windows start hidden and show after `PageLoadEvent::Finished`, so first paint
  follows the material result. Background starts never show explicitly; a second
  explicit show (tray "Open Peppy") during a pending load does.
- `window_chrome::sync_webview` sets `html[data-vibrancy]` to `active` or `opaque`.
  No attribute means opaque, and browser and fixture hosts never receive it.
  `DesktopSnapshot` is unchanged.
- Transparency: `macos-private-api` belongs on the common `tauri` dependency in
  `Cargo.toml`, since cfg-only dependencies are ignored by manifest validation.
  The base config keeps the main window opaque; the macOS overlay makes it
  transparent. `apply_macos_window` clears the native background on every sync,
  so the opaque CSS fallback is the only guard against a blank shell.
- `macOSPrivateApi` blocks Mac App Store distribution; keep that recorded for any
  store path. Windows material needs validation on Windows 11 and the Windows 10
  fallback before the opaque fallback may be removed.
- CSS (`vibrancy.css`, only under `html[data-vibrancy="active"]`): desk surfaces
  go transparent; the titlebar and rail use `--peppy-surface-native-chrome`;
  `#conversation-stage` stays `--peppy-surface-editor` so text and visibility
  geometry remain opaque. The content list is never blurred. Native material
  applies only to `.theme-system`; the OS supplies its appearance, and the app
  theme only tints CSS.

## Native and fixture limits

- The browser fixture uses realistic data. ` · Simulated` marks a simulated native
  gateway only. The fixture cannot prove banner delivery or activation, and
  preview preferences stay distinct from encrypted-sync state.
- Floating-head behavior needs native input-region and focus integration. Keep
  the main-window and composer fallbacks; do not infer native behavior from browser CSS.
- Browser inspection and builds cover CSS and layout only. Native material needs
  macOS runtime evidence (cold start, bright and dark backgrounds, resizing,
  fullscreen, reduced transparency). Report missing evidence as unverified.
