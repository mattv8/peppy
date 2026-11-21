# Peppy mobile style guide

`packages/mobile-design` generates shared native design outputs. Run
`pnpm --dir packages/mobile-design generate`; CI and reviews use
`pnpm --dir packages/mobile-design check` for drift. Native token values come
from [desktop CSS tokens](../packages/desktop-ui/src/peppy-tokens.css);
[mobile token mappings](../packages/mobile-design/src/tokens.mjs) own the
semantic Kotlin and Swift names.

## Progressive disclosure and plain language

Applies to all Peppy screens: desktop, browser, account website, Android and iOS,
not just Settings.

- Show primary actions, current status and everyday choices first. Put secondary
  controls, detailed diagnostics and uncommon options on clearly named details
  pages or behind disclosure.
- Put an (i) immediately after the relevant label for optional help. Clicking or
  tapping opens a titled modal dialog, scrollable if needed, with explicit
  dismissal. Do not substitute long inline helper paragraphs or hover-only
  tooltips. Preserve an accessible contextual name, keyboard, TalkBack and
  VoiceOver access, and platform touch targets.
- Write for average users: use short familiar words and direct, actionable state
  messages. Prefer concrete user effects over implementation terms such as
  reconciliation, acquisition, provider scan or vault recovery capacity. Keep
  standard terms such as SMS/MMS when clearer than vague alternatives. Do not
  promise connection, progress or completion without state evidence.
- Keep urgent failures, pending approvals and needed next steps visible in root
  summaries. Keep full required permission, privacy, security and consent
  disclosures at the point of decision, not behind (i). Never obscure destructive
  consequences.

## Generated native API

- Android imports `dev.peppy.mobile.ui.theme.PeppyTokens` and chooses
  `PeppyTokens.Dark` or `PeppyTokens.Light` from the system theme.
- iOS uses `PeppyTokens.colors(for: colorScheme)`. Its generated symbols use
  PascalCase semantic names (for example `Accent`, `SurfacePanel`).
- Pair action backgrounds with their semantic foregrounds: Android `accent`
  with `accentText` and `primary` with `primaryText`; iOS `Accent` with
  `AccentText` and `Primary` with `PrimaryText`. Do not assume white text in
  both themes.
- Android copy is `@string/peppy_<catalog_key>`. iOS strings live in the
  **Peppy** table: use `String(localized: "peppy.<catalog_key>", table: "Peppy")`
  or `Text("peppy.<catalog_key>", tableName: "Peppy")`. The default table does
  not find these keys. Wrap computed SwiftUI keys in `LocalizedStringKey`;
  accessibility labels also need a localized value or `Text` from this table.
- Add shared copy only to [the source catalog](../packages/mobile-design/src/catalog.mjs),
  never directly to platform output. Native outputs use its filtered
  `nativeCatalog`; desktop uses `desktopCatalog`. A key present in the shared
  source is not necessarily shipped on mobile: preview/store-only keys are
  intentionally excluded.
- Separate values with semantic native layout, not concatenated dot, bullet,
  pipe, or dash separators. Use distinct `Text` views in stacks for separate
  values; use prose or commas for plain-text-only labels, options, and
  notifications.
- [The canonical mark](../apps/desktop/public/peppy-logo.svg) owns
  Peppy's brand geometry. Do not hand-edit generated logo, launcher, or asset
  output. Binary icons use `pnpm --dir packages/mobile-design icons`, which
  requires desktop Tauri tooling and macOS `sips`.

## Android package names

Android classes named in `AndroidManifest.xml` and WorkManager worker classes stay in package `dev.peppy.mobile`: the OS persists their FQCNs in notification-listener grants and queued work. Put new feature code in feature subpackages.

## Enrollment

