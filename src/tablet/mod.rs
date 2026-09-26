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
}

#[derive(Copy, Clone, Debug)]
pub struct TabletSample {
    pub pos: [f32; 2],
    pub pressure: f32,
    pub is_eraser: bool,
    pub phase: TabletPhase,
}

/// Latest pen sample from the platform, where the OS delivers a stylus as
/// plain pointer events (Android): pressure and tool arrive out of band.
#[derive(Copy, Clone, Debug)]
pub struct PenState {
    pub pressure: f32,
    pub is_eraser: bool,
    /// False when the last pointer was a real mouse.
    pub is_stylus: bool,
}

#[cfg(target_os = "android")]
pub fn pen_state() -> Option<PenState> {
    let pen = winit::platform::android::pen_state();
    Some(PenState {
        pressure: pen.pressure,
        is_eraser: pen.is_eraser,
        is_stylus: pen.is_stylus,
    })
}

/// Desktop pens go through [`TabletInput`] instead.
#[cfg(not(target_os = "android"))]
pub fn pen_state() -> Option<PenState> {
    None
}

/// Minimal tablet bridge: pumps octotablet events and emits normalized samples.
#[cfg(not(target_os = "android"))]
pub struct TabletInput {
    manager: octotablet::Manager,
    tool_types: HashMap<tool::ID, bool>, // is eraser
    tool_positions: HashMap<tool::ID, [f32; 2]>,
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

        let builder = Builder::new().emulate_tool_from_mouse(true);

        // Wrap the unsafe and potentially panicking call (octotablet can panic on Windows/Wine if COM is missing)
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            // Safety: matches the octotablet eframe example; drops before window.
            unsafe { builder.build_raw(cc) }
        }));

        match result {
            Ok(Ok(manager)) => Some(Self {
                manager,
                tool_types: HashMap::new(),
                tool_positions: HashMap::new(),
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

    /// Pump events and return a list of samples in logical egui points.
    pub fn poll(&mut self, scale: f32) -> Vec<TabletSample> {
        let mut out = Vec::new();
        let events = match self.manager.pump() {
            Ok(evts) => evts,
            Err(_) => return out,
        };
        for event in events {
            if let Event::Tool { tool, event } = event {
                let is_eraser = matches!(tool.tool_type, Some(tool::Type::Eraser));
                let tool_id = tool.id();
                self.tool_types.entry(tool_id.clone()).or_insert(is_eraser);
                match event {
                    ToolEvent::Down => {
                        if let Some(pos) = self.tool_positions.get(&tool_id).copied() {
                            out.push(TabletSample {
                                pos,
                                pressure: 1.0,
                                is_eraser,
                                phase: TabletPhase::Down,
                            });
                        }
                    }
                    ToolEvent::Up | ToolEvent::Out | ToolEvent::Removed => {
                        if let Some(pos) = self.tool_positions.get(&tool_id).copied() {
                            out.push(TabletSample {
                                pos,
                                pressure: 0.0,
                                is_eraser,
                                phase: TabletPhase::Up,
                            });
                        }
                    }
                    ToolEvent::Pose(mut pose) => {
                        pose.position = [pose.position[0] * scale, pose.position[1] * scale];
                        let pressure = pose.pressure.get().unwrap_or(1.0);
                        self.tool_positions.insert(tool_id, pose.position);
                        // Emit Move with real position; Down/Up already signaled separately.
                        out.push(TabletSample {
                            pos: pose.position,
                            pressure,
                            is_eraser,
                            phase: TabletPhase::Move,
                        });
                    }
                    _ => {}
                }
            }
        }
        out
    }
}

#[cfg(target_os = "android")]
pub struct TabletInput;

#[cfg(target_os = "android")]
impl TabletInput {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Option<Self> {
        None
    }

    pub fn poll(&mut self, _scale: f32) -> Vec<TabletSample> {
        Vec::new()
    }
}
