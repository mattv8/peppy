# Desktop update release provisioning

Only protected `updates-stable` and `updates-staging` environments may provide
desktop update credentials. Direct `build-artifacts.yml` dispatches and pull
requests never enable the trusted updater build input and therefore produce no
update artifacts or signatures.

Configure the following protected inputs before enabling an update release:

- `PEPPY_UPDATE_PUBLIC_KEY` environment variable: the updater verification
  public key compiled into official desktop builds.
- `PEPPY_UPDATER_PRIVATE_KEY` and optional
  `PEPPY_UPDATER_PRIVATE_KEY_PASSWORD` secrets: Tauri updater signing key.
- `PEPPY_MACOS_SIGNING_MODE` protected environment variable: `developer-id`
  (the default when unset) requires `APPLE_CERTIFICATE` (base64 P12),
  `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`,
  `APPLE_PASSWORD`, and `APPLE_TEAM_ID` secrets for Developer ID signing and
  notarization. Set it explicitly to `adhoc` for a free macOS build: it uses
  Tauri's `-` ad-hoc identity, verifies code-signature integrity, and neither
  notarizes nor claims Gatekeeper approval. It deliberately ignores stale Apple
  certificate and notary inputs.
- `PEPPY_WINDOWS_SIGNING_MODE` protected environment variable: `authenticode`
  (the default when unset) requires `WINDOWS_CERTIFICATE` (base64 PFX) and
  `WINDOWS_CERTIFICATE_PASSWORD` secrets. Set it explicitly to `none` for a
  free Windows build; updater signatures remain required, but installers do not
  carry an Authenticode publisher signature.
- `PEPPY_RELEASE_GPG_KEY`, `PEPPY_RELEASE_GPG_KEY_ID`, and optional
  `PEPPY_RELEASE_GPG_PASSPHRASE`: detached-sign `SHA256SUMS` for repository
  handoff. Linux repository operators pin and verify its public key.
- `PEPPY_RELEASE_RPM_GPG_KEY` and `PEPPY_RELEASE_RPM_GPG_KEY_ID`: sign RPMs
  before updater payload signatures and checksums. Repository operators pin
  this RPM keyring separately from the checksum-signing key. This must be an
  unprotected, isolated CI RPM signing key: `rpmsign` has no safe passphrase
  input, so passphrase-protected RPM keys are rejected.

The trusted build fails closed when its channel, updater key, required selected
platform credentials, or explicit mode value is invalid. It never downgrades a
strict platform because credentials are absent. It compiles the stable or staging channel and public
key into a temporary Tauri overlay, enables `createUpdaterArtifacts`, and does
not alter the base Tauri configuration. The publishing job signs each final
renamed updater payload with its release version, then creates `latest.json`.
Its six qualified targets are AppImage, deb, rpm, macOS app archive, NSIS, and
MSI. The URLs are always GitHub release asset URLs for `mattv8/peppy`.

The macOS updater payload is Tauri's `.app.tar.gz`, not a CI archive containing
multiple bundles. DMG remains a bootstrap/download artifact. Linux package
payloads are signed for metadata verification but installation remains the OS
package transaction owned by the native/Linux lanes.

## Key rotation and recovery

Rotate the updater key with an overlap/bridge release: ship a release signed by
the currently pinned key that contains code accepting the replacement public
key, then release artifacts signed by the replacement key. Keep the old signing
key available only until the bridge release has propagated; revoke it according
to the organization key-incident procedure. Never replace a pinned public key
without a signed bridge release.

If an updater key or release pipeline is unavailable, publish a manually
downloadable bootstrap installer as a new GitHub Release using the selected
platform policy.
Existing installations must not accept an unsigned rescue artifact. Operators
need GitHub Releases write permission, protected environments, updater and Linux
signing keys, and the Linux repository operator's signed metadata hosting inputs.
Developer ID notarization and Authenticode credentials are required only when
their strict modes are selected; free-mode recovery does not require an Apple
account or store enrollment. The Linux repository lane consumes the exact
`dist/release` directory after `sign-update-assets.sh` and
`generate-update-manifest.sh`; it must publish those immutable final assets and
their signatures without renaming them.

Protected environments must permit deployment only from `production` for
`updates-stable` and from `staging` for `updates-staging`. The only trusted
contexts are a Release workflow dispatch on `production` for stable and a push
to `staging` for staging; both require the requested build/release SHA to equal
the triggering SHA. Other callers, pull requests, and direct artifact dispatch
remain development-only and do not receive update credentials. Store all listed
release credentials only as environment secrets or variables, never repository
or organization secrets. Require deployment reviewers and forbid administrator
bypass for both environments; any configured reviewer or administrator bypass
can authorize release-key use and is therefore an operational security decision.
Use protected-environment variables (not repository defaults) for the two mode
values. Both are validated before trusted build work; unavailable GitHub
environment variables are mapped to the strict defaults, while an explicitly
empty or unknown script policy value fails. These GitHub
Release artifacts are not App Store, Mac App Store, Microsoft Store, Android
store, or iOS store submissions.

## Public verification keys

Channel-specific public verification material is committed under
`infra/release/keys/`: `stable-checksums.asc`, `stable-rpm.asc`,
`staging-checksums.asc`, and `staging-rpm.asc`, with each channel's updater
public key alongside them. Release operators must verify that each committed
fingerprint matches the corresponding protected-environment key before using or
rotating it; do not treat an arbitrary repository file as authoritative merely
because it has a familiar name. The repository and release assets distribute
public keys, but they cannot bootstrap first trust by themselves. Record the
expected public fingerprints independently (for example beside the recovery
password in a password manager) and treat an encrypted-backup decrypt failure
as a tamper signal rather than trying alternate passwords.

Linux packages use distro ordering without changing the canonical runtime,
manifest, or updater-signature version. Stable `1.2.3` packages are normalized
to Debian/RPM version `1.2.3` with RPM release `1`; staging `1.2.3-staging.4`
packages are normalized to `1.2.3~staging.4` with RPM release `1`. This makes
the staging package sort before stable. RPM metadata is normalized and RPM-signed
before Tauri updater signatures and final checksums are generated.

## Linux repository handoff

After `write-checksums.sh` and `sign-checksums.sh`, the Linux repository
operator receives the final `dist/release` directory without renaming files.
Invoke its `build-repository.sh <channel> <canonical-version> <staged-dir>
<out-dir>` contract only after that final checksum signature exists. Its
verification keyring inputs are `PEPPY_RELEASE_VERIFY_KEYRING` and
`PEPPY_RPM_VERIFY_KEYRING`, corresponding respectively to the protected release
checksum and RPM signing keys. The handoff package filenames are the final
`peppy-<version>-linux-x86_64.deb` and `.rpm` assets.
