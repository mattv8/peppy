# Mac App Store packaging

Set `PEPPY_MACOS_DISTRIBUTION=app-store` only in a protected release environment.
This overlay produces a sandboxed `org.peppy.desktop` app with no updater endpoints or
updater artifacts. It builds a universal Apple Silicon/Intel app without the updater feature,
ships `PrivacyInfo.xcprivacy`, and requires a plain `X.Y.Z` marketing version. The release
script stamps App Store/Xcode metadata, embeds the supplied provisioning profile, signs nested
code before the app, and creates a signed `.pkg`; it does not create or notarize a DMG.

`MACOS_BUILD_NUMBER` (CFBundleVersion) is required and must be a positive integer. Stable
desktop releases automatically compute it as `major*1000000 + minor*1000 + patch`, with each
component at most 999. Manual `build-artifacts.yml` dispatches must supply the matching
`macos_build_number` with `macos_distribution=app-store`. The build refuses to run unless
`GITHUB_ACTIONS=true`, so local builds can never produce an App Store package.

The number must exceed previous uploads. Do not mix this scheme with larger timestamp-based
numbers, which would block subsequent uploads.

Packaging rejects a package with non-world-readable files, quarantine attributes, a non-universal
executable, or updater markers. The uploaded artifact includes `Peppy.Info.plist` beside the pkg
for submission inspection.

After a desktop release publishes, `release.yml` dispatches `testflight.yml` on production
to build and upload the App Store package for the internal group configured to receive all builds.
The TestFlight workflow can also be dispatched manually on production with a version and its
release commit SHA, to re-upload a released version. The `v<version>` tag must resolve to that SHA,
and the SHA must already be in production's history. Configure
`APPLE_ASC_KEY_ID`, `APPLE_ASC_ISSUER_ID`, and `APPLE_ASC_PRIVATE_KEY` (the p8 text) in the
protected `mac-app-store` environment, alongside the `APPLE_MAS_*` signing secrets.

`ITSAppUsesNonExemptEncryption` is false because Peppy uses only encryption exempt from
export compliance documentation and France is not offered. If France is added again, set it
to true and add `ITSEncryptionExportComplianceCode`.
