//! Showing the canvas: GPU atlases of composited tiles ([`gpu_canvas`]),
//! which tiles to re-composite and upload each frame ([`render`]), and the
//! canvas <-> screen mapping (zoom, pan, rotation, flip; [`viewport`]).
//! Shader layers showing live are composited on the GPU ([`shader_gpu`]).
//! Viewing aids over it: the grid, guide lines and snapping ([`aids`]).
pub(crate) mod aids;
pub(crate) mod brush_cursor;
pub(crate) mod gpu_canvas;
pub(crate) mod grid;
pub(crate) mod guide_lines;
pub(crate) mod render;
pub(crate) mod shader_gpu;
pub(crate) mod viewport;

/// The GPU tests' device, made once (making one takes about a second) and
/// lent to one test at a time: error scopes belong to the device, so two
/// tests on it at once would catch each other's errors.
#[cfg(test)]
pub(crate) fn test_gpu() -> Option<std::sync::MutexGuard<'static, (wgpu::Device, wgpu::Queue)>> {
    use std::sync::{Mutex, OnceLock, PoisonError};
    static GPU: OnceLock<Option<Mutex<(wgpu::Device, wgpu::Queue)>>> = OnceLock::new();
    let gpu = GPU.get_or_init(|| {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
        eprintln!("GPU test adapter: {:?}", adapter.get_info());
        let descriptor = wgpu::DeviceDescriptor {
            required_limits: adapter.limits(),
            ..Default::default()
        };
        let device = pollster::block_on(adapter.request_device(&descriptor, None)).ok()?;
        Some(Mutex::new(device))
    });
    // A failed test poisons the lock; the device is still good.
    Some(gpu.as_ref()?.lock().unwrap_or_else(PoisonError::into_inner))
}
