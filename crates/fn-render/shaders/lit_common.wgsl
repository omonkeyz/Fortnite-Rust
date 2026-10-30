// Lighting shared by the scene shaders (scene.wgsl, static_scene.wgsl): camera/light blocks + group 0 bindings, cascade shadows,
// sky ambient, tangent-frame normal mapping, GGX and the fog/HDR tail. Prepended after common.wgsl by the Rust side.

struct Camera {
    view_proj: mat4x4<f32>,
    view: mat4x4<f32>,
    view_inverse: mat4x4<f32>,
    position: vec4<f32>,   // xyz eye, w time
    viewport: vec4<f32>,   // w, h, 1/w, 1/h
    proj: vec4<f32>,       // tan half fov x, tan half fov y, near, far
}
struct Light {
    sun_dir: vec4<f32>,        // unit vector towards the sun (world)
    sun_radiance: vec4<f32>,
    sky_radiance: vec4<f32>,
    shadow_matrix: array<mat4x4<f32>, 3>,   // world -> shadow uv (xy 0..1, y down) + depth 0..1
    cascade_far: vec4<f32>,    // xyz: view depth (m) where each cascade ends (nominal SplitFar), w: whole-scene fade FarDistance (CSM max or DF distance)
    cascade_texel: vec4<f32>,  // xyz: world metres per shadow texel, w: ShadowSharpen * 7 + 1
    shadow_params: vec4<f32>,  // x enabled, y shadow-mapped cascade count, z r.ShadowQuality (PCF kernel), w unused
    misc: vec4<f32>,           // x lit enabled, y material_debug bits, z shadow map size, w fog enabled
    cascade_ext: vec4<f32>,    // xyz: SplitFarFadeRegion (m) of each cascade (blend into the next), w: ShadowDistanceFadeoutFraction
    cascade_bias: vec4<f32>,   // xyz: ShaderDepthBias == ComputeTransitionSize (normalised depth units) of each cascade
    light_fn: vec4<f32>,       // directional light function (FLightFunctionPS): x volumetric-fog grey, y DisabledBrightness, z LightFunctionFadeDistance (m), w View.GameTime
    sky_sh: array<vec4<f32>, 7>,   // SKY LIGHT: View.SkyIrradianceEnvironmentMap x SkyLightColor (sky_light.rs), see sky_lookup
    fog: HeightFogU,           // TOD LIVE: this frame's height-fog view uniforms (common.wgsl, src/fog.rs HeightFogUniform)
    light_fn_world: vec4<f32>, // LIGHT FUNCTION: xyz renderer -> absolute UE offset (cm, light_function.rs), w 1 = light function on
}
@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var<uniform> light: Light;
@group(0) @binding(2) var shadow_map: texture_depth_2d_array;
@group(0) @binding(3) var shadow_samp: sampler_comparison;
// PERF (2026-10-06): point (non-filtering, clamp) sampler for the textureGather of the shadow depths (csm_gather)
@group(0) @binding(15) var shadow_point_samp: sampler;
@group(0) @binding(4) var tex_samp: sampler;

// ---- DISTANCE FIELD HOOK: ray-traced distance-field cascade + distance-field sky occlusion (distance_field.rs / distance_field_ao.rs) ----
struct DfU {
    sh0: vec4<f32>,   // x enabled, y MinDepth (cm), z MaxDepth (cm), w NearFadePlaneOffset (cm)
    sh1: vec4<f32>,   // x InvNearFadePlaneLength, y TanLightAngle, z TraceDistance (cm), w RayStartOffsetDepthScale
    sh2: vec4<f32>,   // x TwoSidedMeshDistanceBias (cm), y/z light tiles, w ShadowMaxObjectsPerTile
    sh3: vec4<f32>,   // xyz towards the light (DF space), w whole-scene shadow fade distance (m)
    world_to_shadow: mat4x4<f32>,
    ao0: vec4<f32>,   // x AO valid, y OcclusionExponent, z MinOcclusion, w OcclusionCombineMode
    ao1: vec4<f32>,   // xyz OcclusionTint (linear)
    ao2: vec4<f32>,   // ContrastAndNormalizeMulAdd
    ao3: vec4<f32>,   // AO buffer size, inverse size
    prev_view_proj: mat4x4<f32>,
    prev_eye: vec4<f32>,
    prev_fwd: vec4<f32>,
    sh4: vec4<f32>,   // DF SHADOW SCREEN PASS: xy RayTracedShadows size = floor(BufferSize / DownsampleFactor), z DownsampleFactor, w 1 = bilateral upsample (r.DFFullResolution 0)
}
@group(0) @binding(5) var<uniform> dfu: DfU;
@group(0) @binding(10) var df_ao: texture_2d<f32>;
// DF SHADOW SCREEN PASS (2026-10-06): the half-resolution ray-traced shadow factors of this frame (distance_field_shadow.wgsl, PF_G16R16F `RayTracedShadows`:
// x = the trace's Result, y = SceneDepth in cm). Replaces the trace inputs this block used to bind (objects 6, light tiles 7, atlas 8, its sampler 9: now the pass's own).
@group(0) @binding(16) var df_shadow_tex: texture_2d<f32>;

