# Peppy desktop frontend style guide

Use this guide for the Tauri desktop UI: a dense, bounded workbench with
Pushbullet-style placement and Ragtime Modern as visual intent. It documents
UI contracts as well as presentation; behavior and source take precedence over
this guide. [Domain guidance](AGENTS.md) covers security, data, and evidence.

## Source ownership

Resolve a discrepancy from the implementation and its tests first, then the
token and style sources, then this guide. Ragtime is visual intent only; do
not import from it.

| Source | Owns |
|---|---|
| `packages/desktop-ui/src/peppy-tokens.css` | `--peppy-*` theme tokens, local Droid Sans, reduced motion |
| `packages/desktop-ui/src/styles.css` | Shared titlebar, panes, composer, resize, and status-popover styling |
| `packages/desktop-ui/src/index.tsx` | `AppTitlebar`, `Panel`, `StatusPopover`, `ConnectionDot`, shared desktop UI, composer, recipient panel |
| `packages/desktop-ui/src/phone.ts` | Shared phone validation, normalization, input and display formatting |
| `packages/desktop-ui/src/ResizeHandle.tsx` | Resizing behavior and handle semantics |
| `apps/desktop/src/App.tsx` | Shells, views, drafts, persistence, status details, and status severity bindings |
| `apps/desktop/src/status.ts` | Desktop-domain connection/sync labels, summaries, and severity priority |
| `apps/desktop/src/app.css` | Desktop shell, conversation view, bubbles, and app-shell chrome |
| `apps/desktop/src/settings.css` | Settings categories, cards, and disclosure styling |
| `apps/desktop/src/vibrancy.css` | Native material overrides under `html[data-vibrancy="active"]`, imported by `App.tsx` |
| `apps/desktop/src-tauri/src/window_chrome.rs`, `material_state.rs`, `tray.rs` | Native effect lifecycle, success inspection, deferred show, and `data-vibrancy` reporting |
| `apps/desktop/src-tauri/tauri.macos.conf.json` | macOS-only overlay making the main window transparent; the base `tauri.conf.json` keeps it opaque |

## Distinctive design and themes

Keep the rail, thread list, and conversation as docked workbench panes. Use
the compact 4px rhythm, tonal surfaces, and structural borders rather than a
generic web-page layout. The composer and contacts rail are deliberate
exceptions: both float above the conversation as Tier 3 glass surfaces.

All themeable color, surface, border, typography, spacing, radius, and shadow
values use explicit `--peppy-*` tokens. Shells carry `theme-system`,
`theme-light`, or `theme-dark`; light values intentionally appear in both
`.theme-light` and the system-light media rule. Legacy aliases in `styles.css`
are compatibility mappings, not new-code tokens.

The palette stays VS Code Modern in dark and light. Keep the compact rhythm,
custom in-window chrome, coral accent, and locally bundled Droid Sans.

### Surface tiers

| Tier | Surfaces | Treatment |
|---|---|---|
| 1. Desk | Window background behind all panes | Solid or subtle tonal fill. Translucent native desk only on macOS, when native success reports `data-vibrancy=active` and the shell uses the system theme. |
| 2. Docked | Rail, titlebar, thread list, conversation, contacts and settings panes | Structural borders and full legibility. Raised settings cards use opaque backgrounds, `--peppy-radius-md`, and `--peppy-shadow-2`, not blur. |
| 3. Floating | Composer, recipient rail, popovers, pickers, dialogs | Glass: resolved `--peppy-glass-surface` and `--peppy-glass-backdrop-filter` on a small area, plus `--peppy-glass-border`, `--peppy-glass-highlight`, and `--peppy-shadow-floating`. |

Never blur a full scrolling list. Blur belongs only on small floating areas.
Preserve the composer's opaque gutters and fade, the read-visibility observer
geometry, draggable recipient-rail boundaries, pane resizing, and saved layouts
when restyling any tier.

### Tokens

Define these in `peppy-tokens.css`, with matching values in `.theme-light` and
the system-light rule. Dark values are listed first.

