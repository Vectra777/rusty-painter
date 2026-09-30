//! Shader layers: a layer whose pixels come from a GLSL fragment shader
//! written Shadertoy style (`mainImage(out vec4 fragColor, in vec2
//! fragCoord)` with `iTime`, `iResolution`, `iMouse`, and `iChannel0` = the
//! layers below it).
//!
//! On screen the shader runs on the GPU every frame (see
//! `app::view::shader_gpu`); the layer's own tiles hold its last *baked*
//! frame, which export, merging, thumbnails and colour picking use like any
//! other layer's pixels. This module is GPU-free: the layer's data, the GLSL
//! wrapper, compiling (naga) and how the stack splits around shader layers.

use serde::{Deserialize, Serialize};

use super::blend_modes::LayerBlend;
use super::storage::{Canvas, LayerId, LayerKind};

/// A shader layer's settings (the shader source is the layer's content).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShaderLayer {
    /// GLSL defining `mainImage`.
    pub source: String,
    /// Seconds of shader time (`iTime`) shown when not playing, and where
    /// playing resumes from.
    #[serde(default)]
    pub time: f32,
    /// Shader seconds per real second.
    #[serde(default = "default_speed")]
    pub speed: f32,
}

fn default_speed() -> f32 {
    1.0
}

impl ShaderLayer {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            time: 0.0,
            speed: 1.0,
        }
    }
}

/// Built-in starting points: `(name, source)`. The ones reading the layers
/// below say so in their name.
pub const TEMPLATES: &[(&str, &str)] = &[
    ("Plasma", PLASMA),
    ("Rings", RINGS),
    ("Clouds", CLOUDS),
    ("Stars", STARS),
    ("Ripple (layers below)", RIPPLE),
    ("Chromatic split (layers below)", CHROMATIC),
    ("Glow (layers below)", GLOW),
    ("Vignette (layers below)", VIGNETTE),
];

const PLASMA: &str = "\
// Shadertoy style: fragCoord is in canvas pixels (origin bottom-left),
// iResolution is the canvas size, iTime the time in seconds.
void mainImage(out vec4 fragColor, in vec2 fragCoord)
{
    vec2 uv = fragCoord / iResolution.xy;
    float v = sin(uv.x * 10.0 + iTime)
            + sin((uv.y * 10.0 + iTime) * 0.5)
            + sin((uv.x * 10.0 + uv.y * 10.0 + iTime) * 0.5);
    vec2 c = uv * 10.0 + vec2(sin(iTime / 3.0), cos(iTime / 2.0)) * 5.0;
    v += sin(sqrt(c.x * c.x + c.y * c.y + 1.0) + iTime);
    vec3 col = 0.5 + 0.5 * cos(3.14159 * v + vec3(0.0, 2.0, 4.0));
    fragColor = vec4(col, 1.0);
}
";

const RINGS: &str = "\
void mainImage(out vec4 fragColor, in vec2 fragCoord)
{
    vec2 p = (2.0 * fragCoord - iResolution.xy) / iResolution.y;
    float d = length(p);
    float rings = 0.5 + 0.5 * sin(d * 30.0 - iTime * 4.0);
    vec3 col = mix(vec3(0.05, 0.1, 0.3), vec3(1.0, 0.6, 0.2), rings);
    // Fades out towards the edges: a transparent layer over your painting.
    fragColor = vec4(col, smoothstep(1.2, 0.2, d));
}
";

const CLOUDS: &str = "\
float hash(vec2 p)
{
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}

float noise(vec2 p)
{
    vec2 i = floor(p);
    vec2 f = fract(p);
    vec2 u = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x),
               mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x), u.y);
}

float fbm(vec2 p)
{
    float v = 0.0;
    float a = 0.5;
    for (int i = 0; i < 6; i++) {
        v += a * noise(p);
        p *= 2.0;
        a *= 0.5;
    }
    return v;
}

void mainImage(out vec4 fragColor, in vec2 fragCoord)
{
    vec2 uv = fragCoord / iResolution.y * 3.0;
    float n = fbm(uv + vec2(iTime * 0.1, 0.0) + fbm(uv + iTime * 0.05));
    vec3 sky = mix(vec3(0.25, 0.45, 0.8), vec3(0.6, 0.8, 1.0), fragCoord.y / iResolution.y);
    fragColor = vec4(mix(sky, vec3(1.0), smoothstep(0.4, 0.8, n)), 1.0);
}
";

