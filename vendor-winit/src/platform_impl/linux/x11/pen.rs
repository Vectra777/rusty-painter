//! rusty-painter patch: pen pressure and tilt on X11.
//!
//! XInput2 reports a tablet pen as the master pointer, with the pen's own
//! axes (valuators) attached to each event; winit turns those events into
//! plain mouse events and drops the pressure. A pen is a physical device with
//! an "Abs Pressure" axis: its button 1 presses, motions and releases are also
//! queued here as samples with their pressure and tilt, for the app to read
//! through `platform::x11::take_pen_samples`.

use std::collections::VecDeque;
use std::ffi::CStr;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use x11_dl::xinput2::{self, XIDeviceEvent, XIValuatorClassInfo};

use super::ffi;
use super::XConnection;
use crate::platform::x11::{PenPhase, PenSample};

static PEN_SAMPLES: Mutex<VecDeque<PenSample>> = Mutex::new(VecDeque::new());
/// Samples kept if the app stops draining the queue.
const MAX_PEN_SAMPLES: usize = 4096;
/// The last pointer motion came from a pen (hovering or touching).
static PEN_IN_RANGE: AtomicBool = AtomicBool::new(false);

/// One pen axis: its valuator number and range.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Axis {
    number: i32,
    min: f64,
    max: f64,
}

impl Axis {
    fn normalized(&self, value: f64) -> f64 {
        if self.max > self.min {
            ((value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// A tilt reading as an angle from upright, in radians. Drivers report
    /// degrees (about -64..=64); a wider range is scaled onto that.
    fn tilt_radians(&self, value: f64) -> f64 {
        let degrees = if self.min >= -90.0 && self.max <= 90.0 {
            value
        } else {
            (self.normalized(value) * 2.0 - 1.0) * 64.0
        };
        degrees.clamp(-90.0, 90.0).to_radians()
    }
}

/// A pen's axes and contact state.
#[derive(Debug, Clone)]
pub(crate) struct Pen {
    pressure: Axis,
    tilt_x: Option<Axis>,
    tilt_y: Option<Axis>,
    is_eraser: bool,
    /// Last readings, for events that don't carry every axis.
    last_pressure: f64,
    last_tilt: [f64; 2],
    down: bool,
}

impl Pen {
    /// The pen axes of a physical device, if it has a pressure axis.
    pub(crate) fn from_device(xconn: &XConnection, info: &ffi::XIDeviceInfo) -> Option<Self> {
        let classes = unsafe {
            slice::from_raw_parts(
                info.classes as *const *const ffi::XIAnyClassInfo,
                info.num_classes as usize,
            )
        };
        let (mut pressure, mut tilt_x, mut tilt_y) = (None, None, None);
        for &class_ptr in classes {
            if unsafe { (*class_ptr)._type } != ffi::XIValuatorClass {
                continue;
            }
            let class = unsafe { &*(class_ptr as *const XIValuatorClassInfo) };
            if class.label == 0 {
                continue;
            }
            let name = unsafe {
                let raw = (xconn.xlib.XGetAtomName)(xconn.display, class.label);
                if raw.is_null() {
                    continue;
                }
                let name = CStr::from_ptr(raw).to_string_lossy().into_owned();
                (xconn.xlib.XFree)(raw as _);
                name
            };
            let axis = Axis { number: class.number, min: class.min, max: class.max };
            match name.as_str() {
                "Abs Pressure" => pressure = Some(axis),
                "Abs Tilt X" => tilt_x = Some(axis),
                "Abs Tilt Y" => tilt_y = Some(axis),
                _ => {},
            }
        }
        let device_name = unsafe { CStr::from_ptr(info.name) }.to_string_lossy().to_lowercase();
        Some(Pen {
            pressure: pressure?,
            tilt_x,
            tilt_y,
            is_eraser: device_name.contains("eraser"),
            last_pressure: 0.0,
            last_tilt: [0.0; 2],
            down: false,
        })
    }

    /// Read the axes an event carries, keeping the last value of the others.
    fn read(&mut self, event: &XIDeviceEvent) {
        let mask = unsafe {
            slice::from_raw_parts(event.valuators.mask, event.valuators.mask_len as usize)
        };
        let mut value = event.valuators.values;
        for i in 0..event.valuators.mask_len * 8 {
            if !xinput2::XIMaskIsSet(mask, i) {
                continue;
            }
            let v = unsafe { *value };
            value = unsafe { value.offset(1) };
            if i == self.pressure.number {
                self.last_pressure = self.pressure.normalized(v);
            } else if self.tilt_x.is_some_and(|a| a.number == i) {
                self.last_tilt[0] = self.tilt_x.unwrap().tilt_radians(v);
            } else if self.tilt_y.is_some_and(|a| a.number == i) {
                self.last_tilt[1] = self.tilt_y.unwrap().tilt_radians(v);
            }
        }
    }

    fn sample(&self, event: &XIDeviceEvent, phase: PenPhase) -> PenSample {
        PenSample {
            x: event.event_x as f32,
            y: event.event_y as f32,
            pressure: self.last_pressure as f32,
            tilt: (self.tilt_x.is_some() || self.tilt_y.is_some())
                .then(|| [self.last_tilt[0] as f32, self.last_tilt[1] as f32]),
            is_eraser: self.is_eraser,
            phase,
        }
    }

    /// Handle a button press (`Some(true)`), release (`Some(false)`) or
    /// motion (`None`) from this pen, queueing the samples it makes.
    pub(crate) fn handle(&mut self, event: &XIDeviceEvent, button: Option<bool>) {
        PEN_IN_RANGE.store(true, Ordering::Relaxed);
        self.read(event);
        let phase = match button {
            // Side buttons are only mouse buttons.
            Some(_) if event.detail != 1 => return,
            Some(true) if !self.down => {
                self.down = true;
                PenPhase::Down
            },
            Some(false) if self.down => {
                self.down = false;
                PenPhase::Up
            },
            None if self.down => PenPhase::Move,
            _ => return,
        };
        push(self.sample(event, phase));
    }
}

fn push(sample: PenSample) {
    if let Ok(mut queue) = PEN_SAMPLES.lock() {
        if queue.len() >= MAX_PEN_SAMPLES {
            queue.pop_front();
        }
        queue.push_back(sample);
    }
}

/// Pointer motion from a device that isn't a pen, or the pointer left.
pub(crate) fn pen_out_of_range() {
    PEN_IN_RANGE.store(false, Ordering::Relaxed);
}

pub(crate) fn take_pen_samples() -> Vec<PenSample> {
    PEN_SAMPLES.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default()
}

pub(crate) fn pen_in_range() -> bool {
    PEN_IN_RANGE.load(Ordering::Relaxed)
}

