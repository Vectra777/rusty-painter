//! 3D figure layers on the GPU: their scene rendered with depth, live into
//! the shader layers' composite (see [`super::shader_gpu`]), and baked into
//! canvas pixels for everything else.
//!
//! Skinning is linear blend skinning in the vertex shader: each vertex is
//! moved by up to four joints' skinning matrices (one uniform array per
//! figure), then projected by the scene's camera onto the canvas. The
//! canvas's normalised device coordinates are then mapped onto the render
//! target's by a 2D affine map ([`ndc_map`]), so the same projection draws
//! the canvas on screen (zoomed, turned, mirrored) and a chunk of it when
//! baking. Figures are drawn with their own depth buffer (the canvas has
//! none), four times multisampled.
//!
//! Line art is an inverted hull: each figure is drawn a second time first,
//! pushed out along its normals by the outline's width on the canvas, only
//! its sides facing away from the camera, in black; the figure itself then
//! covers all of it but a rim round its silhouette (and round parts in
//! front of others). Which sides face away is read from the normals, not
//! the triangles' winding, so a mirrored view needs nothing special.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use eframe::egui::Color32;
use eframe::egui_wgpu::wgpu;
use wgpu::util::DeviceExt;

use crate::canvas::figure::RenderFrame;
use crate::canvas::figure::model::MAX_JOINTS;

/// Samples per pixel.
const SAMPLES: u32 = 4;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Bake target: sRGB-encoded premultiplied bytes, as canvas tiles store.
const BAKE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Largest square rendered at once when baking.
const BAKE_CHUNK: u32 = 2048;

const SHADER: &str = r#"
struct Params {
    view_proj: mat4x4<f32>,
    // Canvas NDC (and w) to the target's: x' = dot(ndc_x.xyz, (x, y, w)).
    ndc_x: vec4<f32>,
    ndc_y: vec4<f32>,
    // Towards the light (scene), and the ambient light.
    light: vec4<f32>,
    // x: style (0 shaded, 1 toon, 2 silhouette, 3 line art); y: write
    // sRGB values (else linear light, for an sRGB target); z: the outline
    // pass; w: line art filled white (else see-through).
    flags: vec4<f32>,
    // The camera (scene), and the outline's width (canvas pixels).
    eye: vec4<f32>,
    // The canvas's size (pixels).
    canvas: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<uniform> joints: array<mat4x4<f32>, MAX_JOINTS>;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) joint: vec4<u32>,
    @location(4) weight: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) world: vec3<f32>,
};

@vertex
fn vs_main(v: VertexIn) -> VertexOut {
    let skin = joints[v.joint.x] * v.weight.x
        + joints[v.joint.y] * v.weight.y
        + joints[v.joint.z] * v.weight.z
        + joints[v.joint.w] * v.weight.w;
    let world = skin * vec4<f32>(v.position, 1.0);
    let normal = (skin * vec4<f32>(v.normal, 0.0)).xyz;
    var clip = params.view_proj * world;
    if (params.flags.z > 0.5) {
        // Out along the normal as it looks on the canvas, the outline's
        // width in canvas pixels whatever the distance.
        let size = params.canvas.xy;
        let across = (params.view_proj * vec4<f32>(normal, 0.0)).xy * size;
        let len = length(across);
        if (len > 1e-9) {
            let offset = across / len / size * 2.0 * params.eye.w;
            clip = vec4<f32>(clip.xy + offset * clip.w, clip.z, clip.w);
        }
    }
    let xyw = vec3<f32>(clip.x, clip.y, clip.w);
    var out: VertexOut;
    out.clip = vec4<f32>(dot(params.ndc_x.xyz, xyw), dot(params.ndc_y.xyz, xyw), clip.z, clip.w);
    out.normal = normal;
    out.color = v.color;
    out.world = world.xyz;
    return out;
}

fn linear_from_srgb(c: vec3<f32>) -> vec3<f32> {
    let cutoff = c < vec3<f32>(0.04045);
    return select(pow((c + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4)), c / vec3<f32>(12.92), cutoff);
}