- Rust owns enrollment protocol and durable state; native UI presents its
  verified results. Install the opaque Rust identity before reporting native
  secret storage ready. See [domain guidance](AGENTS.md#hosted-enrollment-and-pairing)
  for authentication, handoff, transport, and protected-storage boundaries.
- For phone status, native hosts use only Rust-produced, claimant-authenticated
  same-origin descriptors. Use the canonical enrollment origin for identity;
  the private claim header is not UI data or a general HTTP capability. See
  [Android enrollment host](../apps/android/app/src/main/java/dev/peppy/mobile/enrollment/EnrollmentHost.kt)
  and [iOS enrollment host](../apps/ios/PeppyNative/EnrollmentHost.swift).
- The phone UI scans the `connect_phone` code and shows the SAS for the owner to
  compare. A transfer pairing confirms automatically after owner approval and
  advances only after verified native installation; a shared-secret pairing
  still requires the secret and an explicit submit.
  Camera permission is requested only when the user starts scanning. Contacts
  permission comes from an explicit foreground opt-in, not app startup or a
  background pass. Other OS-permission prompts also follow explicit foreground
  actions; do not add automatic requests, default-SMS role requests, or general
  RCS claims. The Google Messages link may claim one-to-one RCS text chats only.
  Pairing belongs in onboarding or the primary owner flow, not
  Settings, and has no Save QR action.
  See [Android enrollment UI](../apps/android/app/src/main/java/dev/peppy/mobile/enrollment/EnrollmentScreen.kt)
  and [iOS enrollment UI](../apps/ios/PeppyMobile/PhoneEnrollmentView.swift).

### Owner phone approval

- Offer **Add phone** only when the `owner` role's vault keys are unlocked,
  on the Account tab on both platforms, outside the separate Settings
  screen. Hide or disable the entry otherwise. Show the `connect_phone` QR, then
  the locally derived SAS with an owner-specific codes-match control
  (`owner_phone_*` copy, not recipient pairing copy) before **Confirm and
  approve**.
- After approval, retain approved-waiting until the phone connects, even if a
  later status reports expiry; offer neither a replacement nor **Start again**.
  Denied or unapproved-expired states can restart. Invitation rotation and
  cancellation rules are in [domain guidance](AGENTS.md#hosted-enrollment-and-pairing).
- Approval uses the stored Vault key phrase; when it is missing, prompt once with
  the "not your admin password" copy and save only after verification. **Enter
  Vault key phrase on phone** switches the waiting `connect_phone` invitation to
  the shared-secret method, then reveals the Vault key phrase only after device
  authentication, transiently, never in saved UI state, and clears it when the
  app leaves the foreground. This control does not apply to `connect_computer`.
  See [Android owner flow](../apps/android/app/src/main/java/dev/peppy/mobile/approval/OwnerPhoneApprovalScreen.kt)
  and [iOS owner flow](../apps/ios/PeppyMobile/OwnerPhoneApprovalView.swift).

### Computer approval

- Owner phones may scan a `connect_computer` QR from the same owner-only entry
  point as **Add phone**: the `owner` role's vault keys must be unlocked, and the
  control is hidden or disabled otherwise. Never place it in Settings. The
  camera starts only from the explicit scan action. Gateway phones cannot
  approve.
- Pass the scanned payload to Rust, and configure the owner's actual enrolled
  device ID first with `configure_owner_computer_device` from the protected
  credential. Never use a generated or placeholder identifier. Create requests
  come from `owner_computer_create_request`.
- Show the locally derived SAS with the owner's explicit codes-match control,
  then approve with `approve_owner_computer_request`. Approval uses the stored
  Vault key phrase, with the same missing-secret prompt and verification rules
  as `connect_phone`. Nothing here offers **Enter Vault key phrase on phone**.
- After approval, show approved-waiting until the computer completes. Offer
  neither a replacement nor **Start again** for an approved invitation.
  Denied or unapproved-expired states may restart.
- The computer's QR payload necessarily includes its invitation secret. Do not
  describe that secret as absent from the renderer.

### Community and hosted setup

- Community first setup visibly identifies the server-advertised mode. Open
  first claim says that the first device to complete setup becomes owner and
  omits setup-code entry. Token-protected first claim says the operator requires
  a code configured with `PEPPY_SETUP_TOKEN` and shows it. Preserve separate
  setup-code, administrator-password, and vault-shared-secret authorities. Treat
  missing policy conservatively for older servers. Do not offer first-claim
  destructive recovery or reset controls. This does not prohibit the existing
  owner-only **Delete vault** action, which requires exact `ERASE` confirmation. See
  [Android account settings](../apps/android/app/src/main/java/dev/peppy/mobile/gateway/GatewayAccountSettings.kt)
  and [iOS Account](../apps/ios/PeppyMobile/PeppyMobileApp.swift).
- Community owner and family-vault setup store a generated vault secret silently;
  show saved-secret feedback only after storage succeeds. Resume already
  prepared material using its stored secret or request re-entry when that copy
  is missing, never silently replace it. See [protected-storage guidance](AGENTS.md#hosted-enrollment-and-pairing).
- An invite link and display name request a **new vault on the server**, not
  admission to another vault. Show waiting, denied, and expired states distinctly.
  After administrator approval, collect new-vault credentials and create the
  vault. See [Android vault requests](../apps/android/app/src/main/java/dev/peppy/mobile/enrollment/EnrollmentScreen.kt)
  and [iOS vault requests](../apps/ios/PeppyMobile/SelfHostedEnrollmentView.swift).
- Hosted onboarding retains its save acknowledgement and confirmation screens;
  the acknowledgement is a UI step, not a replacement for protected native
  storage. Existing-vault admission and website billing are separate outcomes.
  The phrase step offers an optional **Save to password manager** (Android
  `CreatePasswordRequest`; iOS `webcredentials:peppy.pro`) with its disclosure
  beside the button. A save, like a copy, never counts as acknowledgement and
  never ticks the checkbox; show **Saved** only after the platform reports
  success. See [Android hosted enrollment](../apps/android/app/src/main/java/dev/peppy/mobile/enrollment/HostedEnrollmentScreen.kt)
  and [iOS hosted enrollment](../apps/ios/PeppyMobile/HostedEnrollmentView.swift).

## Contacts

- Contact sync is opt-in; request Contacts permission through explicit foreground
  action, not startup or background work. Offer only writable accounts as
  destinations for new contacts. Approval means pending OS work, not a completed
  write; show uncertain outcomes without implying success or automatic retry.
  See [Android contact settings](../apps/android/app/src/main/java/dev/peppy/mobile/contacts/ContactSyncSettings.kt)
  and [domain contacts guidance](AGENTS.md#contacts).
- iOS **Pause contact sync** leaves the last published book intact. **Retire**
  leaves phone contacts untouched and re-enabling starts a new book. Android's
  **Stop and retire** has the retirement effect; when requested while locked,
  show publication as pending until the next unlock. See
  [iOS contact settings](../apps/ios/PeppyMobile/ContactsSettings.swift) and
  [Android contact host](../apps/android/app/src/main/java/dev/peppy/mobile/contacts/ContactSyncHost.kt).
- iOS selected Contacts access syncs only the selection; excluded contacts are
  not deletions. iOS does not sync notes. Its OS-scheduled updates may be delayed;
  do not promise immediate sync.

## Settings and capability status

- Android uses Mirroring, SMS, Contacts and Account tabs. iOS uses Sync and
  Account; unavailable SMS/MMS and notification mirroring remain status rows,
  never actionable toggles. Settings opens from Account, not a toolbar gear.
  An enrolled but locked Android screen keeps a Settings entry; diagnostics and
  server information are under Settings → Advanced. Owner actions still require
  unlocked keys.
- Feature switches express user intent, not permission grants. Turning a
  sensitive feature on first opens a Peppy consent bottom sheet with the full
  relevant disclosure, a Privacy Policy link and scope, and explicit agree and
  decline actions. Keep the existing SMS/MMS/notification disclosures unchanged.
  A privacy policy supplements this disclosure; it does not replace it.
- Declining, dismissing or leaving a consent flow requests nothing and changes
  nothing. Request OS access only after agreement; enable only after the grant
  and a successful preference readback. Pending enable intent is local to the
  current flow: consume denied results too, and cancel on navigation, lock or
  recreation. A later unrelated Settings grant must not enable a feature.
- Contacts access alone never starts sync. Preserve existing explicit opt-ins,
  but an unset intent requires consent even if OS access was already granted.
  Read access and accepting remote edits are separate choices. Retain the
  off/confirm/automatic edit policy, writable destinations and approval controls.
- SMS capture uses the actual read-access and capture-policy state. If send or
  receive access is incomplete, show that separately rather than hiding active
  capture behind an off switch. An MMS-only grant of shared SMS read access must
  not silently enable SMS capture. Turning SMS sync off requires confirmation
  with its full pending-send/uncertain-outcome caveat.
- Android's battery switch displays the OS battery-exemption state, refreshed
  on foreground. An off-state tap explains the trade-off before opening the
  battery optimization settings list; an on-state tap opens that list directly.
  Announce that the control opens settings. Do not add the restricted direct
  exemption-request permission or promise immediate delivery. Handle unavailable
  OEM settings screens without claiming the exemption changed.
- Android settings backed by the core (mirroring, WiFi-only, skip silent, SMS
  capture, MMS media WiFi-only) load off the main thread; the control is
  disabled until its value arrives and while a write is in flight, then shows
  the value read back. A control that reads the core during composition
  freezes the app whenever a background pass holds the client lock. See
  [domain guidance](AGENTS.md#android-carrier-companion).
- The main screen's startup indicator is centred in the safe drawing area
  (`startup-loading-container`); edge-to-edge is enabled, so a bare padded
  composable lands under the status bar.
- Android notification access, mirroring capture, and vault unlock are separate
  states. **WiFi only** means active WiFi, excluding Ethernet and cellular.
  A queued test notification or MMS history import is not delivery or completion.
  See [notification settings](../apps/android/app/src/main/java/dev/peppy/mobile/notifications/NotificationMirroringSettings.kt)
  and [MMS settings](../apps/android/app/src/main/java/dev/peppy/mobile/mms/MmsSettings.kt).
- Android own-number access is optional and per SIM route, for excluding this
  phone from group-MMS replies. Retain manual entry when access or detection is
  unavailable. See [own-number settings](../apps/android/app/src/main/java/dev/peppy/mobile/telephony/OwnNumberSettings.kt).
- iOS carrier, SMS/MMS, and notification-mirroring unavailable rows are status,
  not actionable toggles. Wake relay registration is explicit opt-in; hints may
  be delayed or missing and do not guarantee timely sync. See
  [iOS capability status](../apps/ios/PeppyMobile/PeppyMobileApp.swift) and
  [wake protocol guidance](AGENTS.md#notifications).

## Android settings pages

These rules cover the Android Account tab and every page reached from it:
Settings, Advanced, Devices, Storage, Subscription, About and licenses. Other
Android tabs adopt them when they next change. iOS adopts them when its Account
tab next changes.

- Pages below a tab root use the top app bar through `DetailPage`: back arrow,
  title and optional icon actions. Do not add an in-page back button or a
  second title. The bottom navigation hides while a detail page is shown.
- Rows sit in titled groups (`SettingsGroup`). Groups are flat: a divider
  before every group except the first, then a `titleMedium` heading with
  heading semantics. Only the danger zone is a card, with a 1dp error border,
  error-surface fill and 8dp corners.
- Row types:
  - Navigation: trailing chevron.
  - External link: trailing open-in-new icon, announced as "Opens in your
    browser".
  - Toggle (`SettingToggleRow`): the whole row toggles.
  - Status (`DetailsStatusRow`): label left, value right.
  - Action: compact button at the row end.
  - Destructive: consequence, then a compact error-filled button below it.
- Rows are at least 48dp tall, or 56dp with a summary line. They have no
  horizontal inset, so they line up with group headings. Navigation, link and
  toggle rows clip ripples to 8dp corners. Give every row in a group a leading
  icon, or none.
- Put optional help in an `InfoButton` (i) right after the label. The (i) is
  outside the row's clickable area or is its own focusable node. Navigation and
  link rows keep it outside the clickable region. Toggle rows keep it inside the
  toggle row as a separate node. Either way TalkBack and switch access reach
  both. Inline text is at most one short line of current state.
- Consent, permission, privacy, security and destructive text stays inline at
  the point of decision, unchanged. The SMS, MMS, notification and
  account-deletion consent sheets keep their exact text.
- Summaries state a fact, not an explanation: "Registered", "4 active".
- Use at most two levels below a tab root. Deeper pages are for reference
  reading only, such as license text.
- Technical and rarely used controls go on one Advanced page.
- Separate values with badges or rows, never "·" or other joined separators.
- Allow one filled primary button per page. Use outlined buttons for secondary
  actions, compact buttons inside rows, and the error fill for destructive
  actions. Coral is never body text. Text actions on warning surfaces use the
  warning text color.
- A state that needs action shows a warning badge on its root row and a callout
  on its page. Show each message once: the read-only state is reported by the
  system ribbon, so Account adds only the plan-row badge.
- Destructive buttons take their row label as the accessible name ("Delete
  vault", "Delete Peppy account"), so two Delete buttons stay distinct. The
  label and consequence form one TalkBack stop.
- Rows switch to a stacked layout when their content is narrower than 312dp
  (phones under about 360dp) or the font scale is above 1.15. Stacked status
  rows put the value under the label. Action rows put the label (and its (i))
  first, then the button below, start-aligned with the label. Callouts put the
  action under the text, end-aligned. Navigation rows put the attention badge
  under the label.
- Destructive rows always stack: label, then consequence, then the button below,
  start-aligned.
- Page-level errors use one style on every page: body-medium text in the error
  color, announced politely as a live region. Field errors sit on the field,
  with the error state and supporting text.
- Device facts (platform, role, connection, this device) are badges on one
  line.
  - Order: badges are ordered by priority: This device, then a warning such as
    Sending paused, then presence, then role. A warning is shown before facts
    when space allows.
  - Overflow: badges that don't fit collapse into a trailing `+N` badge. The
    device name ellipsizes. Badge text is never cut, and nothing scrolls
    sideways.
  - Accessibility: the platform icon and text form one TalkBack stop whose
    description lists every badge, including those collapsed into `+N`. The ⋮
    button is a second stop.
- Connection labels match desktop: Connected, Last seen …, Never connected,
  Status unknown. Row actions (Rename, Remove, Disconnect) sit in an overflow
  menu.
- Account tab layout: a Devices group; a Plan and storage group (hosted only);
  an App group (Settings, Guides, About). Add phone and Connect computer are
  Account-tab only and gated as described in [Enrollment](#enrollment).
  Settings has a Sync group, an Advanced row and a Danger zone; when locked, a
  Plan group replaces Sync and the Danger zone has only account deletion.
  Advanced has Connection, Custom relay (self-hosted) and Diagnostics.
- Build pages from the shared pieces. `DetailPage`, `PageChrome` and
  `PageAction` are in [PageChrome.kt](../apps/android/app/src/main/java/dev/peppy/mobile/ui/PageChrome.kt).
  `SettingsGroup`, `NavigationRow`, `ExternalLinkRow`, `ActionRow`,
  `DestructiveRow`, `AttentionCallout`, `PeppyBadge` and `SingleLineBadges` are
  in [SettingsRows.kt](../apps/android/app/src/main/java/dev/peppy/mobile/ui/SettingsRows.kt).
  Toggles use [SettingToggleRow.kt](../apps/android/app/src/main/java/dev/peppy/mobile/ui/SettingToggleRow.kt),
  status pairs use `DetailsStatusRow` in
  [DetailsPage.kt](../apps/android/app/src/main/java/dev/peppy/mobile/ui/DetailsPage.kt),
  and help uses [InfoButton.kt](../apps/android/app/src/main/java/dev/peppy/mobile/ui/InfoButton.kt).
- Every user-facing string comes from the shared catalog
  (`packages/mobile-design/src/catalog.mjs`, used as `peppy_<key>`). No
  hard-coded English.
- Give every page, group, row and dialog a stable, unique test tag.

## Native presentation

The mobile design uses native typography and touch targets with a 4px rhythm,
not desktop control dimensions. Use the coral graphic brand and semantic coral
accents; coral is not normal-size text. Follow system light/dark appearance and
retain opaque readable content when reduced transparency or motion is enabled.

Feature screens use a dark neutral brand band with a static, decorative coral
phone-to-computer illustration and a short data-flow summary, followed by plain
setting rows. Pair hero text with the dark scheme's text tokens even in light
mode; other surfaces follow the system theme. Use real account and sync facts,
not sample names or counters. Technical status, pending approvals and recovery
actions remain reachable through detail screens, with a visible root summary
when attention is needed.

Consent sheets are distinct from system prompts. Full disclosures scroll without
truncation, and agree/decline actions remain reachable at large text sizes. Use
native typography and at least 48dp Android / 44pt iOS touch targets. A toggle row
has one accessible switch action, not nested competing toggle targets. Keep
coral as graphic emphasis or a paired action fill, not normal-size body text.

## Composer status and route badges

These match the desktop composer; keep them consistent across platforms.

- Show phone and SIM route facts as separate filled badges: one badge per value
  (for example `SIM 1` and `Online`). Do not use outlined pills or join the values
  with a separator. Use a neutral raised surface fill with a small corner radius.
  Only the state badge takes a semantic availability color, and its text still
  states the meaning, so color is never the only signal.
- Show every composer status on the composer action row, directly after the route
  badges, on one line. Draft statuses (saving, loading, save failures, sync
  review, moved or replaced drafts, send failures) come first, then route and
  sending blockers, group refusals and progress. Never add a second status row
  above the action row.
- When the selected route cannot send to this chat, the state badge says so (for
  example `RCS unconfirmed` or `Relink needed`) instead of `Online`, so the badge
  never contradicts the status message.
- Write each status as one sentence: what is wrong and how to fix it. Do not
  prefix it with "Sending unavailable". Use one color per tone everywhere: error
  for a send that was attempted and did not go out, warning for a blocker or an
  uncertain outcome, secondary text for progress. A small tone icon goes with
  the color.
- When several statuses apply, show the most important one with a `+N` count.
  Hover, tap or keyboard activation opens an anchored popover above the action
  row with the full text, details and actions for every status.
- Keep draft status short and direct; for example, use **Error saving draft.**
  with **Retry**, not a sentence that explains internal save state. When space
  runs out, truncate the message with an ellipsis. Action buttons, badges and
  Send never shrink or wrap. Optional hints such as keyboard shortcuts hide
  first. Accessibility labels and announcements carry the full message.
- Show brief pending states such as **Saving…** only after a short delay, so
  quick saves do not flash. Never stack two messages that report the same
  failure.
- When a confirmation needs more than one line, such as discarding local
  changes, show it in an anchored popover or sheet above the action row. Do not
  grow the row. The popover keeps the full destructive consequence and explicit
  Cancel and confirm actions.
