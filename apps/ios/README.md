# Peppy iOS gateway client

Follow the canonical [contributing workflow](../../CONTRIBUTING.md) for shared setup and checks.

This SwiftUI app is a product **gateway-role client** with Mirroring, SMS, and Account tabs plus Settings. It can enroll, unlock, synchronize, manage its device account, contacts, and optional wake enrollment. It is not a carrier executor: iOS has no carrier SMS/MMS executor or inbox listener in this build, no system-wide notification listener, and no RCS integration. Subscriber conversation, composer, and feed UI remain the unchanged shared webview surface.

## Enrollment, roles, and local security

**Peppy Hosted:** Google Sign-In uses the native GoogleSignIn SDK with a server-issued nonce. Account and entitlement checks use a native bearer session; billing opens Peppy's website and may require signing in there with the same Google account. New-vault encryption material and its vault shared secret are saved in device-only Keychain storage before provisioning, scoped to the hosted account and origin. Retries retain the same operation, vault identity, credentials, and protected secret; the SwiftUI passphrase fields are cleared and are not persistent UI state.

**Public OAuth settings:** Set these public Xcode build settings for hosted sign-in: `PEPPY_GOOGLE_IOS_CLIENT_ID`, `PEPPY_GOOGLE_NATIVE_SERVER_CLIENT_ID`, and `PEPPY_GOOGLE_REVERSED_CLIENT_ID`. Register the iOS client for bundle ID `dev.peppy.mobile`; the reversed ID is its registered callback scheme. The backend must explicitly allow the native token audience, separately from the browser OAuth client. Missing or empty configuration disables real sign-in rather than substituting a preview. Rebuild the app after changing any OAuth ID. Native Apple authentication remains unavailable pending its code-exchange integration and provider configuration.

Connect with the server address and shared secret, or scan the single **Connect your phone** QR shown by a web or desktop owner. Compare the locally computed verification codes and approve the connection. The selected mode either transfers verified keys through an encrypted channel or asks for the shared secret in a native secure field. The server receives neither the QR secret nor plaintext vault keys. Identity installation uses the shared Rust verifier before credentials are persisted in Keychain.

A browser, desktop or phone can be the first owner. The self-hosted first-run
screen identifies whether the unclaimed server is open or token-protected. Open
first claim needs no setup code; token-protected first claim requires the
operator code and an admin password, then prompts to save a generated vault
shared secret. That code authorizes only the initial claim, the admin password
authorizes server management, and the generated vault shared secret protects
vault data. A phone joining an existing vault remains a gateway-role client.
There is no phone-displayed QR, reversed **Add a computer** flow, or
credential-file onboarding. Hosted identity and billing remain separate from
vault encryption.

Use a nonempty `PEPPY_SETUP_TOKEN` of at least 32 UTF-8 bytes to require a setup
code, and configure the same token on every replica. An absent or empty value is
open, with no generated or logged fallback token. An unclaimed installation
upgraded without this setting becomes open.
The root [recovery procedure](../../README.md#recover-from-an-unwanted-first-claim)
for an unwanted stranger claim destroys only the affected deployment state; it
does not recover passwords.

The Account tab uses the actual role returned by the server. A device can sign itself out; only an `owner` can remove another device and see the typed-`ERASE` vault-deletion control. Gateway capability or a SIM does not grant owner privileges. Disconnect archives the encrypted database, keys, and queued work locally; it is not destructive reset or proof that a revoked credential can resume sync. Keychain records are device-only and not synchronized or restored by backup.

## Shared policy and iOS capability limits

Gateway settings and policy are durable shared Rust-core state accessed through generated UniFFI bindings. iOS supplies host facts only; it does not duplicate policy persistence. The UI reports SMS/MMS capture and notification mirroring as unavailable because this build has no carrier executor, SMS/MMS listener, or notification-listener API. RCS is unavailable. Contacts and administrative gateway functions remain available.

Foreground sync is bounded and cancels when the scene leaves the foreground. Contact passes may also request `BGAppRefresh` and `BGProcessing`, but iOS chooses whether and when to run them; requested starts are not delivery guarantees. Host tests use fakes and do not establish device Contacts, background scheduling, or iOS SDK behavior.

## Background updates and APNs wake relay

For an eligible Peppy Hosted enrollment, Background updates are automatic unless a previous relay setting was explicitly off. The app requests an APNs device token after enrollment and on launch without requesting notification authorization; the toggle pauses registration and local wake processing immediately. Hosted configuration is device-authenticated, pins the production relay, and fails closed when the signed build's APNs environment does not match the server configuration. APNs registration and iOS background scheduling are best-effort only: neither proves live delivery nor guarantees immediate locked-phone work.

Community servers retain the optional manual operator-relay setup in Settings. Hints carry only the relay's nested `peppy` `kind` and optional challenge value; they contain no message body. A received hint starts bounded sync/contact work and never authorizes carrier work. If a hint is delayed or missing, authoritative queued sync remains available on later foreground work.

Vault sync envelopes and private attachments keep the existing encrypted body transport. Carrier SMS/MMS would be outside that boundary, but iOS does not perform carrier transport in this build.

## Layout

- `Generated/`: UniFFI output owned by `crates/mobile-bindings`; never edit it by hand.
- `PeppyNative/`: enrollment, Keychain, bounded sync, contacts, gateway capability, and relay host code over the generated core facade.
- `ContactsHistory/`: Objective-C bridge for Contacts change history.
- `PeppyMobile/`: the SwiftUI app and iOS-only push wiring.
- `Smoke/` and `Tests/PeppyNativeTests/`: host smoke and Swift Testing coverage.

## Generate and verify

Generate Swift bindings from the repository root, then run host checks from this directory. With Command Line Tools only, Swift Testing requires its explicit macro-plugin path:

```sh
cargo build --locked -p peppy-mobile-bindings
cargo run --locked -p peppy-mobile-bindings --features cli --bin uniffi-bindgen -- generate --library target/debug/libpeppy_mobile_bindings.dylib --language swift --out-dir apps/ios/Generated

cd apps/ios
swift build
swift run PeppyMobileSmoke
swift test -Xswiftc -plugin-path -Xswiftc /Library/Developer/CommandLineTools/usr/lib/swift/host/plugins/testing
```

`swift build` and `swift test` compile macOS SwiftPM targets only; they are not evidence that the iOS app compiles. An iOS build requires full Xcode, an iOS SDK, and the Rust static library for `aarch64-apple-ios` or `aarch64-apple-ios-sim` at the project's configured library-search paths. Local Command Line Tools cannot substitute for that SDK verification.
