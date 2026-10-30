// SHADING MODELS (2026-10-05, foliage / character materials): the per-shading-model terms of UE4.16's deferred lighting that DefaultLit does not have, for
// the GBuffer an ORIGINAL base-pass pixel program wrote (fx_resolve_sm.wgsl `fx_scene_lit_sm`, char_scene.wgsl). Appended after common.wgsl + lit_common.wgsl.
//
// Sources (UE4.16):
//   DeferredShadingCommon.usf:259-302  SHADINGMODELID_*, DecodeShadingModelId, IsSubsurfaceModel; :639 ExtractSubsurfaceColor = Square(CustomData.rgb)
//   ShadingModels.usf:606-638          SubsurfaceShadingSubsurface / SubsurfaceShadingTwoSided; :704-746 SurfaceShading (Standard for 1, 2, 3, 5, 6) / SubsurfaceShading
//   DeferredLightingCommon.usf:239-274 GetShadowTerms (directional: SSSShadowTerm = min(lerp(LightAttenuation.y, static, fade), .w) * LightFunction)
//   DeferredLightingCommon.usf:518-536 surface = SurfaceShading * LightColor * NoL * SurfaceShadow; subsurface = SubsurfaceShading * LightColor * SubsurfaceShadow
//   ShadowProjectionPixelShader.usf:84-193 + ShadowFilteringCommon.usf:174-185  the whole-scene shadow's SSS transmission (LightAttenuation.y)
//   SkyLighting.usf:96-108            sky diffuse: TWOSIDED_FOLIAGE adds GetSkySHDiffuse(-N) * SkyLightColor * SubsurfaceColor * SkyVisibility;
//                                     SUBSURFACE / PREINTEGRATED_SKIN add SubsurfaceColor to DiffuseColor
// GBuffer of the base pass (BasePassPixelShader.usf:1036-1096, cooked r.BasePassOutputsVelocity=True, r.SelectiveBasePassOutputs=True): SV_Target4 = velocity,
// SV_Target5 = GBufferD = CustomData (written only by the models that use it: WRITES_CUSTOMDATA_TO_GBUFFER).


