// Pixel operations on egui's premultiplied Color32 words. Each kind of pass
// is its own entry point, so each compiles only the code (and registers) it
// needs; all share one binding layout.
struct Params { size: vec2<u32>, method: u32, radius: u32, a: vec4<f32>, b: vec4<f32>, c: vec4<f32> }
@group(0) @binding(0) var<storage, read> source: array<u32>;
@group(0) @binding(1) var<storage, read_write> destination: array<u32>;
@group(0) @binding(2) var<storage, read> codecs: array<u32>;
@group(0) @binding(3) var<storage, read> lookup: array<u32>;
@group(0) @binding(4) var<uniform> p: Params;
@group(0) @binding(5) var<storage, read> original: array<u32>;

// Pixels each column-blur thread runs its window sum down.
const SEGMENT: u32 = 64u;

fn unpack(v: u32) -> vec4<u32> { return vec4<u32>(v & 255u, (v >> 8u) & 255u, (v >> 16u) & 255u, v >> 24u); }
fn pack(v: vec4<u32>) -> u32 { return v.x | (v.y << 8u) | (v.z << 16u) | (v.w << 24u); }
fn straight(v: vec4<u32>) -> vec3<f32> {
    let at = v.w * 256u;
    return vec3<f32>(f32(codecs[at+v.x]),f32(codecs[at+v.y]),f32(codecs[at+v.z])) / 255.0;
}
fn encode(v: vec3<f32>, alpha: u32) -> u32 {
    let rgb = vec3<u32>(floor(clamp(v,vec3<f32>(0.0),vec3<f32>(1.0))*255.0+0.5));
    let at = 65536u + alpha*256u;
    return pack(vec4<u32>(codecs[at+rgb.x],codecs[at+rgb.y],codecs[at+rgb.z],alpha));
}
fn hsl(v: vec3<f32>) -> vec3<f32> {
    let hi = max(v.x,max(v.y,v.z)); let lo = min(v.x,min(v.y,v.z));
    let l = (hi+lo)*0.5; let d = hi-lo;
    if d < 0.000001 { return vec3<f32>(0.0,0.0,l); }
    let s = min(1.0,d/max(0.000001,1.0-abs(2.0*l-1.0)));
    var h = (v.x-v.y)/d+4.0;
    if hi == v.x { h = (v.y-v.z)/d; h = h-floor(h/6.0)*6.0; }
    else if hi == v.y { h = (v.z-v.x)/d+2.0; }
    return vec3<f32>(h/6.0,s,l);
}
fn rgb(v: vec3<f32>) -> vec3<f32> {
    let c = (1.0-abs(2.0*v.z-1.0))*v.y;
    let h = v.x*6.0; let x = c*(1.0-abs((h-floor(h/2.0)*2.0)-1.0));
    var r = vec3<f32>(c,0.0,x);
    switch u32(h) { case 0u: { r=vec3<f32>(c,x,0.0); } case 1u: { r=vec3<f32>(x,c,0.0); }
    case 2u: { r=vec3<f32>(0.0,c,x); } case 3u: { r=vec3<f32>(0.0,x,c); } case 4u: { r=vec3<f32>(x,0.0,c); } default: {} }
    return r+vec3<f32>(v.z-c*0.5);
}

/// Per-pixel colour filters (`method` 0..=4).
@compute @workgroup_size(8,8)
fn point(@builtin(global_invocation_id) at: vec3<u32>) {
    if any(at.xy >= p.size) { return; }
    let i = at.y*p.size.x+at.x;
    let v = unpack(source[i]);
    if v.w == 0u { destination[i]=source[i]; return; }
    var color = straight(v);
    if p.method == 0u {
        let q=vec3<u32>(floor(color*255.0+0.5));
        color=vec3<f32>(f32(lookup[q.x]),f32(lookup[256u+q.y]),f32(lookup[512u+q.z]))/255.0;
    } else if p.method == 1u {
        var h=hsl(color); h.x=fract(h.x+p.a.x); h.y=clamp(h.y*(1.0+p.a.y),0.0,1.0);
        h.z=select(h.z*(1.0+p.a.z),h.z+(1.0-h.z)*p.a.z,p.a.z>=0.0); color=rgb(h);
    } else if p.method == 2u { color=vec3<f32>(dot(color,vec3<f32>(0.299,0.587,0.114))); }
    else if p.method == 3u {
        let l=hsl(color).z;
        let ws=clamp((l-0.333)/-0.25+0.5,0.0,1.0)*0.7;
        let wm=clamp((l-0.333)/0.25+0.5,0.0,1.0)*clamp((l+0.333-1.0)/-0.25+0.5,0.0,1.0)*0.7;
        let wh=clamp((l+0.333-1.0)/0.25+0.5,0.0,1.0)*0.7;
        color=clamp(color+p.a.xyz*ws+p.b.xyz*wm+p.c.xyz*wh,vec3<f32>(0.0),vec3<f32>(1.0));
        if p.a.w>0.5 { let h=hsl(color); color=rgb(vec3<f32>(h.xy,l)); }
    } else if p.method == 4u {
        let t=u32(floor(clamp(dot(color,vec3<f32>(0.299,0.587,0.114)),0.0,1.0)*65535.0+0.5)); color=vec3<f32>(unpack(lookup[t]).xyz)/255.0;
    }
    destination[i]=encode(color,v.w);
}

