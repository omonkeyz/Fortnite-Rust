struct SssU {
    size: vec4<f32>,
    p0: vec4<f32>,
    p1: vec4<f32>,
    p2: vec4<u32>,
}
@group(0) @binding(0) var<uniform> su: SssU;
@group(0) @binding(1) var<storage, read> profiles: array<vec4<f32>>;
@group(0) @binding(2) var tex_lit: texture_2d<f32>;
@group(0) @binding(3) var tex_sky: texture_2d<f32>;
@group(0) @binding(4) var tex_base: texture_2d<f32>;
@group(0) @binding(5) var tex_fog: texture_2d<f32>;
@group(0) @binding(6) var tex_depth: texture_depth_2d;
@group(0) @binding(7) var tex_ao: texture_2d<f32>;
@group(0) @binding(8) var refl_sky: texture_2d<f32>;
@group(0) @binding(9) var refl_brdf: texture_2d<f32>;
@group(0) @binding(10) var ssr_tex: texture_2d<f32>;
@group(0) @binding(11) var tex_scene: texture_2d<f32>;
@group(0) @binding(12) var tex_half: texture_2d<f32>;

const PROFILE_STRIDE: u32 = 32u;

struct FsIn { @builtin(position) pos: vec4<f32> }
@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> FsIn {
    var o: FsIn;
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    o.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    return o;
}

fn full_size() -> vec2<i32> { return vec2<i32>(su.size.xy); }
fn half_size() -> vec2<i32> { return vec2<i32>(su.size.zw); }
fn luma_ue(c: vec3<f32>) -> f32 { return dot(c, vec3<f32>(0.3, 0.59, 0.11)); }
fn clamp_full(px: vec2<i32>) -> vec2<i32> { return clamp(px, vec2<i32>(0), full_size() - vec2<i32>(1)); }
fn depth_cm(px: vec2<i32>) -> f32 {
    let d = textureLoad(tex_depth, clamp_full(px), 0);
    let n = su.p1.x;
    let f = su.p1.y;
    return n * f / (f - d * (f - n)) * 100.0;
}
fn is_profile(px: vec2<i32>) -> bool { return textureLoad(tex_base, clamp_full(px), 0).a >= 1.0; }
fn profile_int(px: vec2<i32>) -> u32 {
    let b = textureLoad(tex_base, clamp_full(px), 0).a;
    if (b < 1.0) { return 0u; }
    return u32(floor(b)) - 1u;
}
fn opacity_of(px: vec2<i32>) -> f32 {
    let b = textureLoad(tex_base, clamp_full(px), 0).a;
    if (b < 1.0) { return 0.0; }
    return clamp(b - floor(b), 0.0, 1.0);
}
fn kernel(i: u32, pid: u32) -> vec4<f32> { return profiles[pid * PROFILE_STRIDE + i]; }

fn reflections(px: vec2<i32>, screen_ao: f32) -> vec3<f32> {
    let r = textureLoad(refl_sky, px, 0);
    let b = textureLoad(refl_brdf, px, 0);
    let ao = b.a * screen_ao;
    let so = clamp((r.a + ao) * (r.a + ao) - 1.0 + ao, 0.0, 1.0);
    var ssr = vec4<f32>(0.0);
    if (su.p1.z > 0.5) { ssr = textureLoad(ssr_tex, px, 0); }
    return so * (ssr.rgb * b.rgb + (1.0 - ssr.a) * r.rgb);
}

@fragment
fn fs_compose(in: FsIn) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.pos.xy);
    if (!is_profile(px)) { return vec4<f32>(0.0); }
    let a = textureLoad(tex_lit, px, 0);
    let s = textureLoad(tex_sky, px, 0);
    let f = textureLoad(tex_fog, px, 0);
    var screen_ao = 1.0;
    var k = 1.0;
    if (su.p1.w > 0.5) {
        screen_ao = textureLoad(tex_ao, px, 0).r;
        k = select(screen_ao, min(s.a, screen_ao) / s.a, s.a > 1e-4);
    }
    let r = reflections(px, screen_ao) / max(f.a, 1e-6);
    return vec4<f32>(a.rgb + s.rgb * k + r, a.a + luma_ue(s.rgb) * k);
}

struct DiffSpec { diffuse: vec3<f32>, specular: vec3<f32> }
fn reconstruct(px: vec2<i32>) -> DiffSpec {
    let c = textureLoad(tex_scene, clamp_full(px), 0);
    let l = luma_ue(c.rgb);
    var factor = 0.0;
    if (l > 0.0) { factor = clamp(c.a / l, 0.0, 1.0); } else if (c.a > 0.0) { factor = 1.0; }
    var o: DiffSpec;
    o.diffuse = c.rgb * factor;
    o.specular = c.rgb * (1.0 - factor);
    return o;
}
fn setup_one(px: vec2<i32>) -> vec4<f32> {
    if (!is_profile(px)) { return vec4<f32>(0.0); }
    return vec4<f32>(reconstruct(px).diffuse, 1.0);
}

