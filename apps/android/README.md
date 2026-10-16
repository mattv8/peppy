# Peppy Android gateway

Follow the canonical [contributing workflow](../../CONTRIBUTING.md) for setup, container builds, emulator operations, and checks.

This native Kotlin/Compose app is a **gateway phone**, not a replacement for the default messaging app. Once enrolled and unlocked, its bottom navigation has **Mirroring**, **SMS**, **Contacts**, and **Account** tabs; Settings is available from the enrolled shell. Subscriber conversations and composition remain in the shared webview product surface, not in this app.

## Enrollment and account

**Peppy Hosted:** use the native Google sign-in flow to create or access an account at `https://peppy.pro`. Billing opens Peppy's website; sign in there with the same Google account, then return to check the subscription. The app uses the server's entitlement response before creating a vault. Passphrase generation and confirmation stay native. Prepared vault material and retry grants are Keystore-protected; interrupted setup resumes the same operation.

Hosted builds need a dedicated native-backend Google OAuth client ID through the Gradle property `peppyGoogleServerClientId` or environment variable `PEPPY_GOOGLE_NATIVE_SERVER_CLIENT_ID`. Register an Android OAuth client for `dev.peppy.mobile` and the signing certificate of the installed build. The backend's native Google audience allowlist must include the native-backend client ID, separate from the browser OAuth client. Missing configuration leaves hosted sign-in unavailable; it never selects a simulated provider. Native Apple authentication is not currently advertised.

Connect using the server address and shared secret, or scan the **Connect your phone** QR shown by an authorized web or desktop client. Compare the locally computed verification codes and approve the connection. Pairing supports encrypted key transfer or manual shared-secret entry. The server receives only an invitation digest and authenticated claim, never the QR secret or plaintext vault keys. Credentials and verified key caches are protected by Android Keystore.

A web client, desktop or phone can create the first vault and become its owner.
For a fresh self-hosted instance, the phone's setup wizard identifies whether
first claim is open or token-protected. Open first claim needs no setup code;
token-protected first claim requires the operator code and an admin password,
then generates the vault shared secret. The setup code authorizes that initial
claim only, the admin password authorizes server management, and the generated
vault shared secret protects vault data. A phone joining an existing vault is a
gateway. The phone does not display a pairing QR or an **Add a computer** action;
Settings contains no pairing QR.