fn srgb_from_linear(c: vec3<f32>) -> vec3<f32> {
    let cutoff = c < vec3<f32>(0.0031308);
    return select(vec3<f32>(1.055) * pow(c, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055), c * vec3<f32>(12.92), cutoff);
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let style = u32(params.flags.x + 0.5);
    if (params.flags.z > 0.5) {
        // The outline: only the far side of the pushed-out figure.
        if (dot(n, params.eye.xyz - in.world) > 0.0) {
            discard;
        }
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    if (style == 3u) {
        // Line art's fill: white, or nothing (still hiding the lines
        // behind it).
        return select(vec4<f32>(0.0), vec4<f32>(1.0), params.flags.w > 0.5);
    }
    let lambert = max(dot(n, normalize(params.light.xyz)), 0.0);
    let ambient = params.light.w;
    // A little light from the sky, so shadowed sides keep their shape.
    let sky = 0.08 * (n.y * 0.5 + 0.5);
    var rgb = linear_from_srgb(in.color.rgb);
    if (style == 2u) {
        rgb = vec3<f32>(0.03, 0.03, 0.04);
    } else if (style == 1u) {
        rgb = rgb * select(ambient + sky, 1.0, lambert > 0.3);
    } else {
        rgb = rgb * (ambient + sky + (1.0 - ambient) * lambert);
    }
    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    if (params.flags.y > 0.5) {
        rgb = srgb_from_linear(rgb);
    }
    return vec4<f32>(rgb * in.color.a, in.color.a);
}
"#;

/// A vertex as the GPU takes it.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    color: [u8; 4],
    joints: [u16; 4],
    weights: [f32; 4],
}

const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![
    0 => Float32x3,
    1 => Float32x3,
    2 => Unorm8x4,
    3 => Uint16x4,
    4 => Float32x4,
];

/// A model's mesh on the GPU (with some morph weights applied).
struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}

/// The multisampled colour and depth targets, at one size and format.
struct Targets {
    size: [u32; 2],
    format: wgpu::TextureFormat,
    color: wgpu::TextureView,
    depth: wgpu::TextureView,
}

pub struct FigureGpu {
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    module: wgpu::ShaderModule,
    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    /// By model and morph weights, with when each was last used.
    meshes: HashMap<(u64, u64), (GpuMesh, u64)>,
    /// Counts [`Self::prepare`] calls (when meshes were last used).
    tick: u64,
    targets: Option<Targets>,
}

/// Meshes kept per model (with different morph weights: a slider being
/// dragged makes a new one each frame).
const MESHES_PER_MODEL: usize = 3;

/// Rows of the affine map from the canvas's normalised device coordinates
/// to a render target's, given the map from canvas pixels (y down) to the
/// target's texture coordinates (`u = dot(to_u, (x, y, 1))`, likewise `v`)
/// for a `width`×`height` canvas.
pub fn ndc_map(to_u: [f32; 3], to_v: [f32; 3], width: f32, height: f32) -> [[f32; 3]; 2] {
    let [a, b, c] = to_u;
    let [d, e, f] = to_v;
    [
        [
            a * width,
            -b * height,
            a * width + b * height + 2.0 * c - 1.0,
        ],
        [
            -d * width,
            e * height,
            1.0 - d * width - e * height - 2.0 * f,
        ],
    ]
}