| Token | Dark | Light |
|---|---|---|
| `--peppy-glass-tint` | `rgba(30,30,32,.78)` | `rgba(248,248,250,.82)` |
| `--peppy-glass-surface` | resolved tint or opaque fallback | resolved tint or opaque fallback |
| `--peppy-glass-backdrop-filter` | resolved blur or `none` | resolved blur or `none` |
| `--peppy-glass-border` | `rgba(255,255,255,.10)` | `rgba(0,0,0,.08)` |
| `--peppy-glass-highlight` | `rgba(255,255,255,.06)` | `rgba(255,255,255,.90)` |
| `--peppy-glass-blur` | `12px` | `12px` |
| `--peppy-shadow-2` | `0 1px 4px rgba(0,0,0,.18)` | `0 1px 4px rgba(0,0,0,.06)` |
| `--peppy-shadow-3` | `0 4px 20px rgba(0,0,0,.30),0 1px 4px rgba(0,0,0,.18)` | same form, alphas `.12` and `.08` |
| `--peppy-shadow-floating` | alias of `--peppy-shadow-3` | alias of `--peppy-shadow-3` |
| `--peppy-backdrop` | `rgba(0,0,0,.40)` | `rgba(0,0,0,.25)` |
| `--peppy-danger-hover` | `#c42b1c` | `#c42b1c` |

`--peppy-danger-text` keeps close-control text legible over the danger hover.

`--peppy-surface-native-chrome` is defined once under
`:where(.theme-system, .theme-light, .theme-dark)` as
`color-mix(in srgb, var(--peppy-surface-chrome) 82%, transparent)`. It resolves
per theme, so the native-chrome tint is 82% of the selected theme's chrome
surface (`#181818` dark, `#f8f8f8` light). Use it only under
`html[data-vibrancy="active"]`.

Shared, theme-independent tokens:

- Radius: `--peppy-radius-sm` 4px, `--peppy-radius-md` 8px,
  `--peppy-radius-lg` 12px, `--peppy-radius-pill` 999px.
- Duration: `--peppy-duration-fast` 80ms, `--peppy-duration-base` 150ms,
  `--peppy-duration-slow` 250ms.
- Easing: `--peppy-ease-out` `cubic-bezier(0,0,.2,1)`, and
  `--peppy-ease-in-out` `cubic-bezier(.4,0,.2,1)`.

Use these tokens instead of literals in touched styling. Do not add spring or
bounce animation, and do not animate badges for decoration.

### Fallbacks, readability, and motion

Glass degrades to opaque surfaces with zero blur in these cases:

- native-only `:where(.theme-system, .theme-light, .theme-dark)[data-bridge-mode="native"][data-platform="linux"]`;
- `prefers-reduced-transparency`;
- unsupported `backdrop-filter`;
- native vibrancy failure or inactive state.

Forced colors use system colors and no shadows. Reduced motion disables
animations and transitions. Scope overrides to the shell element that carries
the theme class. Do not write selectors that descend from `:root` in ways that
are invalid for the document.

Opaque fallback must stay readable. A native fallback must never leave a
transparent, blank shell. Web keeps supported in-page CSS glass, but it never
uses native vibrancy.

Bundle fonts and assets locally with relative URLs. The CSP remains
`default-src 'self'`; do not introduce remote font or asset sources. Treat
display timestamps as opaque strings rather than values to parse or reformat.

## Titlebar status and chrome

The main window supplies a `Panel.status` with the sync state, carrier
disclosure, and connection state. `StatusPopover` exposes those details from
the navigation-rail dot; keep the detail rows readable and wrapped. The main
titlebar has no status pills. Composer and floating-head headers use only a
read-only `ConnectionDot` with the complete `Connection: <connectionText>`
label; they do not show sync or carrier disclosures.

Keep this fixed text exactly: `Device sync encrypted`, `Device sync key
mismatch`, `Device sync not unlocked`, and `Carrier SMS/MMS not end-to-end
encrypted`. Do not truncate these disclosure rows.

The app draws platform-appropriate chrome so both windows share a recognizable
shell. Do not switch to native decorations: main-window and composer close
paths use `closeAfterSave`. A draft flush failure keeps the window open.