// renderer world (m, Y up) -> DF space (cm, Z up, UE axes)
fn df_space(p: vec3<f32>) -> vec3<f32> { return vec3<f32>(p.x, p.z, p.y) * 100.0; }
fn df_line_box(o: vec3<f32>, e: vec3<f32>, bmin: vec3<f32>, bmax: vec3<f32>) -> vec2<f32> {
    // LineBoxIntersect (Common.usf:1602)
    let inv = 1.0 / (e - o);
    let a = (bmin - o) * inv;
    let b = (bmax - o) * inv;
    let c = min(a, b);
    let f = max(a, b);
    return clamp(vec2<f32>(max(c.x, max(c.y, c.z)), min(f.x, min(f.y, f.z))), vec2<f32>(0.0), vec2<f32>(1.0));
}
// The pixel position of the fragment when the caller knows it (fx_resolve.wgsl fx_scene_world sets it from SV_Position); < 0 = project `world_pos` instead.
var<private> g_frag_xy: vec2<f32> = vec2<f32>(-1.0, -1.0);
// DF SHADOW SCREEN PASS (2026-10-06): DistanceFieldShadowingUpsamplePS (UE4.16 DistanceFieldShadowing.usf:616-667) at this pixel: the RayTracedShadows texels
// around it weighted bilinearly and by 1 / (|CornerDepth - SceneDepth| + 1e-4) (BILATERAL_UPSAMPLE && UPSAMPLE_REQUIRED), then FarBlendFactor (1: the ray-traced
// cascade is the last one, FadePlaneLength 0 -> InvFadePlaneLength 0) and NearBlendFactor = saturate((SceneDepth - (SplitNear - SplitNearFadeRegion)) /
// SplitNearFadeRegion). `scene_depth_cm` = this pixel's view depth (UE: CalcSceneDepth of the same pixel). UV = the full-resolution pixel centre UV (DrawRectangle
// maps the PS UV to it, odd sizes included); LowResBufferSize = floor(BufferSize / DOWNSAMPLE_FACTOR); the texel samples are at texel centres, so the bilinear
// sampler (AM_Clamp) returns the texels, read here with each corner clamped on its own. UE's quarter-texel offset (texel i was traced at full-res pixel 2i, the
// upsample takes it as centred at 2i + 1) is kept.
// All four corner weights 0 (fp16 SceneDepth overflowed: sky / far texels) gives 0 / 0 in UE; the NaN goes through EncodeLightAttenuation into the PF_B8G8R8A8
// light attenuation with BO_Min, where the float -> UNORM conversion turns it into 0 (D3D11): fully shadowed. Returned here as 0 for the same result.
fn df_shadow_upsample(world_pos: vec3<f32>, scene_depth_cm: f32) -> f32 {
    var uv: vec2<f32>;
    if (g_frag_xy.x >= 0.0) { uv = g_frag_xy * camera.viewport.zw; }
    else { let c = camera.view_proj * vec4<f32>(world_pos, 1.0); uv = vec2<f32>(c.x / c.w * 0.5 + 0.5, 0.5 - c.y / c.w * 0.5); }
    let low = dfu.sh4.xy;
    let lmax = vec2<i32>(low) - vec2<i32>(1);
    var out: f32;
    if (dfu.sh4.w > 0.5) {
        let t0 = floor(uv * low - 0.5);                            // the texel Corner00UV samples
        let corner00 = t0 / low + 0.5 * (1.0 / low);               // Corner00UV = floor(UV * LowResBufferSize - .5) / LowResBufferSize + .5 * LowResTexelSize
        let bw = (uv - corner00) * low;                            // BilinearWeights
        // PERF (2026-10-06): the four corner samples as two gathers of the 2x2 texels t0 .. t0 + 1 (x = Result, y = SceneDepth): exact texel values, each texel
        // clamped to the edge like UE's AM_Clamp sample (was four loads); gather order (0, 1), (1, 1), (1, 0), (0, 0) relative to t0 (csm_gather)
        let gc = (t0 + 1.0) / low;
        let gr = textureGather(0, df_shadow_tex, shadow_point_samp, gc);
        let gd = textureGather(1, df_shadow_tex, shadow_point_samp, gc);
        let values = vec4<f32>(gr.w, gr.z, gr.x, gr.y);           // TextureValues00 / 10 / 01 / 11 .x
        let corner_depths = abs(vec4<f32>(gd.w, gd.z, gd.x, gd.y));
        let cw = vec4<f32>((1.0 - bw.y) * (1.0 - bw.x), (1.0 - bw.y) * bw.x, bw.y * (1.0 - bw.x), bw.y * bw.x);
        let dw = 1.0 / (abs(corner_depths - vec4<f32>(scene_depth_cm)) + 0.0001);
        let fw = cw * dw;
        let sum = dot(fw, vec4<f32>(1.0));
        out = select(0.0, (fw.x * values.x + fw.y * values.y + fw.z * values.z + fw.w * values.w) / sum, sum > 0.0);
    } else {
        // r.DFFullResolution 1 (debug only, FN_DEBUG_DF_FULLRES): TDistanceFieldShadowingUpsamplePS<false>, the texel under the pixel
        out = textureLoad(df_shadow_tex, clamp(vec2<i32>(floor(uv * low)), vec2<i32>(0), lmax), 0).x;
    }
    let near_blend = clamp((scene_depth_cm - dfu.sh0.w) * dfu.sh1.x, 0.0, 1.0);
    return mix(1.0, out, near_blend);
}
var<private> g_df_vis: f32 = 1.0;   // the ray-traced cascade's value this fragment (debug view, material_debug bit 32)
var<private> g_sky_vis: f32 = 1.0;  // DFAO sky visibility (debug view, material_debug bit 64)
var<private> g_amb_n: vec3<f32> = vec3<f32>(0.0, 1.0, 0.0);
var<private> g_amb_set: bool = false;

// ---- screen-space derivatives with OpenGL orientation (window y up) so the ported tangent-frame maths keeps its sign ----
fn gl_dfdy3(v: vec3<f32>) -> vec3<f32> { return -dpdy(v); }
fn gl_dfdy2(v: vec2<f32>) -> vec2<f32> { return -dpdy(v); }
fn gl_dfdy1(v: f32) -> f32 { return -dpdy(v); }

fn dbg(bit: u32) -> bool { return ((u32(light.misc.y) / bit) % 2u) >= 1u; }

