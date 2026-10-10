//! Pen input with pressure, as a stream of contact samples: from octotablet
//! on Windows (Windows Ink), and from the patched winit on X11, Wayland,
//! Android and iOS.
//!
//! The same pen also reaches egui as pointer (or touch) events, which the
//! canvas must then ignore: see [`TabletInput::pen_active`].

#[cfg(not(mobile))]
use octotablet::{
    builder::Builder,
    events::{Event, ToolEvent},
    tool,
};
#[cfg(not(mobile))]
use std::collections::HashMap;
#[cfg(not(mobile))]
use std::panic::{self, AssertUnwindSafe};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TabletPhase {
    Down,
    Move,
    Up,
    /// The system cancelled the contact (palm rejection, a system gesture):
    /// whatever it drew should be taken back. Android and iOS report it.
    #[cfg_attr(not(mobile), allow(dead_code))]
    Cancel,
}

#[derive(Copy, Clone, Debug)]
pub struct TabletSample {
    /// Position in egui points.
    pub pos: [f32; 2],
    /// Raw pressure, 0..=1.
    pub pressure: f32,
    /// The pen's lean projected on the screen (y down): its direction is the
    /// way the pen leans, its length how far (0 upright, 1 flat). `None`
    /// when the tablet doesn't report tilt.
    pub tilt: Option<[f32; 2]>,
    /// The pen's turn about its own axis (radians, a screen angle), when
    /// the tablet reports it (Wacom's Art Pen).
    pub roll: Option<f32>,
    /// An airbrush pen's finger wheel, 0..=1, when it has one.
    pub wheel: Option<f32>,
    pub is_eraser: bool,
    pub phase: TabletPhase,
}

/// A lean vector from angles from perpendicular along x and y (radians).
#[cfg_attr(mobile, allow(dead_code))]
fn lean_from_xy([ax, ay]: [f32; 2]) -> [f32; 2] {
    let (x, y) = (ax.sin(), ay.sin());
    let len = (x * x + y * y).sqrt();
    if len > 1.0 {
        [x / len, y / len]
    } else {
        [x, y]
    }
}

