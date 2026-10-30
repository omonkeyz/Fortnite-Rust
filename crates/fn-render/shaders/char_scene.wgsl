// BR character material pass (head / hair / body): the ORIGINAL compiled base-pass pixel shader of the material (char_material_gen.wgsl, translated from the cooked
// DXBC by fn-dxbc) evaluated per pixel, then lit like a deferred G-buffer: sun (cascaded shadows) + sky ambient + the shader's own emissive output.
//
// Prepended: common.wgsl, lit_common.wgsl, char_material_gen.wgsl (translated PS functions `head_ps` / `hair_ps` / `body_ps`).
//
// What the translated shader consumes (UE 4.16 base pass, TBasePassPSFNoLightMapPolicy of TGPUSkinVertexFactory):
//   v[0] = TangentToWorld[0] (tangent X), v[1] = TangentToWorld[2] (normal) with the bitangent sign in w, v[2] = vertex colour, v[3].xy = UV0,
//   v[4] = VELOCITY_PREV_POS, v[6] = SV_Position, v[7].x = SV_IsFrontFace (body only); View uniform buffer (cb1), primitive buffer (cb2), pass buffer (cb0), material buffer (cb3, evaluated on the CPU
//   from the cooked uniform expression set + the material instance parameters).
// Everything the shader reads is in Unreal world space (cm, X forward, Y right, Z up). The renderer's world is metres, Y up: u = (r.x, r.z, r.y) * 100 (the axis map
// of the prop / mesh export, see fn-app props.rs). The shader reconstructs the pixel's world position from SV_Position with View.SvPositionToTranslatedWorld (cb1[40..43]);
// here that matrix is replaced by the constant mapping (anything) -> the interpolated translated world position, which is the same point.
//
// Not evaluated (reported): the screen-space subsurface profile (SS_HeroSkin_01) pass, the velocity target o4 (GBufferD o5 = CustomData feeds the shading-model resolve,
// SHADING MODELS), the checkerboard subsurface path (cb1[144].x = 0).

struct Material { kind: vec4<u32> }
@group(1) @binding(0) var<uniform> material: Material;
@group(1) @binding(1) var t_diffuse: texture_2d<f32>;
@group(1) @binding(2) var t_normal: texture_2d<f32>;
@group(1) @binding(3) var t_spec: texture_2d<f32>;
@group(1) @binding(4) var t_mask: texture_2d<f32>;

struct Object { model: mat4x4<f32> }
@group(2) @binding(0) var<uniform> object: Object;
@group(2) @binding(1) var<storage, read> bones: array<mat4x4<f32>>;

// group 3: the material's constant buffer (cb3 registers, evaluated from the cooked uniform expression set) + its fixed texture slot (index 7 = UI_Faces/Noise)
struct CharMat { cb: array<vec4<f32>, 32>, perm_info: vec4<u32> }   // perm_info.x: permutation (0 head, 1 hair, 2 body); was `meta`, a reserved word in wgpu 30's naga
@group(3) @binding(0) var<uniform> cmat: CharMat;
@group(3) @binding(1) var fx_tex7: texture_2d<f32>;
@group(3) @binding(2) var fx_samp7: sampler;
// CHAR PREPASS (2026-10-06): fixed textures 8, 9, 10 of the depth-only coverage shader (char_depth_gen.wgsl; CharacterResInEffect)
@group(3) @binding(3) var fx_tex8: texture_2d<f32>;
@group(3) @binding(4) var fx_samp8: sampler;
@group(3) @binding(5) var fx_tex9: texture_2d<f32>;
@group(3) @binding(6) var fx_samp9: sampler;
@group(3) @binding(7) var fx_tex10: texture_2d<f32>;
@group(3) @binding(8) var fx_samp10: sampler;

struct VsOut {
    @builtin(position) @invariant clip: vec4<f32>,   // CHAR PREPASS (2026-10-06): the pre-pass (fs_char_depth) and the base pass (fs_char, depth Equal) must rasterize the same depth
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) world_tangent: vec4<f32>,
}

@vertex
fn vs_char(
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) joints: vec4<u32>,
    @location(4) weights: vec4<f32>,
    @location(6) tangent: vec4<f32>,
) -> VsOut {
    var o: VsOut;
    let m = bones[joints.x] * weights.x + bones[joints.y] * weights.y + bones[joints.z] * weights.z + bones[joints.w] * weights.w;
    let p_local = (m * vec4<f32>(pos, 1.0)).xyz;
    var n_local = (m * vec4<f32>(normal, 0.0)).xyz;
    n_local = n_local / max(length(n_local), 1e-6);
    let wp = object.model * vec4<f32>(p_local, 1.0);
    o.clip = camera.view_proj * wp;
    o.world_pos = wp.xyz;
    o.world_normal = (object.model * vec4<f32>(n_local, 0.0)).xyz;
    o.uv = uv;
    o.world_tangent = vec4<f32>((object.model * vec4<f32>((m * vec4<f32>(tangent.xyz, 0.0)).xyz, 0.0)).xyz, tangent.w);
    return o;
}