// ---- cascaded shadow maps: UE4.16 whole-scene directional shadow (ShadowProjectionPixelShader.usf + ShadowFilteringCommon.usf + LightRendering) ----
// Stored depth d (shadow.wgsl / static_shadow.wgsl) = normalised light-space depth + ShaderDepthBias B; the receiver depth s is the plain normalised depth.
// CalculateOcclusion: occ = saturate((d - s) * TransitionScale + 1) with TransitionScale = 1 / ComputeTransitionSize(), and TransitionSize == B.
fn csm_tap(layer: i32, t: vec2<i32>, s: f32, k: f32, size: i32) -> f32 {
    // outside the map = the cleared border (depth 1.0) = lit; occ(1.0) = 1 because s <= 0.99999
    if (t.x < 0 || t.y < 0 || t.x >= size || t.y >= size) { return 1.0; }
    let d = textureLoad(shadow_map, t, layer, 0);
    return clamp((d - s) * k + 1.0, 0.0, 1.0);
}
// ManualPCF (ShadowFilteringCommon.usf:265) with FEATURE_GATHER4, kernel picked by SHADOW_QUALITY = r.ShadowQuality (light.shadow_params.z; whole-scene
// directional shadows keep it unchanged, ShadowRendering.cpp:882-912). With tc = floor(uv * size - 0.5) and f = fract of it:
//   1 ManualNoFiltering: one point tap of texel floor(uv * size);
//   2 Manual1x1PCF:      texels 0..1, weights [1-f, f] (PCF1x1, one bilinear tap);
//   3 PCF3x3gather:      texels -1..2, weights [1-f, 1, 1, f] / 9 (3x3 bilinear taps over a 4x4 footprint);
//   4, 5 (gather path):  texels -2..3, weights [1-f, 1, 1, 1, 1, f] / 25 (5x5 bilinear taps over a 6x6 footprint).
// PERF (2026-10-06): the 2x2 texel block t0 .. t0 + (1, 1) of the shadow map in ONE textureGather (FEATURE_GATHER4, as UE's ManualPCF fetches it), in the
// gather order x = (0, 1), y = (1, 1), z = (1, 0), w = (0, 0) relative to t0; texels outside the map read as the cleared border (depth 1.0), as csm_tap does.
fn csm_gather(layer: i32, t0: vec2<i32>, size: i32) -> vec4<f32> {
    let d = textureGather(shadow_map, shadow_point_samp, (vec2<f32>(t0) + 1.0) / f32(size), layer);
    let xs = vec4<i32>(t0.x, t0.x + 1, t0.x + 1, t0.x);
    let ys = vec4<i32>(t0.y + 1, t0.y + 1, t0.y, t0.y);
    let outside = (xs < vec4<i32>(0)) | (ys < vec4<i32>(0)) | (xs >= vec4<i32>(size)) | (ys >= vec4<i32>(size));
    return select(d, vec4<f32>(1.0), outside);
}
// Weight of texel row / column `i` of a (2h)-wide kernel: the bilinear edge weights 1 - f / f, 1 inside (see the kernel list below).
fn csm_pcf_weight(i: i32, h: i32, f: f32) -> f32 {
    if (i == 1 - h) { return 1.0 - f; }
    if (i == h) { return f; }
    return 1.0;
}
fn csm_pcf(layer: i32, uv: vec2<f32>, s: f32, k: f32) -> f32 {
    let size = i32(light.misc.z);
    let q = i32(light.shadow_params.z);
    if (q <= 1) { return csm_tap(layer, vec2<i32>(floor(uv * light.misc.z)), s, k, size); }
    let h = min(q - 1, 3);   // 2 -> 1, 3 -> 2, 4 / 5 -> 3
    let tp = uv * light.misc.z - 0.5;
    let fr = fract(tp);
    let tc = vec2<i32>(floor(tp));
    var sum = 0.0;
    // PERF: the (2h)x(2h) texels j, i in [1 - h, h] as h x h gathered 2x2 blocks; each texel keeps its weight and its csm_tap comparison
    for (var by = 1 - h; by <= h; by = by + 2) {
        let wy0 = csm_pcf_weight(by, h, fr.y);
        let wy1 = csm_pcf_weight(by + 1, h, fr.y);
        for (var bx = 1 - h; bx <= h; bx = bx + 2) {
            let wx0 = csm_pcf_weight(bx, h, fr.x);
            let wx1 = csm_pcf_weight(bx + 1, h, fr.x);
            let d = csm_gather(layer, tc + vec2<i32>(bx, by), size);
            let c = clamp((d - vec4<f32>(s)) * k + 1.0, vec4<f32>(0.0), vec4<f32>(1.0));
            sum = sum + wx0 * wy1 * c.x + wx1 * wy1 * c.y + wx1 * wy0 * c.z + wx0 * wy0 * c.w;
        }
    }
    let taps = f32(2 * h - 1);
    return sum / (taps * taps);
}
// One cascade's projection pass: ManualPCF -> ShadowSharpen. The pass writes EncodeLightAttenuation(Square(Shadow)) = Shadow (FadedShadow = lerp(1, Square(Shadow), ShadowFadeFraction = 1),
// sqrt encode), the cascade blend happens on those stored values and the light pass reads Square(stored) (GetPerPixelLightAttenuation): the cascades are blended on Shadow, then squared.
fn cascade_shadow(layer: i32, world_pos: vec3<f32>) -> f32 {
    let c = light.shadow_matrix[layer] * vec4<f32>(world_pos, 1.0);
    let s = min(c.z, 0.99999);   // LightSpacePixelDepthForOpaque
    let k = 1.0 / light.cascade_bias[layer];
    let sh = clamp((csm_pcf(layer, c.xy, s, k) - 0.5) * light.cascade_texel.w + 0.5, 0.0, 1.0);
    return sh;
}
// Cascades render far to near; every non-last cascade is extended by SplitFarFadeRegion and blended over the next one with
// BlendFactor = 1 - saturate((SceneW - FadePlaneOffset) * InvFadePlaneLength) (ShadowProjectionPixelShader.usf USE_FADE_PLANE, BO_Add SrcAlpha / InvSrcAlpha).
// Then DeferredLightingCommon.usf GetShadowTerms fades to "no dynamic shadow" with DistanceFromCameraFade = Square(saturate((depth - Near) / (Far - Near))).
fn sun_shadow(world_pos: vec3<f32>, n: vec3<f32>, ndl: f32) -> f32 {
    let depth = -(camera.view * vec4<f32>(world_pos, 1.0)).z;
    // DISTANCE FIELD HOOK: with the ray-traced cascade the whole-scene fade runs to DistanceFieldShadowDistance (GetDirectionalLightDistanceFadeParameters)
    let df_on = dfu.sh0.x > 0.5;
    // cascade_far.w = FarDistance of GetDirectionalLightDistanceFadeParameters: the DF distance whenever the proxy creates the ray-traced cascade (even with
    // r.DistanceFieldShadowing 0), else the scaled CSM distance (renderer.rs)
    let far_all = light.cascade_far.w;
    if (light.shadow_params.x < 0.5 || depth >= far_all) { return 1.0; }
    // shadow_params.y = shadow-mapped cascades (min(DynamicShadowCascades, r.Shadow.CSM.MaxCascades)); a non-last cascade fades into the next one, the last
    // into the cleared attenuation (its cascade_ext is non-zero only when the ray-traced cascade follows it, BO_Add SrcAlpha / InvSrcAlpha)
    let n_csm = i32(light.shadow_params.y);
    var s: f32 = 1.0;   // the stored (sqrt-encoded) CSM attenuation; 1 = the cleared buffer
    for (var i = 0; i < n_csm; i = i + 1) {
        let e = light.cascade_far[i];
        let ext = light.cascade_ext[i];
        if (depth < e) { s = cascade_shadow(i, world_pos); break; }
        if (depth < e + ext) {
            var next = 1.0;
            if (i + 1 < n_csm) { next = cascade_shadow(i + 1, world_pos); }
            s = mix(next, cascade_shadow(i, world_pos), 1.0 - clamp((depth - e) / ext, 0.0, 1.0));
            break;
        }
    }
    s = s * s;   // GetPerPixelLightAttenuation = Square(stored)
    g_df_vis = 1.0;
    let depth_cm = depth * 100.0;
    if (df_on && depth_cm > dfu.sh0.y && depth_cm < dfu.sh0.z) {
        // ray-traced cascade, written EncodeLightAttenuation(lerp(1, DF, NearBlendFactor)) with BO_Min over the CSM: min on sqrt values = min after squaring
        // (the depth bounds test of the upsample pass, SplitNear - SplitNearFadeRegion .. SplitFar, is the `if` above)
        // DF SHADOW SCREEN PASS (2026-10-06): the trace ran at half resolution in screen space (distance_field_shadow.wgsl, r.DFFullResolution 0); this pixel takes
        // UE's bilateral upsample of it (was: the trace per shaded fragment, the r.DFFullResolution 1 look)
        g_df_vis = df_shadow_upsample(world_pos, depth_cm);
        s = min(s, g_df_vis);
    }
    let far = far_all;
    let near = far - far * light.cascade_ext.w;   // NearDistance = FarDistance - FarDistance * ShadowDistanceFadeoutFraction
    let fade = clamp((depth - near) / max(far - near, 0.0001), 0.0, 1.0);
    return mix(s, 1.0, fade * fade);
}
// LIGHT FUNCTION (2026-10-07): the Athena sun's light function M_Athena_Fortress_Skybox_LF_Spinning_2, the HIGH quality-level FLightFunctionPS (stream 76146,
// r.MaterialQualityLevel 1 at Epic; the old constant 0.6 was the LOW program), see src/light_function.rs for the evidence and the CPU mirror.
// Written sqrt-encoded into the unorm light attenuation buffer and read back squared, i.e. OpaqueShadowTerm *= saturate(value).
@group(0) @binding(20) var lf_fort: texture_2d<f32>;    // T_FORT_LF (t1 of the program)
@group(0) @binding(21) var lf_spiral: texture_2d<f32>;  // T_spiralLF (t0)
@group(0) @binding(22) var lf_samp: sampler;
// explicit LOD (the program samples with implicit derivatives; this helper also runs in non-uniform control flow): isotropic footprint of one pixel
fn lf_sample(t: texture_2d<f32>, uv: vec2<f32>, texels_per_uv: f32, footprint_cm: f32, size_cm: f32) -> f32 {
    return textureSampleLevel(t, lf_samp, uv, max(log2(max(footprint_cm * texels_per_uv / size_cm, 1e-6)), 0.0)).r;
}
fn light_function(world_pos: vec3<f32>) -> f32 {
    if (light.light_fn_world.w < 0.5) { return 1.0; }   // EngineShowFlags.LightFunctions off: no light-function pass, attenuation 1
    let w = vec3<f32>(world_pos.x * 100.0 - light.light_fn_world.x, world_pos.z * 100.0 - light.light_fn_world.y, world_pos.y * 100.0 - light.light_fn_world.z);
    let t = light.light_fn.w;
    let cam_dist_cm = length(world_pos - camera.position.xyz) * 100.0;
    let foot = cam_dist_cm * 2.0 * camera.proj.y / max(camera.viewport.y, 1.0);
    let fort_n = f32(textureDimensions(lf_fort).x);
    let spiral_n = f32(textureDimensions(lf_spiral).x);
    // material constants (cb2[2..5]): Speed_1, Size, Speed, Size_1 / Speed_2, Size_2, Lerp Sub B, Lerp Sub A / ContrastHigh, ContrastLow, LightenShadow, Bright Distance / Dark Dist
    let fort3 = lf_sample(lf_fort, w.xy / 8437.12 + vec2<f32>(0.0, t * 7.003327 * 0.01), fort_n, foot, 8437.12);
    let spiral = lf_sample(lf_spiral, w.xy / 11682.961 + vec2<f32>(0.0, t * 1.368582 * 0.01), spiral_n, foot, 11682.961);
    let fort2 = lf_sample(lf_fort, w.xy / 7685.4736 + vec2<f32>(0.0, t * 0.893945 * 0.01), fort_n, foot, 7685.4736);
    let m3 = fort3 * fort3 * (0.0 - (-0.149712)) + (-0.149712);
    let prod = spiral * fort2;
    var a = clamp(-(1.0 - prod) * m3 + prod, 0.0, 1.0);
    a = clamp(a * (1.416333 - 0.333945) + 0.333945, 0.0, 1.0);
    let y = clamp(1.0 + (-0.006251) - a, 0.05, 1.0);
    let x = (y * 2.0 + 0.5) * 0.666667;
    let far_v = (x * x - 1.0) * 0.35 + 1.0 - x * x;
    let d = length(w);
    let fb = clamp((d - 250000.0) / 800000.0, 0.0, 1.0);
    let fd = clamp((d - 16000.0) / 2800000.0, 0.0, 1.0);
    var v = fb * far_v + x * x;
    v = max(v - fd * v, 0.0);
    let grey = v * 0.3333 * 3.0;
    let fade = clamp((light.light_fn.z * 100.0 - cam_dist_cm) / (0.2 * light.light_fn.z * 100.0), 0.0, 1.0);
    return clamp(light.light_fn.y + (grey - light.light_fn.y) * fade, 0.0, 1.0);
}
// UE evaluates the shadow at the pixel's world position (no normal offset); n and ndl are kept for the callers' signature.
var<private> g_vis: f32 = 1.0;   // last sun visibility evaluated by this fragment (debug view, material_debug bit 16)
fn sun_visibility(world_pos: vec3<f32>, n: vec3<f32>, ndl: f32) -> f32 {
    g_vis = sun_shadow(world_pos, n, ndl) * light_function(world_pos);   // OpaqueShadowTerm = lerp(CSM, static, fade) * LightAttenuation.z (light function)
    return g_vis;
}