const STARS: &str = "\
float hash(vec2 p)
{
    return fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453);
}

void mainImage(out vec4 fragColor, in vec2 fragCoord)
{
    vec2 cell = floor(fragCoord / 24.0);
    vec2 local = fract(fragCoord / 24.0) - 0.5;
    float h = hash(cell);
    vec2 offset = vec2(hash(cell + 1.7), hash(cell + 3.1)) - 0.5;
    float d = length(local - offset * 0.6);
    float twinkle = 0.5 + 0.5 * sin(iTime * (1.0 + 3.0 * h) + h * 40.0);
    float star = step(0.85, h) * smoothstep(0.08, 0.0, d) * twinkle;
    // Only the stars show; the rest of the layer stays transparent.
    fragColor = vec4(vec3(1.0, 0.95, 0.8), star);
}
";

const RIPPLE: &str = "\
// texture(iChannel0, uv) reads the layers below this one.
void mainImage(out vec4 fragColor, in vec2 fragCoord)
{
    vec2 uv = fragCoord / iResolution.xy;
    vec2 centre = iMouse.xy / iResolution.xy;
    if (iMouse.x <= 0.0) centre = vec2(0.5);
    vec2 d = uv - centre;
    float dist = length(d * vec2(iResolution.x / iResolution.y, 1.0));
    float wave = sin(dist * 60.0 - iTime * 6.0) * 0.006 * smoothstep(0.6, 0.0, dist);
    fragColor = texture(iChannel0, uv + normalize(d + 1e-6) * wave);
}
";

const CHROMATIC: &str = "\
void mainImage(out vec4 fragColor, in vec2 fragCoord)
{
    vec2 uv = fragCoord / iResolution.xy;
    vec2 shift = vec2(0.004 + 0.003 * sin(iTime * 2.0), 0.0);
    vec4 r = texture(iChannel0, uv + shift);
    vec4 g = texture(iChannel0, uv);
    vec4 b = texture(iChannel0, uv - shift);
    fragColor = vec4(r.r, g.g, b.b, max(max(r.a, g.a), b.a));
}
";

const GLOW: &str = "\
void mainImage(out vec4 fragColor, in vec2 fragCoord)
{
    vec2 uv = fragCoord / iResolution.xy;
    vec2 px = 1.0 / iResolution.xy;
    vec3 sum = vec3(0.0);
    float weight = 0.0;
    for (int i = 0; i < 16; i++) {
        float a = float(i) * 2.39996;
        float r = sqrt(float(i) + 0.5) * 6.0;
        vec4 c = texture(iChannel0, uv + vec2(cos(a), sin(a)) * r * px);
        // Only the bright parts glow.
        sum += c.rgb * smoothstep(0.6, 1.0, max(c.r, max(c.g, c.b)));
        weight += 1.0;
    }
    vec4 base = texture(iChannel0, uv);
    float pulse = 0.8 + 0.4 * sin(iTime * 2.0);
    fragColor = vec4(base.rgb + sum / weight * pulse, base.a);
}
";

const VIGNETTE: &str = "\
void mainImage(out vec4 fragColor, in vec2 fragCoord)
{
    vec2 uv = fragCoord / iResolution.xy;
    vec4 c = texture(iChannel0, uv);
    float v = smoothstep(0.9, 0.3, length(uv - 0.5));
    float breathe = 0.85 + 0.15 * sin(iTime);
    fragColor = vec4(c.rgb * mix(0.35, 1.0, v * breathe), c.a);
}
";

/// Per-frame values every shader sees (std140: all `vec4`s). The fields
/// match the `RpFrame` block in [`PRELUDE`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameUniforms {
    /// Canvas width, height, 1, 0 (`iResolution`).
    pub resolution: [f32; 4],
    /// `iMouse`, in canvas pixels with the origin bottom-left.
    pub mouse: [f32; 4],
    /// `iDate`: year, month (0-based), day, seconds into the day.
    pub date: [f32; 4],
    /// `iTime`, `iTimeDelta`, `iFrame`, `iFrameRate`.
    pub time: [f32; 4],
    /// Canvas pixel (y down) of a render target pixel: `x = dot(xyz, (px, py, 1))`.
    pub to_canvas_x: [f32; 4],
    pub to_canvas_y: [f32; 4],
    /// `iChannel0` texture coordinate of a canvas pixel (y down).
    pub to_channel_x: [f32; 4],
    pub to_channel_y: [f32; 4],
    /// x: the canvas wraps around; y: the channel holds sRGB values times
    /// alpha (the gamma blend space; else premultiplied linear light); z:
    /// write those too (else premultiplied linear, for an sRGB target); w:
    /// unused.
    pub flags: [f32; 4],
}

