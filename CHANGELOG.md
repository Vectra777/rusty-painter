# Changelog

## Unreleased

### A window that never stops answering
- Liquify keeps up with a pen: its dabs are spaced along the stroke however finely the tablet reports it (a pen's many short moves did the full work each, and twirl, pinch and bloat came out stronger than with a mouse), and the layer is drawn once a frame.
- File dialogs no longer hold up the window while they're open.
- Opening, saving, autosaving and exporting a document, importing an image, a reference picture or a palette's picture, and importing brushes are read, decoded, encoded and written on other threads; the canvas waits (with a note) only while a document is being opened or saved.
- Brush previews (the presets list, the pop-up palette, the brush settings strip) are drawn on a thread of their own, only for the presets on screen; a brush that fails to draw loses its preview, not the app.
- Settings, presets, the brush library, swatches, gradients and panel sizes are written to disk on their own thread.
- A new stroke no longer waits for the last one to finish painting; undo, shortcuts, the layer panel and the menus wait their turn instead of holding up the frame.
- Pasting and copying images to the system clipboard happen off the UI thread.
- Brush presets load in parallel at startup.
- Filters, merges (down, visible, flatten) and fills run on the stroke worker, in order with the strokes: the window keeps drawing while a big blur works (the filter dialog closes at once), and undo, the layer panel and saving wait their turn.
- The Smudge and Blur tools and mixing brushes paint big dabs across all cores and convert colours without a table lookup per pixel: a 200 px smudge stroke about 40% faster, blur 45%, deform, clone and sharpen 15–20%.

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
- Krita import fixes: wash painting mode, random (and other non-pressure) size, opacity, flow and squash, auto spacing, full-strength scatter, texture strength by pressure, texture brightness, contrast and random offset, picture tips always painting with their dark parts, image-stamp tips in their colours; options that don't come across are named in the import report.
- Krita round tips keep Krita's exact falloff (default, Gaussian and soft circles; the default circle's fade was read backwards), sharpness reads Krita's threshold, textures keep Krita's grain levels and brightness, and Krita's subtract texturing takes paint from the light grain as Krita does. Softness curves are tabulated, so curve tips paint about 30% faster.
- Wash mode paints as Krita's alpha darken: going over a light stroke at light pressure stays light.
- Brushes from Krita paint with Krita's exact texture formulas and its colour smudge engine (smearing, dulling, smear alpha, smudge radius).
- Hard edges can follow pressure (or any input) and keep a soft band; inputs can swing a setting both ways and drive darken.
- Tips can paint by lightness (Krita's lightness map) or as a gradient map.
- Round and square tips: spikes (a star when squashed), separate fades across and down, density and per-pixel randomness, like Krita's auto tips.
- Auto spacing (dabs by the square root of the size), and spacing, flow and lightness strength as inputs any sensor can drive; X and Y tilt sensors.
- Colour source (Brush → Colour source): a random colour each dab, each pixel, or a pattern pinned to the canvas from the brush colour to the secondary. Krita presets bring their spikes, fades, density, randomness, auto spacing, random colour sources and these sensors.
- Krita import reads options as Krita does (curve on or off, the common curve) and brings hue, saturation, value, darken, gradient colour source and lightness or gradient map tips; the report names options plainly.
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
