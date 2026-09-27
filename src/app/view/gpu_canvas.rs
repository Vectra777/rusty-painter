//! Canvas display on the GPU with real mipmaps.
//!
//! The composited canvas lives in a few `ATLAS_SIZE`² atlas textures, each
//! holding a contiguous 32×32-tile block of the canvas (so mip filtering
//! blends true neighbours). Dirty tiles are written to mip level 0 and only
//! their region of the smaller levels is regenerated on the GPU, so zooming
//! never recomposites the canvas and zoomed-out views are properly filtered.
//!
//! The display shader mirrors egui's own (egui-wgpu `egui.wgsl`) so the canvas
//! blends exactly like an egui image did before: sample in linear light,
//! convert to gamma, dither, premultiplied-alpha blend.

use eframe::egui_wgpu::{self, wgpu};
use wgpu::util::DeviceExt;

use crate::app::document::{ATLAS_SIZE, TILE_SIZE};

/// Mip levels per atlas: level 4 is 1/16 scale, enough for the 0.1 minimum zoom.
pub const MIP_LEVELS: u32 = 5;
/// Border around each atlas's canvas block that mirrors the neighbouring
/// atlases' edge pixels, so filtering across an atlas boundary sees the true
/// neighbours instead of a clamped edge (no seams when zoomed out). It is one
/// texel wide at the coarsest mip level.
pub const ATLAS_BORDER: usize = 1 << (MIP_LEVELS - 1);
/// Atlas texture side: the canvas block plus a border on each side.
pub const ATLAS_TEXTURE_SIZE: usize = ATLAS_SIZE + 2 * ATLAS_BORDER;
/// Canvas tiles per atlas side.
pub const TILES_PER_ATLAS: usize = ATLAS_SIZE / TILE_SIZE;
const ATLAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

