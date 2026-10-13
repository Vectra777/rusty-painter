//! rusty-painter patch: pens on Wayland's tablet protocol (tablet-v2).
//!
//! A compositor sends a tablet pen to a client through the tablet protocol
//! only, and KWin doesn't turn it into pointer events for one that doesn't
//! bind it: unhandled, the pen does nothing in the window. Here the pen is
//! the pointer (moves, a left button for the tip, middle and right for the
//! barrel buttons, as X11's pen drivers do) and, as on X11, each of its
//! frames while touching is also queued as a sample with its pressure and
//! tilt, for the app to read through `platform::wayland::take_pen_samples`.

use std::collections::VecDeque;
use std::sync::Mutex;

use ahash::AHashMap;

use sctk::globals::GlobalData;
use sctk::reexports::client::backend::ObjectId;
use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::{
    delegate_dispatch, event_created_child, Connection, Dispatch, Proxy, QueueHandle, WEnum,
};
use sctk::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use sctk::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use sctk::reexports::protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2::ZwpTabletManagerV2,
    zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
    zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2,
    zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2,
    zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
    zwp_tablet_seat_v2::{self, ZwpTabletSeatV2},
    zwp_tablet_tool_v2::{self, ButtonState, ZwpTabletToolV2},
    zwp_tablet_v2::ZwpTabletV2,
};

use crate::dpi::LogicalPosition;
use crate::event::{ElementState, MouseButton, WindowEvent};
use crate::platform::wayland::{PenPhase, PenSample};
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{self, DeviceId, WindowId};
use crate::window::CursorIcon;

static PEN_SAMPLES: Mutex<VecDeque<PenSample>> = Mutex::new(VecDeque::new());
/// Samples kept if the app stops draining the queue.
const MAX_PEN_SAMPLES: usize = 4096;

pub(crate) fn take_pen_samples() -> Vec<PenSample> {
    PEN_SAMPLES.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default()
}

fn push(sample: PenSample) {
    if let Ok(mut queue) = PEN_SAMPLES.lock() {
        if queue.len() >= MAX_PEN_SAMPLES {
            queue.pop_front();
        }
        queue.push_back(sample);
    }
}

/// The tablet manager, each seat's tablet seat, and the cursor shapes for
/// the pens' cursors.
pub struct TabletState {
    manager: ZwpTabletManagerV2,
    cursor_shapes: Option<WpCursorShapeManagerV1>,
    seats: AHashMap<ObjectId, ZwpTabletSeatV2>,
}

impl TabletState {
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<WinitState>,
        seats: impl Iterator<Item = WlSeat>,
    ) -> Result<Self, BindError> {
        let manager = globals.bind(queue_handle, 1..=1, GlobalData)?;
        let cursor_shapes = globals.bind(queue_handle, 1..=1, GlobalData).ok();
        let mut this = Self { manager, cursor_shapes, seats: AHashMap::default() };
        for seat in seats {
            this.add_seat(&seat, queue_handle);
        }
        Ok(this)
    }

    pub fn add_seat(&mut self, seat: &WlSeat, queue_handle: &QueueHandle<WinitState>) {
        let tablet_seat = self.manager.get_tablet_seat(seat, queue_handle, GlobalData);
        if let Some(old) = self.seats.insert(seat.id(), tablet_seat) {
            old.destroy();
        }
    }

    pub fn remove_seat(&mut self, seat: &ObjectId) {
        if let Some(tablet_seat) = self.seats.remove(seat) {
            tablet_seat.destroy();
        }
    }
}

/// A pen (or its eraser end, or another tool): its pose and what changed
/// since its last frame.
#[derive(Debug, Default)]
pub struct ToolData {
    inner: Mutex<Tool>,
}