// SKY LIGHT: GetSkySHDiffuse(N) * View.SkyLightColor (ReflectionEnvironmentShared.usf:67) on the specified-cubemap SH, full sphere (sky_light.rs);
// light.sky_sh already holds the SkyLightColor multiply. N is turned into UE axes with the canonical (x, z, y) swap (inverse of lighting::ue_to_world).
// SUN-AXIS FIX (2026-10-05): was UE (-z, x, y), the inverse of the old 90-deg-off sun map; the cubemap's UE axes now line up with the props / terrain.
fn sky_lookup(n_world: vec3<f32>) -> vec3<f32> {
    let n = vec4<f32>(n_world.x, n_world.z, n_world.y, 1.0);
    let i0 = vec3<f32>(dot(light.sky_sh[0], n), dot(light.sky_sh[1], n), dot(light.sky_sh[2], n));
    let vb = n.xyzz * n.yzzx;
    let i1 = vec3<f32>(dot(light.sky_sh[3], vb), dot(light.sky_sh[4], vb), dot(light.sky_sh[5], vb));
    let i2 = light.sky_sh[6].xyz * (n.x * n.x - n.y * n.y);
    return max(vec3<f32>(0.0), i0 + i1 + i2);
}
fn ambient_light(n: vec3<f32>) -> vec3<f32> {
    // DISTANCE FIELD HOOK: the shading normal of the fragment's sky term (first call), used by output_hdr's DFAO sky occlusion
    if (!g_amb_set) { g_amb_n = n; g_amb_set = true; }
    return sky_lookup(n);
}
// DISTANCE FIELD HOOK: SkyLighting.usf SkyLightDiffusePS with the DFAO bent normal. The bent normal comes from the AO buffer of the previous frame
// (forward shading: see distance_field_ao.rs), bilaterally upsampled like AOUpsamplePS at the reprojected UV. `amb` = DiffuseColor * Lookup(N) of
// this port's sky model; returns the occluded sky term. Pixels without an AO sample keep `amb`.
// The upsampled DFAO bent normal at `world_pos` (renderer world axes; xyz = bent normal, w = 1 when an AO sample exists, else 0): the reprojection + bilateral
// upsample of df_sky_occlusion, shared with the sky specular occlusion (SKY REFLECTION HOOK, `BentNormalAOTexture` of SkyLightingShared.usf).
// LIT-TAIL DEDUPE (2026-10-07, OPT_LOG round 5): reflection_inputs and output_hdr's df_sky_occlusion both ask for the bent normal at the same world_pos; the
// first result is kept per invocation, keyed on the bit pattern of world_pos (same inputs -> same value, pixel-identical), so the upsample runs once per pixel.
var<private> g_bn_have: bool = false;
var<private> g_bn_key: vec3<u32> = vec3<u32>(0u);
var<private> g_bn_val: vec4<f32> = vec4<f32>(0.0);
fn df_bent_normal(world_pos: vec3<f32>) -> vec4<f32> {
    let key = bitcast<vec3<u32>>(world_pos);
    if (g_bn_have && all(key == g_bn_key)) { return g_bn_val; }
    let r = df_bent_normal_eval(world_pos);
    g_bn_have = true;
    g_bn_key = key;
    g_bn_val = r;
    return r;
}
fn df_bent_normal_eval(world_pos: vec3<f32>) -> vec4<f32> {
    if (dfu.ao0.x < 0.5) { return vec4<f32>(0.0); }
    let clip = dfu.prev_view_proj * vec4<f32>(world_pos, 1.0);
    if (clip.w <= 0.0) { return vec4<f32>(0.0); }
    let uv = vec2<f32>(clip.x / clip.w * 0.5 + 0.5, 0.5 - clip.y / clip.w * 0.5);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) { return vec4<f32>(0.0); }
    let scene_depth = dot(world_pos - dfu.prev_eye.xyz, dfu.prev_fwd.xyz) * 100.0;
    let low = dfu.ao3.xy;
    let c00 = floor(uv * low - 0.5) / low + 0.5 * dfu.ao3.zw;
    let bw = (uv - c00) * low;
    let sz = vec2<i32>(low);
    let t0 = clamp(vec2<i32>(floor(c00 * low)), vec2<i32>(0), sz - vec2<i32>(1));
    let t1 = clamp(t0 + vec2<i32>(1), vec2<i32>(0), sz - vec2<i32>(1));
    let v00 = textureLoad(df_ao, t0, 0);
    let v10 = textureLoad(df_ao, vec2<i32>(t1.x, t0.y), 0);
    let v01 = textureLoad(df_ao, vec2<i32>(t0.x, t1.y), 0);
    let v11 = textureLoad(df_ao, t1, 0);
    let cw = vec4<f32>((1.0 - bw.y) * (1.0 - bw.x), (1.0 - bw.y) * bw.x, bw.y * (1.0 - bw.x), bw.y * bw.x);
    let dw = 1.0 / (abs(vec4<f32>(v00.w, v10.w, v01.w, v11.w) - vec4<f32>(scene_depth)) + 0.0001);
    let fw = cw * dw;
    if (min(min(abs(v00.w - scene_depth), abs(v10.w - scene_depth)), min(abs(v01.w - scene_depth), abs(v11.w - scene_depth))) > 0.05 * scene_depth + 50.0) { return vec4<f32>(0.0); }
    let bent_df = (fw.x * v00.xyz + fw.y * v10.xyz + fw.z * v01.xyz + fw.w * v11.xyz) / dot(fw, vec4<f32>(1.0));
    return vec4<f32>(bent_df.x, bent_df.z, bent_df.y, 1.0);   // DF space -> renderer world (direction)
}
// FX SHADOW OCCLUSION TINT (2026-10-05): SkyLightDiffusePS (UE4.16 SkyLighting.usf:76-88, :134) with SkyVisibility = V * m (OCM_Multiply, the Athena sky light),
// V = the DFAO visibility after contrast / OcclusionExponent / MinOcclusion, m = min(GBufferAO, SSAO):
//   Lighting += (V m DotProductFactor) * Lookup(SkyLightingNormal) * DiffuseColor + (1 - V m) * OcclusionTint * DiffuseColor
// The tint term carries no AO factor of its own. m is only known in the AO pass (ssao_apply.wgsl), which scales the ambient share by m / GBufferAO, so the
// term linear in m is returned (scaled part: GBufferAO * V * DiffuseColor * (Dot * Lookup - Tint)) and the constant OcclusionTint * DiffuseColor goes to
// `g_sky_tint` (added unscaled by output_hdr). `amb` = DiffuseColor * Lookup(N) WITHOUT any AO (every caller), so DiffuseColor is exact even at
// GBufferAO = 0 (UE: the full Tint * DiffuseColor); GBufferAO = g_refl_ao (reflection_inputs runs before output_hdr in every pass with an AO input, 1 elsewhere).
// DFAO off (r.DistanceFieldAO 0 at ShadowQuality 0 / 1, or no AO buffer yet): the non-APPLY_SHADOWING permutation, SkyVisibility = 1 before
// pow / MinOcclusion (the tint / exponent / MinOcclusion uniforms are set every frame, distance_field_ao.rs fill_uniform), i.e. V = 1. A pixel without an AO
// sample (disocclusion) takes the same V = 1, Dot = 1, GBuffer normal. No ambient_light call (`!g_amb_set`: unlit / emissive-only paths = ShadingModelID 0,
// no sky diffuse in UE) keeps `amb * GBufferAO` and no tint.
var<private> g_sky_tint: vec3<f32> = vec3<f32>(0.0);
fn df_sky_occlusion(amb: vec3<f32>, world_pos: vec3<f32>) -> vec3<f32> {
    g_sky_vis = 1.0;
    g_sky_tint = vec3<f32>(0.0);
    if (!g_amb_set) { return amb * g_refl_ao; }
    let n = g_amb_n;
    var vis = 1.0;
    var sky_n = n;
    var dot_factor = 1.0;
    let bn = df_bent_normal(world_pos);   // w = 0 when the DFAO is off / no AO sample
    if (bn.w > 0.5) {
        let bent = bn.xyz;
        vis = length(bent);
        let nb = bent / max(vis, 0.00001);
        sky_n = mix(nb, n, vis);
        dot_factor = mix(dot(nb, n), 1.0, vis);
        let curve = 1.0 / (1.0 + exp(-dfu.ao2.x * (vis * 10.0 - 5.0)));
        vis = clamp(curve * dfu.ao2.y + dfu.ao2.z, 0.0, 1.0);
    }
    vis = pow(vis, dfu.ao0.y);
    vis = mix(vis, 1.0, dfu.ao0.z);
    g_sky_vis = vis;
    let ln = sky_lookup(n);
    let diffuse = select(vec3<f32>(0.0), amb / max(ln, vec3<f32>(1e-6)), ln > vec3<f32>(1e-6));   // DiffuseColor
    let tint = dfu.ao1.xyz;
    g_sky_tint = tint * diffuse;
    // LIT-TAIL DEDUPE: without an AO sample sky_n == n, so Lookup(sky_n) is `ln` (same input, same value)
    var ls = ln;
    if (bn.w > 0.5) { ls = sky_lookup(sky_n); }
    return g_refl_ao * vis * diffuse * (dot_factor * ls - tint);
}