// ---- the whole-scene directional shadow's subsurface transmission (LightAttenuation.y) ----
// ShadowProjectionPixelShader.usf:132-168 for IsSubsurfaceModel pixels: Density = -.05 * log(1 - min(Opacity, .999)) (HAIR / EYE: Opacity = Density = 1);
// ManualPCF with Settings.bSubsurface: SceneDepth = LightSpacePixelDepthForSSS (NOT clamped) + ProjectionDepthBiasParameters.x, DensityMulConstant =
// Density * ProjectionDepthBiasParameters.y, CalculateOcclusion = ShadowmapDepth > .99 ? 1 : saturate(exp(-max(SceneDepth - (ShadowmapDepth - Bias.x), 0) * DensityMul)).
// ProjectionDepthBiasParameters = (GetShaderDepthBias(), MaxSubjectZ - MinSubjectZ) (ShadowRendering.h:1392): x is the cascade's ShaderDepthBias
// (light.cascade_bias, the bias the depth pass added), y the cascade's depth range in UE cm = 100 / |d(depth)/d(world m)|, read off the cascade's own matrix.
// No ShadowSharpen on this term; FadedSSSShadow = Square(SSSTransmission) is stored sqrt-encoded (= SSSTransmission) and read back squared, like the opaque term.
fn csm_sss_tap(layer: i32, t: vec2<i32>, scene: f32, bias: f32, density_mul: f32, size: i32) -> f32 {
    if (t.x < 0 || t.y < 0 || t.x >= size || t.y >= size) { return 1.0; }
    let d = textureLoad(shadow_map, t, layer, 0);
    if (d > 0.99) { return 1.0; }
    return clamp(exp(-max(scene - (d - bias), 0.0) * density_mul), 0.0, 1.0);
}
fn csm_sss_pcf(layer: i32, uv: vec2<f32>, scene: f32, bias: f32, density_mul: f32) -> f32 {
    let size = i32(light.misc.z);
    let q = i32(light.shadow_params.z);
    if (q <= 1) { return csm_sss_tap(layer, vec2<i32>(floor(uv * light.misc.z)), scene, bias, density_mul, size); }
    let h = min(q - 1, 3);
    let tp = uv * light.misc.z - 0.5;
    let fr = fract(tp);
    let tc = vec2<i32>(floor(tp));
    var sum = 0.0;
    // PERF (2026-10-06): gathered 2x2 blocks (lit_common.wgsl csm_gather: the outside texels read 1.0, which csm_sss_tap's d > .99 rule turns into 1)
    for (var by = 1 - h; by <= h; by = by + 2) {
        let wy0 = csm_pcf_weight(by, h, fr.y);
        let wy1 = csm_pcf_weight(by + 1, h, fr.y);
        for (var bx = 1 - h; bx <= h; bx = bx + 2) {
            let wx0 = csm_pcf_weight(bx, h, fr.x);
            let wx1 = csm_pcf_weight(bx + 1, h, fr.x);
            let d = csm_gather(layer, tc + vec2<i32>(bx, by), size);
            let t = clamp(exp(-max(vec4<f32>(scene) - (d - vec4<f32>(bias)), vec4<f32>(0.0)) * density_mul), vec4<f32>(0.0), vec4<f32>(1.0));
            let c = select(t, vec4<f32>(1.0), d > vec4<f32>(0.99));
            sum = sum + wx0 * wy1 * c.x + wx1 * wy1 * c.y + wx1 * wy0 * c.z + wx0 * wy0 * c.w;
        }
    }
    let taps = f32(2 * h - 1);
    return sum / (taps * taps);
}
fn cascade_sss(layer: i32, world_pos: vec3<f32>, density: f32) -> f32 {
    let m = light.shadow_matrix[layer];
    let c = m * vec4<f32>(world_pos, 1.0);
    let range_cm = 100.0 / max(length(vec3<f32>(m[0].z, m[1].z, m[2].z)), 1e-9);
    let bias = light.cascade_bias[layer];
    return csm_sss_pcf(layer, c.xy, c.z + bias, bias, density * range_cm);
}
// The subsurface counterpart of lit_common.wgsl sun_shadow + sun_visibility: same cascade selection / fade-region blend, Square, ray-traced DF cascade (it writes
// all four attenuation channels with BO_Min, DistanceFieldShadowing.usf:666), whole-scene distance fade and light function (GetShadowTerms SSSShadowTerm).
fn sun_sss_visibility(world_pos: vec3<f32>, id: u32, opacity_in: f32) -> f32 {
    var opacity = opacity_in;
    var density = -0.05 * log(1.0 - min(opacity, 0.999));
    if (id == SM_HAIR || id == SM_EYE) { opacity = 1.0; density = 1.0; }
    let depth = -(camera.view * vec4<f32>(world_pos, 1.0)).z;
    let far_all = light.cascade_far.w;
    var s: f32 = 1.0;
    if (light.shadow_params.x > 0.5 && depth < far_all) {
        let n_csm = i32(light.shadow_params.y);
        for (var i = 0; i < n_csm; i = i + 1) {
            let e = light.cascade_far[i];
            let ext = light.cascade_ext[i];
            if (depth < e) { s = cascade_sss(i, world_pos, density); break; }
            if (depth < e + ext) {
                var next = 1.0;
                if (i + 1 < n_csm) { next = cascade_sss(i + 1, world_pos, density); }
                s = mix(next, cascade_sss(i, world_pos, density), 1.0 - clamp((depth - e) / ext, 0.0, 1.0));
                break;
            }
        }
        s = s * s;
        let depth_cm = depth * 100.0;
        // PERF (2026-10-06): the ray-traced cascade of this pixel was already evaluated by sun_shadow (lit_common.wgsl: g_df_vis = the upsampled
        // lerp(1, DF, NearBlendFactor) at the same pixel / depth; every caller evaluates sun_visibility first, sm_lit_world). UE writes the one DF result into the
        // attenuation buffer that both the opaque and the subsurface terms read (CW_RG + BO_Min, ShadowRendering.cpp:420-424).
        if (dfu.sh0.x > 0.5 && depth_cm > dfu.sh0.y && depth_cm < dfu.sh0.z) { s = min(s, g_df_vis); }
        let near = far_all - far_all * light.cascade_ext.w;
        let fade = clamp((depth - near) / max(far_all - near, 0.0001), 0.0, 1.0);
        s = mix(s, 1.0, fade * fade);
    }
    return s * light_function(world_pos);
}

// The sun's subsurface light of a GBuffer pixel: SubsurfaceShading * LightColor * SubsurfaceShadow (DeferredLightingCommon.usf:528-533). light.sun_radiance
// carries the Lambert 1/pi (LightColor / pi), hence the pi.
fn sm_sun_subsurface(id: u32, custom: vec4<f32>, gbuffer_ao: f32, world_pos: vec3<f32>, n: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    if (id != SM_SUBSURFACE && id != SM_TWOSIDED_FOLIAGE) { return vec3<f32>(0.0); }
    let l = light.sun_dir.xyz;
    let t = sm_subsurface_shading(id, custom, gbuffer_ao, l, v, n);
    if (all(t <= vec3<f32>(0.0))) { return vec3<f32>(0.0); }
    return light.sun_radiance.xyz * 3.14159265 * t * sun_sss_visibility(world_pos, id, custom.a);
}
// Sky diffuse inputs of a subsurface model (SkyLighting.usf:96-108): the DiffuseColor the sky term multiplies (SUBSURFACE / PREINTEGRATED_SKIN add the
// subsurface colour) and the TWOSIDED_FOLIAGE back-face term GetSkySHDiffuse(-N) * SkyLightColor * SubsurfaceColor, which is scaled by the same SkyVisibility
// (DFAO, GBufferAO, SSAO) as the front term: lit_common.wgsl output_hdr adds it to the occluded ambient through `g_sky_back`.
fn sm_sky_diffuse_color(id: u32, diffuse_color: vec3<f32>, custom: vec4<f32>) -> vec3<f32> {
    if (id == SM_SUBSURFACE || id == SM_PREINTEGRATED_SKIN) { return diffuse_color + sm_subsurface_color(custom); }
    return diffuse_color;
}
fn sm_sky_back(id: u32, custom: vec4<f32>, n: vec3<f32>) -> vec3<f32> {
    if (id != SM_TWOSIDED_FOLIAGE) { return vec3<f32>(0.0); }
    return sky_lookup(-n) * sm_subsurface_color(custom);
}
