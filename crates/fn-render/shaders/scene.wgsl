// Lit scene pass: baseplate ground (terrain colour variation material), skinned character parts and the pickaxe prop.
// Port of color.vert/color.frag/materials.glsl. Everything is linear HDR; exposure/tonemap live in post.wgsl.
// Common helpers (srgb, fog) are prepended from common.wgsl.
//
// RECOVERED DATA: sun colour/brightness (day TODM), SkyLightColor tint, character parameters from M_Med_Soldier_04 /
//   CharacterShader_Gloss props (RimLightColor, SpecRoughnessMin/Max, Skin Boost, spec multipliers), terrain HighLight/EdgeDark.
// NOT RECOVERED: the node wiring of CharacterShader and of the Athena grass material; channel meanings are inferred.

// Shared lighting (camera/light blocks, shadows, ambient, normal mapping, GGX, output_hdr) lives in lit_common.wgsl.

struct Material { kind: vec4<u32> }   // x: 0 generic untextured, 1 ground, 2 character, 3 prop (pickaxe)
@group(1) @binding(0) var<uniform> material: Material;
@group(1) @binding(1) var t_diffuse: texture_2d<f32>;
@group(1) @binding(2) var t_normal: texture_2d<f32>;
@group(1) @binding(3) var t_spec: texture_2d<f32>;
@group(1) @binding(4) var t_mask: texture_2d<f32>;

struct Object { model: mat4x4<f32> }
@group(2) @binding(0) var<uniform> object: Object;
@group(2) @binding(1) var<storage, read> bones: array<mat4x4<f32>>;

struct VsOut {
    // FULL PREPASS (2026-10-06): the depth pre-pass twins (ground / props / skinned, renderer.rs) run these same vertex entries (same module), as the ground pair
    // always did. NOT `@invariant`: that changes the ground's rasterized depth bits and with them the ground / grass depth ties of the pre-pass (1.2 % of the
    // forest pixels moved in the image check), and the velocity pass relies on the current bits too.
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec3<f32>,   // vertex colour (props); white elsewhere
}

@vertex
fn vs_ground(@location(0) pos: vec3<f32>, @location(1) uv: vec2<f32>) -> VsOut {
    var o: VsOut;
    let wp = object.model * vec4<f32>(pos, 1.0);
    o.clip = camera.view_proj * wp;
    o.world_pos = wp.xyz;
    o.world_normal = vec3<f32>(0.0, 1.0, 0.0);
    o.uv = uv;
    o.color = vec3<f32>(1.0);
    return o;
}

// Procedural props (trees, rocks): world-space triangles with flat normals and vertex colours, drawn untextured.
@vertex
fn vs_prop(@location(0) pos: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) color: vec3<f32>) -> VsOut {
    var o: VsOut;
    o.clip = camera.view_proj * vec4<f32>(pos, 1.0);
    o.world_pos = pos;
    o.world_normal = normal;
    o.uv = vec2<f32>(0.0);
    o.color = color;
    return o;
}

@vertex
fn vs_skinned(
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) joints: vec4<u32>,
    @location(4) weights: vec4<f32>,
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
    o.color = vec3<f32>(1.0);
    return o;
}

// Ambient (sky-light) part of the final lit colour, written by every shading path so the ambient share can be stored in the
// HDR target's alpha; post.wgsl then applies ambient occlusion to that share only (UE applies AO to ambient light, not the sun).
var<private> amb_out: vec3<f32>;
// ---- value noise for macro variation of the grass ----
fn hash21(p_in: vec2<f32>) -> f32 {
    var p = fract(p_in * vec2<f32>(123.34, 456.21));
    p = p + dot(p, p + 45.32);
    return fract(p.x * p.y);
}
fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    var f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash21(i), hash21(i + vec2<f32>(1.0, 0.0)), f.x), mix(hash21(i + vec2<f32>(0.0, 1.0)), hash21(i + vec2<f32>(1.0, 1.0)), f.x), f.y);
}