## Icons and placement

Use Lucide icons only. Consumers import them through `desktop-ui` re-exports;
do not add a direct Lucide dependency to an app package. Sizes are fixed:

| Context | Size |
|---|---|
| Navigation rail | 20px |
| Toolbars and titlebar | 16px |
| Inline text | 14px |
| Chips | 12px |

Use a consistent 1.75 stroke. The exception is native-control fidelity, where
the platform's conventions take priority. Keep the window control conventions
of each platform; do not use Maximize2 where the platform convention differs.
Text collapse uses Minus. Lucide equivalents replace the Remix icons for close,
minimize, maximize, panel toggle, and compose. In-place compose uses
`MessageSquarePlus`; the detached `New message window` keeps `SquarePen`.

Compose has one visible in-place action per window state:

- side-rail mode: the thread-list toolbar;
- titlebar mode: the titlebar.

When the thread list is hidden or collapsed, or another view is selected, the
titlebar supplies the compose action. `AppTitlebar.onNewMessage` is the
contract: pass it in titlebar mode, or when the list is inaccessible, and leave
it `undefined` otherwise. The detached-composer action is a separate header
action labeled `New message window`. Header and per-row popouts keep their own
targets.

Keep existing `aria-label`, `id`, and `data-*` hooks where behavior permits.
Hover-only controls also appear on keyboard focus and on coarse pointers.

## Settings and progressive disclosure

Settings use a 160px category navigation beside one scrolling column of content
cards. Scroll position selects the current category: the navigation links use
anchors and `aria-current`, not tab roles, because several sections can be
visible at once. A category without rendered content does not appear in the
navigation.

Category order is:

1. Server
2. Sync encryption
3. Devices and pairing
4. Notifications
5. Appearance
6. Startup, if supported
7. Account, if available
8. Advanced, always last

Below 560px, the navigation becomes a selector rather than a sidebar. Keep the
existing `data-settings-section` values.

Each card shows its summary. Common controls stay directly usable. Secondary
controls sit behind disclosure. The existing Change server disclosure stays in
Server. There is no settings search. Do not add settings QR, QR-save,
credential-file onboarding, or credential export controls.

Rules for disclosure:

- A notification-settings entry point opens and scrolls to its card and
  preserves focus.
- Async failures remain visible in the card.
- Pending drafts and in-flight actions survive a disclosure toggle.
- Contact-form optional groups (nickname, organization, title, address,
  birthday, notes) start collapsed when empty and open when populated.
- Capability and read-only states remain visible.
- On submit, reveal validation errors before focusing a hidden field. Known host
  errors focus only their matching field (for example, `The birthday is invalid.`);
  ambiguous edit errors reveal populated groups without moving focus.
- Setup follows the discovered deployment mode and enrollment state. Do not
  route a community browser into hosted identity sign-in because an outer UI
  preference defaults to Hosted.
- Composer and notifications keep their existing progressive behavior. Do not
  add speculative features.

### Enrollment UI

- The phone step shows a transfer `connect_phone` QR as soon as an owner has no
  phone, after first setup or from **Add phone**. It replaces the QR only near
  or after expiry, and never once a phone has claimed it; the server cancels
  claimed invitations unconditionally, so frequent rotation can break a phone
  mid-claim. Secondary actions are **Enter shared secret on phone**, which on
  desktop opens the native secret reveal, and **Set up phone later**, which
  cancels only the invitation and is remembered per origin. After the phone
  claims, show the SAS and require the owner's explicit SAS checkbox before
  approval. The QR is not a credential or approval and has no Save action. Keep
  the stable stage and QR hooks in [SetupLanding](../apps/desktop/src/SetupLanding.tsx).
