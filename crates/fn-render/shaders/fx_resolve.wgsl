// FX RECEIVE (2026-10-05): resolve of the deferred GBuffer the ORIGINAL base-pass pixel program of an opaque / masked lit FX material writes (building pieces,
// lit debris, props drawn through the FX pass). Appended after common.wgsl + lit_common.wgsl (whose group 0 is re-numbered to group 3 by fx.rs
// `FxRenderer::scene_lit_prelude`, the DXBC resources own groups 0..2), so the sun visibility (CSM PCF, DF shadow, light function), the sky SH diffuse with the
// DFAO bent normal, the sky reflection inputs (deferred ReflectionApplyPS + SSR) and the fog / HDR tail are the SAME helpers the static meshes use
// (static_scene.wgsl fs_static). UE4.16 draws these materials in the deferred base pass and lights them from the GBuffer: the directional light
// (DeferredLightPixelShaders.usf, shadowed by the projected whole-scene shadow, ShadowProjectionPixelShader.usf), the movable sky light's diffuse
// (SkyLighting.usf SkyLightDiffusePS, after DFAO) and its specular (ReflectionEnvironmentShaders.usf ReflectionApplyPS).
//
// GBuffer (DeferredShadingCommon.usf:417-442 EncodeGBuffer, r.AllowStaticLighting=0 in the 1.8 CookedIniVersion.txt -> GBufferC.a = GBufferAO):
//   SV_Target1 = GBufferA (normal * 0.5 + 0.5), SV_Target2 = GBufferB (Metallic, Specular, Roughness, ShadingModelID), SV_Target3 = GBufferC (BaseColor, GBufferAO),
//   SV_Target0 = scene colour = emissive (the `TBasePassPSFNoLightMapPolicy` permutation: a movable sky light is not in the deferred opaque base pass,
//   ScenePrivate.h:2269-2273 ShouldRenderSkylightInBasePass_Internal; its diffuse is added here like SkyLightDiffusePS).

// The pixel's world position (renderer metres, Y up) from its SV_Position. The FX programs' clip W is the view-space depth in UE cm (fn-fx view.rs `Clip`: the W
// row is ViewForward on the translated world, no projection jitter); WebGPU hands over 1 / W. The ray is the camera's (camera.proj = tan half fov, the FX
// view uses the same frustum and the same view-size target).
fn fx_scene_world(frag: vec4<f32>) -> vec3<f32> {
    g_frag_xy = frag.xy;   // DF SHADOW SCREEN PASS: the pixel the distance-field shadow upsample reads at (lit_common.wgsl df_shadow_upsample)
    let d = 0.01 / frag.w;
    let ndc = vec2<f32>(frag.x * camera.viewport.z * 2.0 - 1.0, 1.0 - frag.y * camera.viewport.w * 2.0);
    return camera.position.xyz + (camera.view_inverse * vec4<f32>(ndc.x * camera.proj.x * d, ndc.y * camera.proj.y * d, -d, 0.0)).xyz;
}

// `MinRoughness` of the directional light's specular lobe: DeferredLightingCommon.usf:164 AreaLightSpecular `LobeRoughness = max(LobeRoughness, MinRoughness)`,
// FDirectionalLightSceneProxy `LightMinRoughness = max(MinRoughness, .02)` (DirectionalLightComponent.cpp:180) with ULightComponent::MinRoughness = 0.08
// (LightComponent.cpp:349, the class default; the Athena sun does not override it in the port's data).
const FX_SUN_MIN_ROUGHNESS: f32 = 0.08;