Operators use a nonempty `PEPPY_SETUP_TOKEN` of at least 32 UTF-8 bytes for
token-protected first claim and configure the same token on every replica. An
absent or empty value is open; no fallback token is generated or logged. An
unclaimed installation upgraded without that setting becomes open.
For an unwanted stranger claim, the root
[recovery procedure](../../README.md#recover-from-an-unwanted-first-claim)
destroys only the affected deployment state; it is not password recovery.

The Account tab shows the server-reported role and device roster. A device may disconnect itself; only a server-reported `owner` can remove another device or reveal the typed-`ERASE` vault-delete control. A gateway role does not grant owner privileges. Disconnect archives local encrypted state; it is not a reset or a claim that a revoked credential can resume sync.

## Gateway policy and platform limits

Durable gateway settings and policy decisions live in the shared Rust core and are exposed through generated bindings. Android supplies transient facts such as granted permissions, notification-listener access, Wi-Fi transport, and silent-notification status; it does not maintain a second policy store. New mirroring is off by default. SMS and MMS synchronization, skip-silent, and Wi-Fi-only media settings are capability- and permission-gated. A Wi-Fi-only setting means Android's actual Wi-Fi transport, not merely an unmetered connection.

Android can capture supported SMS broadcasts and submit permitted SMS commands through the public carrier API. It requests `RECEIVE_SMS` and `SEND_SMS`; experimental MMS capture additionally requires `READ_SMS` and `RECEIVE_MMS`. Notification mirroring requires the user to enable Android's notification-listener access. Peppy does not request `WRITE_SMS`, the default-SMS role, or hidden carrier APIs. SIM availability or default-SMS status does not provide general RCS access; RCS remains unavailable.

Commands use the current default SMS subscription. If it changes or disappears, an old-route command remains pending rather than moving to another SIM. MMS does not downgrade to SMS. Carrier behavior, restricted-permission approval, and physical SMS/MMS delivery require device and carrier verification; emulator and unit results do not establish them.

## Relay wakes and encryption boundary

For a verified enrollment at `https://peppy.pro`, Background updates register automatically when the hosted server advertises FCM support. An existing disabled legacy relay preference remains paused; users can pause or resume the durable hosted setting from Account. Self-hosted/community relays retain their manual origin and enable controls. Firebase auto-init is disabled: the app requests an FCM token only from the eligible reconciliation worker (or for an explicitly enabled custom relay). Builds without Firebase app configuration report unavailable rather than acquiring a token.

Registration and cleanup run through network-constrained WorkManager reconciliation, so they may be delayed by connectivity, process death, quota, or locked keys. The relay stores only provider token metadata and content-free `wake`/`challenge` values. A wake delegates to the optional gateway reconnect seam, then falls back to expedited bounded sync with quota fallback; it never authorizes carrier work or promises immediate delivery. This repository does not claim a live signed FCM delivery path.

Vault sync envelopes and private attachment copies retain the existing encrypted body transport. Carrier SMS/MMS itself is outside that encryption boundary. The server's private attachment store is encrypted; public derivatives require a separate explicit publication action.

## Contacts and experimental MMS

The unlocked **Contacts** tab is the home of contact sync. Contact sync is separately enabled and permission-gated. The phone publishes its own book and applies remote edits according to its local Auto / Confirm / Off policy; permission loss and incomplete scans do not imply deletion. Photos are private encrypted 256×256 JPEG attachments, capped at 64 KiB. Doze, force-stop, provider behavior, ContactsProvider behavior, and background work can delay synchronization.

Experimental MMS requires participating clients that advertise MMS content version 2 or later. The existing messaging app downloads carrier MMS; Peppy scans available provider parts and uploads completed encrypted records. The phone validates encoded size against the reported carrier limit or a conservative 300 KiB fallback. Confirmed send is distinct from delivery, uncertain attempts are not automatically resent, and carrier MMS remains outside Peppy's encryption boundary.

For group MMS replies, the app reads the default SMS SIM's own number from the carrier (via `READ_PHONE_NUMBERS` on Android 13+, or the existing `READ_SMS` on older releases) solely so replies to a group MMS go to everyone except this phone. If the carrier does not provide the number, the MMS settings offer manual entry.

## Build configuration

**Public OAuth settings:** Hosted sign-in requires `peppyGoogleServerClientId` (Gradle property) or `PEPPY_GOOGLE_NATIVE_SERVER_CLIENT_ID` (environment variable). This is the public-facing OAuth client ID for the native-backend authentication flow, separate from the browser OAuth client. Missing or empty values produce self-hosting-capable builds with hosted sign-in unavailable. Rebuild the app after changing the ID.

**Rust library profile:** Set `PEPPY_ANDROID_NATIVE_PROFILE=debug` (default) or `release` to control whether the Rust-native bindings library is built with optimizations. The artifact workflow explicitly selects `release`; developer and local smoke tests default to `debug`. Profile selection does not change runtime security gates or debug origin checks.

## SMS latency diagnostics (debug only)

### Build and collect a comparison

With the [Android build prerequisites](#build-backends) installed and native libraries generated, run from `apps/android`:

```sh
# Baseline: existing 15-second debounce and 15-second capture delay.
./gradlew :app:assembleDebug -PpeppySmsDiagnostics=true -PpeppySmsFastCapture=false

# Experiment: 1-second observer quiet period, 3-second maximum awake wait,
# and no additional delay for a newly accepted observer request.
./gradlew :app:assembleDebug -PpeppySmsDiagnostics=true -PpeppySmsFastCapture=true
```

Both commands write `app/build/outputs/apk/debug/app-debug.apk`; retain the baseline artifact before building the experiment. The properties accept only `true` or `false`, default to false, and cannot enable either feature in a release build. Omit the properties or set them false to return to the baseline with diagnostics off.

After installing the chosen build on a test phone, collect only this diagnostic tag:

```sh
adb logcat -v threadtime -s PeppySyncTrace
```

For clean isolated-send measurements, allow more than 15 seconds after foregrounding or a completed sync and verify no delayed capture request is pending. Foreground entry and gateway passes enqueue delayed work; `KEEP` preserves that work even when a fast observer hint arrives. An enqueue trace without a corresponding worker start can therefore be expected, rather than evidence of a lost SMS. Measure this pending-work case separately from the clean case.

Compare isolated messages and bursts with settled history, then test backfill separately. Cover foreground/background, screen on/off, and Wi-Fi/cellular with the same phone, server, permissions, and workload. Record the build flags and native build profile for each run. Server acknowledgement and appearance on another client are different endpoints; measure them separately.

For battery comparisons, repeat matched unplugged idle-baseline and workload runs. Use Android CPU, wake-lock, and network measurements alongside these traces; logcat is not an energy meter. Keep debugging/collection conditions matched and account for tracing overhead. Physical-phone latency and energy results have not yet been established; emulator tests and APK builds do not establish them.

### Trace reference and limits

The line format is `event=<code> session=<process-token> timestamp_ms=<elapsed> trace=<scoped-trace> <field-code>=<value>...`. For example, `event=4 session=9 timestamp_ms=1234 trace=12 5=0 4=37` illustrates a capture start, attempt zero, with a measured queue wait of 37 ms. This is a format example, not a recorded result.

Event/field codes are defined in [`SyncDiagnostics.kt`](app/src/main/java/dev/peppy/mobile/SyncDiagnostics.kt); stable gateway phase codes are in [`GatewayWork.kt`](app/src/main/java/dev/peppy/mobile/GatewayWork.kt). `SOURCE` is event-specific:

| Event | Source values |
| --- | --- |
| Capture enqueue/start | 0 default, 1 observer, 2 continuation, 3 unknown |
| Capture plan | 0 live, 1 history, 2 reconciliation |
| Gateway enqueue | 0 normal unique-work path, 1 appended to an active pass |

- Timestamps use elapsed real time; CPU deltas measure the executing thread. The observer timer uses uptime, so sleep can extend its apparent elapsed wait beyond the configured maximum.
- Queue metadata is valid only within the enqueuing process session. Missing, mismatched, or future metadata emits `UNAVAILABLE=1` and omits a measured delay. Join trace numbers with the session token, never across sessions. Gateway-start `TRACE_ID` identifies the enqueue request; the line's `trace` identifies the execution scope.
- `CAPTURE_END` and `GATEWAY_END` outcomes are 1 success, 2 retry, 3 failure, and 4 throw. `SUCCESS=1` means a successful worker result, which does not itself prove that a message was uploaded. `OUTBOX_ACK` is emitted only after successful local acknowledgement of the server response.
- HTTP `SUCCESS=1` means a response completed, including 4xx/5xx; inspect `STATUS` for HTTP success. Byte fields describe application payloads, not wire/TLS/radio bytes. Transport failures can leave incomplete byte accounting. Route categories include attachments and exclude dynamic identifiers and queries.
- `NATIVE_OPEN SUCCESS=1` means a non-null client was returned. `COLD=1` means no cached client was observed at entry, not proof that this call initialized it; duration can include lock contention.
- Logs contain numeric metadata, not SMS bodies, phone/provider IDs, URLs, credentials, response bodies, or exception text. Logcat can drop lines. Coalesced hints, process death, retry/backoff, and Doze prevent exact per-message attribution from these pass-level traces.

The five-second acknowledgement target applies to a healthy network, a live process, settled history, and no pre-existing delayed/retry or recovery work. The experiment does not expedite uploads, keep the process alive, or change the periodic fallback.

## Build backends

Use `just android-build` from the repository root. With `PEPPY_ANDROID_BUILD_BACKEND` unset, empty, or set to `auto`, it builds natively on macOS and uses Docker on Linux or Windows via WSL. Set the variable to `native` or `docker` to select a backend explicitly. Native builds are supported only on macOS and fail with setup guidance on other hosts; they do not silently use Docker instead.

The native macOS route requires JDK 17, the repository-pinned Rust toolchain with `aarch64-linux-android` and `x86_64-linux-android` targets, `pkg-config`, host `libsodium`, and Android SDK command-line tools with API 36, build-tools 35.0.0, platform-tools, and NDK 27.2.12479018. It resolves the SDK from `ANDROID_SDK_ROOT`, `ANDROID_HOME`, or `~/Library/Android/sdk`. The native helper automatically discovers JDK 17 and Cargo/Rustup shims already installed on macOS; see [JDK 17 discovery](../../CONTRIBUTING.md#jdk-17-discovery) and [Cargo and Rustup shims](../../CONTRIBUTING.md#cargo-and-rustup-shims) for details. See [Android build backends](../../CONTRIBUTING.md#android-build-backends) for the full setup commands and license gate.

Native `android-build` does not require Docker or `.env`. `just android-run` still starts the container backend with `just dev-up`, so it requires both. `just` reads optional backend, artifact, profile, and license settings from ignored `.opencode/dev/android.env`; direct `bash infra/dev/android.sh ...` calls require explicit shell exports instead.

`just android-build` writes only `app-debug.apk`; `just android-test` routes through the Android helper with the same backend selector, runs the native verifier and five Gradle tasks, then writes the debug and instrumentation APKs. Artifacts default to `.opencode/dev/artifacts/android/` and honor `PEPPY_ANDROID_ARTIFACTS`. Native builds use the checkout `target` cache by default and regenerate tracked Rust-owned Kotlin plus ignored JNI libraries in the checkout. Inspect generated diffs after a native build. Docker keeps generation in its private workspace. Native host tooling avoids Linux QEMU, although some NDK tools can still need Rosetta.

## Generate and verify

Generated Kotlin is Rust-owned output under `app/src/main/java`; do not edit it manually. From the repository root:

Before generating or verifying, install pkg-config and system libsodium: Ubuntu `apt install pkg-config libsodium-dev`; macOS `brew install pkg-config libsodium`.

```sh
cargo build --locked -p peppy-mobile-bindings
cargo run --locked -p peppy-mobile-bindings --features cli --bin uniffi-bindgen -- generate --library target/debug/libpeppy_mobile_bindings.dylib --language kotlin --out-dir apps/android/app/src/main/java

export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"
infra/compose/verify-android-native.sh
cd apps/android
./gradlew :jvm-smoke:run :app:testDebugUnitTest :app:lintDebug :app:assembleDebug :app:assembleDebugAndroidTest
```

The Android build needs JDK 17, command-line tools, `platform-tools`, `platforms;android-36`, `build-tools;35.0.0`, and `ndk;27.2.12479018`; set `JAVA_HOME` and `ANDROID_HOME` or `ANDROID_SDK_ROOT`. The native verifier checks `arm64-v8a` and `x86_64` libraries and crypto symbols. `just android-smoke` additionally requires a selected emulator and verifies instrumentation execution, but neither it nor the host checks prove carrier, FCM, store, or physical-network behavior.
