# Tablets and pens

| Platform | What works |
|---|---|
| Windows | Pressure, tilt and the eraser end (Windows Ink) |
| Linux, X11 | Pressure, tilt and the eraser through XInput2 (Wacom and libinput drivers) |
| Linux, Wayland | Pressure, tilt and the eraser through the tablet protocol, OpenTabletDriver's Artist Mode included; the pen is the pointer too |
| Android | Every stylus sample; a resting palm is ignored while the pen is down |
| iPad | The Apple Pencil's pressure, tilt and, on the Pencil Pro, barrel roll, at its full 240 Hz |

## Calibration

Settings → Calibrate fits the pen's pressure curve to a few test strokes. Each brush setting's own pressure curve applies on top of it.

## Touch

In touch mode the top bar has the menu sheet, finger painting, undo and redo. The first time, a short list of the gestures shows over the canvas; Help → Gestures & Shortcuts lists them all.

| Gesture | What it does |
|---|---|
| One finger | Paints (pans instead when Paint with one finger is off) |
| Two fingers | Pan, pinch to zoom, twist to rotate; near upright it snaps straight |
| Two-finger tap | Undo |
| Two fingers held still | Undo, again and again until you lift |
| Three-finger tap | Redo |
| Three fingers held still | The QuickMenu |
| One finger held still on the canvas | Picks a colour; lifting goes back to your tool (Settings → Touch & pen can turn it off) |
| Long press a layer, brush, swatch, frame or preset | Its menu |
| Long press a slider | Type a value; dragging above or below the bar moves it in finer steps |
| Drag the colour swatch onto the canvas | Fills there; keep holding to slide the threshold left or right, lift to keep it |

With Hide side panels while painting on, the tools fade while you paint: tap the tab at the left edge to bring them back. Ctrl, Alt and Shift keys sit in the bottom-left corner: tap one to keep it down (tap again to let go), or hold it while you paint with the pen.

The QuickMenu is six actions in a ring round your fingers (J on a keyboard). Long press a slot, or set them in Settings → Touch & pen, to choose what each runs. Settings → Touch & pen also leaves tools you don't use off the strip, and Settings → Appearance sets the interface size.
