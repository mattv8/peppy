# macOS DMG artwork

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
