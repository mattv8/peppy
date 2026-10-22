# Windows installer artwork

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
