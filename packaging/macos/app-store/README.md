# Mac App Store packaging

Set `PEPPY_MACOS_DISTRIBUTION=app-store` only in a protected release environment.
This overlay produces a sandboxed `org.peppy.desktop` app with no updater endpoints or
updater artifacts. The release script embeds the supplied provisioning profile and creates
a signed `.pkg`; it does not create or notarize a DMG.

`MACOS_BUILD_NUMBER` (CFBundleVersion) is required and must be a positive integer. It is
supplied explicitly through the manual `build-artifacts.yml` dispatch input `macos_build_number`
with `macos_distribution=app-store`; no version calculation emits it. The build refuses to run
unless `GITHUB_ACTIONS=true`, so local builds can never produce an App Store package.

Choose the value per upload so it is strictly greater than every build already uploaded for the
app record (App Store Connect rejects reuse). Keep it in the same scheme for every upload; a
`YYYYMMDDHHMM`-style timestamp (about 2e11) must not be mixed with a run-number scheme (about 1e6),
because the larger value would block later uploads.

`ci.yml` and `release.yml` do not pass a build number and do not build App Store packages.
