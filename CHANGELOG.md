# Changelog

## Unreleased

### A window that never stops answering
- Liquify keeps up with a pen: its dabs are spaced along the stroke however finely the tablet reports it (a pen's many short moves did the full work each, and twirl, pinch and bloat came out stronger than with a mouse), and the layer is drawn once a frame.
- File dialogs no longer hold up the window while they're open.
- Big colour-mixing and smudge dabs keep one scratch buffer from dab to dab instead of a fresh one each (half or more of their page faults gone; about a sixth less time at 200–400 px).
- Mixing brushes smudge as Krita's colour smudge does: each dab drags the paint from where the last one was and mixes in the brush colour (the colour rate still per brush width), on 8-bit documents straight on the stored pixels; a stroke of a 300 px brush is done 2–3× sooner (a brush kept dirty keeps the paint-carrying engine). A pen lift's last dabs and post-correction are redrawn at once (they showed as blocks of the old picture until the document was reopened). A mixing brush with a taper by percentage no longer lays its whole stroke again when the pen lifts (the stroke went darker after it was drawn), and at tight spacing on 8-bit documents its colour adds up instead of rounding away to nothing.
- Undo no longer waits for a stroke a heavy brush is behind on to be painted to the end before taking it back: the samples not yet painted are dropped (a big colour-mixing stroke froze the window up to 10 s; now well under a second).
- Opening, saving, autosaving and exporting a document, importing an image, a reference picture or a palette's picture, and importing brushes are read, decoded, encoded and written on other threads; the canvas waits (with a note) only while a document is being opened or saved.
- Brush previews (the presets list, the pop-up palette, the brush settings strip) are drawn on a thread of their own, only for the presets on screen; a brush that fails to draw loses its preview, not the app.
- Settings, presets, the brush library, swatches, gradients and panel sizes are written to disk on their own thread.
- A new stroke no longer waits for the last one to finish painting; undo, shortcuts, the layer panel and the menus wait their turn instead of holding up the frame.
- Pasting and copying images to the system clipboard happen off the UI thread.
- Brush presets load in parallel at startup.
- Filters, merges (down, visible, flatten) and fills run on the stroke worker, in order with the strokes: the window keeps drawing while a big blur works (the filter dialog closes at once), and undo, the layer panel and saving wait their turn.
- The Smudge and Blur tools and mixing brushes paint big dabs across all cores and convert colours without a table lookup per pixel: a 200 px smudge stroke about 40% faster, blur 45%, deform, clone and sharpen 15–20%.
- Big round and square tips, squashed or turned, work out only the pixels each row of a dab reaches, and fill their solid middle at once: only the anti-aliased edge is worked out pixel by pixel. A 326 px calligraphy tip squashed to 0.18 paints in under a quarter of the time, the same pixels.
- With the integrated GPU chosen, NVIDIA's Vulkan driver isn't loaded on a machine that has another GPU: listing its card woke the sleeping discrete GPU, about 2 s before the window opened and again as the app quit.
- The built-in paper textures are made in less than half the time at startup (each lattice point's gradient worked out once, not at every pixel; the same textures).
- Saving and autosaving encode the project and its flattened picture side by side, and the picture with one PNG filter for every row rather than a pick per row: a 4096² painting saves in about half the time (the file 0.3% larger).

### Brushes: what Clip Studio and Photoshop had
- Stay inside the lines (Brush → Options), as Clip Studio's: a stroke paints only the area the pen goes down in, found on the reference layers as the Fill tool finds it (its tolerance, gap closing and spread), within the selection. Mixing brushes too.
- Mixing brushes, as Photoshop's mixer: a paint load (the brush colour lasts so many brush widths, then the stroke runs dry and only smears what it carries), keeping the picked-up paint between strokes rather than cleaning the brush, and sampling all layers. The Smudge tool can smudge all layers too, and Krita's colour smudge presets bring their overlay mode.
- Paint behind (Brush → Blend): the paint goes under what's on the layer, showing where it's see-through.
- Tapers by percentage of the stroke (Clip Studio's): the whole stroke redrawn tapered when the pen lifts.
- Several tips taken there and back (first to last and back again), as Clip Studio's round trip.
- Purity (Brush → Randomness): every dab duller or purer, as Photoshop's.
- Anti-aliasing off, weak, medium or strong: round and square tips' edges fading over one to three pixels, as Clip Studio's levels.

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
- Wet paint dries in the background: the window no longer stutters while a big wash dries (each step was worked out on the UI thread, a frame's time or more on a laptop); about 12% less work a step.
- Big brushes falling behind the pen catch up faster: the stroke's pixels brought up to date about once a frame while samples queue, not after each one. Smudge and mixing brushes do a quarter to a third less work a dab and wake the thread pool only for big dabs. Small round dabs (particle and curve lines, fine pens) are worked out once per size (Particle Swarm, Experimental Webs: from up to half a second behind the pen to none).
- `scripts/flamegraph_brush.sh` and `examples/profile_presets`: any preset's flamegraph, and every preset's lag at a pen's pace, without the window.
- Wet paint flows while you paint, as Clip Studio's watercolour does: the stroke so far spreads, darkens at its edges and runs, and the dabs after go on top of it; one undo takes it all back.
- SVG brush tips (Krita's, or an `.svg` in the brushes folder), drawn again at the size the brush paints so they stay sharp however big; Krita's Stamp Leaves joins the default brushes.
- Bristle brushes dry along a curve (Krita's ink depletion curve) and can soak up the layer's colour where the stroke starts (Krita's soak ink); Krita's bristle presets bring both, and their ink amount and random offset (both were read under the wrong names).

### ibisPaint brushes
- Import brushes from ibisPaint: pick the picture of its brush QR codes (Export Brush QR Code) in Import brushes, a brush per code. Size, opacity, flow, spacing, pressure, speed, start and end, rotation, jitter (position, size, opacity, angle, hue, saturation, brightness), anti-aliasing, paper texture, particles (as a spray of the tip, each speck's colour jittered), outline brushes (as the stroke's edge), Double brushes' second tip and Water brushes' mixing come across; blurring, spacing jitter and Color brushes' tips are named in the import report.
- ibisPaint's tips (162) are in the tip list as `ibis …`, kept as their PNG until a stroke uses one; its paper textures (62) come with the brushes that use them. They're placeholders in `assets/ibis`, to be redrawn.

### Shader layers
- A layer drawn by a GLSL shader (Shadertoy style: `mainImage`, `iTime`, `iResolution`, `iMouse`, `iChannel0` = the layers below), animated live on the GPU with the layer's blend mode and opacity.
- One editor window per shader layer: syntax colouring, errors at their line as you type, play/pause, speed, templates, and Bake.
- Export, merging and saving use the current frame; the shader, its time and speed are saved with the project.

### 16-bit and 32-bit float documents
- A document can keep 16 bits per channel or 32-bit floats (linear light, with values past white) as well as 8 bits: pick it for a new canvas, or convert with Image → Colour Depth (one undo step).
- Brush strokes (every blend mode, the eraser, alpha lock, colour randomness) and gradients work at the document's full depth: soft airbrushing and gradients no longer band, and glazes too faint for 8 bits build up.
- Undo, redo and saved files keep the full depth (older versions open such files at 8 bits).
- PNG (16-bit) is composited at full precision; new TIFF (16-bit) and TIFF (32-bit float, linear) exports.
- Colour adjustments (levels, curves, hue/saturation...) and the Image menu (canvas and image size, crop, rotate, flip) work at full depth too.
- 16-bit Photoshop files open (layers, masks, ZIP-compressed channels too) as 16-bit documents, and deeper documents are written as 16-bit PSDs; 16-bit and float Krita documents open at their depth.
- Tools not yet working at full depth (blurs and other spatial filters, transforms, smudging, wet paint, fills and the rest) still paint at 8 bits: the tiles they change are rounded to 8 bits.

### Colour management
- Documents have a colour profile: sRGB, Display P3, Adobe RGB (1998), Rec. 2020, or an RGB ICC profile from a file. Image → Colour Profile assigns one (same numbers) or converts to one (same colours, with a rendering intent), each one undo step; saved with the document.
- The canvas is shown converted for the monitor's profile (View → Colour Management → Monitor), and View → Colour Management → Proof Colours shows how it would print on a CMYK profile (picked, or found on the system), with an optional gamut warning.
- Exports embed the document's profile (PNG, JPEG, WebP, TIFF at every depth, PSD); new TIFF (CMYK, for print) export through the print profile.
- Pictures are imported converted from the profile they carry to the document's; Photoshop and Krita documents open with theirs.
- Colour harmonies in the colour panel: complementary, split complementary, analogous, triadic and tetradic hues shown on the wheel and as swatches to pick.

### Puppet Warp
- Edit → Puppet Warp (Shift+W, the pin in the tool strip): click the paint to pin it (exactly there) and drag the pins to bend it, the rest following as rigidly as it can. The mesh follows the drawing's outline (holes too), with Density and Expansion; Rigid, Normal or Distort; live while dragging.
- Pins as in Photoshop: Shift+click picks several (they move together), Alt+click removes one, Alt+drag near one turns the paint round it, a pin's turn Auto or Fixed at an angle, Depth ▲/▼ for which part goes on top where the paint folds over itself, Remove pins and Remove all. Ctrl+Z / Ctrl+Shift+Z step through the pins while warping (past the first pin, it's cancelled).
- Works on the selected layer, or only the selected part of it; text and vector layers are rasterised first. Parts that don't touch, and parts without a pin, stay where they are. Enter (or another tool) applies it as one undo step, Esc puts the layer back.
- Warps can be changed later: a layer remembers its warps (saved with the document), and picking Puppet Warp on it again, with nothing selected, reopens the last one with its pins, on the paint from before it. Undo and redo bring the matching warp back with the pixels; painting on the layer (or any other change to it) lets it go.

### Skeletal animation
- Rig layers: bones with their setup pose (and how much they take from their parents), slots in draw order, region and weighted mesh attachments, two-bone IK, and animations of bones (turn, place, scale, shear), slot colours, attachment swaps and mesh deforms with linear, stepped and Bézier keys; drawn at the timeline's frame, saved with the document.
- File → Import Animation: Spine JSON (3.8 and 4.x, with its texture atlas or loose pictures), DragonBones JSON (5.x, with its texture atlas) and Lottie (layers as bones; images, solids and still shapes as pictures; precompositions), each from its published format; what can't come across is listed.
- The timeline's rig section: the animation playing, and each bone's turn and place, keyed at the frame showing (one undo step a gesture).
- File → Import Video as Frames (and dropping a video or an animation JSON on the window): videos through ffmpeg, GIFs, animated PNGs and WebPs as an animated layer, a drawing a frame.
- Layer → Rig → Make Rig from Layers: the picked layers (or a folder's) become one rig layer, each a part with a mesh following its paint (Puppet Warp's), its opacity, blend mode and visibility, in their order; the layers stay, hidden. One undo step. The rig comes with an animation as long as the timeline.
- The Bone tool (Y, the bone in the tool strip): on a rig layer, draw bones on the canvas (a drag from empty canvas, a child of the selected bone; Shift+drag from a bone's tail for its child), move one by its head (its children follow), aim and lengthen it by its tail, Delete to remove it (its children and parts go to its parent, staying where they are). Pose: drag a bone to turn it round its head, Shift+drag to move it, keyed at the frame showing. Bind smooth (each point weighted between the bones near it, measured along the part, so an arm doesn't follow the body across a gap) or rigid (each part on one bone); keys already made are kept as they look.
- Puppet Warp on a rig layer keys its parts: click a part to pin it, drag pins to bend it, and the bend is a key at the frame showing (each drag one undo step). Bend it at frame 1 and again at frame 12, and the frames between follow, eased per key (Linear, Ease in-out, in, out or Hold on the tool bar). Key this frame holds the shape where it is; Delete key takes one out. The pins are kept with the part and show again at any frame.
- Rig layers get onion skins: the rig posed at the frames before and after, tinted like a drawing's.
- Layer → Rig → New Animation, and New animation in the timeline's rig section.

### Animation
- Frame-by-frame animation: animated layers whose drawings start on a frame and are held until the next, a timeline panel (View → Timeline) to play, step and set the frame rate and range, add, copy, drag and remove drawings, and onion skins before and after.
- Undo works per drawing: a stroke undoes on the drawing it was made on, whatever frame is showing; adding, moving and removing drawings are undo steps too.
- File → Export Animation: GIF, animated PNG, MP4 and WebM (through ffmpeg); documents save their timeline.
- `,` and `.` step through frames, Shift+Space plays and pauses.

### Krita's engines, closer
- Spray: density mode (particles by the share of the area they cover), the spray area's aspect and turn, jitter moving the whole cloud, and each particle's own random hue, saturation, value and opacity and mix with the secondary colour; Krita presets bring them.
- Grid: cells taller or wider than square, divisions (also by pressure), a random border, and cells repainted by every dab; Krita presets bring them.
- Particle: each particle answering the pen's pull at its own rate, several steps per dab, dots instead of lines; Krita particle presets start them all at the pen as Krita does.
- Bristle: hairs from an image tip's pixels, shear, density, random offset, light pressure lifting hairs off, and colour fading as the hairs run dry; Krita bristle presets bring their settings.

### Fixed
- A brush with a time-based stabiliser no longer draws straight segments when the app falls behind: the mouse's moves (and a frame late pen's samples) kept no time of their own, so a slow frame's all came at one instant and the stabiliser jumped straight to the last one.
- A Photoshop brush file with a tip whose bounds are far apart is refused instead of crashing a debug build (or reading a wrong size in a release one).

### Development
- `cargo test` runs about five times faster (10 s → 2 s): tests build at opt-level 1, and the GPU tests share one device instead of making one each.
- A randomized history test: seeded runs of gradients, deletions, moves and layer changes with undo and redo among them, each landing exactly where it was.
- The biggest files are split: the brush (types and placing dabs / painting them into tiles), the Blend tools, documents, history, filters, the canvas renderer and Krita import keep their tests in files of their own, and the brush dynamics tests are grouped by subject.

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