impl FrameUniforms {
    pub fn as_bytes(&self) -> Vec<u8> {
        let fields = [
            self.resolution,
            self.mouse,
            self.date,
            self.time,
            self.to_canvas_x,
            self.to_canvas_y,
            self.to_channel_x,
            self.to_channel_y,
            self.flags,
        ];
        fields
            .iter()
            .flatten()
            .flat_map(|f| f.to_ne_bytes())
            .collect()
    }
}

/// Everything before the user's code: the uniforms, `iChannel0` and the
/// Shadertoy names. Bindings: 0 = [`FrameUniforms`], 1 = the channel
/// texture, 2 = its sampler.
const PRELUDE: &str = r#"#version 450
layout(set = 0, binding = 0) uniform RpFrame {
    vec4 resolution;
    vec4 mouse;
    vec4 date;
    vec4 time;
    vec4 to_canvas_x;
    vec4 to_canvas_y;
    vec4 to_channel_x;
    vec4 to_channel_y;
    vec4 flags;
} _rp;
layout(set = 0, binding = 1) uniform texture2D _rp_channel0_tex;
layout(set = 0, binding = 2) uniform sampler _rp_channel0_samp;
layout(location = 0) out vec4 _rp_out;
vec2 _rp_unwrap = vec2(0.0);
vec3 _rp_linear(vec3 c) {
    return mix(c / 12.92, pow((c + 0.055) / 1.055, vec3(2.4)), step(vec3(0.04045), c));
}
vec3 _rp_gamma(vec3 c) {
    return mix(c * 12.92, 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, step(vec3(0.0031308), c));
}
vec4 _rp_channel(vec2 uv) {
    vec2 c = uv * _rp.resolution.xy;
    c = vec2(c.x, _rp.resolution.y - c.y);
    // Outside the canvas: its edge (Shadertoy's clamp), unless it wraps.
    if (_rp.flags.x < 0.5) {
        c = clamp(c, vec2(0.5), _rp.resolution.xy - 0.5);
    }
    c += _rp_unwrap;
    vec2 t = vec2(dot(_rp.to_channel_x.xyz, vec3(c, 1.0)), dot(_rp.to_channel_y.xyz, vec3(c, 1.0)));
    vec4 p = textureLod(sampler2D(_rp_channel0_tex, _rp_channel0_samp), t, 0.0);
    if (p.a <= 0.0) {
        return vec4(0.0);
    }
    vec3 straight = p.rgb / p.a;
    if (_rp.flags.y < 0.5) {
        straight = _rp_gamma(clamp(straight, 0.0, 1.0));
    }
    return vec4(clamp(straight, 0.0, 1.0), min(p.a, 1.0));
}
#define iResolution _rp.resolution.xyz
#define iMouse _rp.mouse
#define iDate _rp.date
#define iTime _rp.time.x
#define iTimeDelta _rp.time.y
#define iFrame int(_rp.time.z)
#define iFrameRate _rp.time.w
#define iChannel0 0
#define texture(channel, uv) _rp_channel(uv)
#define textureLod(channel, uv, lod) _rp_channel(uv)
"#;

/// After the user's code: the entry point, mapping the target pixel to the
/// canvas and `mainImage`'s straight sRGB colour to premultiplied output.
const EPILOGUE: &str = r#"
void main() {
    vec2 px = gl_FragCoord.xy;
    vec2 c = vec2(dot(_rp.to_canvas_x.xyz, vec3(px, 1.0)), dot(_rp.to_canvas_y.xyz, vec3(px, 1.0)));
    vec2 size = _rp.resolution.xy;
    vec2 w = c;
    if (_rp.flags.x > 0.5) {
        w = c - size * floor(c / size);
    } else if (c.x < 0.0 || c.y < 0.0 || c.x >= size.x || c.y >= size.y) {
        _rp_out = vec4(0.0);
        return;
    }
    _rp_unwrap = c - w;
    // One colour per canvas pixel, as the baked layer has.
    w = floor(w) + 0.5;
    vec4 col = vec4(0.0, 0.0, 0.0, 1.0);
    mainImage(col, vec2(w.x, size.y - w.y));
    col = clamp(col, 0.0, 1.0);
    vec3 rgb = _rp.flags.z > 0.5 ? col.rgb : _rp_linear(col.rgb);
    _rp_out = vec4(rgb * col.a, col.a);
}
"#;

