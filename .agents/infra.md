# Infrastructure and release boundaries

Agent reference for public Peppy build, release, and native-artifact constraints.
Paths prefixed with `peppy-platform/` belong to a separate private repository
that is not in this checkout. Its inventory, hosts, credentials, runbooks, and
hosted deployment state are out of scope here; do not infer them from this
checkout.

## Ownership and signing authority

- This repository owns shared server and contracts, native clients, and public
  images. Public images stay self-hostable without hosted credentials.
- The private platform repository owns identity, billing, policy, hosted web,
  deployment, and hosted images. Hosted artifacts are not public library images.
- Signing authority is per repository. Public `COSIGN_PRIVATE_KEY` and
  `COSIGN_PUBLIC_KEY` cover public images; the private hosted publisher has its
  own key and verifier. Same-named secrets or variables across repositories do
  not imply the same signing authority. Do not move keys between them.
- Hosted publication binds two commits: the private release SHA and the exact
  public revision pinned in the private source-pin file. A matching branch name
  or a locally edited sibling checkout is not enough. The private pipeline
  consumes the signed public relay at that pinned revision; it does not rebuild
  or re-sign the relay with the hosted key.

## Public image lanes and release

- [`publish-community-image.yml`](../.github/workflows/publish-community-image.yml)
  accepts only `edge` for a `production` branch push, `staging` for a `staging`
  push, or a semver tag from protected stable dispatch. Publication also writes
  immutable `sha-<commit>` tags.
