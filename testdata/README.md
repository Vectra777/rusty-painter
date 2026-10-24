# Test documents

Files the tests open (`src/project/kra.rs`, `src/project/psd.rs`).

- `krita-layers-8bit.kra`, `krita-layers-16bit.kra`: 200×120 documents made
  with Krita 5.2 (RGBA, 8 and 16 bits per channel), bottom to top:
  - **Background**: white.
  - **Red square**: red at (10, 10), 80×60, and a 5×5 half-transparent patch
    at (130, 70).
  - **Folder** (opacity 191) holding **Blue multiply** (blue at alpha 200 at
    (60, 40), 100×70, multiply) and **Hidden** (green, invisible).

  Each includes Krita's flattened picture (`mergedimage.png`), which the
  tests compare the app's own rendering against.
- `krita-layers-16bit.psd`: the 16-bit document exported by Krita 5.2 as a
  16-bit PSD (`krita krita-layers-16bit.kra --export --export-filename
  krita-layers-16bit.psd`, with `QT_QPA_PLATFORM=minimal`).

Clip Studio Paint documents can't be made without Clip Studio, so the
`.clip` tests (`src/project/clip.rs`) build theirs in code, to the layout
documented by the open-source readers clip_to_psd and the `clipfile` crate.

`ibis/*.ipbz`: the data of six brush QR codes exported by ibisPaint 10.1.0
(Android), as read from the QR images (`src/brush_engine/import/ibis.rs`):
**Airbrush (Normal)** and **Pen (Soft)**, two of its built-in brushes;
**test1**, a pen-fade brush renamed (so it carries its name); **hard
sketch/lineart**, a particle brush (grainy specks over a round tip);
**もこもこペン(線)** ("fluffy pen (line)"), an outline brush whose tip ibisPaint
makes itself (it isn't among the app's pictures); and **Watercolor Brush**
(its name in mathematical letters, emoji-range characters), a Water brush.

`ibis/small-screenshot.png`: a 240-pixel screenshot of a brush QR code
(**SCALES DALLIE**, in circled letters), too small and soft for the QR
detector until it's enlarged.

- `procreate-tiny.procreate`: a 300×60 Procreate document made by
  `scripts/make_procreate_fixture.py` (no Procreate needed; tiles are
  literal-only LZO streams), top to bottom: **Top** (red, (20, 10), 100×20),
  **Folder** holding **Inside** (blue at alpha 128, (150, 35), 130×20,
  multiply, across the edge tile), white background; with its composite and
  thumbnail. Opened by `src/project/procreate.rs`.
