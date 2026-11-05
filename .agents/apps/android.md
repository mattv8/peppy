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

## Advanced features (sideload)

The sideload build's **Advanced features** card enables two optional Android settings for Peppy itself. It uses one ADB session over Wireless debugging, with no computer. Play and debug builds have no setup path. The transport, [AdbRestrictedSetupTransport.kt](../../apps/android/app/src/sideload/java/dev/peppy/mobile/restrictedsetup/AdbRestrictedSetupTransport.kt), runs these fixed shell commands in order, with the package name validated:

```sh
appops set dev.peppy.mobile READ_RESTRICTED_MESSAGES allow
appops set dev.peppy.mobile WRITE_SMS allow
settings put global adb_wifi_enabled 0
```

After the connected grant/readback flow, Peppy attempts to turn Wireless debugging off whether or not the grants verified; there is no option to skip that attempt. Cancellation, timeout, or transport failure can leave it on, so the user may need to check Developer options. Readback reports each capability as Allowed, Not allowed, Unsupported, or Unknown. Setup completes only when every supported capability is Allowed; an unsupported capability is skipped. `WRITE_SMS` is an AppOp only: the manifest never declares it. [SystemBlocklist.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/messagingapp/SystemBlocklist.kt) needs the AppOp to read blocked numbers and to unblock one. [ProviderArchiveReader.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/messagingapp/ProviderArchiveReader.kt) reads archive state with `READ_SMS` alone. [MessagingAppSync.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/messagingapp/MessagingAppSync.kt) runs both in the `MESSAGING_APP` gateway phase before upload: blocklist, then unblock requests, then archive, each isolated so one failure never skips the others or the upload. Without the AppOp the last list is kept and published as not unblockable; an interrupted unblock is reconciled with `isBlocked`, never repeated.

Evidence boundary: a Pixel 7 on Android 17 with Google Messages 20261020 showed archive toggles reflected in `threads.archived` within seconds, and blocking a number added it to `com.android.blockednumber` and set the thread `archived=1`. The emulator instrumentation test proves only the AppOp path. Carrier and other-OEM behavior is unestablished.

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
