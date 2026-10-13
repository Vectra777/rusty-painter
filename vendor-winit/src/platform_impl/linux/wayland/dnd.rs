//! Files dropped on a window (`wl_data_device` drag and drop).
//!
//! Only file drops are taken: the offer must carry `text/uri-list`. On the
//! drop the list is read without blocking the loop, then every `file://`
//! entry becomes a `WindowEvent::DroppedFile`.

use std::io::Read;
use std::path::PathBuf;

use sctk::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use sctk::data_device_manager::data_source::DataSourceHandler;
use sctk::data_device_manager::{DataDeviceManagerState, WritePipe};
use sctk::reexports::calloop::PostAction;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, QueueHandle};

use crate::dpi::LogicalPosition;
use crate::event::WindowEvent;
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{make_wid, DeviceId};

const URI_LIST: &str = "text/uri-list";

impl WinitState {
    fn drag_offer(&self, device: &WlDataDevice) -> Option<DragOffer> {
        self.data_devices
            .values()
            .find(|d| d.inner() == device)?
            .data()
            .drag_offer()
    }
}

impl WinitState {
    /// Report the drag's position as the cursor's, so the app knows where
    /// a drop will land (the pointer itself sends nothing during a drag).
    fn drag_moved(&mut self, surface: &WlSurface, x: f64, y: f64) {
        let window_id = make_wid(surface);
        let Some(window) = self.windows.get_mut().get(&window_id) else {
            return;
        };
        let scale_factor = window.lock().unwrap().scale_factor();
        let position = LogicalPosition::new(x, y).to_physical(scale_factor);
        let device_id = crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(DeviceId));
        self.events_sink.push_window_event(
            WindowEvent::CursorMoved {
                device_id,
                position,
            },
            window_id,
        );
    }
}

fn has_uri_list(offer: &DragOffer) -> bool {
    offer.with_mime_types(|mimes| mimes.iter().any(|m| m == URI_LIST))
}

/// The local paths in a `text/uri-list` (RFC 2483): one URI per line,
/// `#` comments, percent-encoded bytes.
pub(crate) fn parse_uri_list(list: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    String::from_utf8_lossy(list)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|uri| uri.strip_prefix("file://"))
        // `file://host/path`: the path starts at the first slash.
        .filter_map(|rest| rest.find('/').map(|i| &rest[i..]))
        .map(|path| PathBuf::from(std::ffi::OsString::from_vec(percent_decode(path))))
        .collect()
}

fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

impl DataDeviceHandler for WinitState {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        x: f64,
        y: f64,
        surface: &WlSurface,
    ) {
        self.drag_moved(surface, x, y);
        let Some(offer) = self.drag_offer(device) else {
            return;
        };
        if has_uri_list(&offer) {
            offer.accept_mime_type(offer.serial, Some(URI_LIST.to_string()));
            offer.set_actions(DndAction::Copy, DndAction::Copy);
        } else {
            offer.accept_mime_type(offer.serial, None);
        }
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        x: f64,
        y: f64,
    ) {
        if let Some(offer) = self.drag_offer(device) {
            self.drag_moved(&offer.surface, x, y);
        }
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn drop_performed(&mut self, conn: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        let Some(offer) = self.drag_offer(device) else {
            return;
        };
        if !has_uri_list(&offer) {
            offer.finish();
            offer.destroy();
            return;
        }
        let window_id = make_wid(&offer.surface);
        let pipe = match offer.receive(URI_LIST.to_string()) {
            Ok(pipe) => pipe,
            Err(err) => {
                tracing::warn!("Couldn't read the dropped files: {err}");
                offer.destroy();
                return;
            }
        };
        // Sends the receive request, so the source starts writing.
        let _ = conn.flush();
        let mut list = Vec::new();
        let inserted = self.loop_handle.insert_source(pipe, move |_, file, state| {
            let mut buf = [0u8; 4096];
            // SAFETY: the file is only read, never closed here.
            match unsafe { file.get_mut() }.read(&mut buf) {
                Ok(0) => {
                    for path in parse_uri_list(&list) {
                        state
                            .events_sink
                            .push_window_event(WindowEvent::DroppedFile(path), window_id);
                    }
                    offer.finish();
                    offer.destroy();
                    PostAction::Remove
                }
                Ok(n) => {
                    list.extend_from_slice(&buf[..n]);
                    PostAction::Continue
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => PostAction::Continue,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => PostAction::Continue,
                Err(err) => {
                    tracing::warn!("Couldn't read the dropped files: {err}");
                    offer.finish();
                    offer.destroy();
                    PostAction::Remove
                }
            }
        });
        if let Err(err) = inserted {
            tracing::warn!("Couldn't read the dropped files: {err}");
        }
    }
}

impl DataOfferHandler for WinitState {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        offer: &mut DragOffer,
        _: DndAction,
    ) {
        offer.set_actions(DndAction::Copy, DndAction::Copy);
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

/// Winit never offers data itself; these are never called.
impl DataSourceHandler for WinitState {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: String,
        _: WritePipe,
    ) {
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

/// The data device for `seat`, when the compositor has drag and drop.
pub(crate) fn data_device(
    manager: Option<&DataDeviceManagerState>,
    queue_handle: &QueueHandle<WinitState>,
    seat: &sctk::reexports::client::protocol::wl_seat::WlSeat,
) -> Option<DataDevice> {
    manager.map(|m| m.get_data_device(queue_handle, seat))
}

sctk::delegate_data_device!(WinitState);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_lists_become_paths() {
        let list = b"# comment\r\nfile:///home/a/My%20Picture.png\r\nfile://host/tmp/b.jpg\r\nhttp://x/y\r\n";
        assert_eq!(
            parse_uri_list(list),
            vec![
                PathBuf::from("/home/a/My Picture.png"),
                PathBuf::from("/tmp/b.jpg")
            ]
        );
        assert_eq!(percent_decode("100%"), b"100%");
        assert_eq!(percent_decode("%C3%A9"), "é".as_bytes());
    }
}
