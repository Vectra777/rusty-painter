# Test documents

Files the tests open (`src/project/kra.rs`).

- `krita-layers-8bit.kra`, `krita-layers-16bit.kra`: 200×120 documents made
  with Krita 5.2 (RGBA, 8 and 16 bits per channel), bottom to top:
  - **Background**: white.
  - **Red square**: red at (10, 10), 80×60, and a 5×5 half-transparent patch
    at (130, 70).
  - **Folder** (opacity 191) holding **Blue multiply** (blue at alpha 200 at
    (60, 40), 100×70, multiply) and **Hidden** (green, invisible).

  Each includes Krita's flattened picture (`mergedimage.png`), which the
  tests compare the app's own rendering against.

Clip Studio Paint documents can't be made without Clip Studio, so the
`.clip` tests (`src/project/clip.rs`) build theirs in code, to the layout
documented by the open-source readers clip_to_psd and the `clipfile` crate.
