// rusty-painter patch: winit reports the Apple Pencil as touches with its
// force and altitude but no azimuth or roll, and only the newest of the
// samples UIKit coalesces into each event. Queue every Pencil sample for the
// app to read through `platform::ios::take_pen_samples`, the samples UIKit
// predicts after them (flagged), and the measured values that replace
// estimated ones (`platform::ios::take_pen_updates`).

use std::collections::VecDeque;
use std::f64::consts::FRAC_PI_2;
use std::sync::Mutex;

use objc2::runtime::NSObjectProtocol;
use objc2::sel;
use objc2_foundation::NSSet;
use objc2_ui_kit::{UIEvent, UITouch, UITouchPhase, UITouchProperties, UITouchType, UIView};

use crate::platform::ios::{PenPhase, PenSample, PenUpdate};

static PEN_SAMPLES: Mutex<VecDeque<PenSample>> = Mutex::new(VecDeque::new());
static PEN_UPDATES: Mutex<VecDeque<PenUpdate>> = Mutex::new(VecDeque::new());
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
        let mut push = |t: &UITouch, predicted: bool| {
            if queue.len() >= MAX_PEN_SAMPLES {
                queue.pop_front();
            }
            let mut s = sample(t, scale, touch_id, phase);
            s.predicted = predicted;
            queue.push_back(s);
        };
        let coalesced = match (phase, event) {
            // SAFETY: `touch` belongs to `event`.
            (PenPhase::Move, Some(event)) => unsafe { event.coalescedTouchesForTouch(touch) },
            _ => None,
        };
        match coalesced {
            Some(all) if all.count() > 0 => all.iter().for_each(|t| push(t, false)),
            _ => push(touch, false),
        }
        if let (PenPhase::Move, Some(event)) = (phase, event) {
            // SAFETY: as above.
            if let Some(ahead) = unsafe { event.predictedTouchesForTouch(touch) } {
                ahead.iter().for_each(|t| push(t, true));
            }
        }
    }
}

/// Queue the measured values UIKit sends for Pencil samples it first
/// gave estimates for.
pub(super) fn queue_pencil_updates(touches: &NSSet<UITouch>) {
    let Ok(mut queue) = PEN_UPDATES.lock() else {
        return;
    };
    for touch in touches {
        if touch.r#type() != UITouchType::Pencil {
            continue;
        }
        // SAFETY: a plain property read on a live touch.
        let Some(index) = (unsafe { touch.estimationUpdateIndex() }) else {
            continue;
        };
        let (pressure, tilt, orientation) = values(touch);
        if queue.len() >= MAX_PEN_SAMPLES {
            queue.pop_front();
        }
        queue.push_back(PenUpdate { index: index.as_u64(), pressure, tilt, orientation });
    }
}

/// A touch's pressure (0..=1), tilt from upright and pointing direction
/// (radians, Android's conventions).
fn values(touch: &UITouch) -> (f32, f32, f32) {
    let max = touch.maximumPossibleForce();
    let pressure = if max > 0.0 { (touch.force() / max).clamp(0.0, 1.0) } else { 1.0 };
    (
        pressure as f32,
        // UIKit: altitude from the screen's plane (π/2 = upright).
        (FRAC_PI_2 - touch.altitudeAngle()).clamp(0.0, FRAC_PI_2) as f32,
        // UIKit: azimuth from the x axis, clockwise (y points down); turned
        // so 0 is up the screen.
        (touch.azimuthAngleInView(None) + FRAC_PI_2) as f32,
    )
}

fn sample(touch: &UITouch, scale: f64, touch_id: u64, phase: PenPhase) -> PenSample {
    // SAFETY: plain property reads on a live touch, in window coordinates.
    let at = unsafe { touch.preciseLocationInView(None) };
    let (pressure, tilt, orientation) = values(touch);
    // Measured values still to come for the pressure or angles.
    let angles = UITouchProperties::UITouchPropertyForce
        | UITouchProperties::UITouchPropertyAzimuth
        | UITouchProperties::UITouchPropertyAltitude;
    // SAFETY: plain property reads on a live touch.
    let awaiting = unsafe { touch.estimatedPropertiesExpectingUpdates() }
        .intersects(angles)
        .then(|| unsafe { touch.estimationUpdateIndex() })
        .flatten()
        .map(|n| n.as_u64());
    // `rollAngle` came with iOS 17.5 (Apple Pencil Pro): asked only where
    // UIKit has it.
    let roll = touch
        .respondsToSelector(sel!(rollAngle))
        .then(|| touch.rollAngle() as f32);
    PenSample {
        x: (at.x * scale) as f32,
        y: (at.y * scale) as f32,
        pressure,
        tilt,
        orientation,
        roll,
        touch_id,
        phase,
        time: touch.timestamp(),
        predicted: false,
        awaiting,
    }
}

pub(crate) fn take_pen_samples() -> Vec<PenSample> {
    PEN_SAMPLES.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default()
}

pub(crate) fn take_pen_updates() -> Vec<PenUpdate> {
    PEN_UPDATES.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default()
}