// ---- SKY REFLECTION HOOK: the sky light's specular (sky_reflection.rs; bindings 11..14 of the globals) ----
struct SkyReflU {
    color: vec4<f32>,    // rgb View.SkyLightColor, w SkyLightParameters.y (1 = the processed sky cube exists)
    params: vec4<f32>,   // x SkyLightParameters.x (max mip), y InvSkySpecularOcclusionStrength, z View.DistanceFieldAOSpecularOcclusionMode
}
@group(0) @binding(11) var sky_refl_cube: texture_cube<f32>;
@group(0) @binding(12) var sky_refl_samp: sampler;
@group(0) @binding(13) var<uniform> sky_refl: SkyReflU;
@group(0) @binding(14) var preintegrated_gf: texture_2d<f32>;
// EnvBRDF (BRDF.usf:196): the PreIntegratedGF LUT at (NoV, Roughness), bilinear clamp.
fn env_brdf(spec_color: vec3<f32>, roughness: f32, nov: f32) -> vec3<f32> {
    let ab = textureSampleLevel(preintegrated_gf, sky_refl_samp, vec2<f32>(nov, roughness), 0.0).rg;
    return spec_color * ab.x + clamp(50.0 * spec_color.g, 0.0, 1.0) * ab.y;
}
// GetSkyLightReflection (ReflectionEnvironmentShared.usf:35): the processed cube at ComputeReflectionCaptureMipFromRoughness, x View.SkyLightColor.
// The cube is in UE axes: the renderer direction is swapped (x, z, y) like sky_lookup.
fn sky_light_reflection(r_world: vec3<f32>, roughness: f32) -> vec3<f32> {
    let mip = sky_refl.params.x - 1.0 - (1.0 - 1.2 * log2(max(roughness, 1e-4)));
    let d = vec3<f32>(r_world.x, r_world.z, r_world.y);
    return textureSampleLevel(sky_refl_cube, sky_refl_samp, d, mip).rgb * sky_refl.color.rgb;
}
// ApproximateConeConeIntersection (SkyLightingShared.usf:7).
fn cone_cone_intersection(arc0: f32, arc1: f32, angle_between: f32) -> f32 {
    let diff = abs(arc0 - arc1);
    return smoothstep(0.0, 1.0, 1.0 - clamp((angle_between - diff) / (arc0 + arc1 - diff), 0.0, 1.0));
}
// ---- REFLECTION APPLY (reflection.rs): the per-pixel inputs of the deferred ReflectionApplyPS, written by every opaque lit pass as extra targets ----
// UE 4.16 applies the specular indirect light in a deferred pass AFTER the screen-space AO (ReflectionEnvironmentShaders.usf:537 ReflectionApplyPS):
//   AO = GBufferAO * ScreenSpaceData.AmbientOcclusion; SpecularOcclusion = saturate(Square(NoV + AO) - 1 + AO) (:561-563)
//   SpecularLighting = SSR; .rgb *= SpecularColor (EnvBRDF * SO); .rgb += (1 - SSR.a) * (Sky * SpecularColor * IndirectSpecularOcclusion + ExtraIndirectSpecular * SpecularColor) (:585-665)
// The occlusion is not linear in AO, so this forward renderer cannot fold it in here: the lit passes store the parts that do not depend on the screen-space AO
// (LitOut below) and reflection.rs / ssao_apply.wgsl apply them after the AO, with the SSR, before any later pass (UE fogs the reflections too: the
// fog transmittance is premultiplied, `g_fog_t` of output_hdr).
var<private> g_refl_sky: vec3<f32> = vec3<f32>(0.0);    // GetSkyLightReflection(R) * EnvBRDF * IndirectSpecularOcclusion + ExtraIndirectSpecular * EnvBRDF (no SO)
var<private> g_refl_brdf: vec3<f32> = vec3<f32>(0.0);   // EnvBRDF(SpecularColor, Roughness, NoV)
var<private> g_refl_nov: f32 = 0.0;
var<private> g_refl_ao: f32 = 1.0;                      // GBufferAO
var<private> g_refl_n: vec3<f32> = vec3<f32>(0.0);      // GBuffer world normal (renderer axes); 0 = no reflection (ShadingModelID 0 for the SSR)
var<private> g_refl_rough: f32 = 0.0;                   // GBuffer roughness
var<private> g_fog_t: f32 = 1.0;                        // fog transmittance output_hdr applied to the lit colour
struct LitOut {
    @location(0) hdr: vec4<f32>,
    @location(1) refl: vec4<f32>,   // rgb g_refl_sky * fog transmittance, a NoV
    @location(2) brdf: vec4<f32>,   // rgb EnvBRDF * fog transmittance, a GBufferAO
    @location(3) gbuf: vec4<f32>,   // xyz world normal, w roughness (the SSR's GBuffer)
}
fn lit_out(hdr: vec4<f32>) -> LitOut {
    var o: LitOut;
    o.hdr = hdr;
    o.refl = vec4<f32>(g_refl_sky * g_fog_t, g_refl_nov);
    o.brdf = vec4<f32>(g_refl_brdf * g_fog_t, g_refl_ao);
    o.gbuf = vec4<f32>(g_refl_n, g_refl_rough);
    return o;
}
// The sky light's specular of an opaque lit pixel: ReflectionApplyPS with APPLY_SKYLIGHT and no reflection captures (ReflectionEnvironmentShaders.usf:533,
// the path UE 4.16 takes when ShouldDoReflectionEnvironment() is false, ReflectionEnvironment.cpp:907):
//   SpecularColor = EnvBRDF(SpecularColor, Roughness, NoV) * SpecularOcclusion,  SpecularOcclusion = saturate(Square(NoV + AO) - 1 + AO)
//   R = GetOffSpecularPeakReflectionDir(N, reflect(-V, N), Roughness)
//   + (GetSkyLightReflection(R, Roughness) * SpecularColor) * IndirectSpecularOcclusion + ExtraIndirectSpecular * SpecularColor
// with the DFAO terms of GetDistanceFieldAOSpecularOcclusion (SkyLightingShared.usf:26). `ao` = GBufferAO. Records the AO-independent parts for the deferred
// apply (see LitOut above; the SO with the screen-space AO and the SSR composite run in ssao_apply.wgsl). The sky term is 0 while the processed cube is
// missing (SkyLightParameters.y = 0); the GBuffer part (normal, roughness, EnvBRDF) is recorded anyway, the SSR needs it.
fn reflection_inputs(n: vec3<f32>, v: vec3<f32>, world_pos: vec3<f32>, spec_color_in: vec3<f32>, roughness: f32, ao: f32) {
    let nov = clamp(dot(n, v), 0.0, 1.0);
    let spec_color = env_brdf(spec_color_in, roughness, nov);
    g_refl_n = n;
    g_refl_rough = roughness;
    g_refl_ao = ao;
    g_refl_nov = nov;
    g_refl_brdf = spec_color;
    if (sky_refl.color.w < 0.5) { return; }
    var r = 2.0 * dot(v, n) * n - v;
    let a = roughness * roughness;
    r = mix(n, r, (1.0 - a) * (sqrt(1.0 - a) + a));   // GetOffSpecularPeakReflectionDir (ReflectionEnvironmentShared.usf:107)
    // GetDistanceFieldAOSpecularOcclusion: ApplyBentNormalAO = 1 when the DFAO ran (dfu.ao0.x); BentNormalLength = the raw upsampled bent normal length
    var iso = 1.0;
    var extra = vec3<f32>(0.0);
    let bn = df_bent_normal(world_pos);
    if (bn.w > 0.5) {
        let len = length(bn.xyz);
        if (sky_refl.params.z < 0.5) {
            iso = len;
        } else {
            let refl_cone = max(roughness, 0.1) * 3.14159265;
            let unoccluded = len * 3.14159265 * sky_refl.params.y;
            let between = acos(clamp(dot(bn.xyz, r) / max(len, 0.001), -1.0, 1.0));
            iso = cone_cone_intersection(refl_cone, unoccluded, between);
            iso = mix(0.0, iso, clamp((unoccluded - 0.1) / 0.2, 0.0, 1.0));
        }
        iso = mix(iso, 1.0, dfu.ao0.z);       // OcclusionTintAndMinOcclusion.w = MinOcclusion
        extra = (1.0 - iso) * dfu.ao1.xyz;    // OcclusionTint
    }
    g_refl_sky = sky_light_reflection(r, roughness) * spec_color * iso + extra * spec_color;
}