/// A compile problem, at a line of the user's code (1-based; 0 when it
/// isn't at one, like a missing `mainImage`).
#[derive(Clone, Debug, PartialEq)]
pub struct ShaderError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl std::fmt::Display for ShaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line > 0 {
            write!(f, "line {}:{}: {}", self.line, self.column, self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}

/// A compiled, validated shader ready to become a GPU pipeline.
pub struct ShaderProgram {
    /// Of the source it was compiled from.
    pub hash: u64,
    pub module: naga::Module,
}

impl std::fmt::Debug for ShaderProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShaderProgram")
            .field("hash", &self.hash)
            .finish_non_exhaustive()
    }
}

pub fn source_hash(source: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    h.finish()
}

/// The full GLSL for `source`, and how many lines come before the user's
/// code. `#version` lines are blanked (the prelude sets it), keeping the
/// line numbers.
fn wrap(source: &str) -> (String, usize) {
    let prelude_lines = PRELUDE.lines().count();
    let mut full = String::with_capacity(PRELUDE.len() + source.len() + EPILOGUE.len() + 1);
    full.push_str(PRELUDE);
    for line in source.lines() {
        if !line.trim_start().starts_with("#version") {
            full.push_str(line);
        }
        full.push('\n');
    }
    full.push_str(EPILOGUE);
    (full, prelude_lines)
}

/// Compile and validate `source` (the user's code).
pub fn compile(source: &str) -> Result<ShaderProgram, Vec<ShaderError>> {
    let (full, prelude_lines) = wrap(source);
    let user_lines = source.lines().count().max(1);
    let at = |span: naga::Span| -> (usize, usize) {
        if !span.is_defined() {
            return (0, 0);
        }
        let loc = span.location(&full);
        let line = loc.line_number as usize;
        if line > prelude_lines && line <= prelude_lines + user_lines {
            (line - prelude_lines, loc.line_position as usize)
        } else {
            (0, 0)
        }
    };
    let mut frontend = naga::front::glsl::Frontend::default();
    let options = naga::front::glsl::Options::from(naga::ShaderStage::Fragment);
    let module = frontend.parse(&options, &full).map_err(|e| {
        e.errors
            .into_iter()
            .map(|err| {
                let (line, column) = at(err.meta);
                ShaderError {
                    line,
                    column,
                    message: err.kind.to_string(),
                }
            })
            .collect::<Vec<_>>()
    })?;
    // Only the bindings the prelude declares exist on the GPU side.
    for (_, var) in module.global_variables.iter() {
        if let Some(binding) = &var.binding
            && (binding.group != 0 || binding.binding > 2)
        {
            return Err(vec![ShaderError {
                line: 0,
                column: 0,
                message: format!(
                    "Custom uniforms and textures aren't supported ({}): use iTime, \
                     iResolution, iMouse, iDate, iFrame and iChannel0",
                    var.name.as_deref().unwrap_or("unnamed")
                ),
            }]);
        }
    }
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .map_err(|err| {
        let (line, column) = err
            .spans()
            .map(|(span, _)| at(*span))
            .find(|&(line, _)| line > 0)
            .unwrap_or((0, 0));
        let mut message = err.as_inner().to_string();
        let mut source = std::error::Error::source(err.as_inner());
        while let Some(inner) = source {
            message.push_str(": ");
            message.push_str(&inner.to_string());
            source = inner.source();
        }
        vec![ShaderError {
            line,
            column,
            message,
        }]
    })?;
    Ok(ShaderProgram {
        hash: source_hash(source),
        module,
    })
}

/// One step of the on-screen composite when shader layers show live.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveStep {
    /// Run `run` of [`LiveLayout::runs`]: layers composited on the CPU,
    /// blended over what's below with `blend` (a run with a mode is that
    /// one entry, composited over transparency: exactly the pixels the
    /// mode then combines).
    Run { run: usize, blend: LayerBlend },
    /// A shader layer, blended over what's below with its mode and opacity.
    Shader {
        id: LayerId,
        blend: LayerBlend,
        opacity: f32,
    },
}