- Native desktop requests the setup code only when discovery reports
  token-protected first claim, then requests the admin password in a native
  secure prompt. It generates the vault shared secret silently, keeps it in
  protected secure storage, and shows it only in a native dialog with **Copy
  secret** (from Settings or the shared-secret phone option). The secret never
  enters the webview. Setup copy says where the secret is kept. **Confirm and
  approve** uses the stored secret without prompting; when none is stored (for
  example a vault created before secure storage), it asks once in a native
  **Approve phone** prompt that says this is not the admin password, verifies
  the entry against the vault header, and saves it for later approvals. The browser
  still shows its generated secret once and requires a save acknowledgement.
  App-lock credentials remain a
  separate native gate. Browser-hosted secret prompts are DOM controls by design:
  they remain accessible to same-origin JavaScript and must not be described as
  native secure controls. Keep their values outside shared React snapshots and
  clear their fields after use. See
  [native enrollment](../apps/desktop/src-tauri/src/enrollment.rs),
  [native app lock](../apps/desktop/src-tauri/src/app_lock.rs), and
  [browser bridge](../apps/web/src/browser-bridge.ts).
- Community first setup visibly identifies the server-advertised mode. Open
  first claim shows no setup-code input and no extra explanation of claim
  ownership. Token-protected first claim says that the
  operator requires a code configured with `PEPPY_SETUP_TOKEN` and shows that
  input. Preserve the distinct setup-code, administrator-password, and vault
  shared-secret authorities. Missing policy remains conservative for older
  servers; do not imply recovery or a reset control in the UI.
- Default optional local lock settings to off. A lock must protect local UI
  access only; it does not replace account authorization, device credentials,
  or the vault shared secret. Enabling applies on the next launch. Enforce the
  lock in native command dispatch, sanitize snapshots, and suppress plaintext
  notification/head presentation; a React overlay is not a security gate.
- The web-only **Manage server** page uses authenticated community management
  routes for device status/revocation and read-only storage/quota/retention
  values. It contains no pairing QR or destructive database-reset controls.

## Windows, drafts, and layout

The main window owns rail, list, and conversation views; the composer window
is a focused conversation surface without rail or thread list. The composer
must preserve draft recovery and close behavior rather than acting as a
second main window.

`peppy.layout.v1` is shared by both windows. Read it defensively, clamp
values while rendering, and persist only explicit user resize or rail position
changes. Merge only changed keys before writing so one window does not erase
the other's settings; never write back a value merely clamped for its current
viewport.

The composer overlays the conversation: `#message-list` reserves space from
`--composer-overlay-height`, while opaque gutters and its fade preserve legible
content beneath it. The visibility observer is rooted at that list and uses a
rounded negative bottom `rootMargin` for the overlay, recreating on height
changes without re-sending already seen message ids. Messages become read only
when actually visible.

The composer resize handle uses the supplied
`resize-handle resize-handle-vertical composer-resize-grip` `className`; that
class replaces default handle classes. Its card-top grip sizes the composer
without turning it into the list sash.

## Floating contacts rail

For a new conversation, `RecipientPanel` is a separate overlay inside
`#conversation-stage`, not a child of the composer. Its clipped
`#recipient-rail-layer` ends at the composer's top edge: downward movement
stops with the rail's bottom flush to that edge, never over the message input.
Default to the left edge with an 8px gap above that boundary. Drag freely in
the remaining conversation area; there are no corner anchors, docking rows,
or drop-target grids. Keep the panel mounted while moving so pending input
and focus survive.

Size the rail to its contents; its input grows and shrinks with its text and
placeholder. Wrap chips within the available width and scroll inside the rail
when its content exceeds the safe area's height. Observe the boundary layer
as well as the rail so composer resizing or validation errors re-clamp the
rail without saving window-only adjustments. The composer stacks above the
rail layer as a final protection against transient measurement changes.
Hide the composer's top fade while the rail is present so it does not dim the
rail's controls at the flush boundary; keep the fade in existing threads.

Persist exact `{ x, y }` pixels relative to the conversation area's top-left
as `recipientPosition` in the shared layout. Accept only finite nonnegative
coordinates. Ignore legacy `recipientAnchor`; remove it on the next explicit
rail move. Resize-only clamping never writes back the saved position. Pointer
cancellation reverts without saving. Grip arrows move 8px (Shift: 32px), with
no quantization of pointer movement. Keep the rail rounded, above the composer
in stacking order, and use `--peppy-shadow-floating` in both themes.