/// The key a mesh is cached under: its model, and the morph weights it
/// was uploaded with (0 when none apply).
fn mesh_key(uid: u64, morphs: &[f32]) -> (u64, u64) {
    if morphs.iter().all(|&w| w == 0.0) {
        return (uid, 0);
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for w in morphs {
        w.to_bits().hash(&mut h);
    }
    (uid, h.finish() | 1)
}

impl FigureGpu {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("figure layers"),
            source: wgpu::ShaderSource::Wgsl(
                format!("const MAX_JOINTS: u32 = {MAX_JOINTS}u;\n{SHADER}").into(),
            ),
        });
        let uniform = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("figure layer"),
            entries: &[uniform(0), uniform(1)],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("figure layer"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        Self {
            layout,
            pipeline_layout,
            module,
            pipelines: HashMap::new(),
            meshes: HashMap::new(),
            tick: 0,
            targets: None,
        }
    }

    fn ensure_pipeline(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        if self.pipelines.contains_key(&format) {
            return;
        }
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("figure layer"),
            layout: Some(&self.pipeline_layout),
            vertex: wgpu::VertexState {
                module: &self.module,
                entry_point: "vs_main",
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &ATTRIBUTES,
                }],
            },
            // Both sides: a mirrored view turns the triangles round.
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: SAMPLES,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.module,
                entry_point: "fs_main",
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        self.pipelines.insert(format, pipeline);
    }

    fn ensure_targets(
        &mut self,
        device: &wgpu::Device,
        size: [u32; 2],
        format: wgpu::TextureFormat,
    ) {
        if self
            .targets
            .as_ref()
            .is_some_and(|t| t.size == size && t.format == format)
        {
            return;
        }
        let texture = |label, format, usage| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: SAMPLES,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        self.targets = Some(Targets {
            size,
            format,
            color: texture(
                "figure layer colour",
                format,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            ),
            depth: texture(
                "figure layer depth",
                DEPTH_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            ),
        });
    }

    /// Upload what `frame` draws with that isn't on the GPU yet, and make
    /// the targets for a `size` picture in `format`. Call before
    /// [`Self::draw`].
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        frame: &RenderFrame,
        size: [u32; 2],
        format: wgpu::TextureFormat,
    ) {
        self.ensure_pipeline(device, format);
        self.ensure_targets(device, size, format);
        self.tick += 1;
        for instance in &frame.instances {
            let model = &instance.model;
            let key = mesh_key(model.uid, &instance.morphs);
            if let Some((_, used)) = self.meshes.get_mut(&key) {
                *used = self.tick;
                continue;
            }
            // Room for it: the model's least recently used mesh goes.
            let mut same: Vec<((u64, u64), u64)> = (self.meshes.iter())
                .filter(|(k, _)| k.0 == model.uid)
                .map(|(k, (_, used))| (*k, *used))
                .collect();
            if same.len() >= MESHES_PER_MODEL {
                same.sort_by_key(|(_, used)| *used);
                for (old, _) in &same[..=same.len() - MESHES_PER_MODEL] {
                    self.meshes.remove(old);
                }
            }
            let mesh = &model.mesh;
            let positions = if key.1 == 0 {
                std::borrow::Cow::Borrowed(&mesh.positions)
            } else {
                std::borrow::Cow::Owned(model.morphed_positions(&instance.morphs))
            };
            let vertices: Vec<Vertex> = (0..mesh.positions.len())
                .map(|i| Vertex {
                    position: positions[i],
                    normal: mesh.normals[i],
                    color: mesh.colors[i],
                    joints: mesh.joints[i].map(|j| j.min(MAX_JOINTS as u16 - 1)),
                    weights: mesh.weights[i],
                })
                .collect();
            let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("figure vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("figure indices"),
                contents: bytemuck::cast_slice(&mesh.indices),
                usage: wgpu::BufferUsages::INDEX,
            });
            self.meshes.insert(
                key,
                (
                    GpuMesh {
                        vertices,
                        indices,
                        count: mesh.indices.len() as u32,
                    },
                    self.tick,
                ),
            );
        }
    }

    /// Forget the meshes of models no longer shown (by uid).
    pub fn retain_models(&mut self, uids: &[u64]) {
        self.meshes.retain(|(uid, _), _| uids.contains(uid));
    }

    /// Draw `frame` into `output` (cleared first; the size and format
    /// [`Self::prepare`] was given), mapped there by `map` (see
    /// [`ndc_map`]); `gamma` writes sRGB values (a plain target holding the
    /// gamma blend space) instead of linear light.
    pub fn draw(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        frame: &RenderFrame,
        map: [[f32; 3]; 2],
        output: &wgpu::TextureView,
        gamma: bool,
    ) {
        let Some(targets) = &self.targets else {
            return;
        };
        let Some(pipeline) = self.pipelines.get(&targets.format) else {
            return;
        };
        let line_art = frame.style == crate::canvas::figure::RenderStyle::LineArt;
        let params = |outline_pass: bool| {
            let mut params: Vec<f32> = frame.view_proj.to_cols_array().to_vec();
            params.extend([map[0][0], map[0][1], map[0][2], 0.0]);
            params.extend([map[1][0], map[1][1], map[1][2], 0.0]);
            params.extend(frame.light.to_array());
            params.push(frame.ambient);
            params.extend([
                frame.style.code() as f32,
                if gamma { 1.0 } else { 0.0 },
                if outline_pass { 1.0 } else { 0.0 },
                if frame.line_fill { 1.0 } else { 0.0 },
            ]);
            params.extend(frame.eye.to_array());
            params.push(frame.outline);
            params.extend([frame.canvas[0] as f32, frame.canvas[1] as f32, 0.0, 0.0]);
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("figure params"),
                contents: bytemuck::cast_slice(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            })
        };
        let passes: Vec<wgpu::Buffer> = if line_art && frame.outline > 0.0 {
            vec![params(true), params(false)]
        } else {
            vec![params(false)]
        };
        // For each pass, each figure's bindings and mesh.
        let mut groups: Vec<(wgpu::BindGroup, &GpuMesh)> = Vec::new();
        let instances: Vec<(wgpu::Buffer, &GpuMesh)> = frame
            .instances
            .iter()
            .filter_map(|instance| {
                let (mesh, _) = self
                    .meshes
                    .get(&mesh_key(instance.model.uid, &instance.morphs))?;
                let mut matrices = vec![0f32; MAX_JOINTS * 16];
                for (i, m) in instance.skin.iter().take(MAX_JOINTS).enumerate() {
                    matrices[i * 16..(i + 1) * 16].copy_from_slice(&m.to_cols_array());
                }
                let joints = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("figure joints"),
                    contents: bytemuck::cast_slice(&matrices),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
                Some((joints, mesh))
            })
            .collect();
        for params in &passes {
            for (joints, mesh) in &instances {
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("figure"),
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: params.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: joints.as_entire_binding(),
                        },
                    ],
                });
                groups.push((group, mesh));
            }
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("figure layer"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &targets.color,
                resolve_target: Some(output),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Discard,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &targets.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(pipeline);
        for (group, mesh) in &groups {
            pass.set_bind_group(0, group, &[]);
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.count, 0, 0..1);
        }
    }

    /// Render `frame` for its whole canvas. Returns the pixels, row by row,
    /// in the canvas tiles' format.
    pub fn bake(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &RenderFrame,
    ) -> Result<Vec<Color32>, String> {
        let [width, height] = frame.canvas;
        if width == 0 || height == 0 {
            return Err("nothing to bake".into());
        }
        let max = device.limits().max_texture_dimension_2d;
        let chunk = BAKE_CHUNK.min(max) as usize;
        let mut out = vec![Color32::TRANSPARENT; width * height];
        for oy in (0..height).step_by(chunk) {
            for ox in (0..width).step_by(chunk) {
                let (w, h) = ((width - ox).min(chunk), (height - oy).min(chunk));
                let map = ndc_map(
                    [1.0 / w as f32, 0.0, -(ox as f32) / w as f32],
                    [0.0, 1.0 / h as f32, -(oy as f32) / h as f32],
                    width as f32,
                    height as f32,
                );
                let pixels = self.bake_chunk(device, queue, frame, map, w, h)?;
                for row in 0..h {
                    let dst = (oy + row) * width + ox;
                    out[dst..dst + w].copy_from_slice(&pixels[row * w..(row + 1) * w]);
                }
            }
        }
        Ok(out)
    }

    fn bake_chunk(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &RenderFrame,
        map: [[f32; 3]; 2],
        width: usize,
        height: usize,
    ) -> Result<Vec<Color32>, String> {
        let (w, h) = (width as u32, height as u32);
        self.prepare(device, frame, [w, h], BAKE_FORMAT);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("figure layer bake"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: BAKE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let padded = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("figure layer bake readback"),
            size: u64::from(padded) * u64::from(h),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        self.draw(device, &mut encoder, frame, map, &view, false);
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .map_err(|_| "the GPU didn't return the render".to_string())?
            .map_err(|e| e.to_string())?;
        let data = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity(width * height);
        for row in data.chunks(padded as usize) {
            pixels.extend(
                row[..width * 4]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3])),
            );
        }
        drop(data);
        buffer.unmap();
        Ok(pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::figure::model::{FigureModel, Mesh};
    use crate::canvas::figure::skeleton::{Joint, JointLimit, RestTransform, Skeleton};
    use crate::canvas::figure::{FigureLayer, RenderInstance, RenderStyle};
    use glam::{Mat4, Vec3};
    use std::sync::Arc;

    fn gpu() -> Option<std::sync::MutexGuard<'static, (wgpu::Device, wgpu::Queue)>> {
        crate::app::view::test_gpu()
    }

    #[test]
    fn the_ndc_map_of_the_whole_canvas_is_the_identity() {
        let map = ndc_map(
            [1.0 / 200.0, 0.0, 0.0],
            [0.0, 1.0 / 100.0, 0.0],
            200.0,
            100.0,
        );
        assert_eq!(map, [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
        // The right half of the canvas as a target: the canvas's middle is
        // the target's left edge.
        let map = ndc_map(
            [1.0 / 100.0, 0.0, -1.0],
            [0.0, 1.0 / 100.0, 0.0],
            200.0,
            100.0,
        );
        let x = map[0][0] * 0.0 + map[0][1] * 0.0 + map[0][2];
        assert!((x + 1.0).abs() < 1e-6, "{x}");
    }

    /// A one-joint model: an axis-aligned box `size` wide round `center`,
    /// in `color`.
    fn cube(center: Vec3, size: f32, color: [u8; 4]) -> Arc<FigureModel> {
        let mut mesh = Mesh::default();
        let h = size / 2.0;
        for (axis, sign) in [
            (0, 1.0),
            (0, -1.0),
            (1, 1.0),
            (1, -1.0),
            (2, 1.0),
            (2, -1.0),
        ] {
            let mut n = Vec3::ZERO;
            n[axis] = sign;
            let (u, v) = n.any_orthonormal_pair();
            let base = mesh.positions.len() as u32;
            for (a, b) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let p = center + (n + u * a + v * b) * h;
                mesh.positions.push(p.into());
                mesh.normals.push(n.into());
                mesh.colors.push(color);
                mesh.joints.push([0; 4]);
                mesh.weights.push([1.0, 0.0, 0.0, 0.0]);
            }
            mesh.indices
                .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        let skeleton = Skeleton {
            joints: vec![Joint {
                name: "root".into(),
                parent: None,
                rest: RestTransform::default(),
                inverse_bind: Mat4::IDENTITY,
                human: None,
                limit: JointLimit::FREE,
            }],
        };
        Arc::new(FigureModel::new("cube", skeleton, mesh, Vec::new()))
    }

    fn frame_of(models: &[Arc<FigureModel>], width: usize, height: usize) -> RenderFrame {
        let mut layer = FigureLayer::mannequin();
        layer.camera.target = [0.0, 0.0, 0.0];
        layer.camera.pitch = 0.0;
        layer.camera.yaw = 0.0;
        layer.camera.distance = 4.0;
        layer.light.ambient = 1.0;
        layer.style = RenderStyle::Shaded;
        let mut frame = layer.render_frame(width, height);
        frame.instances = models
            .iter()
            .map(|m| RenderInstance {
                model: m.clone(),
                skin: vec![Mat4::IDENTITY],
                morphs: Vec::new(),
            })
            .collect();
        frame
    }

    #[test]
    fn the_mannequin_bakes_into_pixels_in_the_middle() {
        let Some(lock) = gpu() else {
            eprintln!("no GPU adapter available; skipping");
            return;
        };
        let (device, queue) = &*lock;
        let (w, h) = (120usize, 160usize);
        let layer = FigureLayer::mannequin();
        let frame = layer.render_frame(w, h);
        let mut figures = FigureGpu::new(device);
        let pixels = figures.bake(device, queue, &frame).unwrap();
        assert_eq!(pixels.len(), w * h);
        let opaque = |x: usize, y: usize| pixels[y * w + x].a() > 200;
        // The chest is drawn; the corners are empty.
        assert!(opaque(w / 2, h * 2 / 5), "the chest");
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            assert_eq!(pixels[y * w + x], Color32::TRANSPARENT, "({x}, {y})");
        }
        let covered = pixels.iter().filter(|p| p.a() > 0).count();
        assert!(covered > w * h / 30 && covered < w * h / 2, "{covered}");
    }

    #[test]
    fn a_front_cube_hides_the_one_behind_it() {
        let Some(lock) = gpu() else {
            eprintln!("no GPU adapter available; skipping");
            return;
        };
        let (device, queue) = &*lock;
        let (w, h) = (64usize, 64usize);
        let red = cube(Vec3::new(0.0, 0.0, 1.0), 0.6, [255, 0, 0, 255]);
        let blue = cube(Vec3::new(0.0, 0.0, -1.0), 1.4, [0, 0, 255, 255]);
        let mut figures = FigureGpu::new(device);
        // Whichever is drawn first, the nearer (red) shows in the middle,
        // the bigger one behind it round it.
        for order in [[red.clone(), blue.clone()], [blue.clone(), red.clone()]] {
            let frame = frame_of(&order, w, h);
            let pixels = figures.bake(device, queue, &frame).unwrap();
            let mid = pixels[(h / 2) * w + w / 2];
            assert!(mid.r() > 200 && mid.b() < 30, "middle {mid:?}");
            let side = pixels[(h / 2) * w + w / 2 + 13];
            assert!(side.b() > 200 && side.r() < 30, "side {side:?}");
        }
    }

    #[test]
    fn line_art_rims_the_figure_with_an_outline() {
        let Some(lock) = gpu() else {
            eprintln!("no GPU adapter available; skipping");
            return;
        };
        let (device, queue) = &*lock;
        let (w, h) = (64usize, 64usize);
        let model = cube(Vec3::ZERO, 1.0, [200, 60, 60, 255]);
        let mut figures = FigureGpu::new(device);
        let row = |pixels: &[Color32]| pixels[(h / 2) * w..(h / 2 + 1) * w].to_vec();
        let right = |row: &[Color32]| (0..w).rfind(|&x| row[x].a() > 0).unwrap();
        let shaded = row(&figures
            .bake(device, queue, &frame_of(std::slice::from_ref(&model), w, h))
            .unwrap());
        let mut frame = frame_of(std::slice::from_ref(&model), w, h);
        frame.style = RenderStyle::LineArt;
        frame.outline = 3.0;
        frame.line_fill = true;
        let lines = row(&figures.bake(device, queue, &frame).unwrap());
        // The outline reaches about its width past the shaded edge, black;
        // inside is white.
        let (edge, outer) = (right(&shaded), right(&lines));
        assert!((2..=4).contains(&(outer - edge)), "{edge} → {outer}");
        let rim = lines[outer - 1];
        assert!(rim.a() == 255 && rim.r() < 40, "{rim:?}");
        assert_eq!(lines[w / 2], Color32::WHITE);
        // See-through: the rim stays, the inside is empty.
        frame.line_fill = false;
        let hollow = row(&figures.bake(device, queue, &frame).unwrap());
        assert_eq!(hollow[w / 2], Color32::TRANSPARENT);
        assert!(hollow[outer - 1].a() == 255);
        assert_eq!(hollow[0], Color32::TRANSPARENT);
    }

    #[test]
    fn a_large_canvas_bakes_in_chunks_that_meet() {
        let Some(lock) = gpu() else {
            eprintln!("no GPU adapter available; skipping");
            return;
        };
        let (device, queue) = &*lock;
        // Wider than a chunk, the cube moved over the seam between two.
        let (w, h) = (BAKE_CHUNK as usize + 64, 96usize);
        let seam = BAKE_CHUNK as usize;
        let model = cube(Vec3::ZERO, 0.5, [0, 200, 0, 255]);
        let mut frame = frame_of(&[model], w, h);
        let shift = 2.0 * seam as f32 / w as f32 - 1.0;
        frame.view_proj = Mat4::from_translation(Vec3::new(shift, 0.0, 0.0))
            * Mat4::perspective_rh(0.4, w as f32 / h as f32, 0.1, 100.0)
            * Mat4::look_at_rh(Vec3::new(0.0, 0.0, 4.0), Vec3::ZERO, Vec3::Y);
        let mut figures = FigureGpu::new(device);
        let pixels = figures.bake(device, queue, &frame).unwrap();
        let row = &pixels[(h / 2) * w..(h / 2 + 1) * w];
        let left = (0..w).find(|&x| row[x].a() > 0).unwrap();
        let right = (0..w).rfind(|&x| row[x].a() > 0).unwrap();
        assert!(left < seam && right > seam, "{left}..{right}");
        // No gap or seam line between the chunks.
        for (x, p) in row.iter().enumerate().take(right).skip(left + 1) {
            assert!(p.a() == 255 && p.g() > 100, "{x}: {p:?}");
        }
        assert!(((left + right) as f32 / 2.0 - seam as f32).abs() <= 1.5);
    }
}