// renderer world (m, Y up) <-> Unreal world (cm, Z up)
fn to_ue(r: vec3<f32>) -> vec3<f32> { return vec3<f32>(r.x, r.z, r.y); }
fn from_ue(u: vec3<f32>) -> vec3<f32> { return vec3<f32>(u.x, u.z, u.y); }

struct GBuf { emissive: vec3<f32>, normal_ue: vec3<f32>, metallic: f32, specular: f32, roughness: f32, shading_model: f32, base: vec3<f32>, ao: f32, custom: vec4<f32> }   // custom: GBufferD (o5, SHADING MODELS)

fn char_run(perm: u32, v: array<vec4<f32>, 8>, tw: vec3<f32>, cam_ue: vec3<f32>, time: f32) -> GBuf {
    var o: array<vec4<f32>, 6>;
    // head / hair declare 7 inputs (no front-face flag read), body 8
    let pf4 = vec4<f32>((1.5 + sin((time - floor(time / 0.5) * 0.5) * 12.566371)) * 0.4, 0.0, 0.0, 0.0);
    var v7: array<vec4<f32>, 7>;
    for (var i = 0; i < 7; i = i + 1) { v7[i] = v[i]; }
    // PERF (2026-10-06): the programs' constant-buffer arrays are folded at load (char_material.rs fold_private_cbs): the written slots are the private vec4s
    // `<cb>_<slot>` below, the material registers (cb3) are read from `cmat.cb` directly, every other slot is the zero it always was
    if (perm == 0u) {
        head_cb0_3 = pf4;
        head_cb1_43 = vec4<f32>(tw, 1.0);
        head_cb1_62 = vec4<f32>(-cam_ue, 0.0);
        head_cb1_131 = vec4<f32>(0.0, 0.0, time, 0.0);
        o = head_ps(v7);
    } else if (perm == 1u) {
        hair_cb0_3 = pf4;
        hair_cb1_43 = vec4<f32>(tw, 1.0);
        hair_cb1_62 = vec4<f32>(-cam_ue, 0.0);
        hair_cb1_131 = vec4<f32>(0.0, 0.0, time, 0.0);
        o = hair_ps(v7);
    } else {
        body_cb0_3 = pf4;
        body_cb1_43 = vec4<f32>(tw, 1.0);
        body_cb1_62 = vec4<f32>(-cam_ue, 0.0);
        body_cb1_130 = vec4<f32>(0.0, 0.0, 0.0, 1.0);   // View.CullingSign (1 for the main view)
        body_cb1_131 = vec4<f32>(0.0, 0.0, time, 0.0);
        body_cb2_9 = vec4<f32>(0.0, 0.0, 0.0, 1.0);     // Primitive.LocalToWorldDeterminantSign (the UE mesh is not mirrored)
        o = body_ps(v);
    }
    var g: GBuf;
    g.emissive = o[0].xyz;
    g.normal_ue = normalize(o[1].xyz * 2.0 - 1.0);
    g.metallic = o[2].x;
    g.specular = o[2].y;
    g.roughness = o[2].z;
    g.shading_model = o[2].w;
    g.base = o[3].xyz;
    g.ao = o[3].w;
    g.custom = o[5];
    return g;
}

// CHAR PREPASS (2026-10-06): the depth pre-pass draw = the material's depth-only pixel shader (FDepthOnlyPS of TGPUSkinVertexFactorytrue, char_depth_gen.wgsl): the
// BLEND_Masked coverage clip (OpacityMask - 0.3333 < 0 discards), nothing else. Inputs as the base pass feeds them: v[3].xy UV0, v[5] SV_Position (the View's
// SvPositionToTranslatedWorld rows 40..43 = (0, 0, 0, translated world) give the interpolated position, as in fs_char), v[6].x the front-face flag (not read);
// v[0..2] / v[4] (tangent basis, vertex colour, PixelPosition) are declared but not read by the cooked program. View = dcb0 (PreViewTranslation 62), Material = dcb1 (cmat).
@fragment
fn fs_char_depth(in: VsOut) {
    let tw = (to_ue(in.world_pos) - to_ue(camera.position.xyz)) * 100.0;
    var v: array<vec4<f32>, 6>;
    v[3] = vec4<f32>(in.uv, 0.0, 0.0);
    v[5] = in.clip;
    let perm = cmat.perm_info.x;
    // PERF (2026-10-06): folded constant buffers as in char_run (dcb1 = `cmat.cb`)
    if (perm == 0u) {
        head_dcb0_43 = vec4<f32>(tw, 1.0);
        head_dcb0_62 = vec4<f32>(-to_ue(camera.position.xyz) * 100.0, 0.0);
        _ = head_depth_ps(v);
    } else if (perm == 1u) {
        hair_dcb0_43 = vec4<f32>(tw, 1.0);
        hair_dcb0_62 = vec4<f32>(-to_ue(camera.position.xyz) * 100.0, 0.0);
        _ = hair_depth_ps(v);
    } else {
        body_dcb0_43 = vec4<f32>(tw, 1.0);
        body_dcb0_62 = vec4<f32>(-to_ue(camera.position.xyz) * 100.0, 0.0);
        _ = body_depth_ps(v);
    }
}

