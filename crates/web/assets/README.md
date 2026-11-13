# `crates/web/assets` (M7-18, #151)

Static files for the browser frontend, copied into `dist` by the
`<link data-trunk rel="copy-file">` entries in `../index.html`. There is no
service worker and no offline support on purpose; a web app manifest and icons
are enough for Chromium to offer an install.

| File | What it is |
|---|---|
| `manifest.webmanifest` | The web app manifest. `theme_color` and `background_color` are the `--bg` token of `../style.css`, and `crates/web/tests/manifest.rs` reads that token and holds the manifest to it. |
| `icon.svg` | Original icon art: a d-pad on a rounded square in the accent colours of `style.css`. Nothing Sega-owned, and no lettering, so it still reads at 32px. |
| `icon-192.png`, `icon-512.png`, `maskable-512.png` | Generated from `icon.svg`, committed so that building the frontend needs no rasteriser. |

To regenerate the PNGs after changing `icon.svg`, from this directory:

```sh
nix develop -c sh -c '
  cd crates/web/assets
  resvg -w 192 icon.svg icon-192.png
  resvg -w 512 icon.svg icon-512.png
  resvg --background "#0b0d12" -w 512 icon.svg maskable-512.png'
```

`resvg` is in the dev shell for exactly this. The background on the maskable
render fills the corners the rounded square leaves empty, which are the corners
a launcher masks off anyway; the d-pad is drawn inside the 80% maskable safe
zone, so the same artwork serves both purposes.