#[derive(Debug, Default)]
struct Tool {
    is_eraser: bool,
    cursor: Option<WpCursorShapeDeviceV1>,
    /// The window the tool is over, and the serial of its arrival (for
    /// setting its cursor).
    window: Option<WindowId>,
    serial: u32,
    /// The cursor last set: `Some(None)` hidden.
    cursor_set: Option<Option<CursorIcon>>,
    /// Pose, surface-local logical pixels.
    position: (f64, f64),
    pressure: f32,
    tilt: Option<[f32; 2]>,
    wheel: Option<f32>,
    down: bool,
    /// This frame's changes.
    entered: bool,
    left: bool,
    moved: bool,
    pressed: bool,
    released: bool,
    buttons: Vec<(MouseButton, ElementState)>,
}

impl Dispatch<ZwpTabletToolV2, ToolData, WinitState> for TabletState {
    fn event(
        state: &mut WinitState,
        proxy: &ZwpTabletToolV2,
        event: zwp_tablet_tool_v2::Event,
        data: &ToolData,
        _conn: &Connection,
        queue_handle: &QueueHandle<WinitState>,
    ) {
        use zwp_tablet_tool_v2::Event;
        let mut tool = data.inner.lock().unwrap();
        match event {
            Event::Type { tool_type } => {
                tool.is_eraser = tool_type == WEnum::Value(zwp_tablet_tool_v2::Type::Eraser);
            },
            Event::Done => {
                if tool.cursor.is_none() {
                    tool.cursor = state
                        .tablet_state
                        .as_ref()
                        .and_then(|t| t.cursor_shapes.as_ref())
                        .map(|m| m.get_tablet_tool_v2(proxy, queue_handle, GlobalData));
                }
            },
            Event::ProximityIn { serial, surface, .. } => {
                let window_id = wayland::make_wid(&surface);
                // A window's own surface only (not its decorations).
                if state.windows.get_mut().contains_key(&window_id) {
                    tool.window = Some(window_id);
                    tool.serial = serial;
                    tool.cursor_set = None;
                    tool.entered = true;
                }
            },
            Event::ProximityOut => tool.left = true,
            Event::Down { .. } => tool.pressed = true,
            Event::Up => tool.released = true,
            Event::Motion { x, y } => {
                tool.position = (x, y);
                tool.moved = true;
            },
            Event::Pressure { pressure } => tool.pressure = pressure as f32 / 65535.0,
            Event::Tilt { tilt_x, tilt_y } => {
                tool.tilt = Some([(tilt_x as f32).to_radians(), (tilt_y as f32).to_radians()]);
            },
            // An Art Pen's barrel, and an airbrush's finger wheel: one axis,
            // 0..=1, as X11's "Abs Wheel".
            Event::Rotation { degrees } => {
                tool.wheel = Some((degrees as f32 / 360.0).rem_euclid(1.0));
            },
            Event::Slider { position } => {
                tool.wheel = Some(((position as f32 / 65535.0 + 1.0) * 0.5).clamp(0.0, 1.0));
            },
            Event::Button { button, state: button_state, .. } => {
                // Barrel buttons as X11's pen drivers have them.
                const BTN_STYLUS: u32 = 0x14b;
                const BTN_STYLUS2: u32 = 0x14c;
                let button = match button {
                    BTN_STYLUS => MouseButton::Middle,
                    BTN_STYLUS2 => MouseButton::Right,
                    other => MouseButton::Other(other as u16),
                };
                let pressed = button_state == WEnum::Value(ButtonState::Pressed);
                let element = if pressed { ElementState::Pressed } else { ElementState::Released };
                tool.buttons.push((button, element));
            },
            Event::Frame { .. } => frame(state, proxy, &mut tool),
            Event::Removed => {
                if let Some(cursor) = tool.cursor.take() {
                    cursor.destroy();
                }
                proxy.destroy();
            },
            _ => {},
        }
    }
}