fn char_gbuffer(in: VsOut, front: bool) -> GBuf {
    let cam = camera.position.xyz;
    let p = in.world_pos;
    let n0 = normalize(in.world_normal);
    // tangent frame of the renderer (screen-space derivatives): same maths as perturb_normal; replaced by the cooked TangentX / TangentZ.w stream when the mesh carries one
    let dp1 = dpdx(p);
    let dp2 = gl_dfdy3(p);
    let duv1 = dpdx(in.uv);
    let duv2 = gl_dfdy2(in.uv);
    let dp2perp = cross(dp2, n0);
    let dp1perp = cross(n0, dp1);
    let t_r = dp2perp * duv1.x + dp1perp * duv2.x;
    let b_r = dp2perp * duv1.y + dp1perp * duv2.y;
    let n_u = normalize(to_ue(n0));
    let t_u = normalize(to_ue(t_r) - n_u * dot(n_u, to_ue(t_r)));
    // the shader builds the bitangent as cross(Z, X) * sign: pick the sign that reproduces the renderer's bitangent direction
    var w_sign = select(-1.0, 1.0, dot(cross(n_u, t_u), to_ue(b_r)) >= 0.0);
    var t_v = t_u;
    if (in.world_tangent.w != 0.0) {
        t_v = to_ue(in.world_tangent.xyz);
        w_sign = in.world_tangent.w;
    }
    let tw = (to_ue(p) - to_ue(cam)) * 100.0;
    var v: array<vec4<f32>, 8>;
    v[0] = vec4<f32>(t_v, 0.0);
    v[1] = vec4<f32>(n_u, w_sign);
    v[2] = vec4<f32>(1.0);
    v[3] = vec4<f32>(in.uv, 0.0, 0.0);
    v[4] = vec4<f32>(0.0, 0.0, 0.0, 1.0);   // VELOCITY_PREV_POS (feeds only the velocity target o4, which is not used)
    v[6] = in.clip;   // SV_Position: the VsOut @builtin(position) is the fragment coordinate (must appear only once)
    v[7] = vec4<f32>(bitcast<f32>(select(0u, 1u, front)), 0.0, 0.0, 0.0);
    return char_run(cmat.perm_info.x, v, tw, to_ue(cam) * 100.0, camera.position.w);
}
@fragment
fn fs_char(in: VsOut, @builtin(front_facing) front: bool) -> LitOut {
    let p = in.world_pos;
    let g = char_gbuffer(in, front);

    // SHADING MODELS (2026-10-05): the deferred resolve of the GBuffer the original program wrote, per ShadingModelID (o2.w) with its CustomData (o5), shared with
    // the scene-lit FX programs (fx_resolve_sm.wgsl sm_lit_world: DeferredLightingCommon.usf GetDynamicLighting for the sun x the projected CSM / DF shadow x the
    // light function, SkyLighting.usf sky diffuse with DFAO + GBufferAO, ReflectionApplyPS inputs, fog). The head / hair / body materials are
    // MSM_SubsurfaceProfile (docs/spec/material_coverage.md): StandardShading on the stored BaseColor, which is what UE computes without the screen-space subsurface
    // pass (subsurface.rs replaces these pixels when r.SSS.Scale > 0: fs_char_sss writes its inputs). Was a local DefaultLit copy without the directional light's MinRoughness clamp (0.08) and F_Schlick's
    // saturate(50 * SpecularColor.g) term.
    return sm_lit_world(p, from_ue(g.normal_ue), g.emissive, g.base, g.metallic, g.specular, g.roughness, g.ao, g.shading_model, g.custom);
}
@fragment
fn fs_char_sss(in: VsOut, @builtin(front_facing) front: bool) -> SssOut {
    let g = char_gbuffer(in, front);
    if (sm_decode_id(g.shading_model) != SM_SUBSURFACE_PROFILE || light.misc.x <= 0.5) { discard; }
    return sm_sss_world(in.world_pos, from_ue(g.normal_ue), g.emissive, g.base, g.metallic, g.specular, g.roughness, g.ao, g.custom);
}
