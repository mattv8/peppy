# iOS client reference

This reference covers iOS implementation and verification. For repository-wide workflow, see [CONTRIBUTING.md](../../CONTRIBUTING.md). User setup and limits are in [Clients](../../README.md#clients).

Hosted Google sign-in uses native GoogleSignIn with a server-issued nonce. Configure the public Xcode settings `PEPPY_GOOGLE_IOS_CLIENT_ID`, `PEPPY_GOOGLE_NATIVE_SERVER_CLIENT_ID`, and `PEPPY_GOOGLE_REVERSED_CLIENT_ID`. Register the iOS client for `dev.peppy.mobile`; use its reversed ID as the callback scheme. The backend must separately allow the native token audience. Missing values disable real sign-in rather than using a preview. Rebuild after changing an OAuth ID. Native Apple authentication remains unavailable pending code exchange and provider configuration.

New-vault material and the vault secret save to device-only Keychain storage before provisioning, scoped to account and origin. Retries retain their operation, vault identity, credentials, and protected secret. Clear SwiftUI passphrase fields and never persist them as UI state. Install identity through the shared Rust verifier before persisting credentials in Keychain. Shared gateway policy remains Rust-core state through UniFFI; do not duplicate it in iOS.

Foreground sync is bounded and cancels when the scene leaves the foreground. Contact passes may request `BGAppRefresh` and `BGProcessing`, but iOS decides whether and when to run them; requested starts are not delivery guarantees. Host tests use fakes and do not establish device Contacts, background scheduling, or iOS SDK behavior.

For eligible Hosted enrollment, Background updates are automatic unless a prior relay setting was explicitly off. Hosted relay configuration is device-authenticated, pins the production relay, and fails closed when the signed build's APNs environment differs from server configuration. The app requests an APNs token after enrollment and on launch without notification authorization; its toggle pauses registration and local wake processing. Community relays keep manual operator setup. Hints carry only the relay's nested `peppy` `kind` and optional challenge value, never a message body. A hint starts bounded sync/contact work and never carrier work. If it is delayed or missing, authoritative queued sync remains available on later foreground work. APNs registration and scheduling are best effort; neither proves delivery or immediate locked-phone work.

## Layout

- `Generated/`: UniFFI output owned by `crates/mobile-bindings`; never edit it by hand.
- `PeppyNative/`: enrollment, Keychain, bounded sync, contacts, gateway capability, and relay host code over the generated facade.
- `ContactsHistory/`: Objective-C bridge for Contacts change history.
- `PeppyMobile/`: SwiftUI app and iOS-only push wiring.
- `Smoke/` and `Tests/PeppyNativeTests/`: host smoke and Swift Testing coverage.

## Generate and verify

Generate Swift bindings at the root, then run host checks here. With Command Line Tools only, Swift Testing needs its macro-plugin path:

```sh
cargo build --locked -p peppy-mobile-bindings
cargo run --locked -p peppy-mobile-bindings --features cli --bin uniffi-bindgen -- generate --library target/debug/libpeppy_mobile_bindings.dylib --language swift --out-dir apps/ios/Generated

cd apps/ios
swift build
swift run PeppyMobileSmoke
swift test -Xswiftc -plugin-path -Xswiftc /Library/Developer/CommandLineTools/usr/lib/swift/host/plugins/testing
```

`swift build` and `swift test` compile macOS SwiftPM targets only. They do not prove an iOS app build. iOS verification needs full Xcode, an iOS SDK, and the Rust static library for `aarch64-apple-ios` or `aarch64-apple-ios-sim` at configured library-search paths. Command Line Tools cannot substitute for that SDK verification.