/// A tool's frame: its changes as pointer events, and its contact as
/// samples.
fn frame(state: &mut WinitState, proxy: &ZwpTabletToolV2, tool: &mut Tool) {
    let Some(window_id) = tool.window else {
        tool.clear_frame();
        return;
    };
    let (scale_factor, wanted_cursor) = match state.windows.get_mut().get(&window_id) {
        Some(window) => {
            let window = window.lock().unwrap();
            (window.scale_factor(), window.pen_cursor())
        },
        None => {
            tool.window = None;
            tool.clear_frame();
            return;
        },
    };
    let device_id = crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(DeviceId));
    let position = LogicalPosition::new(tool.position.0, tool.position.1).to_physical(scale_factor);
    let sink = &mut state.events_sink;

    if tool.entered {
        sink.push_window_event(WindowEvent::CursorEntered { device_id }, window_id);
    }
    if tool.entered || tool.moved {
        sink.push_window_event(WindowEvent::CursorMoved { device_id, position }, window_id);
    }
    if tool.cursor_set != Some(wanted_cursor) {
        tool.set_cursor(proxy, wanted_cursor);
    }

    let sample = |tool: &Tool, phase| PenSample {
        x: position.x as f32,
        y: position.y as f32,
        pressure: tool.pressure,
        tilt: tool.tilt,
        wheel: tool.wheel,
        is_eraser: tool.is_eraser,
        phase,
    };
    if tool.pressed && !tool.down {
        tool.down = true;
        push(sample(tool, PenPhase::Down));
        sink.push_window_event(
            WindowEvent::MouseInput { device_id, state: ElementState::Pressed, button: MouseButton::Left },
            window_id,
        );
    } else if tool.moved && tool.down && !tool.released {
        push(sample(tool, PenPhase::Move));
    }
    for (button, element) in tool.buttons.drain(..) {
        sink.push_window_event(
            WindowEvent::MouseInput { device_id, state: element, button },
            window_id,
        );
    }
    // Leaving lifts the tip too.
    if (tool.released || tool.left) && tool.down {
        tool.down = false;
        push(sample(tool, PenPhase::Up));
        sink.push_window_event(
            WindowEvent::MouseInput { device_id, state: ElementState::Released, button: MouseButton::Left },
            window_id,
        );
    }
    if tool.left {
        sink.push_window_event(WindowEvent::CursorLeft { device_id }, window_id);
        tool.window = None;
    }
    tool.clear_frame();
}

impl Tool {
    fn clear_frame(&mut self) {
        self.entered = false;
        self.left = false;
        self.moved = false;
        self.pressed = false;
        self.released = false;
        self.buttons.clear();
    }

    /// Show the window's cursor for the tool (`None`: hidden, as the
    /// window hides the mouse's).
    fn set_cursor(&mut self, proxy: &ZwpTabletToolV2, icon: Option<CursorIcon>) {
        match (icon, &self.cursor) {
            (Some(icon), Some(device)) => device.set_shape(self.serial, shape(icon)),
            // No cursor-shape protocol: the compositor's own cursor stays.
            (Some(_), None) => {},
            // Hidden (a cursor-shape device can't hide it; no surface does).
            (None, _) => proxy.set_cursor(self.serial, None, 0, 0),
        }
        self.cursor_set = Some(icon);
    }
}

fn shape(icon: CursorIcon) -> Shape {
    match icon {
        CursorIcon::ContextMenu => Shape::ContextMenu,
        CursorIcon::Help => Shape::Help,
        CursorIcon::Pointer => Shape::Pointer,
        CursorIcon::Progress => Shape::Progress,
        CursorIcon::Wait => Shape::Wait,
        CursorIcon::Cell => Shape::Cell,
        CursorIcon::Crosshair => Shape::Crosshair,
        CursorIcon::Text => Shape::Text,
        CursorIcon::VerticalText => Shape::VerticalText,
        CursorIcon::Alias => Shape::Alias,
        CursorIcon::Copy => Shape::Copy,
        CursorIcon::Move => Shape::Move,
        CursorIcon::NoDrop => Shape::NoDrop,
        CursorIcon::NotAllowed => Shape::NotAllowed,
        CursorIcon::Grab => Shape::Grab,
        CursorIcon::Grabbing => Shape::Grabbing,
        CursorIcon::EResize => Shape::EResize,
        CursorIcon::NResize => Shape::NResize,
        CursorIcon::NeResize => Shape::NeResize,
        CursorIcon::NwResize => Shape::NwResize,
        CursorIcon::SResize => Shape::SResize,
        CursorIcon::SeResize => Shape::SeResize,
        CursorIcon::SwResize => Shape::SwResize,
        CursorIcon::WResize => Shape::WResize,
        CursorIcon::EwResize => Shape::EwResize,
        CursorIcon::NsResize => Shape::NsResize,
        CursorIcon::NeswResize => Shape::NeswResize,
        CursorIcon::NwseResize => Shape::NwseResize,
        CursorIcon::ColResize => Shape::ColResize,
        CursorIcon::RowResize => Shape::RowResize,
        CursorIcon::AllScroll => Shape::AllScroll,
        CursorIcon::ZoomIn => Shape::ZoomIn,
        CursorIcon::ZoomOut => Shape::ZoomOut,
        _ => Shape::Default,
    }
}

