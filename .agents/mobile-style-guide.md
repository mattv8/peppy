# Peppy mobile style guide

`packages/mobile-design` is the source and deterministic generator for shared
native design outputs. Run `pnpm --dir packages/mobile-design generate`; CI and
reviews use `pnpm --dir packages/mobile-design check` for drift.

## Generated native API

- Android imports `dev.peppy.mobile.ui.theme.PeppyTokens` and chooses
  `PeppyTokens.Dark` or `PeppyTokens.Light` from the system theme.
- iOS uses `PeppyTokens.colors(for: colorScheme)`. Its generated symbols use
  PascalCase semantic names (for example `Accent`, `SurfacePanel`).
- Android copy is `@string/peppy_<catalog_key>`. iOS strings live in the
  **Peppy** table: use `String(localized: "peppy.<catalog_key>", table: "Peppy")`
  or `Text("peppy.<catalog_key>", tableName: "Peppy")`. The default table does
  not find these keys. Wrap computed SwiftUI keys in `LocalizedStringKey`;
  accessibility labels also need a localized value or `Text` from this table.
- Add shared copy only to `src/catalog.mjs`, never directly to platform output.
  Native outputs use its filtered `nativeCatalog`; desktop uses
  `desktopCatalog`. A key present in the shared source is not necessarily
  shipped on mobile: preview/store-only keys are intentionally excluded.

## Enrollment

- Rust owns enrollment identity, QR parsing, claim HMAC, SAS, and root handoff.
  Install the opaque Rust identity before reporting native secret storage ready;
  a transfer activates only after the claimed SAS and explicit owner approval.
  See [Android host](../apps/android/app/src/main/java/dev/peppy/mobile/EnrollmentHost.kt),
  [Android installation](../apps/android/app/src/main/java/dev/peppy/mobile/NativeGateway.kt),
  and [iOS session](../apps/ios/PeppyNative/NativeSession.swift).
- Native hosts send Rust-produced, claimant-authenticated phone-status
  descriptors only. The private claim header is not UI data or a general HTTP
  capability. Use the canonical enrollment origin for identity and the
  same-origin status descriptor for transport. See [iOS host](../apps/ios/PeppyNative/EnrollmentHost.swift).
- The phone UI scans one `connect_phone` code and shows the SAS for the owner to
  compare. A transfer pairing confirms automatically after owner approval and
  advances only after verified native installation; a shared-secret pairing
  still requires the secret and an explicit submit.
  Camera permission is requested only when the user starts scanning. Do not add
  automatic OS-permission requests, default-SMS role requests, or RCS claims.
  See [Android enrollment UI](../apps/android/app/src/main/java/dev/peppy/mobile/EnrollmentScreen.kt)
  and [iOS enrollment UI](../apps/ios/PeppyMobile/PhoneEnrollmentView.swift).
- Owner phones offer **Add phone** on the main screen, never in Settings, and
  only when the `owner` role's vault keys are unlocked; hide or disable the
  entry otherwise rather than failing after it is opened. The owner flow shows
  the QR, then the locally derived SAS with an owner-specific codes-match control
  (`owner_phone_*` copy, not recipient pairing copy) before **Confirm and
  approve**; after approval it shows the approved-waiting state until the phone
  connects, and a denied claim shows its own message with **Start again**.
  Approval uses the stored vault secret; when it is missing, prompt once with the
  "not your admin password" copy and save only after verification. Run network
  and secret checks off the main thread. Phone vault creation stores the
  generated secret silently and says so only after the write succeeds. **Enter
  shared secret on phone** switches the invitation to the shared-secret method,
  then reveals the secret only after device authentication, transiently, never
  in saved UI state, and clears it when the app leaves the foreground. See
  [Android owner flow](../apps/android/app/src/main/java/dev/peppy/mobile/OwnerPhoneApprovalScreen.kt)
  and [iOS owner flow](../apps/ios/PeppyMobile/OwnerPhoneApprovalView.swift).
- First setup visibly identifies the server-advertised mode. Open first claim
  says that the first device to complete setup becomes owner and omits setup-code
  entry. Token-protected first claim states that the operator requires a code
  configured with `PEPPY_SETUP_TOKEN` and shows it. Preserve separate setup-code,
  administrator-password, and vault-shared-secret authorities. Treat missing
  policy conservatively for older servers, and do not offer destructive recovery
  or reset controls in the app.

Use native typography and touch targets with a 4px spacing rhythm; do not copy
desktop control dimensions. Use the coral graphic brand and semantic coral accents; coral is not normal-size text, follow the
system light/dark appearance, and retain opaque readable content when reduced
transparency or motion is enabled. Use stable semantic native test identifiers.

For a changed shared mobile behavior, update and test Rust, bindings, Android,
and iOS when they are affected. State platform capability exceptions explicitly;
do not force meaningless edits to unaffected surfaces.