/// Window pixels per egui point for octotablet's positions. Windows Ink
/// converts to pixels at the window's DPI (`GetDpiForWindow`), which under
/// winit's per-monitor awareness are physical pixels; Wayland reports
/// logical ones, which differ from points only by the UI zoom.
#[cfg(not(mobile))]
fn pose_scale(native_pixels_per_point: Option<f32>, zoom: f32) -> f32 {
    let native = if cfg!(windows) {
        native_pixels_per_point.filter(|p| p.is_finite() && *p > 0.0)
    } else {
        None
    };
    let scale = native.unwrap_or(1.0) * zoom;
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

/// Per-tool contact tracking.
#[cfg(not(mobile))]
#[derive(Default)]
struct ToolState {
    /// Last pose, in egui points.
    pos: Option<[f32; 2]>,
    pressure: f32,
    tilt: Option<[f32; 2]>,
    roll: Option<f32>,
    wheel: Option<f32>,
    /// Touching, and its `Down` sample has been sent.
    down: bool,
    /// Touched, but no pose has arrived since: the `Down` sample waits for
    /// one so it carries the contact's real position and pressure.
    pending_down: bool,
    /// A pose arrived in the current frame (a backend may send it before
    /// the frame's `Down`).
    posed_this_frame: bool,
    in_range: bool,
}

#[cfg(not(mobile))]
impl ToolState {
    /// Track one tool event, adding the contact samples it produces.
    fn apply(
        &mut self,
        event: &ToolEvent<'_>,
        scale: f32,
        is_eraser: bool,
        out: &mut Vec<TabletSample>,
    ) {
        let mut emit = |state: &Self, phase| {
            if let Some(pos) = state.pos {
                out.push(TabletSample {
                    pos,
                    pressure: state.pressure,
                    tilt: state.tilt,
                    roll: state.roll,
                    wheel: state.wheel,
                    is_eraser,
                    phase,
                });
            }
        };
        match event {
            ToolEvent::In { .. } => self.in_range = true,
            ToolEvent::Down => self.pending_down = true,
            ToolEvent::Pose(pose) => {
                self.pos = Some([pose.position[0] / scale, pose.position[1] / scale]);
                self.pressure = pose.pressure.get().unwrap_or(1.0);
                self.tilt = pose.tilt.map(lean_from_xy);
                self.roll = pose.roll.get();
                // An airbrush's wheel is its slider (-1..=1) on Wayland.
                self.wheel = pose.slider.get().map(|s| ((s + 1.0) * 0.5).clamp(0.0, 1.0));
                self.posed_this_frame = true;
                if self.pending_down {
                    self.pending_down = false;
                    self.down = true;
                    emit(self, TabletPhase::Down);
                } else if self.down {
                    emit(self, TabletPhase::Move);
                }
            }
            ToolEvent::Up | ToolEvent::Out | ToolEvent::Removed => {
                // A tap too quick for a pose still paints a dot.
                if self.pending_down {
                    self.pending_down = false;
                    self.down = true;
                    emit(self, TabletPhase::Down);
                }
                if self.down {
                    self.down = false;
                    emit(self, TabletPhase::Up);
                }
                if !matches!(event, ToolEvent::Up) {
                    self.in_range = false;
                }
            }
            // The frame's pose came before its `Down`: that pose is where
            // the contact starts.
            ToolEvent::Frame(_) => {
                if self.pending_down && self.posed_this_frame {
                    self.pending_down = false;
                    self.down = true;
                    emit(self, TabletPhase::Down);
                }
                self.posed_this_frame = false;
            }
            _ => {}
        }
    }
}

/// Pumps octotablet events into contact samples, and on X11 the patched
/// winit's pen samples.
#[cfg(not(mobile))]
pub struct TabletInput {
    /// `None` off Windows (the patched winit has the pen there) or when
    /// turned off.
    manager: Option<octotablet::Manager>,
    tools: HashMap<tool::ID, ToolState>,
    #[cfg(winit_pen)]
    winit: WinitPen,
}

/// The pen's contact on X11 and Wayland, from the samples the patched winit
/// queues (it also makes the pen the pointer there).
#[cfg(winit_pen)]
#[derive(Default)]
struct WinitPen {
    /// The pen is touching.
    down: bool,
    /// A pen sample has arrived: there is such a pen.
    seen: bool,
}

#[cfg(winit_pen)]
impl WinitPen {
    fn poll(&mut self, scale: f32, out: &mut Vec<TabletSample>) {
        // X11's and Wayland's samples have the same fields.
        macro_rules! drain {
            ($platform:ident) => {
                for s in winit::platform::$platform::take_pen_samples() {
                    use winit::platform::$platform::PenPhase;
                    let phase = match s.phase {
                        PenPhase::Down => TabletPhase::Down,
                        PenPhase::Move => TabletPhase::Move,
                        PenPhase::Up => TabletPhase::Up,
                    };
                    self.seen = true;
                    self.down = phase != TabletPhase::Up;
                    let pose = WinitPose {
                        pos: [s.x, s.y],
                        pressure: s.pressure,
                        tilt: s.tilt,
                        wheel: s.wheel,
                        is_eraser: s.is_eraser,
                    };
                    out.push(winit_sample(pose, scale, phase));
                }
            };
        }
        drain!(x11);
        drain!(wayland);
    }
}

/// A winit pen sample's pose: physical pixels, tilt angles along x and y.
#[cfg(winit_pen)]
struct WinitPose {
    pos: [f32; 2],
    pressure: f32,
    tilt: Option<[f32; 2]>,
    wheel: Option<f32>,
    is_eraser: bool,
}

/// A winit pen sample in egui points (`scale`: physical pixels per point).
#[cfg(winit_pen)]
fn winit_sample(s: WinitPose, scale: f32, phase: TabletPhase) -> TabletSample {
    TabletSample {
        pos: [s.pos[0] / scale, s.pos[1] / scale],
        pressure: s.pressure,
        tilt: s.tilt.map(lean_from_xy),
        // One axis, either kind of pen: a brush reads it as whichever it
        // follows.
        roll: s.wheel.map(|w| w * std::f32::consts::TAU),
        wheel: s.wheel,
        is_eraser: s.is_eraser,
        phase,
    }
}

#[cfg(not(mobile))]
impl TabletInput {
    /// Create a tablet input manager using the eframe creation context for a window handle.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        let manager = Self::octotablet(cc);
        // On X11 and Wayland the patched winit reports the pen.
        if manager.is_none() && !cfg!(winit_pen) {
            return None;
        }
        Some(Self {
            manager,
            tools: HashMap::new(),
            #[cfg(winit_pen)]
            winit: WinitPen::default(),
        })
    }

    fn octotablet(cc: &eframe::CreationContext<'_>) -> Option<octotablet::Manager> {
        // To rule the tablet out when something goes wrong at start-up.
        if std::env::var_os("RUSTY_PAINTER_DISABLE_TABLET").is_some() {
            log::info!("Tablet input turned off by RUSTY_PAINTER_DISABLE_TABLET");
            return None;
        }
        // Windows Ink only: the patched winit has the pen on X11 and Wayland
        // (as the pointer too, which octotablet's Wayland pen isn't: the
        // compositor sends it to the app as tablet events alone).
        if !cfg!(windows) {
            return None;
        }

        // The mouse stays a plain egui pointer; only real pens come through here.
        let builder = Builder::new().emulate_tool_from_mouse(false);

        // Wrap the unsafe and potentially panicking call (octotablet can panic on Windows/Wine if COM is missing)
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            // Safety: matches the octotablet eframe example; drops before window.
            unsafe { builder.build_raw(cc) }
        }));

        match result {
            Ok(Ok(manager)) => Some(manager),
            Ok(Err(e)) => {
                log::error!("Failed to initialize tablet: {:?}", e);
                None
            }
            Err(_) => {
                log::error!("Tablet initialization panicked (likely missing COM classes in Wine)");
                None
            }
        }
    }

    /// Pump events and return this frame's contact samples. Hovering sends
    /// none: only a touching pen paints.
    pub fn poll(&mut self, ctx: &eframe::egui::Context) -> Vec<TabletSample> {
        let scale = pose_scale(ctx.native_pixels_per_point(), ctx.zoom_factor());
        let mut out = Vec::new();
        #[cfg(winit_pen)]
        self.winit.poll(ctx.pixels_per_point(), &mut out);
        let Some(Ok(events)) = self.manager.as_mut().map(|m| m.pump()) else {
            return out;
        };
        for event in events {
            let Event::Tool { tool, event } = event else {
                continue;
            };
            let is_eraser = matches!(tool.tool_type, Some(tool::Type::Eraser));
            let state = self.tools.entry(tool.id()).or_default();
            state.apply(&event, scale, is_eraser, &mut out);
        }
        out
    }

    /// Whether a pen is near or on the tablet. Its input may then also
    /// arrive as pointer or touch events (Windows reports a pen as touches),
    /// which the canvas ignores in favor of these samples.
    pub fn pen_active(&self) -> bool {
        #[cfg(winit_pen)]
        if self.winit.down {
            return true;
        }
        self.tools.values().any(|t| t.in_range || t.down)
    }

    /// The pen also moves the mouse pointer (X11, Wayland), rather than
    /// arriving as touches (Windows): the pointer events then belong to the
    /// pen while it touches.
    pub fn pen_is_pointer(&self) -> bool {
        #[cfg(winit_pen)]
        if self.winit.seen {
            return true;
        }
        false
    }
}

