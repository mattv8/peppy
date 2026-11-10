# Android client reference

This reference covers Android implementation, build, and verification work. For repository-wide workflow, see [CONTRIBUTING.md](../../CONTRIBUTING.md). User installation, permissions, and capability limits are in the root [Clients](../../README.md#clients) guide.

## Hosted sign-in configuration

Hosted builds require `peppyGoogleServerClientId` or `PEPPY_GOOGLE_NATIVE_SERVER_CLIENT_ID`. Register an Android OAuth client for `dev.peppy.mobile` and the installed build's signing certificate. The backend native Google audience allowlist must include that native-backend client ID, separately from the browser OAuth client. Missing configuration leaves hosted sign-in unavailable and never selects a simulated provider. Rebuild after changing the OAuth ID. Native Apple authentication is not advertised.

Prepared vault material, retry grants, credentials, and verified key caches use Android Keystore. Durable gateway policy lives in the shared Rust core through generated bindings. Android supplies transient permission, notification-listener, Wi-Fi-transport, and silent-notification facts; do not add a second policy store.

For verified `https://peppy.pro` enrollment, background-update registration is automatic only when the hosted server advertises FCM support. Firebase auto-init is disabled. Acquire a token only in eligible reconciliation work or for an explicitly enabled custom relay. Builds without Firebase configuration report unavailable. Registration and cleanup use network-constrained WorkManager reconciliation. A wake may use the gateway reconnect seam before expedited bounded sync with quota fallback; it never authorizes carrier work. Do not claim a live signed FCM delivery path.

## SMS capture timing and diagnostics

SMS provider changes use a one-second quiet period, capped at three seconds awake per notification burst. Observer work has no extra initial delay. Foreground and gateway follow-up scans retain a 15-second delay. Unique-work coalescing, retries, and Android scheduling still apply; do not add expedited uploads, polling loops, or a persistent service for the observer path.

Diagnostics are off by default. **Settings → Diagnostics** records bounded numeric timing and counter records locally. **Share log** creates a bounded private snapshot. Neither logcat collection nor a Gradle property enables recording. For an already configured HTTPS local development stack, the native development helper sets the local CA automatically. Before invoking Gradle directly from `apps/android`, set this from the repository root:

```sh
export DEBUG=true
export PEPPY_DEV_TLS_CERT="$(python3 infra/dev/dev_tls.py ca-path)"
```

These settings only configure debug loopback trust. A direct Gradle build without them does not trust the local development CA.

Compare isolated messages and bursts with settled history, then test backfill separately. Cover foreground/background, screen on/off, and Wi-Fi/cellular on the same phone, server, permissions, and workload. Record diagnostics state and native build profile. Server acknowledgement and appearance on another client are different endpoints. For battery comparisons, use matched unplugged idle-baseline and workload runs with Android CPU, wake-lock, and network measurements. Logcat is not an energy meter. Physical-phone latency and energy are unestablished; emulators and APK builds do not establish them.

### Trace reference and limits

The line format is `event=<code> session=<process-token> timestamp_ms=<elapsed> trace=<scoped-trace> <field-code>=<value>...`. For example, `event=4 session=9 timestamp_ms=1234 trace=12 5=0 4=37` shows capture start, attempt zero, and a 37 ms queue wait; it is a format example, not a recorded result.

Event and field codes are in [`SyncDiagnostics.kt`](../../apps/android/app/src/main/java/dev/peppy/mobile/diagnostics/SyncDiagnostics.kt). Stable gateway phase codes are in [`GatewayWork.kt`](../../apps/android/app/src/main/java/dev/peppy/mobile/GatewayWork.kt). `SOURCE` is event-specific:

| Event | Source values |
| --- | --- |
| Capture enqueue/start | 0 default, 1 observer, 2 continuation, 3 unknown |
| Capture plan | 0 live, 1 history, 2 reconciliation |
| Gateway enqueue | 0 normal unique-work path, 1 appended to an active pass |

- Timestamps use elapsed real time; CPU deltas cover the executing thread. The observer uses uptime, so sleep can extend apparent elapsed wait.
- Queue metadata is valid only in the enqueuing process session. Missing, mismatched, or future metadata emits `UNAVAILABLE=1` and omits the delay. Join trace numbers with the session token, never across sessions. Gateway-start `TRACE_ID` identifies the enqueue request; the line's `trace` identifies execution.
- `CAPTURE_END` and `GATEWAY_END` outcomes are 1 success, 2 retry, 3 failure, and 4 throw. `SUCCESS=1` is a successful worker result, not upload proof. `OUTBOX_ACK` follows successful local acknowledgement of the server response.
- HTTP `SUCCESS=1` means a response completed, including 4xx/5xx; inspect `STATUS`. Byte fields cover application payloads, not wire, TLS, or radio bytes. Transport failures can leave incomplete accounting. Route categories include attachments and exclude dynamic identifiers and queries.
- `NATIVE_OPEN SUCCESS=1` means a non-null client returned. `COLD=1` means no cached client at entry, not that the call initialized it; duration can include lock contention.
- Logs exclude SMS bodies, phone/provider IDs, URLs, credentials, response bodies, and exception text. Coalesced hints, process death, retry/backoff, and Doze prevent exact per-message attribution.

The five-second acknowledgement target applies only to a healthy network, live process, settled history, and no delayed/retry/recovery work. It is not a delivery guarantee. The observer path does not expedite uploads, keep the process alive, or change periodic fallback.

## Advanced features (GitHub release APK)

The GitHub release APK's **Advanced features** card enables two optional Android settings for Peppy itself. It uses one ADB session over Wireless debugging, with no computer. Play and debug builds have no on-phone setup path; their `RestrictedSetupEntry` stubs render nothing. The Play build can still be granted the access with the copied adb command from a computer. The transport, [AdbRestrictedSetupTransport.kt](../../apps/android/app/src/sideload/java/dev/peppy/mobile/restrictedsetup/AdbRestrictedSetupTransport.kt), runs these fixed shell commands in order, with the package name validated:

```sh
appops set dev.peppy.mobile READ_RESTRICTED_MESSAGES allow
appops set dev.peppy.mobile WRITE_SMS allow
settings put global adb_wifi_enabled 0
```

After the connected grant/readback flow, Peppy attempts to turn Wireless debugging off whether or not the grants verified; there is no option to skip that attempt. Cancellation, timeout, or transport failure can leave it on, so the user may need to check Developer options. Readback reports each capability as Allowed, Not allowed, Unsupported, or Unknown. Setup completes only when every supported capability is Allowed; an unsupported capability is skipped. `WRITE_SMS` is an AppOp only: the manifest never declares it. [SystemBlocklist.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/messagingapp/SystemBlocklist.kt) needs the AppOp to read blocked numbers and to unblock one. [ProviderArchiveReader.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/messagingapp/ProviderArchiveReader.kt) reads archive state with `READ_SMS` alone. [MessagingAppSync.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/messagingapp/MessagingAppSync.kt) runs both in the `MESSAGING_APP` gateway phase before upload: blocklist, then unblock requests, then archive, each isolated so one failure never skips the others or the upload. Without the AppOp the last list is kept and published as not unblockable; an interrupted unblock is reconciled with `isBlocked`, never repeated.

Evidence boundary: a Pixel 7 on Android 17 with Google Messages 20261020 showed archive toggles reflected in `threads.archived` within seconds, and blocking a number added it to `com.android.blockednumber` and set the thread `archived=1`. The emulator instrumentation test proves only the AppOp path. Carrier and other-OEM behavior is unestablished.

## Google Messages link (GitHub release APK)

The Google Messages link runs libgm in the native Android host on the gateway phone. The GitHub release APK and the Play build both carry it. Trusted CI builds the GitHub release APK with `-PpeppyDistribution=direct -PpeppyGoogleMessagesLink=true` and `verifySideloadGoogleMessagesLink`, and the Play bundle with `-PpeppyDistribution=play -PpeppyGoogleMessagesLink=true` and `verifyReleaseGoogleMessagesLink` (`build-artifacts.yml`). CI development builds exclude it. Owner-approved 2026-11-09: `NATIVE_RCS_ENABLED` gates the native send executor, eligibility publication, and the receive coordinator together. Tests can force it with `nativeRcsSendExecutorEnabledForTest`. The link's UI is the "Google Messages (RCS)" card at the top of the SMS tab, with a "Google Messages" details page opened by "Manage link", not Sync details or Advanced features. Claim one-to-one text send and receive and typing indicators in one-to-one chats, both directions. Do not claim group, media, reaction, or reply support until it is built.

Limits to keep in user-facing copy and docs: Only one-to-one chats that already have a Google Messages thread use RCS; a new contact with no such chat stays SMS. Unmapped RCS capture requires `READ_RESTRICTED_MESSAGES`. On-phone restricted-message setup (Advanced features) is in the GitHub release APK only. The Play build can be granted that access with the copied adb command from a computer (MMS → Sync details); Play copy must not offer on-phone setup. Recovery after an outage back-dates at most 15 minutes, so a message whose Google timestamp is further behind can be missed. Edits and deletes are not reflected after ingest. Unlinking switches chats back to SMS and may show a cutover notice. Typing indicators work in one-to-one chats; groups, photos, media and reactions are unsupported.

Owner-device observations found stable conversation IDs across reconnects and temporary-message echoes in projected events with the same upstream libgm version. They do not establish behavior on other devices, carriers, or contacts.

`peppyGoogleMessagesLink` defaults to `false` for every distribution. A local direct or Play build can opt in with `-PpeppyGoogleMessagesLink=true` after building the AAR. Debug builds and flag-off builds exclude libgm, `libgojni.so`, and link notices. Trusted release builds always create the AAR and pass that flag to both the direct APK and the Play bundle, so both include libgm; `verifySideloadGoogleMessagesLink` and `verifyReleaseGoogleMessagesLink` expect it present. CI development builds leave the flag off.

Link code sits in four source sets. Restricted setup stays in `src/sideload` and the release and debug `RestrictedSetupEntry` stubs.

- `src/gmlink`: link service, sign-in activity, restart receiver, UI, resources, and the three link manifest components (`src/gmlink/AndroidManifest.xml`). Sideload always includes it; release includes it when the flag is true.
- `src/gmlinkOn`: the real libgm engine (`GoLinkEngine`) and the link notices in `assets/third_party`. Sideload and release include it when the flag is true.
- `src/gmlinkOff`: the `GoLinkEngine` stub. Sideload uses it when the flag is false, so flag-off sideload still compiles the link code without libgm.
- `src/gmlinkStub`: the no-op `GoogleMessagesLink` entry. Debug always uses it; release uses it when the flag is false.

Link unit tests stay in `src/testSideload`. `verifyReleaseGoogleMessagesLink` checks the release APK and AAB for libgm, `libgojni.so` alignment, and the link notices when the flag is true, and expects none of them when it is false.

### Native RCS implementation traps

- The distribution gate is not the activation constant. `peppyGoogleMessagesLink=true` includes libgm. `NATIVE_RCS_ENABLED` is a `true` constant that gates the native send executor and the receive coordinator's default `enabled`. `GoogleMessagesLinkService.nativeRcsSendExecutorEnabledForTest` still takes precedence when a test sets it. A flag-off sideload build uses the unavailable engine, so the constant has no effect there. Test overrides are not activation evidence. See [GoogleMessagesLinkService.kt](../../apps/android/app/src/gmlink/java/dev/peppy/mobile/googlemessages/GoogleMessagesLinkService.kt) and [RcsReceiveCoordinator.kt](../../apps/android/app/src/gmlink/java/dev/peppy/mobile/googlemessages/RcsReceiveCoordinator.kt).
- Route-only observation intentionally yields `GoogleConversationAmbiguous`. Expect `Mapped` only after the target is classified and bound. See [RcsNativeSendCoordinator.kt](../../apps/android/app/src/gmlink/java/dev/peppy/mobile/googlemessages/RcsNativeSendCoordinator.kt).
- Use captured link and core leases. Keep the route-change ticket revoker registered until final cleanup. Revalidate the exact route identity and evidence no older than 30 seconds before commit. A submitted or transport acknowledgement is not delivery, and an unknown outcome cannot authorize a blind resend.
- Typing runs in one-to-one chats only; group chats have no typing signal. Production path: `GoogleMessagesLinkService` holds the `/v1/live` socket while the link is linked, running, and has gateway credentials. Membership (roster, role, revocation, lease renewal) arrives in-band through `LiveScaffold`, not from a separate device list. Incoming typing from Google is sealed to the vault and sent on the live socket, and it displays on other devices whatever the sender's sharing setting. Outbound typing from other devices is opened, ordered, and forwarded to `LinkRuntime.setTyping` as admitted `STARTED` effects with validity capped at 5 seconds. Sharing is enforced at the origin device's seal: with sharing off, no typing frame is sealed, and the gateway does not re-check peer frames. See [LinkRuntime.kt](../../apps/android/app/src/gmlink/java/dev/peppy/mobile/googlemessages/LinkRuntime.kt) and [LiveScaffold.kt](../../apps/android/app/src/gmlink/java/dev/peppy/mobile/googlemessages/live/LiveScaffold.kt).

### Build and check commands

Use Go `1.27.3` (the module declares `go 1.26.0` and `toolchain go1.27.3`), JDK 17, Android SDK API 36/build-tools 35.0.0, and NDK `27.2.12479018`. Pins are libgm `v0.2610.1-0.20261103213626-d9140a5e807e` and gomobile/gobind `v0.0.0-20261011034528-197770444867`. The private safety patch SHA-256 is `c8090ce0e6da503ed1af75f22e5343de3f439430dce437787e81311859ca8aa3`; `prepare-libgm.sh` checks it, copies the pinned upstream into ignored `build/third_party`, applies it, and verifies the resulting tree. From the repository root:

```sh
cd apps/android/googlemessages
./prepare-libgm.sh
go test -race ./...
go vet ./...
test -z "$(gofmt -l .)"
```

CI builds the AAR, runs `check-licenses.sh` and `check-licenses-test.sh`, and packages a flag-on sideload artifact. For a local sideload artifact, set `JAVA_HOME` to JDK 17 and `ANDROID_HOME` to the SDK, then run from the repository root:

```sh
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"
(cd apps/android/googlemessages && ./build-aar.sh)
cd apps/android
./gradlew --console=plain --dependency-verification=strict -PpeppyGoogleMessagesLink=true :app:assembleSideload
```

Run the Rust-native verifier described below before Gradle if the packaged Rust JNI libraries are not current. `build-aar.sh` prepares libgm, runs vet/tests, installs the pinned mobile tools, and binds `./gmlink` with `-target=android/arm64,android/amd64 -androidapi 26 -javapkg dev.peppy.gmlink -ldflags='-extldflags=-Wl,-z,max-page-size=16384'`. Its output is `apps/android/googlemessages/build/gmlink.aar`; it requires exactly the arm64-v8a and x86_64 `libgojni.so` entries and verifies every ELF LOAD alignment is at least `0x4000` (16 KiB). Artifact checks do not prove real-account pairing or carrier behavior.

### Credentials and owner gates

The native host owns the Google session and keeps it Keystore-sealed and local to the phone. Google cookies and credentials must never enter shared DTOs, Rust durable state, the vault, logs, diagnostics, process arguments, Intent extras, saved instance state, or Peppy's server. No message plaintext or raw Google objects/errors go to logs or the server; diagnostics use opt-in fixed numeric codes only. The link does not request the default-SMS role, declare `WRITE_SMS`, or use hidden APIs.

Before linking Peppy on an account, unpair any other Messages for Web client (including the retired `dev.peppy.gmtest`) under Google Messages → Device pairing. A second web client takes over the active session. Never touch the `dev.peppy.gmtest` app or its durability evidence. Ask the owner before every real-account link. Real sends to third parties, phone-setting changes, and server deployment remain separately owner-gated. M1 documentation and CI checks authorize none of those actions.

## Build configuration

`PEPPY_ANDROID_NATIVE_PROFILE` selects `debug` (default) or `release` for Rust-native bindings. Artifact workflows select `release`; local developer and smoke tests default to `debug`. This does not change runtime security gates or debug-origin checks.

Run `just android-build` from the repository root. With `PEPPY_ANDROID_BUILD_BACKEND` unset, empty, or `auto`, it builds natively on macOS and uses Docker on Linux or Windows through WSL. Set `native` or `docker` to select a backend. Native builds run only on macOS and fail with setup guidance elsewhere.

The macOS route needs JDK 17, the pinned Rust toolchain with `aarch64-linux-android` and `x86_64-linux-android`, `pkg-config`, host `libsodium`, and Android command-line tools with API 36, build-tools 35.0.0, platform-tools, and NDK 27.2.12479018. It resolves the SDK from `ANDROID_SDK_ROOT`, `ANDROID_HOME`, or `~/Library/Android/sdk`. Native helpers discover existing JDK 17 and Cargo/Rustup shims; see [JDK 17 discovery](../../CONTRIBUTING.md#jdk-17-discovery) and [Cargo and Rustup shims](../../CONTRIBUTING.md#cargo-and-rustup-shims). See [Android build backends](../../CONTRIBUTING.md#android-build-backends) for setup commands and the license gate. Native builds need neither Docker nor `.env`; `just android-run` starts `just dev-up` and needs both. `just` reads optional backend, artifact, profile, and license settings from ignored `.opencode/dev/android.env`; direct `bash infra/dev/android.sh ...` calls need shell exports.

`just android-build` writes `app-debug.apk`. `just android-test` uses the same backend selector, runs the native verifier and five Gradle tasks, and writes debug and instrumentation APKs. Artifacts default to `.opencode/dev/artifacts/android/` and respect `PEPPY_ANDROID_ARTIFACTS`. Native builds use the checkout `target` cache and regenerate tracked Rust-owned Kotlin plus ignored JNI libraries; inspect generated diffs. Docker generates in a private workspace. Native host tooling avoids Linux QEMU, though some NDK tools still need Rosetta.

## Generate and verify

Generated Kotlin under `app/src/main/java` is Rust-owned; do not edit it manually. Install `pkg-config` and system `libsodium` first: Ubuntu `apt install pkg-config libsodium-dev`; macOS `brew install pkg-config libsodium`. From the repository root:

```sh
cargo build --locked -p peppy-mobile-bindings
cargo run --locked -p peppy-mobile-bindings --features cli --bin uniffi-bindgen -- generate --library target/debug/libpeppy_mobile_bindings.dylib --language kotlin --out-dir apps/android/app/src/main/java

export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"
infra/compose/verify-android-native.sh
cd apps/android
./gradlew :jvm-smoke:run :app:testDebugUnitTest :app:lintDebug :app:assembleDebug :app:assembleDebugAndroidTest
```

Set `JAVA_HOME` and `ANDROID_HOME` or `ANDROID_SDK_ROOT`. The build needs JDK 17, command-line tools, `platform-tools`, `platforms;android-36`, `build-tools;35.0.0`, and `ndk;27.2.12479018`. The native verifier checks `arm64-v8a` and `x86_64` libraries and crypto symbols. `just android-smoke` also needs a selected emulator and verifies instrumentation execution. Neither it nor host checks prove carrier, FCM, store, or physical-network behavior.