- A production push publishes `:edge` and its immutable SHA tag, not a stable
  version. Stable publication through the separate Release workflow requires a
  successful CI push run's `gate` job for the same commit. See
  [release channels](../README.md#release-channels).
- [`publish-image.sh`](../infra/release/publish-image.sh) keeps the library
  namespace and checks `sha-<commit>` before building. If an existing immutable
  tag inspects successfully, it verifies that digest's signature with the public
  key and promotes it; it never rebuilds or overwrites that tag. It builds only
  when inspection reports a missing manifest, then signs the digest produced by
  its own push. Aliases are created from the digest with `imagetools create`.
- The public script lacks the private publisher's metadata-file and error
  classification flow; a live sign/verify cycle is still needed to claim
  registry compatibility.

### Release streams and assets

- A passing `staging` push publishes `vX.Y.Z-staging.N`, calculated from Conventional Commits and commits since the stable tag. WiX retains a monotonic prerelease version-code sequence. Keep the newest 10 prereleases per stream. After the gate succeeds, publish `peppy-server:staging`, `peppy-push-relay:staging`, and immutable SHA tags.
- A passing `production` push can create the next stable `v*` release after gate and both edge-image publishers succeed. Promote immutable images as `X.Y.Z`, `X.Y`, `latest`, and `X` from `1.0.0`; do not rebuild them. Conventional Commits select major, minor, or patch, with a patch fallback. Missing stable-signing inputs skip automatic release with a warning; partial inputs fail. Stable release requires CI success for the exact production commit.
- The Release workflow runs from `production` with `bump` (`auto`, `patch`, `minor`, `major`), optional `version` (`X.Y.Z`), and `dry_run`. CI and stable-release concurrency retain one pending run, so a burst can cancel an intermediate run.
- Android uses independent `android-v*` tags and versions, beginning at `v0.17.1`. A staging push publishes `android-vX.Y.Z-staging.N`; a stable release publishes `android-vX.Y.Z`. It publishes when Android-relevant paths changed since its previous tag or after 500 commits. Its scope is `infra/release/components/android.paths`. Android releases are never GitHub latest and do not update `latest.json`.
- Pull-request CI runs affected jobs only; desktop macOS and Windows jobs run for desktop or shared-Rust changes. Pushes run full validation.
- Desktop `v*` assets include Linux AppImage, deb, and stable-only rpm; macOS DMG and archive; Windows MSI and NSIS; `latest.json`; `SHA256SUMS`; and generated notes. Android assets include a certificate-verified APK and AAB, GPG-signed checksums, and generated notes. The APK supports direct sideloading; the AAB is not installable. iOS remains unpublished CI output.

### Signing and release checks

- Trusted desktop releases include Tauri updater signatures, GPG-signed checksums, and GPG-signed RPM packages. macOS uses strict `developer-id` by default or explicit `adhoc`; Windows uses strict `authenticode` by default or explicit `none`. Strict modes fail closed. Ad-hoc macOS builds are not notarized; Windows `none` has no publisher signature.
- Android release assets require the protected-environment keystore, access inputs, and pinned SHA-256 certificate fingerprint. The iOS archive remains unsigned. Keep signing credentials only in protected CI environment secrets.
- Harbor publication requires protected `staging` and `production` environments with `HARBOR_USERNAME`, `HARBOR_PASSWORD`, `COSIGN_PRIVATE_KEY`, and `COSIGN_PASSWORD`. Public images contain no billing capability.
- Use `just version --channel prerelease` to preview a version. After changing `infra/release/` or release workflows, run `just release-test`. It requires git-cliff 2.14.2 on `PATH` (install with `infra/release/install-git-cliff.sh <dir>`), JDK 17 in `JAVA_HOME`, and an Android SDK at `ANDROID_HOME` or `ANDROID_SDK_ROOT` with build-tools `35.0.0` and `platforms/android-35`.
- See [release-update guidance](ops/release-updates.md) for provisioning, key rotation, rescue bootstrap, Linux package mapping, and Android keystore backup, key continuity, and OAuth certificate registration.

## Harbor's unsigned-manifest trap

- Harbor project policy that requires Cosign can reject **manifest reads** with
  HTTP 412 after a push already succeeded. That failure does not prove the tag
  is absent or that the build failed.
- The public publisher reads the new manifest before signing, so that policy
  can block it at digest discovery. It does not use the private publisher's
  build-metadata digest flow. A 412 is not a missing-manifest response; rerunning
  or disabling signature policy does not establish safe immutable-tag repair.

## Community setup, state, and proxies

- With `PEPPY_SETUP_TOKEN` absent or empty, community first claim needs no token
  but is open only for 60 minutes after each server process start, measured by a
  monotonic clock held in `ApiState`. Afterward a fresh claim returns
  `setup_window_closed` until the process restarts. Each replica has an
  independent window, and a restart reopens it. No environment variable changes
  this window. A nonempty value must contain at least 32 UTF-8 bytes; it protects
  first claim and has no time window. Protected retries and already-claimed
  responses do not depend on the window. Hosted row-security mode never offers
  community setup. The window limits how long an unclaimed token-free install
  stays exposed. It does not authenticate the operator and does not stop an
  attacker who claims during an open window. Configure the same token on every
  replica when protection is required, and never generate or log a fallback.
  Unclaimed installs upgraded without the setting become open for a window after
  their next start, so set the token before upgrading when protection is
  required. The setup code, admin password, and vault shared secret remain
  distinct. Setup and recovery detail
  is canonical in [first-run setup](../README.md#first-run-setup-and-device-connections),
  [unwanted first claim](../README.md#recover-from-an-unwanted-first-claim), and
  [hosted enrollment](AGENTS.md#hosted-enrollment-and-pairing).
- Root Compose sets `name: peppy`, so Docker prefixes its named volumes
  `postgres-data`, `seaweed-volume-data`, and `seaweed-filer-data`. Inspect the
  exact prefixed names before removing any volume. Never delete a shared
  database or object store. Integration fixtures are fail-fast and disposable;
  they are not the persistent development volumes ([CONTRIBUTING](../CONTRIBUTING.md)).
- Rate limits use the real socket peer. Forwarding headers count only when that
  peer matches explicit `TRUSTED_PROXY_CIDRS`; parse the chain right to left and
  fall back to the socket peer on malformed input. Never trust a bare forwarded
  header or the whole container subnet. Without the setting, proxied clients
  share the proxy's bucket. See [server assembly](../services/server/src/lib.rs)
  and [request authorization](../services/server/src/api.rs).
- A reverse proxy in front of a Peppy server can answer with its own status.
  Observed: a Pangolin/Traefik auth middleware answered `POST /v1/events` with
  a bodiless 403 after a ~10 s upstream timeout; the request never reached the
  server. In a Traefik access log such a response has no `ServiceName`, a
  `DownstreamContentSize` of 0, and a `Duration` near the timeout. Peppy
  server rejections always carry a JSON `code`, so a code-less 4xx is proxy
  or network evidence, never a server decision; clients treat it as
  retryable (see [domain guidance](AGENTS.md#android-carrier-companion)).
  Check the proxy's access log before changing the server.
- Public core migrations live in [`services/server/migrations`](../services/server/migrations/).
  They are not evidence of a hosted migration history, upgrade path, or deployed
  schema; hosted role, source-pin, and ordering details are private.

## Native artifact traps

- Regenerating UniFFI bindings does not update packaged JNI or static libraries.
  A missing new symbol at iOS link time can mean stale libraries rather than an
  invalid Swift API. Use `just android-build` or `just android-test`, which run
  [`verify-android-native.sh`](../infra/compose/verify-android-native.sh) before
  Gradle. A bare `./gradlew` assemble packages whatever is in `src/main/jniLibs`,
  which can fail at runtime with `UnsatisfiedLinkError` for new symbols.
- The Android script defaults to `PEPPY_ANDROID_NATIVE_PROFILE=debug`; the
  [artifact workflow](../.github/workflows/build-artifacts.yml) sets `release`.
  Bindgen uses the host debug library in both cases. Gradle `assembleRelease`
  does not optimize prebuilt JNI libraries.
- The iOS build script [`build-ios-artifacts.sh`](../infra/build/build-ios-artifacts.sh)
  checks Swift bindings against fresh UniFFI output and builds matching device
  and simulator libraries.
  Its `.xcarchive` is intentionally unsigned, not an installable IPA. Host Swift
  tests, simulator builds, and signed device installation are separate evidence;
  see [iOS limits](AGENTS.md#ios-limits).
- The browser core must be built with [`build-browser-core.sh`](../infra/build/build-browser-core.sh)
  for `wasm32-unknown-emscripten`. The script enforces a pinned Emscripten
  version and the `rust-toolchain.toml` channel. Its output is a generated ES
  module plus a normalized `.wasm` sidecar that the module references by name;
  keep the pair together. A generic wasm build is not a substitute.
- Google client IDs and callback schemes are public bundled configuration, not
  OAuth secrets. Changing them requires rebuilding the mobile app. Changing the
  server audience allowlist does not rewrite bundled configuration. See
  [Android setup](apps/android.md) and [iOS setup](apps/ios.md).
- An installed desktop app and the checkout are separate evidence. Before
  comparing them, record the executable path, bundle version, and available build
  provenance of the installed binary. An equal version string does not prove that
  a newer checkout is installed.
- Desktop updater endpoint, channel, and public key are compiled into the artifact
  by [`build-desktop-bundles.sh`](../infra/release/build-desktop-bundles.sh). A
  checkout fix cannot repair an already-installed binary. It needs an update from a
  build that contains the fix, or a manual replacement.
- Peppy's update feed ([`feed.rs`](../apps/desktop/src-tauri/src/updates/feed.rs))
  fetches the stable manifest itself. Tauri's updater builder also needs a
  configured endpoint. If it has none, `build()` returns `EmptyEndpoints` before the
  manifest is checked. [`updates/mod.rs`](../apps/desktop/src-tauri/src/updates/mod.rs)
  reports that failure as the generic `update-feed` "could not be verified" message.
  That message does not prove a bad signature or manifest. Check the builder error
  first.
- The endpoint and public-key overlay are asserted by
  [`test-desktop-bundles.sh`](../infra/release/test-desktop-bundles.sh). Run it after
  changing the overlay.
- A build that lacks the endpoint cannot update itself. Recover it with a manual
  install of a post-fix release over the existing app. Preserve user data; do not
  reset application state.