/// Drains the stylus samples the patched winit queues (every batched sample,
/// with its pressure).
#[cfg(target_os = "android")]
pub struct TabletInput {
    down: bool,
}

#[cfg(target_os = "android")]
impl TabletInput {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Option<Self> {
        Some(Self { down: false })
    }

    pub fn poll(&mut self, ctx: &eframe::egui::Context) -> Vec<TabletSample> {
        use winit::platform::android::PenPhase;
        let scale = ctx.pixels_per_point();
        let samples: Vec<TabletSample> = winit::platform::android::take_pen_samples()
            .into_iter()
            .map(|s| TabletSample {
                pos: [s.x / scale, s.y / scale],
                pressure: s.pressure,
                // Android: angle from perpendicular, and the direction the
                // pen points (0 up, clockwise).
                tilt: Some({
                    let lean = s.tilt.clamp(0.0, std::f32::consts::FRAC_PI_2).sin();
                    [s.orientation.sin() * lean, -s.orientation.cos() * lean]
                }),
                roll: None,
                wheel: None,
                is_eraser: s.is_eraser,
                phase: match s.phase {
                    PenPhase::Down => TabletPhase::Down,
                    PenPhase::Move => TabletPhase::Move,
                    PenPhase::Up => TabletPhase::Up,
                    PenPhase::Cancel => TabletPhase::Cancel,
                },
            })
            .collect();
        if let Some(last) = samples.last() {
            self.down = matches!(last.phase, TabletPhase::Down | TabletPhase::Move);
        }
        samples
    }

