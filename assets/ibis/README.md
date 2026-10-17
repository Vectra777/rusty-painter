# ibisPaint patterns

The tips (`tips/`) and paper textures (`papers/`) ibisPaint's brush QR codes
can name, for the ibisPaint brush import (`src/brush_engine/import/ibis.rs`).
`build.rs` builds every PNG here into the app.

**Placeholders**: these are ibisPaint's own pictures, extracted from the
ibisPaint 10.1.0 APK. They're ibisPaint's artwork, so they must be redrawn
before a release.

## Redrawing one

Keep the file name and draw a new picture in the same format:
- **8-bit greyscale PNG**: white paints and black doesn't. Grey paints
  partly.
- **The same size as the old picture**: ibisPaint sizes a brush by its
  tip's picture, so a tip drawn smaller paints smaller.

A brush finds its picture through `patterns.tsv` (`md5 → folder → name`), not
through the pixels, so a redrawn picture works without any other change.

## patterns.tsv

Each line has the MD5 a brush QR code names a picture by, the folder, and
the file name without `.png`. A paper texture has two lines: ibisPaint names
it either way up.

ibisPaint computes each MD5 over its picture as RGBA, with RGB = 255 and
A = the picture's coverage. `scripts/ibis/build_tip_table.py` rebuilds the
table from an ibisPaint APK; `scripts/ibis/ibis_brush_qr.py` dumps what a
brush QR code holds, as JSON.