Recipient tokenization commits on Enter outside IME, delimiters, blur, and
separated paste. It trims and deduplicates committed tokens in first-occurrence
order, but typing alone does not commit; a paste's unseparated trailing text
remains in the input. Empty-input Backspace removes the last token.

Use the shared phone helpers for new recipient entry. Parsing defaults to US;
explicit international numbers remain supported. New numbers normalize to
E.164 IDs (for example, `+12025550123`) while US display uses national format
(`(202) 555-0123`). Keep existing synced IDs and contact names unchanged.
Validation checks numbering-plan structure, not ownership or SMS reachability.

Format the rail input while typing without changing IME composition or losing
the caret. Validate on commit, retain rejected text with an inline accessible
error, and never partially commit a mixed valid/invalid batch. Search remains
free text for existing contacts; only valid numbers offer a new conversation.
Pending recipient text blocks Send until committed or cleared. Chip removal
uses stored IDs; an explicit empty recipient list clears a saved draft rather
than restoring its previous recipients. Established replies still resolve
conversation recipients at send time.

## Contacts view

Keep contact books, contact search/list and contact detail as bounded workbench
panes. Use the existing theme tokens, controls and resize behavior. Each phone's
book remains identifiable; do not silently merge books or shared phone numbers.

Contact edits are requests to the owning phone. Show pending, conflict, approval,
expired and unavailable states without presenting an optimistic local edit as an
OS write that succeeded. Include the source account/container and read-only
field capabilities. Local forgetting hides a book; it never deletes OS contacts.

Resolve names and avatars for display without rewriting conversation IDs,
addresses, draft recipients or mirrored notification contents. Honor native
banner preview settings. Preserve a number-selection step for contacts with
multiple phone numbers and an initials fallback when a photo is unavailable.

Use stable contact/book identifiers for repeated component hooks. Keep photo
cropping keyboard-accessible and maintain focus through async save states.

## Native vibrancy

The macOS native contract below is implemented in `window_chrome.rs`,
`material_state.rs`, `tray.rs`, and `lib.rs`, with `vibrancy.css` as its CSS
half. Windows native material is disabled until validated (see below). Treat
every native visual state as unverified until the runtime checks in "Native and
fixture limits" pass.

Mechanism and scope:

- Native material uses Tauri 2's built-in `set_effects` API. No separate
  window-vibrancy crate is used.
- Material is requested on macOS only, for the main window and normal (non-panel)
  composers. Native head windows (bubble HWNDs and NSWindows) are not touched.

Success is inspected, not assumed. `set_effects` reports only dispatch success,
so the implementation checks the installed result:

- macOS: material is active only when the content view contains an
  `NSVisualEffectView` child (`UnderWindowBackground`, following window active
  state).
- Windows: native material is disabled. Every Windows window reports opaque, and
  the code does not mutate the WebView background. A readable DWM system-backdrop
  attribute is not evidence of a visible backdrop, and transparent WebView2 has
  not been validated. Acrylic is not used.
- Linux and other platforms: never active.

Lifecycle:

- `material_state.rs` caches the result per window label. Effects are applied or
  cleared only on explicit mode transitions: the first configuration of a window,
  and a change of effective panel mode (normal to head panel or back). Resize and
  page-load hooks republish the cached result and never reinstall the effect.
- A transition is reserved in the cache before native calls run, and the cache
  is not borrowed while AppKit or DWM work is in progress. The result is recorded
  only if the window is still in the reserved mode, so a stale result from an
  earlier transition is ignored.
- A failed effect keeps the window opaque and is not retried on refresh. Closing
  a window drops its cached state (`forget_material`), so a recreated window with
  the same label retries.
- The main window and every composer are created or left hidden
  (`visible: false`) and shown only after that webview's `PageLoadEvent::Finished`
  (`show_when_ready` and `page_loaded`), so the first paint follows the material
  result. Background starts never send an explicit show request and stay hidden
  after page load until an explicit show request.
- A second explicit show request (for example, tray "Open Peppy") while the first
  page load is still pending shows the window. This recovers from a navigation
  that never emits `Finished`. A late `Finished` for that window does not refocus
  it.