/// How the stack splits around the visible shader layers, bottom to top.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveLayout {
    /// Per run, which entries (by index) it shows; the rest are hidden in
    /// that run's composite.
    pub runs: Vec<Vec<bool>>,
    pub steps: Vec<LiveStep>,
}

/// Whether shader layers can show live on screen, and how.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveStatus {
    /// No shader layer shows.
    None,
    /// Shown as its last baked frame instead, for this reason.
    Unsupported(&'static str),
    Live(LiveLayout),
}

/// A visible shader layer: shows and has something to show.
fn shows_shader(canvas: &Canvas, i: usize) -> bool {
    let l = &canvas.layers[i];
    l.shader.is_some() && l.kind == LayerKind::Paint && l.visible && l.opacity > 0.0
}

/// Split the stack at the visible shader layers. The layers above one are
/// composited in runs on their own (over transparency) and blended over
/// it, which gives the same picture as long as nothing is clipped to the
/// shader layer and no adjustment layer is above it.
pub fn live_layout(canvas: &Canvas) -> LiveStatus {
    let layers = &canvas.layers;
    let shaders: Vec<usize> = (0..layers.len())
        .filter(|&i| shows_shader(canvas, i))
        .collect();
    let Some(&first) = shaders.first() else {
        return LiveStatus::None;
    };
    if shaders.iter().any(|&i| layers[i].parent.is_some()) {
        return LiveStatus::Unsupported("a shader layer is inside a folder");
    }
    let shader_ids: Vec<LayerId> = shaders.iter().map(|&i| layers[i].id).collect();
    if layers
        .iter()
        .any(|l| matches!(l.kind, LayerKind::Mask { owner } if shader_ids.contains(&owner)))
    {
        return LiveStatus::Unsupported("a shader layer has a mask");
    }
    if shaders.iter().any(|&i| layers[i].clipped) {
        return LiveStatus::Unsupported("a shader layer is clipped");
    }
    // A border is drawn around the layer's pixels, on the CPU.
    if shaders.iter().any(|&i| !layers[i].style.is_plain()) {
        return LiveStatus::Unsupported("a shader layer has a border");
    }
    let top_level: Vec<usize> = (0..layers.len())
        .filter(|&i| {
            layers[i].parent.is_none() && !matches!(layers[i].kind, LayerKind::Mask { .. })
        })
        .collect();
    // Above the lowest shader layer, each run is composited on its own
    // (over transparency) and then blended on: a run is one entry with a
    // blend mode (and what's clipped to it), or entries that just stack
    // (Normal, Dissolve). Adjustment layers change what's below them,
    // which the CPU no longer has there.
    // What an entry clipped here would clip to: starting just above the
    // lowest shader layer, that one.
    let mut base_is_shader = true;
    for &i in top_level.iter().filter(|&&i| i > first) {
        let l = &layers[i];
        if shows_shader(canvas, i) {
            base_is_shader = true;
            continue;
        }
        if l.clipped {
            if base_is_shader && l.visible {
                return LiveStatus::Unsupported("a layer is clipped to a shader layer");
            }
        } else {
            base_is_shader = false;
        }
        if l.visible && l.adjustment.is_some() {
            return LiveStatus::Unsupported("an adjustment layer is above a shader layer");
        }
    }
    let stacks = |blend: LayerBlend| matches!(blend, LayerBlend::Normal | LayerBlend::Dissolve);
    let tops: Vec<usize> = (0..layers.len()).map(|i| top_level_of(canvas, i)).collect();
    let mut runs: Vec<Vec<bool>> = Vec::new();
    let mut steps = Vec::new();
    // The run being gathered: its entries and how it goes on.
    let mut current: Vec<usize> = Vec::new();
    let mut current_blend = LayerBlend::Normal;
    let flush = |current: &mut Vec<usize>,
                 blend: LayerBlend,
                 runs: &mut Vec<Vec<bool>>,
                 steps: &mut Vec<LiveStep>| {
        if current.iter().any(|&t| layers[t].visible) {
            let shown = tops.iter().map(|t| current.contains(t)).collect();
            steps.push(LiveStep::Run {
                run: runs.len(),
                blend,
            });
            runs.push(shown);
        }
        current.clear();
    };
    for &i in &top_level {
        let l = &layers[i];
        if shows_shader(canvas, i) {
            flush(&mut current, current_blend, &mut runs, &mut steps);
            current_blend = LayerBlend::Normal;
            steps.push(LiveStep::Shader {
                id: l.id,
                blend: l.blend,
                opacity: l.opacity,
            });
            continue;
        }
        // Below the first shader layer everything is one run: the CPU
        // composites it as usual. Clipped entries go with their base.
        let own_run = i > first && !l.clipped && l.visible && !stacks(l.blend);
        let after_own_run = !stacks(current_blend) && !l.clipped;
        if own_run || after_own_run {
            flush(&mut current, current_blend, &mut runs, &mut steps);
            current_blend = if own_run { l.blend } else { LayerBlend::Normal };
        }
        current.push(i);
    }
    flush(&mut current, current_blend, &mut runs, &mut steps);
    LiveStatus::Live(LiveLayout { runs, steps })
}