@fragment
fn fs_setup(in: FsIn) -> @location(0) vec4<f32> {
    let hp = vec2<i32>(in.pos.xy);
    if (su.p0.w > 0.5) {
        let b = hp * 2;
        let ta = setup_one(b + vec2<i32>(0, 1));
        let tb = setup_one(b + vec2<i32>(1, 1));
        let tc = setup_one(b + vec2<i32>(1, 0));
        let td = setup_one(b);
        let sum = (ta + tb) + (tc + td);
        let div = 1.0 / max(sum.a, 0.00001);
        let depths = vec4<f32>(depth_cm(b + vec2<i32>(0, 1)), depth_cm(b + vec2<i32>(1, 1)), depth_cm(b + vec2<i32>(1, 0)), depth_cm(b));
        return vec4<f32>(sum.rgb * div, dot(depths, vec4<f32>(ta.a, tb.a, tc.a, td.a)) * div);
    }
    return vec4<f32>(setup_one(hp).rgb, depth_cm(hp));
}

fn half_load(t: vec2<i32>) -> vec4<f32> {
    let sz = half_size();
    if (any(t < vec2<i32>(0)) || any(t >= sz)) { return vec4<f32>(0.0); }
    return textureLoad(tex_half, t, 0);
}
fn half_bilinear(uv: vec2<f32>) -> vec4<f32> {
    let p = uv * su.size.zw - 0.5;
    let f = floor(p);
    let w = p - f;
    let t = vec2<i32>(f);
    let a = half_load(t);
    let b = half_load(t + vec2<i32>(1, 0));
    let c = half_load(t + vec2<i32>(0, 1));
    let d = half_load(t + vec2<i32>(1, 1));
    return mix(mix(a, b, w.x), mix(c, d, w.x), w.y);
}

fn blur(in: FsIn, dir: vec2<f32>) -> vec4<f32> {
    let hp = vec2<i32>(in.pos.xy);
    let vp_uv = in.pos.xy / su.size.zw;
    var color_m = half_load(hp);
    let out_depth = color_m.a;
    if (!(out_depth > 0.0)) { return vec4<f32>(0.0); }
    let gpx = vec2<i32>(vp_uv * su.size.xy);
    let strength = select(0.0, opacity_of(gpx), is_profile(gpx));
    let pid = profile_int(gpx);
    let scale = su.p0.x / out_depth;
    let final_step = scale * dir * strength;
    let off = su.p2.x;
    let k0 = kernel(off, pid);
    var inv_div = vec3<f32>(0.00001) + k0.rgb;
    var accum = color_m.rgb * k0.rgb;
    for (var i = 1u; i < su.p2.y; i = i + 1u) {
        let k = kernel(off + i, pid);
        var local = vec4<f32>(0.0);
        let uvo = k.a * final_step;
        for (var side = -1; side <= 1; side = side + 2) {
            var c = half_bilinear(vp_uv + uvo * f32(side));
            let local_depth = c.a;
            c.a = select(0.0, 1.0, c.a > 0.0);
            let sv = clamp(12000.0 / 400000.0 * su.p0.y * abs(out_depth - local_depth), 0.0, 1.0);
            c.a = c.a * (1.0 - sv);
            c = vec4<f32>(c.rgb * c.a, c.a);
            local = local + c;
        }
        accum = accum + k.rgb * local.rgb;
        inv_div = inv_div + k.rgb * local.a;
    }
    return vec4<f32>(accum / inv_div, out_depth);
}

@fragment
fn fs_blur_x(in: FsIn) -> @location(0) vec4<f32> { return blur(in, vec2<f32>(1024.0, 0.0)); }

@fragment
fn fs_blur_y(in: FsIn) -> @location(0) vec4<f32> {
    let o = blur(in, vec2<f32>(0.0, 1024.0 * su.p0.z));
    return vec4<f32>(o.rgb, select(0.0, 1.0, o.a > 0.0));
}

@fragment
fn fs_recombine(in: FsIn) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.pos.xy);
    if (!is_profile(px)) { discard; }
    let uv = in.pos.xy / su.size.xy;
    let sss_a = half_bilinear(uv);
    let sss_color = sss_a.rgb / max(sss_a.a, 0.00001);
    let pid = profile_int(px);
    var lerp_factor = 1.0;
    if (su.p0.w > 0.5) {
        let final_step = su.p0.x / depth_cm(px) * 1024.0 * kernel(13u, pid).a;
        let pixel_size_radius = final_step / ((1.0 / su.size.z) * 0.5);
        lerp_factor = clamp(pixel_size_radius - 4.0, 0.0, 1.0) * clamp(opacity_of(px) * 10.0, 0.0, 1.0);
    }
    if (su.p2.z == 0u) { lerp_factor = 0.0; }
    let base = textureLoad(tex_base, px, 0).rgb;
    let ds = reconstruct(px);
    let faded = kernel(0u, pid).rgb * lerp_factor;
    let lighting = mix(ds.diffuse, sss_color, faded);
    let f = textureLoad(tex_fog, px, 0);
    if (su.p2.w == 1u) { return vec4<f32>((ds.diffuse * base + ds.specular) * f.a + f.rgb, 0.0); }
    if (su.p2.w == 2u) { return vec4<f32>(ds.diffuse, 0.0); }
    if (su.p2.w == 3u) { return vec4<f32>(ds.specular, 0.0); }
    if (su.p2.w == 4u) { return vec4<f32>(textureLoad(tex_scene, px, 0).rgb, 0.0); }
    if (su.p2.w == 5u) { return vec4<f32>(vec3<f32>(textureLoad(tex_scene, px, 0).a), 0.0); }
    if (su.p2.w == 6u) { return vec4<f32>(sss_color, 0.0); }
    if (su.p2.w == 7u) { return vec4<f32>(faded, 0.0); }
    return vec4<f32>((lighting * base + ds.specular) * f.a + f.rgb, 0.0);
}
