//! Pen input with pressure, as a stream of contact samples: from octotablet
//! on desktop (Windows Ink, Wayland) and from the patched winit on Android.
//!
//! The same pen also reaches egui as pointer (or touch) events, which the
//! canvas must then ignore: see [`TabletInput::pen_active`].

#[cfg(not(target_os = "android"))]
use octotablet::{
    builder::Builder,
    events::{Event, ToolEvent},
    tool,
};
#[cfg(not(target_os = "android"))]
use std::collections::HashMap;
#[cfg(not(target_os = "android"))]
use std::panic::{self, AssertUnwindSafe};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TabletPhase {
    Down,
    Move,
    Up,
    /// The system cancelled the contact (palm rejection, a system gesture):
    /// whatever it drew should be taken back. Only Android reports it.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
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
    pub is_eraser: bool,
    pub phase: TabletPhase,
}

/// A lean vector from angles from perpendicular along x and y (radians).
#[cfg_attr(target_os = "android", allow(dead_code))]
fn lean_from_xy([ax, ay]: [f32; 2]) -> [f32; 2] {
    let (x, y) = (ax.sin(), ay.sin());
    let len = (x * x + y * y).sqrt();
    if len > 1.0 {
        [x / len, y / len]
    } else {
        [x, y]
    }
}

/// Per-tool contact tracking.
#[cfg(not(target_os = "android"))]
#[derive(Default)]
struct ToolState {
    /// Last pose, in egui points.
    pos: Option<[f32; 2]>,
    pressure: f32,
    tilt: Option<[f32; 2]>,
    /// Touching, and its `Down` sample has been sent.
    down: bool,
    /// Touched, but no pose has arrived since: the `Down` sample waits for
    /// one so it carries the contact's real position and pressure.
    pending_down: bool,
    in_range: bool,
}

#[cfg(not(target_os = "android"))]
impl ToolState {
    /// Track one tool event, adding the contact samples it produces.
    fn apply(
        &mut self,
        event: &ToolEvent<'_>,
        zoom: f32,
        is_eraser: bool,
        out: &mut Vec<TabletSample>,
    ) {
        let mut emit = |state: &Self, phase| {
            if let Some(pos) = state.pos {
                out.push(TabletSample {
                    pos,
                    pressure: state.pressure,
                    tilt: state.tilt,
                    is_eraser,
                    phase,
                });
            }
        };
        match event {
            ToolEvent::In { .. } => self.in_range = true,
            ToolEvent::Down => self.pending_down = true,
            ToolEvent::Pose(pose) => {
                self.pos = Some([pose.position[0] / zoom, pose.position[1] / zoom]);
                self.pressure = pose.pressure.get().unwrap_or(1.0);
                self.tilt = pose.tilt.map(lean_from_xy);
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
            _ => {}
        }
    }
}

/// Pumps octotablet events into contact samples.
#[cfg(not(target_os = "android"))]
pub struct TabletInput {
    manager: octotablet::Manager,
    tools: HashMap<tool::ID, ToolState>,
}

#[cfg(not(target_os = "android"))]
impl TabletInput {
    /// Create a tablet input manager using the eframe creation context for a window handle.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some()
            && std::env::var_os("RUSTY_PAINTER_ENABLE_WAYLAND_TABLET").is_none()
        {
            log::warn!(
                "Skipping Wayland tablet initialization; set RUSTY_PAINTER_ENABLE_WAYLAND_TABLET=1 to force octotablet"
            );
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
            Ok(Ok(manager)) => Some(Self {
                manager,
                tools: HashMap::new(),
            }),
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
        // octotablet reports logical window pixels; egui points differ from
        // those only by the UI zoom factor.
        let zoom = ctx.zoom_factor();
        let mut out = Vec::new();
        let Ok(events) = self.manager.pump() else {
            return out;
        };
        for event in events {
            let Event::Tool { tool, event } = event else {
                continue;
            };
            let is_eraser = matches!(tool.tool_type, Some(tool::Type::Eraser));
            let state = self.tools.entry(tool.id()).or_default();
            state.apply(&event, zoom, is_eraser, &mut out);
        }
        out
    }

    /// Whether a pen is near or on the tablet. Its input may then also
    /// arrive as pointer or touch events (Windows reports a pen as touches),
    /// which the canvas ignores in favor of these samples.
    pub fn pen_active(&self) -> bool {
        self.tools.values().any(|t| t.in_range || t.down)
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
}

#[cfg(all(test, not(target_os = "android")))]
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
