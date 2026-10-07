//! Shader layers on the GPU: the on-screen composite when shader layers
//! show live, and baking a shader's frame into canvas pixels.
//!
//! Live, the canvas is drawn into an offscreen "accumulator" at the screen's
//! resolution, step by step ([`ComposePlan`]): each run of plain layers (its
//! own atlases, composited on the CPU) is drawn over it; each shader layer
//! renders with the accumulator as `iChannel0`, then is blended onto it with
//! the layer's mode ([`BLEND_SHADER`], ported from `canvas::blend_modes`).
//! Animating costs a few full-screen passes and no CPU work.
//!
//! The accumulator holds values in the document's blend space, like the
//! CPU compositor: linear light in an sRGB texture (so the hardware encodes
//! and blends in linear light), or the stored sRGB values in a plain one.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui::Color32;
use eframe::egui_wgpu::wgpu;
use wgpu::util::DeviceExt;

use crate::canvas::blend_modes::LayerBlend;
use crate::canvas::shader::{FrameUniforms, ShaderProgram};
use crate::canvas::storage::LayerId;

/// One step of a live composite; see [`crate::canvas::shader::LiveStep`].
pub enum ComposeStep {
    /// Draw run `run`'s atlases onto the accumulator with `blend`.
    Run { run: usize, blend: LayerBlend },
    /// Render a shader layer and blend it on.
    Shader {
        id: LayerId,
        program: Arc<ShaderProgram>,
        /// `iTime`, `iTimeDelta`, `iFrame`, `iFrameRate`.
        time: [f32; 4],
        blend: LayerBlend,
        opacity: f32,
    },
}

/// This frame's live composite.
pub struct ComposePlan {
    /// Render target size (the canvas's paint area in physical pixels).
    pub size: [u32; 2],
    /// The document blends stored sRGB values (else linear light).
    pub gamma: bool,
    /// Atlases per run: run `n` uses atlases `n * per_run ..`.
    pub per_run: usize,
    pub steps: Vec<ComposeStep>,
    /// Everything but `time` and `flags.y/z`, filled in per shader.
    pub frame: FrameUniforms,
}

const FULLSCREEN_VERTEX: &str = r#"
@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(i & 2u) * 2.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}
"#;