// Tangent frame from screen-space derivatives (the exported meshes carry no usable tangents), in world space.
fn perturb_normal(n: vec3<f32>, p: vec3<f32>, tex: vec2<f32>, tn: vec3<f32>) -> vec3<f32> {
    let dp1 = dpdx(p);
    let dp2 = gl_dfdy3(p);
    let duv1 = dpdx(tex);
    let duv2 = gl_dfdy2(tex);
    let dp2perp = cross(dp2, n);
    let dp1perp = cross(n, dp1);
    let t = dp2perp * duv1.x + dp1perp * duv2.x;
    let b = dp2perp * duv1.y + dp1perp * duv2.y;
    let invmax = inverseSqrt(max(max(dot(t, t), dot(b, b)), 1e-20));
    return normalize(mat3x3<f32>(t * invmax, b * invmax, n) * tn);
}
fn unpack_normal(texel: vec4<f32>) -> vec3<f32> {
    let xy = texel.rg * 2.0 - 1.0;   // UE tangent space (DirectX, +G toward +V); z reconstructed
    return vec3<f32>(xy, sqrt(max(1.0 - dot(xy, xy), 0.0)));
}

// GGX specular for the sun only (UE4 default lit: D_GGX * Vis_SmithJointApprox * F_Schlick).
// BRDF AUDIT (2026-10-05): every caller is the directional light, so AreaLightSpecular's `LobeRoughness = max(LobeRoughness, LightData.MinRoughness)`
// (DeferredLightingCommon.usf:164) is applied here: MinRoughness = max(ULightComponent::MinRoughness 0.08, .02) (exe ctor 0x1420a4c90 stores 0.08 at +0x2d0, the only
// 0.08 of the ctor, LightComponent.cpp:349; TODM_A / _Child_Athena do not serialize it; GetParameters exe 0x1420b2720 = DirectionalLightComponent.cpp:180, SourceRadius /
// SourceLength 0 -> LobeEnergy 1). NoV = saturate(abs(NoV) + 1e-5) as StandardShading (ShadingModels.usf:51); Vis_SmithJointApprox without an epsilon (BRDF.usf:154).
const LIGHT_MIN_ROUGHNESS: f32 = 0.08;
fn ggx_spec(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, roughness_in: f32, f0: f32) -> f32 {
    let roughness = max(roughness_in, LIGHT_MIN_ROUGHNESS);
    let h = normalize(v + l);
    let nl = clamp(dot(n, l), 0.0, 1.0);
    let nv = clamp(abs(dot(n, v)) + 1e-5, 0.0, 1.0);
    let nh = clamp(dot(n, h), 0.0, 1.0);
    let vh = clamp(dot(v, h), 0.0, 1.0);
    let a = roughness * roughness;
    let a2 = a * a;
    let dd = nh * nh * (a2 - 1.0) + 1.0;
    let d = a2 / (3.14159265 * dd * dd);
    let vis = 0.5 / (nl * (nv * (1.0 - a) + a) + nv * (nl * (1.0 - a) + a));
    let f = f0 + (1.0 - f0) * pow(1.0 - vh, 5.0);
    return d * vis * f * nl;
}
// The sun's GGX lobe with UE's F_Schlick on a SpecularColor (BRDF.usf:169: saturate(50 * SpecularColor.g) * Fc + (1 - Fc) * SpecularColor), i.e. the
// DefaultLit specular of the deferred directional light (ShadingModels.usf StandardShading: D_GGX * Vis_SmithJointApprox * F_Schlick) times NoL.
fn ggx_spec_color(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, roughness: f32, spec_color: vec3<f32>) -> vec3<f32> {
    let s0 = ggx_spec(n, v, l, roughness, 0.0);   // D * Vis * NoL * Fc, Fc = (1 - VoH)^5
    let s1 = ggx_spec(n, v, l, roughness, 1.0);   // D * Vis * NoL
    return clamp(50.0 * spec_color.g, 0.0, 1.0) * vec3<f32>(s0) + (s1 - s0) * spec_color;
}