const SHADER: &str = r#"
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = vec4<f32>(pos, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

// The helpers below are copied from egui-wgpu's egui.wgsl.
fn interleaved_gradient_noise(n: vec2<f32>) -> f32 {
    let f = 0.06711056 * n.x + 0.00583715 * n.y;
    return fract(52.9829189 * fract(f));
}

fn dither_interleaved(rgb: vec3<f32>, levels: f32, frag_coord: vec4<f32>) -> vec3<f32> {
    var noise = interleaved_gradient_noise(frag_coord.xy);
    noise = (noise - 0.5) * 0.95;
    return rgb + noise / (levels - 1.0);
}

fn linear_from_gamma_rgb(srgb: vec3<f32>) -> vec3<f32> {
    let cutoff = srgb < vec3<f32>(0.04045);
    let lower = srgb / vec3<f32>(12.92);
    let higher = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(higher, lower, cutoff);
}

fn gamma_from_linear_rgb(rgb: vec3<f32>) -> vec3<f32> {
    let cutoff = rgb < vec3<f32>(0.0031308);
    let lower = rgb * vec3<f32>(12.92);
    let higher = vec3<f32>(1.055) * pow(rgb, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(higher, lower, cutoff);
}

fn display_gamma(in: VertexOut) -> vec4<f32> {
    let tex_linear = textureSample(tex, samp, in.uv);
    let rgb = dither_interleaved(gamma_from_linear_rgb(tex_linear.rgb), 256.0, in.position);
    return vec4<f32>(rgb, tex_linear.a);
}

@fragment
fn fs_display_gamma_framebuffer(in: VertexOut) -> @location(0) vec4<f32> {
    return display_gamma(in);
}

@fragment
fn fs_display_linear_framebuffer(in: VertexOut) -> @location(0) vec4<f32> {
    let gamma = display_gamma(in);
    return vec4<f32>(linear_from_gamma_rgb(gamma.rgb), gamma.a);
}

// Each output texel sits on the shared corner of four source texels, so one
// bilinear sample is their exact average (in linear light: the sRGB view
// decodes on read and encodes on write).
@fragment
fn fs_downsample(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(tex, samp, in.uv, 0.0);
}
"#;

/// Pixels to write into one atlas at mip `level` (sRGB-encoded, premultiplied
/// RGBA8). `x`/`y`/`width`/`height` are texels of that level, in texture
/// coordinates (including the border). Smaller levels are regenerated from it.
pub struct TileUpload {
    pub atlas: usize,
    pub level: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// One atlas's on-screen quad: corners (top-left, top-right, bottom-right,
/// bottom-left) in the paint callback's normalized device coordinates, with
/// matching texture coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtlasQuad {
    pub atlas: usize,
    pub corners: [[f32; 2]; 4],
    pub uvs: [[f32; 2]; 4],
}

/// Two triangles for a quad, as interleaved `pos.xy, uv.xy` floats.
fn quad_vertices(corners: &[[f32; 2]; 4], uvs: &[[f32; 2]; 4], out: &mut Vec<f32>) {
    for i in [0, 1, 2, 0, 2, 3] {
        out.extend_from_slice(&[corners[i][0], corners[i][1], uvs[i][0], uvs[i][1]]);
    }
}

fn float_bytes(floats: &[f32]) -> Vec<u8> {
    floats.iter().flat_map(|f| f.to_ne_bytes()).collect()
}

/// Region `[x0, x1) × [y0, y1)` at mip `to` of a rect given at mip `from`,
/// rounded outward so every texel the rect contributes to is included.
fn mip_region(x: u32, y: u32, width: u32, height: u32, from: u32, to: u32) -> [u32; 4] {
    let shift = to - from;
    let scale = 1u32 << shift;
    [
        x >> shift,
        y >> shift,
        (x + width).div_ceil(scale),
        (y + height).div_ceil(scale),
    ]
}

/// Merge uploads that touch edge to edge (same atlas and level) into larger
/// rectangles: first side-by-side ones with equal rows into bands, then
/// stacked bands with equal columns. Per-copy overhead dominates small tile
/// uploads, so a dense block of dirty tiles should become a few big copies.
fn merge_uploads(mut uploads: Vec<TileUpload>) -> Vec<TileUpload> {
    uploads.sort_by_key(|u| (u.atlas, u.level, u.y, u.height, u.x));
    let joins_right = |a: &TileUpload, b: &TileUpload| {
        (a.atlas, a.level, a.y, a.height) == (b.atlas, b.level, b.y, b.height)
            && a.x + a.width == b.x
    };
    let mut bands: Vec<TileUpload> = Vec::with_capacity(uploads.len());
    let mut rest = uploads.into_iter().peekable();
    while let Some(first) = rest.next() {
        let mut run = vec![first];
        while let Some(next) = rest.next_if(|n| joins_right(run.last().unwrap(), n)) {
            run.push(next);
        }
        if run.len() == 1 {
            bands.extend(run);
            continue;
        }
        // Interleave the run's rows once (building incrementally is quadratic).
        let width: u32 = run.iter().map(|u| u.width).sum();
        let height = run[0].height as usize;
        let mut pixels = Vec::with_capacity(width as usize * height * 4);
        for row in 0..height {
            for u in &run {
                let row_bytes = (u.width * 4) as usize;
                pixels.extend_from_slice(&u.pixels[row * row_bytes..(row + 1) * row_bytes]);
            }
        }
        bands.push(TileUpload {
            width,
            pixels,
            ..run.swap_remove(0)
        });
    }
    bands.sort_by_key(|u| (u.atlas, u.level, u.x, u.width, u.y));
    let mut merged: Vec<TileUpload> = Vec::with_capacity(bands.len());
    for band in bands {
        match merged.last_mut() {
            Some(prev)
                if (prev.atlas, prev.level, prev.x, prev.width)
                    == (band.atlas, band.level, band.x, band.width)
                    && prev.y + prev.height == band.y =>
            {
                prev.pixels.extend_from_slice(&band.pixels);
                prev.height += band.height;
            }
            _ => merged.push(band),
        }
    }
    merged
}

/// Upper bound per staging buffer. One buffer for a whole 8K canvas
/// (256 MiB+) exceeds wgpu's default `max_buffer_size`.
const MAX_STAGING_BYTES: u64 = 32 << 20;

fn row_pitch(width: u32) -> u32 {
    (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

/// Bytes an upload takes in a staging buffer (rows padded for copying).
fn staged_bytes(upload: &TileUpload) -> u64 {
    u64::from(row_pitch(upload.width)) * u64::from(upload.height)
}

struct Atlas {
    texture: wgpu::Texture,
    /// All mip levels, for display.
    display_group: wgpu::BindGroup,
    /// One single-level view per mip, as render targets.
    level_views: Vec<wgpu::TextureView>,
    /// One single-level view per mip, as downsample sources.
    level_groups: Vec<wgpu::BindGroup>,
}

/// GPU-side canvas state, stored in egui-wgpu's callback resources.
pub struct GpuCanvas {
    display: wgpu::RenderPipeline,
    downsample: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    display_sampler: wgpu::Sampler,
    downsample_sampler: wgpu::Sampler,
    atlases: Vec<Atlas>,
    generation: Option<u64>,
    /// This frame's display quads, and each draw's atlas and vertex range.
    quad_buffer: Option<wgpu::Buffer>,
    draws: Vec<(usize, std::ops::Range<u32>)>,
}

const VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: 16,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
};

impl GpuCanvas {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canvas shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("canvas texture layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("canvas pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = |label, entry_point, format, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: "vs_main",
                    compilation_options: Default::default(),
                    buffers: &[VERTEX_LAYOUT],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point,
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            })
        };
        // Same blend state as egui's pipeline (premultiplied alpha).
        let egui_blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let display_entry = if target_format.is_srgb() {
            "fs_display_linear_framebuffer"
        } else {
            "fs_display_gamma_framebuffer"
        };
        let display = pipeline(
            "canvas display",
            display_entry,
            target_format,
            Some(egui_blend),
        );
        let downsample = pipeline("canvas downsample", "fs_downsample", ATLAS_FORMAT, None);

        let sampler = |label, mag_filter, mipmap_filter| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(label),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter,
                ..Default::default()
            })
        };
        Self {
            display,
            downsample,
            layout,
            // Crisp pixels when zoomed in (as before), trilinear when zoomed out.
            display_sampler: sampler(
                "canvas display",
                wgpu::FilterMode::Nearest,
                wgpu::FilterMode::Linear,
            ),
            downsample_sampler: sampler(
                "canvas downsample",
                wgpu::FilterMode::Linear,
                wgpu::FilterMode::Nearest,
            ),
            atlases: Vec::new(),
            generation: None,
            quad_buffer: None,
            draws: Vec::new(),
        }
    }

    fn bind_group(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("canvas atlas"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    /// (Re)create the atlases when the canvas was rebuilt (new generation).
    /// New textures start fully transparent.
    fn ensure_atlases(&mut self, device: &wgpu::Device, generation: u64, count: usize) {
        if self.generation == Some(generation) && self.atlases.len() == count {
            return;
        }
        self.generation = Some(generation);
        self.atlases = (0..count)
            .map(|_| {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("canvas atlas"),
                    size: wgpu::Extent3d {
                        width: ATLAS_TEXTURE_SIZE as u32,
                        height: ATLAS_TEXTURE_SIZE as u32,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: MIP_LEVELS,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: ATLAS_FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let all_levels = texture.create_view(&wgpu::TextureViewDescriptor::default());
                let level_views: Vec<_> = (0..MIP_LEVELS)
                    .map(|level| {
                        texture.create_view(&wgpu::TextureViewDescriptor {
                            base_mip_level: level,
                            mip_level_count: Some(1),
                            ..Default::default()
                        })
                    })
                    .collect();
                let level_groups = level_views
                    .iter()
                    .map(|view| self.bind_group(device, view, &self.downsample_sampler))
                    .collect();
                Atlas {
                    display_group: self.bind_group(device, &all_levels, &self.display_sampler),
                    texture,
                    level_views,
                    level_groups,
                }
            })
            .collect();
    }

    /// Copy every upload into its mip level through mapped staging buffers
    /// and per-upload copy commands.
    fn copy_uploads(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        uploads: &[TileUpload],
    ) {
        self.copy_uploads_chunked(device, encoder, uploads, MAX_STAGING_BYTES);
    }

    /// [`Self::copy_uploads`] with staging buffers of at most `max_bytes`
    /// each (a single upload never exceeds one atlas, ~17 MiB).
    fn copy_uploads_chunked(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        uploads: &[TileUpload],
        max_bytes: u64,
    ) {
        let mut start = 0;
        let mut size = 0;
        for (i, upload) in uploads.iter().enumerate() {
            let bytes = staged_bytes(upload);
            if i > start && size + bytes > max_bytes {
                self.copy_chunk(device, encoder, &uploads[start..i]);
                start = i;
                size = 0;
            }
            size += bytes;
        }
        self.copy_chunk(device, encoder, &uploads[start..]);
    }

    fn copy_chunk(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        uploads: &[TileUpload],
    ) {
        let size: u64 = uploads.iter().map(staged_bytes).sum();
        if size == 0 {
            return;
        }
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("canvas tile staging"),
            size,
            usage: wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        let mut offsets = Vec::with_capacity(uploads.len());
        {
            let mut mapped = staging.slice(..).get_mapped_range_mut();
            let mut offset = 0usize;
            for upload in uploads {
                let pitch = row_pitch(upload.width) as usize;
                let row_bytes = (upload.width * 4) as usize;
                for (row, src) in upload.pixels.chunks_exact(row_bytes).enumerate() {
                    let dst = offset + row * pitch;
                    mapped[dst..dst + row_bytes].copy_from_slice(src);
                }
                offsets.push(offset as u64);
                offset += pitch * upload.height as usize;
            }
        }
        staging.unmap();

        for (upload, offset) in uploads.iter().zip(offsets) {
            let Some(atlas) = self.atlases.get(upload.atlas) else {
                continue;
            };
            encoder.copy_buffer_to_texture(
                wgpu::ImageCopyBuffer {
                    buffer: &staging,
                    layout: wgpu::ImageDataLayout {
                        offset,
                        bytes_per_row: Some(row_pitch(upload.width)),
                        rows_per_image: Some(upload.height),
                    },
                },
                wgpu::ImageCopyTexture {
                    texture: &atlas.texture,
                    mip_level: upload.level,
                    origin: wgpu::Origin3d {
                        x: upload.x,
                        y: upload.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: upload.width,
                    height: upload.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    /// Write the uploads, then regenerate just their regions of every smaller
    /// level, one render pass per level per touched atlas.
    fn upload(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        uploads: &[TileUpload],
    ) {
        self.copy_uploads(device, encoder, uploads);

        for (atlas_idx, atlas) in self.atlases.iter().enumerate() {
            let mine: Vec<&TileUpload> = uploads.iter().filter(|u| u.atlas == atlas_idx).collect();
            if mine.is_empty() {
                continue;
            }
            for level in 1..MIP_LEVELS {
                let size = (ATLAS_TEXTURE_SIZE >> level) as f32;
                let mut vertices = Vec::with_capacity(mine.len() * 24);
                for upload in mine.iter().filter(|u| u.level < level) {
                    let [x0, y0, x1, y1] = mip_region(
                        upload.x,
                        upload.y,
                        upload.width,
                        upload.height,
                        upload.level,
                        level,
                    );
                    let (u0, v0, u1, v1) = (
                        x0 as f32 / size,
                        y0 as f32 / size,
                        x1 as f32 / size,
                        y1 as f32 / size,
                    );
                    let ndc = |u: f32, v: f32| [u * 2.0 - 1.0, 1.0 - v * 2.0];
                    quad_vertices(
                        &[ndc(u0, v0), ndc(u1, v0), ndc(u1, v1), ndc(u0, v1)],
                        &[[u0, v0], [u1, v0], [u1, v1], [u0, v1]],
                        &mut vertices,
                    );
                }
                if vertices.is_empty() {
                    continue;
                }
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("canvas mip regions"),
                    contents: &float_bytes(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("canvas mip"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &atlas.level_views[level as usize],
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&self.downsample);
                pass.set_bind_group(0, &atlas.level_groups[level as usize - 1], &[]);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..(vertices.len() / 4) as u32, 0..1);
            }
        }
    }

    fn prepare_quads(&mut self, device: &wgpu::Device, quads: &[AtlasQuad]) {
        let mut vertices = Vec::with_capacity(quads.len() * 24);
        self.draws.clear();
        for quad in quads.iter().filter(|q| q.atlas < self.atlases.len()) {
            let start = (vertices.len() / 4) as u32;
            quad_vertices(&quad.corners, &quad.uvs, &mut vertices);
            self.draws.push((quad.atlas, start..start + 6));
        }
        self.quad_buffer = (!vertices.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("canvas quads"),
                contents: &float_bytes(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
    }

    fn paint(&self, render_pass: &mut wgpu::RenderPass<'static>) {
        let Some(buffer) = &self.quad_buffer else {
            return;
        };
        render_pass.set_pipeline(&self.display);
        render_pass.set_vertex_buffer(0, buffer.slice(..));
        for (atlas, range) in &self.draws {
            render_pass.set_bind_group(0, &self.atlases[*atlas].display_group, &[]);
            render_pass.draw(range.clone(), 0..1);
        }
    }
}

/// One frame's canvas drawing: tile uploads plus the atlas quads to show.
pub struct CanvasPaint {
    pub generation: u64,
    pub atlas_count: usize,
    /// Taken (not copied) by `prepare`, which runs once per frame.
    pub uploads: std::sync::Mutex<Vec<TileUpload>>,
    pub quads: Vec<AtlasQuad>,
}

impl egui_wgpu::CallbackTrait for CanvasPaint {
    fn prepare(
        &self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(gpu) = resources.get_mut::<GpuCanvas>() {
            gpu.ensure_atlases(device, self.generation, self.atlas_count);
            let uploads = self
                .uploads
                .lock()
                .map(|mut u| std::mem::take(&mut *u))
                .unwrap_or_default();
            gpu.upload(device, egui_encoder, &merge_uploads(uploads));
            gpu.prepare_quads(device, &self.quads);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: eframe::egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(gpu) = resources.get::<GpuCanvas>() {
            gpu.paint(render_pass);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
        eprintln!("GPU test adapter: {:?}", adapter.get_info());
        let descriptor = wgpu::DeviceDescriptor {
            required_limits: adapter.limits(),
            ..Default::default()
        };
        pollster::block_on(adapter.request_device(&descriptor, None)).ok()
    }

    fn read_texture(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        level: u32,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        let padded = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        data.chunks(padded as usize)
            .flat_map(|row| row[..(width * 4) as usize].to_vec())
            .collect()
    }

    fn linear(v: u8) -> f32 {
        let s = v as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }

    fn gamma(l: f32) -> u8 {
        let s = if l <= 0.0031308 {
            l * 12.92
        } else {
            1.055 * l.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0).round() as u8
    }

    /// 2x2 box average in linear light (RGB) and linearly (alpha).
    fn average(px: [[u8; 4]; 4]) -> [u8; 4] {
        let mut out = [0u8; 4];
        for c in 0..3 {
            out[c] = gamma(px.iter().map(|p| linear(p[c])).sum::<f32>() / 4.0);
        }
        out[3] = (px.iter().map(|p| p[3] as f32).sum::<f32>() / 4.0).round() as u8;
        out
    }

    fn assert_close(actual: &[u8], expected: &[u8], what: &str) {
        for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (*a as i16 - *e as i16).abs() <= 1,
                "{what}: byte {i}: {a} vs {e}"
            );
        }
    }

    #[test]
    fn mips_are_linear_box_filtered_and_display_matches_the_tile() {
        let Some((device, queue)) = gpu() else {
            eprintln!("no GPU adapter available; skipping");
            return;
        };
        let mut canvas = GpuCanvas::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        canvas.ensure_atlases(&device, 0, 1);

        let size = TILE_SIZE as u32;
        let pixels: Vec<u8> = (0..size * size)
            .flat_map(|i| {
                let (x, y) = (i % size, i / size);
                [
                    (x * 4) as u8,
                    (y * 4) as u8,
                    ((x + y) * 3) as u8,
                    255 - ((x ^ y) % 7) as u8 * 20,
                ]
            })
            .collect();
        let mut encoder = device.create_command_encoder(&Default::default());
        canvas.upload(
            &device,
            &mut encoder,
            &[TileUpload {
                atlas: 0,
                level: 0,
                x: 0,
                y: 0,
                width: size,
                height: size,
                pixels: pixels.clone(),
            }],
        );
        queue.submit([encoder.finish()]);

        let texture = &canvas.atlases[0].texture;
        let mut previous = pixels.clone();
        let mut previous_size = size;
        for level in 1..MIP_LEVELS {
            let level_size = size >> level;
            let got = read_texture(&device, &queue, texture, level, level_size, level_size);
            let at = |x: u32, y: u32| {
                let i = ((y * previous_size + x) * 4) as usize;
                [
                    previous[i],
                    previous[i + 1],
                    previous[i + 2],
                    previous[i + 3],
                ]
            };
            let expected: Vec<u8> = (0..level_size * level_size)
                .flat_map(|i| {
                    let (x, y) = (i % level_size * 2, i / level_size * 2);
                    average([at(x, y), at(x + 1, y), at(x, y + 1), at(x + 1, y + 1)])
                })
                .collect();
            assert_close(&got, &expected, &format!("mip level {level}"));
            previous = got;
            previous_size = level_size;
        }

        // Draw the tile 1:1 into a 64x64 gamma framebuffer: it must reproduce
        // the uploaded bytes (up to egui's ±0.5/255 dither).
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let uv = size as f32 / ATLAS_TEXTURE_SIZE as f32;
        canvas.prepare_quads(
            &device,
            &[AtlasQuad {
                atlas: 0,
                corners: [[-1.0, 1.0], [1.0, 1.0], [1.0, -1.0], [-1.0, -1.0]],
                uvs: [[0.0, 0.0], [uv, 0.0], [uv, uv], [0.0, uv]],
            }],
        );
        let view = target.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                })
                .forget_lifetime();
            canvas.paint(&mut pass);
        }
        queue.submit([encoder.finish()]);
        let shown = read_texture(&device, &queue, &target, 0, size, size);
        assert_close(&shown, &pixels, "1:1 display");
    }

    #[test]
    fn mip_regions_round_outward() {
        assert_eq!(mip_region(64, 128, 64, 64, 0, 1), [32, 64, 64, 96]);
        assert_eq!(mip_region(64, 0, 40, 64, 0, 4), [4, 0, 7, 4]);
        assert_eq!(mip_region(0, 0, 1, 1, 0, 4), [0, 0, 1, 1]);
        assert_eq!(mip_region(8, 4, 16, 16, 2, 3), [4, 2, 12, 10]);
    }

    #[test]
    fn preview_level_uploads_land_exactly_and_regenerate_coarser_levels() {
        let Some((device, queue)) = gpu() else {
            eprintln!("no GPU adapter available; skipping");
            return;
        };
        let mut canvas = GpuCanvas::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        canvas.ensure_atlases(&device, 0, 1);
        // A 16x16 block at level 2 (a 64 px tile at 1/4 scale), placed at an
        // odd-looking but aligned spot.
        let pixels: Vec<u8> = (0..16 * 16)
            .flat_map(|i| [i as u8, 255 - i as u8, 90, 255])
            .collect();
        let mut encoder = device.create_command_encoder(&Default::default());
        canvas.upload(
            &device,
            &mut encoder,
            &[TileUpload {
                atlas: 0,
                level: 2,
                x: 0,
                y: 0,
                width: 16,
                height: 16,
                pixels: pixels.clone(),
            }],
        );
        queue.submit([encoder.finish()]);
        let texture = &canvas.atlases[0].texture;
        assert_eq!(read_texture(&device, &queue, texture, 2, 16, 16), pixels);
        let level3 = read_texture(&device, &queue, texture, 3, 8, 8);
        let at = |x: usize, y: usize| {
            let i = (y * 16 + x) * 4;
            [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
        };
        let expected: Vec<u8> = (0..64)
            .flat_map(|i| {
                let (x, y) = (i % 8 * 2, i / 8 * 2);
                average([at(x, y), at(x + 1, y), at(x, y + 1), at(x + 1, y + 1)])
            })
            .collect();
        assert_close(&level3, &expected, "level 3 from a level-2 upload");
    }

    #[test]
    fn merge_uploads_joins_touching_rects_losslessly() {
        // A 3x2 grid of 2x2 uploads with distinct pixels, plus one in another atlas.
        let upload = |atlas, x: u32, y: u32| TileUpload {
            atlas,
            level: 0,
            x,
            y,
            width: 2,
            height: 2,
            pixels: (0..4)
                .flat_map(|i| [x as u8, y as u8, i as u8, atlas as u8])
                .collect(),
        };
        let mut uploads = vec![upload(1, 0, 0)];
        for y in [0, 2] {
            for x in [4, 0, 2] {
                uploads.push(upload(0, x, y));
            }
        }
        let merged = merge_uploads(uploads);
        assert_eq!(merged.len(), 2);
        let big = &merged[0];
        assert_eq!(
            (big.atlas, big.x, big.y, big.width, big.height),
            (0, 0, 0, 6, 4)
        );
        for py in 0..4u32 {
            for px in 0..6u32 {
                let i = ((py * 6 + px) * 4) as usize;
                let (tile_x, tile_y, within) = (px / 2 * 2, py / 2 * 2, (py % 2) * 2 + px % 2);
                assert_eq!(
                    big.pixels[i..i + 4],
                    [tile_x as u8, tile_y as u8, within as u8, 0]
                );
            }
        }
        assert_eq!(
            (merged[1].atlas, merged[1].width, merged[1].height),
            (1, 2, 2)
        );
    }

    #[test]
    fn uploads_split_across_small_staging_buffers_land_intact() {
        let Some((device, queue)) = gpu() else {
            eprintln!("no GPU adapter available; skipping");
            return;
        };
        let mut canvas = GpuCanvas::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        canvas.ensure_atlases(&device, 0, 1);
        let tile = |x: u32, shade: u8| TileUpload {
            atlas: 0,
            level: 0,
            x,
            y: 0,
            width: 64,
            height: 64,
            pixels: (0..64 * 64)
                .flat_map(|i| [shade, i as u8, (i / 64) as u8, 255])
                .collect(),
        };
        let uploads = vec![tile(0, 10), tile(64, 20), tile(128, 30)];
        let mut encoder = device.create_command_encoder(&Default::default());
        // One 64x64 tile (16 KiB) per staging buffer.
        canvas.copy_uploads_chunked(&device, &mut encoder, &uploads, 20 << 10);
        queue.submit([encoder.finish()]);
        let got = read_texture(&device, &queue, &canvas.atlases[0].texture, 0, 192, 64);
        for (n, upload) in uploads.iter().enumerate() {
            for row in 0..64usize {
                let got_row = &got[(row * 192 + n * 64) * 4..(row * 192 + n * 64 + 64) * 4];
                assert_eq!(
                    got_row,
                    &upload.pixels[row * 256..(row + 1) * 256],
                    "tile {n} row {row}"
                );
            }
        }
    }
}