/// Runs drawn onto the accumulator, and the accumulator drawn on screen.
const COMPOSE_SHADER: &str = r#"
struct QuadOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_quad(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>) -> QuadOut {
    var out: QuadOut;
    out.position = vec4<f32>(pos, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

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

// Premultiplied linear light to sRGB values times alpha (the gamma blend
// space: `canvas::blend::gamma_color32_to_rgba`), and back.
fn gamma_premultiplied(c: vec4<f32>) -> vec4<f32> {
    if (c.a <= 0.0) { return vec4<f32>(0.0); }
    return vec4<f32>(gamma_from_linear_rgb(c.rgb / c.a) * c.a, c.a);
}

fn linear_premultiplied(c: vec4<f32>) -> vec4<f32> {
    if (c.a <= 0.0) { return vec4<f32>(0.0); }
    return vec4<f32>(linear_from_gamma_rgb(c.rgb / c.a) * c.a, c.a);
}

// Atlas texels (sRGB-decoded on read: premultiplied linear) in the
// accumulator's space.
@fragment
fn fs_run_linear(in: QuadOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv);
}

@fragment
fn fs_run_gamma(in: QuadOut) -> @location(0) vec4<f32> {
    return gamma_premultiplied(textureSample(tex, samp, in.uv));
}

// The accumulator on screen, as the atlases are drawn (egui's display
// path: stored values, dithered).
struct ScreenOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_screen(@builtin(vertex_index) i: u32) -> ScreenOut {
    let x = f32((i << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(i & 2u) * 2.0 - 1.0;
    var out: ScreenOut;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>(x * 0.5 + 0.5, 0.5 - y * 0.5);
    return out;
}

fn interleaved_gradient_noise(n: vec2<f32>) -> f32 {
    let f = 0.06711056 * n.x + 0.00583715 * n.y;
    return fract(52.9829189 * fract(f));
}

fn dither_interleaved(rgb: vec3<f32>, levels: f32, frag_coord: vec4<f32>) -> vec3<f32> {
    var noise = interleaved_gradient_noise(frag_coord.xy);
    noise = (noise - 0.5) * 0.95;
    return rgb + noise / (levels - 1.0);
}

fn screen(stored: vec3<f32>, a: f32, position: vec4<f32>) -> vec4<f32> {
    let rgb = dither_interleaved(stored, 256.0, position);
    if (FRAMEBUFFER_LINEAR) {
        return vec4<f32>(linear_from_gamma_rgb(rgb), a);
    }
    return vec4<f32>(rgb, a);
}

@fragment
fn fs_screen_linear(in: ScreenOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    return screen(gamma_from_linear_rgb(c.rgb), c.a, in.position);
}

@fragment
fn fs_screen_gamma(in: ScreenOut) -> @location(0) vec4<f32> {
    let c = linear_premultiplied(textureSample(tex, samp, in.uv));
    return screen(gamma_from_linear_rgb(c.rgb), c.a, in.position);
}
"#;

/// A layer blended onto the accumulator: `canvas::blend_modes::composite`
/// per pixel, in the accumulator's space.
const BLEND_SHADER: &str = r#"
struct BlendParams {
    mode: u32,
    pad0: u32,
    opacity: f32,
    pad1: f32,
    to_canvas_x: vec4<f32>,
    to_canvas_y: vec4<f32>,
};

@group(0) @binding(0) var base_tex: texture_2d<f32>;
@group(0) @binding(1) var top_tex: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: BlendParams;

fn color_dodge(b: f32, s: f32) -> f32 {
    if (b <= 0.0) { return 0.0; }
    if (s >= 1.0) { return 1.0; }
    return min(b / (1.0 - s), 1.0);
}

fn color_burn(b: f32, s: f32) -> f32 {
    if (b >= 1.0) { return 1.0; }
    if (s <= 0.0) { return 0.0; }
    return 1.0 - min((1.0 - b) / s, 1.0);
}

fn hard_light(b: f32, s: f32) -> f32 {
    if (s <= 0.5) { return b * 2.0 * s; }
    let s2 = 2.0 * s - 1.0;
    return b + s2 - b * s2;
}

fn soft_light(b: f32, s: f32) -> f32 {
    if (s <= 0.5) { return b - (1.0 - 2.0 * s) * b * (1.0 - b); }
    var d = sqrt(b);
    if (b <= 0.25) { d = ((16.0 * b - 12.0) * b + 4.0) * b; }
    return b + (2.0 * s - 1.0) * (d - b);
}

fn vivid_light(b: f32, s: f32) -> f32 {
    if (s <= 0.5) { return color_burn(b, 2.0 * s); }
    return color_dodge(b, 2.0 * s - 1.0);
}

// Mode codes: `blend_code` in shader_gpu.rs.
fn separable(mode: u32, b: f32, s: f32) -> f32 {
    switch mode {
        case 2u: { return min(b, s); }
        case 3u: { return b * s; }
        case 4u: { return color_burn(b, s); }
        case 5u: { return max(b + s - 1.0, 0.0); }
        case 7u: { return max(b, s); }
        case 8u: { return b + s - b * s; }
        case 9u: { return color_dodge(b, s); }
        case 10u: { return min(b + s, 1.0); }
        case 12u: { return hard_light(s, b); }
        case 13u: { return soft_light(b, s); }
        case 14u: { return hard_light(b, s); }
        case 15u: { return vivid_light(b, s); }
        case 16u: { return clamp(b + 2.0 * s - 1.0, 0.0, 1.0); }
        case 17u: {
            if (s <= 0.5) { return min(b, 2.0 * s); }
            return max(b, 2.0 * s - 1.0);
        }
        case 18u: {
            if (b + s >= 1.0) { return 1.0; }
            return 0.0;
        }
        case 19u: { return abs(b - s); }
        case 20u: { return b + s - 2.0 * b * s; }
        case 21u: { return max(b - s, 0.0); }
        case 22u: {
            if (s <= 0.0) {
                if (b <= 0.0) { return 0.0; }
                return 1.0;
            }
            return min(b / s, 1.0);
        }
        case 27u: {
            // Nothing where either channel is nothing.
            if (b <= 1.1920929e-7 || s <= 1.1920929e-7) { return 0.0; }
            return clamp(2.0 / (1.0 / b + 1.0 / s), 0.0, 1.0);
        }
        default: { return s; }
    }
}

fn lum(c: vec3<f32>) -> f32 {
    return 0.3 * c.r + 0.59 * c.g + 0.11 * c.b;
}

fn clip_color(c_in: vec3<f32>) -> vec3<f32> {
    var c = c_in;
    let l = lum(c);
    let n = min(min(c.r, c.g), c.b);
    let x = max(max(c.r, c.g), c.b);
    if (n < 0.0) {
        let d = l - n;
        if (d > 0.0) { c = vec3<f32>(l) + (c - vec3<f32>(l)) * l / d; }
    }
    if (x > 1.0) {
        let d = x - l;
        if (d > 0.0) { c = vec3<f32>(l) + (c - vec3<f32>(l)) * (1.0 - l) / d; }
    }
    return c;
}

fn set_lum(c: vec3<f32>, l: f32) -> vec3<f32> {
    return clip_color(c + vec3<f32>(l - lum(c)));
}

fn sat(c: vec3<f32>) -> f32 {
    return max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b);
}

fn set_sat(c: vec3<f32>, s: f32) -> vec3<f32> {
    let mx = max(max(c.r, c.g), c.b);
    let mn = min(min(c.r, c.g), c.b);
    if (mx <= mn) { return vec3<f32>(0.0); }
    return (c - vec3<f32>(mn)) * s / (mx - mn);
}

fn blend_color(mode: u32, b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 0u, 1u: { return s; }
        case 6u: {
            if (lum(s) < lum(b)) { return s; }
            return b;
        }
        case 11u: {
            if (lum(s) > lum(b)) { return s; }
            return b;
        }
        case 23u: { return set_lum(set_sat(s, sat(b)), lum(b)); }
        case 24u: { return set_lum(set_sat(b, sat(s)), lum(b)); }
        case 25u: { return set_lum(s, lum(b)); }
        case 26u: { return set_lum(b, lum(s)); }
        default: {
            return vec3<f32>(separable(mode, b.r, s.r), separable(mode, b.g, s.g), separable(mode, b.b, s.b));
        }
    }
}

fn pixel_noise(x: u32, y: u32) -> f32 {
    var h = (x * 0x9E3779B1u) ^ (y * 0x85EBCA77u);
    h = h ^ (h >> 15u);
    h = h * 0x2C1B3C6Du;
    h = h ^ (h >> 12u);
    return f32(h >> 8u) / 16777216.0;
}

const MIN_ALPHA: f32 = 1e-6;

fn composite(mode: u32, src: vec4<f32>, dst: vec4<f32>, noise: f32) -> vec4<f32> {
    let a_s = src.a;
    if (a_s <= MIN_ALPHA) { return dst; }
    if (mode == 0u) { return src + dst * (1.0 - a_s); }
    if (mode == 1u) {
        if (noise >= a_s) { return dst; }
        return vec4<f32>(src.rgb / a_s, 1.0);
    }
    let a_b = dst.a;
    if (a_b <= MIN_ALPHA) { return src; }
    let mixed = blend_color(
        mode,
        clamp(dst.rgb / a_b, vec3<f32>(0.0), vec3<f32>(1.0)),
        clamp(src.rgb / a_s, vec3<f32>(0.0), vec3<f32>(1.0)),
    );
    let both = a_s * a_b;
    return vec4<f32>(
        src.rgb * (1.0 - a_b) + dst.rgb * (1.0 - a_s) + both * mixed,
        a_s + a_b - both,
    );
}

@fragment
fn fs_blend(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy);
    let dst = textureLoad(base_tex, p, 0);
    let src = textureLoad(top_tex, p, 0) * params.opacity;
    let c = vec2<f32>(
        dot(params.to_canvas_x.xyz, vec3<f32>(pos.xy, 1.0)),
        dot(params.to_canvas_y.xyz, vec3<f32>(pos.xy, 1.0)),
    );
    let cell = vec2<u32>(max(floor(c), vec2<f32>(0.0)));
    return composite(params.mode, src, dst, pixel_noise(cell.x, cell.y));
}
"#;

