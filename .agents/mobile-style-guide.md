# Peppy mobile style guide

`packages/mobile-design` generates shared native design outputs. Run
`pnpm --dir packages/mobile-design generate`; CI and reviews use
`pnpm --dir packages/mobile-design check` for drift. Native token values come
from [desktop CSS tokens](../packages/desktop-ui/src/peppy-tokens.css);
[mobile token mappings](../packages/mobile-design/src/tokens.mjs) own the
semantic Kotlin and Swift names.

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
- [The canonical mark](../apps/desktop/public/peppy-logo.svg) owns
  Peppy's brand geometry. Do not hand-edit generated logo, launcher, or asset
  output. Binary icons use `pnpm --dir packages/mobile-design icons`, which
  requires desktop Tauri tooling and macOS `sips`.

## Enrollment

- Rust owns enrollment protocol and durable state; native UI presents its
  verified results. Install the opaque Rust identity before reporting native
  secret storage ready. See [domain guidance](AGENTS.md#hosted-enrollment-and-pairing)
  for authentication, handoff, transport, and protected-storage boundaries.
- For phone status, native hosts use only Rust-produced, claimant-authenticated
  same-origin descriptors. Use the canonical enrollment origin for identity;
  the private claim header is not UI data or a general HTTP capability. See
  [Android enrollment host](../apps/android/app/src/main/java/dev/peppy/mobile/EnrollmentHost.kt)
  and [iOS enrollment host](../apps/ios/PeppyNative/EnrollmentHost.swift).
- The phone UI scans one `connect_phone` code and shows the SAS for the owner to
  compare. A transfer pairing confirms automatically after owner approval and
  advances only after verified native installation; a shared-secret pairing
  still requires the secret and an explicit submit.
  Camera permission is requested only when the user starts scanning. Contacts
  permission comes from an explicit foreground opt-in, not app startup or a
  background pass. Other OS-permission prompts also follow explicit foreground
  actions; do not add automatic requests, default-SMS role requests, or general
  RCS claims. Pairing belongs in onboarding or the primary owner flow, not
  Settings, and has no Save QR action.
  See [Android enrollment UI](../apps/android/app/src/main/java/dev/peppy/mobile/EnrollmentScreen.kt)
  and [iOS enrollment UI](../apps/ios/PeppyMobile/PhoneEnrollmentView.swift).

### Owner phone approval

- Offer **Add phone** only when the `owner` role's vault keys are unlocked,
  on the Account tab on both platforms, outside the separate Settings
  screen. Hide or disable the entry otherwise. Show the QR, then the locally
  derived SAS with an owner-specific codes-match control (`owner_phone_*` copy,
  not recipient pairing copy) before **Confirm and approve**.
- After approval, retain approved-waiting until the phone connects, even if a
  later status reports expiry; offer neither a replacement nor **Start again**.
  Denied or unapproved-expired states can restart. Invitation rotation and
  cancellation rules are in [domain guidance](AGENTS.md#hosted-enrollment-and-pairing).
- Approval uses the stored vault secret; when it is missing, prompt once with
  the "not your admin password" copy and save only after verification. **Enter
  shared secret on phone** switches the waiting invitation to the shared-secret
  method, then reveals the secret only after device authentication, transiently,
  never in saved UI state, and clears it when the app leaves the foreground. See
  [Android owner flow](../apps/android/app/src/main/java/dev/peppy/mobile/OwnerPhoneApprovalScreen.kt)
  and [iOS owner flow](../apps/ios/PeppyMobile/OwnerPhoneApprovalView.swift).

### Community and hosted setup

- Community first setup visibly identifies the server-advertised mode. Open
  first claim says that the first device to complete setup becomes owner and
  omits setup-code entry. Token-protected first claim says the operator requires
  a code configured with `PEPPY_SETUP_TOKEN` and shows it. Preserve separate
  setup-code, administrator-password, and vault-shared-secret authorities. Treat
  missing policy conservatively for older servers. Do not offer first-claim
  destructive recovery or reset controls. This does not prohibit the existing
  owner-only **Delete vault** action, which requires exact `ERASE` confirmation. See
  [Android account settings](../apps/android/app/src/main/java/dev/peppy/mobile/GatewayAccountSettings.kt)
  and [iOS Account](../apps/ios/PeppyMobile/PeppyMobileApp.swift).
- Community owner and family-vault setup store a generated vault secret silently;
  show saved-secret feedback only after storage succeeds. Resume already
  prepared material using its stored secret or request re-entry when that copy
  is missing, never silently replace it. See [protected-storage guidance](AGENTS.md#hosted-enrollment-and-pairing).
- An invite link and display name request a **new vault on the server**, not
  admission to another vault. Show waiting, denied, and expired states distinctly.
  After administrator approval, collect new-vault credentials and create the
  vault. See [Android vault requests](../apps/android/app/src/main/java/dev/peppy/mobile/EnrollmentScreen.kt)
  and [iOS vault requests](../apps/ios/PeppyMobile/SelfHostedEnrollmentView.swift).
- Hosted onboarding retains its save acknowledgement and confirmation screens;
  the acknowledgement is a UI step, not a replacement for protected native
  storage. Existing-vault admission and website billing are separate outcomes.
  See [Android hosted enrollment](../apps/android/app/src/main/java/dev/peppy/mobile/HostedEnrollmentScreen.kt)
  and [iOS hosted enrollment](../apps/ios/PeppyMobile/HostedEnrollmentView.swift).

## Contacts

- Contact sync is opt-in; request Contacts permission through explicit foreground
  action, not startup or background work. Offer only writable accounts as
  destinations for new contacts. Approval means pending OS work, not a completed
  write; show uncertain outcomes without implying success or automatic retry.
  See [Android contact settings](../apps/android/app/src/main/java/dev/peppy/mobile/ContactSyncSettings.kt)
  and [domain contacts guidance](AGENTS.md#contacts).
- iOS **Pause contact sync** leaves the last published book intact. **Retire**
  leaves phone contacts untouched and re-enabling starts a new book. Android's
  **Stop and retire** has the retirement effect; when requested while locked,
  show publication as pending until the next unlock. See
  [iOS contact settings](../apps/ios/PeppyMobile/ContactsSettings.swift) and
  [Android contact host](../apps/android/app/src/main/java/dev/peppy/mobile/ContactSyncHost.kt).
- iOS selected Contacts access syncs only the selection; excluded contacts are
  not deletions. iOS does not sync notes. Its OS-scheduled updates may be delayed;
  do not promise immediate sync.

## Settings and capability status

- Android uses Mirroring, SMS, Contacts and Account tabs. iOS uses Sync and
  Account; unavailable SMS/MMS and notification mirroring remain status rows,
  never actionable toggles. Settings opens from Account, not a toolbar gear.
  An enrolled but locked Android screen retains a Settings entry for diagnostics
  and server information; owner actions still require unlocked keys.
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
  See [notification settings](../apps/android/app/src/main/java/dev/peppy/mobile/NotificationMirroringSettings.kt)
  and [MMS settings](../apps/android/app/src/main/java/dev/peppy/mobile/MmsSettings.kt).
- Android own-number access is optional and per SIM route, for excluding this
  phone from group-MMS replies. Retain manual entry when access or detection is
  unavailable. See [own-number settings](../apps/android/app/src/main/java/dev/peppy/mobile/OwnNumberSettings.kt).
- iOS carrier, SMS/MMS, and notification-mirroring unavailable rows are status,
  not actionable toggles. Wake relay registration is explicit opt-in; hints may
  be delayed or missing and do not guarantee timely sync. See
  [iOS capability status](../apps/ios/PeppyMobile/PeppyMobileApp.swift) and
  [wake protocol guidance](AGENTS.md#notifications).

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
