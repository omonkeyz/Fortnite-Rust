// SHADING MODELS (2026-10-05): the deferred resolve of a GBuffer an ORIGINAL base-pass pixel program wrote, per ShadingModelID (GBufferB.a) with its CustomData
// (GBufferD): the directional light of DeferredLightingCommon.usf GetDynamicLighting (:518-536: SurfaceShading = StandardShading for DEFAULT_LIT / SUBSURFACE /
// PREINTEGRATED_SKIN / SUBSURFACE_PROFILE / TWOSIDED_FOLIAGE, + SubsurfaceShading x the subsurface shadow term) and the sky light diffuse of
// SkyLighting.usf:96-108. The CSM / DF shadow, light function, DFAO, sky reflection inputs and fog are fx_resolve.wgsl `fx_scene_lit`'s (same helpers, same
// order); with ShadingModelID DEFAULT_LIT the result is fx_scene_lit's. Appended after fx_resolve.wgsl and shading_models.wgsl.
// SUBSURFACE_PROFILE is lit as StandardShading with the stored BaseColor / Specular: what DecodeGBufferData does when View.bSubsurfacePostprocessEnabled is 0
// (DeferredShadingCommon.usf:485-505). The screen-space subsurface pass (PostProcessSubsurface.cpp) runs for the character (subsurface.rs, `sm_sss_world` below).
// CLEAR_COAT / CLOTH / EYE / HAIR surface terms and PREINTEGRATED_SKIN's BRDF texture are not implemented (no audited material uses them,
// docs/spec/material_coverage.md); UNLIT never reaches this (lit tails only).
fn sm_lit_world(world: vec3<f32>, n_in: vec3<f32>, emissive: vec3<f32>, base: vec3<f32>, metallic: f32, specular: f32, roughness_in: f32, gbuffer_ao: f32, gbuffer_b_a: f32, custom: vec4<f32>) -> LitOut {
    let cam = camera.position.xyz;
    if (light.misc.x <= 0.5) { return lit_out(output_hdr(emissive + base, vec3<f32>(0.0), world, cam)); }
    let id = sm_decode_id(gbuffer_b_a);
    let n = normalize(n_in);
    let v = normalize(cam - world);
    let l = light.sun_dir.xyz;
    let ndl = dot(n, l);
    // DecodeGBufferData: DiffuseColor = BaseColor - BaseColor * Metallic, SpecularColor = lerp(0.08 * Specular, BaseColor, Metallic)
    let diffuse_color = base * (1.0 - metallic);
    let spec_color = mix(vec3<f32>(0.08 * specular), base, metallic);
    let roughness = clamp(roughness_in, 0.0, 1.0);
    let vis = sun_visibility(world, n, max(ndl, 0.0));
    // sky diffuse: DiffuseColor (+ SubsurfaceColor for SUBSURFACE / PREINTEGRATED_SKIN) x Lookup(N); TWOSIDED_FOLIAGE's back-face term through g_sky_back
    let amb = sm_sky_diffuse_color(id, diffuse_color, custom) * ambient_light(n);
    g_sky_back = sm_sky_back(id, custom, n);
    var lit = emissive + diffuse_color * light.sun_radiance.xyz * max(ndl, 0.0) * vis + amb;
    if (ndl > 0.0) {
        lit = lit + light.sun_radiance.xyz * 3.14159265 * ggx_spec_color(n, v, l, max(roughness, FX_SUN_MIN_ROUGHNESS), spec_color) * vis;
    }
    // SubsurfaceShading x LightColor x SubsurfaceShadow (the transmission through the shadow map, not the opaque shadow)
    lit = lit + sm_sun_subsurface(id, custom, gbuffer_ao, world, n, v);
    lit = lit + local_lighting(world, n, v, diffuse_color, spec_color, roughness, id, custom, gbuffer_ao);
    reflection_inputs(n, v, world, spec_color, roughness, gbuffer_ao);
    if (dbg_part_any()) { dbg_terms(diffuse_color, n, ndl, vis, select(vec3<f32>(0.0), light.sun_radiance.xyz * 3.14159265 * ggx_spec_color(n, v, l, max(roughness, FX_SUN_MIN_ROUGHNESS), spec_color) * vis, ndl > 0.0)); }
    return lit_out(output_hdr(lit, amb, world, cam));
}
// The scene-lit FX tail (fn-fx program.rs, programs that write GBufferD): the pixel's world position from SV_Position like fx_scene_lit.
fn fx_scene_lit_sm(frag: vec4<f32>, n_in: vec3<f32>, emissive: vec3<f32>, base: vec3<f32>, metallic: f32, specular: f32, roughness_in: f32, gbuffer_ao: f32, gbuffer_b_a: f32, custom: vec4<f32>) -> LitOut {
    return sm_lit_world(fx_scene_world(frag), n_in, emissive, base, metallic, specular, roughness_in, gbuffer_ao, gbuffer_b_a, custom);
}
struct SssOut {
    @location(0) lit: vec4<f32>,
    @location(1) sky: vec4<f32>,
    @location(2) base: vec4<f32>,
    @location(3) fog: vec4<f32>,
}
fn sss_luma(c: vec3<f32>) -> f32 { return dot(c, vec3<f32>(0.3, 0.59, 0.11)); }
fn sm_sss_world(world: vec3<f32>, n_in: vec3<f32>, emissive: vec3<f32>, base: vec3<f32>, metallic: f32, specular: f32, roughness_in: f32, gbuffer_ao: f32, custom: vec4<f32>) -> SssOut {
    let cam = camera.position.xyz;
    let n = normalize(n_in);
    let v = normalize(cam - world);
    let l = light.sun_dir.xyz;
    let ndl = dot(n, l);
    let diffuse_w = vec3<f32>(1.0 - metallic);
    let spec_color = mix(vec3<f32>(0.08 * specular), base, metallic);
    let roughness = clamp(roughness_in, 0.0, 1.0);
    let vis = sun_visibility(world, n, max(ndl, 0.0));
    let sun_irr = light.sun_radiance.xyz * max(ndl, 0.0) * vis;
    var lit = emissive + diffuse_w * sun_irr;
    if (ndl > 0.0) {
        lit = lit + light.sun_radiance.xyz * 3.14159265 * ggx_spec_color(n, v, l, max(roughness, FX_SUN_MIN_ROUGHNESS), spec_color) * vis;
    }
    var dl = sss_luma(sun_irr);
    lit = lit + local_lighting(world, n, v, diffuse_w, spec_color, roughness, SM_SUBSURFACE_PROFILE, custom, gbuffer_ao);
    dl = dl + sss_luma(g_ll_scatter);
    let amb = diffuse_w * ambient_light(n);
    g_refl_ao = gbuffer_ao;
    let sky_scaled = df_sky_occlusion(amb, world);
    lit = lit + g_sky_tint;
    dl = dl + sss_luma(g_sky_tint);
    var fog = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    if (light.misc.w > 0.5) { fog = height_fog(light.fog, world, cam, light.sun_dir.xyz, -camera.view_inverse[2].xyz); }
    let pid = round(clamp(custom.r, 0.0, 1.0) * 255.0);
    var o: SssOut;
    o.lit = vec4<f32>(lit, dl);
    o.sky = vec4<f32>(sky_scaled, gbuffer_ao);
    o.base = vec4<f32>(base, pid + 1.0 + min(custom.a, 0.999));
    o.fog = fog;
    return o;
}

