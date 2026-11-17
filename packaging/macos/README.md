# macOS release artifacts

Direct-download builds publish separate Apple Silicon and Intel DMGs:
`peppy-<version>-macos-arm64.dmg` and `peppy-<version>-macos-x64.dmg`.
Their signed updater payloads use the corresponding `darwin-aarch64` and
`darwin-x86_64` manifest keys. CI preserves them as
`desktop-macos-arm64.app.tar.gz` and `desktop-macos-x64.app.tar.gz`.
The Mac App Store package remains a separate
universal artifact described in `app-store/README.md`.

Xcode 27 and later reject macOS 11.0 deployment targets. Direct builds that
retain the macOS 11 minimum require an Xcode 26-or-earlier runner; otherwise,
raise the direct-build `minimumSystemVersion` to 12.0.