fn fx_scene_lit(frag: vec4<f32>, n_in: vec3<f32>, emissive: vec3<f32>, base: vec3<f32>, metallic: f32, specular: f32, roughness_in: f32, gbuffer_ao: f32) -> LitOut {
    let cam = camera.position.xyz;
    let world = fx_scene_world(frag);
    if (light.misc.x <= 0.5) { return lit_out(output_hdr(emissive + base, vec3<f32>(0.0), world, cam)); }
    let n = normalize(n_in);
    let v = normalize(cam - world);
    let l = light.sun_dir.xyz;
    let ndl = dot(n, l);
    // DiffuseColor / SpecularColor of the GBuffer (DeferredShadingCommon.usf DecodeGBuffer: BaseColor - BaseColor * Metallic, ComputeF0(Specular, BaseColor, Metallic))
    let diffuse_color = base * (1.0 - metallic);
    let spec_color = mix(vec3<f32>(0.08 * specular), base, metallic);
    let roughness = clamp(roughness_in, 0.0, 1.0);
    // the sun: the projected whole-scene shadow x the light function (lit_common.wgsl sun_visibility, the static meshes' helper)
    let vis = sun_visibility(world, n, max(ndl, 0.0));
    // the sky light diffuse: GetSkySHDiffuse(N) * SkyLightColor * DiffuseColor (SkyLighting.usf:134-136); output_hdr applies the DFAO bent-normal occlusion to
    // `amb` (df_sky_occlusion) and the AO pass the screen-space AO. `amb` carries NO AO: df_sky_occlusion applies GBufferAO (g_refl_ao, set by reflection_inputs
    // below before output_hdr) to the AO-dependent part only, the AO pass rescales that share to min(GBufferAO, SSAO) (SkyLighting.usf:88), and the
    // OcclusionTint term stays AO-free (:134).
    let amb = diffuse_color * ambient_light(n);
    var lit = emissive + diffuse_color * light.sun_radiance.xyz * max(ndl, 0.0) * vis + amb;
    if (ndl > 0.0) {
        // sun_radiance carries the Lambert 1/pi; the GGX lobe (lit_common.wgsl ggx_spec_color, UE F_Schlick on SpecularColor) gets sun_radiance * pi
        lit = lit + light.sun_radiance.xyz * 3.14159265 * ggx_spec_color(n, v, l, max(roughness, FX_SUN_MIN_ROUGHNESS), spec_color) * vis;
    }
    // SKY REFLECTION HOOK: the sky light's specular + SSR, applied by the deferred reflection pass (lit_common.wgsl reflection_inputs; ReflectionApplyPS reads the
    // GBuffer roughness unclamped and GBufferAO)
    lit = lit + local_lighting(world, n, v, diffuse_color, spec_color, roughness, SM_DEFAULT_LIT, vec4<f32>(0.0), gbuffer_ao);
    reflection_inputs(n, v, world, spec_color, roughness, gbuffer_ao);
    if (dbg_part_any()) { dbg_terms(diffuse_color, n, ndl, vis, select(vec3<f32>(0.0), light.sun_radiance.xyz * 3.14159265 * ggx_spec_color(n, v, l, max(roughness, FX_SUN_MIN_ROUGHNESS), spec_color) * vis, ndl > 0.0)); }
    return lit_out(output_hdr(lit, amb, world, cam));
}

// FX ORDER (2026-10-05): an opaque / masked UNLIT FX material (fn-fx program::assemble_scene_unlit) in the scene pass. UE4.16 base pass: SceneColor = the emissive
// the unlit program wrote, GBuffer ShadingModelID 0 (no deferred light, no sky light, no ReflectionApplyPS, no SSR); the height fog (+ volumetric fog) reach it in
// the full-screen fog pass (FogRendering.cpp, every opaque pixel by depth), here the shared tail `output_hdr` with no ambient share (the AO pass leaves the pixel
// alone) and the vfog composite. The reflection inputs keep lit_common.wgsl's defaults: normal 0 = nothing to reflect.
fn fx_scene_unlit(frag: vec4<f32>, emissive: vec3<f32>) -> LitOut {
    let world = fx_scene_world(frag);
    return lit_out(output_hdr(emissive, vec3<f32>(0.0), world, camera.position.xyz));
}
