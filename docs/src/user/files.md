# Files, import and export

## Projects

Work is saved as a single `.rpainter` file (File → Save, `Ctrl+S`; File → Open, `Ctrl+O`): layers, the timeline and the whole undo history round-trip. File managers show a preview of the drawing, and Krita, GIMP or MyPaint can open it as a flattened image.

The file is an [OpenRaster](https://www.openraster.org/) archive: a ZIP with `mergedimage.png` (the flattened picture) and `Thumbnails/thumbnail.png`. The project itself is one more entry, `rusty-painter/project.rpnt`, in a versioned binary format with each tile zstd-compressed. Older project files stay loadable as the format grows.

## Export

File → Export (`Ctrl+E`):

| Format | Notes |
|---|---|
| PNG | 8 or 16-bit |
| JPEG | |
| TIFF | 8, 16-bit or 32-bit float; or CMYK for print |
| WebP | lossless |
| PSD | layered; 16-bit for a deeper document |
| SVG | layered: vector layers as editable paths, fill layers as shapes, paint layers as embedded pictures |

Exports embed the document's [colour profile](colour.md#colour-management). Draft layers are left out. For animation and time-lapse export, see [Animation](animation.md).

## Opening other apps' files

- **Photoshop** (`.psd`, 8 or 16-bit RGB or grayscale): layers, folders, masks, blend modes and clipping.
- **Krita** (`.kra`, 8 or 16-bit or float RGBA, opened at their depth): paint layers, folders, transparency masks, opacity, visibility and blend modes.
- **Clip Studio Paint** (`.clip`): raster layers, folders, masks, paper colour, clipping and blend modes.

Anything that can't come across (filter layers, vector data) is left out, and the app's own saved picture of the document is added as a hidden layer to compare against.

## Images

- `Ctrl+Shift+O` imports an image as a layer, or drop image files on the window.
- `Ctrl+V` pastes as a new layer, ready to transform, images from other programs too. `Ctrl+Shift+C` copies merged.