    /// Whether the pen is touching: its mouse events are then the pen's, and
    /// any finger touching the screen is the hand holding it. Hovering
    /// doesn't count, so fingers can still pinch with the pen nearby.
    pub fn pen_active(&self) -> bool {
        self.down
    }

    /// The pen is the mouse pointer.
    pub fn pen_is_pointer(&self) -> bool {
        true
    }
}

/// Drains the Apple Pencil samples the patched winit queues (every
/// coalesced sample, with its pressure, tilt and roll).
#[cfg(target_os = "ios")]
pub struct TabletInput {
    down: bool,
}

#[cfg(target_os = "ios")]
impl TabletInput {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Option<Self> {
        Some(Self { down: false })
    }

    pub fn poll(&mut self, ctx: &eframe::egui::Context) -> Vec<TabletSample> {
        use winit::platform::ios::PenPhase;
        let scale = ctx.pixels_per_point();
        let samples: Vec<TabletSample> = winit::platform::ios::take_pen_samples()
            .into_iter()
            .map(|s| TabletSample {
                pos: [s.x / scale, s.y / scale],
                pressure: s.pressure,
                // As on Android: angle from perpendicular, and the direction
                // the pen points (0 up, clockwise).
                tilt: Some({
                    let lean = s.tilt.clamp(0.0, std::f32::consts::FRAC_PI_2).sin();
                    [s.orientation.sin() * lean, -s.orientation.cos() * lean]
                }),
                roll: s.roll,
                wheel: None,
                // The Pencil has no eraser end.
                is_eraser: false,
                phase: match s.phase {
                    PenPhase::Down => TabletPhase::Down,
                    PenPhase::Move => TabletPhase::Move,
                    PenPhase::Up => TabletPhase::Up,
                    PenPhase::Cancel => TabletPhase::Cancel,
                },
            })
            .collect();
        if let Some(last) = samples.last() {
            self.down = matches!(last.phase, TabletPhase::Down | TabletPhase::Move);
        }
        samples
    }

    /// Whether the Pencil is touching: any finger on the screen is then the
    /// hand holding it.
    pub fn pen_active(&self) -> bool {
        self.down
    }

    /// The Pencil is the pointer: egui-winit makes the first touch, the
    /// Pencil's too, the mouse.
    pub fn pen_is_pointer(&self) -> bool {
        true
    }
}

#[cfg(all(test, not(mobile)))]
mod tests {
    use super::*;