// Terrain colour variation. RECOVERED: MF_TerrainColorVariation HighLight / EdgeDark, GrassColorMaskContrast -2.
// INFERRED blend lerp(EdgeDark, HighLight, CheapContrast(mask, -2)).  This temporary flat baseplate has no
// Athena landscape UVs, so it intentionally does NOT apply a made-up global mean of T_Athena_Terrain_Mask.
// The recovered variation constants remain here for the map-backed terrain path; `terrain_color_multiplier`
// returns neutral for the standalone baseplate.
const TERRAIN_HIGHLIGHT: vec3<f32> = vec3<f32>(3.0, 2.337033, 1.38);
const TERRAIN_EDGE_DARK: vec3<f32> = vec3<f32>(0.9, 0.9, 0.9);
const TERRAIN_MASK_CONTRAST: f32 = -2.0;
const TERRAIN_MASK_MEAN: f32 = 0.2086;
fn cheap_contrast(x: f32, c: f32) -> f32 { let s = x * x * (3.0 - 2.0 * x); return x + (s - x) * c; }
fn terrain_color_multiplier() -> vec3<f32> {
    return vec3<f32>(1.0, 1.0, 1.0);
}

// REFLECTION APPLY: the GBufferB the Athena landscape writes, from the compiled base-pass PS of its component material instances
// (TBasePassPSFNoLightMapPolicySkylight of the LandscapeMaterialInstanceConstants in Athena_Terrain_LS_00, all 48 permutations):
//   Metallic 0, Roughness 0.8 (constant), Specular = saturate(pow(max(|GrassD.a|, 1e-6), GrassSpecPower) * GrassSpecAmount * GrassLayerWeight)
// with GrassD = the grass layer's T_Grasslands_D sample (its alpha) and M_Athena_Terrain_Landscape's defaults GrassSpecPower 2, GrassSpecAmount 0.3 (no MI /
// MIC override). The cb slot -> parameter mapping (cb[44].x / .y) is by name (the uniform set of the umap MICs is not decoded): ASSUMED.
const LANDSCAPE_ROUGHNESS: f32 = 0.8;
const GRASS_SPEC_POWER: f32 = 2.0;
const GRASS_SPEC_AMOUNT: f32 = 0.3;
fn landscape_specular(grass_alpha: f32, grass_weight: f32) -> f32 {
    return clamp(pow(max(abs(grass_alpha), 1e-6), GRASS_SPEC_POWER) * GRASS_SPEC_AMOUNT * grass_weight, 0.0, 1.0);
}

// Baseplate grass: raw T_Grasslands_D (terrain variation waits for real terrain UVs), a second larger-scale tile to hide repetition (APPROXIMATION),
// luminance-only macro variation and a bump from albedo luminance (APPROXIMATION); the landscape's GBufferB (above, grass weight 1).
fn shade_ground(uv: vec2<f32>, world_pos: vec3<f32>, cam_pos: vec3<f32>) -> vec3<f32> {
    let mult = terrain_color_multiplier();
    // SRGB BEFORE FILTER (2026-10-07): T_Grasslands_D (cooked SRGB=true) is uploaded as Rgba8UnormSrgb (renderer.rs ground_tex): linear samples, decoded before filtering
    let grass_tex = textureSample(t_diffuse, tex_samp, uv);
    let base = grass_tex.rgb * mult;
    let macro_tile = textureSample(t_diffuse, tex_samp, uv * 0.137 + vec2<f32>(0.31, 0.77)).rgb * mult;
    let lum = luma709(base);
    let macro_lum = max(luma709(macro_tile), 1e-3);
    var albedo = base * mix(1.0, clamp(macro_lum / max(lum, 1e-3), 0.6, 1.6), select(0.35, 0.0, dbg(1u)));
    let variation = value_noise(world_pos.xz * 0.045) * 0.6 + value_noise(world_pos.xz * 0.31) * 0.4;
    if (!dbg(1u)) { albedo = albedo * mix(0.92, 1.08, variation); }
    var n = vec3<f32>(0.0, 1.0, 0.0);
    let dp1 = dpdx(world_pos);
    let dp2 = gl_dfdy3(world_pos);
    let dh1 = dpdx(lum);
    let dh2 = gl_dfdy1(lum);
    let r1 = cross(dp2, n);
    let r2 = cross(n, dp1);
    let det = dot(dp1, r1);
    let grad = (r1 * dh1 + r2 * dh2) / select(1e-12, det, abs(det) > 1e-12);
    if (!dbg(1u)) { n = normalize(n - clamp(grad * 0.04, vec3<f32>(-0.3), vec3<f32>(0.3))); }
    let sun_dir = light.sun_dir.xyz;
    let ndl = clamp(dot(n, sun_dir), 0.0, 1.0);
    let vis = sun_visibility(world_pos, n, ndl);
    let nl = ndl * vis;
    let v = normalize(cam_pos - world_pos);
    amb_out = albedo * ambient_light(n);
    let diffuse = albedo * light.sun_radiance.xyz * nl + amb_out;
    // sun_radiance already carries the Lambert 1/pi; the GGX BRDF is not divided by pi, so its light is Intensity = x pi.
    // DefaultLit SpecularColor = 0.08 * Specular (Metallic 0) of the landscape GBufferB (was F0 0.04 / roughness 0.9, not the material's)
    let spec_color = vec3<f32>(0.08 * landscape_specular(grass_tex.a, 1.0));
    let spec = light.sun_radiance.xyz * 3.14159265 * ggx_spec_color(n, v, sun_dir, LANDSCAPE_ROUGHNESS, spec_color) * vis;
    reflection_inputs(n, v, world_pos, spec_color, LANDSCAPE_ROUGHNESS, 1.0);   // REFLECTION APPLY (lit_common.wgsl), GBufferAO 1
    return diffuse + spec + local_lighting(world_pos, n, v, albedo, spec_color, LANDSCAPE_ROUGHNESS, SM_DEFAULT_LIT, vec4<f32>(0.0), 1.0);
}