/// A mode's code in [`BLEND_SHADER`].
pub fn blend_code(mode: LayerBlend) -> u32 {
    match mode {
        LayerBlend::Normal => 0,
        LayerBlend::Dissolve => 1,
        LayerBlend::Darken => 2,
        LayerBlend::Multiply => 3,
        LayerBlend::ColorBurn => 4,
        LayerBlend::LinearBurn => 5,
        LayerBlend::DarkerColor => 6,
        LayerBlend::Lighten => 7,
        LayerBlend::Screen => 8,
        LayerBlend::ColorDodge => 9,
        LayerBlend::LinearDodge => 10,
        LayerBlend::LighterColor => 11,
        LayerBlend::Overlay => 12,
        LayerBlend::SoftLight => 13,
        LayerBlend::HardLight => 14,
        LayerBlend::VividLight => 15,
        LayerBlend::LinearLight => 16,
        LayerBlend::PinLight => 17,
        LayerBlend::HardMix => 18,
        LayerBlend::Difference => 19,
        LayerBlend::Exclusion => 20,
        LayerBlend::Subtract => 21,
        LayerBlend::Divide => 22,
        LayerBlend::Hue => 23,
        LayerBlend::Saturation => 24,
        LayerBlend::Color => 25,
        LayerBlend::Luminosity => 26,
        LayerBlend::Parallel => 27,
    }
}

/// Accumulator format for a document's blend space.
fn accumulator_format(gamma: bool) -> wgpu::TextureFormat {
    if gamma {
        wgpu::TextureFormat::Rgba8Unorm
    } else {
        wgpu::TextureFormat::Rgba8UnormSrgb
    }
}

/// Bake target: sRGB-encoded premultiplied bytes, as canvas tiles store.
const BAKE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Largest square rendered at once when baking.
const BAKE_CHUNK: u32 = 2048;

/// Premultiplied-alpha "over", the hardware way.
const OVER: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
};

