# Mac App Store packaging

Set `PEPPY_MACOS_DISTRIBUTION=app-store` only in a protected release environment.
This overlay produces a sandboxed `org.peppy.desktop` app with no updater endpoints or
updater artifacts. It builds a universal Apple Silicon/Intel app without the updater feature,
ships `PrivacyInfo.xcprivacy`, and requires a plain `X.Y.Z` marketing version. The release
script stamps App Store/Xcode metadata, embeds the supplied provisioning profile, signs nested
code before the app, and creates a signed `.pkg`; it does not create or notarize a DMG.

`MACOS_BUILD_NUMBER` (CFBundleVersion) is required and must be a positive integer. It is
supplied explicitly through the manual `build-artifacts.yml` dispatch input `macos_build_number`
with `macos_distribution=app-store`; no version calculation emits it. The build refuses to run
unless `GITHUB_ACTIONS=true`, so local builds can never produce an App Store package.

Choose the value per upload so it is strictly greater than every build already uploaded for the
app record (App Store Connect rejects reuse). Keep it in the same scheme for every upload; a
`YYYYMMDDHHMM`-style timestamp (about 2e11) must not be mixed with a run-number scheme (about 1e6),
because the larger value would block later uploads.

Packaging rejects a package with non-world-readable files, quarantine attributes, a non-universal
executable, or updater markers. The uploaded artifact includes `Peppy.Info.plist` beside the pkg
for submission inspection.

`ci.yml` and `release.yml` do not pass a build number and do not build App Store packages.
