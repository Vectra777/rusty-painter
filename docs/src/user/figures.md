# 3D figures

A 3D figure layer holds posable 3D figures — a wooden mannequin, like the
artist's mannequin on a desk but with every joint down to the fingers,
realistic men, women, children and hands, and models you bring in — seen
through a camera you can move. Use it as a reference: pose it, frame it,
lower its opacity and draw over it on a layer above.

Besides posing by hand, hands can be posed from the camera (the
[Hand Scanner](#hand-scanner)) and the whole body from the camera or a
photo ([Pose from camera and photo](#pose-from-camera-and-photo)); poses
can be keyed on the timeline to [animate](#animating-poses), the camera
[matched to a perspective ruler](#matching-a-perspective-ruler), and the
layer [rasterised](#rasterising) into paint.

## Adding a figure

**Layer → New 3D Figure**, or the **3D** button at the top of the
Layers panel, adds a figure layer above the selected layer with the
mannequin standing in it, and picks the Figure tool. The layer shows **3D**
in the Layers panel; double-click it to pick the Figure tool again.

A figure layer is locked: its pixels are the rendered figures, so brushes
don't paint on it. Its blend mode and opacity work as on any layer.

### More figures and props

One layer can hold several figures and props. In the options bar's
**Scene** menu:

- **Add mannequin** puts another mannequin beside the others.
- **Add male**, **Add female**, **Add child**, **Add left hand** and **Add
  right hand** add one of the realistic figures (see below). A body stands
  at ease; a hand alone comes open, with the camera brought close to it.
- **Import model…** brings in a glTF model (`.glb`, or a `.gltf` with its
  data inside it). A model with a human skeleton (Mixamo, VRM/VRoid,
  Unreal, MakeHuman, Rigify, Biped names are recognised) is a figure you
  can pose like the mannequin; any other model is a prop. Dropping a
  `.glb`/`.gltf` file on the window does the same. With no figure layer
  selected, a new one is made for it. The model is kept in the project.
- The list picks a figure; **Delete selected** (or the `Delete` key with
  the Figure tool) removes it.
- **Snap to ground** puts the selected figure's lowest point on the
  ground.

### Realistic figures

Besides the mannequin, the app has realistic human figures: an adult man
and woman, a child of about eight, and a left and a right hand on their
own. Each has the full skeleton — three joints in every finger and the
thumb — so everything below works on them as on the mannequin: dragging,
presets, the hand pad, mirroring, copying a pose from one to another. Their
**Body** menu has their own shape sliders too: Heavy, Slim, Muscular, Soft
and Older (and Bust on the woman; Heavy and Slim on the child).

They come in a figure pack beside the app (a `figures` folder next to the
program, copied into the app's data folder by the Linux `install.sh`; inside
the APK on Android, unpacked on first use). Without it the Scene menu says
*The realistic figures aren't installed* and the mannequin works as ever.
Projects keep a realistic figure by name, not the model itself, so they
stay small; a project opened where the pack isn't installed shows each of
them as the mannequin, in the same pose.

The figures are made from the [MakeHuman](http://www.makehumancommunity.org)
project's assets (through MPFB 2, its Blender add-on), which its authors
released under [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/):
the figures are in the public domain too, free to use for anything. The
pack's `LICENSE.md` has the details; `scripts/make_figure_pack.sh` makes
them again.

## The Figure tool

Pick it from the tool strip (the person icon) or with `Shift+Y`, with a
figure layer selected. The figures' joints show as dots; the part under
the pointer lights up.

### Posing

- **Drag a part** to pose it: the point you grabbed follows the pointer and
  the joints above it bend to follow (inverse kinematics). Drag a hand and
  the arm follows, the elbow keeping the way it bends; drag a foot and the
  leg bends; drag the head and the neck and chest bend; drag a finger and
  the finger curls. Dragging the hips moves the whole figure.
- **Shift+drag** a part turns just that part's joint.
- **Alt+drag** moves the whole figure.
- **Click** a part or a dot to pick its joint. Three rings appear round it:
  red, green and blue, its own three axes. **Drag a ring** to turn the joint
  about that axis; everything beyond the joint turns with it. Every finger
  joint can be picked this way.
- **Double-click** a joint to **pin** it (a red diamond); double-click it
  again to unpin it. A pinned joint keeps its place while you pose the
  rest: pin both feet and drag the hips down, and the knees bend; pin a
  hand and drag the elbow, and the hand stays put.

Joints turn only as far as a human's do: elbows and knees bend one way,
fingers curl towards the palm, a neck doesn't turn round. To pose past
that, untick **Limits** in the options bar for the picked joint (its dot
gets an orange ring). **Pin** does the same as a double-click.

### Moving and turning a figure

The selected figure has a ring on the ground round its feet. Drag the
square in its middle to move the figure across the ground; drag the ring to
turn it round.

### The camera

- **Drag empty space** (or drag with the middle button) to orbit the
  camera round the scene. **Shift+drag** empty space pans the camera.
- **The mouse wheel** moves the camera closer or further. `Ctrl`+wheel still
  zooms the view.
- **On a touch screen**, with the Figure tool on a figure layer, two fingers
  move the figure's camera instead of the view: move them together to
  orbit, pinch to move closer or further, twist to roll. One finger poses
  (or orbits from empty space). Holding two or three fingers still does
  nothing here (it would start a pinch); a quick two-finger tap still
  undoes.
- A click on empty canvas lets go of the joint and the figure.

Each drag is one undo step (a run of wheel steps, a touch gesture too), and
the History panel names it: Pose, Move figure, Turn figure, Orbit camera,
Apply pose, Mirror pose, and so on.

## Poses

The options bar's **Pose** menu:

- **Body** poses: T-pose, A-pose, Standing, Walking, Running, Sitting,
  Kneeling, Reaching, Crouching, Lying. They pose the selected figure (the
  only one, if there's one), leave its fingers as they are and put it on
  the ground.
- **Mirror** swaps left and right.
- **Copy** and **Paste** carry a pose to another figure, even a different
  model: poses go by joint name.
- **Reset** puts every figure back in its standing pose; **Reset part** (in
  the bar) puts the picked joint and everything below it back at rest.
- **My poses**: **Save pose** keeps the selected figure's whole pose under
  the name typed, **Save hand** the chosen hand's (it fits either hand).
  Click a saved pose to use it, ✕ to forget it. They're kept in the app's
  data folder (`figure_poses.json`), for every document.

## Hands

The **Hand** menu works on the left or right hand of the selected figure:

- The **hand pad**: drag across it to spread the fingers, down it to curl
  them, from open to a fist. **Lock** a finger to keep its pose while you
  use the pad.
- **Hand poses**: Open, Relaxed, Fist, Point, Pinch, Peace, Thumbs up,
  Holding a cup, Holding a pen.

## Hand Scanner

**Hand → Hand Scanner…** (or a shortcut you set in Settings → Shortcuts)
opens a window with the camera's picture: hold your hand up to it and the
hand it finds is drawn over the picture, each finger's joints joined. It's
the same window as Pose from Camera, in its **Hand** mode; **Body** at its
top switches to posing the whole figure (see below) without stopping the
camera.

- **Pose**: which of the selected figure's hands to pose. **Auto** poses the
  hand the camera sees (your left hand poses the figure's left hand); pick
  **Left hand** or **Right hand** to pose that one whatever hand you show
  (the other hand poses it as a mirror would). With Auto, a figure that is
  only a hand is posed whichever hand you show.
- **Live** poses the hand as yours moves, smoothed so it doesn't shake. It's
  one undo step until you turn it off.
- **Capture** poses the hand as it is now, as one undo step.
- **Mirror** shows the picture as in a mirror (it doesn't change the pose).
- **Camera** picks the camera, when there are several; the app remembers
  the one you chose.

The fingers are posed, not the wrist: turn the hand with the Figure tool.
The scanner follows one hand at a time; keep the whole hand in the picture,
fingers apart when you can, and in good light.

The camera runs only while the window is open and the app is in front:
closing the window or switching to another app stops it. On Android the
app asks for permission to use the camera the first time; if you refuse,
allow it later in the system's settings for the app.

**Privacy**: everything happens on your device. The camera's pictures go to
the hand tracker and the preview only — they aren't recorded, saved or sent
anywhere — and the tracker (MediaPipe's hand models, run by the app itself)
needs no network.

The models ship beside the program (in the APK on Android). A build without
them says the pose tracking models aren't installed; a build made without
the `pose-capture` feature has no camera at all.

## Pose from camera and photo

The **Pose** menu's **Pose from camera…** opens the camera window in its
**Body** mode: stand back so the camera sees you, head to feet if you can,
and the body it finds is drawn over the picture (joints it can't see
faintly). It poses the selected figure (or the only one): the torso and
its lean, the head, both arms and legs, the feet, and with **Hands** ticked
the wrists and every finger too (each hand is read again close up, where
it's clear enough).

- **Live** poses the figure as you move, smoothed. It's one undo step until
  you turn it off (or switch to Hand, close the window, or leave the app).
- **Capture** poses the figure as you are now, as one undo step.
- **Mirror** shows the picture as in a mirror and poses the figure as your
  mirror image: raise your right arm and the figure raises its left, as a
  reflection would.
- **Turn the figure**: the whole figure turns as you're turned to the
  camera. Untick it to keep the figure facing as it does and take only the
  pose.

**Pose from photo…** asks for a picture (the file dialog; the system's
picker on Android and iOS), reads the people in it, and draws each body it
found over the photo. The biggest is picked to start with; click a body to
pick or unpick it (picked ones are drawn in colour, the others grey).
**Apply** poses the selected figure as the first body picked, and adds a
copy of it for each other one, all as one undo step; **Cancel** leaves the
figures alone. **Hands** and **Turn the figure** work as above. If no one
is found it says so.

**Place** moves each figure where its body stands to the photo's camera, as
the layer's camera sees it: the photo is fitted to the canvas, each
figure's hips land on the canvas where the body's are in the photo, and
its distance from the camera follows the body's size there (half the size,
twice as far). Several people picked are always placed, so they stand as
they do in the photo. Set the layer's **Lens** close to the photo's first
for the closest match.

People are found by their faces first. Drawings often have faces the finder
misses; when it finds no one, the whole picture is read as one body, which
works for a single figure filling most of the frame.

What it can't see it leaves alone: an arm out of the picture keeps the pose
it had, and so does a leg hidden behind a desk. Where along the spine a
bend happens is a guess (the camera sees only the hips and shoulders),
shared between the hips and the spine. Pose from Camera follows one person
at a time.

**Privacy**: as with the Hand Scanner, all of it happens on your device.
Camera pictures go to the tracker and the preview only, and a photo is
read in memory: neither is recorded, saved, copied or sent anywhere, and
the models (MediaPipe's BlazePose, run by the app itself) need no network.

## Body shape

The **Body** menu changes the selected figure's proportions: its height,
and for a figure with a human skeleton its head, shoulders, torso, arms,
legs, hands and build. A figure standing on the ground stays on it. A model
with morph targets (shape keys) also gets a slider for each. **Reset shape**
puts it all back.

## How it's drawn

The options bar also has:

- **Lens**: the camera's field of view. Wide (a high number) shows more of
  the scene with stronger perspective; narrow flattens it, like a long lens.
- **Reset camera**: the camera back where a new figure starts.
- **Match perspective ruler**: see
  [below](#matching-a-perspective-ruler).
- **Shaded**, **Toon** (two flat tones), **Silhouette** (one flat dark
  colour) or **Line art**: an outline round each figure and round parts in
  front of others, its width in canvas pixels, the inside white or (untick
  **White fill**) see-through — ready to trace.
- **Opacity**: the layer's opacity.

## Animating poses

The figures' poses can be keyed on the timeline (View → Timeline), as a
layer's motion is:

1. Go to a frame and pose the figures.
2. **Key pose** in the options bar keeps every figure of the layer as it is
   at that frame: its joints, where it stands and how it's turned, its
   size, body shape and morphs. The button shows ◆ on a frame with a key.
3. Go to another frame, pose them again, **Key pose** again.

Between keys each joint turns from one key's pose to the next the shorter
way round, at an even rate, and the rest goes straight from one to the
other; before the first key and after the last the figures hold still. The
timeline shows the keys as diamonds on the layer's row: drag one to move
it, right-click it to pick its easing (linear, ease in, ease out, ease in &
out, overshoot, hold) or to remove it. **Remove key** in the options bar takes away the key at
the frame showing. Each of these is one undo step.

Going to another frame poses the figures as the keys say, so a pose you
haven't keyed is replaced when you move to another frame: key it first.
Pins aren't keyed (they stay as they are), nor is the camera or the look.
Poses taken with **Capture** or from a photo can be keyed like any other;
**Live** doesn't record keys.

Playback, scrubbing and File → Export Animation show the figures as the
keys pose them at each frame.

## Matching a perspective ruler

With a 1-, 2- or 3-point perspective assistant shown (View → Assistants;
see [Shapes and painting aids](painting-aids.md)), **Match perspective ruler**
in the options bar turns the figure camera and sets its lens so the
scene's ground and uprights run to the ruler's vanishing points: a figure
then stands in the same perspective as the drawing. The most recently added
shown one is used; the button is greyed out without one. It's one undo step.

- **Three points**: the camera's turn, tilt and lens all come from them.
- **Two points**: the two on the horizon give the turn, the tilt and the
  lens (the uprights converge as a camera tilted that way would make them).
- **One point**: the camera looks along the ground towards it, its horizon
  level, with the usual lens (35°).

The camera has no lens shift, so the canvas's middle is taken as the centre
of vision: the further a two-point ruler's centre of vision is from the
middle, the less exactly the uprights match. Of the ways the figures could
face, the one closest to the camera's current turn is kept. Where the
camera stands and what it looks at stay; points that fit no camera (both
vanishing points on one side of the middle, say) are refused with a
message.

Pose from camera's **Turn the figure** turns the figure relative to this
camera: facing the real camera, the figure faces the figure camera.

## Rasterising

**Layer → Rasterise 3D Figure** (or the layer's right-click menu in the
Layers panel) makes a figure layer a plain pixel layer with the figures
drawn as they show now, unlocked for painting. The scene is gone from it
(undo brings it back); it's one undo step.

## Saving and exporting

Projects keep the figures, their poses, pins, shapes, pose keys, the
camera and look, and the imported models (the realistic figures by name),
and open with the scene as you left it. Export,
merging and thumbnails use the figures as rendered. Other apps (and PSD
files) see the rendered pixels.

A figure inside a folder, masked or clipped can't be drawn live: it shows as
its last render, made again whenever you change it.
