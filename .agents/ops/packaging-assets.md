# Packaging artwork assets

For coding agents: use this reference when changing checked-in macOS DMG or Windows installer artwork and its generators.

## macOS DMG artwork

`background.svg` is the original Finder background source. Finder, not this
artwork, supplies the Peppy app icon, Applications folder icon, and both labels.

Regenerate the checked-in Retina TIFF and inspection preview on macOS:

```sh
python3 packaging/macos/dmg/generate_assets.py
python3 packaging/macos/dmg/tests/test_dmg_assets.py
```

The generator uses only macOS `sips` and `tiffutil`. The SVG references the
project's pinned Droid Sans regular face for local rasterization; it does not
duplicate that font in the DMG asset directory.
`background.tiff` contains 700×410 and 1400×820 representations. A preview is
not a release asset; create one only outside the repository, for example with
`--preview .opencode/sessions/macos-setup/background-preview.png`.

`apps/desktop/src-tauri/tauri.macos.conf.json` configures the background and
icon positions under `bundle.macOS.dmg`. Its 700×442 Finder window yields the
700×410 artwork content area on current macOS Finder chrome; asset tests check
this actual packaging configuration against the layout.

`repair_dmg.py` runs after Tauri creates a DMG. It writes the complete,
volume-relative Finder layout, converts to a private same-directory candidate,
verifies its UDZO format and a read-only remount, then replaces the original
only after verification. Developer ID mode preflights the configured identity
before mounting, re-signs with a timestamp, and verifies the candidate before
replacement. If Tauri has already removed its temporary keychain, the helper
imports the existing `APPLE_CERTIFICATE` and `APPLE_CERTIFICATE_PASSWORD` into
its own temporary keychain for that repair only. It does not create
certificates or change the user's keychain search list unless cleanup must
remove that owned keychain entry. Its dependencies are exact hash-pinned in
`requirements.txt`.

## Windows installer artwork

`installer-art.svg` is the maintained WiX illustration source; matching NSIS
and header SVG sources sit beside it. The generator uses macOS `sips` to
rasterize their rounded, anti-aliased shapes, then converts the intermediate
32-bit bitmap to the checked-in 24-bit uncompressed BMPs required by Tauri's
native Windows installer templates. It does not replace those templates or draw
their text, controls, version, or branding.

```sh
python3 packaging/windows/installer/generate_assets.py
python3 packaging/windows/installer/generate_assets.py --check
python3 packaging/windows/installer/generate_assets.py --check-render   # opt-in, macOS maintainer only
python3 packaging/windows/installer/tests/test_installer_assets.py
```

Generation writes `asset-manifest.json`, which records the SHA-256 of each SVG
source and each checked-in BMP, plus the source each BMP came from. `--check`
does not run `sips`. It verifies that the SVG sources match the generator, that
the manifest matches the committed files, and that each BMP is 24-bit
uncompressed `BI_RGB` with the expected dimensions, a white native text area in
the WiX dialog, and illustration content inside its safe margins. Because it
does not re-rasterize, it gives the same result on any CI runner.

`--check-render` also re-rasterizes with `sips` and requires byte-identical
bitmaps. CoreSVG anti-aliasing can differ between macOS releases, so this check
is only expected to pass on the macOS version that produced the committed
bitmaps. Run it locally before regenerating. Any regeneration must update the
manifest in the same change.

The WiX dialog asset reserves its right side in white for native welcome and
finish-page text. The NSIS sidebar is illustration-only.

`tauri.windows.conf.json` is a platform overlay. Its `bundle.windows` object
must be deeply merged with existing release settings so WiX `version` and
`signCommand` values remain intact.