/// The accumulators (ping-pong) and the shader output, at one size.
struct Targets {
    size: [u32; 2],
    gamma: bool,
    /// `[accumulator 0, accumulator 1, shader output]` (tests read them back).
    #[cfg_attr(not(test), allow(dead_code))]
    textures: [wgpu::Texture; 3],
    views: [wgpu::TextureView; 3],
}

/// A user shader's pipeline for one target format; `None` if the GPU
/// refused it (not retried until the source changes).
struct UserPipeline {
    hash: u64,
    pipeline: Option<wgpu::RenderPipeline>,
}

pub struct ShaderGpu {
    /// A texture and its sampler (the accumulator drawn on screen).
    screen_layout: wgpu::BindGroupLayout,
    run_linear: wgpu::RenderPipeline,
    run_gamma: wgpu::RenderPipeline,
    screen_linear: wgpu::RenderPipeline,
    screen_gamma: wgpu::RenderPipeline,
    blend_layout: wgpu::BindGroupLayout,
    blend_linear: wgpu::RenderPipeline,
    blend_gamma: wgpu::RenderPipeline,
    user_layout: wgpu::BindGroupLayout,
    user_pipeline_layout: wgpu::PipelineLayout,
    fullscreen: wgpu::ShaderModule,
    nearest: wgpu::Sampler,
    linear: wgpu::Sampler,
    users: HashMap<(LayerId, wgpu::TextureFormat), UserPipeline>,
    targets: Option<Targets>,
    /// This frame's result (an accumulator's), if composited.
    result: Option<wgpu::BindGroup>,
    /// Which accumulator holds it.
    result_index: usize,
}

fn texture_entry(binding: u32, filterable: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            multisampled: false,
            view_dimension: wgpu::TextureViewDimension::D2,
            sample_type: wgpu::TextureSampleType::Float { filterable },
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn color_pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    view: &'a wgpu::TextureView,
    clear: bool,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("shader layers"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: if clear {
                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                } else {
                    wgpu::LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    })
}

/// Poll a future that is expected to be ready at once (wgpu's error
/// scopes on native backends); `None` if it isn't.
fn ready_now<F: std::future::Future>(future: F) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(v) => Some(v),
        std::task::Poll::Pending => None,
    }
}

