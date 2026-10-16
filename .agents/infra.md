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
  [release boundary](../README.md#release-boundary).
- [`publish-image.sh`](../infra/release/publish-image.sh) keeps the library
  namespace and checks `sha-<commit>` before building. If an existing immutable
  tag inspects successfully, it verifies that digest's signature with the public
  key and promotes it; it never rebuilds or overwrites that tag. It builds only
  when inspection reports a missing manifest, then signs the digest produced by
  its own push. Aliases are created from the digest with `imagetools create`.
- The public script lacks the private publisher's metadata-file and error
  classification flow; a live sign/verify cycle is still needed to claim
  registry compatibility.

## Harbor's unsigned-manifest trap

- Harbor project policy that requires Cosign can reject **manifest reads** with
  HTTP 412 after a push already succeeded. That failure does not prove the tag
  is absent or that the build failed.
- The public publisher reads the new manifest before signing, so that policy
  can block it at digest discovery. It does not use the private publisher's
  build-metadata digest flow. A 412 is not a missing-manifest response; rerunning
  or disabling signature policy does not establish safe immutable-tag repair.

## Community setup, state, and proxies

- Community first claim is open when `PEPPY_SETUP_TOKEN` is absent or empty. A
  nonempty value must contain at least 32 UTF-8 bytes and then protects first
  claim. Configure the same value on every replica; never generate or log a
  fallback. Unclaimed installs upgraded without the setting become open, so set
  it before upgrading when protection is required. The setup code, admin
  password, and vault shared secret remain distinct. Setup and recovery detail
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
  [Android setup](../apps/android/README.md) and [iOS setup](../apps/ios/README.md).