- Close-failure recovery in `lib.rs` re-shows a window the user already opened;
  it does not bypass the deferred first show.

Reporting:

- `window_chrome::sync_webview` sets `document.documentElement.dataset.vibrancy`
  to `active` or `opaque`, alongside the existing `windowRounded` value. It runs
  on each sync, on resize, and after page load. Head-panel composers always
  report `opaque`. The webview selects the shell with `html[data-vibrancy]`.
  The `DesktopSnapshot` contract is unchanged.
- No attribute means opaque. Browser and fixture hosts never receive it, so they
  keep the opaque desk and in-page CSS glass.

Transparency and platform rules:

- The `macos-private-api` feature is on the common `tauri` dependency in
  `Cargo.toml`, not a macOS-only target dependency. Tauri's build-time manifest
  feature validation ignores cfg-only dependencies, so the feature must be
  declared there.
- The base `tauri.conf.json` sets `app.macOSPrivateApi: true`. Its main window
  stays `transparent: false`, so Windows and Linux windows remain opaque.
- `tauri.macos.conf.json` is a macOS-only overlay. It sets only the main
  window's `transparent: true`. macOS composers are created with
  `transparent(true)`.
- On macOS, `apply_macos_window` clears the native window background on every
  sync, whether or not the material succeeded. The opaque CSS fallback is
  therefore the only guard against a blank macOS shell after an effect fails.
- Mac App Store blocker: `macOSPrivateApi` is a direct-distribution-only setting
  and blocks Mac App Store distribution. This limitation stays recorded for any
  store path.
- Windows has no native material and no WebView background change. Its window
  rounding uses only the DWM corner preference, which is not material. Linux stays
  opaque.
- Future Windows enablement is blocked until it is validated on Windows 11 and
  on the Windows 10 fallback. That validation must cover a transparent WebView2,
  a visible Mica backdrop, resizing, and the inactive state. Only after it passes
  may the opaque fallback be removed.

CSS contract (`vibrancy.css`, applied only under `html[data-vibrancy="active"]`):

- The desk surfaces go transparent so the material shows: `#desktop-shell`, and
  the non-panel composer shell and its conversation pane.
- The titlebar and navigation rail use `--peppy-surface-native-chrome`, so the
  material shows through the chrome and rail only.
- The composer's `#conversation-stage` keeps `--peppy-surface-editor`, so message
  text, the composer's opaque gutters, and the read-visibility geometry stay
  opaque.
- Under `prefers-reduced-transparency` or `forced-colors`, the desk and chrome
  fall back to the solid `--peppy-surface-desk` and `--peppy-surface-chrome`.
- Other tiers keep their opaque or CSS-glass treatment. The content list is never
  blurred.

Theme and OS appearance:

- Native material is shown only for the system-theme shell (`.theme-system`).
  The CSS selects that shell, and the native-chrome token applies only there.
  Explicit light and dark shells keep opaque native-surface CSS, so the installed
  effect is covered. CSS glass on floating layers still applies in every theme.
- The OS supplies the material's appearance (macOS `UnderWindowBackground`). The
  app's theme is not pushed to native, so the selected theme controls only the
  CSS tint. Do not describe this as native app-theme propagation.

Fullscreen and maximized (macOS):

- Fullscreen and maximized windows keep a successfully applied material and use
  the existing square-corner policy.

## Native and fixture limits

The browser fixture uses realistic data, while ` · Simulated` identifies a
simulated native gateway only. Native banners depend on OS permission and app
installation, so the fixture cannot prove delivery or banner activation;
notification preview preferences remain distinct from encrypted-sync state.

Floating-head behavior needs the native input-region and focus integration;
keep the main-window and composer fallbacks. Do not infer native-host behavior
from browser CSS alone.

Browser inspection and builds cover CSS and layout only. They do not prove
native material: macOS cold start, bright and dark backgrounds, resizing,
fullscreen, reduced transparency, and explicit light and dark shells require
runtime evidence on macOS. Windows native material is disabled, so its
transparent WebView2 and Mica behavior needs Windows runtime validation before
any enablement. Report missing runtime evidence as unverified; do not treat it as a
pass.