/// Blur input: unmultiplied, then multiplied back with integer rounding (the
/// CPU blur's working values).
@compute @workgroup_size(8,8)
fn premultiply(@builtin(global_invocation_id) at: vec3<u32>) {
    if any(at.xy >= p.size) { return; }
    let i = at.y*p.size.x+at.x;
    let v = unpack(source[i]);
    let s = vec3<u32>(floor(straight(v)*255.0+0.5));
    destination[i] = pack(vec4<u32>((s*v.w+vec3<u32>(127u))/255u,v.w));
}

/// One box pass along rows, edges clamped. A workgroup reads its row
/// segment and the radius either side into workgroup memory once (reads
/// side by side), then each pixel sums its window from there.
var<workgroup> row_tile: array<vec4<u32>, ROW_TILE>;
const ROW_TILE: u32 = 1024u;
const ROW_GROUP: u32 = 256u;
@compute @workgroup_size(256)
fn box_rows(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) local: u32) {
    let y = group.y;
    let x0 = group.x * ROW_GROUP;
    let r = p.radius;
    let last = i32(p.size.x) - 1;
    for (var k = local; k < ROW_GROUP + 2u*r; k += ROW_GROUP) {
        let x = clamp(i32(x0 + k) - i32(r), 0, last);
        row_tile[k] = unpack(source[y*p.size.x + u32(x)]);
    }
    workgroupBarrier();
    let x = x0 + local;
    if x >= p.size.x { return; }
    var sum = vec4<u32>(0u);
    for (var k = 0u; k <= 2u*r; k += 1u) { sum += row_tile[local + k]; }
    let n = 2u*r+1u;
    destination[y*p.size.x+x] = pack((sum+vec4<u32>(n/2u))/n);
}

/// One box pass along columns, edges clamped. Each thread keeps a running
/// window sum down `SEGMENT` pixels of its column, so the cost per pixel
/// doesn't grow with the radius; neighbouring threads take neighbouring
/// columns, so they read memory side by side.
@compute @workgroup_size(64)
fn box_columns(@builtin(global_invocation_id) at: vec3<u32>) {
    let x = at.x;
    let start = at.y * SEGMENT;
    if x >= p.size.x || start >= p.size.y { return; }
    let r = i32(p.radius);
    let last = i32(p.size.y) - 1;
    var sum = vec4<u32>(0u);
    for (var d = -r; d <= r; d += 1) {
        sum += unpack(source[u32(clamp(i32(start) + d, 0, last))*p.size.x + x]);
    }
    let n = 2u*p.radius+1u;
    let end = min(start + SEGMENT, p.size.y);
    for (var y = start; y < end; y += 1u) {
        destination[y*p.size.x + x] = pack((sum+vec4<u32>(n/2u))/n);
        let enter = u32(clamp(i32(y) + r + 1, 0, last));
        let leave = u32(clamp(i32(y) - r, 0, last));
        sum += unpack(source[enter*p.size.x + x]);
        sum -= unpack(source[leave*p.size.x + x]);
    }
}

/// After the blur passes: Gaussian blur (`method` 0) back to Color32, or
/// sharpen (`method` 1) against `original`, in the same premultiplied gamma
/// space as the CPU.
@compute @workgroup_size(8,8)
fn finish(@builtin(global_invocation_id) at: vec3<u32>) {
    if any(at.xy >= p.size) { return; }
    let i = at.y*p.size.x+at.x;
    let v = unpack(source[i]);
    let old = unpack(original[i]);
    let s = vec3<u32>(floor(straight(old)*255.0+0.5));
    let mixed = (s*old.w+vec3<u32>(127u))/255u;
    if p.method == 0u {
        if all(vec4<u32>(mixed,old.w) == v) { destination[i]=original[i]; return; }
        if v.w == 0u { destination[i]=0u; return; }
        let u = min(vec3<u32>(255u),(v.xyz*255u+vec3<u32>(v.w/2u))/v.w);
        destination[i]=encode(vec3<f32>(u)/255.0,v.w); return;
    }
    let sharp = vec3<u32>(floor(clamp(vec3<f32>(mixed)+p.a.x*(vec3<f32>(mixed)-vec3<f32>(v.xyz)),vec3<f32>(0.0),vec3<f32>(f32(old.w)))+0.5));
    if all(sharp == mixed) { destination[i]=original[i]; return; }
    if old.w == 0u { destination[i]=0u; return; }
    let u = min(vec3<u32>(255u),(sharp*255u+vec3<u32>(old.w/2u))/old.w);
    destination[i]=encode(vec3<f32>(u)/255.0,old.w);
}