impl ShaderGpu {
    pub fn new(
        device: &wgpu::Device,
        atlas_layout: &wgpu::BindGroupLayout,
        quad_layout: wgpu::VertexBufferLayout<'static>,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        let compose = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader layers compose"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "const FRAMEBUFFER_LINEAR: bool = {};\n{COMPOSE_SHADER}",
                    target_format.is_srgb()
                )
                .into(),
            ),
        });
        let blend = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader layers blend"),
            source: wgpu::ShaderSource::Wgsl(format!("{FULLSCREEN_VERTEX}{BLEND_SHADER}").into()),
        });
        let fullscreen = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader layers vertex"),
            source: wgpu::ShaderSource::Wgsl(FULLSCREEN_VERTEX.into()),
        });
        let atlas_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("shader layers runs"),
                bind_group_layouts: &[atlas_layout],
                push_constant_ranges: &[],
            });
        let screen_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shader layers screen"),
            entries: &[texture_entry(0, true), sampler_entry(1)],
        });
        let screen_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("shader layers screen"),
                bind_group_layouts: &[&screen_layout],
                push_constant_ranges: &[],
            });
        let blend_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shader layers blend"),
            entries: &[
                texture_entry(0, false),
                texture_entry(1, false),
                uniform_entry(2),
            ],
        });
        let blend_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("shader layers blend"),
                bind_group_layouts: &[&blend_layout],
                push_constant_ranges: &[],
            });
        let user_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shader layer"),
            entries: &[uniform_entry(0), texture_entry(1, true), sampler_entry(2)],
        });
        let user_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shader layer"),
            bind_group_layouts: &[&user_layout],
            push_constant_ranges: &[],
        });
        let pipeline = |label: &str,
                        layout: &wgpu::PipelineLayout,
                        module: &wgpu::ShaderModule,
                        vertex: &str,
                        buffers: &[wgpu::VertexBufferLayout<'static>],
                        fragment: &str,
                        format: wgpu::TextureFormat,
                        blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: vertex,
                    compilation_options: Default::default(),
                    buffers,
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: fragment,
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
        let quads = [quad_layout];
        let run = |fragment, gamma| {
            pipeline(
                "shader layers run",
                &atlas_pipeline_layout,
                &compose,
                "vs_quad",
                &quads,
                fragment,
                accumulator_format(gamma),
                Some(OVER),
            )
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
        let screen = |fragment| {
            pipeline(
                "shader layers screen",
                &screen_pipeline_layout,
                &compose,
                "vs_screen",
                &[],
                fragment,
                target_format,
                Some(egui_blend),
            )
        };
        let blend_pipeline = |gamma| {
            pipeline(
                "shader layers blend",
                &blend_pipeline_layout,
                &blend,
                "vs_fullscreen",
                &[],
                "fs_blend",
                accumulator_format(gamma),
                None,
            )
        };
        let sampler = |label, filter| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(label),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter: filter,
                min_filter: filter,
                ..Default::default()
            })
        };
        Self {
            screen_layout,
            run_linear: run("fs_run_linear", false),
            run_gamma: run("fs_run_gamma", true),
            screen_linear: screen("fs_screen_linear"),
            screen_gamma: screen("fs_screen_gamma"),
            blend_linear: blend_pipeline(false),
            blend_gamma: blend_pipeline(true),
            blend_layout,
            user_layout,
            user_pipeline_layout,
            fullscreen,
            nearest: sampler("shader layers nearest", wgpu::FilterMode::Nearest),
            linear: sampler("shader layers linear", wgpu::FilterMode::Linear),
            users: HashMap::new(),
            targets: None,
            result: None,
            result_index: 0,
        }
    }

    /// The pipeline for `program` rendering to `format`, created if needed.
    /// A pipeline the GPU rejects is remembered as `None` (the frame shows
    /// the layer empty) instead of crashing on wgpu's error handler.
    fn user_pipeline(
        &mut self,
        device: &wgpu::Device,
        id: LayerId,
        program: &ShaderProgram,
        format: wgpu::TextureFormat,
    ) -> Option<&wgpu::RenderPipeline> {
        let stale = self
            .users
            .get(&(id, format))
            .is_none_or(|p| p.hash != program.hash);
        if stale {
            device.push_error_scope(wgpu::ErrorFilter::Internal);
            device.push_error_scope(wgpu::ErrorFilter::Validation);
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("shader layer"),
                source: wgpu::ShaderSource::Naga(Cow::Owned(program.module.clone())),
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("shader layer"),
                layout: Some(&self.user_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.fullscreen,
                    entry_point: "vs_fullscreen",
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: "main",
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
            let validation = ready_now(device.pop_error_scope());
            let internal = ready_now(device.pop_error_scope());
            let failed = [validation, internal]
                .into_iter()
                .any(|e| !matches!(e, Some(None)));
            if failed {
                log::warn!("shader layer pipeline rejected by the GPU");
            }
            self.users.insert(
                (id, format),
                UserPipeline {
                    hash: program.hash,
                    pipeline: (!failed).then_some(pipeline),
                },
            );
        }
        self.users
            .get(&(id, format))
            .and_then(|p| p.pipeline.as_ref())
    }

    fn ensure_targets(&mut self, device: &wgpu::Device, size: [u32; 2], gamma: bool) {
        if self
            .targets
            .as_ref()
            .is_some_and(|t| t.size == size && t.gamma == gamma)
        {
            return;
        }
        let texture = || {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("shader layers accumulator"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: accumulator_format(gamma),
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let textures = [texture(), texture(), texture()];
        let views = [0, 1, 2].map(|i| textures[i].create_view(&Default::default()));
        self.targets = Some(Targets {
            size,
            gamma,
            textures,
            views,
        });
    }

    fn user_group(
        &self,
        device: &wgpu::Device,
        uniforms: &FrameUniforms,
        channel: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("shader layer frame"),
            contents: &uniforms.as_bytes(),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shader layer"),
            layout: &self.user_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(channel),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.linear),
                },
            ],
        })
    }

    /// Record this frame's live composite into `encoder`. `quads` and
    /// `draws` are the canvas's atlas quads (every run's) and `atlas_groups`
    /// each atlas's display bind group.
    pub fn compose(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        plan: &ComposePlan,
        quads: Option<&wgpu::Buffer>,
        draws: &[(usize, std::ops::Range<u32>)],
        atlas_groups: &[&wgpu::BindGroup],
    ) {
        self.result = None;
        if plan.size[0] == 0 || plan.size[1] == 0 {
            return;
        }
        let max = device.limits().max_texture_dimension_2d;
        let size = [plan.size[0].min(max), plan.size[1].min(max)];
        self.ensure_targets(device, size, plan.gamma);
        let format = accumulator_format(plan.gamma);
        // Forget the pipelines of shader layers gone from the screen.
        let showing: Vec<LayerId> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                ComposeStep::Shader { id, .. } => Some(*id),
                ComposeStep::Run { .. } => None,
            })
            .collect();
        self.users.retain(|(id, _), _| showing.contains(id));
        // Pipelines first: creating them needs `&mut self`.
        for step in &plan.steps {
            if let ComposeStep::Shader { id, program, .. } = step {
                self.user_pipeline(device, *id, program, format);
            }
        }
        let Some(targets) = self.targets.as_ref() else {
            return;
        };
        let run_pipeline = if plan.gamma {
            &self.run_gamma
        } else {
            &self.run_linear
        };
        let mut current = 0usize;
        drop(color_pass(encoder, &targets.views[current], true));
        for step in &plan.steps {
            match step {
                ComposeStep::Run { run, blend } => {
                    let Some(quads) = quads else { continue };
                    // Normal runs go straight on; a run with a mode is drawn
                    // alone first, then blended on with it.
                    let normal = matches!(blend, LayerBlend::Normal | LayerBlend::Dissolve);
                    let target = if normal { current } else { 2 };
                    let atlases = run * plan.per_run..(run + 1) * plan.per_run;
                    {
                        let mut pass = color_pass(encoder, &targets.views[target], !normal);
                        pass.set_pipeline(run_pipeline);
                        pass.set_vertex_buffer(0, quads.slice(..));
                        for (atlas, range) in draws.iter().filter(|(a, _)| atlases.contains(a)) {
                            if let Some(group) = atlas_groups.get(*atlas) {
                                pass.set_bind_group(0, group, &[]);
                                pass.draw(range.clone(), 0..1);
                            }
                        }
                    }
                    if !normal {
                        current = self.blend_on(device, encoder, plan, current, *blend, 1.0);
                    }
                }
                ComposeStep::Shader {
                    id,
                    time,
                    blend,
                    opacity,
                    ..
                } => {
                    let Some(pipeline) = self
                        .users
                        .get(&(*id, format))
                        .and_then(|p| p.pipeline.as_ref())
                    else {
                        continue;
                    };
                    let mut uniforms = plan.frame;
                    uniforms.time = *time;
                    let g = if plan.gamma { 1.0 } else { 0.0 };
                    uniforms.flags[1] = g;
                    uniforms.flags[2] = g;
                    let group = self.user_group(device, &uniforms, &targets.views[current]);
                    {
                        let mut pass = color_pass(encoder, &targets.views[2], true);
                        pass.set_pipeline(pipeline);
                        pass.set_bind_group(0, &group, &[]);
                        pass.draw(0..3, 0..1);
                    }
                    current = self.blend_on(device, encoder, plan, current, *blend, *opacity);
                }
            }
        }
        self.result_index = current;
        self.result = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shader layers result"),
            layout: &self.screen_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&targets.views[current]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.nearest),
                },
            ],
        }));
    }

    pub fn clear_result(&mut self) {
        self.result = None;
    }

    /// Blend the shader output texture onto accumulator `current` with
    /// `mode` at `opacity`, into the other accumulator; returns that one.
    fn blend_on(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        plan: &ComposePlan,
        current: usize,
        mode: LayerBlend,
        opacity: f32,
    ) -> usize {
        let Some(targets) = self.targets.as_ref() else {
            return current;
        };
        let next = 1 - current;
        let mut params = Vec::with_capacity(48);
        params.extend(blend_code(mode).to_ne_bytes());
        params.extend(0u32.to_ne_bytes());
        params.extend(opacity.to_ne_bytes());
        params.extend(0f32.to_ne_bytes());
        for v in plan.frame.to_canvas_x.iter().chain(&plan.frame.to_canvas_y) {
            params.extend(v.to_ne_bytes());
        }
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("shader layer blend"),
            contents: &params,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shader layer blend"),
            layout: &self.blend_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&targets.views[current]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&targets.views[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        let pipeline = if plan.gamma {
            &self.blend_gamma
        } else {
            &self.blend_linear
        };
        let mut pass = color_pass(encoder, &targets.views[next], true);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
        next
    }

    /// The accumulator holding the last composite.
    #[cfg(test)]
    pub(crate) fn result_texture(&self) -> Option<&wgpu::Texture> {
        self.result.as_ref()?;
        Some(&self.targets.as_ref()?.textures[self.result_index])
    }

    /// Whether [`Self::compose`] left a picture to show this frame.
    pub fn has_result(&self) -> bool {
        self.result.is_some()
    }

    /// Draw the composite over the canvas area.
    pub fn paint(&self, render_pass: &mut wgpu::RenderPass<'static>) {
        let Some(group) = &self.result else {
            return;
        };
        let gamma = self.targets.as_ref().is_some_and(|t| t.gamma);
        render_pass.set_pipeline(if gamma {
            &self.screen_gamma
        } else {
            &self.screen_linear
        });
        render_pass.set_bind_group(0, group, &[]);
        render_pass.draw(0..3, 0..1);
    }

    /// Render `program` for the whole `width`×`height` canvas at the time
    /// in `frame.time`, with `below` (the layers under it, premultiplied
    /// sRGB like canvas tiles) as `iChannel0`. Returns the pixels, row by
    /// row, in the canvas tiles' format.
    #[allow(clippy::too_many_arguments)]
    pub fn bake(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: LayerId,
        program: &ShaderProgram,
        mut frame: FrameUniforms,
        width: usize,
        height: usize,
        below: &[Color32],
    ) -> Result<Vec<Color32>, String> {
        if width == 0 || height == 0 || below.len() != width * height {
            return Err("nothing to bake".into());
        }
        if self
            .user_pipeline(device, id, program, BAKE_FORMAT)
            .is_none()
        {
            return Err("the GPU rejected this shader".into());
        }
        let max = device.limits().max_texture_dimension_2d as usize;
        // iChannel0: the layers below, shrunk (nearest) if the canvas is
        // larger than a texture can be.
        let step = width.max(height).div_ceil(max).max(1);
        let (cw, ch) = (width.div_ceil(step), height.div_ceil(step));
        let mut channel_bytes = Vec::with_capacity(cw * ch * 4);
        for y in 0..ch {
            for x in 0..cw {
                channel_bytes.extend(below[y * step * width + x * step].to_array());
            }
        }
        let channel = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("shader layer bake channel"),
                size: wgpu::Extent3d {
                    width: cw as u32,
                    height: ch as u32,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &channel_bytes,
        );
        let channel_view = channel.create_view(&Default::default());
        frame.resolution = [width as f32, height as f32, 1.0, 0.0];
        frame.to_channel_x = [1.0 / width as f32, 0.0, 0.0, 0.0];
        frame.to_channel_y = [0.0, 1.0 / height as f32, 0.0, 0.0];
        frame.flags = [0.0; 4];

        let chunk = BAKE_CHUNK.min(max as u32) as usize;
        let mut out = vec![Color32::TRANSPARENT; width * height];
        for oy in (0..height).step_by(chunk) {
            for ox in (0..width).step_by(chunk) {
                let (w, h) = ((width - ox).min(chunk), (height - oy).min(chunk));
                frame.to_canvas_x = [1.0, 0.0, ox as f32, 0.0];
                frame.to_canvas_y = [0.0, 1.0, oy as f32, 0.0];
                let pixels = self.bake_chunk(device, queue, id, &frame, &channel_view, w, h)?;
                for row in 0..h {
                    let dst = (oy + row) * width + ox;
                    out[dst..dst + w].copy_from_slice(&pixels[row * w..(row + 1) * w]);
                }
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn bake_chunk(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: LayerId,
        frame: &FrameUniforms,
        channel: &wgpu::TextureView,
        width: usize,
        height: usize,
    ) -> Result<Vec<Color32>, String> {
        let pipeline = self
            .users
            .get(&(id, BAKE_FORMAT))
            .and_then(|p| p.pipeline.as_ref())
            .ok_or("the GPU rejected this shader")?;
        let (w, h) = (width as u32, height as u32);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shader layer bake"),
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
        let group = self.user_group(device, frame, channel);
        let padded = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shader layer bake readback"),
            size: u64::from(padded) * u64::from(h),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = color_pass(&mut encoder, &view, true);
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
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
            .map_err(|_| "the GPU didn't return the frame".to_string())?
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
    use crate::canvas::blend_modes::composite;
    use eframe::egui::Rgba;

    fn gpu() -> Option<std::sync::MutexGuard<'static, (wgpu::Device, wgpu::Queue)>> {
        crate::app::view::test_gpu()
    }

    fn shader_gpu(device: &wgpu::Device) -> ShaderGpu {
        let canvas = crate::app::view::gpu_canvas::GpuCanvas::new(
            device,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );
        canvas.into_shader_gpu()
    }

    #[test]
    fn every_mode_has_its_own_code() {
        let mut codes: Vec<u32> = LayerBlend::GROUPS
            .iter()
            .flat_map(|g| g.iter())
            .map(|&m| blend_code(m))
            .collect();
        codes.sort_unstable();
        assert_eq!(codes, (0..28).collect::<Vec<_>>());
    }

    #[test]
    fn a_baked_shader_is_its_colour_everywhere() {
        let Some(lock) = gpu() else {
            eprintln!("no GPU adapter available; skipping");
            return;
        };
        let (device, queue) = &*lock;
        let mut gpu = shader_gpu(device);
        let program = crate::canvas::shader::compile(
            "void mainImage(out vec4 c, in vec2 p) { c = vec4(1.0, 0.5, 0.0, 0.5); }",
        )
        .unwrap();
        let (w, h) = (70, 40);
        let below = vec![Color32::TRANSPARENT; w * h];
        let pixels = gpu
            .bake(
                device,
                queue,
                LayerId(1),
                &program,
                FrameUniforms::default(),
                w,
                h,
                &below,
            )
            .unwrap();
        let expected = Color32::from_rgba_unmultiplied(255, 128, 0, 128);
        for p in &pixels {
            for (a, e) in p.to_array().iter().zip(expected.to_array()) {
                assert!((*a as i16 - e as i16).abs() <= 1, "{p:?} vs {expected:?}");
            }
        }
    }

    #[test]
    fn channel_zero_reads_the_layers_below_at_the_same_pixel() {
        let Some(lock) = gpu() else {
            eprintln!("no GPU adapter available; skipping");
            return;
        };
        let (device, queue) = &*lock;
        let mut gpu = shader_gpu(device);
        let program = crate::canvas::shader::compile(
            "void mainImage(out vec4 c, in vec2 p) { c = texture(iChannel0, p / iResolution.xy); }",
        )
        .unwrap();
        let (w, h) = (33, 17);
        let below: Vec<Color32> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                Color32::from_rgba_unmultiplied((x * 7) as u8, (y * 13) as u8, 200, 255)
            })
            .collect();
        let pixels = gpu
            .bake(
                device,
                queue,
                LayerId(1),
                &program,
                FrameUniforms::default(),
                w,
                h,
                &below,
            )
            .unwrap();
        for (i, (p, b)) in pixels.iter().zip(&below).enumerate() {
            for (a, e) in p.to_array().iter().zip(b.to_array()) {
                assert!(
                    (*a as i16 - e as i16).abs() <= 1,
                    "pixel {i}: {p:?} vs {b:?}"
                );
            }
        }
    }

    /// Blend `src` over `dst` on the GPU with `mode` (in linear light).
    fn gpu_blend(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        gpu: &ShaderGpu,
        mode: LayerBlend,
        pairs: &[(Rgba, Rgba)],
    ) -> Vec<[f32; 4]> {
        let format = wgpu::TextureFormat::Rgba32Float;
        let n = pairs.len() as u32;
        let make = |pick: &dyn Fn(&(Rgba, Rgba)) -> Rgba| {
            let data: Vec<u8> = pairs
                .iter()
                .flat_map(|p| pick(p).to_array())
                .flat_map(|f| f.to_ne_bytes())
                .collect();
            device
                .create_texture_with_data(
                    queue,
                    &wgpu::TextureDescriptor {
                        label: None,
                        size: wgpu::Extent3d {
                            width: n,
                            height: 1,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                    wgpu::util::TextureDataOrder::LayerMajor,
                    &data,
                )
                .create_view(&Default::default())
        };
        let base = make(&|p| p.1);
        let top = make(&|p| p.0);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(format!("{FULLSCREEN_VERTEX}{BLEND_SHADER}").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&gpu.blend_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: "vs_fullscreen",
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: "fs_blend",
                compilation_options: Default::default(),
                targets: &[Some(format.into())],
            }),
            multiview: None,
            cache: None,
        });
        let mut params = Vec::new();
        params.extend(blend_code(mode).to_ne_bytes());
        params.extend(0u32.to_ne_bytes());
        params.extend(1f32.to_ne_bytes());
        params.extend(0f32.to_ne_bytes());
        for v in [1f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0] {
            params.extend(v.to_ne_bytes());
        }
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: &params,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &gpu.blend_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&base),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&top),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: n,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256.max(u64::from(n) * 16),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = color_pass(&mut encoder, &view, true);
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &readback,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some((n * 16).next_multiple_of(256)),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: n,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        data[..n as usize * 16]
            .as_chunks::<16>()
            .0
            .iter()
            .map(|c| {
                let f = |i: usize| f32::from_ne_bytes(c[i * 4..i * 4 + 4].try_into().unwrap());
                [f(0), f(1), f(2), f(3)]
            })
            .collect()
    }

    #[test]
    fn gpu_blend_modes_match_the_cpu_compositor() {
        let Some(lock) = gpu() else {
            eprintln!("no GPU adapter available; skipping");
            return;
        };
        let (device, queue) = &*lock;
        let gpu = shader_gpu(device);
        let values = [0.0f32, 0.1, 0.35, 0.5, 0.8, 1.0];
        let mut pairs = Vec::new();
        for (i, &a) in values.iter().enumerate() {
            for (j, &b) in values.iter().enumerate() {
                let alpha_s = [1.0, 0.6, 0.3][(i + j) % 3];
                let alpha_b = [1.0, 0.5][(i * 3 + j) % 2];
                let src = Rgba::from_rgba_premultiplied(
                    a * alpha_s,
                    b * alpha_s,
                    values[(i + 2) % 6] * alpha_s,
                    alpha_s,
                );
                let dst = Rgba::from_rgba_premultiplied(
                    b * alpha_b,
                    values[(j + 3) % 6] * alpha_b,
                    a * alpha_b,
                    alpha_b,
                );
                pairs.push((src, dst));
            }
        }
        for &mode in LayerBlend::GROUPS.iter().flat_map(|g| g.iter()) {
            if mode == LayerBlend::Dissolve {
                continue;
            }
            let out = gpu_blend(device, queue, &gpu, mode, &pairs);
            for ((src, dst), got) in pairs.iter().zip(out) {
                let want = composite(mode, *src, *dst, 0.0).to_array();
                for (g, w) in got.iter().zip(want) {
                    assert!(
                        (g - w).abs() < 1e-3,
                        "{mode:?}: {src:?} over {dst:?}: gpu {got:?} cpu {want:?}"
                    );
                }
            }
        }
    }
}