/// The top-level entry entry `i` belongs to (itself when at the top
/// level; a mask belongs to its owner's).
fn top_level_of(canvas: &Canvas, mut i: usize) -> usize {
    for _ in 0..canvas.layers.len() {
        let parent = match canvas.layers[i].kind {
            LayerKind::Mask { owner } => canvas.layer_index_of(owner),
            _ => canvas.layers[i]
                .parent
                .and_then(|p| canvas.layer_index_of(p)),
        };
        match parent {
            Some(p) => i = p,
            None => break,
        }
    }
    i
}

/// A read-only copy of `canvas` sharing its tiles, showing only the entries
/// `shown` marks (the rest hidden): one run's composite.
pub fn run_view(canvas: &Canvas, shown: &[bool]) -> Canvas {
    canvas.view_showing(shown)
}

/// A copy of `canvas` showing only what's below entry `idx` (a top-level
/// shader layer): its `iChannel0`.
pub fn below_view(canvas: &Canvas, idx: usize) -> Canvas {
    let shown: Vec<bool> = (0..canvas.layers.len())
        .map(|i| top_level_of(canvas, i) < idx)
        .collect();
    canvas.view_showing(&shown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;

    #[test]
    fn every_template_compiles() {
        for (name, source) in TEMPLATES {
            if let Err(errors) = compile(source) {
                panic!("{name}: {errors:?}");
            }
        }
    }

    #[test]
    fn errors_point_at_the_users_line() {
        let source = "void mainImage(out vec4 fragColor, in vec2 fragCoord)\n{\n    fragColor = vec4(foo);\n}\n";
        let errors = compile(source).unwrap_err();
        assert_eq!(errors[0].line, 3, "{errors:?}");
        assert!(errors[0].message.contains("foo"), "{errors:?}");
    }

    #[test]
    fn a_missing_main_image_is_reported_without_a_line() {
        let errors = compile("float f(float x) { return x; }\n").unwrap_err();
        assert!(!errors.is_empty());
        assert_eq!(errors[0].line, 0);
    }

    #[test]
    fn version_lines_are_ignored_and_keep_numbering() {
        let source = "#version 300 es\nprecision highp float;\nvoid mainImage(out vec4 c, in vec2 p) { c = vec4(p, 0.0, 1.0) + bar; }\n";
        let errors = compile(source).unwrap_err();
        assert_eq!(errors[0].line, 3, "{errors:?}");
        let ok = "#version 300 es\nvoid mainImage(out vec4 c, in vec2 p) { c = vec4(1.0); }\n";
        assert!(compile(ok).is_ok());
    }

    #[test]
    fn custom_uniforms_are_refused() {
        let source =
            "uniform float speed;\nvoid mainImage(out vec4 c, in vec2 p) { c = vec4(speed); }\n";
        assert!(compile(source).is_err());
    }

    #[test]
    fn uniforms_are_nine_vec4s() {
        assert_eq!(FrameUniforms::default().as_bytes().len(), 9 * 16);
    }

    fn canvas_with(n: usize) -> Canvas {
        let mut canvas = Canvas::new(64, 64, Color32::WHITE, 64);
        while canvas.layers.len() < n {
            let i = canvas.layers.len();
            canvas.insert_new_layer(i, format!("L{i}"), LayerKind::Paint, None);
        }
        canvas.layers.truncate(n);
        canvas
    }

    #[test]
    fn no_shader_layer_means_no_live_layout() {
        assert_eq!(live_layout(&canvas_with(3)), LiveStatus::None);
    }

    #[test]
    fn the_stack_splits_around_shader_layers() {
        let mut canvas = canvas_with(4);
        canvas.layers[2].shader = Some(Box::new(ShaderLayer::new(PLASMA)));
        let LiveStatus::Live(layout) = live_layout(&canvas) else {
            panic!("expected a live layout");
        };
        assert_eq!(layout.runs.len(), 2);
        assert_eq!(layout.runs[0], vec![true, true, false, false]);
        assert_eq!(layout.runs[1], vec![false, false, false, true]);
        assert_eq!(layout.steps.len(), 3);
        let normal = LayerBlend::Normal;
        assert_eq!(
            layout.steps[0],
            LiveStep::Run {
                run: 0,
                blend: normal
            }
        );
        assert!(matches!(layout.steps[1], LiveStep::Shader { .. }));
        assert_eq!(
            layout.steps[2],
            LiveStep::Run {
                run: 1,
                blend: normal
            }
        );
    }

    #[test]
    fn a_blend_mode_above_a_shader_layer_gets_its_own_run() {
        let mut canvas = canvas_with(5);
        canvas.layers[1].shader = Some(Box::new(ShaderLayer::new(PLASMA)));
        canvas.layers[3].blend = LayerBlend::Multiply;
        canvas.layers[4].clipped = true;
        let LiveStatus::Live(layout) = live_layout(&canvas) else {
            panic!("expected a live layout");
        };
        let blends: Vec<LayerBlend> = layout
            .steps
            .iter()
            .filter_map(|s| match s {
                LiveStep::Run { blend, .. } => Some(*blend),
                LiveStep::Shader { .. } => None,
            })
            .collect();
        assert_eq!(
            blends,
            vec![LayerBlend::Normal, LayerBlend::Normal, LayerBlend::Multiply]
        );
        // The Multiply layer and the layer clipped to it, together.
        assert_eq!(layout.runs[2], vec![false, false, false, true, true]);
        // Below the shader layer any mode is fine: one run, as usual.
        let mut canvas = canvas_with(3);
        canvas.layers[1].blend = LayerBlend::Multiply;
        canvas.layers[2].shader = Some(Box::new(ShaderLayer::new(PLASMA)));
        let LiveStatus::Live(layout) = live_layout(&canvas) else {
            panic!("expected a live layout");
        };
        assert_eq!(layout.runs, vec![vec![true, true, false]]);
    }

    #[test]
    fn an_adjustment_clipping_or_border_is_not_live() {
        let mut canvas = canvas_with(3);
        canvas.layers[1].shader = Some(Box::new(ShaderLayer::new(PLASMA)));
        canvas.layers[2].adjustment = Some(crate::canvas::filters::Filter::Invert);
        assert!(matches!(live_layout(&canvas), LiveStatus::Unsupported(_)));
        let mut canvas = canvas_with(3);
        canvas.layers[1].shader = Some(Box::new(ShaderLayer::new(PLASMA)));
        canvas.layers[2].clipped = true;
        assert!(matches!(live_layout(&canvas), LiveStatus::Unsupported(_)));
        let mut canvas = canvas_with(3);
        canvas.layers[1].shader = Some(Box::new(ShaderLayer::new(PLASMA)));
        canvas.layers[1].style.border = Some(Default::default());
        assert!(matches!(live_layout(&canvas), LiveStatus::Unsupported(_)));
    }

    #[test]
    fn a_hidden_shader_layer_shows_nothing() {
        let mut canvas = canvas_with(3);
        canvas.layers[1].shader = Some(Box::new(ShaderLayer::new(PLASMA)));
        canvas.layers[1].visible = false;
        assert_eq!(live_layout(&canvas), LiveStatus::None);
        // With a visible one above, the hidden one splits nothing off: it
        // stays in the run below, hidden there too.
        canvas.layers[2].shader = Some(Box::new(ShaderLayer::new(PLASMA)));
        let LiveStatus::Live(layout) = live_layout(&canvas) else {
            panic!("expected a live layout");
        };
        assert_eq!(layout.runs, vec![vec![true, true, false]]);
    }

    #[test]
    fn run_views_hide_everything_else() {
        let canvas = canvas_with(3);
        let view = run_view(&canvas, &[false, true, true]);
        assert!(!view.layers[0].visible && view.layers[1].visible && view.layers[2].visible);
        let below = below_view(&canvas, 2);
        assert!(below.layers[0].visible && below.layers[1].visible && !below.layers[2].visible);
    }
}
