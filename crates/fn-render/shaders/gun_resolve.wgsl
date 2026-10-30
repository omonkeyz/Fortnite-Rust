// Resolve of the GBuffer the ORIGINAL base-pass pixel program of the gun material writes (GBufferA normal, B metallic / specular / roughness, C base colour, the
// scene-colour target = emissive + sky-light diffuse the program itself evaluated from the View sky SH). Appended after common.wgsl + lit_common.wgsl, so the
// camera / light blocks, the cascaded sun shadows, the fog and the HDR tail are the renderer's own.
//
// The lighting model below is the UE 4.16 DefaultLit BxDF of the deferred directional light (Lambert diffuse, D_GGX * Vis_SmithJointApprox * F_Schlick specular,
// DiffuseColor = BaseColor * (1 - Metallic), SpecularColor = lerp(0.08 * Specular, BaseColor, Metallic)); it is NOT recovered from the 1.8 shader bytecode (the
// deferred light programs live in the global shader cache and need the scene texture set), the sun / sky constants it consumes are the renderer's recovered ones.
// Sky light: the diffuse SH term comes from the program (scene colour); output_hdr applies the DFAO sky occlusion to it (SkyLighting.usf SkyLightDiffusePS) and the
// SSAO pass the screen-space AO (ssao_pass.rs). The specular term is the shared `sky_specular` of lit_common.wgsl (ReflectionApplyPS with APPLY_SKYLIGHT, no reflection
// captures, docs/spec/pickup_visual_gaps.md section 1); since docs/spec/reflection_gaps.md it is applied, with the SSR, by the deferred reflection pass
// (the generated `gun_fs_main` returns lit_common.wgsl `LitOut`).
const GUN_PI: f32 = 3.14159265;
fn gun_resolve(world: vec3<f32>, n: vec3<f32>, indirect: vec3<f32>, base: vec3<f32>, metallic: f32, specular: f32, roughness_in: f32) -> vec4<f32> {
    let cam = camera.position.xyz;
    if (light.misc.x <= 0.5) { return output_hdr(base, vec3<f32>(0.0), world, cam); }
    // DFAO of the sky diffuse (docs/spec/pickup_visual_gaps.md section 4): `indirect` = DiffuseColor * GetSkySHDiffuse(N) * SkyLightColor of the program, so its
    // shading normal is the GBuffer normal; output_hdr -> df_sky_occlusion occludes it with the bent normal like every other lit path (ambient_light sets it there).
    g_amb_n = n;
    g_amb_set = true;
    let v = normalize(cam - world);
    let l = light.sun_dir.xyz;
    let nl = clamp(dot(n, l), 0.0, 1.0);
    let vis = sun_visibility(world, n, nl);
    let diffuse_color = base * (1.0 - metallic);
    let spec_color = mix(vec3<f32>(0.08 * specular), base, metallic);
    // BRDF AUDIT (2026-10-05): the sun lobe is lit_common.wgsl ggx_spec_color (D_GGX * Vis_SmithJointApprox * F_Schlick * NoL with the directional light's
    // MinRoughness 0.08 and StandardShading's NoV = saturate(abs(NoV) + 1e-5)); was a local copy clamped at 0.02 with NoV = max(NoV, 1e-3)
    let sun = light.sun_radiance.xyz;
    // SKY REFLECTION HOOK: the sky light's specular + SSR (deferred, lit_common.wgsl reflection_inputs). The GBuffer roughness is used unclamped, as
    // ReflectionApplyPS reads it; GBufferAO = 1 (the cooked WeaponMat_Base PS writes AO = 1, docs/spec/pickup_visual_gaps.md section 1).
    reflection_inputs(n, v, world, spec_color, clamp(roughness_in, 0.0, 1.0), 1.0);
    // sun_radiance carries the Lambert 1/pi (scene.wgsl); the specular lobe is not divided by pi, so its light colour is sun_radiance * pi
    let lit = indirect + diffuse_color * sun * nl * vis + sun * GUN_PI * ggx_spec_color(n, v, l, clamp(roughness_in, 0.0, 1.0), spec_color) * vis + local_lighting(world, n, v, diffuse_color, spec_color, clamp(roughness_in, 0.0, 1.0), SM_DEFAULT_LIT, vec4<f32>(0.0), 1.0);
    return output_hdr(lit, indirect, world, cam);
}

// ---- GUN SHADOW: the depth pass of the gun programs into the sun's cascades (gun_material.rs `draw_shadow`) ----
// The shadow View buffer's TranslatedWorldToClip maps the translated UE world (cm) to the PORT world (m, w = 1) (fn-matshader gun.rs `shadow_view`), so the
// cooked vertex program's SV_Position is the world position; this applies the cascade like shadow.wgsl `shadow_depth_outputs` (ShadowDepthVertexShader.usf
// SetShadowDepthOutputs: bClampToNearPlane, ShaderDepthBias added to the stored depth). Group 2 = the cascade's LightViewProj (the renderer's shadow vp buffer).
struct GunShadowVp { m: mat4x4<f32>, depth_bias: vec4<f32> }
@group(2) @binding(0) var<uniform> gun_shadow_vp: GunShadowVp;
fn gun_shadow_depth_outputs(world_m: vec4<f32>) -> vec4<f32> {
    var p = gun_shadow_vp.m * vec4<f32>(world_m.xyz / world_m.w, 1.0);
    if (p.z < 0.0) { p.z = 0.000001; p.w = 1.0; }
    p.z = (p.z + gun_shadow_vp.depth_bias.x) * p.w;
    return p;
}