// Shared tail: height fog, then linear HDR out. Exposure, bloom, AO, vignette, white balance, film curve and the sRGB encode happen in post.wgsl.
// HDR target alpha = share of this pixel's final luminance that is ambient light, after fog attenuation.
// SHADING MODELS: the TWOSIDED_FOLIAGE back-face sky term (shading_models.wgsl sm_sky_back, SkyLighting.usf:96-101), occluded by the same SkyVisibility as the
// front term and part of the ambient share; zero for every other model / caller.
var<private> g_sky_back: vec3<f32> = vec3<f32>(0.0);
// LIGHTING BREAKDOWN (2026-10-07, verification aid, FN_MATERIAL_DEBUG bits, not UE): one lighting term per view, no fog / AO pass / reflections unless noted.
//   128 sun diffuse UNSHADOWED, 256 sun diffuse with the shadow x light function, 512 sky diffuse unoccluded (SH x DiffuseColor), 1024 sky diffuse after
//   GBufferAO + DFAO, 2048 world normal (n * 0.5 + 0.5), 4096 DiffuseColor, 8192 sun specular (shadowed) + the deferred sky reflection / SSR.
// The lit tails fill these with dbg_terms before output_hdr.
var<private> g_dbg_diffuse: vec3<f32> = vec3<f32>(0.0);
var<private> g_dbg_n: vec3<f32> = vec3<f32>(0.0, 1.0, 0.0);
var<private> g_dbg_ndl: f32 = 0.0;
var<private> g_dbg_vis: f32 = 1.0;
var<private> g_dbg_spec: vec3<f32> = vec3<f32>(0.0);
fn dbg_terms(diffuse: vec3<f32>, n: vec3<f32>, ndl: f32, vis: f32, sun_spec: vec3<f32>) { g_dbg_diffuse = diffuse; g_dbg_n = n; g_dbg_ndl = max(ndl, 0.0); g_dbg_vis = vis; g_dbg_spec = sun_spec; }
fn dbg_part_any() -> bool { return dbg(128u) || dbg(256u) || dbg(512u) || dbg(1024u) || dbg(2048u) || dbg(4096u) || dbg(8192u); }
fn output_hdr(lit_in: vec3<f32>, amb_in: vec3<f32>, world_pos: vec3<f32>, cam: vec3<f32>) -> vec4<f32> {
    // DISTANCE FIELD HOOK: replace the unoccluded sky term by the DFAO-occluded one
    let amb = df_sky_occlusion(amb_in, world_pos) + g_sky_back * (g_refl_ao * g_sky_vis);
    var lit = lit_in - amb_in + amb + g_sky_tint;   // FX SHADOW OCCLUSION TINT: + the AO-independent OcclusionTint * DiffuseColor (not in the ambient share)
    var fog = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    if (light.misc.w > 0.5) { fog = height_fog(light.fog, world_pos, cam, light.sun_dir.xyz, -camera.view_inverse[2].xyz); }
    if (dbg_part_any()) {
        var p = vec3<f32>(0.0);
        if (dbg(128u)) { p = g_dbg_diffuse * light.sun_radiance.xyz * g_dbg_ndl; }
        else if (dbg(256u)) { p = g_dbg_diffuse * light.sun_radiance.xyz * g_dbg_ndl * g_dbg_vis; }
        else if (dbg(512u)) { p = amb_in; }
        else if (dbg(1024u)) { p = amb; }
        else if (dbg(2048u)) { p = g_dbg_n * 0.5 + 0.5; }
        else if (dbg(4096u)) { p = g_dbg_diffuse; }
        else if (dbg(8192u)) { p = g_dbg_spec; }
        g_fog_t = select(0.0, 1.0, dbg(8192u));
        return vec4<f32>(max(p, vec3<f32>(0.0)), 0.0);
    }
    if (dbg(32u)) { lit = vec3<f32>(g_df_vis); }
    else if (dbg(64u)) { lit = vec3<f32>(g_sky_vis); }
    else if (dbg(16u)) { lit = vec3<f32>(g_vis); }
    else if (dbg(8u)) { lit = vec3<f32>((1.0 - fog.a) / (1.0 - light.fog.p0.w + 1e-4) * 0.8); }
    if (!dbg(4u) && !dbg(8u) && !dbg(16u) && !dbg(32u) && !dbg(64u)) { lit = lit * fog.a + fog.rgb; }
    // REFLECTION APPLY: the deferred reflections are fogged like the lit colour (UE's fog pass runs after ReflectionApplyPS); none in the debug views
    g_fog_t = select(select(fog.a, 1.0, dbg(4u)), 0.0, dbg(8u) || dbg(16u) || dbg(32u) || dbg(64u));
    let amb_luma = luma709(amb) * select(fog.a, 1.0, dbg(8u) || dbg(4u));
    let lit_pos = max(lit, vec3<f32>(0.0));
    return vec4<f32>(lit_pos, clamp(amb_luma / max(luma709(lit_pos), 1e-4), 0.0, 1.0));
}
const SM_UNLIT: u32 = 0u;
const SM_DEFAULT_LIT: u32 = 1u;
const SM_SUBSURFACE: u32 = 2u;
const SM_PREINTEGRATED_SKIN: u32 = 3u;
const SM_CLEAR_COAT: u32 = 4u;
const SM_SUBSURFACE_PROFILE: u32 = 5u;
const SM_TWOSIDED_FOLIAGE: u32 = 6u;
const SM_HAIR: u32 = 7u;
const SM_CLOTH: u32 = 8u;
const SM_EYE: u32 = 9u;

// DecodeShadingModelId (DeferredShadingCommon.usf:285): GBufferB.a = (ShadingModelID | SelectiveOutputMask) / 255
fn sm_decode_id(gbuffer_b_a: f32) -> u32 { return u32(round(clamp(gbuffer_b_a, 0.0, 1.0) * 255.0)) & 0xFu; }
// IsSubsurfaceModel (DeferredShadingCommon.usf:295)
fn sm_is_subsurface(id: u32) -> bool {
    return id == SM_SUBSURFACE || id == SM_PREINTEGRATED_SKIN || id == SM_SUBSURFACE_PROFILE || id == SM_TWOSIDED_FOLIAGE || id == SM_HAIR || id == SM_EYE;
}
fn sm_subsurface_color(custom: vec4<f32>) -> vec3<f32> { return custom.rgb * custom.rgb; }