impl Dispatch<ZwpTabletManagerV2, GlobalData, WinitState> for TabletState {
    fn event(
        _: &mut WinitState,
        _: &ZwpTabletManagerV2,
        _: <ZwpTabletManagerV2 as Proxy>::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}

impl Dispatch<ZwpTabletSeatV2, GlobalData, WinitState> for TabletState {
    fn event(
        _: &mut WinitState,
        _: &ZwpTabletSeatV2,
        _: zwp_tablet_seat_v2::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        // The tablets, tools and pads it announces get their own handlers.
    }

    event_created_child!(WinitState, ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, GlobalData),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, ToolData::default()),
        zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (ZwpTabletPadV2, GlobalData),
    ]);
}

/// Tablets only describe themselves; pads (the tablet's own buttons and
/// rings) aren't used.
impl Dispatch<ZwpTabletV2, GlobalData, WinitState> for TabletState {
    fn event(
        _: &mut WinitState,
        tablet: &ZwpTabletV2,
        event: <ZwpTabletV2 as Proxy>::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        if let sctk::reexports::protocols::wp::tablet::zv2::client::zwp_tablet_v2::Event::Removed =
            event
        {
            tablet.destroy();
        }
    }
}

impl Dispatch<ZwpTabletPadV2, GlobalData, WinitState> for TabletState {
    fn event(
        _: &mut WinitState,
        pad: &ZwpTabletPadV2,
        event: zwp_tablet_pad_v2::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        if let zwp_tablet_pad_v2::Event::Removed = event {
            pad.destroy();
        }
    }

    event_created_child!(WinitState, ZwpTabletPadV2, [
        zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (ZwpTabletPadGroupV2, GlobalData),
    ]);
}

impl Dispatch<ZwpTabletPadGroupV2, GlobalData, WinitState> for TabletState {
    fn event(
        _: &mut WinitState,
        _: &ZwpTabletPadGroupV2,
        _: zwp_tablet_pad_group_v2::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }

    event_created_child!(WinitState, ZwpTabletPadGroupV2, [
        zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (ZwpTabletPadRingV2, GlobalData),
        zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (ZwpTabletPadStripV2, GlobalData),
    ]);
}

impl Dispatch<ZwpTabletPadRingV2, GlobalData, WinitState> for TabletState {
    fn event(
        _: &mut WinitState,
        _: &ZwpTabletPadRingV2,
        _: <ZwpTabletPadRingV2 as Proxy>::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}

impl Dispatch<ZwpTabletPadStripV2, GlobalData, WinitState> for TabletState {
    fn event(
        _: &mut WinitState,
        _: &ZwpTabletPadStripV2,
        _: <ZwpTabletPadStripV2 as Proxy>::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}

delegate_dispatch!(WinitState: [ZwpTabletManagerV2: GlobalData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletSeatV2: GlobalData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletV2: GlobalData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletToolV2: ToolData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletPadV2: GlobalData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletPadGroupV2: GlobalData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletPadRingV2: GlobalData] => TabletState);
delegate_dispatch!(WinitState: [ZwpTabletPadStripV2: GlobalData] => TabletState);
