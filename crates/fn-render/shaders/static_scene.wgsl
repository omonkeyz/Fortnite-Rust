// Static textured meshes in the HDR scene pass. Prepended: common.wgsl, lit_common.wgsl (globals, shadows, fog tail), static_common.wgsl.
// Shading = the prop path of scene.wgsl (sun diffuse x cascade visibility + sky hemisphere ambient with the ambient share in HDR alpha, height fog)
// with a base colour texture, optional normal map, alpha test, two-sided lighting and a simple GGX sun specular.

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) fade: f32,
}

@vertex
fn vs_static(@location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) uv: vec2<f32>, @location(3) color: vec4<f32>, inst: InstIn) -> VsOut {
    let sp = static_transform(pos, normal, color, inst);
    var o: VsOut;
    o.clip = camera.view_proj * vec4<f32>(sp.world, 1.0);
    o.world_pos = sp.world;
    o.world_normal = sp.normal;
    o.uv = uv;
    o.color = color;
    o.fade = inst.rot_fade.z;
    return o;
}

override EARLY_Z_MASKING: bool = false; // PREPASS
// Interleaved gradient noise (Jimenez): cheap, temporally stable screen-door pattern for the distance fade.
fn ign(p: vec2<f32>) -> f32 { return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715)))); }

@fragment
fn fs_static(in: VsOut) -> LitOut {
    let cam = camera.position.xyz;
    let tex = textureSample(t_base, s_static, in.uv);
    // Mip-aware alpha test: the CPU mip chain rescales the alpha of every level to keep the level-0 coverage at the cutoff (static_mesh.rs),
    // so a plain compare does not thin out or shimmer with distance.
    // PREPASS: with the depth pre-pass the clip tests ran there (cooked r.EarlyZPassOnlyMaterialMasking = True: the mask is evaluated only in the early Z
    // pass, the base pass then draws with CF_Equal and no clip, BasePassRendering.cpp / DepthRendering.cpp); EARLY_Z_MASKING = true on the lit pipelines that follow the pre-pass (statics.rs).
    if (!EARLY_Z_MASKING) {
    if (smat.p0.y > 0.5 && tex.a < smat.p0.x) { discard; }
    if (in.fade < 0.999 && ign(in.clip.xy) > in.fade) { discard; }
    }

    var albedo = tex.rgb * smat.tint.rgb;
    if (smat.p2.y > 0.5) { albedo = albedo * in.color.rgb; }
    let two_sided = smat.p0.z > 0.5;
    let v = normalize(cam - in.world_pos);
    var n = normalize(in.world_normal);
    // Two-sided surfaces light the side that faces the viewer (independent of the exported winding): flip the normal on back faces.
    if (two_sided && dot(n, v) < 0.0) { n = -n; }
    if (smat.p0.w > 0.5) {
        n = perturb_normal(n, in.world_pos, in.uv, unpack_normal(textureSample(t_nrm, s_static, in.uv)));
    }
    let sun = light.sun_dir.xyz;
    let ndl = dot(n, sun);
    var lit = albedo;
    var amb = vec3<f32>(0.0);
    if (light.misc.x > 0.5) {
        // Receiver lookups offset towards the sun side of the surface (so a leaf lit from behind does not shadow itself); the slope bias
        // uses |n.l|.
        let n_sun = select(-n, n, ndl >= 0.0);
        let front = max(ndl, 0.0);
        var back = 0.0;
        if (two_sided) { back = max(-ndl, 0.0) * smat.tint.a; }
        let vis = sun_visibility(in.world_pos, n_sun, abs(ndl));
        amb = albedo * ambient_light(n);
        lit = albedo * light.sun_radiance.xyz * (front + back) * vis + amb;
        // a dielectric, SpecularColor = 0.08 * Specular (the material's Specular, p1.y), roughness p1.x. The sun lobe uses UE's F_Schlick on that colour
        // (lit_common.wgsl ggx_spec_color; was F0 0.04 with the whole lobe scaled by Specular, i.e. the grazing term too)
        let spec_color = vec3<f32>(0.08 * smat.p1.y);
        if (ndl > 0.0) {
            lit = lit + light.sun_radiance.xyz * 3.14159265 * ggx_spec_color(n, v, sun, smat.p1.x, spec_color) * vis;
        }
        lit = lit + local_lighting(in.world_pos, n, v, albedo, spec_color, smat.p1.x, SM_DEFAULT_LIT, vec4<f32>(0.0), 1.0);
        // SKY REFLECTION HOOK (lit_common.wgsl reflection_inputs, applied by the deferred reflection pass)
        reflection_inputs(n, v, in.world_pos, spec_color, smat.p1.x, 1.0);
    }
    return lit_out(output_hdr(lit, amb, in.world_pos, cam));
}

// PREPASS (UE4.16 FDeferredShadingSceneRenderer::RenderPrePass, cooked RendererSettings r.EarlyZPass = 2 = opaque AND masked; DepthRendering.cpp
// TDepthOnlyVS / FDepthOnlyPS): every static mesh draws its depth first, with the clip tests of the lit pass (opacity mask, distance-fade dither); the lit
// pipelines then test CF_Equal without clip (r.EarlyZPassOnlyMaterialMasking = True). The pre-pass runs vs_static itself (same vertex stage = same depth).
@fragment
fn fs_static_depth(in: VsOut) {
    let tex = textureSample(t_base, s_static, in.uv);
    if (smat.p0.y > 0.5 && tex.a < smat.p0.x) { discard; }
    if (in.fade < 0.999 && ign(in.clip.xy) > in.fade) { discard; }
}

// OUTLINE (custom depth): `ABuildingActor` vfunc_259 @1404823b0 turns on the components' custom depth with the custom stencil value of
// `SelectionOutlineID_CodeReferenced`; UE4.16 draws them depth-only (clip tests of the material, no distance-fade dither) into the CustomDepth target and
// writes the stencil value. The port keeps the stencil in an Rg8Uint colour target (the post program reads `.y`, the X24_G8 stencil view's channel).
override CUSTOM_STENCIL: f32 = 0.0;
@fragment
fn fs_static_custom(in: VsOut) -> @location(0) vec4<u32> {
    let tex = textureSample(t_base, s_static, in.uv);
    if (smat.p0.y > 0.5 && tex.a < smat.p0.x) { discard; }
    let s = u32(CUSTOM_STENCIL);
    return vec4<u32>(s, s, 0u, 0u);
}
