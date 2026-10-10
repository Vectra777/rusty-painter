// rusty-painter patch: winit reports the Apple Pencil as touches with its
// force and altitude but no azimuth or roll, and only the newest of the
// samples UIKit coalesces into each event. Queue every Pencil sample for the
// app to read through `platform::ios::take_pen_samples`.

use std::collections::VecDeque;
use std::f64::consts::FRAC_PI_2;
use std::sync::Mutex;

use objc2::runtime::NSObjectProtocol;
use objc2::sel;
use objc2_foundation::NSSet;
use objc2_ui_kit::{UIEvent, UITouch, UITouchPhase, UITouchType, UIView};

use crate::platform::ios::{PenPhase, PenSample};

static PEN_SAMPLES: Mutex<VecDeque<PenSample>> = Mutex::new(VecDeque::new());
/// Samples kept if the app stops draining the queue.
const MAX_PEN_SAMPLES: usize = 4096;

/// Queue the Pencil's samples among `touches` (with those coalesced into a
/// move, oldest first).
pub(super) fn queue_pencil_touches(view: &UIView, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
    let scale = view.contentScaleFactor();
    for touch in touches {
        if touch.r#type() != UITouchType::Pencil {
            continue;
        }
        let touch_id = touch as *const UITouch as u64;
        let phase = match touch.phase() {
            UITouchPhase::Began => PenPhase::Down,
            UITouchPhase::Moved => PenPhase::Move,
            UITouchPhase::Ended => PenPhase::Up,
            UITouchPhase::Cancelled => PenPhase::Cancel,
            _ => continue,
        };
        let Ok(mut queue) = PEN_SAMPLES.lock() else {
            return;
        };
        let mut push = |t: &UITouch| {
            if queue.len() >= MAX_PEN_SAMPLES {
                queue.pop_front();
            }
            queue.push_back(sample(t, scale, touch_id, phase));
        };
        let coalesced = match (phase, event) {
            // SAFETY: `touch` belongs to `event`.
            (PenPhase::Move, Some(event)) => unsafe { event.coalescedTouchesForTouch(touch) },
            _ => None,
        };
        match coalesced {
            Some(all) if all.count() > 0 => all.iter().for_each(|t| push(t)),
            _ => push(touch),
        }
    }
}

fn sample(touch: &UITouch, scale: f64, touch_id: u64, phase: PenPhase) -> PenSample {
    // SAFETY: plain property reads on a live touch, in window coordinates.
    let at = unsafe { touch.preciseLocationInView(None) };
    let max = touch.maximumPossibleForce();
    let pressure = if max > 0.0 { (touch.force() / max).clamp(0.0, 1.0) } else { 1.0 };
    // `rollAngle` came with iOS 17.5 (Apple Pencil Pro): asked only where
    // UIKit has it.
    let roll = touch
        .respondsToSelector(sel!(rollAngle))
        .then(|| touch.rollAngle() as f32);
    PenSample {
        x: (at.x * scale) as f32,
        y: (at.y * scale) as f32,
        pressure: pressure as f32,
        // UIKit: altitude from the screen's plane (π/2 = upright).
        tilt: (FRAC_PI_2 - touch.altitudeAngle()).clamp(0.0, FRAC_PI_2) as f32,
        // UIKit: azimuth from the x axis, clockwise (y points down); turned
        // so 0 is up the screen.
        orientation: (touch.azimuthAngleInView(None) + FRAC_PI_2) as f32,
        roll,
        touch_id,
        phase,
    }
}

pub(crate) fn take_pen_samples() -> Vec<PenSample> {
    PEN_SAMPLES.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default()
}