// FALLBACK only (docs/spec/smallfix_followups.md section 3): CharacterShader-owned sections run the original DXBC (`char_scene`), and with the original gun
// material the held gun / pickaxe / dropped weapons run their cooked WeaponMat_Base (gun pass). This draws what is left: the held pickaxe, guns and dropped
// items in the default mode (kind 3, Prop) and character sections without a CharacterShader owner (kind 2).
// Character (kind 2): the CharacterShader G-buffer (translated char_material_gen.wgsl head :329-337, hair :805-813, body :1285-1294):
//   Metallic = sat(S.g); Specular = sat(lerp(ClothSpecMultiplier, SkinSpecMultiply, M.b) * S.r) (both 0.5 cooked -> 0.5 * S.r);
//   Roughness = sat(lerp(SpecRoughnessMin, SpecRoughnessMax, S.b)) (head / hair; the body's CharacterShader_Gloss uses 1 - that: NOT applied here, the
//   per-material flag and Min / Max are not plumbed, ASSUMED 0.2 / 0.8 head form); AO = sat(AOLevel * (M.r - 1) + 1) = 1 (AOLevel 0).
// Prop (kind 3): a plain dielectric, Specular 0.5 -> F0 0.04, roughness 0.5: ASSUMED (the real WeaponMat_Base runs only with --original-gun-material, see
//   docs/spec/pickup_visual_gaps.md section 2 for why that is not the default yet).
// UE DefaultLit split: DiffuseColor = Base * (1 - Metallic), SpecularColor = lerp(0.08 * Specular, Base, Metallic) (as shade_architect).
fn shade_character(uv: vec2<f32>, world_normal: vec3<f32>, world_pos: vec3<f32>, cam_pos: vec3<f32>, is_prop: bool) -> vec3<f32> {
    let albedo = to_linear(textureSample(t_diffuse, tex_samp, uv).rgb);
    var tn = unpack_normal(textureSample(t_normal, tex_samp, uv));
    if (dbg(2u)) { tn = vec3<f32>(0.0, 0.0, 1.0); }
    let n = perturb_normal(normalize(world_normal), world_pos, uv, tn);
    let v = normalize(cam_pos - world_pos);
    let prop = is_prop || dbg(2u);
    let sm = clamp(textureSample(t_spec, tex_samp, uv).rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    var mm = textureSample(t_mask, tex_samp, uv).rgb;
    var metallic = sm.g;
    var specular = 0.5 * sm.r;                          // lerp(ClothSpecMultiplier 0.5, SkinSpecMultiply 0.5, M.b) * S.r
    var roughness = mix(0.2, 0.8, sm.b);               // SpecRoughnessMin / Max (ASSUMED, see above)
    if (prop) { metallic = 0.0; specular = 0.5; roughness = 0.5; mm = vec3<f32>(1.0, 0.0, 0.0); }
    let skin = select(mm.b, 0.0, prop);
    let diffuse_color = albedo * (1.0 - metallic);
    let spec_color = mix(vec3<f32>(0.08 * specular), albedo, metallic);
    let sun_dir = light.sun_dir.xyz;
    let ndl = clamp(dot(n, sun_dir), 0.0, 1.0);
    let vis = sun_visibility(world_pos, n, ndl);
    let nl = ndl * vis;
    let ambient = ambient_light(n);                     // AO = 1 (AOLevel 0)
    amb_out = diffuse_color * ambient;
    var lit = diffuse_color * (light.sun_radiance.xyz * nl + ambient);
    // UE4.16 DeferredLightingCommon.usf:525 / ShadingModels.usf StandardShading: LightColor * NoL * (Diffuse_Lambert = Diffuse / PI + D_GGX * Vis * F); sun_radiance is
    // LightColor / PI (lighting.rs sun_radiance_of), so the specular lobe takes sun_radiance * PI, as the ground / Architect / char_scene / gun paths do.
    // GGX with a colour F0: Schlick is linear in F0, spec(F0) = spec(0) + (spec(1) - spec(0)) * F0 (as shade_architect)
    // BRDF AUDIT: UE F_Schlick = saturate(50 * SpecularColor.g) * Fc + (1 - Fc) * SpecularColor (BRDF.usf:169), was Fc + (1 - Fc) * SpecularColor
    lit = lit + light.sun_radiance.xyz * 3.14159265 * ggx_spec_color(n, v, sun_dir, roughness, spec_color) * vis;
    lit = lit + local_lighting(world_pos, n, v, diffuse_color, spec_color, roughness, SM_DEFAULT_LIT, vec4<f32>(0.0), 1.0);
    reflection_inputs(n, v, world_pos, spec_color, roughness, 1.0);   // SKY REFLECTION HOOK (lit_common.wgsl, applied by the deferred reflection pass), AO = 1 as above
    let nv = clamp(dot(n, v), 0.0, 1.0);
    // Skin Boost Color And Exponent (0.525,0.343875,0.367316, exp 2): fresnel-weighted subsurface-like lift.
    lit = lit + vec3<f32>(0.525, 0.343875, 0.367316) * albedo * ambient_light(vec3<f32>(0.0, 1.0, 0.0)) * pow(1.0 - nv, 2.0) * skin * 0.6;
    // RimLightColor (1.22958,1.48567,1.5): exponent and intensity are guesses; UpVectorPower=2 masks to up-facing normals.
    let up_mask = clamp(n.y * 0.5 + 0.5, 0.0, 1.0);
    lit = lit + vec3<f32>(1.22958, 1.48567, 1.5) * pow(1.0 - nv, 3.0) * (up_mask * up_mask) * select(mm.g, 0.0, prop) * 0.05;
    return lit;
}

// BUILD ANIM HOOK: M_BluePrintArchitect (MaterialKind::Architect; the compiled base-pass PS of the material, see mesh.rs): TwoSided (the PS flips the
// normal on back faces), BaseColor = Diffuse, Metallic / Specular / Roughness = S.g / S.r / S.b (fixed T_ArchitectTools_S, linear),
// EmissiveColor = max(EmissiveMask.rgb * EmissiveMultiply (material.kind.y) + SelectionColor (0), 0). UE DefaultLit split: DiffuseColor = Base - Base * Metallic,
// SpecularColor = lerp(0.08 * Specular, Base, Metallic); the sun / sky terms are this renderer's (the same as the other lit paths).
fn shade_architect(uv: vec2<f32>, world_normal: vec3<f32>, world_pos: vec3<f32>, cam_pos: vec3<f32>, front: bool) -> vec3<f32> {
    let base = clamp(to_linear(textureSample(t_diffuse, tex_samp, uv).rgb), vec3<f32>(0.0), vec3<f32>(1.0));
    let s = clamp(textureSample(t_spec, tex_samp, uv).rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let metallic = s.g;
    let specular = s.r;
    let roughness = s.b;
    let tn = unpack_normal(textureSample(t_normal, tex_samp, uv));
    let n0 = normalize(world_normal) * select(-1.0, 1.0, front);
    let n = perturb_normal(n0, world_pos, uv, tn);
    let v = normalize(cam_pos - world_pos);
    let diffuse_color = base - base * metallic;
    let spec_color = mix(vec3<f32>(0.08 * specular), base, metallic);
    let sun_dir = light.sun_dir.xyz;
    let ndl = clamp(dot(n, sun_dir), 0.0, 1.0);
    let vis = sun_visibility(world_pos, n, ndl);
    amb_out = diffuse_color * ambient_light(n);
    // GGX with a colour F0: the Schlick term is linear in F0, so spec(F0) = spec(0) + (spec(1) - spec(0)) * F0
    // BRDF AUDIT: UE F_Schlick with the saturate(50 * SpecularColor.g) term (BRDF.usf:169; was Fc + (1 - Fc) * SpecularColor)
    let spec = light.sun_radiance.xyz * 3.14159265 * ggx_spec_color(n, v, sun_dir, roughness, spec_color) * vis;
    let emissive = max(to_linear(textureSample(t_mask, tex_samp, uv).rgb) * bitcast<f32>(material.kind.y), vec3<f32>(0.0));
    reflection_inputs(n, v, world_pos, spec_color, roughness, 1.0);   // SKY REFLECTION HOOK (lit_common.wgsl, applied by the deferred reflection pass)
    return diffuse_color * light.sun_radiance.xyz * ndl * vis + amb_out + spec + emissive + local_lighting(world_pos, n, v, diffuse_color, spec_color, roughness, SM_DEFAULT_LIT, vec4<f32>(0.0), 1.0);
}

@fragment
fn fs_main(in: VsOut, @builtin(front_facing) front: bool) -> LitOut {
    let cam = camera.position.xyz;
    let kind = material.kind.x;
    let lit_enabled = light.misc.x > 0.5;
    var lit: vec3<f32>;
    amb_out = vec3<f32>(0.0);
    if (lit_enabled && kind == 4u) {
        lit = shade_architect(in.uv, in.world_normal, in.world_pos, cam, front);
    } else if (lit_enabled && kind == 1u) {
        lit = shade_ground(in.uv, in.world_pos, cam);
    } else if (lit_enabled && kind >= 2u) {
        lit = shade_character(in.uv, in.world_normal, in.world_pos, cam, kind == 3u);
    } else {
        // Untextured generic surface (white albedo) lit by sun + SH sky light.
        let albedo = in.color;
        var irradiance = vec3<f32>(1.0);
        if (lit_enabled) {
            let n = normalize(in.world_normal);
            let ndl = max(dot(n, light.sun_dir.xyz), 0.0);
            let visibility = sun_visibility(in.world_pos, n, ndl);
            let sky = sky_lookup(n);   // SKY LIGHT: SH of the specified cubemap (lit_common.wgsl)
            irradiance = light.sun_radiance.xyz * ndl * visibility + sky;
            amb_out = albedo * sky;
        }
        lit = albedo * irradiance;
    }
    return lit_out(output_hdr(lit, amb_out, in.world_pos, cam));   // REFLECTION APPLY: + the deferred reflection inputs (lit_common.wgsl LitOut)
}

// Static procedural props (trees, rocks; vs_prop): port of the old runtime's untextured generic path of color.frag with the
// vertex colour as linear albedo (the `shade` varying):
//   albedo * (sun_radiance * max(n.l, 0) * cascade_visibility + GetSkySHDiffuse(n) * SkyLightColor), then fog.
// The Athena sky light keeps its lower hemisphere (TODM ctor bLowerHemisphereIsBlack = 0, sky_light.rs), so down-facing faces get the SH of the
// cubemap's lower half too. No normal map, no skinning. The ambient share goes to alpha like the other paths so post.wgsl applies AO to it only.
// REFLECTION APPLY: these fallback props have no material, so they take UE's unconnected DefaultLit inputs, Specular 0.5 / Roughness 0.5
// (MaterialShared.cpp:2894-2895, SpecularColor 0.08 * 0.5): the sun's GGX lobe and the deferred sky specular / SSR like every DefaultLit GBuffer pixel (ASSUMED material).
const DEFAULT_SPECULAR: f32 = 0.5;
const DEFAULT_ROUGHNESS: f32 = 0.5;
@fragment
fn fs_prop(in: VsOut) -> LitOut {
    let cam = camera.position.xyz;
    let albedo = in.color;
    var lit = albedo;
    var amb = vec3<f32>(0.0);
    if (light.misc.x > 0.5) {
        let n = normalize(in.world_normal);
        let ndl = max(dot(n, light.sun_dir.xyz), 0.0);
        let visibility = sun_visibility(in.world_pos, n, ndl);
        amb = albedo * sky_lookup(n);   // SKY LIGHT: SH of the specified cubemap (lit_common.wgsl)
        lit = albedo * light.sun_radiance.xyz * ndl * visibility + amb;
        let v = normalize(cam - in.world_pos);
        let spec_color = vec3<f32>(0.08 * DEFAULT_SPECULAR);
        lit = lit + light.sun_radiance.xyz * 3.14159265 * ggx_spec_color(n, v, light.sun_dir.xyz, DEFAULT_ROUGHNESS, spec_color) * visibility;
        lit = lit + local_lighting(in.world_pos, n, v, albedo, spec_color, DEFAULT_ROUGHNESS, SM_DEFAULT_LIT, vec4<f32>(0.0), 1.0);
        reflection_inputs(n, v, in.world_pos, spec_color, DEFAULT_ROUGHNESS, 1.0);
    }
    return lit_out(output_hdr(lit, amb, in.world_pos, cam));
}
