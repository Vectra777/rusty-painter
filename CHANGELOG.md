# Changelog

## Unreleased

### Brushes: Krita's missing options
- Colour mixing for any brush (Brush → Colour mixing), like Krita's Colour Smudge: smudge length and colour rate, each optionally by pressure. Krita's colour smudge presets import as mixing brushes.
- Tips flipped at random (left-right, top-bottom), like Krita's Mirror option.
- Pressure can set the spacing, with its own curve.
- Hard edges: a sharpness threshold that makes any tip crisp.
- Barrel rotation and an airbrush's finger wheel (Wayland, X11): the tip can follow the barrel, and both are inputs for any setting.
- Colour dodge and hard mix texture modes.
- The Parallel blend mode (Krita's), for layers and brushes.
- The Smudge tool (and mixing brushes) about a third faster, and pixels the brush doesn't reach are left exactly as they were.
- Krita's soft round tips keep their falloff curve (its airbrush was imported as a hard, solid tip).
- Krita presets bring their rotation (stroke direction, random, tilt, barrel, wheel), mirror, pressure spacing, sharpness, blend mode and these texture modes.

### Shader layers
- A layer drawn by a GLSL shader (Shadertoy style: `mainImage`, `iTime`, `iResolution`, `iMouse`, `iChannel0` = the layers below), animated live on the GPU with the layer's blend mode and opacity.
- One editor window per shader layer: syntax colouring, errors at their line as you type, play/pause, speed, templates, and Bake.
- Export, merging and saving use the current frame; the shader, its time and speed are saved with the project.

### Fixed (from the `windows` branch)
- Windows: the pen landed away from the pointer on displays scaled above 100% (Windows Ink positions are physical pixels).
- A pen stroke starts where the pen touched even when the tablet reports the position before the touch.
- Losing the window's focus mid-drag (Alt+Tab) ends the stroke, selection or other drag instead of leaving it stuck.

### Added (from the `windows` branch)
- A log file on Windows (`rusty-painter.log` in the data folder): start-up, the GPU used, errors and panics with their backtrace.
- `RUSTY_PAINTER_DISABLE_TABLET=1` starts without tablet input, to rule it out.
- CI runs its checks on Windows too.

## 0.1.0

The first version with a license. Earlier tagged versions (0.0.1 to 0.0.4) were unlicensed and kept their data in the folder the app was started from.

### Painting
- Brush engines: soft, pixel, bristle, sketch and hatching; dual brushes, textures, tips (several, colour, ribbon), watercolour edges, airbrush.
- Any input (pressure, tilt, speed, and more) can drive brush settings through its own curve.
- Stabiliser modes, QuickShape, mirror and radial painting, wrap-around, drawing assistants.
- Smudge and blur with deform, clone, sharpen and colour-adjust modes; a filter brush.

### Brushes as files
- Every preset is a file in the data folder; the default brushes are copied there on first start.
- Changes to a brush are saved into its preset. Reset to default and Restore default brushes undo them.
- Import brushes from GIMP, Photoshop, Krita, MyPaint and Clip Studio; export and share `.rpbrush` files.
- Tags, favourites, recent brushes and a pop-up palette.

### Documents
- Layers with blend modes, clipping masks, adjustment layers, text layers, merge down, merge visible, flatten, locks, drafts and references.
- Selections: rectangle, ellipse, lasso, polygon, magnetic lasso, wand, colour range, brush; modify, save by name, quick mask.
- Filters, an Image menu (canvas size, image size, crop, rotate, flip), PSD and OpenRaster files, time-lapse export, autosave with recovery.

### Settings
- Preferences, each tool's options, the brush and eraser in use, panel widths and view aids are kept between sessions.
- The keyboard layout, New Canvas and export choices, and the last folder used in file dialogs are remembered.

### Changed
- Data is kept in a per-user folder instead of the folder the app was started from. Nothing is moved over: copy an old `brushes/`, `swatches.json` and `gradients.json` into the new folder to keep them.
- The New Canvas dialog shows the open canvas's size in the unit picked (it showed pixels as inches before).

### License
- GPL-3.0-only.
