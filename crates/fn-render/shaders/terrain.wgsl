// TERRAIN MAIN PASS (2026-10-07): the Athena landscape components within the main pass far plane, drawn in the MAIN pass like UE4.16 draws a landscape (one scene
// depth, depth pre-pass + base pass, lit by the sun with the CSM / ray-traced DF shadows, the sky light with DFAO, the height fog). Prepended: common.wgsl,
// lit_common.wgsl (group 0 = the globals), terrain_common.wgsl (group 1 = the backdrop material bind group; its `bd` uniform is not read here). The depth pre-pass
// twin and the lit pipeline both run `vs_terrain` of this ONE module (identical depth bits for the Equal test, as the ground / props pairs of scene.wgsl).
// The CSM caster pass uses shadow.wgsl `vs_prop` with the BdVertex stride (backdrop.rs).

struct TerrainOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) wp: vec3<f32>,
    @location(1) n: vec3<f32>,
    @location(2) col: vec4<f32>,
}

@vertex
fn vs_terrain(@location(0) pos: vec3<f32>, @location(1) n: vec3<f32>, @location(2) uv: vec2<f32>, @location(3) col: vec4<f32>) -> TerrainOut {
    var o: TerrainOut;
    o.clip = camera.view_proj * vec4<f32>(pos, 1.0);
    o.wp = pos;
    o.n = n;
    o.col = col;
    return o;
}

// The landscape surface of terrain_common.wgsl, DefaultLit (Metallic 0) as the plate's shade_ground: the sun diffuse and its GGX lobe times the sun visibility
// (CSM + ray-traced DF cascade + light function, lit_common.wgsl sun_visibility), the sky light (DFAO in output_hdr), the deferred reflection inputs, the fog.
@fragment
fn fs_terrain(in: TerrainOut) -> LitOut {
    let cam = camera.position.xyz;
    let n = normalize(in.n);
    let s = terrain_surface(in.wp, n, in.col);
    let albedo = s.rgb;
    var lit = albedo;
    var amb = vec3<f32>(0.0);
    if (light.misc.x > 0.5) {
        let sun = light.sun_dir.xyz;
        let ndl = max(dot(n, sun), 0.0);
        let vis = sun_visibility(in.wp, n, ndl);
        let v = normalize(cam - in.wp);
        let spec_color = vec3<f32>(0.08 * s.a);
        amb = albedo * ambient_light(n);
        lit = albedo * light.sun_radiance.xyz * ndl * vis + amb + light.sun_radiance.xyz * 3.14159265 * ggx_spec_color(n, v, sun, LANDSCAPE_ROUGHNESS, spec_color) * vis;
        lit = lit + local_lighting(in.wp, n, v, albedo, spec_color, LANDSCAPE_ROUGHNESS, SM_DEFAULT_LIT, vec4<f32>(0.0), 1.0);
        reflection_inputs(n, v, in.wp, spec_color, LANDSCAPE_ROUGHNESS, 1.0);   // REFLECTION APPLY, GBufferAO 1
        if (dbg_part_any()) { dbg_terms(albedo, n, ndl, vis, light.sun_radiance.xyz * 3.14159265 * ggx_spec_color(n, v, sun, LANDSCAPE_ROUGHNESS, spec_color) * vis); }
    }
    return lit_out(output_hdr(lit, amb, in.wp, cam));
}
