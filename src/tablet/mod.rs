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
    pub phase: TabletPhase,
}

/// Minimal tablet bridge: pumps octotablet events and emits normalized samples.
pub struct TabletInput {
    #[cfg(not(target_os = "android"))]
    manager: octotablet::Manager,
    #[cfg(not(target_os = "android"))]
    tool_types: HashMap<tool::ID, bool>, // is eraser
}

impl TabletInput {
    /// Create a tablet input manager using the eframe creation context for a window handle.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        #[cfg(target_os = "android")]
        {
            let _ = cc;
            return None;
        }

        #[cfg(not(target_os = "android"))]
        {
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
                }),
                Ok(Err(e)) => {
                    log::error!("Failed to initialize tablet: {:?}", e);
                    None
                }
                Err(_) => {
                    log::error!(
                        "Tablet initialization panicked (likely missing COM classes in Wine)"
                    );
                    None
                }
            }
        }
    }

    /// Pump events and return a list of samples in logical egui points.
    pub fn poll(&mut self, scale: f32) -> Vec<TabletSample> {
        #[cfg(target_os = "android")]
        {
            let _ = scale;
            return Vec::new();
        }

        #[cfg(not(target_os = "android"))]
        {
            let mut out = Vec::new();
            let events = match self.manager.pump() {
                Ok(evts) => evts,
                Err(_) => return out,
            };
            for event in events {
                if let Event::Tool { tool, event } = event {
                    let is_eraser = matches!(tool.tool_type, Some(tool::Type::Eraser));
                    self.tool_types.entry(tool.id()).or_insert(is_eraser);
                    match event {
                        ToolEvent::Down => out.push(TabletSample {
                            pos: [0.0, 0.0],
                            pressure: 1.0,
                            phase: TabletPhase::Down,
                        }),
                        ToolEvent::Up | ToolEvent::Out | ToolEvent::Removed => {
                            out.push(TabletSample {
                                pos: [0.0, 0.0],
                                pressure: 0.0,
                                phase: TabletPhase::Up,
                            })
                        }
                        ToolEvent::Pose(mut pose) => {
                            pose.position = [pose.position[0] * scale, pose.position[1] * scale];
                            let pressure = pose.pressure.get().unwrap_or(1.0);
                            // Emit Move with real position; Down/Up already signaled separately.
                            out.push(TabletSample {
                                pos: pose.position,
                                pressure,
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
}
