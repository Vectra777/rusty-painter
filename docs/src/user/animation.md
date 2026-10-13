# Animation

## Frame by frame

View → Timeline opens the timeline under the canvas; **Animate layer** makes the selected layer animated (its picture the first drawing).

Each animated layer has drawings that start on a frame and are held until the next. Add one blank or as a copy, drag it along, take it away; each is one undo step, and a stroke undoes on its own drawing whatever frame shows.

- Play at any frame rate over a range of frames. `Shift+Space` plays, `,` and `.` step.
- Onion skins show the drawings before (red) and after (green).
- Saved documents keep their timeline.

File → Export Animation writes a GIF, an animated PNG, MP4 or WebM (the last two with ffmpeg).

## Rigs

### Importing

File → Import Animation brings in Spine (JSON with its atlas or loose pictures), DragonBones (JSON with its texture atlas) and Lottie animations as rig layers: bones, slots, region and weighted mesh attachments, IK, and animated bones, colours, attachment swaps and mesh deforms, posed at the timeline's frame and redrawn as it plays.

### Making one

1. Layer → Rig → Make Rig from Layers turns the picked layers (or a folder) into a rig layer, each a part with a mesh.
2. The Bone tool (`Y`) draws its bones on the canvas, binds the parts to them (smooth or rigid) and poses them.
3. The timeline's rig section picks the animation (or makes a new one) and turns or moves bones, keyed at the frame showing.

Puppet Warp on a rig layer pins a part and keys the bend at the frame showing, so a pose at frame 1 and another at frame 12 make the frames between (eased per key). Rig layers have onion skins.

## Video in

File → Import Video as Frames brings a video (through ffmpeg), GIF, animated PNG or WebP in as an animated layer, a drawing a frame: the way in for Moho or Alight Motion work, through what they export.

## Time-lapse

File → Record Time-lapse keeps a frame after each change; export it as MP4 (with ffmpeg) or GIF.