// SubsurfaceShadingSubsurface (ShadingModels.usf:606)
fn sm_shading_subsurface(custom: vec4<f32>, gbuffer_ao: f32, l: vec3<f32>, v: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let sss = sm_subsurface_color(custom);
    let opacity = custom.a;
    let h = normalize(v + l);
    let in_scatter = pow(clamp(dot(l, -v), 0.0, 1.0), 12.0) * mix(3.0, 0.1, opacity);
    let normal_contribution = clamp(dot(n, h) * opacity + 1.0 - opacity, 0.0, 1.0);
    let back_scatter = gbuffer_ao * normal_contribution / (3.14159265 * 2.0);
    return sss * mix(back_scatter, 1.0, in_scatter);
}
// SubsurfaceShadingTwoSided (ShadingModels.usf:625)
fn sm_shading_two_sided(sss: vec3<f32>, l: vec3<f32>, v: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let wrap = 0.5;
    let nol = clamp((dot(-n, l) + wrap) / ((1.0 + wrap) * (1.0 + wrap)), 0.0, 1.0);
    let vol = clamp(dot(v, -l), 0.0, 1.0);
    let a = 0.6;
    let a2 = a * a;
    let d = (vol * a2 - vol) * vol + 1.0;
    let ggx = (a2 / 3.14159265) / (d * d);
    return nol * ggx * sss;
}
// SubsurfaceShading (ShadingModels.usf:726) for the models this port draws; PREINTEGRATED_SKIN (needs the PreintegratedSkinBRDF texture), HAIR and EYE return 0
// here: no material of the audited foliage / character set uses them (docs/spec/material_coverage.md), they are reported instead of approximated.
fn sm_subsurface_shading(id: u32, custom: vec4<f32>, gbuffer_ao: f32, l: vec3<f32>, v: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    if (id == SM_SUBSURFACE) { return sm_shading_subsurface(custom, gbuffer_ao, l, v, n); }
    if (id == SM_TWOSIDED_FOLIAGE) { return sm_shading_two_sided(sm_subsurface_color(custom), l, v, n); }
    return vec3<f32>(0.0);
}
struct LocalLightGpu {
    pos_inv_radius: vec4<f32>,
    color_falloff: vec4<f32>,
    dir_min_rough: vec4<f32>,
    spot_source: vec4<f32>,
    flags: vec4<f32>,
}
@group(0) @binding(23) var<storage, read> ll_lights: array<LocalLightGpu>;
@group(0) @binding(24) var<storage, read> ll_grid: array<u32>;
@group(0) @binding(25) var ll_ies: texture_2d<f32>;
var<private> g_ll_scatter: vec3<f32> = vec3<f32>(0.0);
fn ll_cell(world: vec3<f32>) -> vec2<u32> {
    let nx = ll_grid[0];
    let ny = ll_grid[1];
    let nz = ll_grid[2];
    if (ll_grid[7] == 0u) { return vec2<u32>(0u, 0u); }
    let pv = camera.view * vec4<f32>(world, 1.0);
    let depth = -pv.z;
    if (depth <= 0.0) { return vec2<u32>(0u, 0u); }
    let ndc = vec2<f32>(pv.x / (depth * camera.proj.x), pv.y / (depth * camera.proj.y));
    let px = clamp((ndc.x * 0.5 + 0.5) * camera.viewport.x, 0.0, camera.viewport.x - 1.0);
    let py = clamp((0.5 - ndc.y * 0.5) * camera.viewport.y, 0.0, camera.viewport.y - 1.0);
    let zp = vec3<f32>(bitcast<f32>(ll_grid[4]), bitcast<f32>(ll_grid[5]), bitcast<f32>(ll_grid[6]));
    let zs = min(u32(max(0.0, log2(depth * 100.0 * zp.x + zp.y) * zp.z)), nz - 1u);
    let cx = min(u32(px) >> ll_grid[3], nx - 1u);
    let cy = min(u32(py) >> ll_grid[3], ny - 1u);
    let c = (zs * ny + cy) * nx + cx;
    return vec2<u32>(ll_grid[8u + 2u * c], ll_grid[8u + 2u * c + 1u]);
}
fn ll_d_ggx(r: f32, noh: f32) -> f32 {
    let a = r * r;
    let a2 = a * a;
    let d = (noh * a2 - noh) * noh + 1.0;
    return a2 / (3.14159265 * d * d);
}
fn ll_vis(r: f32, nov: f32, nol: f32) -> f32 {
    let a = r * r;
    let sv = nol * (nov * (1.0 - a) + a);
    let sl = nov * (nol * (1.0 - a) + a);
    return 0.5 / (sv + sl);
}
fn ll_f_schlick(spec: vec3<f32>, voh: f32) -> vec3<f32> {
    let fc = pow(1.0 - voh, 5.0);
    return clamp(50.0 * spec.g, 0.0, 1.0) * fc + (1.0 - fc) * spec;
}
fn local_lighting(world: vec3<f32>, n: vec3<f32>, v: vec3<f32>, diffuse_color: vec3<f32>, spec_color: vec3<f32>, roughness: f32, id: u32, custom: vec4<f32>, gbuffer_ao: f32) -> vec3<f32> {
    g_ll_scatter = vec3<f32>(0.0);
    let cell = ll_cell(world);
    if (cell.y == 0u) { return vec3<f32>(0.0); }
    let p = world * 100.0;
    var total = vec3<f32>(0.0);
    for (var k = 0u; k < cell.y; k = k + 1u) {
        let li = ll_grid[cell.x + k];
        let l = ll_lights[li];
        var to_light = l.pos_inv_radius.xyz - p;
        let d2 = dot(to_light, to_light);
        var ld = to_light * inverseSqrt(d2);
        var dist_att = 1.0;
        var nol = 0.0;
        var mask = 0.0;
        let inv_r = l.pos_inv_radius.w;
        let src_r = l.spot_source.z;
        let src_len = l.spot_source.w;
        if (l.flags.x > 0.5) {
            if (src_len > 0.0) {
                let l01 = l.dir_min_rough.xyz * src_len;
                let t0 = to_light - 0.5 * l01;
                let t1 = to_light + 0.5 * l01;
                let ls0 = dot(t0, t0);
                let ls1 = dot(t1, t1);
                let r0 = inverseSqrt(ls0);
                let r1 = inverseSqrt(ls1);
                dist_att = 1.0 / ((ls0 * r0 * ls1 * r1 + dot(t0, t1)) * 0.5 + 1.0);
                nol = clamp(0.5 * (dot(n, t0) * r0 + dot(n, t1) * r1), 0.0, 1.0);
            } else {
                dist_att = 1.0 / (d2 + 1.0);
                nol = clamp(dot(n, ld), 0.0, 1.0);
                if (src_r > 0.0) {
                    nol = dot(n, ld);
                    let sin_a2 = clamp(src_r * src_r / d2, 0.0, 1.0);
                    let sin_a = sqrt(sin_a2);
                    if (nol < sin_a) {
                        nol = max(nol, -sin_a);
                        nol = (sin_a + nol) * (sin_a + nol) / (4.0 * sin_a);
                    }
                }
            }
            let m0 = clamp(1.0 - (d2 * inv_r * inv_r) * (d2 * inv_r * inv_r), 0.0, 1.0);
            mask = m0 * m0;
        } else {
            nol = clamp(dot(n, ld), 0.0, 1.0);
            let wl = to_light * inv_r;
            mask = pow(1.0 - clamp(dot(wl, wl), 0.0, 1.0), l.color_falloff.w);
        }
        let sc = clamp((dot(ld, l.dir_min_rough.xyz) - l.spot_source.x) * l.spot_source.y, 0.0, 1.0);
        let spot = sc * sc;
        if (!(mask > 0.0 && spot > 0.0)) { continue; }
        let atten = dist_att * mask * spot;
        let lobe_r = max(roughness, l.dir_min_rough.w);
        let m = lobe_r * lobe_r;
        var energy = 1.0;
        var r = 2.0 * dot(v, n) * n - v;
        let inv_dist = inverseSqrt(dot(to_light, to_light));
        r = normalize(mix(n, r, (1.0 - m) * (sqrt(1.0 - m) + m)));
        if (src_len > 0.0) {
            let line_angle = clamp(src_len * inv_dist, 0.0, 1.0);
            energy = energy * m / clamp(m + 0.5 * line_angle, 0.0, 1.0);
            let l01 = l.dir_min_rough.xyz * src_len;
            let l0 = to_light - 0.5 * l01;
            let a = src_len * src_len;
            let b = dot(r, l01);
            let t = clamp(dot(l0, b * r - l01) / (a - b * b), 0.0, 1.0);
            to_light = l0 + t * l01;
        }
        if (src_r > 0.0) {
            let sphere_angle = clamp(src_r * inv_dist, 0.0, 1.0);
            let e = m / clamp(m + 0.5 * sphere_angle, 0.0, 1.0);
            energy = energy * e * e;
            let closest = dot(to_light, r) * r;
            let c2r = closest - to_light;
            to_light = to_light + c2r * clamp(src_r * inverseSqrt(dot(c2r, c2r)), 0.0, 1.0);
        }
        ld = normalize(to_light);
        let h = normalize(v + ld);
        let nol_s = clamp(dot(n, ld), 0.0, 1.0);
        let nov = clamp(abs(dot(n, v)) + 1e-5, 0.0, 1.0);
        let noh = clamp(dot(n, h), 0.0, 1.0);
        let voh = clamp(dot(v, h), 0.0, 1.0);
        let d = ll_d_ggx(lobe_r, noh) * energy;
        let vis = ll_vis(lobe_r, nov, nol_s);
        let f = ll_f_schlick(spec_color, voh);
        let diffuse = diffuse_color * (1.0 / 3.14159265);
        let lcm = l.color_falloff.xyz * (nol * atten);
        var c = (diffuse + (d * vis) * f) * lcm;
        let sss = sm_subsurface_shading(id, custom, gbuffer_ao, ld, v, n) * l.color_falloff.xyz * atten;
        c = c + sss;
        var profile = 1.0;
        if (l.flags.y >= 0.0) {
            let neg = normalize(p - l.pos_inv_radius.xyz);
            let ang = asin(clamp(dot(neg, l.dir_min_rough.xyz), -1.0, 1.0));
            let rows = f32(textureDimensions(ll_ies).y);
            profile = textureSampleLevel(ll_ies, sky_refl_samp, vec2<f32>(ang / 3.14159265 + 0.5, (l.flags.y + 0.5) / rows), 0.0).r;
        }
        total = total + c * profile;
        g_ll_scatter = g_ll_scatter + (1.0 / 3.14159265) * lcm * profile;
    }
    return total;
}