    fn pose(x: f32, y: f32, pressure: f32) -> ToolEvent<'static> {
        let mut pose = octotablet::axis::Pose {
            position: [x, y],
            ..Default::default()
        };
        pose.pressure = octotablet::util::NicheF32::new_some(pressure).unwrap();
        ToolEvent::Pose(pose)
    }

    fn run(events: &[ToolEvent<'static>]) -> Vec<(TabletPhase, [f32; 2], f32)> {
        let mut state = ToolState::default();
        let mut out = Vec::new();
        for event in events {
            state.apply(event, 2.0, false, &mut out);
        }
        out.iter().map(|s| (s.phase, s.pos, s.pressure)).collect()
    }

    #[test]
    fn hovering_sends_nothing() {
        assert!(run(&[pose(10.0, 10.0, 0.0), pose(20.0, 10.0, 0.0)]).is_empty());
    }

    #[test]
    fn tilt_angles_become_a_lean_on_the_screen() {
        let upright = lean_from_xy([0.0, 0.0]);
        assert_eq!(upright, [0.0, 0.0]);
        // Leaning 30° to the right: half way to flat, pointing right.
        let [x, y] = lean_from_xy([std::f32::consts::FRAC_PI_6, 0.0]);
        assert!((x - 0.5).abs() < 1e-5 && y.abs() < 1e-6, "{x} {y}");
        // Readings past flat stay no longer than flat.
        let [x, y] = lean_from_xy([1.4, 1.4]);
        assert!(((x * x + y * y).sqrt() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_contact_starts_at_its_first_pose_with_its_pressure() {
        let samples = run(&[
            pose(10.0, 10.0, 0.0),
            ToolEvent::Down,
            pose(12.0, 10.0, 0.25),
            pose(14.0, 10.0, 0.5),
            ToolEvent::Up,
            pose(30.0, 30.0, 0.0),
        ]);
        // Positions are logical pixels over the UI zoom (2.0 here).
        assert_eq!(
            samples,
            vec![
                (TabletPhase::Down, [6.0, 5.0], 0.25),
                (TabletPhase::Move, [7.0, 5.0], 0.5),
                (TabletPhase::Up, [7.0, 5.0], 0.5),
            ]
        );
    }

    #[test]
    fn a_pose_before_its_down_in_the_same_frame_starts_the_contact() {
        let samples = run(&[
            pose(10.0, 10.0, 0.0),
            ToolEvent::Frame(None),
            pose(30.0, 40.0, 0.25),
            ToolEvent::Down,
            ToolEvent::Frame(None),
            pose(32.0, 40.0, 0.5),
            ToolEvent::Frame(None),
        ]);
        assert_eq!(
            samples,
            vec![
                (TabletPhase::Down, [15.0, 20.0], 0.25),
                (TabletPhase::Move, [16.0, 20.0], 0.5),
            ]
        );
        // A hover pose from an earlier frame doesn't start it: the next
        // pose does.
        let samples = run(&[
            pose(10.0, 10.0, 0.0),
            ToolEvent::Frame(None),
            ToolEvent::Down,
            ToolEvent::Frame(None),
            pose(12.0, 10.0, 0.3),
        ]);
        assert_eq!(samples, vec![(TabletPhase::Down, [6.0, 5.0], 0.3)]);
    }

    #[test]
    fn positions_are_scaled_to_points() {
        // Windows Ink sends physical pixels; elsewhere only the UI zoom
        // separates window pixels from points.
        let native = if cfg!(windows) { 1.5 } else { 1.0 };
        assert_eq!(pose_scale(Some(1.5), 2.0), native * 2.0);
        assert_eq!(pose_scale(None, 1.0), 1.0);
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(pose_scale(Some(bad), 1.0), 1.0);
            assert_eq!(pose_scale(Some(1.0), bad), 1.0);
        }
    }

    #[test]
    fn a_quick_tap_still_paints_a_dot() {
        let samples = run(&[pose(10.0, 10.0, 0.4), ToolEvent::Down, ToolEvent::Up]);
        let phases: Vec<_> = samples.iter().map(|s| s.0).collect();
        assert_eq!(phases, vec![TabletPhase::Down, TabletPhase::Up]);
    }

    #[test]
    fn leaving_while_touching_ends_the_contact() {
        let samples = run(&[
            ToolEvent::Down,
            pose(10.0, 10.0, 0.4),
            ToolEvent::Out,
            pose(12.0, 10.0, 0.4),
        ]);
        let phases: Vec<_> = samples.iter().map(|s| s.0).collect();
        assert_eq!(phases, vec![TabletPhase::Down, TabletPhase::Up]);
    }
}

#[cfg(all(test, winit_pen))]
mod winit_tests {
    use super::*;

    #[test]
    fn winit_samples_are_in_points_with_a_lean() {
        let s = WinitPose {
            pos: [200.0, 100.0],
            pressure: 0.5,
            tilt: Some([std::f32::consts::FRAC_PI_6, 0.0]),
            wheel: Some(0.25),
            is_eraser: true,
        };
        let t = winit_sample(s, 2.0, TabletPhase::Move);
        assert_eq!(t.pos, [100.0, 50.0]);
        assert_eq!(t.pressure, 0.5);
        assert!(t.is_eraser);
        let [x, y] = t.tilt.unwrap();
        assert!((x - 0.5).abs() < 1e-5 && y.abs() < 1e-6);
        assert_eq!(t.wheel, Some(0.25));
        assert!((t.roll.unwrap() - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    }
}
