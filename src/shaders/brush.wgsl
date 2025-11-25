// Circle brush shader: expands a 2x2 strip into a clip-space quad and discards pixels outside the radius.
struct Brush {
    color: vec4<f32>,
    position: vec2<f32>,
    radius: f32,
};

@group(0) @binding(0)
var<uniform> u_brush: Brush;

@group(1) @binding(0)
var<uniform> view: mat4x4<f32>;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) in_vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(in_vertex_index % 2u) * 2.0 - 1.0;
    let y = f32(in_vertex_index / 2u) * 2.0 - 1.0;
    let screen_pos = vec2<f32>(x, y) * u_brush.radius;
    out.world_position = u_brush.position + screen_pos;
    let clip_pos = vec2<f32>(
        out.world_position.x * 2.0 - 1.0,
        1.0 - out.world_position.y * 2.0,
    );
    out.clip_position = vec4<f32>(clip_pos, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let dist = distance(in.world_position, u_brush.position);
    if (dist > u_brush.radius) {
        discard;
    }
    return u_brush.color;
}
